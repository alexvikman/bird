//! Linux system integration: a systemd service, systemd-resolved for the TLD
//! and the CA in the system trust store and the browsers' NSS databases.
//!
//! The service runs as the user with only CAP_NET_BIND_SERVICE, so the
//! daemon can bind 127.0.0.1:80/443 itself; it drops the capability again
//! right after binding (see `daemon`).

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::config::Config;
use crate::paths::{paths, user_home};
use crate::{tls, ui};

pub const SERVICE: &str = "bird.service";
const UNIT: &str = "/etc/systemd/system/bird.service";
const RESOLVED_CONF: &str = "/etc/systemd/resolved.conf.d/bird.conf";
const MARKER: &str = "# Managed by Bird";

pub fn is_installed() -> bool {
    Path::new(UNIT).exists()
}

fn systemctl(args: &[&str]) -> Result<std::process::Output> {
    Command::new("systemctl")
        .args(args)
        .output()
        .context("could not run systemctl")
}

fn has(cmd: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {cmd}")])
        .output()
        .is_ok_and(|o| o.status.success())
}

pub fn unit(user: &str, group: &str, home: &Path) -> String {
    let p = paths();
    let path_env = format!(
        "{}:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
        p.bin().display()
    );
    format!(
        "{MARKER}\n\
         [Unit]\n\
         Description=Bird local PHP development server\n\
         After=network.target\n\
         \n\
         [Service]\n\
         User={user}\n\
         Group={group}\n\
         Environment=\"BIRD_HOME={bird_home}\"\n\
         Environment=\"HOME={home}\"\n\
         Environment=\"PATH={path_env}\"\n\
         ExecStart=\"{bin}\" daemon\n\
         Restart=always\n\
         RestartSec=1\n\
         # Only what binding 127.0.0.1:80/443 needs; dropped after binding.\n\
         AmbientCapabilities=CAP_NET_BIND_SERVICE\n\
         CapabilityBoundingSet=CAP_NET_BIND_SERVICE\n\
         NoNewPrivileges=true\n\
         StandardOutput=append:{log}\n\
         StandardError=append:{log}\n\
         \n\
         [Install]\n\
         WantedBy=multi-user.target\n",
        bird_home = p.home.display(),
        home = home.display(),
        bin = p.bird_binary().display(),
        log = p.daemon_log().display(),
    )
}

pub fn resolved_conf(cfg: &Config) -> String {
    format!(
        "{MARKER}\n\
         # Sends *.{tld} to Bird's DNS server; nothing else.\n\
         [Resolve]\n\
         DNS=127.0.0.1:{port}\n\
         Domains=~{tld}\n",
        tld = cfg.tld,
        port = cfg.dns_port
    )
}

fn resolved_active() -> bool {
    systemctl(&["is-active", "--quiet", "systemd-resolved"]).is_ok_and(|o| o.status.success())
}

/// Where the CA goes in the system trust store, and the command that
/// rebuilds the store: Debian/Ubuntu, then Fedora/RHEL/Arch.
fn trust_store() -> Option<(PathBuf, &'static str)> {
    let name = format!(
        "bird-{}",
        user_home()
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default()
    );
    if Path::new("/usr/local/share/ca-certificates").is_dir() && has("update-ca-certificates") {
        return Some((
            PathBuf::from(format!("/usr/local/share/ca-certificates/{name}.crt")),
            "update-ca-certificates",
        ));
    }
    if Path::new("/etc/pki/ca-trust/source/anchors").is_dir() && has("update-ca-trust") {
        return Some((
            PathBuf::from(format!("/etc/pki/ca-trust/source/anchors/{name}.pem")),
            "update-ca-trust",
        ));
    }
    if Path::new("/etc/ca-certificates/trust-source/anchors").is_dir() && has("update-ca-trust") {
        return Some((
            PathBuf::from(format!(
                "/etc/ca-certificates/trust-source/anchors/{name}.pem"
            )),
            "update-ca-trust",
        ));
    }
    None
}

