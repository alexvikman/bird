# Bird

Local PHP development without the bloat: `*.test` domains, HTTPS and several
PHP versions side by side. One Rust binary, a CLI, and nothing else.

```bash
bird install            # CA, latest PHP, Composer, DNS resolver, launchd service (asks for sudo once)
cd ~/Code && bird park  # every folder in ~/Code is now <folder>.test
cd my-app && bird isolate 8.3
```

## How it works

| Piece | Bird |
|---|---|
| DNS | Built-in DNS server on `127.0.0.1:53535`; `/etc/resolver/test` points macOS at it. No dnsmasq. |
| Web server | Built-in HTTP/1.1 + HTTP/2 server (hyper + rustls). Static files are served directly, PHP goes to PHP-FPM over FastCGI. No nginx, no config to regenerate. |
| HTTPS | A local CA (trusted once at install) issues a certificate the first time a hostname is requested. Every site and subdomain works over HTTPS. The CA is name-constrained to the TLD (and no IP addresses), so even a leaked key can't impersonate real domains; `bird tld` replaces it. |
| Ports 80/443 | launchd binds them as root and hands the sockets to the daemon, which runs as you. No privileged helper stays running. |
| PHP | Bird's own static builds ([php-builds/](php-builds/), made with [static-php-cli](https://static-php.dev)), checked against sha256 sums from the release manifest, in `~/.bird/php/<version>`. One FPM pool per version, started on first request and stopped when idle. |
| CLI | `~/.bird/bin/php`, `php8.4` and `composer` are shims that pick the same PHP version the site is served with. |

PHP version per site, in order: `bird isolate` → `.php-version` in the project → global (`bird use`).

Opcache is on by default with `opcache.revalidate_freq = 0`: every request
checks the files' modification times, so an edit shows up on the next request
(a Laravel page goes from ~65 ms to ~9 ms). `bird opcache off` turns it off for
all versions. Bird's settings live in `~/.bird/php/<version>/conf.d/00-bird.ini`;
override them in a later file such as `conf.d/99-local.ini`.

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
bird ini [VERSION] [--edit] | logs [bird|8.4] [-f] | tld [NAME] | open [NAME]
bird php …  | bird composer …
bird import herd [--install-php]
```

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
