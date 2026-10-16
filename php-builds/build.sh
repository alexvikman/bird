#!/usr/bin/env bash
# Builds php (CLI) and php-fpm for one PHP version with static-php-cli (spc),
# plus the shared extensions in shared-extensions.txt, and packages them the
# way Bird's installer expects:
#
#   php-<full>-cli-<platform>.tar.gz      -> php        (at the archive root)
#   php-<full>-fpm-<platform>.tar.gz      -> php-fpm
#   php-<full>-<ext>-<platform>.tar.gz    -> <ext>.so   (one per shared extension)
#   php-<full>-<platform>.json            -> build metadata, merged by manifest.py
#
# Usage: php-builds/build.sh <8.4 | 8.4.26> [--out DIR]
#
# Environment:
#   SPC_VERSION       static-php-cli release to use (default below)
#   BIRD_BUILD_WORK   work directory for spc, sources and caches
#                     (default: php-builds/.work)
#   SPC_PREBUILT=1    let spc download its pre-built static libraries instead
#                     of compiling them (much faster, but trusts spc's binaries)
#   SPC_DEBUG=1       verbose spc output
set -euo pipefail

SPC_VERSION="${SPC_VERSION:-2.8.5}"

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
work="${BIRD_BUILD_WORK:-$here/.work}"
out="$here/dist"

die() { echo "error: $*" >&2; exit 1; }
log() { printf '\n==> %s\n' "$*" >&2; }

# Reads a list file: one item per line, '#' comments and blank lines ignored.
read_list() { sed -e 's/#.*//' -e 's/[[:space:]]//g' "$1" | grep -v '^$' || true; }
join_commas() { paste -sd, -; }

version=""
while [ $# -gt 0 ]; do
    case "$1" in
        --out) out="$2"; shift 2 ;;
        -h|--help) sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        -*) die "unknown option $1" ;;
        *) [ -z "$version" ] || die "only one version per run"; version="$1"; shift ;;
    esac
done
[ -n "$version" ] || die "usage: $0 <php version> [--out DIR]"
mkdir -p "$out" "$work"
out="$(cd "$out" && pwd)"   # absolute: we cd into the build tree below
work="$(cd "$work" && pwd)"

case "$(uname -s)-$(uname -m)" in
    Darwin-arm64)  platform="macos-aarch64"; spc_asset="spc-macos-aarch64.tar.gz" ;;
    Darwin-x86_64) platform="macos-x86_64";  spc_asset="spc-macos-x86_64.tar.gz" ;;
    # TODO(linux): spc builds Linux binaries in Docker (bin/spc-alpine-docker for
    # musl, bin/spc-gnu-docker for glibc). Xdebug needs the glibc variant, since
    # a fully static musl binary cannot dlopen extensions.
    *) die "unsupported build host $(uname -s)-$(uname -m) (macOS only for now)" ;;
esac

# Resolve 8.4 -> latest 8.4.x from php.net.
if [[ "$version" =~ ^[0-9]+\.[0-9]+$ ]]; then
    full="$(curl -fsSL "https://www.php.net/releases/index.php?json&version=$version" |
        python3 -I -c 'import json,sys; print(json.load(sys.stdin)["version"])')"
    [ -n "$full" ] || die "could not resolve the latest PHP $version release"
elif [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    full="$version"
else
    die "'$version' is not a PHP version (expected e.g. 8.4 or 8.4.26)"
fi
minor="${full%.*}"

static_exts="$(read_list "$here/extensions.txt" | join_commas)"
shared_exts="$(read_list "$here/shared-extensions.txt" | join_commas)"
libs="$(read_list "$here/libs.txt" | join_commas)"
all_exts="$static_exts${shared_exts:+,$shared_exts}"

log "PHP $full for $platform with spc $SPC_VERSION"

# --- static-php-cli -----------------------------------------------------------
spc_dir="$work/spc-$SPC_VERSION-$platform"
spc="$spc_dir/spc"
if [ ! -x "$spc" ]; then
    log "Downloading spc $SPC_VERSION"
    mkdir -p "$spc_dir"
    curl -fsSL --retry 3 -o "$spc_dir/spc.tar.gz" \
        "https://github.com/crazywhalecc/static-php-cli/releases/download/$SPC_VERSION/$spc_asset"
    tar -xzf "$spc_dir/spc.tar.gz" -C "$spc_dir"
    rm "$spc_dir/spc.tar.gz"
    [ -x "$spc" ] || die "spc binary not found in $spc_asset"
fi

# spc works in the current directory (downloads/, source/, buildroot/), so give
# every PHP version its own tree.
build_dir="$work/$platform/$full"
mkdir -p "$build_dir"
cd "$build_dir"

spc_flags=()
[ "${SPC_DEBUG:-}" = "1" ] && spc_flags+=(--debug)

# Checks for (and installs, via Homebrew) autoconf, automake, libtool, bison,
# re2c and friends.
log "spc doctor"
"$spc" doctor --auto-fix

log "Downloading sources"
download_flags=(--retry=3)
while read -r name url; do
    [ -n "$name" ] && download_flags+=(-U "$name:$url")
done < <(sed -e 's/#.*//' "$here/sources.txt" | grep -v '^[[:space:]]*$')
[ "${SPC_PREBUILT:-}" = "1" ] && download_flags+=(--prefer-pre-built)
"$spc" download --with-php="$full" --for-extensions="$all_exts" --for-libs="$libs" \
    --ignore-cache-sources=php-src "${download_flags[@]}" ${spc_flags[@]+"${spc_flags[@]}"}

log "Building"
build_flags=(--build-cli --build-fpm --with-libs="$libs")
[ -n "$shared_exts" ] && build_flags+=(--build-shared="$shared_exts")
"$spc" build "$static_exts" "${build_flags[@]}" ${spc_flags[@]+"${spc_flags[@]}"}

php="$build_dir/buildroot/bin/php"
fpm="$build_dir/buildroot/bin/php-fpm"
modules="$build_dir/buildroot/modules"

# --- smoke tests -----------------------------------------------------------------
log "Testing"
"$php" -n -v
"$fpm" -n -v

actual="$("$php" -n -r 'echo PHP_VERSION;')"
[ "$actual" = "$full" ] || die "built PHP reports $actual, expected $full"

check_exts() { # <binary> <exts...>
    local bin="$1"; shift
    # shellcheck disable=SC2016
    "$bin" -n -r '
        $missing = [];
        foreach (array_slice($argv, 1) as $ext) {
            $ok = match ($ext) {
                "opcache" => extension_loaded("Zend OPcache"),
                "mbregex" => function_exists("mb_ereg"),
                default => extension_loaded($ext),
            };
            if (!$ok) $missing[] = $ext;
        }
        if ($missing) { fwrite(STDERR, "missing: " . implode(", ", $missing) . "\n"); exit(1); }
    ' -- "$@"
}
# shellcheck disable=SC2046
check_exts "$php" $(read_list "$here/extensions.txt") || die "php is missing extensions"
# php-fpm has no -r; compare its module list instead.
fpm_modules="$("$fpm" -n -m | tr '[:upper:]' '[:lower:]')"
for ext in $(read_list "$here/extensions.txt"); do
    case "$ext" in
        opcache) name="zend opcache" ;;
        mbregex) continue ;;
        *) name="$ext" ;;
    esac
    grep -qx "$name" <<<"$fpm_modules" || die "php-fpm is missing $ext"
