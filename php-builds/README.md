# Bird PHP builds

Bird's own PHP binaries, built with [static-php-cli](https://static-php.dev)
(`spc`, MIT). They replace the prebuilt "common" set on dl.static-php.dev,
which lacks opcache, intl and sodium, lags behind patch releases, and cannot
load Xdebug.

| File | Purpose |
| --- | --- |
| `extensions.txt` | Extensions compiled into `php` and `php-fpm` |
| `shared-extensions.txt` | Extensions built as loadable `.so` files (Xdebug) |
| `libs.txt` | Extra libraries (gd: FreeType, JPEG, WebP) |
| `sources.txt` | Pinned PECL sources (otherwise spc takes the newest, even alphas) |
| `build.sh` | Builds, tests and packages one PHP version on the current Mac |
| `manifest.py` | Resolves versions from php.net; merges build metadata into `manifest.json` |
| `../.github/workflows/php-builds.yml` | Builds PHP 8.1–8.5 × macOS arm64/x86_64 and publishes them |

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

Before packaging, `build.sh` checks that:

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

On Linux the same only works with spc's glibc target (`bin/spc-gnu-docker`),
not the default static musl build.

## Releases

The workflow publishes to one rolling GitHub Release with the tag `php`
(marked "not latest", so it never shadows Bird's own releases). Every archive
has a unique name, so older patch versions stay downloadable; `manifest.json`
is replaced on each publish:

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

### Archives

| Name | Contains (at the archive root) |
| --- | --- |
| `php-<version>-cli-<platform>.tar.gz` | `php` |
| `php-<version>-fpm-<platform>.tar.gz` | `php-fpm` |
| `php-<version>-<ext>-<platform>.tar.gz` | `<ext>.so`, e.g. `xdebug.so` |

`<platform>` is `macos-aarch64` or `macos-x86_64` (as `platform()` in
`src/php.rs`). The cli/fpm naming is the same as dl.static-php.dev's, so
Bird's current download code works unchanged with
`php_mirror = "https://github.com/<owner>/<repo>/releases/download/php"`; only
the version listing (`?format=json` today) has to switch to `manifest.json`.

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

Installing PHP `8.4` on `macos-aarch64`, then, is:

1. Fetch `manifest.json`, check `schema == 1`.
2. `full = latest["8.4"]["macos-aarch64"]` (missing → not available).
3. Find the build with `version == full` and that platform.
4. Download `cli.file` and `fpm.file`, verify `sha256`, and extract `php` /
   `php-fpm` as today.
5. Optionally the same for `shared_extensions.xdebug`.

## Not done yet

- Linux builds (`linux-x86_64`, `linux-aarch64`): needs spc's Docker builds in
  `build.sh`; glibc for Xdebug.
- A scheduled run that picks up new patch releases automatically (commented
  out in the workflow; it should skip versions already in the manifest).
- Signing/notarization. Bird downloads the archives itself, so they carry no
  quarantine attribute and Gatekeeper does not check them; the linker's ad-hoc
  signature is enough to run on Apple Silicon.
