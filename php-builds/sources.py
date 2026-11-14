#!/usr/bin/env python3
"""Pins, verifies and records everything a PHP build downloads (README.md, "Sources").

spc verifies nothing it downloads and takes the newest release of most
sources. build.sh therefore tells it exactly what to fetch, checks the result
before anything is compiled or run, and records what was used:

  sources.py download-args MINOR OS [--downloads DIR] [--hand-pinned-only]
      Prints the `spc download` arguments (one per line) that pin every
      archive source to its URL in sources.txt or sources.lock.json. With
      --downloads, sources that are already there with the right sha256 are
      left out, so spc keeps them instead of fetching them again.

  sources.py url NAME
      Prints the locked URL of a prebuilt tool (for `spc install-pkg -U`).

  sources.py verify-tools DOWNLOADS
      Checks the prebuilt tools spc has downloaded so far (pkg-config, zig)
      against sources.lock.json. Run before anything executes them.

  sources.py checkout DOWNLOADS MINOR OS
      Moves every git source spc cloned to its locked commit.

  sources.py verify DOWNLOADS VERSION OS --record FILE [--allow-unverified]
      Fails unless every downloaded source is pinned and matches: archives
      by sha256, git sources by commit, php-src by the sha256 php.net
      publishes and by the release managers' OpenPGP signature
      (php-keys.txt). Writes what was used to FILE, for the build metadata.
      --allow-unverified (SPC_PREBUILT=1 builds) records what does not match
      the lock as unverified instead; manifest.py never publishes that.

  sources.py unchanged DOWNLOADS FILE
      Fails if the downloads differ from what `verify` recorded in FILE
      (something was fetched or changed while building).

  sources.py refresh --spc SPC --work DIR [--only NAMES] [--add NAMES]
                     [--add-tool NAMES]
      Rewrites sources.lock.json: lets spc pick the current release of every
      locked source, as it would without pins, and locks what it picked.
      Review the diff before committing it. See README.md.
"""

import argparse
import base64
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
PINS_FILE = os.path.join(HERE, "sources.txt")
LOCK_FILE = os.path.join(HERE, "sources.lock.json")
KEYS_FILE = os.path.join(HERE, "php-keys.txt")

PHP_NET = "https://www.php.net"

# spc's .lock.json: what an entry was downloaded as.
SPC_SOURCE = 1
SPC_PREBUILT = 2
SPC_PACKAGE = 3

# Names and URLs end up on spc's command line, and spc puts URLs inside a
# double-quoted shell string: nothing a shell could interpret.
NAME_RE = re.compile(r"[A-Za-z0-9][A-Za-z0-9._+-]*")
URL_RE = re.compile(r"https://[A-Za-z0-9.-]+/[A-Za-z0-9._~/%+=,@-]*")
SHA256_RE = re.compile(r"[0-9a-f]{64}")
COMMIT_RE = re.compile(r"[0-9a-f]{40}")
FINGERPRINT_RE = re.compile(r"[0-9A-F]{40}")
VERSION_RE = re.compile(r"\d+\.\d+\.\d+")

# What spc can extract (FileSystem::extractArchive picks by extension).
ARCHIVE_EXTENSIONS = (".tar", ".tar.gz", ".tgz", ".tar.xz", ".txz", ".tar.bz2", ".zip")


def die(message):
    raise SystemExit("error: " + message)


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def minor_of(version):
    if not VERSION_RE.fullmatch(version):
        die("'%s' is not a full PHP version" % version)
    return version.rsplit(".", 1)[0]


def version_key(version):
    return tuple(int(p) for p in version.split("."))


# --- pins ---------------------------------------------------------------------


def applies(conditions, minor, os_name):
    """Whether a list-file line applies to this build (as `applies` in build.sh)."""
    for cond in conditions:
        if cond in ("macos", "linux"):
            if cond != os_name:
                return False
            continue
        m = re.fullmatch(r"(>=|<)(\d+\.\d+)", cond)
        if not m:
            die("bad condition '%s'" % cond)
        newer_or_same = version_key(minor) >= version_key(m.group(2))
        if newer_or_same != (m.group(1) == ">="):
            return False
    return True


