"""Tests for sources.py: python3 -m unittest discover -s php-builds"""

import argparse
import base64
import contextlib
import hashlib
import io
import json
import os
import shutil
import subprocess
import tempfile
import unittest
from unittest import mock

import manifest
import sources

GIT_URL = "https://github.com/static-php/qdbm.git"
RAMSEY = "39B641343D8C104B2B146DC3F9C39DC0B9698544"
BUCKLEY = "9D7F99A0CB8F05C8A6958D6256A97AF7600A39A6"
MANN = "AFD8691FDAEDF03BDF6E460563F15A9B715376CA"

# gpgv --status-fd output, as gpgv 2.4 printed it for real PHP releases.
GOOD = """[GNUPG:] NEWSIG
[GNUPG:] KEY_CONSIDERED 9D7F99A0CB8F05C8A6958D6256A97AF7600A39A6 0
[GNUPG:] SIG_ID 3B76sAkIzsfR+HHMRg2MFITBUpU 2026-10-19 1792421469
[GNUPG:] GOODSIG 56A97AF7600A39A6 Calvin Buckley (PHP) <calvinb@php.net>
[GNUPG:] NOTATION_NAME manu
[GNUPG:] VALIDSIG 9D7F99A0CB8F05C8A6958D6256A97AF7600A39A6 2026-10-19 1792421469 0 4 0 22 10 00 9D7F99A0CB8F05C8A6958D6256A97AF7600A39A6
"""
SUBKEY = """[GNUPG:] NEWSIG
[GNUPG:] KEY_CONSIDERED AFD8691FDAEDF03BDF6E460563F15A9B715376CA 0
[GNUPG:] GOODSIG BEC555E22A143553 Eric A Mann <eric@sixthree.me>
[GNUPG:] VALIDSIG 4B1FC0D9DF92321CED9F615DBEC555E22A143553 2026-10-19 1792418655 0 4 0 1 8 00 AFD8691FDAEDF03BDF6E460563F15A9B715376CA
"""
EXPIRED = """[GNUPG:] NEWSIG
[GNUPG:] KEY_CONSIDERED 39B641343D8C104B2B146DC3F9C39DC0B9698544 0
[GNUPG:] KEYEXPIRED 1763999470
[GNUPG:] SIG_ID P1RTGANeKK8Apl+nmK2jd1bPw2M 2025-12-16 1765910406
[GNUPG:] EXPKEYSIG F9C39DC0B9698544 Ben Ramsey <ramsey@php.net>
[GNUPG:] VALIDSIG 39B641343D8C104B2B146DC3F9C39DC0B9698544 2025-12-16 1765910406 0 4 0 1 8 00 39B641343D8C104B2B146DC3F9C39DC0B9698544
"""
BAD = """[GNUPG:] NEWSIG
[GNUPG:] KEY_CONSIDERED 9D7F99A0CB8F05C8A6958D6256A97AF7600A39A6 0
[GNUPG:] BADSIG 56A97AF7600A39A6 Calvin Buckley (PHP) <calvinb@php.net>
"""


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def run_git(path, *args):
    subprocess.run(
        ("git", "-C", path, "-c", "user.name=test", "-c", "user.email=test@example.invalid", "-c", "commit.gpgsign=false") + args,
        check=True,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )


def head(path):
    return subprocess.run(("git", "-C", path, "rev-parse", "HEAD"), check=True, stdout=subprocess.PIPE, universal_newlines=True).stdout.strip()


