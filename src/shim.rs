//! `php`, `php8.4` and `composer` in `~/.bird/bin` are symlinks to the bird
//! binary. When invoked under one of those names, bird picks the right PHP
//! version for the current directory and execs it directly.

use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::config::Config;
use crate::paths::paths;
use crate::php;

pub enum Shim {
    /// `php` — version chosen from the current directory.
    Php,
    /// `php8.4` — explicit version.
    PhpVersion(String),
    Composer,
}

pub fn detect(argv0: &str) -> Option<Shim> {
    let name = Path::new(argv0).file_name()?.to_str()?;
    match name {
        "php" => Some(Shim::Php),
        "composer" => Some(Shim::Composer),
        _ => name
            .strip_prefix("php")
            .filter(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
            .and_then(php::normalize)
            .map(Shim::PhpVersion),
    }
}

pub fn run(shim: Shim, args: Vec<OsString>) -> Result<()> {
    let version = match &shim {
        Shim::PhpVersion(v) => v.clone(),
        Shim::Php | Shim::Composer => current_version()?,
    };
    match shim {
        Shim::Composer => exec_composer(&version, args),
        _ => exec_php(&version, args),
    }
}

pub fn current_version() -> Result<String> {
    let cfg = Config::load()?;
    let cwd = std::env::current_dir()?;
    match php::resolve_for_dir(&cfg, &cwd) {
        Some((v, _)) => Ok(v),
        None => bail!("no PHP version configured. Install one with: bird php install 8.4"),
    }
}

pub fn php_command(version: &str) -> Result<Command> {
    let bin = paths().php_binary(version);
    if !bin.is_file() {
        bail!("PHP {version} is not installed. Install it with: bird php:install {version}");
    }
    let mut cmd = Command::new(bin);
    cmd.env("PHPRC", paths().php_dir(version))
        .env("PHP_INI_SCAN_DIR", paths().php_conf_d(version));
    Ok(cmd)
}

pub fn exec_php(version: &str, args: Vec<OsString>) -> Result<()> {
    let err = php_command(version)?.args(args).exec();
    Err(err).context("could not run php")
}

/// `php` with Xdebug loaded and a debug session started (`bird debug`).
pub fn exec_php_xdebug(version: &str, args: Vec<OsString>) -> Result<()> {
    let err = php_command(version)?
        .env("PHP_INI_SCAN_DIR", php::xdebug_scan_dirs(version)?)
        .args(["-d", "xdebug.start_with_request=yes"])
        .args(args)
        .exec();
    Err(err).context("could not run php")
}

/// `php` with PCOV loaded for code coverage (`bird coverage`).
pub fn exec_php_coverage(version: &str, args: Vec<OsString>) -> Result<()> {
    let so = php::pcov_so(version);
    if !so.is_file() {
        bail!(
            "PHP {version} has no PCOV; `bird php:update {version}` installs Bird's build, which has it"
        );
    }
    let err = php_command(version)?
        .arg("-d")
        .arg(format!("extension={}", so.display()))
        .args(["-d", "pcov.enabled=1"])
        .args(args)
        .exec();
    Err(err).context("could not run php")
}

pub fn exec_composer(version: &str, args: Vec<OsString>) -> Result<()> {
    let phar = paths().composer_phar();
    if !phar.is_file() {
        install_composer()?;
    }
    let err = php_command(version)?.arg(&phar).args(args).exec();
    Err(err).context("could not run composer")
}

const COMPOSER_HOST: &str = "https://getcomposer.org";

/// The key Composer signs its releases with ("Tags Public Key" on
/// https://composer.github.io/pubkeys.html, and what `composer self-update`
/// itself checks against). Next to every released composer.phar is a `.sig`
/// with an RSA signature (SHA-384) of the file.
const COMPOSER_RELEASE_KEY: &str = "\
-----BEGIN PUBLIC KEY-----
MIICIjANBgkqhkiG9w0BAQEFAAOCAg8AMIICCgKCAgEA0Vi/2K6apCVj76nCnCl2
MQUPdK+A9eqkYBacXo2wQBYmyVlXm2/n/ZsX6pCLYPQTHyr5jXbkQzBw8SKqPdlh
vA7NpbMeNCz7wP/AobvUXM8xQuXKbMDTY2uZ4O7sM+PfGbptKPBGLe8Z8d2sUnTO
bXtX6Lrj13wkRto7st/w/Yp33RHe9SlqkiiS4MsH1jBkcIkEHsRaveZzedUaxY0M
mba0uPhGUInpPzEHwrYqBBEtWvP97t2vtfx8I5qv28kh0Y6t+jnjL1Urid2iuQZf
noCMFIOu4vksK5HxJxxrN0GOmGmwVQjOOtxkwikNiotZGPR4KsVj8NnBrLX7oGuM
nQvGciiu+KoC2r3HDBrpDeBVdOWxDzT5R4iI0KoLzFh2pKqwbY+obNPS2bj+2dgJ
rV3V5Jjry42QOCBN3c88wU1PKftOLj2ECpewY6vnE478IipiEu7EAdK8Zwj2LmTr
RKQUSa9k7ggBkYZWAeO/2Ag0ey3g2bg7eqk+sHEq5ynIXd5lhv6tC5PBdHlWipDK
tl2IxiEnejnOmAzGVivE1YGduYBjN+mjxDVy8KGBrjnz1JPgAvgdwJ2dYw4Rsc/e
TzCFWGk/HM6a4f0IzBWbJ5ot0PIi4amk07IotBXDWwqDiQTwyuGCym5EqWQ2BD95
RGv89BPD+2DLnJysngsvVaUCAwEAAQ==
-----END PUBLIC KEY-----
";

/// The most a composer.phar may weigh (it is around 3 MB).
const MAX_COMPOSER: u64 = 64 << 20;

/// The oldest Composer Bird installs: the stable release when this was
/// written. The list of versions at getcomposer.org is not signed, while
/// every old release still has a valid signature, so without a floor a
/// tampered list could hand out an old Composer with known holes. Raise it
/// now and then; Composer keeps itself current with `composer self-update`.
const MIN_COMPOSER: &str = "2.11.2";

pub fn install_composer() -> Result<()> {
    eprintln!("Downloading Composer…");
    let bytes = fetch_composer().context("could not download composer.phar")?;
    let phar = paths().composer_phar();
    std::fs::create_dir_all(phar.parent().unwrap())?;
    let tmp = phar.with_extension("download");
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, &phar)?;
    Ok(())
}

