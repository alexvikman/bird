//! PHP versions: naming, resolution per directory, and installation from a
//! static-php-cli distribution.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;

use crate::config::Config;
use crate::paths::paths;
use crate::sites;
use crate::ui;

/// Normalizes user input to a `major.minor` version: `8.4.3`, `84`, `php8.4` → `8.4`.
pub fn normalize(input: &str) -> Option<String> {
    let s = input
        .trim()
        .trim_start_matches("php")
        .trim_start_matches('@');
    let (major, minor) = if let Some((major, rest)) = s.split_once('.') {
        let minor: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        (major.to_string(), minor)
    } else if s.len() == 2 && s.chars().all(|c| c.is_ascii_digit()) {
        (s[..1].to_string(), s[1..].to_string())
    } else {
        return None;
    };
    let major: u32 = major.parse().ok()?;
    let minor: u32 = minor.parse().ok()?;
    (5..=9).contains(&major).then(|| format!("{major}.{minor}"))
}

pub fn require_normalized(input: &str) -> Result<String> {
    normalize(input).ok_or_else(|| anyhow!("'{input}' is not a PHP version (expected e.g. 8.4)"))
}

fn version_key(v: &str) -> Vec<u32> {
    v.split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

/// Reads a `.php-version` file in `dir` (as used by phpenv, asdf, Forge...).
pub fn read_php_version_file(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join(".php-version")).ok()?;
    normalize(text.lines().next()?)
}

fn find_php_version_file(start: &Path) -> Option<(String, PathBuf)> {
    start
        .ancestors()
        .find_map(|dir| read_php_version_file(dir).map(|v| (v, dir.join(".php-version"))))
}

#[derive(Debug, Clone)]
pub struct Installed {
    /// `8.4`
    pub minor: String,
    /// `8.4.23`
    pub version: String,
}

pub fn installed() -> Vec<Installed> {
    let Ok(entries) = std::fs::read_dir(paths().php_root()) else {
        return Vec::new();
    };
    let mut out: Vec<Installed> = entries
        .flatten()
        .filter_map(|e| {
            let minor = e.file_name().to_string_lossy().to_string();
            normalize(&minor).filter(|n| *n == minor)?;
            if !paths().php_binary(&minor).is_file() {
                return None;
            }
            let version = std::fs::read_to_string(e.path().join("VERSION"))
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|_| minor.clone());
            Some(Installed { minor, version })
        })
        .collect();
    out.sort_by_key(|i| version_key(&i.minor));
    out
}

pub fn is_installed(minor: &str) -> bool {
    paths().php_binary(minor).is_file()
}

/// Which PHP version applies in `dir`, and a human-readable reason.
pub fn resolve_for_dir(cfg: &Config, dir: &Path) -> Option<(String, String)> {
    if let Some(v) = std::env::var("BIRD_PHP").ok().and_then(|v| normalize(&v)) {
        return Some((v, "BIRD_PHP environment variable".into()));
    }
    if let Some(site) = sites::site_for_dir(cfg, dir)
        && let Some((v, source)) = site.php_version(cfg)
    {
        let why = match source {
            sites::PhpSource::Isolated => format!("site {} is isolated", cfg.domain(&site.name)),
            sites::PhpSource::PhpVersionFile => format!("{}/.php-version", site.path.display()),
            sites::PhpSource::Global => "global default".into(),
        };
        return Some((v, why));
    }
    if let Some((v, file)) = find_php_version_file(dir) {
        return Some((v, file.display().to_string()));
    }
    cfg.php.clone().map(|v| (v, "global default".into()))
}

// ---------------------------------------------------------------------------
// Installation

fn platform() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "macos-aarch64",
        ("macos", "x86_64") => "macos-x86_64",
        ("linux", "aarch64") => "linux-aarch64",
        ("linux", _) => "linux-x86_64",
        _ => "unsupported",
    }
}

#[derive(Deserialize)]
struct MirrorEntry {
    name: String,
}

fn http() -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .user_agent(concat!("bird/", env!("CARGO_PKG_VERSION")))
        .build()?)
}

/// Latest available patch release per minor version on the mirror.
pub fn available(cfg: &Config) -> Result<BTreeMap<String, String>> {
    let url = format!("{}/?format=json", cfg.php_mirror.trim_end_matches('/'));
    let entries: Vec<MirrorEntry> = http()?
        .get(&url)
        .send()
        .and_then(|r| r.error_for_status())
        .with_context(|| format!("could not fetch PHP index from {url}"))?
        .json()
        .context("unexpected PHP index format")?;
    let names: std::collections::HashSet<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    let suffix = format!("-fpm-{}.tar.gz", platform());
    let mut latest: BTreeMap<String, String> = BTreeMap::new();
    for name in &names {
        let Some(full) = name
            .strip_prefix("php-")
            .and_then(|n| n.strip_suffix(&suffix))
        else {
            continue;
        };
        if !names.contains(format!("php-{full}-cli-{}.tar.gz", platform()).as_str()) {
            continue;
        }
        let Some(minor) = normalize(full) else {
            continue;
        };
        let newer = latest
            .get(&minor)
            .is_none_or(|cur| version_key(full) > version_key(cur));
        if newer {
            latest.insert(minor, full.to_string());
        }
    }
    Ok(latest)
}

