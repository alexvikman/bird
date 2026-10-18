//! PHP versions: naming, resolution per directory, and installation.
//!
//! PHP comes from Bird's own builds (php-builds/, published as the `php`
//! GitHub release with a signed `manifest.json` carrying sha256 sums). A
//! dl.static-php.dev distribution still works as `php_mirror`, without
//! checksums and with fewer extensions.

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
    /// Where the build came from: `bird` or `static-php`.
    pub source: String,
    /// Which build of `version` (the sha256 of its cli archive), for Bird's
    /// builds; a rebuild with more extensions keeps the PHP version.
    pub build: Option<String>,
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
            let read = |file: &str| {
                std::fs::read_to_string(e.path().join(file))
                    .ok()
                    .map(|s| s.trim().to_string())
            };
            let version = read("VERSION").unwrap_or_else(|| minor.clone());
            // Installs from before Bird had its own builds have no SOURCE file.
            let source = read("SOURCE").unwrap_or_else(|| SOURCE_STATIC_PHP.into());
            let build = read("BUILD");
            Some(Installed {
                minor,
                version,
                source,
                build,
            })
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

pub const DEFAULT_MIRROR: &str = "https://github.com/alexvikman/bird/releases/download/php";
pub const SOURCE_BIRD: &str = "bird";
pub const SOURCE_STATIC_PHP: &str = "static-php";

enum Mirror<'a> {
    /// Bird's own builds: `<base>/manifest.json` plus the archives next to it.
    Manifest(&'a str),
    /// A dl.static-php.dev distribution (directory listing, no checksums).
    StaticPhp(&'a str),
}

fn mirror(cfg: &Config) -> Mirror<'_> {
    let base = cfg.php_mirror.trim_end_matches('/');
    if base.contains("dl.static-php.dev") {
        Mirror::StaticPhp(base)
    } else {
        Mirror::Manifest(base)
    }
}

/// `bird` or `static-php`, depending on `php_mirror`.
pub fn mirror_source(cfg: &Config) -> &'static str {
    match mirror(cfg) {
        Mirror::Manifest(_) => SOURCE_BIRD,
        Mirror::StaticPhp(_) => SOURCE_STATIC_PHP,
    }
}

pub(crate) fn http() -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .user_agent(concat!("bird/", env!("CARGO_PKG_VERSION")))
        .build()?)
}

const MANIFEST_SCHEMA: u32 = 1;

#[derive(Deserialize)]
struct Manifest {
    schema: u32,
    #[serde(default)]
    latest: BTreeMap<String, BTreeMap<String, String>>,
    #[serde(default)]
    builds: Vec<Build>,
}

#[derive(Deserialize)]
struct Build {
    version: String,
    platform: String,
    cli: Asset,
    fpm: Asset,
    #[serde(default)]
    shared_extensions: BTreeMap<String, Asset>,
}

#[derive(Deserialize)]
struct Asset {
    file: String,
    sha256: String,
    size: u64,
}

fn fetch_manifest(base: &str) -> Result<Manifest> {
    let client = http()?;
    let url = format!("{base}/manifest.json");
    let body = client
        .get(&url)
        .send()
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.bytes())
        .with_context(|| format!("could not fetch {url}"))?;
    // Bird's own builds count only with a signature from a key compiled into
    // Bird: the manifest's sha256 sums then vouch for every archive. A custom
    // mirror is trusted as configured.
    let signature = if base == DEFAULT_MIRROR {
        let sig_url = format!("{url}.sig");
        let sig = client
            .get(&sig_url)
            .send()
            .and_then(|r| r.error_for_status())
            .and_then(|r| r.text())
            .with_context(|| {
                format!("could not fetch {sig_url}; not trusting an unsigned PHP manifest")
            })?;
        Some(sig)
    } else {
        None
    };
    check_manifest(&url, &body, signature.as_deref(), crate::signing::PHP_KEYS)
}

/// Verifies `signature` (when given) over the raw manifest bytes, then parses
/// them. Nothing in the manifest is read before the signature checks out.
fn check_manifest(
    url: &str,
    body: &[u8],
    signature: Option<&str>,
    keys: &[&str],
) -> Result<Manifest> {
    if let Some(signature) = signature {
        crate::signing::verify(body, signature, crate::signing::PHP_NAMESPACE, keys)
            .with_context(|| format!("the signature of {url} is not valid; not trusting it"))?;
    }
    let manifest: Manifest = serde_json::from_slice(body)
        .with_context(|| format!("{url} is not a valid PHP manifest"))?;
    if manifest.schema != MANIFEST_SCHEMA {
        bail!(
            "{url} has manifest schema {}, this bird understands {MANIFEST_SCHEMA}; update bird",
            manifest.schema
        );
    }
    Ok(manifest)
}

