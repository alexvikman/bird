# Security

## Reporting a vulnerability

Please report security problems privately, not in a public issue:
[report a vulnerability](https://github.com/alexvikman/bird/security/advisories/new)
(the repository's Security tab → "Report a vulnerability"). Only the
maintainer can read the report.

Say what you found, which Bird version (`bird --version`) and platform it is
about, and how to reproduce it if you can. Bird has a single maintainer;
reports are handled on a best-effort basis, and fixes ship as a normal
release.

## Supported versions

Only the latest release gets fixes. `bird self-update` installs it.

## What Bird protects

- **Only this machine can reach it.** The web server and the DNS server
  listen on `127.0.0.1` (unless `listen` in `~/.bird/config.toml` says
  otherwise). Requests for a host outside the configured TLD are refused, so
  a page on another domain that resolves to `127.0.0.1` gets nothing.
- **The local CA can't impersonate real sites.** It is name-constrained to
  the TLD and to no IP addresses, and its key is readable only by you.
- **Nothing privileged keeps running.** The daemon and PHP run as you.
  `bird install` uses `sudo` once, for the DNS resolver entry, the service
  and trusting the CA, and checks what it takes from `~/.bird` before acting
  on it as root.
- **Downloads are verified.** Bird releases and Bird's PHP builds are only
  installed with a signature from a key compiled into Bird, and Composer
  only with a signature from Composer's release key. See the README.

## What it does not protect against

- **Everything in a parked folder is a live site.** Any web page you visit
  can send a request to `http://<folder>.test`, and that runs the project's
  PHP code as you. Don't keep code you don't trust in a parked folder.
- **PHP is not sandboxed.** A site's code runs with all the rights of your
  account.
- **Your own account.** A program running as you can change `~/.bird` (the
  config, the binaries) and anything else of yours. Bird does not defend
  against it.
- **Other accounts on the same machine** can reach your sites on
  `127.0.0.1`, like any local server.
