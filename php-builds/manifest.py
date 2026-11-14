#!/usr/bin/env python3
"""Helpers for Bird's PHP build pipeline (see README.md for the manifest format).

  manifest.py resolve 8.1,8.2,8.4.20
      Prints a JSON list of full versions, resolving each major.minor to its
      latest release on php.net.

  manifest.py matrix VERSIONS PLATFORMS [--skip-published manifest.json]
      Prints the GitHub Actions build matrix ({"include": [...]}). With
      --skip-published, drops builds the manifest already has with the
      current recipe, so a scheduled run only builds new patch releases and
      what changed since (extensions, pins, build.sh).

  manifest.py recipe
      Prints the recipe id: a hash of the files that define a build.

  manifest.py merge [--existing manifest.json] --out manifest.json
                    [--artifacts DIR --matrix JSON --reports FILE --collect DIR]
      Merges the builds of a run into a manifest. DIR holds one directory per
      build job, named like its artifact (php-<version>-<platform>), with what
      build.sh wrote. Nothing in them is trusted: each must be a build the
      run planned (--matrix, as printed by `matrix`), be the build its own
      job reported (--reports: "<job name><tab><annotation line>" per line,
      as the workflow reads them from GitHub), describe that version and
      platform only, and list exactly its own files with their real size and
      sha256. The archives of the accepted builds are moved to --collect.
      Builds already in the existing manifest are kept unless rebuilt (same
      version and platform), so a partial run never drops published versions.
      Without --artifacts it rewrites the existing manifest (to re-sign it).
"""

import argparse
import hashlib
import json
import os
import re
import shutil
import sys
import urllib.request
from datetime import datetime, timezone

SCHEMA = 1

HERE = os.path.dirname(os.path.abspath(__file__))

# Everything that decides what a build contains. A change to any of these
# gives a new recipe id, and the scheduled run rebuilds every version.
RECIPE_FILES = [
    "build.sh",
    "extensions.txt",
    "shared-extensions.txt",
    "libs.txt",
    "sources.txt",
    "sources.lock.json",
    "sources.py",
    "php-keys.txt",
    "libde265-sse-only.php",
]

# GitHub runner per platform.
RUNNERS = {
    "macos-aarch64": "macos-15",
    "macos-x86_64": "macos-15-intel",
    "linux-aarch64": "ubuntu-24.04-arm",
    "linux-x86_64": "ubuntu-24.04",
}


def version_key(version):
    return tuple(int(p) for p in version.split("."))


def latest_release(minor):
    url = "https://www.php.net/releases/index.php?json&version=" + minor
    with urllib.request.urlopen(url, timeout=30) as resp:
        data = json.load(resp)
    if "version" not in data:
        raise SystemExit("php.net has no release for PHP %s" % minor)
    return data["version"]


def resolve_versions(spec):
    versions = []
    for v in filter(None, (s.strip() for s in spec.split(","))):
        if re.fullmatch(r"\d+\.\d+", v):
            versions.append(latest_release(v))
        elif re.fullmatch(r"\d+\.\d+\.\d+", v):
            versions.append(v)
        else:
            raise SystemExit("'%s' is not a PHP version" % v)
    return versions


def resolve(args):
    print(json.dumps(resolve_versions(args.versions)))


def recipe_id():
    h = hashlib.sha256()
    for name in RECIPE_FILES:
        with open(os.path.join(HERE, name), "rb") as f:
            data = f.read()
        h.update(b"%s\0%d\0" % (name.encode(), len(data)))
        h.update(data)
    return h.hexdigest()[:16]


def recipe(args):
    print(recipe_id())


def plan_matrix(versions, platforms, published, recipe):
    """Build matrix entries, minus builds `published` has with `recipe`."""
    unknown = [p for p in platforms if p not in RUNNERS]
    if unknown:
        raise SystemExit("unknown platform(s): " + ", ".join(unknown))
    done = {(b["version"], b["platform"]) for b in published if b.get("recipe") == recipe}
    return [
        {"php": v, "platform": p, "runner": RUNNERS[p]}
        for v in versions
        for p in platforms
        if (v, p) not in done
    ]


def matrix(args):
    platforms = [p.strip() for p in args.platforms.split(",") if p.strip()]
    published = []
    if args.skip_published and os.path.exists(args.skip_published):
        with open(args.skip_published) as f:
            manifest = json.load(f)
        if manifest.get("schema") != SCHEMA:
            raise SystemExit("published manifest has schema %r, expected %d" % (manifest.get("schema"), SCHEMA))
        published = manifest.get("builds", [])
    include = plan_matrix(resolve_versions(args.versions), platforms, published, recipe_id())
    print(json.dumps({"include": include}))


