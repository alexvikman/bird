"""Tests for manifest.py: python3 -m unittest discover -s php-builds"""

import hashlib
import json
import os
import shutil
import tempfile
import unittest
from unittest import mock

import manifest

RECIPE = "0123456789abcdef"
SIGNER = "9D7F99A0CB8F05C8A6958D6256A97AF7600A39A6"  # a PHP 8.4 release manager (php-keys.txt)


def make_artifact(root, version="8.4.27", platform="macos-aarch64", name=None, recipe=RECIPE):
    """Writes what build.sh uploads for one build; returns (directory, build)."""
    directory = os.path.join(root, name or "php-%s-%s" % (version, platform))
    os.makedirs(directory)

    def asset(kind):
        data = ("%s %s %s" % (version, platform, kind)).encode()
        digest = hashlib.sha256(data).hexdigest()
        file = "php-%s-%s-%s-%s.tar.gz" % (version, kind, platform, digest[:12])
        with open(os.path.join(directory, file), "wb") as f:
            f.write(data)
        return {"file": file, "sha256": digest, "size": len(data)}

    build = {
        "version": version,
        "minor": version.rsplit(".", 1)[0],
        "platform": platform,
        "spc_version": "2.8.5",
        "recipe": recipe,
        "cli": asset("cli"),
        "fpm": asset("fpm"),
        "extensions": ["bcmath", "pdo_mysql"],
        "shared_extensions": {"xdebug": dict(version="3.5.3", **asset("xdebug"))},
        "sources": {
            "php-src": {"file": "php-%s.tar.xz" % version, "sha256": "a" * 64, "signed_by": SIGNER},
            "zlib": {"file": "zlib-1.4.tar.gz", "sha256": "b" * 64},
            "qdbm": {"dir": "qdbm", "git": "https://github.com/static-php/qdbm.git", "commit": "c" * 40},
        },
    }
    write_build(directory, build)
    return directory, build


def write_build(directory, build, name=None):
    name = name or "%s.json" % os.path.basename(directory)
    with open(os.path.join(directory, name), "w") as f:
        json.dump(build, f)


def report(directory, job=None, artifact=None, digest=None):
    """The line the workflow passes on for a build job's annotation."""
    name = os.path.basename(directory)
    version, platform = name[len("php-"):].split("-", 1)
    if digest is None:
        with open(os.path.join(directory, name + ".json"), "rb") as f:
            digest = hashlib.sha256(f.read()).hexdigest()
    return "%s\tbird-php-build %s %s\n" % (job or "PHP %s (%s)" % (version, platform), artifact or name, digest)


def reports_of(root, *lines):
    """A reports file for every artifact in root (as the jobs really built them), plus lines."""
    text = "".join(report(os.path.join(root, d)) for d in sorted(os.listdir(root)) if os.path.isdir(os.path.join(root, d)) and d.count("-") >= 3 and os.path.exists(os.path.join(root, d, d + ".json")))
    handle, path = tempfile.mkstemp()
    with os.fdopen(handle, "w") as f:
        f.write(text + "".join(lines))
    return path


def matrix(*builds):
    return json.dumps({"include": [{"php": v, "platform": p, "runner": "x"} for v, p in builds]})