def read_pins(path=PINS_FILE):
    """sources.txt as [(name, url, sha256, conditions)]."""
    pins = []
    with open(path) as f:
        for number, line in enumerate(f, 1):
            fields = line.split("#", 1)[0].split()
            if not fields:
                continue
            where = "%s:%d" % (os.path.basename(path), number)
            if len(fields) < 3:
                die("%s: expected <name> <url> <sha256> [conditions]" % where)
            name, url, digest = fields[:3]
            check_archive_pin(where, name, url, digest)
            pins.append((name, url, digest, fields[3:]))
    return pins


def check_archive_pin(where, name, url, digest):
    if not NAME_RE.fullmatch(name):
        die("%s: bad source name '%s'" % (where, name))
    if not URL_RE.fullmatch(url):
        die("%s: %s has an unusable url '%s'" % (where, name, url))
    if not SHA256_RE.fullmatch(digest):
        die("%s: %s has no valid sha256" % (where, name))


def read_lock(path=LOCK_FILE):
    """sources.lock.json, checked: {"sources": {...}, "tools": {...}}."""
    with open(path) as f:
        lock = json.load(f)
    where = os.path.basename(path)
    if set(lock) != {"sources", "tools"}:
        die("%s: expected exactly the keys 'sources' and 'tools'" % where)
    for name, entry in lock["sources"].items():
        if set(entry) == {"git", "commit"}:
            if not NAME_RE.fullmatch(name):
                die("%s: bad source name '%s'" % (where, name))
            if not URL_RE.fullmatch(entry["git"]):
                die("%s: %s has an unusable git url" % (where, name))
            if not COMMIT_RE.fullmatch(entry["commit"]):
                die("%s: %s has no valid commit" % (where, name))
        elif set(entry) == {"url", "sha256"}:
            check_archive_pin(where, name, entry["url"], entry["sha256"])
        else:
            die("%s: %s must have either url and sha256, or git and commit" % (where, name))
    for name, entry in lock["tools"].items():
        if set(entry) != {"url", "sha256"}:
            die("%s: %s must have url and sha256" % (where, name))
        check_archive_pin(where, name, entry["url"], entry["sha256"])
    return lock


def pinned_sources(minor, os_name, pins=None, lock=None):
    """Every pin that applies to a build: {name: {url, sha256} | {git, commit}}."""
    pins = read_pins() if pins is None else pins
    lock = read_lock() if lock is None else lock
    result = {}
    for name, url, digest, conditions in pins:
        if name in lock["sources"]:
            die("%s is pinned in both sources.txt and sources.lock.json" % name)
        if not applies(conditions, minor, os_name):
            continue
        if name in result:
            die("sources.txt: more than one line for %s applies to PHP %s on %s" % (name, minor, os_name))
        result[name] = {"url": url, "sha256": digest}
    result.update(lock["sources"])
    return result


# --- what spc downloaded ----------------------------------------------------------


def read_spc_lock(downloads):
    path = os.path.join(downloads, ".lock.json")
    if not os.path.exists(path):
        return {}
    with open(path) as f:
        return json.load(f)


def spc_path(downloads, item, key):
    """The file or directory of a .lock.json entry; always directly in downloads."""
    name = item.get(key)
    if not isinstance(name, str) or not name or name != os.path.basename(name) or name.startswith("."):
        die("spc's .lock.json has an unexpected %s: %r" % (key, name))
    return os.path.join(downloads, name)


def git(path, *args):
    result = subprocess.run(("git", "-C", path) + args, stdout=subprocess.PIPE, stderr=subprocess.PIPE, universal_newlines=True)
    if result.returncode != 0:
        die("git %s failed in %s: %s" % (" ".join(args), path, result.stderr.strip()))
    return result.stdout.strip()


# Every file that is not exactly the commit's: changed, untracked or ignored
# (the flags also override a user's own git configuration).
GIT_STATUS = ("status", "--porcelain", "--ignored", "--untracked-files=all")


def git_problems(path):
    """Why a git source's files might not be exactly its commit (empty: they are)."""
    problems = []
    if git(path, *(GIT_STATUS + ("--ignore-submodules=none",))):
        problems.append("has files that are not its commit's")
    for line in git(path, "submodule", "status", "--recursive").splitlines():
        if line[:1] in ("+", "U"):
            problems.append("has submodule %s at another commit than recorded" % line.split()[1])
    # Ignored files inside a submodule do not show in its parent's status.
    if git(path, "submodule", "--quiet", "foreach", "--recursive", "git " + " ".join(GIT_STATUS)):
        problems.append("has files in a submodule that are not its commit's")
    return problems