#[derive(Debug, Clone)]
pub struct Remote {
    /// `8.4.26`
    pub version: String,
    /// Build id (cli archive sha256), when the mirror has a manifest.
    pub build: Option<String>,
}

/// Latest available patch release per minor version on the mirror.
pub fn available(cfg: &Config) -> Result<BTreeMap<String, String>> {
    Ok(available_builds(cfg)?
        .into_iter()
        .map(|(minor, r)| (minor, r.version))
        .collect())
}

/// Like `available`, with the build id of each latest release.
pub fn available_builds(cfg: &Config) -> Result<BTreeMap<String, Remote>> {
    match mirror(cfg) {
        Mirror::Manifest(base) => {
            let manifest = fetch_manifest(base)?;
            Ok(manifest
                .latest
                .iter()
                .filter_map(|(minor, platforms)| {
                    let version = platforms.get(platform())?.clone();
                    let build = manifest
                        .builds
                        .iter()
                        .find(|b| b.version == version && b.platform == platform())
                        .map(|b| b.cli.sha256.to_ascii_lowercase());
                    Some((minor.clone(), Remote { version, build }))
                })
                .collect())
        }
        Mirror::StaticPhp(base) => Ok(static_php_available(base)?
            .into_iter()
            .map(|(minor, version)| {
                (
                    minor,
                    Remote {
                        version,
                        build: None,
                    },
                )
            })
            .collect()),
    }
}

impl Installed {
    /// Whether `remote` is a different build than this install: another
    /// version, another source, or a rebuild of the same version.
    pub fn outdated(&self, remote: &Remote, mirror_source: &str) -> bool {
        self.version != remote.version
            || self.source != mirror_source
            || (remote.build.is_some() && self.build != remote.build)
    }
}

#[derive(Deserialize)]
struct MirrorEntry {
    name: String,
}