class ArtifactTest(unittest.TestCase):
    def setUp(self):
        self.root = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, self.root)
        self.planned = {("8.4.27", "macos-aarch64"), ("8.4.27", "linux-x86_64")}

    def check(self, directory, reports=None):
        # By default the build job reported exactly what is in the directory.
        if reports is None:
            name = os.path.basename(directory)
            meta = os.path.join(directory, name + ".json")
            reports = {name: {manifest.sha256_file(meta)}} if os.path.isfile(meta) else {}
        return manifest.check_artifact(directory, self.planned, RECIPE, reports)

    def assertRejected(self, directory, message):
        with self.assertRaises(manifest.Rejected) as caught:
            self.check(directory)
        self.assertIn(message, str(caught.exception))

    def change(self, **changes):
        directory, build = make_artifact(self.root)
        build.update(changes)
        write_build(directory, build)
        return directory

    def test_accepts_what_build_sh_writes(self):
        directory, build = make_artifact(self.root)
        checked, files = self.check(directory)
        self.assertEqual(checked, build)
        self.assertEqual(sorted(files), sorted(e for e in os.listdir(directory) if e.endswith(".tar.gz")))
        self.assertEqual(len(files), 3)

    def test_rejects_a_build_its_own_job_did_not_report(self):
        # Any job can upload an artifact under another job's name; only the
        # job itself can say what it built.
        directory, _ = make_artifact(self.root)
        name = os.path.basename(directory)
        for reports, message in (
            ({}, "reported no build"),
            ({name: set()}, "reported no build"),
            ({"php-8.4.27-linux-x86_64": {"a" * 64}}, "reported no build"),
            ({name: {"a" * 64}}, "not the build file its build job reported"),
            ({name: {"a" * 64, "b" * 64}}, "reported several builds"),
        ):
            with self.assertRaises(manifest.Rejected) as caught:
                self.check(directory, reports)
            self.assertIn(message, str(caught.exception))

    def test_reads_reports_only_from_the_job_they_are_about(self):
        directory, _ = make_artifact(self.root)
        mine = report(directory)
        digest = mine.split()[-1]
        other = "c" * 64
        path = reports_of(
            self.root,
            # Another job speaking about this build, and this job about another.
            report(directory, job="PHP 8.4.27 (linux-x86_64)", digest=other),
            report(directory, artifact="php-8.4.27-linux-x86_64", digest=other),
            report(directory, job="Publish release", digest=other),
            report(directory, job="PHP 8.4.27 (macos-aarch64) extra", digest=other),
            "PHP 8.4.27 (macos-aarch64)\tbird-php-build php-8.4.27-macos-aarch64 %s trailing\n" % other,
            "PHP 8.4.27 (macos-aarch64)\tsomething else entirely\n",
            "no tab at all\n",
            "\n",
        )
        self.addCleanup(os.remove, path)
        self.assertEqual(manifest.read_reports(path), {"php-8.4.27-macos-aarch64": {digest}})

    def test_rejects_other_names(self):
        for name in ("evil", "php-8.4.27-windows-x86_64", "php-8.4-macos-aarch64", "logs-php-8.4.27-macos-aarch64"):
            directory, _ = make_artifact(self.root, name=name)
            self.assertRejected(directory, "not named")

    def test_rejects_a_build_the_run_did_not_plan(self):
        directory, _ = make_artifact(self.root, version="8.3.36")
        self.assertRejected(directory, "did not plan")
        directory, _ = make_artifact(self.root, platform="linux-aarch64")
        self.assertRejected(directory, "did not plan")

    def test_rejects_metadata_for_another_build(self):
        # A job may only describe the version and platform it was given.
        self.assertRejected(self.change(version="8.5.12"), "says version '8.5.12'")
        shutil.rmtree(os.path.join(self.root, "php-8.4.27-macos-aarch64"))
        self.assertRejected(self.change(platform="linux-x86_64"), "says platform")
        shutil.rmtree(os.path.join(self.root, "php-8.4.27-macos-aarch64"))
        self.assertRejected(self.change(minor="8.5"), "says minor")
        shutil.rmtree(os.path.join(self.root, "php-8.4.27-macos-aarch64"))
        self.assertRejected(self.change(recipe="f" * 16), "says recipe")

    def test_rejects_unknown_and_missing_keys(self):
        self.assertRejected(self.change(built_at="2020-01-01T00:00:00Z"), "exactly the keys")
        directory, build = make_artifact(self.root, platform="linux-x86_64")
        del build["sources"]
        write_build(directory, build)
        self.assertRejected(directory, "exactly the keys")

    def test_rejects_extra_build_files_and_bad_json(self):
        directory, build = make_artifact(self.root)
        write_build(directory, build, name="php-8.5.12-macos-aarch64.json")
        self.assertRejected(directory, "exactly one build file")
        directory, _ = make_artifact(self.root, platform="linux-x86_64")
        for text in ("{", "[" * 100000, '"a string"', "[]"):
            with open(os.path.join(directory, "php-8.4.27-linux-x86_64.json"), "w") as f:
                f.write(text)
            with self.assertRaises(manifest.Rejected, msg=text[:10]):
                self.check(directory)

    def test_rejects_files_the_build_does_not_list(self):
        directory, _ = make_artifact(self.root)
        with open(os.path.join(directory, "php-8.5.12-cli-macos-aarch64-000000000000.tar.gz"), "w") as f:
            f.write("someone else's archive")
        self.assertRejected(directory, "does not list")

    def test_rejects_directories_and_links(self):
        directory, _ = make_artifact(self.root)
        os.mkdir(os.path.join(directory, "nested"))
        self.assertRejected(directory, "not a regular file")
        os.rmdir(os.path.join(directory, "nested"))
        os.symlink("/etc/hosts", os.path.join(directory, "link.tar.gz"))
        self.assertRejected(directory, "not a regular file")

    def test_rejects_a_changed_archive(self):
        directory, build = make_artifact(self.root)
        with open(os.path.join(directory, build["cli"]["file"]), "ab") as f:
            f.write(b"!")
        self.assertRejected(directory, "bytes, not the")
        with open(os.path.join(directory, build["cli"]["file"]), "wb") as f:
            f.write(b"x" * build["cli"]["size"])
        self.assertRejected(directory, "does not have the sha256")
        os.remove(os.path.join(directory, build["cli"]["file"]))
        self.assertRejected(directory, "is missing")

    def test_rejects_archive_names_of_other_builds(self):
        # The name must carry this build's version, platform and sha256, so a
        # job cannot replace or claim another build's file on the release.
        for wrong in (
            "php-8.5.12-cli-macos-aarch64-{sha}.tar.gz",
            "php-8.4.27-cli-linux-x86_64-{sha}.tar.gz",
            "php-8.4.27-fpm-macos-aarch64-{sha}.tar.gz",
            "php-8.4.27-cli-macos-aarch64.tar.gz",
            "../php-8.4.27-cli-macos-aarch64-{sha}.tar.gz",
            "evil.tar.gz",
        ):
            directory, build = make_artifact(self.root)
            name = wrong.format(sha=build["cli"]["sha256"][:12])
            if "/" not in name:
                os.rename(os.path.join(directory, build["cli"]["file"]), os.path.join(directory, name))
            build["cli"]["file"] = name
            write_build(directory, build)
            self.assertRejected(directory, "is named")
            shutil.rmtree(directory)

    def test_rejects_bad_assets(self):
        directory, build = make_artifact(self.root)
        build["fpm"]["size"] = True
        write_build(directory, build)
        self.assertRejected(directory, "no valid size")
        build["fpm"] = dict(build["cli"], url="https://example.com/")
        write_build(directory, build)
        self.assertRejected(directory, "must have exactly")
        build["fpm"] = "php-fpm"
        write_build(directory, build)
        self.assertRejected(directory, "must have exactly")

    def test_rejects_unknown_shared_extensions(self):
        directory, build = make_artifact(self.root)
        build["shared_extensions"]["backdoor"] = build["shared_extensions"]["xdebug"]
        write_build(directory, build)
        self.assertRejected(directory, "not in shared-extensions.txt")

    def test_rejects_bad_extension_lists(self):
        self.assertRejected(self.change(extensions=["ok", "not ok"]), "bad extensions list")
        shutil.rmtree(os.path.join(self.root, "php-8.4.27-macos-aarch64"))
        self.assertRejected(self.change(extensions="bcmath"), "bad extensions list")
        shutil.rmtree(os.path.join(self.root, "php-8.4.27-macos-aarch64"))
        self.assertRejected(self.change(spc_version="2.8.5 && id"), "bad spc_version")

    def test_rejects_bad_sources(self):
        php = {"file": "php-8.4.27.tar.xz", "sha256": "a" * 64, "signed_by": SIGNER}
        cases = [
            ("not verified", {"php-src": php, "zlib-Darwin-arm64": {"file": "zlib.txz", "sha256": "b" * 64, "unverified": True}}),
            ("which php-src", {"zlib": {"file": "zlib-1.4.tar.gz", "sha256": "b" * 64}}),
            ("bad entry for php-src", {"php-src": dict(php, file="php-8.5.12.tar.xz")}),
            # A key php-keys.txt lists for PHP 8.5, not for 8.4.
            ("bad entry for php-src", {"php-src": dict(php, signed_by="49D9AF6BC72A80D6691719C8AA23F5BE9C7097D4")}),
            ("bad entry for php-src", {"php-src": {"file": "php-8.4.27.tar.xz", "sha256": "a" * 64}}),
            # php-src is a signed release tarball, never a clone of something.
            ("bad entry for php-src", {"php-src": {"dir": "php-src", "git": "https://github.com/someone/php-src", "commit": "c" * 40}}),
            ("bad entry 'zlib'", {"php-src": php, "zlib": "zlib-1.4.tar.gz"}),
            ("bad entry for zlib", {"php-src": php, "zlib": {"file": "zlib-1.4.tar.gz", "sha256": "b" * 64, "url": "https://example.com/"}}),
            ("bad entry for zlib", {"php-src": php, "zlib": {"file": "../zlib-1.4.tar.gz", "sha256": "b" * 64}}),
            ("bad entry for qdbm", {"php-src": php, "qdbm": {"dir": "qdbm", "git": "git@example.com:x", "commit": "c" * 40}}),
            ("bad entry 'a b'", {"php-src": php, "a b": {"file": "x.tar.gz", "sha256": "b" * 64}}),
            ("not a list", {}),
            ("not a list", ["php-src"]),
        ]
        for message, sources in cases:
            directory = self.change(sources=sources)
            self.assertRejected(directory, message)
            shutil.rmtree(directory)