def move_to(path, commit):
    """Makes the files of a clone exactly those of `commit`."""
    known = subprocess.run(("git", "-C", path, "cat-file", "-e", commit + "^{commit}"), stderr=subprocess.DEVNULL)
    if known.returncode != 0:
        git(path, "fetch", "--quiet", "origin", commit)
    # From an empty directory: what the branch tip brought along and the
    # locked commit ignores, or keeps in a submodule it does not have, would
    # otherwise stay where it is.
    for entry in os.listdir(path):
        full = os.path.join(path, entry)
        if entry == ".git":
            continue
        if os.path.isdir(full) and not os.path.islink(full):
            shutil.rmtree(full)
        else:
            os.remove(full)
    git(path, "checkout", "--quiet", "--force", "--detach", commit)
    # The submodules spc initialised, at the commits this one records.
    git(path, "submodule", "--quiet", "update", "--recursive", "--force")


def snapshot(downloads, skip_missing=False):
    """What is in downloads, by spc source name: files with sha256, clones with commit."""
    found = {}
    for name, item in sorted(read_spc_lock(downloads).items()):
        kind = item.get("source_type")
        if kind not in ("archive", "git"):
            die("%s was downloaded as '%s', which cannot be verified" % (name, kind))
        path = spc_path(downloads, item, "filename" if kind == "archive" else "dirname")
        if not os.path.exists(path):
            if skip_missing:
                continue
            die("%s is in spc's .lock.json, but %s is missing" % (name, os.path.basename(path)))
        if kind == "archive":
            found[name] = {"file": os.path.basename(path), "sha256": sha256_file(path)}
        else:
            found[name] = {
                "dir": os.path.basename(path),
                "git": git(path, "remote", "get-url", "origin"),
                "commit": git(path, "rev-parse", "HEAD"),
            }
    return found


# --- commands used by build.sh ---------------------------------------------------


def download_args(args):
    pins = pinned_sources(args.minor, args.os)
    if args.hand_pinned_only:
        by_hand = {name for name, _url, _digest, _conditions in read_pins()}
        pins = {name: pin for name, pin in pins.items() if name in by_hand}
    have = snapshot(args.downloads, skip_missing=True) if args.downloads else {}
    for name, pin in sorted(pins.items()):
        if "url" not in pin or have.get(name, {}).get("sha256") == pin["sha256"]:
            continue
        print("-U")
        print("%s:%s" % (name, pin["url"]))


def tool_url(args):
    tools = read_lock()["tools"]
    if args.name not in tools:
        die("%s is not in sources.lock.json; add it with `build.sh --refresh-lock --add-tool %s`" % (args.name, args.name))
    print(tools[args.name]["url"])


def check_tools(downloads, lock):
    """Checks the downloaded tools; returns their records."""
    spc = read_spc_lock(downloads)
    found = snapshot(downloads)
    errors, record = [], {}
    for name, entry in found.items():
        if spc[name].get("lock_as") != SPC_PACKAGE:
            continue
        expected = lock["tools"].get(name)
        if expected is None:
            errors.append("%s (%s) is not in sources.lock.json" % (name, entry.get("file", "a git clone")))
        elif entry.get("sha256") != expected["sha256"]:
            errors.append("%s: %s has sha256 %s, locked is %s" % (name, entry.get("file"), entry.get("sha256"), expected["sha256"]))
        else:
            record[name] = entry
    if errors:
        die("prebuilt tools do not match sources.lock.json:\n  " + "\n  ".join(errors))
    return record


def verify_tools(args):
    for name, entry in check_tools(args.downloads, read_lock()).items():
        print("  ok  %s (%s)" % (name, entry["file"]))


