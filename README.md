# Bird

Local PHP development without the bloat: `*.test` domains, HTTPS and several
PHP versions side by side. One Rust binary with a CLI, plus an optional menu
bar app on macOS. Runs on macOS and Linux.

## Install

Download `bird-<version>-<target>.tar.gz` for your platform from the
[latest release](https://github.com/alexvikman/bird/releases/latest), check it
against `SHA256SUMS` (or `gh attestation verify <file> -R alexvikman/bird`),
unpack it and run:

```bash
./bird install          # CA, latest PHP, Composer, DNS, background service (asks for sudo once)
cd ~/Code && bird park  # every folder in ~/Code is now <folder>.test
cd my-app && bird isolate 8.3
```

`bird self-update` installs new releases (and updates Bird.app); `bird
self-update --check` only looks. From source: `cargo build --release &&
./target/release/bird install`.

## How it works

| Piece | Bird |
|---|---|
| DNS | Built-in DNS server on `127.0.0.1:53535`. On macOS `/etc/resolver/test` points at it; on Linux a systemd-resolved drop-in routes `~test` to it. No dnsmasq. |
| Web server | Built-in HTTP/1.1 + HTTP/2 server (hyper + rustls). Static files are served directly, PHP goes to PHP-FPM over FastCGI. No nginx, no config to regenerate. |
| HTTPS | A local CA (trusted once at install) issues a certificate the first time a hostname is requested. Every site and subdomain works over HTTPS. The CA is name-constrained to the TLD (and no IP addresses), so even a leaked key can't impersonate real domains; `bird tld` replaces it. On Linux it goes into the system trust store and Chrome/Firefox's NSS stores (needs `certutil`). |
| Ports 80/443 | macOS: launchd binds them as root and hands the sockets to the daemon, which runs as you. Linux: a systemd service runs the daemon as you with only `CAP_NET_BIND_SERVICE`, which it drops right after binding. No privileged helper stays running. |
| PHP | Bird's own static builds ([php-builds/](php-builds/), made with [static-php-cli](https://static-php.dev)), checked against sha256 sums from the release manifest, in `~/.bird/php/<version>`. One FPM pool per version, started on first request and stopped when idle. |
| CLI | `~/.bird/bin/php`, `php8.4` and `composer` are shims that pick the same PHP version the site is served with. |

PHP version per site, in order: `bird isolate` → `.php-version` in the project → global (`bird use`).

Opcache is on by default with `opcache.revalidate_freq = 0`: every request
checks the files' modification times, so an edit shows up on the next request
(a Laravel page goes from ~65 ms to ~9 ms). `bird opcache off` turns it off for
all versions. Bird's settings live in `~/.bird/php/<version>/conf.d/00-bird.ini`;
override them in a later file such as `conf.d/99-local.ini`.

Xdebug costs nothing until you ask for it. Each PHP version gets a second
FPM pool with Xdebug loaded, started on demand; requests that carry an Xdebug
trigger (the Xdebug browser extension's `XDEBUG_SESSION` cookie, or
`?XDEBUG_SESSION=1`, `?XDEBUG_PROFILE=1`, `?XDEBUG_TRACE=1` in the URL) go
there, everything else to the normal pool. Your IDE listens on port 9003;
profiles and traces land in `~/.bird/log/xdebug`. `bird xdebug on` debugs
every request, `off` ignores triggers. Xdebug's settings live in
`~/.bird/php/<version>/xdebug.d/` and can be overridden in `conf.d/`.

## Commands

```
bird install | uninstall [--purge] | doctor | status
bird start | stop | restart [VERSION] [--daemon]
bird park [PATH] | forget [PATH] | paths
bird link [NAME] [--secure] [--isolate 8.3] | unlink [NAME] | sites
bird secure [NAME] | unsecure [NAME]          # redirect HTTP → HTTPS
bird proxy NAME http://127.0.0.1:5173 | unproxy NAME
bird use 8.4 | isolate 8.3 [--site NAME] | unisolate | which-php
bird php:install 8.3 8.4 | php:list [--available] | php:update | php:remove 8.3
bird opcache [on|off]                          # on by default; files revalidated every request
bird xdebug [auto|on|off]                      # auto by default: Xdebug only when triggered
bird debug artisan migrate                     # php with Xdebug, starts a debug session
bird coverage vendor/bin/pest --coverage       # php with PCOV
bird ini [VERSION] [--edit] | logs [bird|8.4] [-f] | tld [NAME] | open [NAME]
bird php …  | bird composer …
bird import herd [--install-php]
```

## Menu bar app

`app/` holds Bird's macOS app (SwiftUI):

- **Menu bar menu** for the quick things: status, open a site, restart Bird or
  PHP, start/stop Bird, switch the global PHP version (⌘1…), opcache, Xdebug.
- **Bird window** (⌘O) to manage everything else: sites (PHP version per
  site, HTTPS redirect, open in browser/Finder/Terminal, link and unlink),
  proxies, parked folders, PHP versions (install, update, remove, make
  global) and settings (including `bird doctor`).

It reads `bird state` and makes every change through the normal `bird`
commands, so it never edits Bird's files itself.

```bash
app/build.sh --install   # builds Bird.app into ~/Applications and starts it
```

Needs the Xcode command line tools (no Xcode). The app is signed ad hoc, which
is enough on your own Mac; "Open menu bar app at login" is in Settings.

## Files

```
~/.bird/config.toml   global config, sites, proxies (hand-editable; picked up live)
~/.bird/bin/          bird + shims — put this first on PATH
~/.bird/php/8.4/      bin/php, bin/php-fpm, php.ini, conf.d/
~/.bird/ca/           local certificate authority
~/.bird/log/          bird.log, php-8.4-fpm.log
```

Set `BIRD_HOME` to use another directory (handy for testing:
`BIRD_HOME=/tmp/bird bird daemon --http-port 8080 --https-port 8443`).

## Not in scope

Databases, mail catchers, dump servers, AI assistants, deployment, telemetry.
Use DBngin/Docker/Homebrew for services and fnm/nvm for Node.

## Development

```bash
cargo test
cargo clippy --all-targets
```
