#!/usr/bin/env bash
# Builds php (CLI) and php-fpm for one PHP version with static-php-cli (spc),
# plus the shared extensions in shared-extensions.txt, and packages them the
# way Bird's installer expects:
#
#   php-<full>-cli-<platform>.tar.gz      -> php        (at the archive root, plus licenses/)
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

# sha256 of the spc release assets we run. A different SPC_VERSION needs its
# own entry here (or SPC_SHA256 in the environment); nothing unverified runs.
spc_sha256() {
    case "$SPC_VERSION/$1" in
        2.8.5/spc-macos-aarch64.tar.gz) echo acf2f25d56d0cbf8e65aa82e5054fef555f7be7c5c38046c6e0819f266d83225 ;;
        2.8.5/spc-macos-x86_64.tar.gz)  echo e8b798048f62ca4960764196543b60ae703f7174aa418824cf542aeec1d2cd6a ;;
        *) echo "${SPC_SHA256:-}" ;;
    esac
}

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
work="${BIRD_BUILD_WORK:-$here/.work}"
out="$here/dist"

die() { echo "error: $*" >&2; exit 1; }
log() { printf '\n==> %s\n' "$*" >&2; }

# Whether PHP $minor satisfies a condition from a list file: "" (always),
# ">=8.3" or "<8.3". (sed, not head: head exiting early would SIGPIPE sort
# and fail under pipefail.)
applies() {
    local cond="$1" ver lowest
    case "$cond" in
        "") return 0 ;;
        ">="*) ver="${cond#>=}" ;;
        "<"*) ver="${cond#<}" ;;
        *) die "bad PHP version condition '$cond'" ;;
    esac
    [[ "$ver" =~ ^[0-9]+\.[0-9]+$ ]] || die "bad PHP version condition '$cond'"
    lowest="$(printf '%s\n%s\n' "$minor" "$ver" | sort -V | sed -n 1p)"
    case "$cond" in
        ">="*) [ "$lowest" = "$ver" ] ;;
        *) [ "$lowest" = "$minor" ] && [ "$minor" != "$ver" ] ;;
    esac
}

# Reads a list file: one item per line, '#' comments and blank lines ignored,
# optionally followed by a PHP version condition (see applies).
read_list() {
    local name cond
    while read -r name cond; do
        [ -n "$name" ] || continue
        if applies "$cond"; then echo "$name"; fi
    done < <(sed -e 's/#.*//' "$1")
}
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

# PHP before 8.4 predates C23 (8.1's bcmath still has K&R function
# definitions), but autoconf 2.72 picks -std=gnu23 whenever the compiler
# supports it. spc puts these flags last on the compiler command line, so the
# -std here wins. (spc reads its env.ini settings from the environment.)
if ! applies ">=8.4"; then
    export SPC_CMD_VAR_PHP_MAKE_EXTRA_CFLAGS="-g -fstack-protector-strong -fpic -fpie -Werror=unknown-warning-option --target=$(uname -m)-apple-darwin -Os -std=gnu17"
fi

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
    expected="$(spc_sha256 "$spc_asset")"
    [ -n "$expected" ] || { rm -rf "$spc_dir"; die "no pinned sha256 for spc $SPC_VERSION ($spc_asset); add it to build.sh"; }
    echo "$expected  $spc_dir/spc.tar.gz" | shasum -a 256 -c - >/dev/null ||
        { rm -rf "$spc_dir"; die "spc $SPC_VERSION ($spc_asset) does not match its pinned sha256"; }
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
# The pins that apply to this PHP version, for spc and for verification.
sources="$build_dir/sources.applied"
: > "$sources"
while read -r name url sha cond; do
    [ -n "$name" ] || continue
    [ -n "$sha" ] || die "sources.txt: $name has no sha256"
    applies "$cond" || continue
    printf '%s %s %s\n' "$name" "$url" "$sha" >> "$sources"
    download_flags+=(-U "$name:$url")
