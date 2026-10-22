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
| PHP | Bird's own static builds ([php-builds/](php-builds/), made with [static-php-cli](https://static-php.dev)), checked against sha256 sums from the signed release manifest, in `~/.bird/php/<version>`. One FPM pool per version, started on first request and stopped when idle. |
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
profiles and traces land in `~/.bird/log/xdebug` (kept for a week, at most
1 GB). `bird xdebug on` debugs
every request, `off` ignores triggers. Xdebug's settings live in
`~/.bird/php/<version>/xdebug.d/` and can be overridden in `conf.d/`.

### Vite over HTTPS

laravel-vite-plugin's `detectTls: 'myapp.test'` (or `valetTls`) works with
Bird alone. The plugin reads `Certificates/myapp.test.crt` and `.key` from
Valet's config directory, so Bird keeps such a pair for every site and proxy
in `~/.bird/valet/Certificates` and links `~/.config/valet` there. The `.crt`
holds the leaf and Bird's CA, is valid for `myapp.test` and `*.myapp.test`,
and is renewed 30 days before it expires (after 397 days); the `.key` is
readable only by you. The daemon keeps the files in step with your sites
within seconds, and `link`, `unlink`, `park`, `forget`, `proxy`, `unproxy`
and `tld` update them right away. `detectTls: true` (the directory name plus
the TLD) works too: Bird writes a `config.json` with the TLD next to them.

- **Herd wins while its config directory exists.** The plugin uses
  `~/Library/Application Support/Herd/config/valet` whenever that directory
  is there, Herd running or not, and only then looks at `~/.config/valet`.
  Delete (or move away) Herd's directory to switch to Bird's certificates;
  `bird doctor` warns while it exists.
- **A real Valet install is left alone.** If `~/.config/valet` is already a
  directory (or a link somewhere else), Bird doesn't touch it, and the plugin
  keeps using Valet's certificates. Bird's are still in
  `~/.bird/valet/Certificates` if you want to point Vite at them yourself.
- `bird uninstall` removes the `~/.config/valet` link.

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
bird php:limits [--memory MB|-1] [--upload MB] [--php 8.3]   # memory_limit, upload size
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
  PHP, start/stop Bird, switch the global PHP version (⌘1…), opcache, Xdebug,
  check for updates (⌘U, answered in a dialog). When an update is waiting, the
  menu bar icon gets a dot and the menu offers it.
- **Bird window** (⌘O) to manage everything else: sites (PHP version per
  site, HTTPS redirect, open in browser/Finder/Terminal, link and unlink),
  proxies, parked folders, PHP versions (install, update, remove, make
  global) and settings (including `bird doctor` and updates: check, install
  with one click, and an option to check at launch and once a day; it never
  installs on its own).

It reads `bird state` and makes every change through the normal `bird`
commands, so it never edits Bird's files itself.

Releases include Bird.app: unzip it into /Applications, and `bird self-update`
keeps it up to date there. To build it yourself:

```bash
app/build.sh   # builds app/build/Bird.app; move it to /Applications to use it
```

Needs the Xcode command line tools (no Xcode). The app is signed ad hoc, which
is enough on your own Mac; "Open menu bar app at login" is in Settings.

## Files

```
~/.bird/config.toml   global config, sites, proxies (hand-editable; picked up live)
~/.bird/bin/          bird + shims — put this first on PATH
~/.bird/php/8.4/      bin/php, bin/php-fpm, php.ini, conf.d/
~/.bird/ca/           local certificate authority
~/.bird/valet/        Certificates/<site>.test.crt|.key for Vite (~/.config/valet links here)
~/.bird/log/          bird.log, php-8.4-fpm.log (rotated at 10 MB; 5 gzipped generations kept)
```

Set `BIRD_HOME` to use another directory (handy for testing:
`BIRD_HOME=/tmp/bird bird daemon --http-port 8080 --https-port 8443`).

## Not in scope

Databases, mail catchers, dump servers, AI assistants, deployment, telemetry.
Use DBngin/Docker/Homebrew for services and fnm/nvm for Node.

## Updates and migrations

`bird self-update` (or Update Now in the app) installs a new release. When a
version needs to change something in `~/.bird` (config.toml, installed PHP,
anything stored later), it does so with a migration in `src/migrate.rs`,
applied automatically: by the new binary right after `self-update`, when the
daemon starts, and before CLI commands. Applied migrations are recorded in
`~/.bird/migrations.json`, `config.toml` is backed up to `~/.bird/backups/`
first, and an older Bird that finds newer migrations warns instead of
touching the data. `bird migrate --status` shows what ran.

Adding one: append a `Migration` with the next id (`YYYY_MM_DD_NNNNNN_name`)
to `MIGRATIONS`. Make it idempotent, never change a released one, and add a
test.

Releases are signed. The release workflow signs `SHA256SUMS` with Bird's
release key (`ssh-keygen -Y sign -n bird-release`, Ed25519) into
`SHA256SUMS.sig`, and `bird self-update` only installs a release whose
signature matches a public key compiled into the running binary
(`RELEASE_KEYS` in `src/signing.rs`) and whose files match `SHA256SUMS`. The
private key is a secret of the `release` environment, which needs the owner's
approval, so pushing a tag or editing a release can't produce an update Bird
accepts. The release key is

```text
ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAII5iuep2RhMNGoxLgz+sc8qaXQ9as6/ZcDCzVpJPXVyn bird-release
```

and a download can be checked by hand with:

```bash
echo "bird-release ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAII5iuep2RhMNGoxLgz+sc8qaXQ9as6/ZcDCzVpJPXVyn" > allowed_signers
ssh-keygen -Y verify -f allowed_signers -I bird-release -n bird-release -s SHA256SUMS.sig < SHA256SUMS
sha256sum -c --ignore-missing SHA256SUMS   # macOS: shasum -a 256 -c --ignore-missing SHA256SUMS
```

PHP builds are signed the same way, with a separate key. The PHP workflow
signs `manifest.json` (`ssh-keygen -Y sign -n bird-php`) in the `php-release`
environment, which only `main` can use and which needs the owner's approval,
and Bird refuses the default mirror's manifest unless `manifest.json.sig`
matches a key in `PHP_KEYS` (`src/signing.rs`). Every archive is then checked
against the manifest's sha256 sums. See
[php-builds/README.md](php-builds/README.md#signing). The PHP key is

```text
ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOFCBm+uCYPaByT8wvmM5u+Fw9t0f302T2/bJ1FXGsbp bird-php
```

## Development

```bash
cargo test
cargo clippy --all-targets
```