class Rejected(Exception):
    """A build artifact that must not be published."""


# A build job is named "PHP <version> (<platform>)" by the workflow, and says
# what it built in an annotation: "bird-php-build <artifact> <sha256 of its
# build file>".
JOB_RE = re.compile(r"PHP ([0-9]+\.[0-9]+\.[0-9]+) \(([a-z0-9]+-[a-z0-9_]+)\)")
REPORT_RE = re.compile(r"bird-php-build (php-[0-9.]+-[a-z0-9_-]+) ([0-9a-f]{64})")
ARTIFACT_RE = re.compile(r"php-([0-9]+\.[0-9]+\.[0-9]+)-([a-z0-9]+-[a-z0-9_]+)")
EXTENSION_RE = re.compile(r"[a-z][a-z0-9_]{0,40}")
SOURCE_RE = re.compile(r"[A-Za-z0-9][A-Za-z0-9._+-]{0,80}")
TEXT_RE = re.compile(r"[A-Za-z0-9][A-Za-z0-9._+~-]{0,80}")
SHA256_RE = re.compile(r"[0-9a-f]{64}")
COMMIT_RE = re.compile(r"[0-9a-f]{40}")
FINGERPRINT_RE = re.compile(r"[0-9A-F]{40}")
GIT_URL_RE = re.compile(r"https://[A-Za-z0-9.-]+/[A-Za-z0-9._~/-]{1,200}")
BUILD_KEYS = {"version", "minor", "platform", "spc_version", "recipe", "cli", "fpm", "extensions", "shared_extensions", "sources"}
MAX_ITEMS = 500


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def matches(pattern, value):
    return isinstance(value, str) and pattern.fullmatch(value) is not None


def list_names(filename):
    """The names in a list file (extensions.txt, ...), whatever their conditions."""
    with open(os.path.join(HERE, filename)) as f:
        lines = (line.split("#", 1)[0].split() for line in f)
        return {fields[0] for fields in lines if fields}


def release_managers(minor):
    """The fingerprints php-keys.txt lists for a PHP branch (see sources.py)."""
    with open(os.path.join(HERE, "php-keys.txt")) as f:
        lines = (line.split("#", 1)[0].split() for line in f)
        return {fields[1] for fields in lines if len(fields) >= 2 and fields[0] == minor}


def check_asset(directory, version, platform, kind, asset, keys):
    """One archive of a build: named after exactly this build, present, intact."""
    if not isinstance(asset, dict) or set(asset) != keys:
        raise Rejected("%s must have exactly %s" % (kind, ", ".join(sorted(keys))))
    if not matches(SHA256_RE, asset["sha256"]):
        raise Rejected("%s has no valid sha256" % kind)
    # build.sh's naming: nothing but this build's version, platform and
    # content can appear in the name, so no build can claim another's file.
    name = "php-%s-%s-%s-%s.tar.gz" % (version, kind, platform, asset["sha256"][:12])
    if asset["file"] != name:
        raise Rejected("%s is named %r, expected %s" % (kind, asset["file"], name))
    if type(asset["size"]) is not int or asset["size"] <= 0:
        raise Rejected("%s has no valid size" % kind)
    path = os.path.join(directory, name)
    if os.path.islink(path) or not os.path.isfile(path):
        raise Rejected("%s is missing" % name)
    if os.path.getsize(path) != asset["size"]:
        raise Rejected("%s is %d bytes, not the %d listed" % (name, os.path.getsize(path), asset["size"]))
    if sha256_file(path) != asset["sha256"]:
        raise Rejected("%s does not have the sha256 listed" % name)
    return name


