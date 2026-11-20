# Bird PHP builds

Bird's own PHP binaries, built with [static-php-cli](https://static-php.dev)
(`spc`, MIT). They replace the prebuilt "common" set on dl.static-php.dev,
which lacks opcache, intl and sodium, lags behind patch releases, and cannot
load Xdebug.

| File | Purpose |
| --- | --- |
| `extensions.txt` | Extensions compiled into `php` and `php-fpm` |
| `shared-extensions.txt` | Extensions built as loadable `.so` files (Xdebug, PCOV) |
| `libs.txt` | Extra libraries (gd: FreeType, JPEG, WebP; curl: HTTP/2 via nghttp2) |
| `sources.txt` | Sources pinned by hand: PECL versions with sha256 (otherwise spc takes the newest, even alphas) |
| `sources.lock.json` | Every other source and prebuilt tool spc downloads: URL and sha256, or git URL and commit (see "Sources") |
| `php-keys.txt` | Fingerprints of PHP's release managers per branch, for php-src's signature |
| `sources.py` | Pins, verifies and records what a build downloads; refreshes the lock |
| `build.sh` | Builds, tests and packages one PHP version on the current machine |
| `libde265-sse-only.php` | spc patch for linux-x86_64: libde265 without AVX kernels (see "Xdebug") |
| `manifest.py` | Resolves versions from php.net; validates the builds of a run and merges them into `manifest.json` |
| `test_manifest.py`, `test_sources.py` | Tests for the two helpers: `python3 -m unittest discover -s php-builds` |
| `../.github/workflows/php-builds.yml` | Builds PHP 8.1–8.5 for macOS and Linux (arm64, x86_64) and publishes them |

## Extensions