class Downloads:
    """A downloads/ directory as spc leaves it, with pins that match it."""

    def __init__(self, test):
        self.path = tempfile.mkdtemp()
        test.addCleanup(shutil.rmtree, self.path)
        self.spc = {}
        self.pins = []
        self.lock = {"sources": {}, "tools": {}}

        self.archive("php-src", "php-8.4.27.tar.xz", b"php")
        zlib = self.archive("zlib", "zlib-1.4.tar.gz", b"zlib")
        self.lock["sources"]["zlib"] = {"url": "https://example.com/zlib-1.4.tar.gz", "sha256": zlib}
        redis = self.archive("redis", "redis.tgz", b"redis")
        self.pins.append(("redis", "https://pecl.php.net/get/redis-6.3.0.tgz", redis, []))
        tool = self.archive("pkg-config-aarch64-macos", "pkg-config-aarch64-darwin.txz", b"pkg-config", lock_as=3)
        self.lock["tools"]["pkg-config-aarch64-macos"] = {"url": "https://example.com/pkg-config.txz", "sha256": tool}

        self.repo = os.path.join(self.path, "qdbm")
        os.mkdir(self.repo)
        run_git(self.repo, "init", "-q")
        run_git(self.repo, "remote", "add", "origin", GIT_URL)
        self.commits = []
        for text in ("one", "two"):
            with open(os.path.join(self.repo, "file"), "w") as f:
                f.write(text)
            run_git(self.repo, "add", "file")
            run_git(self.repo, "commit", "-q", "-m", text)
            self.commits.append(head(self.repo))
        self.spc["qdbm"] = {"source_type": "git", "dirname": "qdbm", "move_path": None, "lock_as": 1, "hash": self.commits[1]}
        self.lock["sources"]["qdbm"] = {"git": GIT_URL, "commit": self.commits[1]}
        self.save()

    def archive(self, name, file, data, lock_as=1):
        with open(os.path.join(self.path, file), "wb") as f:
            f.write(data)
        self.spc[name] = {"source_type": "archive", "filename": file, "move_path": None, "lock_as": lock_as, "hash": "sha1"}
        return sha256(data)

    def save(self):
        with open(os.path.join(self.path, ".lock.json"), "w") as f:
            json.dump(self.spc, f)

    def check(self, **kwargs):
        self.save()
        with mock.patch.object(sources, "check_php_src", return_value=BUCKLEY):
            return sources.check_sources(self.path, "8.4.27", "macos", pins=self.pins, lock=self.lock, **kwargs)