def checkout(args):
    pins = pinned_sources(args.minor, args.os)
    spc = read_spc_lock(args.downloads)
    changed = False
    for name, item in sorted(spc.items()):
        pin = pins.get(name)
        if item.get("source_type") != "git" or not pin or "commit" not in pin:
            continue  # verify reports what is not pinned
        path = spc_path(args.downloads, item, "dirname")
        head = git(path, "rev-parse", "HEAD")
        if head != pin["commit"]:
            # spc cloned the branch as it is today; go back to what is locked.
            move_to(path, pin["commit"])
            print("  %s: %s -> locked %s" % (name, head[:12], pin["commit"][:12]))
        if item.get("hash") != pin["commit"]:
            # spc compares this with the extracted copy to decide whether to
            # extract again; keep it true to what is in the directory.
            item["hash"] = pin["commit"]
            changed = True
    if changed:
        with open(os.path.join(args.downloads, ".lock.json"), "w") as f:
            json.dump(spc, f, indent=4)


def check_sources(downloads, version, os_name, allow_unverified=False, pins=None, lock=None):
    """Checks every download against its pin; returns the record for the build metadata."""
    minor = minor_of(version)
    lock = read_lock() if lock is None else lock
    pinned = pinned_sources(minor, os_name, pins, lock)
    spc = read_spc_lock(downloads)
    found = snapshot(downloads)
    record = check_tools(downloads, lock)
    errors = []
    for name, entry in found.items():
        lock_as = spc[name].get("lock_as")
        if lock_as == SPC_PACKAGE:
            continue
        if name == "php-src":
            if lock_as != SPC_SOURCE or "file" not in entry:
                die("php-src was not downloaded as a release tarball")
            signer = check_php_src(os.path.join(downloads, entry["file"]), entry, version)
            record[name] = dict(entry, signed_by=signer)
            continue
        problem = source_problem(name, entry, lock_as, pinned.get(name), downloads)
        if problem is None:
            record[name] = entry
        elif allow_unverified:
            record[name] = dict(entry, unverified=True)
        else:
            errors.append(problem)
    if "php-src" not in found:
        errors.append("php-src was not downloaded")
    if errors:
        hint = ""
        if any(error.endswith("is not pinned") for error in errors):
            hint = "\nPin a new source with `build.sh --refresh-lock --add NAME` (see README.md, \"Sources\")."
        die("sources do not match sources.txt / sources.lock.json:\n  " + "\n  ".join(errors) + hint)
    return dict(sorted(record.items()))


def source_problem(name, entry, lock_as, pin, downloads):
    """Why a downloaded source is not what is pinned (None: it is)."""
    if lock_as == SPC_PREBUILT:
        # A library spc built and published itself (SPC_PREBUILT=1): there is
        # nothing to pin it to.
        return "%s is a pre-built library from spc, which cannot be pinned" % name
    if lock_as != SPC_SOURCE:
        return "%s has an unknown download type %r" % (name, lock_as)
    if pin is None:
        return "%s (%s) is not pinned" % (name, entry.get("file") or "git " + entry["commit"])
    if "file" in entry:
        if "sha256" not in pin:
            return "%s is locked as a git source but was downloaded as %s" % (name, entry["file"])
        if entry["sha256"] != pin["sha256"]:
            return "%s: %s has sha256 %s, pinned is %s" % (name, entry["file"], entry["sha256"], pin["sha256"])
        return None
    if "commit" not in pin:
        return "%s is pinned as an archive but was cloned with git" % name
    if entry["git"] != pin["git"]:
        return "%s was cloned from %s, locked is %s" % (name, entry["git"], pin["git"])
    if entry["commit"] != pin["commit"]:
        return "%s is at commit %s, locked is %s" % (name, entry["commit"], pin["commit"])
    problems = git_problems(os.path.join(downloads, entry["dir"]))
    if problems:
        return "%s %s" % (name, " and ".join(problems))
    return None


def verify(args):
    record = check_sources(args.downloads, args.version, args.os, args.allow_unverified)
    for name, entry in record.items():
        state = "UNVERIFIED" if entry.get("unverified") else "ok"
        print("  %-10s  %-24s %s" % (state, name, entry.get("file") or "git " + entry["commit"]))
    with open(args.record, "w") as f:
        json.dump(record, f, indent=2)
        f.write("\n")


def strip_notes(record):
    """A record reduced to what `snapshot` can see again."""
    keep = ("file", "sha256", "dir", "git", "commit")
    return {name: {k: v for k, v in entry.items() if k in keep} for name, entry in record.items()}


