//! Certificate files in Laravel Valet's layout, for laravel-vite-plugin.
//!
//! With `detectTls: 'app.test'` (or `valetTls`) the plugin serves Vite over
//! HTTPS with `Certificates/app.test.crt` and `.key` from Herd's config
//! directory if that exists, otherwise from Valet's (`~/.config/valet`).
//! Bird mints its own certificates in memory, so for such tools it also keeps
//! a pair per site and proxy in `~/.bird/valet/Certificates`, signed by
//! Bird's CA, and points `~/.config/valet` there. A real Valet install in
//! `~/.config/valet` is left alone: Valet treats every certificate in its
//! Certificates directory as a site it secured.

use std::collections::BTreeSet;
use std::io::{ErrorKind, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rcgen::PublicKeyData;
use time::{Duration, OffsetDateTime};
use x509_parser::certificate::X509Certificate;

use crate::config::Config;
use crate::paths::{paths, user_home};
use crate::sites;
use crate::tls::{self, Ca};

/// Certificates are reissued this long before they expire (they are valid
/// for 397 days, see `Ca::sign_leaf`).
const RENEW_BEFORE: Duration = Duration::days(30);

#[derive(Debug, Default, PartialEq)]
pub struct Synced {
    /// Domains whose certificate was (re)written.
    pub written: Vec<String>,
    /// Domains whose files were removed.
    pub removed: Vec<String>,
    pub link: Link,
}

/// What `~/.config/valet` is.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Link {
    /// A symlink to `~/.bird/valet`.
    Bird,
    #[default]
    Missing,
    /// Anything else, such as a real Valet install.
    Other,
}

/// Where laravel-vite-plugin looks for Valet's config directory.
pub fn valet_link() -> PathBuf {
    user_home().join(".config/valet")
}

/// Herd's config directory, which laravel-vite-plugin prefers while it exists.
pub fn herd_config_dir() -> PathBuf {
    user_home().join("Library/Application Support/Herd/config/valet")
}

/// Every domain Bird serves: sites (linked and parked) and proxies.
pub fn domains(cfg: &Config) -> BTreeSet<String> {
    sites::all_sites(cfg)
        .iter()
        .map(|s| s.name.as_str())
        .chain(cfg.proxies.keys().map(String::as_str))
        // config.toml is hand-editable; a name must not become a path.
        .filter(|name| sites::is_site_name(name))
        .map(|name| cfg.domain(name))
        .collect()
}

/// Brings `~/.bird/valet` in line with `cfg` and links `~/.config/valet` to
/// it if nothing is there. Does nothing before there is a CA.
pub fn sync(cfg: &Config) -> Result<Synced> {
    let ca_pem = match std::fs::read_to_string(paths().ca_cert()) {
        Ok(pem) => pem,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Synced::default()),
        Err(e) => return Err(e).context("could not read the CA certificate"),
    };
    let dir = paths().valet_dir();
    let mut synced = sync_dir(
        &dir,
        &cfg.tld,
        &domains(cfg),
        &ca_pem,
        Ca::load,
        OffsetDateTime::now_utc(),
    )?;
    synced.link = ensure_link(&valet_link(), &dir)?;
    Ok(synced)
}

pub fn link_state() -> Link {
    link_state_of(&valet_link(), &paths().valet_dir())
}

/// Removes `~/.config/valet` if it is Bird's link (`bird uninstall`).
pub fn remove_link() -> Result<bool> {
    if link_state() != Link::Bird {
        return Ok(false);
    }
    std::fs::remove_file(valet_link())?;
    Ok(true)
}

fn link_state_of(link: &Path, target: &Path) -> Link {
    match std::fs::read_link(link) {
        Ok(to) if to == target => Link::Bird,
        _ if std::fs::symlink_metadata(link).is_err() => Link::Missing,
        _ => Link::Other,
    }
}

fn ensure_link(link: &Path, target: &Path) -> Result<Link> {
    let state = link_state_of(link, target);
    if state != Link::Missing {
        return Ok(state);
    }
    if let Some(parent) = link.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::os::unix::fs::symlink(target, link)
        .with_context(|| format!("could not link {} to {}", link.display(), target.display()))?;
    Ok(Link::Bird)
}