/// The latest stable composer.phar, checked against Composer's release key:
/// PHP runs this file with the user's rights, so HTTPS to getcomposer.org
/// alone does not vouch for it.
fn fetch_composer() -> Result<Vec<u8>> {
    #[derive(serde::Deserialize)]
    struct Versions {
        stable: Vec<Release>,
    }
    #[derive(serde::Deserialize)]
    struct Release {
        path: String,
    }
    #[derive(serde::Deserialize)]
    struct Signature {
        sha384: String,
    }
    let client = php::http()?;
    let versions = php::fetch(&client, &format!("{COMPOSER_HOST}/versions"), 1 << 20)?;
    let versions: Versions =
        serde_json::from_slice(&versions).context("unexpected answer from getcomposer.org")?;
    let path = versions
        .stable
        .first()
        .map(|release| release.path.as_str())
        .filter(|path| is_composer_path(path))
        .context("getcomposer.org lists no stable Composer release")?;
    let url = format!("{COMPOSER_HOST}{path}");
    let phar = php::fetch(&client, &url, MAX_COMPOSER)?;
    let signature = php::fetch(&client, &format!("{url}.sig"), 64 << 10)
        .context("not trusting an unsigned composer.phar")?;
    let signature: Signature =
        serde_json::from_slice(&signature).context("unexpected composer.phar.sig")?;
    verify_composer(&phar, &signature.sha384, COMPOSER_RELEASE_KEY)
        .context("composer.phar is not signed by Composer's release key; not installing it")?;
    // The signature says Composer made this file, not which release it is.
    // That comes from the (now trusted) file itself, never from the list.
    let version = composer_version(&phar)
        .context("could not tell which Composer version was downloaded; not installing it")?;
    if !path.contains(&format!("/{version}/")) {
        bail!("{url} is Composer {version}, not the release it was listed as; not installing it");
    }
    if version_key(&version) < version_key(MIN_COMPOSER) {
        bail!(
            "getcomposer.org offered Composer {version}, older than {MIN_COMPOSER}; \
             not installing an outdated Composer"
        );
    }
    Ok(phar)
}