class VerifyTest(unittest.TestCase):
    def setUp(self):
        self.d = Downloads(self)

    def assertFails(self, message, **kwargs):
        with self.assertRaises(SystemExit) as caught:
            self.d.check(**kwargs)
        self.assertIn(message, str(caught.exception))

    def test_records_everything_that_matches(self):
        record = self.d.check()
        self.assertEqual(
            record,
            {
                "php-src": {"file": "php-8.4.27.tar.xz", "sha256": sha256(b"php"), "signed_by": BUCKLEY},
                "pkg-config-aarch64-macos": {"file": "pkg-config-aarch64-darwin.txz", "sha256": sha256(b"pkg-config")},
                "qdbm": {"dir": "qdbm", "git": GIT_URL, "commit": self.d.commits[1]},
                "redis": {"file": "redis.tgz", "sha256": sha256(b"redis")},
                "zlib": {"file": "zlib-1.4.tar.gz", "sha256": sha256(b"zlib")},
            },
        )

    def test_a_changed_archive_fails(self):
        with open(os.path.join(self.d.path, "zlib-1.4.tar.gz"), "wb") as f:
            f.write(b"zlib with a backdoor")
        self.assertFails("zlib: zlib-1.4.tar.gz has sha256")

    def test_a_changed_hand_pinned_archive_fails(self):
        with open(os.path.join(self.d.path, "redis.tgz"), "wb") as f:
            f.write(b"other")
        self.assertFails("redis: redis.tgz has sha256")

    def test_an_unpinned_source_fails(self):
        self.d.archive("newlib", "newlib-1.0.tar.gz", b"new")
        self.assertFails("newlib (newlib-1.0.tar.gz) is not pinned")
        self.assertFails("--refresh-lock --add")

    def test_a_pin_that_does_not_apply_does_not_count(self):
        self.d.pins[0] = self.d.pins[0][:3] + (["linux"],)
        self.assertFails("redis (redis.tgz) is not pinned")

    def test_a_changed_or_unknown_tool_fails(self):
        self.d.archive("zig-aarch64-linux", "zig.tar.xz", b"zig", lock_as=3)
        self.assertFails("zig-aarch64-linux (zig.tar.xz) is not in sources.lock.json")
        del self.d.spc["zig-aarch64-linux"]
        with open(os.path.join(self.d.path, "pkg-config-aarch64-darwin.txz"), "wb") as f:
            f.write(b"another pkg-config")
        self.assertFails("pkg-config-aarch64-macos: pkg-config-aarch64-darwin.txz has sha256")

    def test_a_git_source_at_another_commit_fails(self):
        run_git(self.d.repo, "checkout", "-q", "--detach", self.d.commits[0])
        self.assertFails("qdbm is at commit %s, locked is %s" % (self.d.commits[0], self.d.commits[1]))

    def test_a_git_source_with_other_files_than_its_commit_fails(self):
        with open(os.path.join(self.d.repo, "file"), "w") as f:
            f.write("patched")
        self.assertFails("qdbm has files that are not its commit's")
        run_git(self.d.repo, "checkout", "-q", "file")
        self.d.check()
        with open(os.path.join(self.d.repo, "extra"), "w") as f:
            f.write("new file")
        self.assertFails("qdbm has files that are not its commit's")
        os.remove(os.path.join(self.d.repo, "extra"))
        # Files git ignores count too: plain `git status` would not show them.
        with open(os.path.join(self.d.repo, ".git", "info", "exclude"), "a") as f:
            f.write("*.o\nout/\n")
        with open(os.path.join(self.d.repo, "stale.o"), "w") as f:
            f.write("object")
        self.assertFails("qdbm has files that are not its commit's")
        os.remove(os.path.join(self.d.repo, "stale.o"))
        os.mkdir(os.path.join(self.d.repo, "out"))
        with open(os.path.join(self.d.repo, "out", "generated.c"), "w") as f:
            f.write("int x;")
        self.assertFails("qdbm has files that are not its commit's")

    def test_a_git_source_from_another_remote_fails(self):
        run_git(self.d.repo, "remote", "set-url", "origin", "https://github.com/someone/qdbm.git")
        self.assertFails("qdbm was cloned from https://github.com/someone/qdbm.git")

    def test_the_kind_of_source_must_match_the_pin(self):
        self.d.lock["sources"]["zlib"] = {"git": GIT_URL, "commit": "c" * 40}
        self.assertFails("zlib is locked as a git source but was downloaded as zlib-1.4.tar.gz")
        self.d.lock["sources"]["zlib"] = {"url": "https://example.com/zlib-1.4.tar.gz", "sha256": sha256(b"zlib")}
        self.d.lock["sources"]["qdbm"] = {"url": "https://example.com/qdbm.tar.gz", "sha256": "d" * 64}
        self.assertFails("qdbm is pinned as an archive but was cloned with git")

    def test_prebuilt_libraries_fail_unless_allowed_and_are_marked(self):
        self.d.archive("zlib-Darwin-arm64-none-default", "zlib-prebuilt.txz", b"binary", lock_as=2)
        self.assertFails("zlib-Darwin-arm64-none-default is a pre-built library")
        record = self.d.check(allow_unverified=True)
        self.assertEqual(record["zlib-Darwin-arm64-none-default"], {"file": "zlib-prebuilt.txz", "sha256": sha256(b"binary"), "unverified": True})
        self.assertNotIn("unverified", record["zlib"])

    def test_unverified_covers_what_the_lock_pins_but_never_tools(self):
        with open(os.path.join(self.d.path, "zlib-1.4.tar.gz"), "wb") as f:
            f.write(b"whatever spc picked today")
        record = self.d.check(allow_unverified=True)
        self.assertTrue(record["zlib"]["unverified"])
        with open(os.path.join(self.d.path, "pkg-config-aarch64-darwin.txz"), "wb") as f:
            f.write(b"another pkg-config")
        self.assertFails("pkg-config-aarch64-macos: pkg-config-aarch64-darwin.txz has sha256", allow_unverified=True)

    def test_php_src_is_required_and_checked(self):
        self.d.save()
        with mock.patch.object(sources, "check_php_src", side_effect=SystemExit("error: bad signature")):
            with self.assertRaises(SystemExit) as caught:
                sources.check_sources(self.d.path, "8.4.27", "macos", allow_unverified=True, pins=self.d.pins, lock=self.d.lock)
        self.assertIn("bad signature", str(caught.exception))
        del self.d.spc["php-src"]
        self.assertFails("php-src was not downloaded")

    def test_missing_and_odd_entries_fail(self):
        os.remove(os.path.join(self.d.path, "zlib-1.4.tar.gz"))
        self.assertFails("zlib-1.4.tar.gz is missing")
        self.d.spc["zlib"]["filename"] = "../zlib-1.4.tar.gz"
        self.assertFails("unexpected filename")
        self.d.spc["zlib"] = {"source_type": "local", "dirname": "/somewhere", "lock_as": 1}
        self.assertFails("cannot be verified")

    def test_unchanged_notices_new_and_changed_downloads(self):
        record = os.path.join(self.d.path, "record.json")
        with open(record, "w") as f:
            json.dump(self.d.check(), f)
        args = argparse.Namespace(downloads=self.d.path, record=record)
        sources.unchanged(args)
        self.d.archive("late", "late.tar.gz", b"fetched while building")
        self.d.save()
        with self.assertRaises(SystemExit) as caught:
            sources.unchanged(args)
        self.assertIn("late: null ->", str(caught.exception))
        del self.d.spc["late"]
        self.d.save()
        sources.unchanged(args)
        with open(os.path.join(self.d.path, "zlib-1.4.tar.gz"), "wb") as f:
            f.write(b"swapped")
        with self.assertRaises(SystemExit) as caught:
            sources.unchanged(args)
        self.assertIn("zlib: ", str(caught.exception))

    def test_unchanged_notices_an_edited_clone(self):
        record = os.path.join(self.d.path, "record.json")
        with open(record, "w") as f:
            json.dump(self.d.check(), f)
        args = argparse.Namespace(downloads=self.d.path, record=record)
        with open(os.path.join(self.d.repo, "file"), "w") as f:
            f.write("patched while building")
        with self.assertRaises(SystemExit) as caught:
            sources.unchanged(args)
        self.assertIn("qdbm has files that are not its commit's", str(caught.exception))
        run_git(self.d.repo, "checkout", "-q", "file")
        sources.unchanged(args)
        with open(os.path.join(self.d.repo, "added.c"), "w") as f:
            f.write("int y;")
        with self.assertRaises(SystemExit):
            sources.unchanged(args)


