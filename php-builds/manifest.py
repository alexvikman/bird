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

  manifest.py merge [--existing manifest.json] --out manifest.json build.json...
      Merges the per-build files written by build.sh into a manifest. Builds
      already in the existing manifest are kept unless rebuilt (same version
      and platform), so a partial run never drops published versions.
"""

import argparse
import hashlib
import json
import os
import re
import sys
import urllib.request
from datetime import datetime, timezone

SCHEMA = 1

HERE = os.path.dirname(os.path.abspath(__file__))

# Everything that decides what a build contains. A change to any of these
# gives a new recipe id, and the scheduled run rebuilds every version.
RECIPE_FILES = ["build.sh", "extensions.txt", "shared-extensions.txt", "libs.txt", "sources.txt"]

# GitHub runner per platform.
RUNNERS = {"macos-aarch64": "macos-15", "macos-x86_64": "macos-15-intel"}


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


def merge(args):
    manifest = {"schema": SCHEMA, "builds": []}
    if args.existing and os.path.exists(args.existing):
        with open(args.existing) as f:
            manifest = json.load(f)
        if manifest.get("schema") != SCHEMA:
            raise SystemExit("existing manifest has schema %r, expected %d" % (manifest.get("schema"), SCHEMA))

    now = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    builds = {(b["version"], b["platform"]): b for b in manifest["builds"]}
    for path in args.builds:
        with open(path) as f:
            build = json.load(f)
        build["built_at"] = now
        builds[(build["version"], build["platform"])] = build

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
    p.add_argument("builds", nargs="+", help="php-<version>-<platform>.json files from build.sh")
    p.set_defaults(func=merge)

    args = parser.parse_args()
    args.func(args)


if __name__ == "__main__":
    sys.exit(main())