/// The version a composer.phar says it is: `const VERSION = '2.11.2'` in
/// its Composer.php, there since Composer 1.
fn composer_version(phar: &[u8]) -> Option<String> {
    const MARK: &[u8] = b"const VERSION = '";
    let start = phar.windows(MARK.len()).position(|w| w == MARK)? + MARK.len();
    let len = phar[start..].iter().take(32).position(|&b| b == b'\'')?;
    let version = std::str::from_utf8(&phar[start..start + len]).ok()?;
    let plain = !version.is_empty()
        && version
            .split('.')
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()));
    plain.then(|| version.to_string())
}

fn version_key(version: &str) -> Vec<u32> {
    version.split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

/// `/download/2.11.2/composer.phar`: a released version's phar, nothing else
/// the versions list could point at.
fn is_composer_path(path: &str) -> bool {
    path.strip_prefix("/download/")
        .and_then(|rest| rest.strip_suffix("/composer.phar"))
        .is_some_and(|version| {
            !version.is_empty()
                && !version.starts_with('.')
                && version
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
        })
}

/// Checks `signature` (base64, RSA PKCS#1 v1.5 with SHA-384, as in Composer's
/// `.sig` files) over `phar` against the public key in `key_pem`.
fn verify_composer(phar: &[u8], signature: &str, key_pem: &str) -> Result<()> {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use x509_parser::prelude::FromDer;
    let signature = STANDARD
        .decode(signature.trim())
        .context("the signature is not base64")?;
    let spki: String = key_pem
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with("-----"))
        .collect();
    let spki = STANDARD.decode(spki).context("bad public key")?;
    // ring wants the RSAPublicKey inside the SubjectPublicKeyInfo.
    let (_, spki) = x509_parser::x509::SubjectPublicKeyInfo::from_der(&spki)
        .map_err(|_| anyhow::anyhow!("bad public key"))?;
    ring::signature::UnparsedPublicKey::new(
        &ring::signature::RSA_PKCS1_2048_8192_SHA384,
        spki.subject_public_key.data.as_ref(),
    )
    .verify(phar, &signature)
    .map_err(|_| anyhow::anyhow!("the signature does not match"))
}

