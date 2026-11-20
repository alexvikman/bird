"""Tests for triage.py: python3 -m unittest discover -s php-builds"""

import os
import shutil
import tempfile
import unittest

import triage

MAIN = "a" * 40
PLATFORMS = ("macos-aarch64", "macos-x86_64", "linux-aarch64", "linux-x86_64")


def run(**changes):
    r = {"id": 500, "run_attempt": 1, "conclusion": "failure", "head_sha": MAIN, "event": "schedule",
         "display_title": "PHP builds", "html_url": "https://github.com/o/r/actions/runs/500"}
    r.update(changes)
    return r


def jobs(broken=(), cancelled=(), publish="skipped"):
    """The jobs of a run building 8.4.28 and 8.5.13 on every platform."""
    out = [{"id": 1, "name": "Resolve versions", "conclusion": "success", "steps": []}]
    for n, (version, platform) in enumerate((v, p) for v in ("8.4.28", "8.5.13") for p in PLATFORMS):
        name = "PHP %s (%s)" % (version, platform)
        conclusion = "failure" if name in broken else "cancelled" if name in cancelled else "success"
        steps = [{"name": "Set up job", "conclusion": "success"}, {"name": "Build", "conclusion": conclusion}]
        out.append({"id": 10 + n, "name": name, "conclusion": conclusion, "steps": steps,
                    "html_url": "https://github.com/o/r/actions/runs/500/job/%d" % (10 + n)})
    out.append({"id": 99, "name": "Publish release", "conclusion": publish, "steps": []})
    return out


def ids(all_jobs, *names):
    return [str(j["id"]) for j in all_jobs if j["name"] in names]


class DecideTest(unittest.TestCase):
    def decide(self, r, js, annotations=None, approvals=(), main=MAIN, latest=500):
        return triage.decide(r, js, annotations or {}, list(approvals), main, latest)

    def test_retries_a_job_github_lost(self):
        # One job cancelled on its own, mid-build, while the others went on.
        lost = "PHP 8.5.13 (macos-x86_64)"
        js = jobs(cancelled=[lost])
        notes = {i: ["The operation was canceled."] for i in ids(js, lost)}
        action, reason = self.decide(run(conclusion="cancelled"), js, notes)
        self.assertEqual(action, "retry")
        self.assertIn(lost, reason)

    def test_retries_a_few_failed_builds(self):
        js = jobs(broken=["PHP 8.4.28 (linux-x86_64)", "PHP 8.5.13 (linux-x86_64)"], publish="success")
        self.assertEqual(self.decide(run(), js)[0], "retry")

    def test_retries_when_the_plan_failed(self):
        js = jobs()
        js[0]["conclusion"] = "failure"
        for j in js[1:]:
            j["conclusion"] = "skipped"
        self.assertEqual(self.decide(run(), js)[0], "retry")

    def test_leaves_runs_cancelled_on_purpose(self):
        for message in ("The run was canceled by @someone.",
                        "Canceling since a higher priority waiting request for php-builds-publish exists"):
            js = jobs(cancelled=["PHP 8.4.28 (macos-aarch64)", "PHP 8.5.13 (macos-x86_64)"])
            notes = {i: ["The operation was canceled.", message] for i in ids(js, "PHP 8.5.13 (macos-x86_64)")}
            self.assertEqual(self.decide(run(conclusion="cancelled"), js, notes)[0], "none", message)

    def test_leaves_a_rejected_publish(self):
        js = jobs(broken=["PHP 8.4.28 (linux-x86_64)"], publish="failure")
        action, reason = self.decide(run(), js, approvals=[{"state": "approved"}, {"state": "rejected"}])
        self.assertEqual(action, "none")
        self.assertIn("rejected", reason)

    def test_leaves_runs_that_are_out_of_date(self):
        js = jobs(broken=["PHP 8.4.28 (linux-x86_64)"])
        self.assertEqual(self.decide(run(), js, latest=501)[0], "none")
        self.assertEqual(self.decide(run(), js, main="b" * 40)[0], "none")

    def test_leaves_runs_that_succeeded_or_had_nothing_broken(self):
        self.assertEqual(self.decide(run(conclusion="success"), jobs())[0], "none")
        self.assertEqual(self.decide(run(conclusion="cancelled"), jobs())[0], "none")

    def test_asks_a_person_after_one_rerun(self):
        js = jobs(broken=["PHP 8.4.28 (linux-x86_64)"])
        action, reason = self.decide(run(run_attempt=2), js)
        self.assertEqual(action, "issue")
        self.assertIn("PHP 8.4.28 (linux-x86_64)", reason)

    def test_asks_a_person_when_publishing_failed(self):
        self.assertEqual(self.decide(run(), jobs(publish="failure"))[0], "issue")

    def test_asks_a_person_when_most_builds_failed(self):
        everything_linux = ["PHP %s (%s)" % (v, p) for v in ("8.4.28", "8.5.13") for p in PLATFORMS[2:]]
        self.assertEqual(self.decide(run(), jobs(broken=everything_linux))[0], "retry")  # half: still the runners
        action, reason = self.decide(run(), jobs(broken=everything_linux + ["PHP 8.4.28 (macos-aarch64)"]))
        self.assertEqual(action, "issue")
        self.assertIn("5 of 8 builds", reason)