class CommandTest(unittest.TestCase):
    def setUp(self):
        self.d = Downloads(self)
        for name, value in (("read_pins", self.d.pins), ("read_lock", self.d.lock)):
            patcher = mock.patch.object(sources, name, return_value=value)
            patcher.start()
            self.addCleanup(patcher.stop)

    def output(self, function, **args):
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            function(argparse.Namespace(**args))
        return out.getvalue().splitlines()

    def test_download_args_pin_every_archive(self):
        lines = self.output(sources.download_args, minor="8.4", os="macos", downloads=None, hand_pinned_only=False)
        self.assertEqual(lines, ["-U", "redis:https://pecl.php.net/get/redis-6.3.0.tgz", "-U", "zlib:https://example.com/zlib-1.4.tar.gz"])

    def test_download_args_leave_out_what_is_already_there(self):
        with open(os.path.join(self.d.path, "redis.tgz"), "wb") as f:
            f.write(b"half a download")
        lines = self.output(sources.download_args, minor="8.4", os="macos", downloads=self.d.path, hand_pinned_only=False)
        self.assertEqual(lines, ["-U", "redis:https://pecl.php.net/get/redis-6.3.0.tgz"])
        os.remove(os.path.join(self.d.path, "redis.tgz"))
        lines = self.output(sources.download_args, minor="8.4", os="macos", downloads=self.d.path, hand_pinned_only=True)
        self.assertEqual(lines, ["-U", "redis:https://pecl.php.net/get/redis-6.3.0.tgz"])

    def test_checkout_moves_a_clone_to_its_locked_commit(self):
        self.d.lock["sources"]["qdbm"]["commit"] = self.d.commits[0]
        self.output(sources.checkout, downloads=self.d.path, minor="8.4", os="macos")
        self.assertEqual(head(self.d.repo), self.d.commits[0])
        self.assertEqual(sources.read_spc_lock(self.d.path)["qdbm"]["hash"], self.d.commits[0])
        self.assertEqual(self.d.check()["qdbm"]["commit"], self.d.commits[0])

    def test_checkout_leaves_nothing_of_the_branch_tip_behind(self):
        # The locked commit ignores build/ and *.o. The tip of the branch has
        # since put a submodule at build/ and an object file next to the code:
        # git would leave both where they are, and not mention them.
        repo = self.d.repo
        with open(os.path.join(repo, ".gitignore"), "w") as f:
            f.write("build/\n*.o\n")
        run_git(repo, "add", ".gitignore")
        run_git(repo, "commit", "-q", "-m", "locked")
        locked = head(repo)
        library = os.path.join(self.d.path, "library")
        os.mkdir(library)
        run_git(library, "init", "-q")
        with open(os.path.join(library, "CMakeLists.txt"), "w") as f:
            f.write("# from the tip")
        run_git(library, "add", "CMakeLists.txt")
        run_git(library, "commit", "-q", "-m", "library")
        run_git(repo, "-c", "protocol.file.allow=always", "submodule", "add", "-f", library, "build")
        run_git(repo, "commit", "-q", "-m", "tip")
        with open(os.path.join(repo, "stale.o"), "w") as f:
            f.write("object")
        self.assertTrue(os.path.exists(os.path.join(repo, "build", "CMakeLists.txt")))
        self.d.spc["qdbm"]["hash"] = head(repo)
        self.d.lock["sources"]["qdbm"]["commit"] = locked
        self.d.save()

        self.output(sources.checkout, downloads=self.d.path, minor="8.4", os="macos")
        self.assertEqual(head(repo), locked)
        self.assertEqual(sorted(os.listdir(repo)), [".git", ".gitignore", "file"])
        self.assertEqual(sources.git_problems(repo), [])
        self.assertEqual(self.d.check()["qdbm"]["commit"], locked)

    def test_files_git_ignores_inside_a_submodule_count(self):
        repo = self.d.repo
        library = os.path.join(self.d.path, "library")
        os.mkdir(library)
        run_git(library, "init", "-q")
        with open(os.path.join(library, ".gitignore"), "w") as f:
            f.write("*.o\n")
        run_git(library, "add", ".gitignore")
        run_git(library, "commit", "-q", "-m", "library")
        run_git(repo, "-c", "protocol.file.allow=always", "submodule", "add", library, "vendor")
        run_git(repo, "commit", "-q", "-m", "with a submodule")
        self.assertEqual(sources.git_problems(repo), [])
        with open(os.path.join(repo, "vendor", "stale.o"), "w") as f:
            f.write("object")
        self.assertEqual(sources.git_problems(repo), ["has files in a submodule that are not its commit's"])
        os.remove(os.path.join(repo, "vendor", "stale.o"))
        run_git(os.path.join(repo, "vendor"), "commit", "-q", "--allow-empty", "-m", "moved on")
        self.assertIn("has submodule vendor at another commit than recorded", sources.git_problems(repo))

    def test_checkout_fails_for_a_commit_that_does_not_exist(self):
        self.d.lock["sources"]["qdbm"]["commit"] = "0" * 40
        run_git(self.d.repo, "remote", "set-url", "origin", os.path.join(self.d.path, "no-such-remote"))
        with self.assertRaises(SystemExit) as caught:
            self.output(sources.checkout, downloads=self.d.path, minor="8.4", os="macos")
        self.assertIn("git fetch", str(caught.exception))

    def test_tool_url(self):
        self.assertEqual(self.output(sources.tool_url, name="pkg-config-aarch64-macos"), ["https://example.com/pkg-config.txz"])
        with self.assertRaises(SystemExit):
            self.output(sources.tool_url, name="zig-aarch64-linux")