def unchanged(args):
    with open(args.record) as f:
        before = strip_notes(json.load(f))
    after = snapshot(args.downloads)
    changes = []
    for name in sorted(set(before) | set(after)):
        if before.get(name) != after.get(name):
            changes.append("%s: %s -> %s" % (name, json.dumps(before.get(name)), json.dumps(after.get(name))))
        elif "dir" in after[name]:
            changes += ["%s %s" % (name, problem) for problem in git_problems(os.path.join(args.downloads, after[name]["dir"]))]
    if changes:
        die("the downloads changed after they were verified:\n  " + "\n  ".join(changes))


# --- php-src ------------------------------------------------------------------------


def fetch(url, token=None):
    """The body of a URL. curl, as everywhere else in the build."""
    cmd = ["curl", "-fsSL", "--retry", "3"]
    if token:
        cmd += ["-H", "Authorization: Bearer " + token]
    result = subprocess.run(cmd + [url], stdout=subprocess.PIPE)
    if result.returncode != 0:
        die("could not fetch %s" % url)
    return result.stdout


def read_keys(minor, path=KEYS_FILE):
    """The release managers of a PHP branch: {fingerprint: may be expired}."""
    keys = {}
    with open(path) as f:
        for number, line in enumerate(f, 1):
            fields = line.split("#", 1)[0].split()
            if not fields:
                continue
            where = "%s:%d" % (os.path.basename(path), number)
            if len(fields) not in (2, 3) or fields[2:] not in ([], ["expired-ok"]):
                die("%s: expected <php minor> <fingerprint> [expired-ok]" % where)
            if not re.fullmatch(r"\d+\.\d+", fields[0]) or not FINGERPRINT_RE.fullmatch(fields[1]):
                die("%s: bad PHP branch or fingerprint" % where)
            if fields[0] == minor:
                keys[fields[1]] = len(fields) == 3
    return keys


def dearmor(data):
    """OpenPGP public keys as binary packets, whether or not they came armored."""
    if b"-----BEGIN PGP PUBLIC KEY BLOCK-----" not in data:
        if b"-----BEGIN PGP" in data:
            die("php.net's keyring contains no OpenPGP keys")
        return data
    out, block, headers = bytearray(), None, False
    for line in data.decode("ascii", "replace").splitlines():
        line = line.strip()
        if line == "-----BEGIN PGP PUBLIC KEY BLOCK-----":
            block, headers = [], True
        elif line == "-----END PGP PUBLIC KEY BLOCK-----" and block is not None:
            out += base64.b64decode("".join(block))
            block = None
        elif block is None:
            continue
        elif headers:
            if not line:
                headers = False
            elif ":" not in line:  # no blank line after the headers
                headers = False
                block.append(line)
        elif not line.startswith("="):  # the CRC line
            block.append(line)
    if not out:
        die("php.net's keyring contains no OpenPGP keys")
    return bytes(out)


def run_gpgv(keyring, signature, signed):
    """gpgv's status lines for a detached signature (empty when gpgv cannot run)."""
    if not shutil.which("gpgv"):
        die("gpgv is needed to verify php-src's signature (macOS: brew install gnupg)")
    home = tempfile.mkdtemp(prefix="bird-gpgv-")
    try:
        result = subprocess.run(
            ["gpgv", "--homedir", home, "--keyring", keyring, "--status-fd", "1", signature, signed],
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            universal_newlines=True,
        )
    finally:
        shutil.rmtree(home, ignore_errors=True)
    return result.stdout


def signer_of(status, keys):
    """The pinned release manager whose signature gpgv accepted, or exits.

    `status` is gpgv's --status-fd output; `keys` is read_keys(). Only the
    key's fingerprint counts, never its name: the keyring comes from php.net,
    like the tarball, and may contain anything.
    """
    seen = [line.split()[1:] for line in status.splitlines() if line.startswith("[GNUPG:] ")]
    kinds = [fields[0] for fields in seen if fields]
    valid = [fields for fields in seen if fields and fields[0] == "VALIDSIG"]
    for bad in ("BADSIG", "ERRSIG", "EXPSIG", "REVKEYSIG"):
        if bad in kinds:
            die("php-src's signature is not valid (gpgv: %s)" % bad)
    if kinds.count("NEWSIG") > 1 or len(valid) != 1 or len(valid[0]) < 11:
        die("php-src's signature could not be verified (gpgv reported %s)" % (", ".join(kinds) or "nothing"))
    # VALIDSIG ends in the fingerprint of the primary key; a release manager
    # may sign with a subkey, and php-keys.txt lists primary keys.
    primary = valid[0][10]
    if primary not in keys:
        die("php-src is signed by %s, which php-keys.txt does not list for this PHP branch" % primary)
    if "GOODSIG" in kinds:
        return primary
    if "EXPKEYSIG" in kinds and keys[primary]:
        return primary
    if "EXPKEYSIG" in kinds:
        die(
            "php-src is signed by %s, but php.net's keyring says that key has expired "
            "(php-keys.txt can allow that with expired-ok)" % primary
        )
    die("php-src's signature could not be verified (gpgv reported %s)" % ", ".join(kinds))


