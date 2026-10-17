//! `bird self-update`: installs the latest Bird release from GitHub.
//!
//! Downloads the archive for this platform (and Bird.app on macOS, when it is
//! installed), checks both against the release's SHA256SUMS, swaps them in
//! atomically and restarts the daemon so it runs the new binary.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::json;

use crate::paths::{paths, user_home};
use crate::{control, install, php, ui};

const REPO: &str = "alexvikman/bird";
const TARGET: &str = env!("BIRD_TARGET");

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    /// Release notes (Markdown).
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    html_url: String,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

impl Release {
    fn asset(&self, name: &str) -> Option<&Asset> {
        self.assets.iter().find(|a| a.name == name)
    }
}

fn version_key(v: &str) -> Vec<u64> {
    v.split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

/// Whether `latest` (e.g. `0.3.0`) is newer than `current`.
pub fn is_newer(latest: &str, current: &str) -> bool {
    version_key(latest) > version_key(current)
}

/// `<sha256>  <file>` lines, as written by sha256sum.
fn checksum_for(sums: &str, file: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, name) = line.split_once(char::is_whitespace)?;
        (name.trim().trim_start_matches('*') == file).then(|| hash.to_ascii_lowercase())
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    digest.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// `0.2.0` for a `v0.2.0` tag; other tags (like the `php` builds release)
/// are not Bird releases.
fn release_version(tag: &str) -> Option<&str> {
    let v = tag.strip_prefix('v')?;
    let parts: Vec<&str> = v.split('.').collect();
    (parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit())))
    .then_some(v)
}

/// The newest published Bird release. Not GitHub's "latest release": the
/// repository also has the `php` release with PHP builds.
fn latest_release() -> Result<Option<Release>> {
    let url = format!("https://api.github.com/repos/{REPO}/releases?per_page=50");
    let releases: Vec<Release> = php::http()?
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .and_then(|r| r.error_for_status())
        .context("could not check for Bird releases on GitHub")?
        .json()
        .context("unexpected answer from GitHub")?;
    Ok(releases
        .into_iter()
        .filter(|r| !r.draft && !r.prerelease && release_version(&r.tag_name).is_some())
        .max_by_key(|r| version_key(release_version(&r.tag_name).unwrap_or("0"))))
}

/// Downloads a release asset and checks it against SHA256SUMS.
fn download_checked(release: &Release, sums: &str, name: &str, label: &str) -> Result<Vec<u8>> {
    let asset = release
        .asset(name)
        .with_context(|| format!("release {} has no {name}", release.tag_name))?;
    let expected =
        checksum_for(sums, name).with_context(|| format!("SHA256SUMS lists no {name}"))?;
    let bytes = php::download(&asset.browser_download_url, label)?;
    let actual = sha256_hex(&bytes);
    if actual != expected {
        bail!("{name} failed verification: sha256 {actual}, SHA256SUMS says {expected}");
    }
    Ok(bytes)
}

/// `bird self-update --check --json`, for the macOS app.
pub fn check_json() -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    let release = latest_release()?;
    let latest = release
        .as_ref()
        .and_then(|r| release_version(&r.tag_name))
        .map(str::to_string);
    let available = latest.as_deref().is_some_and(|l| is_newer(l, current));
    println!(
        "{}",
        json!({
            "current": current,
            "latest": latest,
            "available": available,
            "notes": release.as_ref().and_then(|r| r.body.clone()),
            "url": release.map(|r| r.html_url),
        })
    );
    Ok(())
}

pub fn self_update(check_only: bool) -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    let Some(release) = latest_release()? else {
        ui::ok(format!(
            "bird {current}: no Bird releases are published yet"
        ));
        return Ok(());
    };
    let latest = release_version(&release.tag_name)
        .unwrap_or("0")
        .to_string();
    if !is_newer(&latest, current) {
        ui::ok(format!("bird {current} is up to date"));
        return Ok(());
    }
    if check_only {
        println!(
            "bird {latest} is available (you have {current}): {}",
            release.html_url
        );
        println!("Install it with `bird self-update`.");
        return Ok(());
    }

    let sums_asset = release
        .asset("SHA256SUMS")
        .context("the release has no SHA256SUMS")?;
    let fetch = |asset: &Asset| {
        php::http()?
            .get(&asset.browser_download_url)
            .send()
            .and_then(|r| r.error_for_status())
            .and_then(|r| r.text())
            .with_context(|| format!("could not download {}", asset.name))
    };
    let sums = fetch(sums_asset)?;
    // SHA256SUMS only counts if it carries a signature from a key compiled
    // into Bird; the checksums then vouch for every file below.
    let sig_asset = release
        .asset("SHA256SUMS.sig")
        .context("the release is not signed (no SHA256SUMS.sig); not installing it")?;
    let sig = fetch(sig_asset)?;
    crate::signing::verify(
        sums.as_bytes(),
        &sig,
        crate::signing::NAMESPACE,
        crate::signing::RELEASE_KEYS,
    )
    .context("the release signature is not valid; not installing it")?;
    ui::ok("release signature verified");

    // The bird binary.
    let archive = download_checked(
        &release,
        &sums,
        &format!("bird-{latest}-{TARGET}.tar.gz"),
        &format!("bird {latest}"),
    )?;
    let binary = paths().bird_binary();
    std::fs::create_dir_all(binary.parent().unwrap())?;
    // extract_binary writes next to the destination and renames, so the
    // running binary is replaced atomically.
    php::extract_binary(&archive, "bird", &binary).context("could not install the new bird")?;
    ui::ok(format!("bird {current} → {latest}"));

    // The new version migrates ~/.bird right away (it knows its migrations;
    // this one doesn't). The daemon would also do it when it restarts.
    let migrated = Command::new(&binary)
        .args(["migrate"])
        .stdout(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if !migrated {
        ui::warn("the new bird could not migrate ~/.bird; run `bird migrate` to see why");
    }

    // Bird.app, if it is installed.
    if let Some(app) = installed_app().filter(|_| cfg!(target_os = "macos")) {
        let arch = TARGET.split('-').next().unwrap_or("aarch64");
        let name = format!("Bird-{latest}-macos-{arch}.app.zip");
        match download_checked(&release, &sums, &name, "Bird.app")
            .and_then(|zip| replace_app(&zip, &app))
        {
            Ok(()) => ui::ok(format!("updated {}", app.display())),
            Err(e) => ui::warn(format!("could not update Bird.app: {e:#}")),
        }
    }

    // The daemon keeps running the old binary until it restarts.
    if install::running_as_service() == Some(true) {
        control::send(json!({"cmd": "shutdown"}))?;
        ui::ok("restarting the bird daemon with the new version");
    } else if control::is_running() {
        ui::warn("restart `bird daemon` to run the new version");
    }
    Ok(())
}