class PinsTest(unittest.TestCase):
    def test_conditions(self):
        self.assertTrue(sources.applies([], "8.4", "macos"))
        self.assertTrue(sources.applies([">=8.3", "macos"], "8.3", "macos"))
        self.assertFalse(sources.applies([">=8.3", "macos"], "8.3", "linux"))
        self.assertFalse(sources.applies([">=8.3"], "8.2", "macos"))
        self.assertTrue(sources.applies(["<8.5"], "8.4", "linux"))
        self.assertFalse(sources.applies(["<8.5"], "8.5", "linux"))
        self.assertTrue(sources.applies([">=8.9"], "8.10", "linux"))
        with self.assertRaises(SystemExit):
            sources.applies(["windows"], "8.4", "linux")

    def write(self, text):
        handle, path = tempfile.mkstemp()
        self.addCleanup(os.remove, path)
        with os.fdopen(handle, "w") as f:
            f.write(text)
        return path

    def test_reads_pins_and_refuses_what_a_shell_could_run(self):
        good = "redis https://pecl.php.net/get/redis-6.3.0.tgz %s <8.5 # comment\n" % ("a" * 64)
        self.assertEqual(sources.read_pins(self.write("# x\n\n" + good)), [("redis", "https://pecl.php.net/get/redis-6.3.0.tgz", "a" * 64, ["<8.5"])])
        for bad in (
            'redis https://example.com/x.tgz"$(id) %s\n' % ("a" * 64),
            "redis https://example.com/x`id`.tgz %s\n" % ("a" * 64),
            "redis http://example.com/x.tgz %s\n" % ("a" * 64),
            "re:dis https://example.com/x.tgz %s\n" % ("a" * 64),
            "redis https://example.com/x.tgz abc\n",
            "redis https://example.com/x.tgz\n",
        ):
            with self.assertRaises(SystemExit, msg=bad):
                sources.read_pins(self.write(bad))

    def test_reads_the_lock_and_refuses_odd_entries(self):
        good = {"sources": {"a": {"url": "https://example.com/a.tar.gz", "sha256": "a" * 64}, "b": {"git": GIT_URL, "commit": "c" * 40}}, "tools": {}}
        self.assertEqual(sources.read_lock(self.write(json.dumps(good))), good)
        for bad in (
            {"sources": {}},
            {"sources": {"a": {"url": "https://example.com/a.tar.gz"}}, "tools": {}},
            {"sources": {"a": {"url": "https://example.com/a.tar.gz", "sha256": "a" * 64, "commit": "c" * 40}}, "tools": {}},
            {"sources": {"a": {"git": "https://example.com/$(id)", "commit": "c" * 40}}, "tools": {}},
            {"sources": {}, "tools": {"zig": {"git": GIT_URL, "commit": "c" * 40}}},
        ):
            with self.assertRaises(SystemExit, msg=bad):
                sources.read_lock(self.write(json.dumps(bad)))

    def test_one_pin_per_source(self):
        lock = {"sources": {"zlib": {"url": "https://example.com/zlib.tar.gz", "sha256": "a" * 64}}, "tools": {}}
        pins = [("redis", "https://example.com/1.tgz", "a" * 64, [">=8.3"]), ("redis", "https://example.com/2.tgz", "b" * 64, ["<8.3"])]
        self.assertEqual(sources.pinned_sources("8.2", "linux", pins, lock)["redis"]["url"], "https://example.com/2.tgz")
        with self.assertRaises(SystemExit):  # both lines apply
            sources.pinned_sources("8.4", "linux", pins + [("redis", "https://example.com/3.tgz", "c" * 64, [])], lock)
        with self.assertRaises(SystemExit):  # by hand and in the lock
            sources.pinned_sources("8.4", "linux", [("zlib", "https://example.com/z.tgz", "a" * 64, [])], lock)

    def test_the_committed_files_are_consistent(self):
        lock = sources.read_lock()
        by_hand = {name for name, _url, _digest, _conditions in sources.read_pins()}
        self.assertFalse(by_hand & set(lock["sources"]))
        for minor in ("8.1", "8.2", "8.3", "8.4", "8.5", "8.6"):
            for os_name in ("macos", "linux"):
                pinned = sources.pinned_sources(minor, os_name)
                files = [os.path.basename(pin["url"]) for pin in pinned.values() if "url" in pin]
                self.assertEqual(len(files), len(set(files)), "two sources would be saved under one name")
                self.assertTrue(sources.read_keys(minor), "no release managers for PHP " + minor)
                # What a build records from these pins must be what
                # `manifest.py merge` accepts, or the publish stops there.
                for name, pin in pinned.items():
                    self.assertTrue(manifest.matches(manifest.SOURCE_RE, name), name)
                    if "url" in pin:
                        self.assertTrue(manifest.matches(manifest.SOURCE_RE, os.path.basename(pin["url"])), pin["url"])
                    else:
                        self.assertTrue(manifest.matches(manifest.GIT_URL_RE, pin["git"]), pin["git"])
        for name in lock["tools"]:
            self.assertTrue(manifest.matches(manifest.SOURCE_RE, name), name)