/// Recreates the symlinks in `~/.bird/bin` to match the installed PHP versions.
pub fn refresh() -> Result<()> {
    let bin = paths().bin();
    std::fs::create_dir_all(&bin)?;
    let target = if paths().bird_binary().is_file() {
        paths().bird_binary()
    } else {
        std::env::current_exe()?
    };
    let mut wanted = vec!["php".to_string(), "composer".to_string()];
    wanted.extend(
        php::installed()
            .into_iter()
            .map(|i| format!("php{}", i.minor)),
    );

    for entry in std::fs::read_dir(&bin)?.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let is_link = entry.file_type().is_ok_and(|t| t.is_symlink());
        if is_link && detect(&name).is_some() && !wanted.contains(&name) {
            std::fs::remove_file(entry.path())?;
        }
    }
    for name in wanted {
        let link = bin.join(&name);
        if std::fs::read_link(&link).is_ok_and(|t| t == target) {
            continue;
        }
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&target, &link)
            .with_context(|| format!("could not create {}", link.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test-only RSA key and signature (`openssl dgst -sha384 -sign`); not
    // Composer's key. Its private half was thrown away.
    const TEST_KEY: &str = "\
-----BEGIN PUBLIC KEY-----
MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEA2PHI0sisyOYinMRW/EsO
xBJK6aQyPYQvKv25uj9z8Unjj3WYIEvDOT45GIJxt+gD/8Gf2MITRDECe+Bx1Mtm
CUTvEYMnFMzPmHGWpojLlLLeYHGN4M2K/+im8xtb/yeDWIHsvhjAgVlYdrzWkLiV
KSHpR5/DtWuNVuGoC0AJ5efvlYOq/vl6emOf1UOH8HbbvzYk1QrmXo040YOgEvWK
EJQlNoRrcVVAbh/R37sEjKPVNxMotVN501ChxNCieCjAuF78q9q2Xis25TL+dzbl
sfyBV6IwlMQoOa5y4Oo7h23fLPPV5Nxa/j969endpRdEvgQSRL5sRRQ/7ZBImbBQ
fwIDAQAB
-----END PUBLIC KEY-----
";
    const TEST_PHAR: &[u8] = b"composer test phar\n";
    const TEST_SIGNATURE: &str = "Wd+qAM+/W9kr+/gERWtmaxGE2e+Rj6Mwa9VcoKVYiWvineJVmX/MpPlNgUh7zKddtiaXzlBOc+XMJKDznrboDxoCaII41vcB7Lt15CTAr7wfl/C2sdp5j6o1YnmtbpJGcK6oqI4x/+LG60GwP4ekm4kBedAhawaIIcSzHAS+cJiwZTxeSYPuLxmWKaxiWDBe758bbjfcNBovFOrwQx1R3eW4d/o1+N8r88yEYF5CegsujbqFRjGfxCnfB9239Jz3Mw1lDA9jEE9z2ZXmvSiCKc/N7hr7+JgusMoalPl5Z6gBYxt18mRfXWnhXm+UUdIa+omPZu2ePpafz/OtJEVJ6Q==";

    #[test]
    fn composer_signatures_are_checked() {
        verify_composer(TEST_PHAR, TEST_SIGNATURE, TEST_KEY).unwrap();
        assert!(
            verify_composer(b"composer test phar, patched\n", TEST_SIGNATURE, TEST_KEY).is_err(),
            "changed file"
        );
        assert!(
            verify_composer(TEST_PHAR, TEST_SIGNATURE, COMPOSER_RELEASE_KEY).is_err(),
            "signed by another key"
        );
        assert!(verify_composer(TEST_PHAR, "not base64!", TEST_KEY).is_err());
        assert!(verify_composer(TEST_PHAR, "", TEST_KEY).is_err());
    }

    #[test]
    fn only_released_phars_are_fetched() {
        assert!(is_composer_path("/download/2.11.2/composer.phar"));
        assert!(is_composer_path("/download/2.9.0-RC1/composer.phar"));
        for bad in [
            "/composer.phar",
            "/download/../composer.phar",
            "/download//composer.phar",
            "/download/2.11.2/x/composer.phar",
            "//evil.example/download/1/composer.phar",
            "@evil.example/download/1/composer.phar",
        ] {
            assert!(!is_composer_path(bad), "{bad}");
        }
    }

    #[test]
    fn reads_the_version_out_of_a_phar() {
        let phar = b"<?php ...class Composer{const VERSION = '2.11.2';const BRANCH_ALIAS...";
        assert_eq!(composer_version(phar).as_deref(), Some("2.11.2"));
        assert_eq!(
            composer_version(b"const VERSION = '1.10.28'").as_deref(),
            Some("1.10.28")
        );
        // Not a release (a snapshot's VERSION is a commit hash or @package_version@).
        assert_eq!(
            composer_version(b"const VERSION = '@package_version@';"),
            None
        );
        assert_eq!(composer_version(b"const VERSION = '2.9.0-RC1';"), None);
        assert_eq!(composer_version(b"const VERSION = '';"), None);
        assert_eq!(composer_version(b"no version here"), None);
        assert_eq!(composer_version(b"const VERSION = '2.11.2"), None);

        assert!(version_key("2.2.32") < version_key(MIN_COMPOSER));
        assert!(version_key("1.10.28") < version_key(MIN_COMPOSER));
        assert!(version_key("2.11.10") > version_key("2.11.2"));
        assert!(version_key(MIN_COMPOSER) >= version_key(MIN_COMPOSER));
    }

    /// Against the real getcomposer.org: `cargo test -- --ignored composer`.
    #[test]
    #[ignore = "needs network"]
    fn the_real_composer_verifies() {
        // What main() does before anything else.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let phar = fetch_composer().unwrap();
        assert!(phar.len() > 1 << 20, "a real composer.phar is a few MB");
        let version = composer_version(&phar).unwrap();
        assert!(
            version_key(&version) >= version_key(MIN_COMPOSER),
            "{version}"
        );
    }
}
