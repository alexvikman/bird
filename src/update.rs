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

// What GitHub's API says about a release is not signed. Its notes (`body`)
// and page link can be changed by anyone who can edit the release, so
// neither is read: the notes come from a signed asset (`signed_notes`) and
// the link is built from the version (`release_url`).
#[derive(Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    assets: Vec<Asset>,
}

/// The release notes as an asset, so the signed SHA256SUMS covers them.
const NOTES_ASSET: &str = "RELEASE_NOTES.md";

/// The archive with the bird binary of `version` for this platform.
fn binary_archive(version: &str) -> String {
    format!("bird-{version}-{TARGET}.tar.gz")
}

/// The page of a release (`version` is digits and dots, see `release_version`).
fn release_url(version: &str) -> String {
    format!("https://github.com/{REPO}/releases/tag/v{version}")
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
    let bytes = php::download(&asset.browser_download_url, label, php::MAX_DOWNLOAD)?;
    let actual = sha256_hex(&bytes);
    if actual != expected {
        bail!("{name} failed verification: sha256 {actual}, SHA256SUMS says {expected}");
    }
    Ok(bytes)
}

/// SHA256SUMS of a release, once its signature checks out against a key
/// compiled into Bird; the checksums then vouch for every file they list.
fn verified_sums(release: &Release) -> Result<String> {
    let sums_asset = release
        .asset("SHA256SUMS")
        .context("the release has no SHA256SUMS")?;
    let sig_asset = release
        .asset("SHA256SUMS.sig")
        .context("the release is not signed (no SHA256SUMS.sig)")?;
    // Both are small text files, read before anything vouches for them.
    let fetch = |asset: &Asset| -> Result<String> {
        let bytes = php::fetch(&php::http()?, &asset.browser_download_url, 1 << 20)
            .with_context(|| format!("could not download {}", asset.name))?;
        String::from_utf8(bytes).with_context(|| format!("{} is not text", asset.name))
    };
    let sums = fetch(sums_asset)?;
    let sig = fetch(sig_asset)?;
    crate::signing::verify(
        sums.as_bytes(),
        &sig,
        crate::signing::NAMESPACE,
        crate::signing::RELEASE_KEYS,
    )
    .context("the release signature is not valid")?;
    Ok(sums)
}

/// The notes of release `version`, if the release's signature covers them:
/// its RELEASE_NOTES.md, listed in the signed SHA256SUMS. `None` for
/// releases from before the notes were signed, or when anything about them
/// doesn't check out; the app then shows the update without notes.
fn signed_notes(release: &Release, version: &str) -> Option<String> {
    let asset = release.asset(NOTES_ASSET)?;
    let sums = verified_sums(release).ok()?;
    let notes = php::fetch(&php::http().ok()?, &asset.browser_download_url, 256 << 10).ok()?;
    notes_if_signed(&sums, version, &notes)
}

/// `notes` as text, if `sums` (already verified) are those of `version` and
/// list this very file. An older release's SHA256SUMS carries a valid
/// signature too, so the sums must name this version's binary.
fn notes_if_signed(sums: &str, version: &str, notes: &[u8]) -> Option<String> {
    checksum_for(sums, &binary_archive(version))?;
    if checksum_for(sums, NOTES_ASSET)? != sha256_hex(notes) {
        return None;
    }
    String::from_utf8(notes.to_vec()).ok()
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
    // Only fetched when there is something to show them for.
    let notes = match (&release, &latest) {
        (Some(release), Some(latest)) if available => signed_notes(release, latest),
        _ => None,
    };
    println!(
        "{}",
        json!({
            "current": current,
            "latest": latest,
            "available": available,
            "notes": notes,
            "url": latest.as_deref().map(release_url),
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
            release_url(&latest)
        );
        println!("Install it with `bird self-update`.");
        return Ok(());
    }

    // SHA256SUMS only counts if it carries a signature from a key compiled
    // into Bird; the checksums then vouch for every file below.
    let sums = verified_sums(&release).context("not installing this release")?;
    ui::ok("release signature verified");

    // The bird binary.
    let archive = download_checked(
        &release,
        &sums,
        &binary_archive(&latest),
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

/// The Bird.app to update: the one that started this update (Bird.app passes
/// its own path in $BIRD_APP), else the one in /Applications, else one in
/// ~/Applications (where earlier versions of build.sh installed it).
fn installed_app() -> Option<PathBuf> {
    find_app(
        std::env::var_os("BIRD_APP").map(PathBuf::from),
        &user_home(),
    )
}

fn find_app(from_app: Option<PathBuf>, home: &Path) -> Option<PathBuf> {
    from_app
        .into_iter()
        .chain([
            PathBuf::from("/Applications/Bird.app"),
            home.join("Applications/Bird.app"),
        ])
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
    fn updates_the_app_that_asked() {
        let dir = std::env::temp_dir().join(format!("bird-find-app-{}", std::process::id()));
        let app = dir.join("Somewhere/Bird.app");
        std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        std::fs::write(app.join("Contents/MacOS/Bird"), "").unwrap();
        let home = dir.join("home");
        assert_eq!(find_app(Some(app.clone()), &home), Some(app));
        // A path without an app falls back to the usual places.
        let gone = dir.join("Gone/Bird.app");
        assert_ne!(find_app(Some(gone.clone()), &home), Some(gone));
        std::fs::remove_dir_all(&dir).unwrap();
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
    fn notes_count_only_when_the_signed_sums_list_them() {
        let notes = b"## Changes\n\n- Something\n";
        let version = "0.5.0";
        let sums = format!(
            "abc123  {}\n{}  {NOTES_ASSET}\n",
            binary_archive(version),
            sha256_hex(notes)
        );
        assert_eq!(
            notes_if_signed(&sums, version, notes).as_deref(),
            Some("## Changes\n\n- Something\n")
        );
        // Edited notes, as someone who can edit the release could upload.
        assert_eq!(
            notes_if_signed(&sums, version, b"run this in Terminal"),
            None
        );
        // The (validly signed) sums of another release.
        assert_eq!(notes_if_signed(&sums, "0.6.0", notes), None);
        // A release from before the notes were signed.
        let old = format!("abc123  {}\n", binary_archive(version));
        assert_eq!(notes_if_signed(&old, version, notes), None);
    }

    #[test]
    fn links_to_the_release_page() {
        assert_eq!(
            release_url("0.5.0"),
            "https://github.com/alexvikman/bird/releases/tag/v0.5.0"
        );
    }

    #[test]
    fn knows_its_target() {
        assert!(TARGET.contains("darwin") || TARGET.contains("linux"));
    }
}