class SignatureTest(unittest.TestCase):
    KEYS = {BUCKLEY: False, MANN: False, RAMSEY: False}

    def assertRefused(self, status, message, keys=None):
        with self.assertRaises(SystemExit) as caught:
            sources.signer_of(status, self.KEYS if keys is None else keys)
        self.assertIn(message, str(caught.exception))

    def test_accepts_a_good_signature_from_a_listed_key(self):
        self.assertEqual(sources.signer_of(GOOD, self.KEYS), BUCKLEY)

    def test_matches_the_primary_key_when_a_subkey_signed(self):
        self.assertEqual(sources.signer_of(SUBKEY, self.KEYS), MANN)
        self.assertRefused(SUBKEY, "does not list", {"4B1FC0D9DF92321CED9F615DBEC555E22A143553": False})

    def test_refuses_a_key_that_is_not_listed_for_the_branch(self):
        self.assertRefused(GOOD, "signed by %s, which php-keys.txt does not list" % BUCKLEY, {MANN: False})
        self.assertRefused(GOOD, "does not list", {})

    def test_refuses_a_bad_signature(self):
        self.assertRefused(BAD, "BADSIG")
        self.assertRefused(GOOD + BAD.replace("[GNUPG:] NEWSIG\n", ""), "BADSIG")

    def test_an_expired_key_needs_expired_ok(self):
        self.assertRefused(EXPIRED, "has expired")
        self.assertEqual(sources.signer_of(EXPIRED, {RAMSEY: True}), RAMSEY)

    def test_refuses_revoked_keys_expired_signatures_and_errors(self):
        for kind in ("REVKEYSIG", "EXPSIG", "ERRSIG"):
            status = EXPIRED.replace("EXPKEYSIG", kind)
            self.assertRefused(status, kind, {RAMSEY: True})

    def test_refuses_no_or_several_signatures(self):
        self.assertRefused("", "could not be verified")
        self.assertRefused("gpgv: something unrelated\n", "could not be verified")
        self.assertRefused(GOOD + SUBKEY, "could not be verified")
        self.assertRefused("[GNUPG:] VALIDSIG " + BUCKLEY + "\n", "could not be verified")

    def test_reads_the_committed_keys(self):
        self.assertEqual(set(sources.read_keys("8.4")), {MANN, BUCKLEY, "0616E93D95AF471243E26761770426E17EBBB3DD"})
        self.assertFalse(any(sources.read_keys("8.4").values()))
        self.assertTrue(sources.read_keys("8.1")[RAMSEY])
        self.assertFalse(sources.read_keys("8.2")[RAMSEY])
        self.assertEqual(sources.read_keys("9.9"), {})

    def test_dearmor(self):
        packets = bytes(range(200))
        body = base64.b64encode(packets).decode()
        armored = "-----BEGIN PGP PUBLIC KEY BLOCK-----\nComment: x\n\n%s\n%s\n=AAAA\n-----END PGP PUBLIC KEY BLOCK-----\n" % (body[:64], body[64:])
        self.assertEqual(sources.dearmor((armored + "\n" + armored).encode()), packets + packets)
        self.assertEqual(sources.dearmor(("A keyring.\n\n" + armored).encode()), packets)
        self.assertEqual(sources.dearmor(b"\x99\x01binary"), b"\x99\x01binary")
        with self.assertRaises(SystemExit):
            sources.dearmor(b"-----BEGIN PGP SIGNATURE-----\n\nabcd\n-----END PGP SIGNATURE-----\n")