def check_php_src(path, entry, version):
    """php-src against php.net's published sha256 and the release signature."""
    name = entry["file"]
    if not re.fullmatch(r"php-%s\.tar\.(xz|gz|bz2)" % re.escape(version), name):
        die("php-src was downloaded as %s, expected the PHP %s tarball" % (name, version))
    release = json.loads(fetch("%s/releases/index.php?json&version=%s" % (PHP_NET, version)))
    published = {s.get("filename"): s.get("sha256") for s in release.get("source", [])}
    if release.get("version") != version or name not in published:
        die("php.net publishes no sha256 for %s" % name)
    if entry["sha256"] != published[name]:
        die("%s has sha256 %s, php.net publishes %s" % (name, entry["sha256"], published[name]))

    keys = read_keys(minor_of(version))
    if not keys:
        die("php-keys.txt lists no release managers for PHP %s" % minor_of(version))
    work = tempfile.mkdtemp(prefix="bird-php-sig-")
    try:
        keyring = os.path.join(work, "keyring.gpg")
        signature = os.path.join(work, name + ".asc")
        with open(keyring, "wb") as f:
            f.write(dearmor(fetch(PHP_NET + "/distributions/php-keyring.gpg")))
        with open(signature, "wb") as f:
            f.write(fetch("%s/distributions/%s.asc" % (PHP_NET, name)))
        return signer_of(run_gpgv(keyring, signature, path), keys)
    finally:
        shutil.rmtree(work, ignore_errors=True)


# --- refreshing the lock -----------------------------------------------------------

# Stands in for curl while spc resolves the newest releases, to learn which
# URL each file came from; spc's own .lock.json only has the file names.
CURL_WRAPPER = """#!/usr/bin/env bash
out="" url=""
args=("$@")
for ((i = 0; i < ${#args[@]}; i++)); do
    case "${args[i]}" in
        -o) out="${args[i + 1]}" ;;
        http://* | https://*) url="${args[i]}" ;;
    esac
done
if [ -n "$out" ] && [ -n "$url" ]; then
    printf '%s\\t%s\\n' "$url" "$out" >> "$BIRD_CURL_LOG"
fi
exec "$BIRD_REAL_CURL" "$@"
"""


def pinnable_url(url, token=None):
    """A URL plain curl can fetch again and again for the file spc got from `url`.

    spc fetches GitHub release assets and tarballs through api.github.com
    (which needs a token, an Accept header or a rate limit to spare), and
    PECL's "latest" URL changes with every release.
    """
    m = re.fullmatch(r"https://api\.github\.com/repos/[^/]+/[^/]+/releases/assets/\d+", url)
    if m:
        return json.loads(fetch(url, token))["browser_download_url"]
    m = re.fullmatch(r"https://api\.github\.com/repos/([^/]+)/([^/]+)/tarball/(.+)", url)
    if m:
        owner, repo, ref = m.groups()
        tag = ref[len("refs/tags/"):] if ref.startswith("refs/tags/") else ref
        # GitHub ignores the last path segment; it only names the download.
        return "https://github.com/%s/%s/archive/refs/tags/%s/%s-%s.tar.gz" % (owner, repo, tag, repo, tag.replace("/", "-"))
    m = re.fullmatch(r"https://pecl\.php\.net/get/([A-Za-z0-9_]+)", url)
    if m:
        headers = subprocess.run(
            ["curl", "-fsSI", "--retry", "3", url], stdout=subprocess.PIPE, universal_newlines=True
        ).stdout
        named = re.search(r"(?im)^content-disposition:.*filename=\"?([A-Za-z0-9_.-]+\.tgz)", headers)
        if not named:
            die("PECL did not say which release %s is" % url)
        return "https://pecl.php.net/get/" + named.group(1)
    return url