def check_sources(version, sources):
    """The record of what a build downloaded (sources.py verify)."""
    if not isinstance(sources, dict) or not 0 < len(sources) <= MAX_ITEMS:
        raise Rejected("sources is not a list of what was downloaded")
    for name, entry in sources.items():
        if not matches(SOURCE_RE, name) or not isinstance(entry, dict):
            raise Rejected("sources has a bad entry %r" % name)
        if "unverified" in entry:
            # A build with spc's pre-built libraries (SPC_PREBUILT=1).
            raise Rejected("%s was not verified against the lock; such builds are not published" % name)
        keys = set(entry)
        if name == "php-src":
            ok = (
                keys == {"file", "sha256", "signed_by"}
                and matches(re.compile(r"php-%s\.tar\.(xz|gz|bz2)" % re.escape(version)), entry["file"])
                and matches(SHA256_RE, entry["sha256"])
                and matches(FINGERPRINT_RE, entry["signed_by"])
                and entry["signed_by"] in release_managers(version.rsplit(".", 1)[0])
            )
        elif keys == {"dir", "git", "commit"}:
            ok = matches(SOURCE_RE, entry["dir"]) and matches(GIT_URL_RE, entry["git"]) and matches(COMMIT_RE, entry["commit"])
        else:
            ok = keys == {"file", "sha256"} and matches(SOURCE_RE, entry["file"]) and matches(SHA256_RE, entry["sha256"])
        if not ok:
            raise Rejected("sources has a bad entry for %s" % name)
    if "php-src" not in sources:
        raise Rejected("sources does not say which php-src was built")


def read_reports(path):
    """What each build job reported having built: {artifact name: {sha256 of its build file}}.

    Any job of a run can upload an artifact under any name, another job's
    included. An annotation, though, belongs to the job that wrote it, and
    GitHub names the job from the workflow's matrix: so a job is only heard
    about its own build, whatever it writes.
    """
    reports = {}
    with open(path) as f:
        for line in f:
            job, _, message = line.rstrip("\n").partition("\t")
            named, said = JOB_RE.fullmatch(job), REPORT_RE.fullmatch(message.strip())
            if named and said and said.group(1) == "php-%s-%s" % named.groups():
                reports.setdefault(said.group(1), set()).add(said.group(2))
    return reports


def check_artifact(directory, planned, recipe, reports):
    """Validates what one build job uploaded; returns (build, archive names)."""
    name = os.path.basename(directory)
    m = ARTIFACT_RE.fullmatch(name)
    if not m or m.group(2) not in RUNNERS:
        raise Rejected("not named php-<version>-<platform>")
    version, platform = m.groups()
    if (version, platform) not in planned:
        raise Rejected("this run did not plan PHP %s for %s" % (version, platform))
    if os.path.islink(directory) or not os.path.isdir(directory):
        raise Rejected("not a directory")
    entries = sorted(os.listdir(directory))
    for entry in entries:
        path = os.path.join(directory, entry)
        if os.path.islink(path) or not os.path.isfile(path):
            raise Rejected("%s is not a regular file" % entry)

    meta = "php-%s-%s.json" % (version, platform)
    if [e for e in entries if e.endswith(".json")] != [meta]:
        raise Rejected("expected exactly one build file, %s" % meta)
    reported = reports.get(name, set())
    if len(reported) != 1:
        raise Rejected("its build job reported %s" % ("several builds" if reported else "no build"))
    if sha256_file(os.path.join(directory, meta)) not in reported:
        raise Rejected("%s is not the build file its build job reported" % meta)
    try:
        with open(os.path.join(directory, meta)) as f:
            build = json.load(f)
    except (ValueError, RecursionError) as e:
        raise Rejected("%s is not valid JSON: %s" % (meta, e))
    if not isinstance(build, dict) or set(build) != BUILD_KEYS:
        raise Rejected("%s must have exactly the keys %s" % (meta, ", ".join(sorted(BUILD_KEYS))))
    for key, expected in (("version", version), ("platform", platform), ("minor", version.rsplit(".", 1)[0]), ("recipe", recipe)):
        if build[key] != expected:
            raise Rejected("%s says %s %r, expected %r" % (meta, key, build[key], expected))
    if not matches(TEXT_RE, build["spc_version"]):
        raise Rejected("%s has a bad spc_version" % meta)
    extensions = build["extensions"]
    if not isinstance(extensions, list) or len(extensions) > MAX_ITEMS or not all(matches(EXTENSION_RE, e) for e in extensions):
        raise Rejected("%s has a bad extensions list" % meta)
    shared = build["shared_extensions"]
    if not isinstance(shared, dict):
        raise Rejected("%s has bad shared_extensions" % meta)
    unknown = sorted(set(shared) - list_names("shared-extensions.txt"))
    if unknown:
        raise Rejected("shared extension(s) not in shared-extensions.txt: %s" % ", ".join(map(repr, unknown)))

    files = [
        check_asset(directory, version, platform, "cli", build["cli"], {"file", "sha256", "size"}),
        check_asset(directory, version, platform, "fpm", build["fpm"], {"file", "sha256", "size"}),
    ]
    for ext, asset in shared.items():
        files.append(check_asset(directory, version, platform, ext, asset, {"version", "file", "sha256", "size"}))
        if not matches(TEXT_RE, asset["version"]):
            raise Rejected("%s has a bad version" % ext)
    extra = sorted(set(entries) - set(files) - {meta})
    if extra:
        raise Rejected("files the build does not list: %s" % ", ".join(extra))
    check_sources(version, build["sources"])
    return build, files


