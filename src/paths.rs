//! Filesystem layout of a Bird installation.
//!
//! Everything lives under `~/.bird` (override with `BIRD_HOME`):
//!
//! ```text
//! ~/.bird/
//!   config.toml        global config: tld, default PHP, parked paths, sites, proxies
//!   bin/               bird itself + shims (php, php8.4, composer) — put this on PATH
//!   php/8.4/           bin/php, bin/php-fpm, php.ini, conf.d/, VERSION
//!   ca/                local certificate authority (ca.pem, ca-key.pem)
//!   valet/             Certificates/<domain>.crt|.key in Valet's layout (~/.config/valet links here)
//!   run/               sockets and pid files
//!   log/               daemon and php-fpm logs
//! ```

use std::path::PathBuf;
use std::sync::OnceLock;

pub struct Paths {
    pub home: PathBuf,
}

pub fn paths() -> &'static Paths {
    static PATHS: OnceLock<Paths> = OnceLock::new();
    PATHS.get_or_init(|| {
        let home = std::env::var_os("BIRD_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| user_home().join(".bird"));
        Paths { home }
    })
}

pub fn user_home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .expect("HOME is not set")
}

/// Only a real directory of our own (no link, nobody else's) will do for the
/// control and php-fpm sockets.
fn check_owned_dir(dir: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::symlink_metadata(dir)?;
    if !meta.file_type().is_dir() || meta.uid() != crate::sys::euid() {
        return Err(std::io::Error::other(format!(
            "{} is not a directory of your own; remove it and try again",
            dir.display()
        )));
    }
    Ok(())
}

impl Paths {
    pub fn config(&self) -> PathBuf {
        self.home.join("config.toml")
    }

    pub fn bin(&self) -> PathBuf {
        self.home.join("bin")
    }

    pub fn bird_binary(&self) -> PathBuf {
        self.bin().join("bird")
    }

    pub fn composer_phar(&self) -> PathBuf {
        self.bin().join("composer.phar")
    }

    pub fn php_root(&self) -> PathBuf {
        self.home.join("php")
    }

    pub fn php_dir(&self, version: &str) -> PathBuf {
        self.php_root().join(version)
    }

    pub fn php_binary(&self, version: &str) -> PathBuf {
        self.php_dir(version).join("bin/php")
    }

    pub fn fpm_binary(&self, version: &str) -> PathBuf {
        self.php_dir(version).join("bin/php-fpm")
    }

    pub fn php_ini(&self, version: &str) -> PathBuf {
        self.php_dir(version).join("php.ini")
    }

    pub fn php_conf_d(&self, version: &str) -> PathBuf {
        self.php_dir(version).join("conf.d")
    }

    pub fn ca_dir(&self) -> PathBuf {
        self.home.join("ca")
    }

    pub fn ca_cert(&self) -> PathBuf {
        self.ca_dir().join("ca.pem")
    }

    pub fn ca_key(&self) -> PathBuf {
        self.ca_dir().join("ca-key.pem")
    }

    /// A Valet-style config directory for laravel-vite-plugin (see `certs`).
    pub fn valet_dir(&self) -> PathBuf {
        self.home.join("valet")
    }

    /// Sockets and pid files. Unix socket paths are limited to 104 bytes on
    /// macOS, so a deep `BIRD_HOME` falls back to a per-user dir in /tmp.
    pub fn run(&self) -> PathBuf {
        let run = self.home.join("run");
        if run.join("php-8.10.sock").as_os_str().len() < 100 {
            return run;
        }
        PathBuf::from(format!("/tmp/bird-{}", crate::sys::uid()))
    }

    pub fn control_socket(&self) -> PathBuf {
        self.run().join("bird.sock")
    }

    pub fn fpm_socket(&self, version: &str) -> PathBuf {
        self.run().join(format!("php-{version}.sock"))
    }

    pub fn fpm_config(&self, version: &str) -> PathBuf {
        self.run().join(format!("php-{version}-fpm.conf"))
    }

    pub fn fpm_pid(&self, version: &str) -> PathBuf {
        self.run().join(format!("php-{version}-fpm.pid"))
    }

    pub fn log(&self) -> PathBuf {
        self.home.join("log")
    }

    pub fn daemon_log(&self) -> PathBuf {
        self.log().join("bird.log")
    }

    pub fn fpm_log(&self, version: &str) -> PathBuf {
        self.log().join(format!("php-{version}-fpm.log"))
    }

    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        for dir in [
            self.home.clone(),
            self.bin(),
            self.php_root(),
            self.ca_dir(),
            self.run(),
            self.log(),
        ] {
            std::fs::create_dir_all(dir)?;
        }
        use std::os::unix::fs::PermissionsExt;
        // The run directory can be /tmp/bird-<uid> (see `run`), a name anyone
        // on the machine can create first.
        if !self.run().starts_with(&self.home) {
            check_owned_dir(&self.run())?;
        }
        // Sockets, the CA key, and logs (requests, PHP errors, Xdebug traces
        // with their arguments) are for this user only.
        for private in [self.run(), self.ca_dir(), self.log()] {
            std::fs::set_permissions(private, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_directory_must_be_our_own_real_directory() {
        let base = std::env::temp_dir().join(format!("bird-paths-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("real")).unwrap();
        assert!(check_owned_dir(&base.join("real")).is_ok());
        // A link someone else planted, even one to a directory of ours.
        std::os::unix::fs::symlink(base.join("real"), base.join("link")).unwrap();
        assert!(check_owned_dir(&base.join("link")).is_err());
        std::fs::write(base.join("file"), "").unwrap();
        assert!(check_owned_dir(&base.join("file")).is_err());
        // Root's directory is not ours (unless the tests run as root).
        if !crate::sys::is_root() {
            assert!(check_owned_dir(std::path::Path::new("/")).is_err());
        }
        std::fs::remove_dir_all(&base).unwrap();
    }
}