def run_spc(spc, cwd, wrapper_dir, log, *args, github_token=True):
    env = dict(os.environ)
    if not github_token:
        env.pop("GITHUB_TOKEN", None)
        env.pop("GITHUB_USER", None)
    env["BIRD_REAL_CURL"] = shutil.which("curl") or die("curl not found")
    env["BIRD_CURL_LOG"] = log
    env["PATH"] = wrapper_dir + os.pathsep + env["PATH"]
    if subprocess.run((spc,) + args + ("--no-motd",), cwd=cwd, env=env).returncode != 0:
        die("spc %s failed" % " ".join(args))


def split_names(values):
    return [n for value in values or [] for n in value.split(",") if n]


def refresh(args):
    lock = read_lock() if os.path.exists(LOCK_FILE) else {"sources": {}, "tools": {}}
    by_hand = {name for name, _url, _digest, _cond in read_pins()}
    add, add_tools, only = split_names(args.add), split_names(args.add_tool), split_names(args.only)
    for name in add + add_tools + only:
        if not NAME_RE.fullmatch(name):
            die("bad source name '%s'" % name)
    clash = sorted(by_hand & (set(add) | set(lock["sources"])))
    if clash:
        die("pinned by hand in sources.txt, not for the lock: " + ", ".join(clash))
    sources = sorted(set(lock["sources"]) | set(add))
    tools = sorted(set(lock["tools"]) | set(add_tools))
    if only:
        unknown = sorted(set(only) - set(sources) - set(tools))
        if unknown:
            die("not in the lock (use --add or --add-tool): " + ", ".join(unknown))
        sources = [n for n in sources if n in only]
        tools = [n for n in tools if n in only]
    if not sources and not tools:
        die("nothing to refresh; name the sources with --add / --add-tool")

    spc = os.path.abspath(args.spc)
    token = os.environ.get("GITHUB_TOKEN")
    if not token:
        print("warning: GITHUB_TOKEN is not set; GitHub's API allows 60 requests an hour without one", file=sys.stderr)
    work = os.path.abspath(args.work)
    shutil.rmtree(work, ignore_errors=True)
    wrapper_dir = os.path.join(work, "bin")
    pinned_dir = os.path.join(work, "pinned")
    os.makedirs(wrapper_dir)
    os.makedirs(pinned_dir)
    with open(os.path.join(wrapper_dir, "curl"), "w") as f:
        f.write(CURL_WRAPPER)
    os.chmod(os.path.join(wrapper_dir, "curl"), 0o755)
    log = os.path.join(work, "curl.log")
    downloads = os.path.join(work, "downloads")

    # No mirrors (--no-alt): the lock should name the upstream itself.
    if sources:
        run_spc(spc, work, wrapper_dir, log, "download", ",".join(sources), "--no-alt", "--retry=3")
    # spc adds GITHUB_TOKEN to its request for zig's release index, which is
    # on ziglang.org: zig is fetched without it.
    for with_token in (True, False):
        some = [name for name in tools if name.startswith("zig-") != with_token]
        if some:
            run_spc(spc, work, wrapper_dir, log, "install-pkg", ",".join(some), "--skip-extract", "--no-alt", github_token=with_token)

    requested = {}
    if os.path.exists(log):
        with open(log) as f:
            for line in f:
                url, _, path = line.rstrip("\n").partition("\t")
                requested[os.path.realpath(path)] = url
    spc_lock = read_spc_lock(downloads)
    found = snapshot(downloads)
    fresh = {"sources": dict(lock["sources"]), "tools": dict(lock["tools"])}
    for name in sources + tools:
        section = "tools" if name in tools else "sources"
        entry = found.get(name)
        if entry is None:
            die("spc did not download %s" % name)
        if spc_lock[name].get("lock_as") != (SPC_PACKAGE if section == "tools" else SPC_SOURCE):
            die("%s is not a %s" % (name, "prebuilt tool" if section == "tools" else "source"))
        if "commit" in entry:
            new = {"git": entry["git"], "commit": entry["commit"]}
        else:
            path = os.path.join(downloads, entry["file"])
            source_url = requested.get(os.path.realpath(path))
            if not source_url:
                die("could not tell where spc downloaded %s from" % entry["file"])
            url = pinnable_url(source_url, token)
            if not URL_RE.fullmatch(url):
                die("%s comes from %s, which cannot be used as a pin" % (name, url))
            # With a pin, spc names the file after the URL, unless its own
            # configuration names it (which shows as a name the URL lacks).
            named_by_spc = url == source_url and entry["file"] != os.path.basename(url)
            if not named_by_spc and not os.path.basename(url).endswith(ARCHIVE_EXTENSIONS):
                die("%s: spc could not extract a file named %s" % (name, os.path.basename(url)))
            digest = entry["sha256"]
            if url != source_url:
                # Lock what the pinned URL serves, which is what a build gets.
                again = os.path.join(pinned_dir, name)
                if subprocess.run(["curl", "-fsSL", "--retry", "3", "-o", again, url]).returncode != 0:
                    die("could not fetch %s" % url)
                digest = sha256_file(again)
            new = {"url": url, "sha256": digest}
        old = fresh[section].get(name)
        if old != new:
            print("%s: %s -> %s" % (name, describe(old), describe(new)))
        fresh[section][name] = new

    names = {}
    for name, entry in fresh["sources"].items():
        if "url" in entry:
            names.setdefault(os.path.basename(entry["url"]), []).append(name)
    same = ["%s (%s)" % (file, ", ".join(sorted(users))) for file, users in sorted(names.items()) if len(users) > 1]
    if same:
        die("sources would be downloaded to the same file name: " + "; ".join(same))

    with open(LOCK_FILE, "w") as f:
        json.dump(fresh, f, indent=2, sort_keys=True)
        f.write("\n")
    read_lock()
    print("wrote %s; review the diff before committing it" % os.path.relpath(LOCK_FILE))