class PhpSrcTest(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, self.dir)
        self.path = os.path.join(self.dir, "php-8.4.27.tar.xz")
        with open(self.path, "wb") as f:
            f.write(b"php")
        self.entry = {"file": "php-8.4.27.tar.xz", "sha256": sha256(b"php")}
        self.release = {"version": "8.4.27", "source": [{"filename": "php-8.4.27.tar.xz", "sha256": sha256(b"php")}]}
        self.fetched = []

    def fetch(self, url, token=None):
        self.fetched.append(url)
        if "releases/index.php" in url:
            return json.dumps(self.release).encode()
        return b"\x99binary keyring or signature"

    def check(self, status=GOOD, version="8.4.27"):
        with mock.patch.object(sources, "fetch", self.fetch), mock.patch.object(sources, "run_gpgv", return_value=status) as gpgv:
            signer = sources.check_php_src(self.path, self.entry, version)
        self.assertEqual(gpgv.call_args[0][2], self.path)
        return signer

    def test_accepts_a_published_signed_tarball(self):
        self.assertEqual(self.check(), BUCKLEY)
        self.assertEqual(
            self.fetched,
            [
                "https://www.php.net/releases/index.php?json&version=8.4.27",
                "https://www.php.net/distributions/php-keyring.gpg",
                "https://www.php.net/distributions/php-8.4.27.tar.xz.asc",
            ],
        )

    def test_refuses_another_sha256_than_php_net_publishes(self):
        self.release["source"][0]["sha256"] = "0" * 64
        with self.assertRaises(SystemExit) as caught:
            self.check()
        self.assertIn("php.net publishes", str(caught.exception))

    def test_refuses_a_tarball_php_net_does_not_list(self):
        for release in ({"version": "8.4.27", "source": []}, {"error": "Unknown version"}, {"version": "8.4.28", "source": self.release["source"]}):
            self.release = release
            with self.assertRaises(SystemExit) as caught:
                self.check()
            self.assertIn("php.net publishes no sha256 for php-8.4.27.tar.xz", str(caught.exception))

    def test_refuses_another_version_than_asked_for(self):
        with self.assertRaises(SystemExit) as caught:
            self.check(version="8.4.26")
        self.assertIn("expected the PHP 8.4.26 tarball", str(caught.exception))

    def test_refuses_a_signature_by_another_branch_s_manager(self):
        with self.assertRaises(SystemExit) as caught:
            self.check(status=EXPIRED)  # Ben Ramsey does not sign 8.4
        self.assertIn("php-keys.txt does not list", str(caught.exception))

    def test_refuses_a_bad_signature(self):
        with self.assertRaises(SystemExit) as caught:
            self.check(status=BAD)
        self.assertIn("BADSIG", str(caught.exception))


