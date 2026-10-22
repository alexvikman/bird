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
        2.8.5/spc-linux-aarch64.tar.gz) echo 675a3840dcdc4ed041fe20eaa54310ce019a9984c1c03951df9ec66df5795213 ;;
        2.8.5/spc-linux-x86_64.tar.gz)  echo 523ba4279c54c7a377156c0dd3a36adf92ee64b01e9a7f5e9e2ec084b8e458e5 ;;
        *) echo "${SPC_SHA256:-}" ;;
    esac
}

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
work="${BIRD_BUILD_WORK:-$here/.work}"
out="$here/dist"

die() { echo "error: $*" >&2; exit 1; }
log() { printf '\n==> %s\n' "$*" >&2; }

# Whether this build matches the conditions of a list-file line: nothing
# (always), PHP versions (">=8.3", "<8.3") and/or an OS ("macos", "linux"),
# separated by spaces; all must hold. (sed, not head: head exiting early
# would SIGPIPE sort and fail under pipefail.)
applies() {
    local cond ver lowest
    for cond in $1; do
        case "$cond" in
            macos | linux) [ "$os" = "$cond" ] || return 1; continue ;;
            ">="*) ver="${cond#>=}" ;;
            "<"*) ver="${cond#<}" ;;
            *) die "bad condition '$cond'" ;;
        esac
        [[ "$ver" =~ ^[0-9]+\.[0-9]+$ ]] || die "bad PHP version condition '$cond'"
        lowest="$(printf '%s\n%s\n' "$minor" "$ver" | sort -V | sed -n 1p)"
        case "$cond" in
            ">="*) [ "$lowest" = "$ver" ] || return 1 ;;
            *) { [ "$lowest" = "$minor" ] && [ "$minor" != "$ver" ]; } || return 1 ;;
        esac
    done
    return 0
}

# Reads a list file: one item per line, '#' comments and blank lines ignored,
# optionally followed by conditions (see applies).
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

# Oldest glibc the Linux builds run on (Debian 10, Ubuntu 20.04, RHEL 8).
GLIBC_TARGET="2.28"

case "$(uname -s)-$(uname -m)" in
    Darwin-arm64)  platform="macos-aarch64"; spc_asset="spc-macos-aarch64.tar.gz" ;;
    Darwin-x86_64) platform="macos-x86_64";  spc_asset="spc-macos-x86_64.tar.gz" ;;
    # On Linux, spc's zig toolchain links against an old glibc instead of
    # building fully static musl binaries: those could not dlopen Xdebug.
    Linux-aarch64) platform="linux-aarch64"; spc_asset="spc-linux-aarch64.tar.gz" ;;
    Linux-x86_64)  platform="linux-x86_64";  spc_asset="spc-linux-x86_64.tar.gz" ;;
    *) die "unsupported build host $(uname -s)-$(uname -m)" ;;
esac
os="${platform%%-*}"
if [ "$os" = linux ]; then
    export SPC_TARGET="native-native-gnu.$GLIBC_TARGET"
fi

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
    if [ "$os" = macos ]; then
        arch="$(uname -m)"
        export SPC_CMD_VAR_PHP_MAKE_EXTRA_CFLAGS="-g -fstack-protector-strong -fpic -fpie -Werror=unknown-warning-option --target=$arch-apple-darwin -Os -std=gnu17"
    else
        export SPC_CMD_VAR_PHP_MAKE_EXTRA_CFLAGS="-g -fstack-protector-strong -fno-ident -fPIE -fPIC -Os -std=gnu17"
    fi
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
# spc's --retry only becomes curl's --retry, and curl reports a pecl.php.net
# HTTP 500 as error 56, which it does not treat as transient: one bad response
# failed the whole build. So retry the whole step, with time for the server to
# recover. A retry fetches php-src and the -U sources again (spc always
# re-downloads those) but skips every other source it already has.
attempt=1
until "$spc" download --with-php="$full" --for-extensions="$all_exts" --for-libs="$libs" \
    --ignore-cache-sources=php-src "${download_flags[@]}" ${spc_flags[@]+"${spc_flags[@]}"}; do
    [ "$attempt" -lt 3 ] || die "downloading sources failed $attempt times"
    log "Downloading sources failed; retrying in $((attempt * 30))s"
    sleep $((attempt * 30))
    attempt=$((attempt + 1))