/// Writes a certificate for every domain in `domains` that lacks a current
/// one into `dir/Certificates`, and removes the files of other domains.
/// `load_ca` is only called when something needs signing.
fn sync_dir(
    dir: &Path,
    tld: &str,
    domains: &BTreeSet<String>,
    ca_pem: &str,
    load_ca: impl FnOnce() -> Result<Ca>,
    now: OffsetDateTime,
) -> Result<Synced> {
    let certs = dir.join("Certificates");
    private_dir(dir)?;
    private_dir(&certs)?;
    // The daemon and CLI commands both sync; one at a time keeps every key
    // with its certificate.
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(dir.join(".lock"))?;
    lock.lock()?;

    // `detectTls: true` builds the host from the directory name and this TLD.
    let config = format!("{{\"tld\": {}}}\n", serde_json::to_string(tld)?);
    if std::fs::read_to_string(dir.join("config.json")).ok() != Some(config.clone()) {
        write_file(&dir.join("config.json"), &config, 0o644)?;
    }

    let ca_der = tls::parse_pem(ca_pem).context("invalid CA certificate")?;
    let (_, ca_cert) =
        x509_parser::parse_x509_certificate(&ca_der).context("invalid CA certificate")?;
    let mut load_ca = Some(load_ca);
    let mut ca = None;
    let mut synced = Synced::default();
    for domain in domains {
        let crt = certs.join(format!("{domain}.crt"));
        let key = certs.join(format!("{domain}.key"));
        if is_current(&crt, &key, domain, &ca_cert, now) {
            continue;
        }
        if ca.is_none() {
            ca = Some(load_ca.take().expect("loaded once")()?);
        }
        let (chain, key_pem) = ca.as_ref().expect("just loaded").issue_pem(domain)?;
        write_file(&key, &key_pem, 0o600)?;
        write_file(&crt, &chain, 0o644)?;
        synced.written.push(domain.clone());
    }

    for entry in std::fs::read_dir(&certs)?.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(domain) = name
            .strip_suffix(".crt")
            .or_else(|| name.strip_suffix(".key"))
        else {
            continue;
        };
        if !domains.contains(domain) {
            std::fs::remove_file(entry.path())?;
            if !synced.removed.iter().any(|d| d == domain) {
                synced.removed.push(domain.to_string());
            }
        }
    }
    synced.removed.sort();
    Ok(synced)
}

/// Whether `crt` holds a certificate for `domain` from this CA that is not
/// about to expire, and `key` its private key.
fn is_current(
    crt: &Path,
    key: &Path,
    domain: &str,
    ca: &X509Certificate,
    now: OffsetDateTime,
) -> bool {
    let (Ok(crt_pem), Ok(key_pem)) = (std::fs::read_to_string(crt), std::fs::read_to_string(key))
    else {
        return false;
    };
    let Some(der) = tls::parse_pem(&crt_pem) else {
        return false;
    };
    let Ok((_, cert)) = x509_parser::parse_x509_certificate(&der) else {
        return false;
    };
    let Ok(key) = rcgen::KeyPair::from_pem(&key_pem) else {
        return false;
    };
    let validity = cert.validity();
    cert.verify_signature(Some(ca.public_key())).is_ok()
        && validity.not_before.timestamp() <= now.unix_timestamp()
        && validity.not_after.timestamp() > (now + RENEW_BEFORE).unix_timestamp()
        && tls::common_name(&crt_pem).as_deref() == Some(domain)
        && cert.public_key().raw == key.subject_public_key_info().as_slice()
}

fn private_dir(dir: &Path) -> Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("could not create {}", dir.display()))?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

