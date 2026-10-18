# Bird PHP builds

Bird's own PHP binaries, built with [static-php-cli](https://static-php.dev)
(`spc`, MIT). They replace the prebuilt "common" set on dl.static-php.dev,
which lacks opcache, intl and sodium, lags behind patch releases, and cannot
load Xdebug.

| File | Purpose |
| --- | --- |
| `extensions.txt` | Extensions compiled into `php` and `php-fpm` |
| `shared-extensions.txt` | Extensions built as loadable `.so` files (Xdebug, PCOV) |
| `libs.txt` | Extra libraries (gd: FreeType, JPEG, WebP) |
| `sources.txt` | Pinned PECL sources with sha256 (otherwise spc takes the newest, even alphas) |
| `build.sh` | Builds, tests and packages one PHP version on the current Mac |
| `manifest.py` | Resolves versions from php.net; merges build metadata into `manifest.json` |
| `../.github/workflows/php-builds.yml` | Builds PHP 8.1–8.5 × macOS arm64/x86_64 and publishes them |

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

Needs the Xcode command line tools and Homebrew; `spc doctor --auto-fix`
installs missing build tools (autoconf, automake, libtool, bison, re2c, …) with
`brew`. Everything else lives in `php-builds/.work/` (sources, build trees,
~4 GB per version; a build takes ~10 minutes on an M-series Mac) and the results in `php-builds/dist/` (both git-ignored).

| Variable | Default | |
| --- | --- | --- |
| `SPC_VERSION` | `2.8.5` | static-php-cli release |
| `BIRD_BUILD_WORK` | `php-builds/.work` | Work directory |
| `SPC_PREBUILT=1` | off | Use spc's pre-built static libraries instead of compiling them. Much faster, but trusts binaries from the spc project. |
| `SPC_DEBUG=1` | off | Verbose spc output |

Before building, `build.sh` verifies the spc binary against a pinned sha256,
php-src against the sha256 php.net publishes, and the PECL sources against
`sources.txt`. Before packaging, it checks that:

- `php -v` reports the requested version,
- every extension in `extensions.txt` is loaded in both `php` and `php-fpm`,
- every shared extension loads in both `php` and `php-fpm`,
- the binaries only link against system libraries (`/usr/lib`, `/System`).

Binaries target macOS 12+ (spc's default `MACOSX_DEPLOYMENT_TARGET`).

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

## Releases

The workflow publishes to one rolling GitHub Release with the tag `php`
(marked "not latest", so it never shadows Bird's own releases). Every archive
has a unique name (the version, platform and the start of its sha256), so
older builds stay downloadable and a rebuild never replaces a file the live
manifest points at; `manifest.json` is the only file replaced on a publish:

```
https://github.com/<owner>/<repo>/releases/download/php/manifest.json
https://github.com/<owner>/<repo>/releases/download/php/php-8.4.26-cli-macos-aarch64.tar.gz
```

Run it from the Actions tab or with:

```bash
gh workflow run php-builds.yml -f versions=8.4 -f publish=true
```

`publish` defaults to false, so a run without it only builds and keeps the
archives as workflow artifacts.

### Weekly run

Every Monday at 04:00 UTC the workflow runs on its own and publishes. It only
builds what the published `manifest.json` lacks: a new PHP patch release, or
every version when the build files changed. Each build records a **recipe id**
(`manifest.py recipe`, a hash of `build.sh`, the list files and the pins), and
a published build only counts as current with the same recipe. So adding an
extension or bumping a pin rebuilds everything the next Monday; a week with no
new PHP releases builds nothing and finishes in seconds.

Manual runs can do the same with `only-missing`:

```bash
gh workflow run php-builds.yml -f only-missing=true -f publish=true
```

GitHub disables scheduled workflows in public repositories after 60 days
without activity in the repository; re-enable it in the Actions tab.

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
`src/php.rs`). Bird's default `php_mirror` is this release; it reads
`manifest.json` and verifies every archive's size and sha256 before
extracting it.

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
      "sources": { "php-src": "php-8.4.26.tar.xz", "openssl": "openssl-3.6.5.tar.gz", "…": "…" },
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
- `sources`: archives spc downloaded, for provenance only.
- `recipe`: id of the build files that made this build (see "Weekly run").

Installing PHP `8.4` on `macos-aarch64`, then, is:

1. Fetch `manifest.json`, check `schema == 1`.
2. `full = latest["8.4"]["macos-aarch64"]` (missing → not available).
3. Find the build with `version == full` and that platform.
4. Download `cli.file` and `fpm.file`, verify `sha256`, and extract `php` /
   `php-fpm` as today.
5. Optionally the same for `shared_extensions.xdebug`.

## Not done yet

- Signing/notarization. Bird downloads the archives itself, so they carry no
  quarantine attribute and Gatekeeper does not check them; the linker's ad-hoc
  signature is enough to run on Apple Silicon.