fn static_php_available(base: &str) -> Result<BTreeMap<String, String>> {
    let url = format!("{base}/?format=json");
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

fn not_available(minor: &str, available: impl Iterator<Item = String>) -> anyhow::Error {
    let list: Vec<String> = available.collect();
    anyhow!(
        "PHP {minor} is not available for {}. Available: {}",
        platform(),
        if list.is_empty() {
            "none".into()
        } else {
            list.join(", ")
        }
    )
}

/// Downloads and installs (or updates) PHP `minor`. Returns the full version installed.
pub fn install(cfg: &Config, minor: &str) -> Result<String> {
    let dir = paths().php_dir(minor);
    let fresh = !dir.exists();
    let result = install_into(cfg, minor, &dir);
    if result.is_err() && fresh {
        // Leave no half-installed version behind (an update keeps the old one).
        let _ = std::fs::remove_dir_all(&dir);
    }
    result
}

fn install_into(cfg: &Config, minor: &str, dir: &Path) -> Result<String> {
    let bin = dir.join("bin");
    let (full, source, build_id) = match mirror(cfg) {
        Mirror::Manifest(base) => {
            let manifest = fetch_manifest(base)?;
            let full = manifest
                .latest
                .get(minor)
                .and_then(|p| p.get(platform()))
                .cloned()
                .ok_or_else(|| {
                    let mut minors: Vec<String> = manifest
                        .latest
                        .iter()
                        .filter(|(_, p)| p.contains_key(platform()))
                        .map(|(m, _)| m.clone())
                        .collect();
                    minors.sort_by_key(|v| version_key(v));
                    not_available(minor, minors.into_iter())
                })?;
            let build = manifest
                .builds
                .iter()
                .find(|b| b.version == full && b.platform == platform())
                .with_context(|| format!("manifest lists PHP {full} but has no build for it"))?;
            std::fs::create_dir_all(&bin)?;
            let cli = download_verified(base, &build.cli, &format!("PHP {full} cli"))?;
            extract_binary(&cli, "php", &bin.join("php"))?;
            extract_licenses(&cli, &dir.join("licenses"))?;
            let fpm = download_verified(base, &build.fpm, &format!("PHP {full} fpm"))?;
            extract_binary(&fpm, "php-fpm", &bin.join("php-fpm"))?;
            // Optional extensions (Xdebug) are installed but stay disabled.
            let modules = dir.join("modules");
            for (ext, asset) in &build.shared_extensions {
                std::fs::create_dir_all(&modules)?;
                let archive = download_verified(base, asset, &format!("{ext} for PHP {full}"))?;
                let so = format!("{ext}.so");
                extract_binary(&archive, &so, &modules.join(&so))?;
            }
            (
                full,
                SOURCE_BIRD,
                Some(build.cli.sha256.to_ascii_lowercase()),
            )
        }
        Mirror::StaticPhp(base) => {
            let available = static_php_available(base)?;
            let full = available
                .get(minor)
                .cloned()
                .ok_or_else(|| not_available(minor, available.keys().cloned()))?;
            std::fs::create_dir_all(&bin)?;
            for (kind, binary) in [("cli", "php"), ("fpm", "php-fpm")] {
                let url = format!("{base}/php-{full}-{kind}-{}.tar.gz", platform());
                let archive = download(&url, &format!("PHP {full} {kind}"))?;
                extract_binary(&archive, binary, &bin.join(binary))
                    .with_context(|| format!("could not extract {binary} from {url}"))?;
            }
            (full, SOURCE_STATIC_PHP, None)
        }
    };
    write_managed_ini(minor, cfg.opcache)?;
    if has_xdebug(minor) {
        write_xdebug_ini(minor, cfg.xdebug == crate::config::XdebugMode::On)?;
    }
    std::fs::write(dir.join("VERSION"), format!("{full}\n"))?;
    std::fs::write(dir.join("SOURCE"), format!("{source}\n"))?;
    match build_id {
        Some(id) => std::fs::write(dir.join("BUILD"), format!("{id}\n"))?,
        None => {
            let _ = std::fs::remove_file(dir.join("BUILD"));
        }
    }
    let ini = paths().php_ini(minor);
    if !ini.exists() {
        std::fs::write(&ini, default_ini(minor))?;
    }
    Ok(full)
}

/// Settings Bird owns, rewritten on install/update and by `bird opcache`
/// (php.ini stays the user's). conf.d is parsed after php.ini, so these win
/// over php.ini.
const MANAGED_INI: &str = "00-bird.ini";

pub fn managed_ini(opcache: bool) -> String {
    let enable = u8::from(opcache);
    format!(
        "; Managed by Bird and rewritten on install/update and `bird opcache on|off`.\n\
         ; conf.d is read after php.ini, so override these in a later file,\n\
         ; e.g. conf.d/99-local.ini.\n\
         \n\
         ; Opcache checks every file's modification time on every request\n\
         ; (revalidate_freq = 0), so code changes show up on the next request.\n\
         opcache.enable = {enable}\n\
         opcache.validate_timestamps = 1\n\
         opcache.revalidate_freq = 0\n\
         opcache.memory_consumption = 256\n\
         opcache.max_accelerated_files = 50000\n"
    )
}

pub fn write_managed_ini(minor: &str, opcache: bool) -> Result<()> {
    let dir = paths().php_conf_d(minor);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join(MANAGED_INI), managed_ini(opcache))?;
    Ok(())
}

pub fn xdebug_so(minor: &str) -> PathBuf {
    paths().php_dir(minor).join("modules/xdebug.so")
}

pub fn pcov_so(minor: &str) -> PathBuf {
    paths().php_dir(minor).join("modules/pcov.so")
}

pub fn has_xdebug(minor: &str) -> bool {
    xdebug_so(minor).is_file()
}

/// Ini directory loading Xdebug, read before conf.d (so conf.d can
/// override e.g. `xdebug.client_port`). Only the Xdebug pool and
/// `bird debug` use it.
pub fn xdebug_ini_dir(minor: &str) -> PathBuf {
    paths().php_dir(minor).join("xdebug.d")
}

pub fn xdebug_ini(minor: &str, always: bool) -> String {
    // On every request (`on`), profiling and tracing would write a file per
    // request; with triggers (`auto`) they run only when asked for.
    let (mode, start) = if always {
        ("debug,develop", "yes")
    } else {
        ("debug,develop,profile,trace", "trigger")
    };
    format!(
        "; Managed by Bird (`bird xdebug auto|on|off`). Loaded only by the Xdebug\n\
         ; pool and `bird debug`; override settings in conf.d/99-local.ini.\n\
         zend_extension = {so}\n\
         xdebug.mode = {mode}\n\
         xdebug.start_with_request = {start}\n\
         xdebug.client_host = 127.0.0.1\n\
         xdebug.client_port = 9003\n\
         xdebug.output_dir = {out}\n",
        so = xdebug_so(minor).display(),
        out = paths().log().join("xdebug").display(),
    )
}