/// Writes `path` with `mode` from the start (a key is never readable by
/// others, not even briefly), then moves it into place.
fn write_file(path: &Path, contents: &str, mode: u32) -> Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let _ = std::fs::remove_file(&tmp);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&tmp)
        .with_context(|| format!("could not write {}", tmp.display()))?;
    file.write_all(contents.as_bytes())?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("bird-certs-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn ca() -> (String, String) {
        tls::generate_ca("test").unwrap()
    }

    fn set(domains: &[&str]) -> BTreeSet<String> {
        domains.iter().map(|d| d.to_string()).collect()
    }

    fn sync(
        dir: &Path,
        domains: &[&str],
        (cert, key): &(String, String),
        now: OffsetDateTime,
    ) -> Synced {
        sync_dir(
            dir,
            "test",
            &set(domains),
            cert,
            || Ca::from_pem(cert, key),
            now,
        )
        .unwrap()
    }

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn writes_valet_style_files_once() {
        let tmp = TempDir::new("write");
        let dir = tmp.0.join("valet");
        let ca = ca();
        let now = OffsetDateTime::now_utc();

        let first = sync(&dir, &["app.test", "vite.test"], &ca, now);
        assert_eq!(first.written, ["app.test", "vite.test"]);
        let certs = dir.join("Certificates");
        let crt = std::fs::read_to_string(certs.join("app.test.crt")).unwrap();
        assert_eq!(crt.matches("BEGIN CERTIFICATE").count(), 2, "leaf and CA");
        assert!(crt.ends_with(ca.0.trim_start()));
        assert_eq!(tls::common_name(&crt).as_deref(), Some("app.test"));
        assert_eq!(
            std::fs::read_to_string(dir.join("config.json")).unwrap(),
            "{\"tld\": \"test\"}\n"
        );
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&certs), 0o700);
        assert_eq!(mode(&certs.join("app.test.key")), 0o600);

        // Nothing to do the second time; the CA is not even loaded.
        let again = sync_dir(
            &dir,
            "test",
            &set(&["app.test", "vite.test"]),
            &ca.0,
            || panic!("nothing needs signing"),
            now,
        )
        .unwrap();
        assert_eq!(again, Synced::default());
        assert_eq!(
            std::fs::read_to_string(certs.join("app.test.crt")).unwrap(),
            crt
        );
    }

    #[test]
    fn removes_domains_that_are_gone_and_leaves_other_files() {
        let tmp = TempDir::new("remove");
        let dir = tmp.0.join("valet");
        let ca = ca();
        let now = OffsetDateTime::now_utc();
        sync(&dir, &["app.test", "old.test"], &ca, now);
        std::fs::write(dir.join("Certificates/notes.txt"), "mine").unwrap();

        let synced = sync(&dir, &["app.test"], &ca, now);
        assert!(synced.written.is_empty());
        assert_eq!(synced.removed, ["old.test"]);
        let certs = dir.join("Certificates");
        assert!(!certs.join("old.test.crt").exists());
        assert!(!certs.join("old.test.key").exists());
        assert!(certs.join("app.test.crt").exists());
        assert!(certs.join("notes.txt").exists());
    }

    #[test]
    fn reissues_expiring_foreign_and_broken_certificates() {
        let tmp = TempDir::new("renew");
        let dir = tmp.0.join("valet");
        let certs = dir.join("Certificates");
        let ca = ca();
        let now = OffsetDateTime::now_utc();
        sync(&dir, &["a.test", "b.test", "c.test", "d.test"], &ca, now);

        // c: its key was replaced (a sync interrupted between the two files).
        let other_key = std::fs::read_to_string(certs.join("d.test.key")).unwrap();
        std::fs::write(certs.join("c.test.key"), other_key).unwrap();
        // d: its certificate is gone.
        std::fs::remove_file(certs.join("d.test.crt")).unwrap();
        let synced = sync(&dir, &["a.test", "b.test", "c.test", "d.test"], &ca, now);
        assert_eq!(synced.written, ["c.test", "d.test"]);

        // A year from now every certificate is within 30 days of expiring.
        let later = now + Duration::days(370);
        let synced = sync(&dir, &["a.test", "b.test", "c.test", "d.test"], &ca, later);
        assert_eq!(synced.written.len(), 4);

        // A new CA (`bird tld`, or a reinstall) means new certificates.
        let synced = sync(
            &dir,
            &["a.test", "b.test", "c.test", "d.test"],
            &self::ca(),
            now,
        );
        assert_eq!(synced.written.len(), 4);
    }

    #[test]
    fn config_json_follows_the_tld() {
        let tmp = TempDir::new("tld");
        let dir = tmp.0.join("valet");
        let (cert, key) = tls::generate_ca("dev").unwrap();
        let now = OffsetDateTime::now_utc();
        sync_dir(
            &dir,
            "dev",
            &set(&["app.dev"]),
            &cert,
            || Ca::from_pem(&cert, &key),
            now,
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("config.json")).unwrap(),
            "{\"tld\": \"dev\"}\n"
        );
    }

    #[test]
    fn links_only_where_nothing_is() {
        let tmp = TempDir::new("link");
        let target = tmp.0.join("bird/valet");
        std::fs::create_dir_all(&target).unwrap();

        let link = tmp.0.join("home/.config/valet");
        assert_eq!(link_state_of(&link, &target), Link::Missing);
        assert_eq!(ensure_link(&link, &target).unwrap(), Link::Bird);
        assert_eq!(std::fs::read_link(&link).unwrap(), target);
        assert_eq!(ensure_link(&link, &target).unwrap(), Link::Bird);

        // A real Valet install, or a link somewhere else, is not touched.
        let valet = tmp.0.join("valet-home/.config/valet");
        std::fs::create_dir_all(valet.join("Certificates")).unwrap();
        assert_eq!(ensure_link(&valet, &target).unwrap(), Link::Other);
        assert!(valet.join("Certificates").is_dir());
        let elsewhere = tmp.0.join("elsewhere");
        std::os::unix::fs::symlink(tmp.0.join("nowhere"), &elsewhere).unwrap();
        assert_eq!(ensure_link(&elsewhere, &target).unwrap(), Link::Other);
    }

    #[test]
    fn domains_cover_sites_and_proxies_but_no_paths() {
        let tmp = TempDir::new("domains");
        std::fs::create_dir_all(tmp.0.join("parked/My_App")).unwrap();
        let mut cfg = Config::default();
        cfg.paths.push(tmp.0.join("parked"));
        cfg.site_mut("linked").path = Some(tmp.0.clone());
        cfg.site_mut("../evil").path = Some(tmp.0.clone());
        cfg.proxies.insert(
            "vite".into(),
            crate::config::ProxyConfig {
                upstream: "http://127.0.0.1:5173".into(),
                secure: false,
            },
        );
        assert_eq!(
            domains(&cfg),
            set(&["linked.test", "my-app.test", "vite.test"])
        );
    }
}