done < <(sed -e 's/#.*//' "$here/sources.txt")
[ "${SPC_PREBUILT:-}" = "1" ] && download_flags+=(--prefer-pre-built)
"$spc" download --with-php="$full" --for-extensions="$all_exts" --for-libs="$libs" \
    --ignore-cache-sources=php-src "${download_flags[@]}" ${spc_flags[@]+"${spc_flags[@]}"}

# spc does not verify what it downloads. Check what we can before building:
# php-src against the sha256 php.net publishes, pinned sources against
# sources.txt. (The libraries spc fetches from their upstreams over HTTPS are
# trusted as spc configures them.)
log "Verifying sources"
release_json="$(curl -fsSL "https://www.php.net/releases/index.php?json&version=$full")"
python3 -I - "$build_dir/downloads" "$sources" "$release_json" <<'PY'
import hashlib, json, os, sys

downloads, sources_txt, release_json = sys.argv[1:4]
lock = json.load(open(os.path.join(downloads, ".lock.json")))

def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()

def check(name, expected):
    entry = lock.get(name)
    if not entry or not entry.get("filename"):
        sys.exit(f"error: {name} was not downloaded as an archive; cannot verify it")
    path = os.path.join(downloads, entry["filename"])
    actual = sha256(path)
    if actual != expected:
        sys.exit(f"error: {entry['filename']} has sha256 {actual}, expected {expected}")
    print(f"  ok  {entry['filename']}")

published = {s["filename"]: s["sha256"] for s in json.loads(release_json).get("source", [])}
php_file = lock.get("php-src", {}).get("filename", "")
if php_file not in published:
    sys.exit(f"error: php.net publishes no sha256 for {php_file!r}")
check("php-src", published[php_file])

for line in open(sources_txt):
    fields = line.split("#", 1)[0].split()
    if fields:
        name, _url, expected = fields
        check(name, expected)
PY

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
    # Capture first: with pipefail, `grep -q` exiting early SIGPIPEs php-fpm
    # and fails the pipeline even when the module is there.
    loaded="$("$fpm" -n -d "$flag" -m)" || die "php-fpm fails with $ext.so"
    grep -qix "$ext" <<<"$loaded" || die "php-fpm cannot load $ext.so"
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

# PHP, its extensions and the bundled libraries require their licenses to
# ship with the binaries; every archive carries them in licenses/.
licenses="$build_dir/licenses-bundle"
rm -rf "$licenses" && mkdir -p "$licenses/licenses"
# dump-license honours only one of --for-extensions/--for-libs per call and
# empties its target directory, so dump twice and merge.
"$spc" dump-license --for-extensions="$all_exts" --dump-dir="$licenses/licenses" >/dev/null
if [ -n "$libs" ]; then
    "$spc" dump-license --for-libs="$libs" --without-php --dump-dir="$licenses/libs" >/dev/null
    # Both dumps can contain the same library (e.g. zlib); keep the first.
    # (Not `cp -n`: on macOS it exits 1 when it skips a file, which set -e
    # turns into a failed build.)
    for f in "$licenses/libs/"*; do
        [ -e "$licenses/licenses/${f##*/}" ] || cp "$f" "$licenses/licenses/"
    done
    rm -rf "$licenses/libs"
fi
[ -f "$licenses/licenses/src_php-src_0.txt" ] || die "license dump is missing PHP's license"

pack() { # <archive name> <dir> <file>
    tar --uid 0 --gid 0 --numeric-owner -czf "$out/$1" -C "$2" "$3" -C "$licenses" licenses
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
recipe="$(python3 -I "$here/manifest.py" recipe)"
python3 -I - "$out" "$full" "$minor" "$platform" "$SPC_VERSION" "$static_exts" "$shared_versions" \
    "$build_dir/downloads/.lock.json" "$recipe" <<'PY'
import hashlib, json, os, sys

out, full, minor, platform, spc_version, static_exts, shared, lock, recipe = sys.argv[1:10]

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
    # Which build files made this build; see manifest.py recipe.
    "recipe": recipe,
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
