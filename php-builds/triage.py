#!/usr/bin/env python3
"""Decides what to do about a PHP builds run that did not succeed
(.github/workflows/php-builds-retry.yml, see README.md, "When a run fails").

  triage.py decide --run RUN --jobs JOBS --annotations ANNOTATIONS
                   --approvals APPROVALS --main-sha SHA --latest-run ID
      Prints "action=<none|retry|issue>" and "reason=<one line>", for
      $GITHUB_OUTPUT. RUN is the workflow_run of the event, JOBS the jobs of
      its latest attempt and APPROVALS its deployment reviews, as GitHub's
      API returns them; ANNOTATIONS maps a job id to the messages of its
      annotations.

  triage.py issue --run RUN --jobs JOBS --annotations ANNOTATIONS
                  --logs DIR --reason TEXT
      Prints the Markdown of an issue about the run: what failed, where, and
      the end of each log (DIR/<job id>.log).

A job that fails or is lost to the runner (GitHub cancels it, alone, with
"The operation was canceled.") is usually the runner's doing, not the
build's: those are re-run once. A run someone cancelled, a publish someone
rejected, a run main has left behind and a newer run need nothing. Anything
still failing after that one re-run, failing in the publish job, or failing
in most builds at once is for a person: an issue.
"""

import argparse
import json
import os
import re
import sys

PUBLISH_JOB = "Publish release"
BUILD_JOB_PREFIX = "PHP "
BROKEN = ("failure", "cancelled", "timed_out")
# What GitHub writes on the jobs of a run cancelled on purpose: by a person,
# or by a concurrency group making way for a newer run. A job GitHub loses
# only says "The operation was canceled."
DELIBERATE = ("The run was canceled by ", "Canceling since ")
LOG_LINES = 30
# gh run view --log: "<job>\t<step>\t<timestamp> <line>"
LOG_PREFIX_RE = re.compile(r"^[^\t]*\t[^\t]*\t\S+ ?")
ANSI_RE = re.compile(r"\x1b\[[0-9;]*[A-Za-z]|\^\[\[[0-9;]*[A-Za-z]")


def broken_jobs(jobs):
    return [j for j in jobs if j.get("conclusion") in BROKEN]


def decide(run, jobs, annotations, approvals, main_sha, latest_run):
    """Returns (action, reason)."""
    if run.get("conclusion") not in ("failure", "cancelled"):
        return "none", "the run did not fail"
    if str(latest_run) != str(run["id"]):
        return "none", "a newer run (%s) has started since" % latest_run
    if run.get("head_sha") != main_sha:
        return "none", "main has moved on since this run, which could no longer publish; the next run builds it"
    if any(a.get("state") == "rejected" for a in approvals):
        return "none", "publishing was rejected"
    broken = broken_jobs(jobs)
    for job in broken:
        for message in annotations.get(str(job["id"])) or []:
            if message.startswith(DELIBERATE):
                return "none", "the run was cancelled on purpose (%s)" % message.strip()
    if not broken:
        return "none", "no job failed"

    names = ", ".join(j["name"] for j in broken)
    if int(run.get("run_attempt", 1)) > 1:
        return "issue", "still failing after a re-run (attempt %s): %s" % (run["run_attempt"], names)
    if any(j["name"] == PUBLISH_JOB for j in broken):
        return "issue", "the publish job failed; it is not re-run on its own, since it needs approval again and rarely fails for a reason a re-run fixes"
    builds = [j for j in jobs if j["name"].startswith(BUILD_JOB_PREFIX)]
    broken_builds = [j for j in broken if j["name"].startswith(BUILD_JOB_PREFIX)]
    if len(builds) >= 4 and len(broken_builds) * 2 > len(builds):
        return "issue", "%d of %d builds failed, more likely the change than the runners" % (len(broken_builds), len(builds))
    return "retry", "%d job%s failed or lost: %s" % (len(broken), "" if len(broken) == 1 else "s", names)


def failed_step(job):
    for step in job.get("steps") or []:
        if step.get("conclusion") in BROKEN:
            return step.get("name") or "-"
    return "-"


def log_tail(text, lines=LOG_LINES):
    """The end of a job's log, up to where GitHub starts cleaning up after it."""
    kept = []
    for line in text.splitlines():
        line = ANSI_RE.sub("", LOG_PREFIX_RE.sub("", line)).rstrip()
        if line == "Post job cleanup.":
            break
        if line and not line.startswith(("##[group]", "##[endgroup]")):
            kept.append(line)
    return "\n".join(kept[-lines:])


def fenced(text):
    """A code block no line of the text can close."""
    longest = max([len(m) for m in re.findall(r"~{3,}", text)] + [2])
    fence = "~" * (longest + 1)
    return "%s text\n%s\n%s" % (fence, text, fence)


def cell(text):
    return " ".join(text.split()).replace("|", "\\|")[:300]


def issue(run, jobs, annotations, logs, reason):
    broken = broken_jobs(jobs)
    out = [
        "[%s, attempt %s](%s) on `%s` (%s) did not succeed: %s." % (
            run.get("display_title") or "PHP builds", run.get("run_attempt", 1), run["html_url"],
            run.get("head_sha", "")[:7], run.get("event", "?"), reason),
        "",
        "Nothing a failed job built was published. Re-run the failed jobs from the run, or with "
        "`gh run rerun %s --failed`; close this issue once PHP builds succeed again." % run["id"],
        "",
        "| Job | Step | Error |",
        "| --- | --- | --- |",
    ]
    for job in broken:
        errors = [m for m in annotations.get(str(job["id"])) or [] if m and m.strip()]
        out.append("| [%s](%s) | %s | %s |" % (cell(job["name"]), job.get("html_url", run["html_url"]),
                                              cell(failed_step(job)), cell(errors[0]) if errors else "see the log"))
    for job in broken:
        path = os.path.join(logs, "%s.log" % job["id"])
        tail = ""
        if os.path.isfile(path):
            with open(path, errors="replace") as f:
                tail = log_tail(f.read())
        out += ["", "<details><summary>%s</summary>" % cell(job["name"]), "",
                fenced(tail or "No log."), "", "</details>"]
    return "\n".join(out) + "\n"


def load(path):
    with open(path) as f:
        return json.load(f)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    for name in ("decide", "issue"):
        p = sub.add_parser(name)
        p.add_argument("--run", required=True, help="the workflow_run, as JSON")
        p.add_argument("--jobs", required=True, help="the jobs of its latest attempt, as a JSON list")
        p.add_argument("--annotations", required=True, help="JSON: job id -> annotation messages")
        if name == "decide":
            p.add_argument("--approvals", required=True, help="the run's deployment reviews, as JSON")
            p.add_argument("--main-sha", required=True)
            p.add_argument("--latest-run", required=True, help="id of the workflow's newest run")
        else:
            p.add_argument("--logs", required=True, help="directory of <job id>.log")
            p.add_argument("--reason", required=True)
    args = parser.parse_args()

    run, jobs, annotations = load(args.run), load(args.jobs), load(args.annotations)
    if args.command == "decide":
        action, reason = decide(run, jobs, annotations, load(args.approvals), args.main_sha, args.latest_run)
        print("action=%s" % action)
        print("reason=%s" % " ".join(reason.split()))
    else:
        sys.stdout.write(issue(run, jobs, annotations, args.logs, args.reason))


if __name__ == "__main__":
    main()