/// The Bird.app to update: the one in /Applications, else one in
/// ~/Applications (where earlier versions of build.sh installed it).
fn installed_app() -> Option<PathBuf> {
    [
        PathBuf::from("/Applications/Bird.app"),
        user_home().join("Applications/Bird.app"),
    ]
    .into_iter()
    .find(|p| p.join("Contents/MacOS/Bird").is_file())
}

/// Unpacks a zipped Bird.app next to the installed one and swaps it in,
/// restarting the app if it was running.
fn replace_app(zip: &[u8], app: &Path) -> Result<()> {
    let parent = app.parent().context("Bird.app has no parent folder")?;
    let staging = parent.join(".Bird.app.update");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)?;
    let zip_path = staging.join("Bird.app.zip");
    std::fs::write(&zip_path, zip)?;
    let status = Command::new("ditto")
        .args(["-x", "-k"])
        .arg(&zip_path)
        .arg(&staging)
        .status()?;
    let new_app = staging.join("Bird.app");
    if !status.success() || !new_app.join("Contents/MacOS/Bird").is_file() {
        let _ = std::fs::remove_dir_all(&staging);
        bail!("the downloaded archive does not contain Bird.app");
    }
    // -a: when the app runs this update, it is our ancestor, which pgrep and
    // pkill skip by default.
    let app_running = || {
        Command::new("pgrep")
            .args(["-a", "-x", "Bird"])
            .output()
            .is_ok_and(|o| o.status.success())
    };
    let was_running = app_running();
    if was_running {
        let _ = Command::new("pkill").args(["-a", "-x", "Bird"]).status();
        // Opening it again while the old one is still quitting fails
        // (LaunchServices error -600), so wait for it to be gone.
        for _ in 0..50 {
            if !app_running() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
    let old = parent.join(".Bird.app.old");
    let _ = std::fs::remove_dir_all(&old);
    std::fs::rename(app, &old)?;
    if let Err(e) = std::fs::rename(&new_app, app) {
        let _ = std::fs::rename(&old, app);
        return Err(e).context("could not move the new Bird.app into place");
    }
    let _ = std::fs::remove_dir_all(&old);
    let _ = std::fs::remove_dir_all(&staging);
    if was_running {
        let _ = Command::new("open").arg(app).status();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert!(is_newer("0.2.0", "0.1.0"));
        assert!(is_newer("0.10.0", "0.9.3"));
        assert!(is_newer("1.0.0", "0.99.99"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.2.0"));
    }

    #[test]
    fn only_version_tags_are_releases() {
        assert_eq!(release_version("v0.2.0"), Some("0.2.0"));
        assert_eq!(release_version("v10.0.1"), Some("10.0.1"));
        assert_eq!(release_version("php"), None);
        assert_eq!(release_version("v0.2"), None);
        assert_eq!(release_version("v0.2.0-rc1"), None);
    }

    #[test]
    fn finds_checksums() {
        let sums = "abc123  bird-0.2.0-aarch64-apple-darwin.tar.gz\nDEF456 *Bird-0.2.0-macos-aarch64.app.zip\n";
        assert_eq!(
            checksum_for(sums, "bird-0.2.0-aarch64-apple-darwin.tar.gz").as_deref(),
            Some("abc123")
        );
        assert_eq!(
            checksum_for(sums, "Bird-0.2.0-macos-aarch64.app.zip").as_deref(),
            Some("def456")
        );
        assert_eq!(
            checksum_for(sums, "bird-0.2.0-x86_64-apple-darwin.tar.gz"),
            None
        );
    }

    #[test]
    fn knows_its_target() {
        assert!(TARGET.contains("darwin") || TARGET.contains("linux"));
    }
}