class MergeTest(unittest.TestCase):
    def setUp(self):
        self.root = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, self.root)
        self.artifacts = os.path.join(self.root, "artifacts")
        self.collect = os.path.join(self.root, "dist")
        self.out = os.path.join(self.root, "manifest.json")
        os.mkdir(self.artifacts)
        patcher = mock.patch.object(manifest, "recipe_id", return_value=RECIPE)
        patcher.start()
        self.addCleanup(patcher.stop)

    def merge(self, planned, existing=None, artifacts=True, reports=None):
        argv = ["manifest.py", "merge", "--out", self.out]
        if artifacts:
            if reports is None and os.path.isdir(self.artifacts):
                reports = reports_of(self.artifacts)
                self.addCleanup(os.remove, reports)
            argv += ["--artifacts", self.artifacts, "--matrix", matrix(*planned), "--reports", reports or os.devnull, "--collect", self.collect]
        if existing is not None:
            path = os.path.join(self.root, "previous.json")
            with open(path, "w") as f:
                json.dump(existing, f)
            argv += ["--existing", path]
        with mock.patch("sys.argv", argv), mock.patch("sys.stderr"):
            manifest.main()
        with open(self.out) as f:
            return json.load(f)

    def old_build(self, version, platform="macos-aarch64"):
        return {"version": version, "minor": version.rsplit(".", 1)[0], "platform": platform, "recipe": "old", "built_at": "2026-10-01T00:00:00Z"}

    def test_merges_accepted_builds(self):
        _, mac = make_artifact(self.artifacts)
        _, linux = make_artifact(self.artifacts, platform="linux-x86_64")
        existing = {"schema": 1, "builds": [self.old_build("8.4.26"), self.old_build("8.4.27"), self.old_build("8.3.36")]}
        result = self.merge([("8.4.27", "macos-aarch64"), ("8.4.27", "linux-x86_64"), ("8.5.12", "macos-aarch64")], existing)

        keys = [(b["version"], b["platform"]) for b in result["builds"]]
        self.assertEqual(keys, [("8.4.27", "macos-aarch64"), ("8.4.27", "linux-x86_64"), ("8.4.26", "macos-aarch64"), ("8.3.36", "macos-aarch64")])
        self.assertEqual(result["latest"], {"8.4": {"macos-aarch64": "8.4.27", "linux-x86_64": "8.4.27"}, "8.3": {"macos-aarch64": "8.3.36"}})
        rebuilt = result["builds"][0]
        self.assertEqual(rebuilt["recipe"], RECIPE)  # replaced the old 8.4.27 build
        self.assertEqual(rebuilt["built_at"], result["updated_at"])
        self.assertEqual(result["builds"][2]["built_at"], "2026-10-01T00:00:00Z")
        wanted = [b[k]["file"] for b in (mac, linux) for k in ("cli", "fpm")] + [b["shared_extensions"]["xdebug"]["file"] for b in (mac, linux)]
        self.assertEqual(sorted(os.listdir(self.collect)), sorted(wanted))

    def test_an_artifact_uploaded_by_another_job_stops_the_publish(self):
        # A job replaces the linux build with one of its own making: complete
        # and consistent, but not what the linux job reported having built.
        make_artifact(self.artifacts)
        directory, build = make_artifact(self.artifacts, platform="linux-x86_64")
        reports = reports_of(self.artifacts)
        self.addCleanup(os.remove, reports)
        build["extensions"].append("backdoor")
        write_build(directory, build)
        planned = [("8.4.27", "macos-aarch64"), ("8.4.27", "linux-x86_64")]
        self.assertEqual(manifest.check_artifact(directory, set(planned), RECIPE, {"php-8.4.27-linux-x86_64": {manifest.sha256_file(os.path.join(directory, "php-8.4.27-linux-x86_64.json"))}})[0], build)
        with self.assertRaises(SystemExit) as caught:
            self.merge(planned, reports=reports)
        self.assertIn("php-8.4.27-linux-x86_64: php-8.4.27-linux-x86_64.json is not the build file its build job reported", str(caught.exception))
        self.assertFalse(os.path.exists(self.collect))

    def test_one_bad_artifact_stops_the_publish(self):
        make_artifact(self.artifacts)
        directory, build = make_artifact(self.artifacts, platform="linux-x86_64")
        build["version"] = "8.5.12"  # claims a build of another job
        write_build(directory, build)
        with self.assertRaises(SystemExit) as caught:
            self.merge([("8.4.27", "macos-aarch64"), ("8.4.27", "linux-x86_64")])
        self.assertIn("php-8.4.27-linux-x86_64: ", str(caught.exception))
        self.assertFalse(os.path.exists(self.collect))
        self.assertFalse(os.path.exists(self.out))

    def test_a_stray_file_among_the_artifacts_stops_the_publish(self):
        make_artifact(self.artifacts)
        with open(os.path.join(self.artifacts, "manifest.json"), "w") as f:
            f.write("{}")
        with self.assertRaises(SystemExit) as caught:
            self.merge([("8.4.27", "macos-aarch64")])
        self.assertIn("manifest.json: not named php-<version>-<platform>", str(caught.exception))

    def test_without_artifacts_rewrites_the_existing_manifest(self):
        existing = {"schema": 1, "updated_at": "2026-10-01T00:00:00Z", "builds": [self.old_build("8.4.26")]}
        result = self.merge([], existing, artifacts=False)
        self.assertEqual(result["builds"], existing["builds"])
        self.assertNotEqual(result["updated_at"], existing["updated_at"])
        # The same when the run built nothing and there is no artifacts directory.
        os.rmdir(self.artifacts)
        self.assertEqual(self.merge([], existing)["builds"], existing["builds"])

    def test_refuses_another_schema(self):
        with self.assertRaises(SystemExit) as caught:
            self.merge([], {"schema": 2, "builds": []}, artifacts=False)
        self.assertIn("schema 2", str(caught.exception))