def read_artifacts(directory, matrix, recipe, reports):
    """Every build in `directory`, validated: [(path, build, archive names)]."""
    try:
        planned = {(entry["php"], entry["platform"]) for entry in json.loads(matrix)["include"]}
    except (ValueError, KeyError, TypeError):
        raise SystemExit("--matrix is not a build matrix")
    accepted, problems = [], []
    for name in sorted(os.listdir(directory)):
        path = os.path.join(directory, name)
        try:
            build, files = check_artifact(path, planned, recipe, reports)
            accepted.append((path, build, files))
        except Rejected as e:
            problems.append("%s: %s" % (name, e))
    if problems:
        # One bad artifact stops the whole publish: something is wrong with
        # the run, and a person should look before anything is signed.
        raise SystemExit("refusing to publish:\n  " + "\n  ".join(problems))
    return accepted


def merge(args):
    manifest = {"schema": SCHEMA, "builds": []}
    if args.existing and os.path.exists(args.existing):
        with open(args.existing) as f:
            manifest = json.load(f)
        if manifest.get("schema") != SCHEMA:
            raise SystemExit("existing manifest has schema %r, expected %d" % (manifest.get("schema"), SCHEMA))

    accepted = []
    if args.artifacts and os.path.isdir(args.artifacts):
        if args.matrix is None or not args.reports or not args.collect:
            raise SystemExit("--artifacts needs --matrix, --reports and --collect")
        accepted = read_artifacts(args.artifacts, args.matrix, recipe_id(), read_reports(args.reports))

    now = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    builds = {(b["version"], b["platform"]): b for b in manifest["builds"]}
    for path, build, files in accepted:
        os.makedirs(args.collect, exist_ok=True)
        for name in files:
            target = os.path.join(args.collect, name)
            if os.path.lexists(target):
                raise SystemExit("%s already exists in %s" % (name, args.collect))
            shutil.move(os.path.join(path, name), target)
        build["built_at"] = now
        builds[(build["version"], build["platform"])] = build
        print("accepted PHP %s for %s (%d archives)" % (build["version"], build["platform"], len(files)), file=sys.stderr)

    ordered = sorted(builds.values(), key=lambda b: (version_key(b["version"]), b["platform"]), reverse=True)
    latest = {}
    for b in ordered:
        latest.setdefault(b["minor"], {}).setdefault(b["platform"], b["version"])

    manifest = {
        "schema": SCHEMA,
        "updated_at": now,
        "latest": dict(sorted(latest.items(), key=lambda kv: version_key(kv[0]), reverse=True)),
        "builds": ordered,
    }
    with open(args.out, "w") as f:
        json.dump(manifest, f, indent=2)
        f.write("\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)

    p = sub.add_parser("resolve", help="resolve major.minor versions to full versions")
    p.add_argument("versions", help="comma separated, e.g. 8.1,8.2,8.4.20")
    p.set_defaults(func=resolve)

    p = sub.add_parser("matrix", help="print the build matrix for GitHub Actions")
    p.add_argument("versions", help="comma separated, e.g. 8.1,8.2,8.4.20")
    p.add_argument("platforms", help="comma separated, e.g. macos-aarch64,macos-x86_64")
    p.add_argument("--skip-published", metavar="MANIFEST", help="skip builds this manifest already has with the current recipe")
    p.set_defaults(func=matrix)

    p = sub.add_parser("recipe", help="print the recipe id of the current build files")
    p.set_defaults(func=recipe)

    p = sub.add_parser("merge", help="merge build files into manifest.json")
    p.add_argument("--existing", help="manifest to merge into (missing file = start fresh)")
    p.add_argument("--out", required=True)
    p.add_argument("--artifacts", metavar="DIR", help="one directory per build artifact (missing: no new builds)")
    p.add_argument("--matrix", metavar="JSON", help="the matrix this run planned, as printed by `matrix`")
    p.add_argument("--reports", metavar="FILE", help="what the build jobs reported: <job name><tab><annotation line>")
    p.add_argument("--collect", metavar="DIR", help="where the archives of accepted builds are moved")
    p.set_defaults(func=merge)

    args = parser.parse_args()
    args.func(args)


if __name__ == "__main__":
    sys.exit(main())
