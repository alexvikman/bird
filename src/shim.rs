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
        bail!("PHP {version} is not installed. Install it with: bird php install {version}");
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

pub fn exec_composer(version: &str, args: Vec<OsString>) -> Result<()> {
    let phar = paths().composer_phar();
    if !phar.is_file() {
        install_composer()?;
    }
    let err = php_command(version)?.arg(&phar).args(args).exec();
    Err(err).context("could not run composer")
}

pub fn install_composer() -> Result<()> {
    eprintln!("Downloading Composer…");
    let bytes =
        reqwest::blocking::get("https://getcomposer.org/download/latest-stable/composer.phar")
            .and_then(|r| r.error_for_status())
            .and_then(|r| r.bytes())
            .context("could not download composer.phar")?;
    let phar = paths().composer_phar();
    std::fs::create_dir_all(phar.parent().unwrap())?;
    let tmp = phar.with_extension("download");
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, &phar)?;
    Ok(())
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