class VersionTest(unittest.TestCase):
    def test_accepts_a_release_of_the_branch(self):
        self.assertEqual(manifest.checked_release("8.4", "8.4.27"), "8.4.27")
        self.assertEqual(manifest.checked_release("8.10", "8.10.0"), "8.10.0")

    def test_rejects_anything_else(self):
        for version in ("8.5.1", "8.4", "8.4.27-dev", "8.4.27\n", "8.4.27; id", "8.4.27/../x", " 8.4.27", "８.４.２７", "", None, 8.4, ["8.4.27"]):
            with self.assertRaises(SystemExit, msg=repr(version)):
                manifest.checked_release("8.4", version)

    def test_latest_release_checks_what_php_net_returns(self):
        def respond(body):
            response = mock.MagicMock()
            response.__enter__.return_value.read.return_value = json.dumps(body).encode()
            return mock.patch.object(manifest.urllib.request, "urlopen", return_value=response)

        with respond({"version": "8.4.27"}):
            self.assertEqual(manifest.latest_release("8.4"), "8.4.27")
        with respond({"version": "8.4.27 $(id)"}), self.assertRaises(SystemExit):
            manifest.latest_release("8.4")
        with respond({"error": "Unknown version"}), self.assertRaises(SystemExit):
            manifest.latest_release("8.4")


class MatrixTest(unittest.TestCase):
    def test_skips_builds_published_with_the_current_recipe(self):
        published = [
            {"version": "8.4.27", "platform": "macos-aarch64", "recipe": RECIPE},
            {"version": "8.4.27", "platform": "linux-x86_64", "recipe": "older"},
        ]
        plan = manifest.plan_matrix(["8.4.27"], ["macos-aarch64", "linux-x86_64"], published, RECIPE)
        self.assertEqual([(e["php"], e["platform"]) for e in plan], [("8.4.27", "linux-x86_64")])

    def test_recipe_covers_the_files_that_decide_a_build(self):
        for name in ("build.sh", "sources.txt", "sources.lock.json", "sources.py", "php-keys.txt"):
            self.assertIn(name, manifest.RECIPE_FILES)
            self.assertTrue(os.path.exists(os.path.join(manifest.HERE, name)), name)


if __name__ == "__main__":
    unittest.main()