pub fn write_xdebug_ini(minor: &str, always: bool) -> Result<()> {
    let dir = xdebug_ini_dir(minor);
    std::fs::create_dir_all(&dir)?;
    std::fs::create_dir_all(paths().log().join("xdebug"))?;
    std::fs::write(dir.join("xdebug.ini"), xdebug_ini(minor, always))?;
    Ok(())
}

/// PHP_INI_SCAN_DIR for running with Xdebug: xdebug.d, then conf.d.
pub fn xdebug_scan_dirs(minor: &str) -> Result<std::ffi::OsString> {
    if !has_xdebug(minor) {
        bail!(
            "PHP {minor} has no Xdebug; `bird php:update {minor}` installs Bird's build, which has it"
        );
    }
    if !xdebug_ini_dir(minor).join("xdebug.ini").is_file() {
        write_xdebug_ini(minor, false)?;
    }
    let dirs = [xdebug_ini_dir(minor), paths().php_conf_d(minor)];
    Ok(std::env::join_paths(dirs)?)
}

/// Whether this PHP build has opcache compiled in (static-php's "common"
/// builds from Bird 0.1 do not).
pub fn has_opcache(minor: &str) -> bool {
    std::process::Command::new(paths().php_binary(minor))
        .args(["-n", "-m"])
        .output()
        .is_ok_and(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .any(|l| l == "Zend OPcache")
        })
}

/// Downloads a manifest asset and checks its size and sha256.
fn download_verified(base: &str, asset: &Asset, label: &str) -> Result<Vec<u8>> {
    let valid_name = !asset.file.is_empty()
        && asset
            .file
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
        && !asset.file.starts_with('.');
    if !valid_name {
        bail!(
            "refusing suspicious file name in manifest: {:?}",
            asset.file
        );
    }
    let url = format!("{base}/{}", asset.file);
    let bytes = download(&url, label)?;
    if bytes.len() as u64 != asset.size {
        bail!(
            "{} is {} bytes, the manifest says {}",
            asset.file,
            bytes.len(),
            asset.size
        );
    }
    let digest = ring::digest::digest(&ring::digest::SHA256, &bytes);
    let actual: String = digest.as_ref().iter().map(|b| format!("{b:02x}")).collect();
    if !actual.eq_ignore_ascii_case(asset.sha256.trim()) {
        bail!(
            "{} failed verification: sha256 {actual}, the manifest says {}",
            asset.file,
            asset.sha256
        );
    }
    Ok(bytes)
}