done

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
# PHP 8.4.27 and 8.5.12 ship a stray ext/imap/tests/ (imap moved to PECL in
# 8.4), and spc only extracts PECL imap into php-src/ext/imap when that
# directory is missing. Extract php-src first and drop the leftover; PHP 8.3's
# own ext/imap has a config.m4 and stays.
if [[ ",$static_exts," == *,imap,* ]]; then
    "$spc" extract php-src ${spc_flags[@]+"${spc_flags[@]}"}
    stray="$build_dir/source/php-src/ext/imap"
    if [ -d "$stray" ] && [ ! -e "$stray/config.m4" ]; then
        rm -rf "$stray"
    fi
fi
build_flags=(--build-cli --build-fpm --with-libs="$libs")
[ -n "$shared_exts" ] && build_flags+=(--build-shared="$shared_exts")
# zig's compiler-rt lacks the x86 CPU detection libde265's AVX kernels need.
[ "$platform" = linux-x86_64 ] && build_flags+=(--with-added-patch="$here/libde265-sse-only.php")
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
# libcurl only speaks HTTP/2 when it was built with nghttp2 (libs.txt).
"$php" -n -r 'exit(curl_version()["features"] & CURL_VERSION_HTTP2 ? 0 : 1);' ||
    die "php's curl has no HTTP/2 support"
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

# Binaries must only depend on the OS, never on Homebrew, distro packages or
# the build tree.
for bin in "$php" "$fpm" "$modules"/*.so; do
    [ -e "$bin" ] || continue
    if [ "$os" = macos ]; then
        bad="$(otool -L "$bin" | tail -n +2 | awk '{print $1}' | grep -Ev '^(/usr/lib/|/System/)' || true)"
    else
        # glibc's own libraries only, and no symbol newer than GLIBC_TARGET.
        bad="$(readelf -d "$bin" | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p' |
            grep -Ev '^(libc|libm|libdl|libpthread|librt|libresolv|libutil|ld-linux[^.]*)\.so' || true)"
        newest="$(objdump -T "$bin" | grep -o 'GLIBC_[0-9.]*' | sed 's/GLIBC_//' | sort -uV | tail -n 1 || true)"
        if [ -n "$newest" ] && [ "$(printf '%s\n%s\n' "$newest" "$GLIBC_TARGET" | sort -V | sed -n 2p)" != "$GLIBC_TARGET" ]; then
            die "$(basename "$bin") needs glibc $newest, newer than $GLIBC_TARGET"
        fi
    fi
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
    local owner=(--uid 0 --gid 0)                        # bsdtar (macOS)
    [ "$os" = linux ] && owner=(--owner=0 --group=0)     # GNU tar
    tar "${owner[@]}" --numeric-owner -czf "$out/$1" -C "$2" "$3" -C "$licenses" licenses
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
    # The archive is renamed after its content (php-8.4.26-cli-macos-aarch64-
    # <sha256 prefix>.tar.gz): a rebuild never replaces a file the published
    # manifest.json points at, so installs keep working while a new build is
    # uploaded, and the manifest is the only file a publish changes.
    path = os.path.join(out, name)
    with open(path, "rb") as f:
        digest = hashlib.sha256(f.read()).hexdigest()
    final = name.removesuffix(".tar.gz") + "-" + digest[:12] + ".tar.gz"
    os.replace(path, os.path.join(out, final))
    return {"file": final, "sha256": digest, "size": os.path.getsize(os.path.join(out, final))}

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