class IssueTest(unittest.TestCase):
    def setUp(self):
        self.logs = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, self.logs)

    def test_describes_what_failed(self):
        js = jobs(broken=["PHP 8.4.28 (linux-x86_64)"], cancelled=["PHP 8.5.13 (macos-x86_64)"])
        failed, lost = ids(js, "PHP 8.4.28 (linux-x86_64)") + ids(js, "PHP 8.5.13 (macos-x86_64)")
        with open(os.path.join(self.logs, failed + ".log"), "w") as f:
            f.write("".join("PHP 8.4.28 (linux-x86_64)\tBuild\t2026-11-19T19:00:%02d.0Z line %d\n" % (i % 60, i) for i in range(100)))
            f.write("PHP 8.4.28 (linux-x86_64)\tBuild\t2026-11-19T19:01:00.0Z \x1b[31mmake: *** [all] Error 2\x1b[0m\n")
            f.write("PHP 8.4.28 (linux-x86_64)\tBuild\t2026-11-19T19:01:00.0Z ~~~\n")
        notes = {failed: ["Process completed with exit code 2."], lost: ["The operation was canceled."]}
        text = triage.issue(run(run_attempt=2), js, notes, self.logs, "still failing")

        self.assertIn("attempt 2", text)
        self.assertIn("gh run rerun 500 --failed", text)
        self.assertIn("| [PHP 8.4.28 (linux-x86_64)](https://github.com/o/r/actions/runs/500/job/", text)
        self.assertIn("| Build | Process completed with exit code 2. |", text)
        self.assertIn("| Build | The operation was canceled. |", text)
        self.assertIn("make: *** [all] Error 2", text)
        self.assertNotIn("\x1b", text)
        self.assertNotIn("line 70\n", text)  # only the end of the log
        self.assertIn("line 99\n", text)
        self.assertIn("~~~~ text\n", text)  # the log's own ~~~ cannot close the block
        self.assertIn("No log.", text)

    def test_keeps_job_names_out_of_the_table_markup(self):
        js = jobs(broken=["PHP 8.4.28 (linux-x86_64)"])
        js[5]["name"] = "PHP | evil\nname"
        js[5]["conclusion"] = "failure"
        text = triage.issue(run(), js, {}, self.logs, "x")
        self.assertIn("PHP \\| evil name", text)


class LogTailTest(unittest.TestCase):
    def test_strips_prefixes_colours_and_groups(self):
        text = "job\tstep\t2026-11-19T20:37:40.0Z ##[group]Run x\njob\tstep\t2026-11-19T20:37:40.0Z ^[[32mok^[[0m\n\n"
        self.assertEqual(triage.log_tail(text), "ok")

    def test_stops_where_the_cleanup_starts(self):
        text = "j\ts\tT ##[error]Process completed with exit code 1.\nj\ts\tT Post job cleanup.\nj\ts\tT git version 2.56.1\n"
        self.assertEqual(triage.log_tail(text), "##[error]Process completed with exit code 1.")


if __name__ == "__main__":
    unittest.main()