/// Copies `licenses/*` from an archive into `dest` (file names only, no paths).
fn extract_licenses(archive: &[u8], dest: &Path) -> Result<()> {
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    let mut created = false;
    for entry in tar.entries()? {
        let mut entry = entry?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry.path()?.to_path_buf();
        let path = path.strip_prefix("./").unwrap_or(&path);
        let mut parts = path.components();
        let (Some(first), Some(std::path::Component::Normal(name)), None) =
            (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        if first.as_os_str() != "licenses" {
            continue;
        }
        if !created {
            let _ = std::fs::remove_dir_all(dest);
            std::fs::create_dir_all(dest)?;
            created = true;
        }
        let mut out = std::fs::File::create(dest.join(name))?;
        std::io::copy(&mut entry, &mut out)?;
    }
    Ok(())
}

pub fn remove(minor: &str) -> Result<()> {
    let dir = paths().php_dir(minor);
    if !dir.exists() {
        bail!("PHP {minor} is not installed");
    }
    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

pub(crate) fn download(url: &str, label: &str) -> Result<Vec<u8>> {
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

pub(crate) fn extract_binary(archive: &[u8], name: &str, dest: &Path) -> Result<()> {
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    for entry in tar.entries()? {
        let mut entry = entry?;
        // Only a regular file at the archive root counts; archives also carry
        // a licenses/ directory, and nothing else may pose as the binary.
        let path = entry.path()?.to_path_buf();
        let path = path.strip_prefix("./").unwrap_or(&path);
        if path != Path::new(name) || !entry.header().entry_type().is_file() {
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
         {ca}",
        tz = system_timezone(),
        ca = ca_bundle()
            .map(|path| format!("\nopenssl.cafile = {path}\ncurl.cainfo = {path}\n"))
            .unwrap_or_default(),
    )
}

/// The system's CA bundle, for PHP's openssl and curl (Bird's static PHP
/// has no default of its own that fits every system). macOS has one place;
/// Linux distributions each have their own.
fn ca_bundle() -> Option<&'static str> {
    if cfg!(target_os = "macos") {
        return Some("/etc/ssl/cert.pem");
    }
    [
        "/etc/ssl/certs/ca-certificates.crt", // Debian, Ubuntu, Arch
        "/etc/pki/tls/certs/ca-bundle.crt",   // Fedora, RHEL
        "/etc/ssl/ca-bundle.pem",             // openSUSE
        "/etc/ssl/cert.pem",
    ]
    .into_iter()
    .find(|path| Path::new(path).is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tarball(entries: &[(&str, tar::EntryType, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        for (path, kind, data) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(*kind);
            header.set_size(data.len() as u64);
            header.set_mode(0o755);
            // Write the name raw, so tests can also build malicious archives
            // (`..` paths) that the tar crate refuses to create itself.
            let name = &mut header.as_old_mut().name;
            name.fill(0);
            name[..path.len()].copy_from_slice(path.as_bytes());
            header.set_cksum();
            builder.append(&header, *data).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn extracts_licenses_without_paths() {
        let dir = std::env::temp_dir().join(format!("bird-lic-{}", std::process::id()));
        let archive = tarball(&[
            ("php", tar::EntryType::Regular, b"bin"),
            (
                "licenses/src_php-src_0.txt",
                tar::EntryType::Regular,
                b"PHP License",
            ),
            ("licenses/../../escape.txt", tar::EntryType::Regular, b"no"),
            ("other/readme.txt", tar::EntryType::Regular, b"no"),
        ]);
        extract_licenses(&archive, &dir).unwrap();
        let names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(names, ["src_php-src_0.txt"]);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn parses_manifest() {
        let m: Manifest = serde_json::from_str(r#"{
            "schema": 1,
            "latest": {"8.4": {"macos-aarch64": "8.4.26"}},
            "builds": [{
                "version": "8.4.26", "minor": "8.4", "platform": "macos-aarch64",
                "cli": {"file": "php-8.4.26-cli-macos-aarch64.tar.gz", "sha256": "ab", "size": 1},
                "fpm": {"file": "php-8.4.26-fpm-macos-aarch64.tar.gz", "sha256": "cd", "size": 2},
                "shared_extensions": {"xdebug": {"version": "3.5.3", "file": "x.tar.gz", "sha256": "ef", "size": 3}},
                "extensions": ["bcmath"], "sources": {}, "built_at": "2026-10-16T19:50:00Z"
            }]
        }"#).unwrap();
        assert_eq!(m.latest["8.4"]["macos-aarch64"], "8.4.26");
        assert_eq!(m.builds[0].shared_extensions["xdebug"].size, 3);
    }

    // Test-only key and signatures (`ssh-keygen -Y sign`); not Bird's PHP key.
    const TEST_PHP_KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOqj6KKG7FhsFNuXZ5TgGDRn0zGsEuQErozojaIiLvI/ bird-php-test";
    const SIGNED_MANIFEST: &[u8] =
        b"{\"schema\": 1, \"latest\": {\"8.4\": {\"macos-aarch64\": \"8.4.26\"}}, \"builds\": []}\n";
    // `-n bird-php`
    const MANIFEST_SIG: &str = "-----BEGIN SSH SIGNATURE-----
U1NIU0lHAAAAAQAAADMAAAALc3NoLWVkMjU1MTkAAAAg6qPooobsWGwU25dnlOAYNGfTMa
wS5ASujOiNoiIu8j8AAAAIYmlyZC1waHAAAAAAAAAABnNoYTUxMgAAAFMAAAALc3NoLWVk
MjU1MTkAAABACdAX7iuSpDyZs65m9KrDWy6YGzNQcBnYSr9BUoSFjkNQ57QB8IjMhMKfti
7Xdxxqu7hcXhBVCEl9YcyBqx3DCQ==
-----END SSH SIGNATURE-----
";
    // The same file and key, but `-n bird-release`.
    const RELEASE_NAMESPACE_SIG: &str = "-----BEGIN SSH SIGNATURE-----
U1NIU0lHAAAAAQAAADMAAAALc3NoLWVkMjU1MTkAAAAg6qPooobsWGwU25dnlOAYNGfTMa
wS5ASujOiNoiIu8j8AAAAMYmlyZC1yZWxlYXNlAAAAAAAAAAZzaGE1MTIAAABTAAAAC3Nz
aC1lZDI1NTE5AAAAQLSM45sFiSVH/V2GC4StWGIUARsZgeILBwnJbaVv4xEpL9+fbjt/UT
Jek2xLuMGjKUUEIcBnDbxJe3uqtkULsQ4=
-----END SSH SIGNATURE-----
";

    #[test]
    fn accepts_a_signed_manifest() {
        let m = check_manifest("m", SIGNED_MANIFEST, Some(MANIFEST_SIG), &[TEST_PHP_KEY]).unwrap();
        assert_eq!(m.latest["8.4"]["macos-aarch64"], "8.4.26");
    }

    #[test]
    fn rejects_badly_signed_manifests() {
        let tampered = String::from_utf8_lossy(SIGNED_MANIFEST).replace("8.4.26", "8.4.27");
        assert!(
            check_manifest(
                "m",
                tampered.as_bytes(),
                Some(MANIFEST_SIG),
                &[TEST_PHP_KEY]
            )
            .is_err(),
            "changed manifest"
        );
        assert!(
            check_manifest(
                "m",
                SIGNED_MANIFEST,
                Some(MANIFEST_SIG),
                crate::signing::PHP_KEYS
            )
            .is_err(),
            "signed with a key Bird does not trust"
        );
        assert!(
            check_manifest(
                "m",
                SIGNED_MANIFEST,
                Some(RELEASE_NAMESPACE_SIG),
                &[TEST_PHP_KEY]
            )
            .is_err(),
            "a release signature is not a manifest signature"
        );
        assert!(check_manifest("m", SIGNED_MANIFEST, Some("garbage"), &[TEST_PHP_KEY]).is_err());
    }

    #[test]
    fn custom_mirrors_need_no_signature() {
        check_manifest("m", SIGNED_MANIFEST, None, &[]).unwrap();
    }

    #[test]
    fn extracts_only_the_root_binary() {
        let dir = std::env::temp_dir().join(format!("bird-extract-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("php");
        let archive = tarball(&[
            ("licenses/php", tar::EntryType::Regular, b"not me"),
            ("php", tar::EntryType::Regular, b"the binary"),
            (
                "licenses/src_php-src_0.txt",
                tar::EntryType::Regular,
                b"license",
            ),
        ]);
        extract_binary(&archive, "php", &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"the binary");

        let only_nested = tarball(&[("evil/php", tar::EntryType::Regular, b"x")]);
        assert!(extract_binary(&only_nested, "php", &dest).is_err());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn managed_ini_revalidates_every_request() {
        let on = managed_ini(true);
        assert!(on.contains("opcache.enable = 1"));
        assert!(on.contains("opcache.validate_timestamps = 1"));
        assert!(on.contains("opcache.revalidate_freq = 0"));
        assert!(managed_ini(false).contains("opcache.enable = 0"));
    }

    #[test]
    fn detects_outdated_installs() {
        let inst = |version: &str, source: &str, build: Option<&str>| Installed {
            minor: "8.5".into(),
            version: version.into(),
            source: source.into(),
            build: build.map(String::from),
        };
        let remote = Remote {
            version: "8.5.11".into(),
            build: Some("abc".into()),
        };
        assert!(!inst("8.5.11", SOURCE_BIRD, Some("abc")).outdated(&remote, SOURCE_BIRD));
        assert!(
            inst("8.5.11", SOURCE_BIRD, Some("old")).outdated(&remote, SOURCE_BIRD),
            "rebuild"
        );
        assert!(
            inst("8.5.8", SOURCE_STATIC_PHP, None).outdated(&remote, SOURCE_BIRD),
            "static-php"
        );
        assert!(
            inst("8.5.10", SOURCE_BIRD, Some("abc")).outdated(&remote, SOURCE_BIRD),
            "patch"
        );
        let static_remote = Remote {
            version: "8.5.11".into(),
            build: None,
        };
        assert!(
            !inst("8.5.11", SOURCE_STATIC_PHP, None).outdated(&static_remote, SOURCE_STATIC_PHP)
        );
    }

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
