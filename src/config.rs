use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::paths::paths;

/// Global Bird configuration, stored in `~/.bird/config.toml`.
///
/// The CLI owns this file but it is meant to be human-editable; the daemon
/// picks up changes automatically.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    /// Top-level domain sites are served under (`myapp.test`).
    pub tld: String,
    /// Global default PHP version (`8.4`), used by sites and the CLI unless overridden.
    pub php: Option<String>,
    /// Address the web server listens on.
    pub listen: Ipv4Addr,
    pub http_port: u16,
    pub https_port: u16,
    /// Port of Bird's DNS server; `/etc/resolver/<tld>` points here.
    pub dns_port: u16,
    /// Stop a PHP-FPM pool after this many idle minutes (0 = keep running).
    pub php_idle_minutes: u64,
    /// Opcache for PHP-FPM (`bird opcache on|off`). It revalidates files on
    /// every request, so edits are picked up immediately.
    pub opcache: bool,
    /// When requests run with Xdebug (`bird xdebug auto|on|off`).
    pub xdebug: XdebugMode,
    /// Where PHP builds are downloaded from: Bird's own release (with
    /// manifest.json), or a dl.static-php.dev distribution.
    pub php_mirror: String,
    /// PHP's upload_max_filesize and post_max_size in MB (`bird php:limits`).
    /// Unset: php.ini decides.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub php_upload_mb: Option<u64>,
    /// PHP's memory_limit in MB, -1 for no limit (`bird php:limits`).
    /// Unset: php.ini decides.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub php_memory_mb: Option<i64>,
    /// Parked directories: every subdirectory is served as `<dir>.<tld>`.
    pub paths: Vec<PathBuf>,
    /// memory_limit for single PHP versions, over `php_memory_mb`.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub php_memory_mb_by_version: BTreeMap<String, i64>,
    /// Linked sites and per-site settings for parked sites, keyed by site name.
    pub sites: BTreeMap<String, SiteConfig>,
    /// Reverse proxies, keyed by site name.
    pub proxies: BTreeMap<String, ProxyConfig>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum XdebugMode {
    /// Only requests that carry an Xdebug trigger (the browser extension's
    /// XDEBUG_SESSION cookie, or ?XDEBUG_SESSION=1) go to the Xdebug pool.
    #[default]
    Auto,
    /// Every request runs with Xdebug and starts a debug session.
    On,
    /// Never, triggers are ignored.
    Off,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SiteConfig {
    /// Set for linked sites; parked sites only carry overrides.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// PHP version pinned for this site (`bird isolate`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub php: Option<String>,
    /// Redirect plain HTTP to HTTPS. HTTPS itself always works.
    #[serde(skip_serializing_if = "is_false")]
    pub secure: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProxyConfig {
    pub upstream: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub secure: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl Default for Config {
    fn default() -> Self {
        Self {
            tld: "test".into(),
            php: None,
            listen: Ipv4Addr::LOCALHOST,
            http_port: 80,
            https_port: 443,
            dns_port: 53535,
            php_idle_minutes: 30,
            opcache: true,
            xdebug: XdebugMode::Auto,
            php_mirror: crate::php::DEFAULT_MIRROR.into(),
            php_upload_mb: None,
            php_memory_mb: None,
            php_memory_mb_by_version: BTreeMap::new(),
            paths: Vec::new(),
            sites: BTreeMap::new(),
            proxies: BTreeMap::new(),
        }
    }
}

impl SiteConfig {
    fn is_empty(&self) -> bool {
        self == &SiteConfig::default()
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        Self::load_from(&paths().config())
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        // Changes to stored configs belong in a migration (see migrate.rs),
        // not here.
        match std::fs::read_to_string(path) {
            Ok(text) => {
                toml::from_str(&text).with_context(|| format!("could not parse {}", path.display()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("could not read {}", path.display())),
        }
    }

    pub fn save(&mut self) -> Result<()> {
        self.save_to(&paths().config())
    }

    pub fn save_to(&mut self, path: &Path) -> Result<()> {
        self.sites.retain(|_, s| !s.is_empty());
        std::fs::create_dir_all(path.parent().unwrap())?;
        let body = toml::to_string_pretty(self)?;
        let text =
            format!("# Bird configuration. Managed by `bird`, safe to edit by hand.\n\n{body}");
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn site_mut(&mut self, name: &str) -> &mut SiteConfig {
        self.sites.entry(name.to_string()).or_default()
    }

    /// The memory_limit Bird sets for a PHP version, if any.
    pub fn php_memory_mb_for(&self, minor: &str) -> Option<i64> {
        self.php_memory_mb_by_version
            .get(minor)
            .copied()
            .or(self.php_memory_mb)
    }

    pub fn domain(&self, name: &str) -> String {
        format!("{name}.{}", self.tld)
    }
}
