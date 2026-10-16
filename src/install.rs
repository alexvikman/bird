//! System integration: launchd service, `/etc/resolver` and CA trust.
//!
//! `bird install` runs as the user and re-invokes itself through `sudo` for
//! the few steps that need root (`bird system-install`). Nothing privileged
//! keeps running afterwards: launchd binds ports 80/443 and hands them to
//! the daemon, which runs as the user.

use std::io::Write;
use std::net::{TcpStream, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::config::Config;
use crate::paths::{paths, user_home};
use crate::ui::{self, dim};
use crate::{control, dns, php, shim, tls};

pub const LABEL: &str = "dev.bird.daemon";
const RESOLVER_MARKER: &str = "# Managed by Bird";
const SYSTEM_KEYCHAIN: &str = "/Library/Keychains/System.keychain";

fn plist_path() -> PathBuf {
    PathBuf::from(format!("/Library/LaunchDaemons/{LABEL}.plist"))
}

fn resolver_path(tld: &str) -> PathBuf {
    PathBuf::from(format!("/etc/resolver/{tld}"))
}

pub fn is_system_installed() -> bool {
    plist_path().exists()
}

fn current_user() -> Result<(String, String)> {
    let out = |args: &[&str]| -> Result<String> {
        let o = Command::new("id").args(args).output()?;
        Ok(String::from_utf8(o.stdout)?.trim().to_string())
    };
    Ok((out(&["-un"])?, out(&["-gn"])?))
}

pub fn install(user_only: bool, php_version: Option<String>) -> Result<()> {
    if unsafe { libc::geteuid() } == 0 {
        bail!("run `bird install` as your normal user; it asks for sudo when needed");
    }
    let p = paths();
    p.ensure_dirs()?;

    // A stable copy of the binary, so the launchd job and shims survive `cargo clean`.
    let exe = std::env::current_exe()?.canonicalize()?;
    if exe != p.bird_binary() {
        let tmp = p.bird_binary().with_extension("new");
        std::fs::copy(&exe, &tmp).context("could not copy the bird binary")?;
        std::fs::rename(&tmp, p.bird_binary())?;
        ui::ok(format!("installed {}", p.bird_binary().display()));
    }

    let mut cfg = Config::load()?;
    cfg.save()?;
    if tls::ensure_ca()? {
        ui::ok("created a local certificate authority");
    }

    if php::installed().is_empty() {
        let version = match php_version {
            Some(v) => php::require_normalized(&v)?,
            None => php::available(&cfg)?
                .keys()
                .max_by_key(|v| {
                    v.split('.')
                        .map(|p| p.parse::<u32>().unwrap_or(0))
                        .collect::<Vec<_>>()
                })
                .cloned()
                .context("no PHP versions available")?,
        };
        let full = php::install(&cfg, &version)?;
        ui::ok(format!("installed PHP {full}"));
        cfg.php = Some(version);
        cfg.save()?;
    } else if cfg.php.is_none() {
        cfg.php = php::installed().last().map(|i| i.minor.clone());
        cfg.save()?;
    }
    shim::refresh()?;
    if !p.composer_phar().exists() {
        match shim::install_composer() {
            Ok(()) => ui::ok("installed Composer"),
            Err(e) => ui::warn(format!(
                "could not download Composer ({e:#}); it is fetched on first use"
            )),
        }
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(p.daemon_log())?;

    if user_only {
        ui::ok("user setup done; run `bird daemon --http-port 8080 --https-port 8443` to try it");
    } else {
        println!(
            "\nThe next steps need administrator rights (DNS resolver, launchd service, trusting the CA)."
        );
        sudo_system_install()?;
        wait_for_daemon();
    }
    path_hint();
    Ok(())
}

pub fn sudo_system_install() -> Result<()> {
    let (user, group) = current_user()?;
    let status = Command::new("sudo")
        .arg("/usr/bin/env")
        .arg(format!("BIRD_HOME={}", paths().home.display()))
        .arg(paths().bird_binary())
        .arg("system-install")
        .arg("--user")
        .arg(&user)
        .arg("--group")
        .arg(&group)
        .arg("--home")
        .arg(user_home())
        .status()?;
    if !status.success() {
        bail!("system setup failed");
    }
    Ok(())
}

fn wait_for_daemon() {
    for _ in 0..50 {
        if control::is_running() {
            ui::ok("bird is running");
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    ui::warn(format!(
        "the daemon did not come up; check `bird logs` ({})",
        paths().daemon_log().display()
    ));
}

fn path_hint() {
    let bin = paths().bin();
    let on_path = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d == bin))
        .unwrap_or(false);
    if !on_path {
        println!(
            "\nAdd Bird's bin directory to the front of your PATH, e.g. in ~/.zshrc:\n\n    export PATH=\"{}:$PATH\"\n",
            bin.display()
        );
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn plist(user: &str, group: &str, home: &Path, cfg: &Config) -> String {
    let p = paths();
    let path_env = format!(
        "{}:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin",
        p.bin().display()
    );
    let socket = |name: &str, port: u16| {
        format!(
            "        <key>{name}</key>\n        <dict>\n            <key>SockNodeName</key><string>{ip}</string>\n            \
             <key>SockServiceName</key><string>{port}</string>\n            <key>SockType</key><string>stream</string>\n            \
             <key>SockFamily</key><string>IPv4</string>\n        </dict>\n",
            ip = cfg.listen
        )
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{bin}</string>
        <string>daemon</string>
    </array>
    <key>UserName</key><string>{user}</string>
    <key>GroupName</key><string>{group}</string>
    <key>EnvironmentVariables</key>
    <dict>
        <key>BIRD_HOME</key><string>{bird_home}</string>
        <key>HOME</key><string>{home}</string>
        <key>PATH</key><string>{path_env}</string>
    </dict>
    <key>Sockets</key>
    <dict>
{http}{https}    </dict>
    <key>RunAtLoad</key><true/>
    <key>KeepAlive</key><true/>
    <key>ProcessType</key><string>Interactive</string>
    <key>StandardOutPath</key><string>{log}</string>
    <key>StandardErrorPath</key><string>{log}</string>
</dict>
</plist>
"#,
        bin = xml_escape(&p.bird_binary().display().to_string()),
        user = xml_escape(user),
        group = xml_escape(group),
        bird_home = xml_escape(&p.home.display().to_string()),
        home = xml_escape(&home.display().to_string()),
        path_env = xml_escape(&path_env),
        http = socket("http", cfg.http_port),
        https = socket("https", cfg.https_port),
        log = xml_escape(&p.daemon_log().display().to_string()),
    )
}

fn launchctl(args: &[&str]) -> std::io::Result<std::process::Output> {
    Command::new("launchctl").args(args).output()
}

/// Root part of the install. Idempotent: re-running it rewrites everything.
pub fn system_install(user: &str, group: &str, home: &Path) -> Result<()> {
    if unsafe { libc::geteuid() } != 0 {
        bail!("system-install must run as root (use `bird install`)");
    }
    let cfg = Config::load()?;

    // DNS: /etc/resolver/<tld> → Bird's DNS server on an unprivileged port.
    std::fs::create_dir_all("/etc/resolver")?;
    if let Ok(entries) = std::fs::read_dir("/etc/resolver") {
        for e in entries.flatten() {
            let ours =
                std::fs::read_to_string(e.path()).is_ok_and(|t| t.starts_with(RESOLVER_MARKER));
            if ours && e.file_name() != cfg.tld.as_str() {
                std::fs::remove_file(e.path())?;
            }
        }
    }
    let resolver = resolver_path(&cfg.tld);
    if let Ok(existing) = std::fs::read_to_string(&resolver)
        && !existing.starts_with(RESOLVER_MARKER)
    {
        ui::warn(format!(
            "replacing {} (it belonged to another tool, e.g. Herd or Valet)",
            resolver.display()
        ));
    }
    std::fs::write(
        &resolver,
        format!(
            "{RESOLVER_MARKER}\nnameserver 127.0.0.1\nport {}\n",
            cfg.dns_port
        ),
    )?;
    ui::ok(format!("*.{} resolves via {}", cfg.tld, resolver.display()));

    // launchd: root binds the ports, the daemon runs as the user.
    let plist_file = plist_path();
    let _ = launchctl(&["bootout", &format!("system/{LABEL}")]);
    std::fs::write(&plist_file, plist(user, group, home, &cfg))?;
    let out = launchctl(&["bootstrap", "system", &plist_file.display().to_string()])?;
    if !out.status.success() {
        bail!(
            "launchctl bootstrap failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    ui::ok(format!("started launchd service {LABEL}"));

    // Trust the CA for TLS in the system keychain (macOS may ask for confirmation).
    let ca = paths().ca_cert();
    if ca_trusted() {
        ui::ok("certificate authority already trusted");
    } else {
        println!("Trusting Bird's certificate authority (macOS may ask you to confirm)…");
        let status = Command::new("security")
            .args([
                "add-trusted-cert",
                "-d",
                "-r",
                "trustRoot",
                "-k",
                SYSTEM_KEYCHAIN,
            ])
            .arg(&ca)
            .status()?;
        if status.success() {
            ui::ok("certificate authority trusted");
        } else {
            ui::warn("could not trust the CA; HTTPS will show certificate warnings until you do");
        }
    }
    Ok(())
}

fn ca_trusted() -> bool {
    Command::new("security")
        .args(["verify-cert", "-p", "ssl", "-c"])
        .arg(paths().ca_cert())
        .output()
        .is_ok_and(|o| o.status.success())
}

pub fn uninstall(purge: bool) -> Result<()> {
    if is_system_installed() {
        let ca_name = tls::ca_common_name();
        let mut cmd = Command::new("sudo");
        cmd.arg("/usr/bin/env")
            .arg(format!("BIRD_HOME={}", paths().home.display()))
            .arg(std::env::current_exe()?)
            .arg("system-uninstall");
        if let Some(name) = &ca_name {
            cmd.arg("--ca-name").arg(name);
        }
        if !cmd.status()?.success() {
            bail!("system uninstall failed");
        }
    }
    if purge {
        if paths().home.join("config.toml").exists() || paths().php_root().exists() {
            std::fs::remove_dir_all(&paths().home)?;
            ui::ok(format!("deleted {}", paths().home.display()));
        }
    } else {
        println!(
            "{}",
            dim(&format!(
                "kept {} (use --purge to delete it)",
                paths().home.display()
            ))
        );
    }
    Ok(())
}

pub fn system_uninstall(ca_name: Option<&str>) -> Result<()> {
    if unsafe { libc::geteuid() } != 0 {
        bail!("system-uninstall must run as root (use `bird uninstall`)");
    }
    let _ = launchctl(&["bootout", &format!("system/{LABEL}")]);
    let _ = std::fs::remove_file(plist_path());
    ui::ok("removed launchd service");
    if let Ok(entries) = std::fs::read_dir("/etc/resolver") {
        for e in entries.flatten() {
            if std::fs::read_to_string(e.path()).is_ok_and(|t| t.starts_with(RESOLVER_MARKER)) {
                std::fs::remove_file(e.path())?;
                ui::ok(format!("removed {}", e.path().display()));
            }
        }
    }
    let ca = paths().ca_cert();
    if ca.exists() {
        let _ = Command::new("security")
            .args(["remove-trusted-cert", "-d"])
            .arg(&ca)
            .status();
    }
    if let Some(name) = ca_name {
        let deleted = Command::new("security")
            .args(["delete-certificate", "-c", name, SYSTEM_KEYCHAIN])
            .output()
            .is_ok_and(|o| o.status.success());
        if deleted {
            ui::ok("removed the certificate authority from the system keychain");
        }
    }
    Ok(())
}

pub fn start() -> Result<()> {
    if !is_system_installed() {
        bail!(
            "Bird is not installed as a service. Run `bird install` (or `bird daemon` in the foreground)"
        );
    }
    let status = Command::new("sudo")
        .args(["launchctl", "bootstrap", "system"])
        .arg(plist_path())
        .status()?;
    if !status.success() && !control::is_running() {
        bail!("could not start the service");
    }
    wait_for_daemon();
    Ok(())
}

pub fn stop() -> Result<()> {
    if !is_system_installed() {
        if control::is_running() {
            control::send(serde_json::json!({"cmd": "shutdown"}))?;
            ui::ok("stopped the foreground daemon");
            return Ok(());
        }
        bail!("Bird is not running");
    }
    let status = Command::new("sudo")
        .args(["launchctl", "bootout", &format!("system/{LABEL}")])
        .status()?;
    if !status.success() {
        bail!("could not stop the service (is it running?)");
    }
    ui::ok("bird stopped");
    Ok(())
}

// ---------------------------------------------------------------------------

pub fn doctor() -> Result<()> {
    let mut cfg = Config::load()?;
    // The daemon may listen on other ports than configured (`bird daemon --http-port`).
    let status = control::send(serde_json::json!({"cmd": "status"})).ok();
    if let Some(s) = &status {
        let port = |key: &str, default: u16| s[key].as_u64().map_or(default, |p| p as u16);
        cfg.http_port = port("http_port", cfg.http_port);
        cfg.https_port = port("https_port", cfg.https_port);
    }
    let mut problems = 0;
    let mut check = |ok: bool, good: String, bad: String| {
        if ok {
            ui::ok(good);
        } else {
            problems += 1;
            ui::fail(bad);
        }
    };

    let installed = php::installed();
    check(
        !installed.is_empty(),
        format!(
            "PHP installed: {}",
            installed
                .iter()
                .map(|i| i.version.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        "no PHP installed — run `bird php:install 8.4`".into(),
    );
    match &cfg.php {
        Some(v) => check(
            php::is_installed(v),
            format!("global PHP {v}"),
            format!("global PHP {v} is not installed"),
        ),
        None => check(
            false,
            String::new(),
            "no global PHP version — run `bird use 8.4`".into(),
        ),
    }

    let which_php = Command::new("sh")
        .args(["-c", "command -v php"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let shim = paths().bin().join("php");
    check(
        Path::new(&which_php) == shim,
        format!("`php` on PATH is Bird's ({which_php})"),
        if which_php.is_empty() {
            format!(
                "`php` is not on PATH — add {} to the front of PATH",
                paths().bin().display()
            )
        } else {
            format!(
                "`php` on PATH is {which_php}, not Bird's — put {} first in PATH",
                paths().bin().display()
            )
        },
    );

    let daemon = status.is_some();
    check(
        daemon,
        "daemon running".into(),
        "daemon not running — `bird start` or `bird install`".into(),
    );

    let resolver = std::fs::read_to_string(resolver_path(&cfg.tld)).unwrap_or_default();
    check(
        resolver.starts_with(RESOLVER_MARKER)
            && resolver.contains(&format!("port {}", cfg.dns_port)),
        format!("/etc/resolver/{} points to Bird", cfg.tld),
        if resolver.is_empty() {
            format!("/etc/resolver/{} is missing — run `bird install`", cfg.tld)
        } else {
            format!(
                "/etc/resolver/{} belongs to another tool (Herd/Valet?) — run `bird install`",
                cfg.tld
            )
        },
    );

    if daemon {
        check(
            dns_works(&cfg),
            format!(
                "DNS answers for *.{} on 127.0.0.1:{}",
                cfg.tld, cfg.dns_port
            ),
            format!("no DNS answer on 127.0.0.1:{}", cfg.dns_port),
        );
        for (name, port) in [("HTTP", cfg.http_port), ("HTTPS", cfg.https_port)] {
            let up =
                TcpStream::connect_timeout(&(cfg.listen, port).into(), Duration::from_millis(500))
                    .is_ok();
            check(
                up,
                format!("{name} listening on {}:{port}", cfg.listen),
                format!("nothing listening on {}:{port}", cfg.listen),
            );
        }
    }

    check(
        paths().ca_cert().exists() && ca_trusted(),
        "certificate authority trusted".into(),
        "certificate authority not trusted — run `bird install`".into(),
    );

    if Path::new("/Library/LaunchDaemons/de.beyondco.herd.helper.plist").exists() {
        ui::warn(
            "Laravel Herd is installed; quit Herd so it doesn't compete for ports 80/443 and /etc/resolver",
        );
    }
    println!();
    if problems == 0 {
        ui::ok("all good");
    } else {
        println!("{problems} problem(s) found");
    }
    std::io::stdout().flush().ok();
    Ok(())
}

fn dns_works(cfg: &Config) -> bool {
    let Ok(sock) = UdpSocket::bind("127.0.0.1:0") else {
        return false;
    };
    let _ = sock.set_read_timeout(Some(Duration::from_millis(500)));
    let mut q = vec![0x12, 0x34, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0];
    for label in ["bird-doctor", cfg.tld.as_str()] {
        q.push(label.len() as u8);
        q.extend_from_slice(label.as_bytes());
    }
    q.extend_from_slice(&[0, 0, 1, 0, 1]);
    if sock.send_to(&q, ("127.0.0.1", cfg.dns_port)).is_err() {
        return false;
    }
    let mut buf = [0u8; 512];
    match sock.recv(&mut buf) {
        Ok(n) => {
            dns::answer(&q, &cfg.tld, cfg.listen).is_some_and(|expected| buf[..n] == expected[..])
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launchd_plist_is_valid() {
        let cfg = Config::default();
        let xml = plist("someone", "staff", Path::new("/Users/some & one"), &cfg);
        assert!(xml.contains("<string>/Users/some &amp; one</string>"));
        assert!(xml.contains("<key>SockServiceName</key><string>443</string>"));
        let file = std::env::temp_dir().join(format!("bird-plist-{}.plist", std::process::id()));
        std::fs::write(&file, xml).unwrap();
        if Path::new("/usr/bin/plutil").exists() {
            let out = Command::new("/usr/bin/plutil")
                .arg("-lint")
                .arg(&file)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stdout)
            );
        }
        std::fs::remove_file(file).ok();
    }
}