class RefreshTest(unittest.TestCase):
    def test_github_tarballs_become_archive_urls(self):
        self.assertEqual(
            sources.pinnable_url("https://api.github.com/repos/google/brotli/tarball/refs/tags/v1.2.1"),
            "https://github.com/google/brotli/archive/refs/tags/v1.2.1/brotli-v1.2.1.tar.gz",
        )
        self.assertEqual(
            sources.pinnable_url("https://api.github.com/repos/php/frankenphp/tarball/caddy/v1.13.1"),
            "https://github.com/php/frankenphp/archive/refs/tags/caddy/v1.13.1/frankenphp-caddy-v1.13.1.tar.gz",
        )

    def test_github_release_assets_become_download_urls(self):
        asset = json.dumps({"browser_download_url": "https://github.com/madler/zlib/releases/download/v1.4/zlib-1.4.tar.gz"}).encode()
        with mock.patch.object(sources, "fetch", return_value=asset) as fetch:
            url = sources.pinnable_url("https://api.github.com/repos/madler/zlib/releases/assets/712419831", "token")
        self.assertEqual(url, "https://github.com/madler/zlib/releases/download/v1.4/zlib-1.4.tar.gz")
        fetch.assert_called_once_with("https://api.github.com/repos/madler/zlib/releases/assets/712419831", "token")

    def test_pecl_latest_becomes_the_release_it_is(self):
        headers = mock.Mock(stdout="HTTP/2 200 \r\ncontent-disposition: attachment;filename=zip-1.22.8.tgz\r\n")
        with mock.patch.object(sources.subprocess, "run", return_value=headers):
            self.assertEqual(sources.pinnable_url("https://pecl.php.net/get/zip"), "https://pecl.php.net/get/zip-1.22.8.tgz")
        with mock.patch.object(sources.subprocess, "run", return_value=mock.Mock(stdout="HTTP/2 200 \r\n")):
            with self.assertRaises(SystemExit):
                sources.pinnable_url("https://pecl.php.net/get/zip")

    def test_other_urls_stay(self):
        for url in ("https://ftp.gnu.org/gnu/gmp/gmp-6.3.0.tar.xz", "https://pecl.php.net/get/zip-1.22.8.tgz", "https://ziglang.org/download/0.17.1/zig-x86_64-linux-0.17.1.tar.xz"):
            self.assertEqual(sources.pinnable_url(url), url)


if __name__ == "__main__":
    unittest.main()