def describe(entry):
    if entry is None:
        return "(new)"
    if "commit" in entry:
        return entry["commit"][:12]
    return os.path.basename(entry["url"])


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)

    p = sub.add_parser("download-args", help="print the spc download arguments that pin the sources")
    p.add_argument("minor", help="PHP branch, e.g. 8.4")
    p.add_argument("os", choices=["macos", "linux"])
    p.add_argument("--downloads", help="leave out sources already there with the right sha256")
    p.add_argument("--hand-pinned-only", action="store_true", help="only the pins in sources.txt (SPC_PREBUILT=1 builds)")
    p.set_defaults(func=download_args)

    p = sub.add_parser("url", help="print the locked URL of a prebuilt tool")
    p.add_argument("name")
    p.set_defaults(func=tool_url)

    p = sub.add_parser("verify-tools", help="check the downloaded prebuilt tools against the lock")
    p.add_argument("downloads")
    p.set_defaults(func=verify_tools)

    p = sub.add_parser("checkout", help="move git sources to their locked commits")
    p.add_argument("downloads")
    p.add_argument("minor")
    p.add_argument("os", choices=["macos", "linux"])
    p.set_defaults(func=checkout)

    p = sub.add_parser("verify", help="check every download and record what was used")
    p.add_argument("downloads")
    p.add_argument("version", help="full PHP version, e.g. 8.4.26")
    p.add_argument("os", choices=["macos", "linux"])
    p.add_argument("--record", required=True, metavar="FILE")
    p.add_argument("--allow-unverified", action="store_true", help="record what does not match the lock instead of failing (never published)")
    p.set_defaults(func=verify)

    p = sub.add_parser("unchanged", help="fail if the downloads changed since verify")
    p.add_argument("downloads")
    p.add_argument("record")
    p.set_defaults(func=unchanged)

    p = sub.add_parser("refresh", help="rewrite sources.lock.json from what spc picks today")
    p.add_argument("--spc", required=True, help="the spc binary")
    p.add_argument("--work", required=True, help="scratch directory (emptied first)")
    p.add_argument("--only", action="append", metavar="NAMES", help="refresh just these (comma separated)")
    p.add_argument("--add", action="append", metavar="NAMES", help="sources to add to the lock")
    p.add_argument("--add-tool", action="append", metavar="NAMES", help="prebuilt tools to add, e.g. zig-x86_64-linux")
    p.set_defaults(func=refresh)

    args = parser.parse_args()
    args.func(args)


if __name__ == "__main__":
    sys.exit(main())