done

for ext in $(read_list "$here/shared-extensions.txt"); do
    so="$modules/$ext.so"
    [ -f "$so" ] || die "$so was not built"
    flag="extension=$so"
    [ "$ext" = "xdebug" ] && flag="zend_extension=$so"
    "$php" -n -d "$flag" -r "exit(extension_loaded('$ext') ? 0 : 1);" ||
        die "php cannot load $ext.so"
    "$fpm" -n -d "$flag" -m | grep -qix "$ext" || die "php-fpm cannot load $ext.so"
done

# Binaries must only depend on the OS, never on Homebrew or the build tree.
for bin in "$php" "$fpm" "$modules"/*.so; do
    [ -e "$bin" ] || continue
    bad="$(otool -L "$bin" | tail -n +2 | awk '{print $1}' | grep -Ev '^(/usr/lib/|/System/)' || true)"
    [ -z "$bad" ] || die "$(basename "$bin") links against non-system libraries: $bad"
done

# --- packaging ---------------------------------------------------------------------
log "Packaging into $out"
mkdir -p "$out"
export COPYFILE_DISABLE=1 # no ._ AppleDouble files in the archives

pack() { # <archive name> <dir> <file>
    tar --uid 0 --gid 0 --numeric-owner -czf "$out/$1" -C "$2" "$3"
}
pack "php-$full-cli-$platform.tar.gz" "$build_dir/buildroot/bin" php
pack "php-$full-fpm-$platform.tar.gz" "$build_dir/buildroot/bin" php-fpm
for ext in $(read_list "$here/shared-extensions.txt"); do
    pack "php-$full-$ext-$platform.tar.gz" "$modules" "$ext.so"
done

# Build metadata for manifest.py. Shared extension versions come from the
# built module itself.
shared_versions="$(
    for ext in $(read_list "$here/shared-extensions.txt"); do
        flag="extension=$modules/$ext.so"
        [ "$ext" = "xdebug" ] && flag="zend_extension=$modules/$ext.so"
        printf '%s=%s\n' "$ext" "$("$php" -n -d "$flag" -r "echo phpversion('$ext');")"
    done
)"
python3 -I - "$out" "$full" "$minor" "$platform" "$SPC_VERSION" "$static_exts" "$shared_versions" \
    "$build_dir/downloads/.lock.json" <<'PY'
import hashlib, json, os, sys

out, full, minor, platform, spc_version, static_exts, shared, lock = sys.argv[1:9]

def entry(name):
    path = os.path.join(out, name)
    with open(path, "rb") as f:
        digest = hashlib.sha256(f.read()).hexdigest()
    return {"file": name, "sha256": digest, "size": os.path.getsize(path)}

shared_exts = {}
for line in shared.splitlines():
    ext, _, ver = line.partition("=")
    shared_exts[ext] = dict(version=ver, **entry(f"php-{full}-{ext}-{platform}.tar.gz"))

build = {
    "version": full,
    "minor": minor,
    "platform": platform,
    "spc_version": spc_version,
    "cli": entry(f"php-{full}-cli-{platform}.tar.gz"),
    "fpm": entry(f"php-{full}-fpm-{platform}.tar.gz"),
    "extensions": static_exts.split(","),
    "shared_extensions": shared_exts,
    # What spc downloaded, for provenance (source name -> archive name).
    "sources": {name: src.get("filename") or src.get("dirname") for name, src in sorted(json.load(open(lock)).items())},
}
with open(os.path.join(out, f"php-{full}-{platform}.json"), "w") as f:
    json.dump(build, f, indent=2)
    f.write("\n")
PY

ls -l "$out"/php-"$full"-*
log "Done: PHP $full for $platform"
