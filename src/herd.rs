//! `bird import herd`: take over parked paths, linked sites, PHP pins and
//! HTTPS settings from a Laravel Herd (or Valet) installation.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::config::Config;
use crate::paths::user_home;
use crate::ui::{self, dim};
use crate::{php, shim, sites};

#[derive(Deserialize)]
struct ValetConfig {
    #[serde(default)]
    paths: Vec<PathBuf>,
    #[serde(default)]
    tld: Option<String>,
}

#[derive(Debug, Default, PartialEq)]
pub struct NginxSite {
    pub php: Option<String>,
    pub secure: bool,
}

/// Reads what Herd stores in a generated nginx site file.
pub fn parse_nginx_site(text: &str) -> NginxSite {
    let php = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("# ISOLATED_PHP_VERSION="))
        .and_then(php::normalize);
    let secure = text.lines().any(|l| {
        let l = l.trim();
        l.starts_with("listen") && l.contains("443") && l.contains("ssl")
    });
    NginxSite { php, secure }
}

pub fn import(install_php: bool) -> Result<()> {
    let herd = user_home().join("Library/Application Support/Herd");
    let valet = herd.join("config/valet");
    if !valet.is_dir() {
        bail!("no Herd installation found at {}", herd.display());
    }
    let vc: ValetConfig =
        serde_json::from_str(&std::fs::read_to_string(valet.join("config.json"))?)
            .context("could not read Herd's config.json")?;
    let herd_tld = vc.tld.unwrap_or_else(|| "test".into());
    let mut cfg = Config::load()?;
    if herd_tld != cfg.tld {
        ui::warn(format!(
            "Herd uses .{herd_tld}, Bird uses .{}; change with `bird tld {herd_tld}`",
            cfg.tld
        ));
    }

    let mut added_paths = 0;
    let sites_dir = valet.join("Sites");
    for p in &vc.paths {
        let p = p.canonicalize().unwrap_or_else(|_| p.clone());
        // Herd lists its own link directory as a "path"; links are imported below.
        if p.starts_with(&herd) || !p.is_dir() || cfg.paths.contains(&p) {
            continue;
        }
        cfg.paths.push(p.clone());
        added_paths += 1;
        ui::ok(format!("parked {}", p.display()));
    }

    let mut added_links = 0;
    if let Ok(entries) = std::fs::read_dir(&sites_dir) {
        let mut links: Vec<_> = entries.flatten().collect();
        links.sort_by_key(|e| e.file_name());
        for e in links {
            let Ok(target) = std::fs::read_link(e.path()) else {
                continue;
            };
            let name = sites::slug(&e.file_name().to_string_lossy());
            if !target.is_dir() {
                ui::warn(format!(
                    "skipping {name}: {} no longer exists",
                    target.display()
                ));
                continue;
            }
            let site = cfg.site_mut(&name);
            if site.path.is_none() {
                site.path = Some(target.clone());
                added_links += 1;
                ui::ok(format!("linked {name}.{} → {}", cfg.tld, target.display()));
            }
        }
    }

    let mut pins = 0;
    let mut needed: BTreeSet<String> = BTreeSet::new();
    if let Ok(entries) = std::fs::read_dir(valet.join("Nginx")) {
        for e in entries.flatten() {
            let file = e.file_name().to_string_lossy().to_string();
            let Some(name) = file.strip_suffix(&format!(".{herd_tld}")) else {
                continue;
            };
            let name = sites::slug(name);
            let Ok(text) = std::fs::read_to_string(e.path()) else {
                continue;
            };
            let parsed = parse_nginx_site(&text);
            if sites::find_site(&cfg, &name).is_none() {
                continue; // site no longer exists
            }
            let site = cfg.site_mut(&name);
            if let Some(v) = parsed.php {
                if site.php.is_none() {
                    site.php = Some(v.clone());
                    pins += 1;
                }
                needed.insert(v);
            }
            site.secure |= parsed.secure;
        }
    }

    if cfg.php.is_none() {
        if let Some(v) = herd_global_php(&herd) {
            cfg.php = Some(v.clone());
            needed.insert(v);
        }
    } else if let Some(v) = &cfg.php {
        needed.insert(v.clone());
    }
    cfg.save()?;
    println!(
        "{}",
        dim(&format!(
            "imported {added_paths} parked path(s), {added_links} link(s), {pins} PHP pin(s)"
        ))
    );

    let missing: Vec<String> = needed
        .into_iter()
        .filter(|v| !php::is_installed(v))
        .collect();
    if !missing.is_empty() {
        if install_php {
            for v in &missing {
                let full = php::install(&cfg, v)?;
                ui::ok(format!("installed PHP {full}"));
            }
            shim::refresh()?;
        } else {
            ui::warn(format!(
                "imported sites need PHP {} — run `bird php:install {}`",
                missing.join(", "),
                missing.join(" ")
            ));
        }
    }
    Ok(())
}

/// Herd's global version is the target of its `herd.sock` symlink (`herd85.sock`).
fn herd_global_php(herd: &Path) -> Option<String> {
    let target = std::fs::read_link(herd.join("herd.sock")).ok()?;
    let name = target.file_name()?.to_str()?;
    php::normalize(name.strip_prefix("herd")?.strip_suffix(".sock")?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_isolated_secure_site() {
        let text = "# ISOLATED_PHP_VERSION=8.3\nserver {\n    listen 127.0.0.1:80;\n}\nserver {\n    listen 127.0.0.1:443 ssl;\n}\n";
        assert_eq!(
            parse_nginx_site(text),
            NginxSite {
                php: Some("8.3".into()),
                secure: true
            }
        );
        assert_eq!(
            parse_nginx_site("server { listen 127.0.0.1:80; }"),
            NginxSite::default()
        );
    }
}