/// Downloads and installs (or updates) PHP `minor`. Returns the full version installed.
pub fn install(cfg: &Config, minor: &str) -> Result<String> {
    let available = available(cfg)?;
    let Some(full) = available.get(minor).cloned() else {
        let list: Vec<_> = available.keys().cloned().collect();
        bail!(
            "PHP {minor} is not available for {}. Available: {}",
            platform(),
            list.join(", ")
        );
    };
    let dir = paths().php_dir(minor);
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin)?;
    std::fs::create_dir_all(paths().php_conf_d(minor))?;

    let base = cfg.php_mirror.trim_end_matches('/');
    for (kind, binary) in [("cli", "php"), ("fpm", "php-fpm")] {
        let url = format!("{base}/php-{full}-{kind}-{}.tar.gz", platform());
        let archive = download(&url, &format!("PHP {full} {kind}"))?;
        extract_binary(&archive, binary, &bin.join(binary))
            .with_context(|| format!("could not extract {binary} from {url}"))?;
    }
    std::fs::write(dir.join("VERSION"), format!("{full}\n"))?;
    let ini = paths().php_ini(minor);
    if !ini.exists() {
        std::fs::write(&ini, default_ini(minor))?;
    }
    Ok(full)
}

pub fn remove(minor: &str) -> Result<()> {
    let dir = paths().php_dir(minor);
    if !dir.exists() {
        bail!("PHP {minor} is not installed");
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

fn download(url: &str, label: &str) -> Result<Vec<u8>> {
    let mut resp = http()?
        .get(url)
        .send()
        .and_then(|r| r.error_for_status())
        .with_context(|| format!("download failed: {url}"))?;
    let total = resp.content_length();
    let mut buf = Vec::with_capacity(total.unwrap_or(0) as usize);
    let mut chunk = vec![0u8; 256 * 1024];
    let mut last_shown = 0usize;
    loop {
        let n = resp.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if ui::is_tty() && buf.len() - last_shown > 1 << 20 {
            last_shown = buf.len();
            let mb = |b: u64| b as f64 / 1_048_576.0;
            match total {
                Some(t) => eprint!(
                    "\r  ↓ {label}  {:.1}/{:.1} MB ",
                    mb(buf.len() as u64),
                    mb(t)
                ),
                None => eprint!("\r  ↓ {label}  {:.1} MB ", mb(buf.len() as u64)),
            }
            std::io::stderr().flush().ok();
        }
    }
    if ui::is_tty() {
        eprint!("\r\x1b[2K");
    }
    eprintln!("  ↓ {label} ({:.1} MB)", buf.len() as f64 / 1_048_576.0);
    Ok(buf)
}

fn extract_binary(archive: &[u8], name: &str, dest: &Path) -> Result<()> {
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    for entry in tar.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_path_buf();
        if path.file_name().and_then(|f| f.to_str()) != Some(name) {
            continue;
        }
        // Write next to the destination and rename, so running processes keep
        // their (old) inode and nobody ever sees a half-written binary.
        let tmp = dest.with_extension("download");
        let mut out = std::fs::File::create(&tmp)?;
        std::io::copy(&mut entry, &mut out)?;
        drop(out);
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
        std::fs::rename(&tmp, dest)?;
        return Ok(());
    }
    bail!("{name} not found in archive")
}

fn system_timezone() -> String {
    std::fs::read_link("/etc/localtime")
        .ok()
        .and_then(|p| {
            let s = p.to_string_lossy().to_string();
            s.split_once("zoneinfo/").map(|(_, tz)| tz.to_string())
        })
        .unwrap_or_else(|| "UTC".into())
}

fn default_ini(minor: &str) -> String {
    format!(
        "; php.ini for PHP {minor}, managed by Bird. Edit freely, then run `bird restart`.\n\
         ; Extra .ini files can go in conf.d/ next to this file.\n\
         \n\
         memory_limit = 512M\n\
         upload_max_filesize = 128M\n\
         post_max_size = 128M\n\
         max_execution_time = 60\n\
         \n\
         error_reporting = E_ALL\n\
         display_errors = On\n\
         display_startup_errors = On\n\
         log_errors = On\n\
         \n\
         date.timezone = {tz}\n\
         \n\
         openssl.cafile = /etc/ssl/cert.pem\n\
         curl.cainfo = /etc/ssl/cert.pem\n",
        tz = system_timezone()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_versions() {
        assert_eq!(normalize("8.4").as_deref(), Some("8.4"));
        assert_eq!(normalize("8.4.23").as_deref(), Some("8.4"));
        assert_eq!(normalize("84").as_deref(), Some("8.4"));
        assert_eq!(normalize("php8.3").as_deref(), Some("8.3"));
        assert_eq!(normalize("php83").as_deref(), Some("8.3"));
        assert_eq!(normalize(" 8.5\n").as_deref(), Some("8.5"));
        assert_eq!(normalize("latest"), None);
        assert_eq!(normalize("1.2"), None);
    }
}
