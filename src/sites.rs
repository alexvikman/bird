//! Mapping between hostnames, site names and project directories.

use std::path::{Path, PathBuf};

use crate::config::{Config, ProxyConfig};
use crate::php;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SiteKind {
    Linked,
    Parked,
}

#[derive(Debug, Clone)]
pub struct Site {
    pub name: String,
    pub path: PathBuf,
    pub kind: SiteKind,
    /// PHP version pinned in the Bird config.
    pub isolated_php: Option<String>,
    pub secure: bool,
}

#[derive(Debug, Clone)]
pub enum Target {
    Site(Site),
    Proxy(ProxyConfig),
}

/// Where the PHP version of a site comes from, in order of precedence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PhpSource {
    Isolated,
    PhpVersionFile,
    Global,
}

impl Site {
    /// Web root: `public/` for Laravel/Symfony-style apps, otherwise the project root.
    pub fn docroot(&self) -> PathBuf {
        for candidate in ["public", "public_html"] {
            let dir = self.path.join(candidate);
            if dir.is_dir() {
                return dir;
            }
        }
        let web = self.path.join("web");
        if web.join("index.php").is_file() {
            return web;
        }
        self.path.clone()
    }

    pub fn php_version(&self, cfg: &Config) -> Option<(String, PhpSource)> {
        if let Some(v) = &self.isolated_php {
            return Some((v.clone(), PhpSource::Isolated));
        }
        if let Some(v) = php::read_php_version_file(&self.path) {
            return Some((v, PhpSource::PhpVersionFile));
        }
        cfg.php.clone().map(|v| (v, PhpSource::Global))
    }
}

/// Normalizes a directory name into a DNS label: `My_App` → `my-app`.
pub fn slug(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() || c == '.' {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches(|c| c == '-' || c == '.').to_string()
}

fn make_site(cfg: &Config, name: &str, path: PathBuf, kind: SiteKind) -> Site {
    let settings = cfg.sites.get(name);
    Site {
        name: name.to_string(),
        path,
        kind,
        isolated_php: settings.and_then(|s| s.php.clone()),
        secure: settings.is_some_and(|s| s.secure),
    }
}

/// Looks up a site by name: linked sites first, then parked directories.
pub fn find_site(cfg: &Config, name: &str) -> Option<Site> {
    if let Some(path) = cfg.sites.get(name).and_then(|s| s.path.clone()) {
        return Some(make_site(cfg, name, path, SiteKind::Linked));
    }
    for parked in &cfg.paths {
        let direct = parked.join(name);
        if direct.is_dir() {
            return Some(make_site(cfg, name, direct, SiteKind::Parked));
        }
        // Directory names are not always valid hostnames; match on the slug.
        let Ok(entries) = std::fs::read_dir(parked) else {
            continue;
        };
        for entry in entries.flatten() {
            let fname = entry.file_name().to_string_lossy().to_string();
            if !fname.starts_with('.') && slug(&fname) == name && entry.path().is_dir() {
                return Some(make_site(cfg, name, entry.path(), SiteKind::Parked));
            }
        }
    }
    None
}

/// Every site Bird serves; linked sites win over parked ones with the same name.
pub fn all_sites(cfg: &Config) -> Vec<Site> {
    let mut sites: Vec<Site> = cfg
        .sites
        .iter()
        .filter_map(|(name, s)| {
            s.path
                .clone()
                .map(|p| make_site(cfg, name, p, SiteKind::Linked))
        })
        .collect();
    for parked in &cfg.paths {
        let Ok(entries) = std::fs::read_dir(parked) else {
            continue;
        };
        let mut found: Vec<_> = entries
            .flatten()
            .filter(|e| e.path().is_dir())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| !n.starts_with('.'))
            .collect();
        found.sort();
        for dir in found {
            let name = slug(&dir);
            if name.is_empty() || sites.iter().any(|s| s.name == name) {
                continue;
            }
            sites.push(make_site(cfg, &name, parked.join(&dir), SiteKind::Parked));
        }
    }
    sites.sort_by(|a, b| a.name.cmp(&b.name));
    sites
}

/// Resolves a `Host` header (`www.myapp.test:443`) to what should serve it.
/// Subdomains fall back to their parent site: `api.myapp.test` → `myapp`.
pub fn resolve_host(cfg: &Config, host: &str) -> Option<Target> {
    let host = host.split(':').next().unwrap_or(host).to_ascii_lowercase();
    let host = host.trim_end_matches('.');
    let name = host.strip_suffix(&format!(".{}", cfg.tld))?;
    for candidate in host_candidates(name) {
        if let Some(proxy) = cfg.proxies.get(candidate) {
            return Some(Target::Proxy(proxy.clone()));
        }
        if let Some(site) = find_site(cfg, candidate) {
            return Some(Target::Site(site));
        }
    }
    None
}

/// `a.b.c` → `["a.b.c", "b.c", "c"]`
fn host_candidates(name: &str) -> impl Iterator<Item = &str> {
    let mut rest = Some(name);
    std::iter::from_fn(move || {
        let current = rest?;
        rest = current.split_once('.').map(|(_, r)| r);
        Some(current)
    })
    .filter(|s| !s.is_empty())
}

/// The site whose project directory contains `dir` (deepest match wins).
pub fn site_for_dir(cfg: &Config, dir: &Path) -> Option<Site> {
    let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    all_sites(cfg)
        .into_iter()
        .filter_map(|site| {
            let root = site
                .path
                .canonicalize()
                .unwrap_or_else(|_| site.path.clone());
            dir.starts_with(&root)
                .then(|| (root.components().count(), site))
        })
        .max_by_key(|(depth, _)| *depth)
        .map(|(_, site)| site)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert_eq!(slug("My_App"), "my-app");
        assert_eq!(slug("my-web"), "my-web");
        assert_eq!(slug("foo bar  baz"), "foo-bar-baz");
        assert_eq!(slug("-x-"), "x");
    }

    #[test]
    fn candidates() {
        let c: Vec<_> = host_candidates("www.api.app").collect();
        assert_eq!(c, ["www.api.app", "api.app", "app"]);
    }

    #[test]
    fn resolves_links_proxies_and_subdomains() {
        let tmp = std::env::temp_dir().join(format!("bird-sites-{}", std::process::id()));
        std::fs::create_dir_all(tmp.join("parked/Cool_Site/public")).unwrap();
        let mut cfg = Config::default();
        cfg.paths.push(tmp.join("parked"));
        cfg.site_mut("linked").path = Some(tmp.clone());
        cfg.proxies.insert(
            "vite".into(),
            ProxyConfig {
                upstream: "http://127.0.0.1:5173".into(),
                secure: false,
            },
        );

        match resolve_host(&cfg, "www.cool-site.test:443") {
            Some(Target::Site(s)) => {
                assert_eq!(s.name, "cool-site");
                assert_eq!(s.kind, SiteKind::Parked);
                assert!(s.docroot().ends_with("public"));
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(
            matches!(resolve_host(&cfg, "LINKED.test"), Some(Target::Site(s)) if s.kind == SiteKind::Linked)
        );
        assert!(matches!(
            resolve_host(&cfg, "vite.test"),
            Some(Target::Proxy(_))
        ));
        assert!(resolve_host(&cfg, "nope.test").is_none());
        assert!(resolve_host(&cfg, "linked.example.com").is_none());
        std::fs::remove_dir_all(tmp).ok();
    }
}