`extensions.txt` matches what Laravel Herd ships for PHP 8.3–8.5 (compared
against Herd's `php -m`), which covers Laravel's own requirements (ctype,
curl, dom, fileinfo, filter, mbstring, openssl, pdo, session, tokenizer, xml)
and what common packages expect: intl, gd, imagick, bcmath, redis, mongodb,
ldap, imap, sqlsrv, sodium, zip, … plus apcu. Xdebug and PCOV are built as
shared extensions and stay off until enabled.

Lines in `extensions.txt`, `shared-extensions.txt` and `sources.txt` can end
in a PHP version condition (`>=8.3` or `<8.3`):

- `imap` is only built for 8.3+: PHP 8.4 moved it to PECL, and the PECL
  release needs 8.3+.
- `sqlsrv`/`pdo_sqlsrv` 5.13 need 8.3+; 8.1 and 8.2 get 5.12.

`sqlsrv`/`pdo_sqlsrv` also need Microsoft's ODBC driver at runtime
(`brew install msodbcsql18`), as with any PHP build.

## Building locally

```bash
php-builds/build.sh 8.4            # latest 8.4.x from php.net
php-builds/build.sh 8.4.26 --out /tmp/php
```

Needs the Xcode command line tools, Homebrew and `gpgv`
(`brew install gnupg`, for php-src's signature); `spc doctor --auto-fix`
installs missing build tools (autoconf, automake, libtool, bison, re2c, …) with
`brew`. Everything else lives in `php-builds/.work/` (sources, build trees,
~4 GB per version; a build takes ~10 minutes on an M-series Mac) and the results in `php-builds/dist/` (both git-ignored).

| Variable | Default | |
| --- | --- | --- |
| `SPC_VERSION` | `2.8.5` | static-php-cli release |
| `BIRD_BUILD_WORK` | `php-builds/.work` | Work directory |
| `SPC_PREBUILT=1` | off | Use spc's pre-built static libraries instead of compiling them. Much faster, but trusts binaries from the spc project that nothing can be pinned to: the build is marked unverified and is never published. |
| `SPC_DEBUG=1` | off | Verbose spc output |

Before building, `build.sh` verifies everything it downloaded (see
"Sources"). Before packaging, it checks that:

- `php -v` reports the requested version,
- every extension in `extensions.txt` is loaded in both `php` and `php-fpm`,
- every shared extension loads in both `php` and `php-fpm`,
- the binaries only link against system libraries (`/usr/lib`, `/System`).

Binaries target macOS 12+ (spc's default `MACOSX_DEPLOYMENT_TARGET`).

## Sources

spc verifies nothing it downloads, and for most of the ~65 sources of a build
it takes whatever is newest that day; a few are git clones of a moving branch,
and two (pkg-config, zig) are prebuilt programs it runs. So everything is
pinned in this directory, and `build.sh` refuses to build anything else:

| What | Pinned by | Checked |
| --- | --- | --- |
| spc itself | sha256 per platform in `build.sh` | before it runs |
| php-src | its URL on php.net, `php-keys.txt` | the sha256 php.net publishes, and the release's OpenPGP signature |
| PECL extensions picked by hand | `sources.txt`: URL, sha256 | sha256 |
| every other archive | `sources.lock.json`: URL, sha256 | sha256 |
| git sources (`ext-zstd`, `libaom`, `libjxl`, `imap`, `qdbm`, `micro`) | `sources.lock.json`: URL, commit | checked out at that commit, unmodified |
| prebuilt tools (`pkg-config-*`, `zig-*`) | `sources.lock.json`: URL, sha256 | sha256, before anything runs them |

How `build.sh` applies them:

1. The prebuilt tools are fetched and checked first, before `spc doctor`
   (which would otherwise fetch the newest and run them). zig comes from its
   locked URL. pkg-config is fetched the way spc picks it and then checked:
   given a URL, spc 2.8.5 skips the step that puts the binary in place. So a
   new upstream build of pkg-config fails the check until the lock is
   refreshed.
2. `spc download` gets a `-U <name>:<url>` for every pinned archive, so it
   fetches exactly those files, without falling back to spc's mirror
   (`--no-alt`). That includes php-src: left to itself, spc asks php.net (or
   its own mirror) for the version to fetch and puts the answer on a shell
   command line.
3. spc clones git sources at the tip of their branch; each clone is then
   emptied and checked out again at its locked commit, so nothing of the
   tip stays behind.
4. `sources.py verify` fails the build unless every downloaded source is
   pinned and matches. A source nobody pinned (a new dependency of an
   extension, say) fails too, and so does a clone holding any file that is
   not its commit's, ignored files included.
5. After `spc build`, `sources.py unchanged` fails the build if anything was
   fetched, replaced or edited since the check.

What was used is written into the build's metadata, and so into the signed
`manifest.json` (`sources`): every archive with its sha256, every git source
with its commit, php-src with the fingerprint that signed it.

**php-src.** The version comes from php.net, which also publishes the
tarball's sha256, so that sum alone only shows the download was not damaged.
The release managers sign each tarball (`php-<version>.tar.xz.asc`);
`php-keys.txt` lists their key fingerprints per PHP branch, and the signature
must be good and made by a key listed for that branch (`gpgv`). The keys
themselves are fetched from php.net with the tarball and prove nothing; only
the committed fingerprints decide. A revoked key is never accepted. A key
php.net's keyring shows as expired is refused unless its line says
`expired-ok` (PHP 8.1's last release needs that). A new PHP branch builds only
after its release managers are added to `php-keys.txt`
(<https://www.php.net/gpg-keys.php>; compare with another source, such as the
`GPG_KEYS` of the official Docker images).

**Not covered.** The build tools `spc doctor` installs with Homebrew or apt,
and the runner image itself, are trusted as they come. `SPC_PREBUILT=1` builds
use libraries compiled by the spc project and are marked unverified;
`manifest.py merge` refuses to publish them. The workflow always builds in an
empty tree; a local `.work/` tree that is used again keeps the tools and
libraries an earlier build left there.

### Refreshing the lock

Nothing moves until the lock does: the libraries in a build are those in
`sources.lock.json`, however often it is rebuilt. To move to the current
upstream releases:

```bash
GITHUB_TOKEN=<a token without any access> php-builds/build.sh --refresh-lock
git diff php-builds/sources.lock.json
```

`GITHUB_TOKEN` only lifts GitHub's API rate limit (a full refresh makes about
a hundred requests; sixty an hour are allowed without one), so give it a
token that can do nothing: a fine-grained token without repository access,
not `gh auth token`. A refresh is spc's own unpinned resolution, which is
what the pins protect a build from: spc reads upstream index pages, and puts
what it finds there on shell command lines. It can run in a container,
which then only gets to change this directory:

```bash
docker run --rm -e GITHUB_TOKEN -v "$PWD/php-builds:/php-builds" ubuntu:24.04 sh -c '
  apt-get update -qq && apt-get install -y -qq curl git python3 ca-certificates xz-utils perl >/dev/null &&
  BIRD_BUILD_WORK=/tmp/work /php-builds/build.sh --refresh-lock'
```

A refresh lets spc resolve the newest release of every locked source (the same
choice it would make without pins), finds out which URL each file came from,
turns that into one plain `curl` can fetch again (GitHub's API URLs become
release and tag archive URLs, PECL's "latest" becomes the release it is),
downloads it and writes URL and sha256 to the lock; git sources get the
commit at the tip of their branch. It runs on any machine and covers all
platforms. Read the diff (which versions moved, and from where), try a build,
and commit it. The lock is part of the recipe, so the next weekly run
rebuilds every version with it. (spc adds the token to its request for zig's
release index on ziglang.org; the refresh therefore fetches zig without it.)

```bash
php-builds/build.sh --refresh-lock --only openssl,curl   # just these
php-builds/build.sh --refresh-lock --add libfoo          # a source a build reported as not pinned
php-builds/build.sh --refresh-lock --add-tool zig-x86_64-linux
```

- A build that stops with "… is not pinned" downloaded a source the lock does
  not know, usually after adding an extension: add it with `--add`.
- The versions in `sources.txt` are edited by hand (URL and
  `shasum -a 256`), and are never touched by a refresh.
- After changing `SPC_VERSION`, refresh the whole lock: another spc can name
  or fetch sources differently.
- Tag archives (`github.com/<owner>/<repo>/archive/…`) are generated by
  GitHub on request. Should it ever generate them differently, builds stop
  with a sha256 mismatch although nothing changed upstream; a refresh then
  shows the same URLs with new sums.
- Entries no build uses any more are harmless and can be deleted.

## Xdebug

Xdebug can only be built as a shared extension, and a *fully* static binary
cannot `dlopen` anything. That is a Linux/musl problem only: on macOS there is
no fully static linking, so spc's binaries link `libSystem` dynamically and
`dlopen` works. With `--build-shared=xdebug`, spc builds `xdebug.so` against
the same PHP and links it with `-undefined dynamic_lookup`, resolving the PHP
symbols from the host `php` or `php-fpm` at load time.

So Xdebug ships as a separate archive per PHP version and platform, and is
off by default. To enable it, Bird extracts `xdebug.so` (e.g. to
`~/.bird/php/8.4/modules/`) and adds
`zend_extension=/path/to/xdebug.so` to an ini file. The `.so` only works with
the `php`/`php-fpm` from the same build (same PHP version, same platform).

On Linux the same only works when PHP links against glibc, not with spc's
default fully static musl build. `build.sh` therefore builds Linux binaries
with spc's zig toolchain for glibc 2.28 (`SPC_TARGET=native-native-gnu.2.28`),
which runs on Debian 10+, Ubuntu 20.04+, RHEL 8+ and newer, and checks that no
newer glibc symbol slipped in.

zig's compiler-rt lacks the x86 CPU detection (`__cpu_model`,
`__cpu_indicator_init`) that `__builtin_cpu_supports` needs. libde265
(HEIC decoding for imagick, via libheif) uses it since 1.1.1 to pick its
AVX2 / AVX-512 kernels, so on linux-x86_64 `build.sh` passes spc the patch
script `libde265-sse-only.php`, which turns those kernels off; libde265 keeps
its SSE4.1 code. macOS and linux-aarch64 build libde265 unchanged.

## Releases

The workflow publishes to one rolling GitHub Release with the tag `php`
(marked "not latest", so it never shadows Bird's own releases). Every archive
has a unique name (the version, platform and the start of its sha256), so
older builds stay downloadable and a rebuild never replaces a file the live
manifest points at; `manifest.json` and its signature `manifest.json.sig` are
the only files replaced on a publish:

```
https://github.com/<owner>/<repo>/releases/download/php/manifest.json
https://github.com/<owner>/<repo>/releases/download/php/manifest.json.sig
https://github.com/<owner>/<repo>/releases/download/php/php-8.4.26-cli-macos-aarch64.tar.gz
```

Run it from the Actions tab or with:

```bash
gh workflow run php-builds.yml -f versions=8.4 -f publish=true
```

`publish` defaults to false, so a run without it only builds and keeps the
archives as workflow artifacts. Publishing only runs from `main`, and waits
for the owner's approval of the `php-release` environment (see "Signing").

### Weekly run

Every Monday at 04:00 UTC the workflow runs on its own and publishes (after
approval, see "Signing"). It only
builds what the published `manifest.json` lacks: a new PHP patch release, or
every version when the build files changed. Each build records a **recipe id**
(`manifest.py recipe`, a hash of `build.sh`, the list files, the pins and the
patch script), and
a published build only counts as current with the same recipe. So adding an
extension or bumping a pin rebuilds everything the next Monday; a week with no
new PHP releases builds nothing, asks for no approval and finishes in seconds.

When some builds fail (say Linux, while macOS succeeds), the publish job still
runs and publishes the builds that succeeded; the run is marked failed and the
failed builds' logs are kept as `logs-php-*` artifacts. Since nothing of a
failed build is published, the next Monday builds it again (and asks for
approval again, even if that is all there is). If every build failed, publish
stops before touching the release. A cancelled run publishes nothing. A
dispatch with `publish` behaves the same.

Manual runs can do the same with `only-missing`:

```bash
gh workflow run php-builds.yml -f only-missing=true -f publish=true
```

GitHub disables scheduled workflows in public repositories after 60 days
without activity in the repository; re-enable it in the Actions tab.

### Signing

Bird only trusts `manifest.json` (and through its sha256 sums, every archive)
with a valid `manifest.json.sig`: an SSH signature
(`ssh-keygen -Y sign -n bird-php`, Ed25519) from a key in `PHP_KEYS` in
`src/signing.rs`. It refuses the default mirror's manifest when the signature
is missing or wrong, before reading anything in it. The PHP key is separate
from Bird's release key, so it can't sign Bird updates.

The private key is the `BIRD_PHP_SIGNING_KEY` secret of the `php-release`
environment, which only `main` may use and which requires the owner's
approval. The publish job:

1. stops if `main` has moved since the run started (a run can wait for
   approval for days, and must not publish over newer builds; run the
   workflow again instead),
2. verifies the published `manifest.json` against `manifest.json.sig` before
   merging new builds into it (anyone who can edit the release could replace
   it, and its builds are signed again),
3. validates what each build job uploaded (below) and merges the builds it
   accepts,
4. signs the merged manifest and checks the signature against `PHP_KEYS`,
5. uploads the archives, then `manifest.json.sig`, then `manifest.json`.

A build job is not trusted with more than its own build. Every job's artifact
(`php-<version>-<platform>`) is downloaded into its own directory, and
`manifest.py merge` only accepts it when:

- the run planned that version and platform,
- its build file is the one that very job reported. An artifact's name
  proves nothing (any job of a run can upload under any name), so each build
  job writes the sha256 of its build file into an annotation
  (`bird-php-build <artifact> <sha256>`), and the publish job reads the
  annotations of the job GitHub itself named `PHP <version> (<platform>)`.
  A job cannot write another job's annotations. A job that ran more than
  once (re-run jobs, all of the same commit) has reports from each attempt;
  each attempt may report one build, and any of them may vouch for the
  artifact,
- it holds exactly one build file, `php-<version>-<platform>.json`, whose
  `version`, `platform`, `minor` and `recipe` are those of that job and of
  this commit, with no fields beyond what `build.sh` writes,
- every archive it lists is named
  `php-<version>-<kind>-<platform>-<sha256 start>.tar.gz` for that same
  version and platform, is there, and has the size and sha256 listed,
- it holds no other file, and none of its sources is marked unverified.

So a job that was tampered with can only spoil its own build: it cannot
replace another job's files or its whole artifact, claim another version or
platform, or put a foreign file on the release. One rejected artifact stops
the whole publish, before anything is signed.

A dispatch with `publish` that finds nothing to build re-signs the published
manifest, e.g. after rotating the key:

```bash
gh workflow run php-builds.yml -f only-missing=true -f publish=true
```

If the published `manifest.json` has no `manifest.json.sig`, the publish job
stops: it never signs over a manifest it can't verify (someone who can edit
the release could delete the signature and swap the manifest). Put the last
signed pair back on the release first.

Bird also remembers the `updated_at` of the newest signed manifest it has seen
(`~/.bird/php/.manifest-seen`) and refuses an older one, so an old manifest
with its old, still valid signature can't be used to roll PHP back.

Rotating the key: add the new public key to `PHP_KEYS`, release Bird, replace
the secret, re-sign, and drop the old key in a later release.

A file can be checked by hand with:

```bash
echo "bird-php ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOFCBm+uCYPaByT8wvmM5u+Fw9t0f302T2/bJ1FXGsbp" > allowed_signers
ssh-keygen -Y verify -f allowed_signers -I bird-php -n bird-php -s manifest.json.sig < manifest.json
```

### Archives

| Name | Contains |
| --- | --- |
| `php-<version>-cli-<platform>.tar.gz` | `php` (at the root) and `licenses/` |
| `php-<version>-fpm-<platform>.tar.gz` | `php-fpm` and `licenses/` |
| `php-<version>-<ext>-<platform>.tar.gz` | `<ext>.so`, e.g. `xdebug.so`, and `licenses/` |

`licenses/` holds the licenses of PHP, the extensions and every bundled
library (`spc dump-license`); Bird installs them to `~/.bird/php/<minor>/licenses/`.

`<platform>` is `macos-aarch64`, `macos-x86_64`, `linux-aarch64` or
`linux-x86_64` (as `platform()` in
`src/php.rs`). Bird's default `php_mirror` is this release; it verifies
`manifest.json.sig`, reads `manifest.json` and verifies every archive's size
and sha256 before extracting it. A custom `php_mirror` with a manifest is
trusted without a signature.

## manifest.json

```json
{
  "schema": 1,
  "updated_at": "2026-10-16T19:50:00Z",
  "latest": {
    "8.5": { "macos-aarch64": "8.5.11", "macos-x86_64": "8.5.11" },
    "8.4": { "macos-aarch64": "8.4.26", "macos-x86_64": "8.4.26" }
  },
  "builds": [
    {
      "version": "8.4.26",
      "minor": "8.4",
      "platform": "macos-aarch64",
      "spc_version": "2.8.5",
      "recipe": "2c5cfed08ac3377c",
      "cli": { "file": "php-8.4.26-cli-macos-aarch64.tar.gz", "sha256": "…", "size": 21474836 },
      "fpm": { "file": "php-8.4.26-fpm-macos-aarch64.tar.gz", "sha256": "…", "size": 21474836 },
      "extensions": ["bcmath", "bz2", "…"],
      "shared_extensions": {
        "xdebug": { "version": "3.5.3", "file": "php-8.4.26-xdebug-macos-aarch64.tar.gz", "sha256": "…", "size": 123456 }
      },
      "sources": {
        "php-src": { "file": "php-8.4.26.tar.xz", "sha256": "…", "signed_by": "9D7F99A0CB8F05C8A6958D6256A97AF7600A39A6" },
        "openssl": { "file": "openssl-3.6.5.tar.gz", "sha256": "…" },
        "libaom": { "dir": "libaom", "git": "https://aomedia.googlesource.com/aom", "commit": "…" },
        "…": {}
      },
      "built_at": "2026-10-16T19:50:00Z"
    }
  ]
}
```

- `schema`: bumped on incompatible changes. An installer should refuse a schema
  it does not know.
- `latest`: newest version per `major.minor` and platform. Derived from
  `builds`, for convenience.
- `builds`: every published build, newest first. `(version, platform)` is
  unique.
- `file`: relative to the manifest's URL (same release). `sha256` is the hex
  digest of the `.tar.gz`; `size` is in bytes.
- `extensions`: what is compiled in, from `extensions.txt` (names as in spc;
  `opcache` shows up as "Zend OPcache" in `php -m`).
- `shared_extensions`: optional add-ons, keyed by extension name.
- `sources`: everything that was downloaded for the build, as verified
  before it was built: archives and prebuilt tools with their sha256, git
  sources with their commit, php-src with the fingerprint of the release
  manager's key that signed it. For provenance; Bird does not read it.
  (Builds from before November 2026 only list file names.)
- `recipe`: id of the build files that made this build (see "Weekly run").

Installing PHP `8.4` on `macos-aarch64`, then, is:

1. Fetch `manifest.json` and `manifest.json.sig`, verify the signature
   (namespace `bird-php`) against the PHP key, then check `schema == 1`.
2. `full = latest["8.4"]["macos-aarch64"]` (missing → not available).
3. Find the build with `version == full` and that platform.
4. Download `cli.file` and `fpm.file`, verify `sha256`, and extract `php` /
   `php-fpm` as today.
5. Optionally the same for `shared_extensions.xdebug`.

## Not done yet

- Apple code signing/notarization. Bird downloads the archives itself, so they carry no
  quarantine attribute and Gatekeeper does not check them; the linker's ad-hoc
  signature is enough to run on Apple Silicon.