/// Root part of `bird install` on Linux.
pub fn system_install(user: &str, group: &str, home: &Path) -> Result<()> {
    let cfg = Config::load()?;

    // DNS
    if resolved_active() {
        std::fs::create_dir_all("/etc/systemd/resolved.conf.d")?;
        std::fs::write(RESOLVED_CONF, resolved_conf(&cfg))?;
        let out = systemctl(&["restart", "systemd-resolved"])?;
        if !out.status.success() {
            bail!(
                "could not restart systemd-resolved: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        ui::ok(format!(
            "*.{} resolves via systemd-resolved ({RESOLVED_CONF})",
            cfg.tld
        ));
    } else {
        ui::warn(format!(
            "systemd-resolved is not running; send *.{tld} to 127.0.0.1:{port} in your resolver \
             (dnsmasq: server=/{tld}/127.0.0.1#{port})",
            tld = cfg.tld,
            port = cfg.dns_port
        ));
    }

    // Service
    std::fs::write(UNIT, unit(user, group, home))?;
    systemctl(&["daemon-reload"])?;
    let out = systemctl(&["enable", SERVICE])?;
    if !out.status.success() {
        bail!(
            "systemctl enable failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let out = systemctl(&["restart", SERVICE])?;
    if !out.status.success() {
        bail!(
            "could not start {SERVICE}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    ui::ok(format!("started systemd service {SERVICE}"));

    // CA in the system store (curl, PHP, most CLI tools).
    match trust_store() {
        Some((path, update)) => {
            std::fs::copy(paths().ca_cert(), &path)?;
            let ok = Command::new(update)
                .output()
                .is_ok_and(|o| o.status.success());
            if ok {
                ui::ok(format!(
                    "certificate authority trusted ({})",
                    path.display()
                ));
            } else {
                ui::warn(format!(
                    "{update} failed; the CA is in {} but may not be trusted yet",
                    path.display()
                ));
            }
        }
        None => ui::warn(format!(
            "unknown trust store; add {} to your system's CA certificates",
            paths().ca_cert().display()
        )),
    }
    Ok(())
}

pub fn system_uninstall() -> Result<()> {
    let _ = systemctl(&["disable", "--now", SERVICE]);
    let _ = std::fs::remove_file(UNIT);
    let _ = systemctl(&["daemon-reload"]);
    ui::ok(format!("removed systemd service {SERVICE}"));
    let ours = std::fs::read_to_string(RESOLVED_CONF).is_ok_and(|t| t.starts_with(MARKER));
    if ours {
        std::fs::remove_file(RESOLVED_CONF)?;
        let _ = systemctl(&["restart", "systemd-resolved"]);
        ui::ok(format!("removed {RESOLVED_CONF}"));
    }
    if let Some((path, update)) = trust_store()
        && path.exists()
    {
        std::fs::remove_file(&path)?;
        let _ = Command::new(update).output();
        ui::ok("removed the certificate authority from the system trust store");
    }
    Ok(())
}

/// NSS databases Chrome/Chromium and Firefox read (user level, no root).
fn nss_databases() -> Vec<PathBuf> {
    let home = user_home();
    let mut dbs = vec![home.join(".pki/nssdb")];
    for profiles in [
        home.join(".mozilla/firefox"),
        home.join("snap/firefox/common/.mozilla/firefox"),
        home.join(".var/app/org.mozilla.firefox/.mozilla/firefox"),
    ] {
        if let Ok(entries) = std::fs::read_dir(profiles) {
            for e in entries.flatten() {
                if e.path().join("cert9.db").exists() {
                    dbs.push(e.path());
                }
            }
        }
    }
    dbs
}

/// Adds the CA to the browsers' NSS databases (needs `certutil`, from
/// libnss3-tools / nss-tools). Browsers don't use the system store.
pub fn trust_ca_in_browsers() {
    if !has("certutil") {
        ui::warn(
            "install certutil (Debian/Ubuntu: libnss3-tools, Fedora: nss-tools) and re-run `bird install` to trust Bird's HTTPS in Chrome and Firefox",
        );
        return;
    }
    let Some(name) = tls::ca_common_name() else {
        return;
    };
    let mut trusted = 0;
    for db in nss_databases() {
        if db.ends_with(".pki/nssdb") && !db.join("cert9.db").exists() {
            let _ = std::fs::create_dir_all(&db);
            let _ = Command::new("certutil")
                .args(["-N", "--empty-password", "-d"])
                .arg(format!("sql:{}", db.display()))
                .output();
        }
        let ok = Command::new("certutil")
            .args(["-A", "-t", "C,,", "-n", &name, "-i"])
            .arg(paths().ca_cert())
            .arg("-d")
            .arg(format!("sql:{}", db.display()))
            .output()
            .is_ok_and(|o| o.status.success());
        if ok {
            trusted += 1;
        }
    }
    if trusted > 0 {
        ui::ok(format!(
            "certificate authority trusted in {trusted} browser certificate store(s)"
        ));
    }
}

pub fn untrust_ca_in_browsers(name: &str) {
    if !has("certutil") {
        return;
    }
    for db in nss_databases() {
        let _ = Command::new("certutil")
            .args(["-D", "-n", name, "-d"])
            .arg(format!("sql:{}", db.display()))
            .output();
    }
}

fn sudo_systemctl(action: &str) -> Result<()> {
    let status = Command::new("sudo")
        .args(["systemctl", action, SERVICE])
        .status()?;
    if !status.success() {
        bail!("systemctl {action} {SERVICE} failed");
    }
    Ok(())
}

pub fn start() -> Result<()> {
    sudo_systemctl("start")
}

pub fn stop() -> Result<()> {
    sudo_systemctl("stop")?;
    ui::ok("bird stopped");
    Ok(())
}

/// For `bird doctor`: whether *.tld is routed to Bird's DNS server.
pub fn dns_configured(cfg: &Config) -> (bool, String) {
    let conf = std::fs::read_to_string(RESOLVED_CONF).unwrap_or_default();
    let ok = conf.starts_with(MARKER) && conf == resolved_conf(cfg);
    let detail = if ok {
        format!("*.{} routed to Bird by systemd-resolved", cfg.tld)
    } else if !resolved_active() {
        format!(
            "systemd-resolved is not running; route *.{} to 127.0.0.1:{} yourself",
            cfg.tld, cfg.dns_port
        )
    } else {
        format!("{RESOLVED_CONF} is missing or outdated — run `bird install`")
    };
    (ok, detail)
}

/// For `bird doctor`: whether the CA is in the system trust store.
pub fn ca_trusted() -> bool {
    let Some((path, _)) = trust_store() else {
        return false;
    };
    match (std::fs::read(path), std::fs::read(paths().ca_cert())) {
        (Ok(installed), Ok(ours)) => installed == ours,
        _ => false,
    }
}

/// Opens a URL or file with the desktop's default app.
pub fn open(target: &str) -> std::io::Result<std::process::ExitStatus> {
    Command::new("xdg-open").arg(target).status()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With BIRD_TEST_SYSTEMD=1 (CI on Linux, or a container), checks the
    /// unit with `systemd-analyze verify`.
    #[test]
    fn unit_passes_systemd_analyze() {
        if std::env::var_os("BIRD_TEST_SYSTEMD").is_none_or(|v| v.is_empty()) {
            return;
        }
        let bin = paths().bird_binary();
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        if !bin.exists() {
            std::fs::write(&bin, "#!/bin/sh\n").unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let dir = std::env::temp_dir().join(format!("bird-unit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("bird.service");
        std::fs::write(&file, unit("root", "root", Path::new("/root"))).unwrap();
        let out = Command::new("systemd-analyze")
            .arg("verify")
            .arg(&file)
            .output()
            .unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.success(),
            "systemd-analyze verify failed:\n{err}"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn unit_and_resolver_config() {
        let u = unit("alex", "alex", Path::new("/home/alex"));
        assert!(u.contains("User=alex"));
        assert!(u.contains("AmbientCapabilities=CAP_NET_BIND_SERVICE"));
        assert!(u.contains("ExecStart=\"") && u.contains("\" daemon"));
        let cfg = Config::default();
        let r = resolved_conf(&cfg);
        assert!(r.contains("DNS=127.0.0.1:53535"));
        assert!(r.contains("Domains=~test"));
    }
}
