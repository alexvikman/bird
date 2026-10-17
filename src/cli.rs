use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use serde_json::json;

use crate::config::{Config, ProxyConfig};
use crate::paths::paths;
use crate::sites::{self, SiteKind};
use crate::ui::{self, bold, dim, green, yellow};
use crate::{control, herd, install, php, proxy, shim};

#[derive(Parser)]
#[command(
    name = "bird",
    version,
    about = "Fast, minimal local PHP development: *.test sites, HTTPS and multiple PHP versions."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Set Bird up: CA, PHP, shims, DNS resolver and the background service (asks for sudo)
    Install {
        /// Only set up ~/.bird; skip everything that needs sudo
        #[arg(long)]
        user_only: bool,
        /// PHP version to install if none is installed yet (default: latest)
        #[arg(long)]
        php: Option<String>,
    },
    /// Remove the background service, DNS resolver and CA trust (asks for sudo)
    Uninstall {
        /// Also delete ~/.bird (PHP versions, config, CA)
        #[arg(long)]
        purge: bool,
    },
    /// Check that everything is wired up correctly
    Doctor,
    /// Update Bird (and Bird.app) to the latest release
    SelfUpdate {
        /// Only check whether a newer release exists
        #[arg(long)]
        check: bool,
        /// With --check: print the result as JSON (used by the macOS app)
        #[arg(long, requires = "check")]
        json: bool,
    },
    /// Show the daemon state and running PHP pools
    Status,
    /// Show which migrations of ~/.bird have run (they run automatically)
    Migrate {
        /// Only list applied and pending migrations
        #[arg(long)]
        status: bool,
    },
    /// Start the background service
    Start,
    /// Stop the background service
    Stop,
    /// Restart PHP-FPM pools (all, or one version) and reload the config
    Restart {
        /// Only this PHP version
        version: Option<String>,
        /// Restart the whole daemon instead
        #[arg(long)]
        daemon: bool,
    },

    /// Serve every subdirectory of PATH (default: current directory) as <dir>.test
    Park { path: Option<PathBuf> },
    /// Stop serving a parked directory
    Forget { path: Option<PathBuf> },
    /// List parked directories
    Paths,
    /// Serve the current directory as NAME.test (default: directory name)
    Link {
        name: Option<String>,
        /// Redirect HTTP to HTTPS
        #[arg(long)]
        secure: bool,
        /// Pin a PHP version for this site
        #[arg(long, value_name = "VERSION")]
        isolate: Option<String>,
    },
    /// Stop serving a linked site
    Unlink { name: Option<String> },
    /// List all sites and proxies
    #[command(visible_aliases = ["links", "parked"])]
    Sites,
    /// Redirect HTTP to HTTPS for a site (HTTPS always works)
    Secure { name: Option<String> },
    /// Stop redirecting HTTP to HTTPS
    Unsecure { name: Option<String> },
    /// Serve NAME.test as a reverse proxy, e.g. `bird proxy vite http://127.0.0.1:5173`
    Proxy {
        name: String,
        upstream: String,
        #[arg(long)]
        secure: bool,
    },
    /// Remove a proxy
    Unproxy { name: String },
    /// Open a site in the browser
    Open { name: Option<String> },

    /// Set the global PHP version (installs it if needed)
    Use { version: String },
    /// Pin a PHP version for the current (or given) site
    Isolate {
        version: String,
        #[arg(long)]
        site: Option<String>,
    },
    /// Remove a site's PHP pin
    Unisolate {
        #[arg(long)]
        site: Option<String>,
    },
    /// Turn opcache on or off for all PHP versions (no argument: show the state)
    Opcache {
        #[arg(value_parser = ["on", "off"])]
        state: Option<String>,
    },
    /// Show which PHP binary serves the current directory, and why
    WhichPhp,
    /// Install PHP versions, e.g. `bird php:install 8.3 8.4`
    #[command(name = "php:install")]
    PhpInstall {
        #[arg(required = true)]
        versions: Vec<String>,
    },
    /// List installed (and with --available, downloadable) PHP versions
    #[command(name = "php:list")]
    PhpList {
        #[arg(long)]
        available: bool,
    },
    /// Update installed PHP versions to their latest patch release
    #[command(name = "php:update")]
    PhpUpdate { versions: Vec<String> },
    /// Remove a PHP version
    #[command(name = "php:remove")]
    PhpRemove { version: String },
    /// Print (or --edit) the php.ini of a PHP version
    Ini {
        version: Option<String>,
        #[arg(long)]
        edit: bool,
    },
    /// Run PHP with the version for the current directory
    #[command(disable_help_flag = true)]
    Php {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<OsString>,
    },
    /// Run PHP with Xdebug and start a debug session, e.g. `bird debug artisan migrate`
    #[command(disable_help_flag = true)]
    Debug {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<OsString>,
    },
    /// Run PHP with PCOV for code coverage, e.g. `bird coverage vendor/bin/pest --coverage`
    #[command(disable_help_flag = true)]
    Coverage {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<OsString>,
    },
    /// When web requests run with Xdebug: auto (on a trigger, default), on or off
    Xdebug {
        #[arg(value_parser = ["auto", "on", "off"])]
        mode: Option<String>,
    },
    /// Run Composer with the PHP version for the current directory
    #[command(disable_help_flag = true)]
    Composer {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<OsString>,
    },

    /// Show or change the top-level domain (default: test)
    Tld { tld: Option<String> },
    /// Tail a log: `bird` (default) or a PHP version for its FPM log
    Logs {
        which: Option<String>,
        #[arg(short, long)]
        follow: bool,
    },
    /// Import sites, parked paths and PHP pins from another tool
    Import {
        #[arg(value_parser = ["herd"])]
        from: String,
        /// Also install the PHP versions the imported sites need
        #[arg(long)]
        install_php: bool,
    },

    /// Run the server in the foreground (used by launchd)
    Daemon {
        #[arg(long)]
        http_port: Option<u16>,
        #[arg(long)]
        https_port: Option<u16>,
    },
    /// Print everything about this Bird as JSON (used by the menu bar app)
    #[command(hide = true)]
    State {
        /// Also list the PHP versions available for download (needs network)
        #[arg(long)]
        available: bool,
    },
    #[command(hide = true)]
    SystemInstall {
        #[arg(long)]
        user: String,
        #[arg(long)]
        group: String,
        #[arg(long)]
        home: PathBuf,
    },
    #[command(hide = true)]
    SystemUninstall {
        #[arg(long)]
        ca_name: Option<String>,
    },
}

pub fn main() -> Result<()> {
    let cli = Cli::parse();
    // Bring ~/.bird up to date with this version before doing anything else.
    // Not for commands that run as root, the daemon (it migrates on start),
    // install (it migrates once ~/.bird exists) or migrate itself.
    let migrates_itself = matches!(
        cli.command,
        Command::Daemon { .. }
            | Command::SystemInstall { .. }
            | Command::SystemUninstall { .. }
            | Command::Install { .. }
            | Command::Uninstall { .. }
            | Command::Migrate { .. }
    );
    if !migrates_itself {
        crate::migrate::run_for_cli()?;
    }
    match cli.command {
        Command::Install { user_only, php } => install::install(user_only, php),
        Command::Uninstall { purge } => install::uninstall(purge),
        Command::Doctor => install::doctor(),
        Command::SelfUpdate {
            check: true,
            json: true,
        } => crate::update::check_json(),
        Command::SelfUpdate { check, .. } => crate::update::self_update(check),
        Command::Status => status(),
        Command::Migrate { status } => {
            if !status {
                crate::migrate::run_for_cli()?;
            }
            crate::migrate::print_status()
        }
        Command::Start => install::start(),
        Command::Stop => install::stop(),
        Command::Restart { version, daemon } => restart(version, daemon),

        Command::Park { path } => park(path),
        Command::Forget { path } => forget(path),
        Command::Paths => {
            for p in Config::load()?.paths {
                println!("{}", p.display());
            }
            Ok(())
        }
        Command::Link {
            name,
            secure,
            isolate,
        } => link(name, secure, isolate),
        Command::Unlink { name } => unlink(name),
        Command::Sites => list_sites(),
        Command::Secure { name } => set_secure(name, true),
        Command::Unsecure { name } => set_secure(name, false),
        Command::Proxy {
            name,
            upstream,
            secure,
        } => add_proxy(name, upstream, secure),
        Command::Unproxy { name } => {
            let mut cfg = Config::load()?;
            if cfg.proxies.remove(&name).is_none() {
                bail!("no proxy named {name}");
            }
            cfg.save()?;
            ui::ok(format!("removed proxy {}", cfg.domain(&name)));
            Ok(())
        }
        Command::Open { name } => open(name),

        Command::Use { version } => use_version(&version),
        Command::Isolate { version, site } => isolate(&version, site),
        Command::Unisolate { site } => unisolate(site),
        Command::Opcache { state } => opcache(state.as_deref()),
        Command::WhichPhp => which_php(),
        Command::PhpInstall { versions } => php_install(&versions),
        Command::PhpList { available } => php_list(available),
        Command::PhpUpdate { versions } => php_update(&versions),
        Command::PhpRemove { version } => php_remove(&version),
        Command::Ini { version, edit } => ini(version, edit),
        Command::Php { args } => shim::exec_php(&shim::current_version()?, args),
        Command::Composer { args } => shim::exec_composer(&shim::current_version()?, args),
        Command::Debug { args } => shim::exec_php_xdebug(&shim::current_version()?, args),
        Command::Coverage { args } => shim::exec_php_coverage(&shim::current_version()?, args),
        Command::Xdebug { mode } => xdebug(mode.as_deref()),

        Command::Tld { tld } => set_tld(tld),
        Command::Logs { which, follow } => logs(which, follow),
        Command::Import { from, install_php } => {
            debug_assert_eq!(from, "herd");
            herd::import(install_php)
        }

        Command::Daemon {
            http_port,
            https_port,
        } => crate::daemon::run(http_port, https_port),
        Command::State { available } => state(available),
        Command::SystemInstall { user, group, home } => {
            install::system_install(&user, &group, &home)
        }
        Command::SystemUninstall { ca_name } => install::system_uninstall(ca_name.as_deref()),
    }
}

fn cwd() -> Result<PathBuf> {
    std::env::current_dir().context("could not determine the current directory")
}

/// The site a command applies to: the given name, or the site containing the current directory.
fn target_site(cfg: &Config, name: Option<String>) -> Result<String> {
    if let Some(name) = name {
        let name = sites::slug(name.trim_end_matches(&format!(".{}", cfg.tld)));
        if sites::find_site(cfg, &name).is_none() {
            bail!("no site named {name} (see `bird sites`)");
        }
        return Ok(name);
    }
    match sites::site_for_dir(cfg, &cwd()?) {
        Some(site) => Ok(site.name),
        None => {
            bail!("the current directory is not a Bird site; run `bird link` or pass a site name")
        }
    }
}

fn status() -> Result<()> {
    let mut cfg = Config::load()?;
    match control::send(json!({"cmd": "status"})) {
        Ok(s) => {
            let port = |key: &str, default: u16| s[key].as_u64().map_or(default, |p| p as u16);
            cfg.http_port = port("http_port", cfg.http_port);
            cfg.https_port = port("https_port", cfg.https_port);
            ui::ok(format!(
                "bird {} running (pid {}, up {})",
                s["version"].as_str().unwrap_or("?"),
                s["pid"],
                human_duration(s["uptime_secs"].as_u64().unwrap_or(0))
            ));
            let pools = s["fpm"].as_array().cloned().unwrap_or_default();
            if pools.is_empty() {
                println!(
                    "  {}",
                    dim("no PHP-FPM pools running (they start on the first request)")
                );
            }
            for p in pools {
                println!(
                    "  php-fpm {}  pid {}  idle {}",
                    p["version"].as_str().unwrap_or("?"),
                    p["pid"],
                    human_duration(p["idle_secs"].as_u64().unwrap_or(0))
                );
            }
        }
        Err(_) => ui::fail("bird daemon is not running (start it with `bird start`)"),
    }
    println!(
        "  tld .{} · global PHP {} · http :{} · https :{}",
        cfg.tld,
        cfg.php.as_deref().unwrap_or("none"),
        cfg.http_port,
        cfg.https_port
    );
    Ok(())
}

fn human_duration(secs: u64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86400 => format!("{}h {}m", s / 3600, s % 3600 / 60),
        s => format!("{}d {}h", s / 86400, s % 86400 / 3600),
    }
}

fn restart(version: Option<String>, daemon: bool) -> Result<()> {
    if daemon {
        let launchd = install::running_as_service().context("the bird daemon is not running")?;
        control::send(json!({"cmd": "shutdown"}))?;
        if launchd {
            ui::ok("daemon restarting (launchd starts it again)");
        } else {
            ui::warn("stopped the foreground daemon; start it again with `bird daemon`");
        }
        return Ok(());
    }
    let version = version.map(|v| php::require_normalized(&v)).transpose()?;
    let reply = control::send(json!({"cmd": "restart-php", "version": version}))?;
    let stopped: Vec<&str> = reply["stopped"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    if stopped.is_empty() {
        ui::ok("config reloaded; no PHP pools were running");
    } else {
        ui::ok(format!(
            "restarted PHP {} (pools start again on the next request)",
            stopped.join(", ")
        ));
    }
    Ok(())
}

fn absolute(path: Option<PathBuf>) -> Result<PathBuf> {
    let path = path.unwrap_or(cwd()?);
    path.canonicalize()
        .with_context(|| format!("{} does not exist", path.display()))
}

fn park(path: Option<PathBuf>) -> Result<()> {
    let path = absolute(path)?;
    let mut cfg = Config::load()?;
    if !cfg.paths.contains(&path) {
        cfg.paths.push(path.clone());
        cfg.save()?;
    }
    let count = sites::all_sites(&cfg)
        .iter()
        .filter(|s| s.path.parent() == Some(&path))
        .count();
    ui::ok(format!(
        "parked {} ({count} sites, served as <dir>.{})",
        path.display(),
        cfg.tld
    ));
    Ok(())
}

fn forget(path: Option<PathBuf>) -> Result<()> {
    let path = absolute(path)?;
    let mut cfg = Config::load()?;
    let before = cfg.paths.len();
    cfg.paths.retain(|p| p != &path);
    if cfg.paths.len() == before {
        bail!("{} is not parked", path.display());
    }
    cfg.save()?;
    ui::ok(format!("forgot {}", path.display()));
    Ok(())
}

fn link(name: Option<String>, secure: bool, isolate: Option<String>) -> Result<()> {
    let dir = absolute(None)?;
    let mut cfg = Config::load()?;
    let name = sites::slug(&name.unwrap_or_else(|| {
        dir.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string()
    }));
    if name.is_empty() {
        bail!("could not derive a site name; pass one: bird link myapp");
    }
    if cfg.proxies.contains_key(&name) {
        bail!("{} is already a proxy", cfg.domain(&name));
    }
    let isolate = isolate.map(|v| php::require_normalized(&v)).transpose()?;
    if let Some(v) = &isolate {
        ensure_php(&cfg, v)?;
    }
    let site = cfg.site_mut(&name);
    site.path = Some(dir.clone());
    site.secure |= secure;
    if isolate.is_some() {
        site.php = isolate;
    }
    cfg.save()?;
    ui::ok(format!(
        "{} → {}",
        bold(&format!("https://{}", cfg.domain(&name))),
        dir.display()
    ));
    Ok(())
}

fn unlink(name: Option<String>) -> Result<()> {
    let mut cfg = Config::load()?;
    let name = match name {
        Some(n) => sites::slug(&n),
        None => {
            let dir = absolute(None)?;
            cfg.sites
                .iter()
                .find(|(_, s)| s.path.as_deref() == Some(dir.as_path()))
                .map(|(n, _)| n.clone())
                .context("the current directory is not linked")?
        }
    };
    match cfg.sites.get_mut(&name) {
        Some(s) if s.path.is_some() => s.path = None,
        _ => bail!("{name} is not a linked site"),
    }
    // Leftover pins/secure flags stay in case it's also a parked site; save() drops empty entries.
    cfg.save()?;
    ui::ok(format!("unlinked {}", cfg.domain(&name)));
    Ok(())
}

/// Machine-readable state for the menu bar app. Works with the daemon
/// stopped (then `daemon` is null); actions go through the normal commands.
fn state(available: bool) -> Result<()> {
    use crate::config::XdebugMode;
    let cfg = Config::load()?;
    let daemon = control::send(json!({"cmd": "status"})).ok();
    let scheme = |secure: bool| if secure { "https" } else { "http" };
    let installed = php::installed();
    let sites: Vec<serde_json::Value> = sites::all_sites(&cfg)
        .into_iter()
        .map(|s| {
            let (php_version, php_source) = match s.php_version(&cfg) {
                Some((v, sites::PhpSource::Isolated)) => (Some(v), "isolated"),
                Some((v, sites::PhpSource::PhpVersionFile)) => (Some(v), "php-version-file"),
                Some((v, sites::PhpSource::Global)) => (Some(v), "global"),
                None => (None, "none"),
            };
            let domain = cfg.domain(&s.name);
            json!({
                "name": s.name,
                "domain": domain,
                // HTTPS always works; `secure` only adds the HTTP redirect.
                "url": format!("https://{domain}"),
                "path": s.path,
                "kind": match s.kind { SiteKind::Linked => "linked", SiteKind::Parked => "parked" },
                "secure": s.secure,
                "php": php_version,
                "php_source": php_source,
                "php_installed": php_version.as_deref().is_some_and(php::is_installed),
            })
        })
        .collect();
    let proxies: Vec<serde_json::Value> = cfg
        .proxies
        .iter()
        .map(|(name, p)| {
            let domain = cfg.domain(name);
            json!({
                "name": name,
                "domain": domain,
                "url": format!("{}://{domain}", scheme(true)),
                "upstream": p.upstream,
                "secure": p.secure,
            })
        })
        .collect();
    // Remote builds only when asked: it fetches the manifest.
    let remote = if available {
        Some(php::available_builds(&cfg))
    } else {
        None
    };
    let source = php::mirror_source(&cfg);
    let versions: Vec<serde_json::Value> = installed
        .iter()
        .map(|i| {
            let latest = remote
                .as_ref()
                .and_then(|r| r.as_ref().ok())
                .and_then(|r| r.get(&i.minor));
            json!({
                "minor": i.minor,
                "version": i.version,
                "source": i.source,
                "xdebug": php::has_xdebug(&i.minor),
                "update": latest.filter(|l| i.outdated(l, source)).map(|l| l.version.clone()),
            })
        })
        .collect();
    let (php_available, available_error) = match &remote {
        Some(Ok(r)) => (
            Some(
                r.iter()
                    .map(|(minor, b)| json!({"minor": minor, "version": b.version}))
                    .collect::<Vec<_>>(),
            ),
            None,
        ),
        Some(Err(e)) => (None, Some(format!("{e:#}"))),
        None => (None, None),
    };
    let out = json!({
        "bird_version": env!("CARGO_PKG_VERSION"),
        "bird_home": paths().home,
        "log": paths().daemon_log(),
        "service_installed": install::is_system_installed(),
        "daemon": daemon,
        "tld": cfg.tld,
        "php": cfg.php,
        "opcache": cfg.opcache,
        "xdebug": match cfg.xdebug {
            XdebugMode::Auto => "auto",
            XdebugMode::On => "on",
            XdebugMode::Off => "off",
        },
        "php_versions": versions,
        "php_available": php_available,
        "php_available_error": available_error,
        "paths": cfg.paths,
        "sites": sites,
        "proxies": proxies,
    });
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

fn list_sites() -> Result<()> {
    let cfg = Config::load()?;
    let all = sites::all_sites(&cfg);
    if all.is_empty() && cfg.proxies.is_empty() {
        println!(
            "No sites yet. Run `bird park` in your projects folder, or `bird link` in a project."
        );
        return Ok(());
    }
    let rows: Vec<Vec<String>> = all
        .iter()
        .map(|s| {
            let php = match s.php_version(&cfg) {
                Some((v, sites::PhpSource::Isolated)) => format!("{v} {}", dim("(isolated)")),
                Some((v, sites::PhpSource::PhpVersionFile)) => {
                    format!("{v} {}", dim("(.php-version)"))
                }
                Some((v, sites::PhpSource::Global)) => v,
                None => yellow("none"),
            };
            let php = match s.php_version(&cfg) {
                Some((v, _)) if !php::is_installed(&v) => {
                    format!("{php} {}", yellow("not installed"))
                }
                _ => php,
            };
            let scheme = if s.secure {
                green("https")
            } else {
                "http".into()
            };
            let kind = match s.kind {
                SiteKind::Linked => "linked",
                SiteKind::Parked => "parked",
            };
            vec![
                format!("{scheme}://{}", cfg.domain(&s.name)),
                php,
                kind.into(),
                s.path.display().to_string(),
            ]
        })
        .collect();
    ui::table(&["SITE", "PHP", "TYPE", "PATH"], &rows);
    if !cfg.proxies.is_empty() {
        println!();
        let rows: Vec<Vec<String>> = cfg
            .proxies
            .iter()
            .map(|(n, p)| {
                let scheme = if p.secure {
                    green("https")
                } else {
                    "http".into()
                };
                vec![format!("{scheme}://{}", cfg.domain(n)), p.upstream.clone()]
            })
            .collect();
        ui::table(&["PROXY", "UPSTREAM"], &rows);
    }
    Ok(())
}

fn set_secure(name: Option<String>, secure: bool) -> Result<()> {
    let mut cfg = Config::load()?;
    let name = match name.as_deref().map(sites::slug) {
        Some(n) if cfg.proxies.contains_key(&n) => {
            cfg.proxies.get_mut(&n).unwrap().secure = secure;
            cfg.save()?;
            ui::ok(format!(
                "{} {}",
                cfg.domain(&n),
                if secure {
                    "redirects to HTTPS"
                } else {
                    "served over HTTP and HTTPS"
                }
            ));
            return Ok(());
        }
        _ => target_site(&cfg, name)?,
    };
    cfg.site_mut(&name).secure = secure;
    cfg.save()?;
    if secure {
        ui::ok(format!(
            "https://{} (HTTP now redirects)",
            cfg.domain(&name)
        ));
    } else {
        ui::ok(format!(
            "{} is served over both HTTP and HTTPS",
            cfg.domain(&name)
        ));
    }
    Ok(())
}

fn add_proxy(name: String, upstream: String, secure: bool) -> Result<()> {
    proxy::parse_upstream(&upstream)?;
    let name = sites::slug(&name);
    let mut cfg = Config::load()?;
    cfg.proxies.insert(
        name.clone(),
        ProxyConfig {
            upstream: upstream.clone(),
            secure,
        },
    );
    cfg.save()?;
    ui::ok(format!("https://{} → {upstream}", cfg.domain(&name)));
    Ok(())
}

fn open(name: Option<String>) -> Result<()> {
    let cfg = Config::load()?;
    let name = target_site(&cfg, name)?;
    let url = format!("https://{}", cfg.domain(&name));
    if cfg!(target_os = "linux") {
        crate::linux::open(&url)?;
    } else {
        std::process::Command::new("open").arg(&url).status()?;
    }
    Ok(())
}

/// Installs a PHP version if it isn't there yet.
fn ensure_php(cfg: &Config, version: &str) -> Result<()> {
    if !php::is_installed(version) {
        println!("PHP {version} is not installed yet; installing…");
        let full = php::install(cfg, version)?;
        shim::refresh()?;
        ui::ok(format!("installed PHP {full}"));
    }
    Ok(())
}

fn use_version(version: &str) -> Result<()> {
    let version = php::require_normalized(version)?;
    let mut cfg = Config::load()?;
    ensure_php(&cfg, &version)?;
    cfg.php = Some(version.clone());
    cfg.save()?;
    shim::refresh()?;
    ui::ok(format!("global PHP is now {version}"));
    Ok(())
}

fn isolate(version: &str, site: Option<String>) -> Result<()> {
    let version = php::require_normalized(version)?;
    let mut cfg = Config::load()?;
    let name = target_site(&cfg, site)?;
    ensure_php(&cfg, &version)?;
    cfg.site_mut(&name).php = Some(version.clone());
    cfg.save()?;
    ui::ok(format!("{} uses PHP {version}", cfg.domain(&name)));
    Ok(())
}

fn unisolate(site: Option<String>) -> Result<()> {
    let mut cfg = Config::load()?;
    let name = target_site(&cfg, site)?;
    cfg.site_mut(&name).php = None;
    cfg.save()?;
    ui::ok(format!(
        "{} uses the global PHP version again",
        cfg.domain(&name)
    ));
    Ok(())
}

fn opcache(state: Option<&str>) -> Result<()> {
    let mut cfg = Config::load()?;
    if let Some(state) = state {
        cfg.opcache = state == "on";
        cfg.save()?;
        for i in php::installed() {
            php::write_managed_ini(&i.minor, cfg.opcache)?;
        }
        if control::is_running() {
            control::send(json!({"cmd": "restart-php"}))?;
        }
        if cfg.opcache {
            ui::ok("opcache is on (files are revalidated on every request)");
        } else {
            ui::ok("opcache is off");
        }
    } else {
        println!(
            "opcache is {}",
            if cfg.opcache {
                green("on")
            } else {
                yellow("off")
            }
        );
    }
    for i in php::installed() {
        if !php::has_opcache(&i.minor) {
            ui::warn(format!(
                "PHP {} ({}) has no opcache compiled in; `bird php:update {}` installs Bird's build",
                i.version, i.source, i.minor
            ));
        }
    }
    Ok(())
}

fn xdebug(mode: Option<&str>) -> Result<()> {
    use crate::config::XdebugMode;
    let mut cfg = Config::load()?;
    if let Some(mode) = mode {
        cfg.xdebug = match mode {
            "on" => XdebugMode::On,
            "off" => XdebugMode::Off,
            _ => XdebugMode::Auto,
        };
        cfg.save()?;
        for i in php::installed() {
            if php::has_xdebug(&i.minor) {
                php::write_xdebug_ini(&i.minor, cfg.xdebug == XdebugMode::On)?;
            }
        }
        if control::is_running() {
            control::send(json!({"cmd": "restart-php"}))?;
        }
    }
    match cfg.xdebug {
        XdebugMode::Auto => {
            println!(
                "xdebug is {}: requests with an Xdebug trigger run with Xdebug, all others without",
                green("auto")
            );
            println!(
                "  {}",
                dim(
                    "trigger: the Xdebug browser extension (XDEBUG_SESSION cookie) or ?XDEBUG_SESSION=1 in the URL"
                )
            );
        }
        XdebugMode::On => println!(
            "xdebug is {}: every request runs with Xdebug and starts a debug session",
            yellow("on")
        ),
        XdebugMode::Off => println!("xdebug is {}", dim("off")),
    }
    if cfg.xdebug != XdebugMode::Off {
        println!(
            "  {}",
            dim("your IDE listens on port 9003; profiles and traces go to ~/.bird/log/xdebug")
        );
    }
    for i in php::installed() {
        if !php::has_xdebug(&i.minor) {
            ui::warn(format!(
                "PHP {} ({}) has no Xdebug; `bird php:update {}` installs Bird's build",
                i.version, i.source, i.minor
            ));
        }
    }
    Ok(())
}

fn which_php() -> Result<()> {
    let cfg = Config::load()?;
    match php::resolve_for_dir(&cfg, &cwd()?) {
        Some((v, why)) => {
            let bin = paths().php_binary(&v);
            println!("{}", bin.display());
            let note = if bin.is_file() {
                String::new()
            } else {
                format!(" — {}", yellow("not installed"))
            };
            eprintln!("{}", dim(&format!("PHP {v} ({why}){note}")));
            Ok(())
        }
        None => bail!("no PHP version configured; run `bird use 8.4`"),
    }
}

fn php_install(versions: &[String]) -> Result<()> {
    let mut cfg = Config::load()?;
    for v in versions {
        let v = php::require_normalized(v)?;
        let full = php::install(&cfg, &v)?;
        ui::ok(format!("installed PHP {full}"));
        if cfg.php.is_none() {
            cfg.php = Some(v.clone());
            cfg.save()?;
            ui::ok(format!("global PHP is now {v}"));
        }
    }
    shim::refresh()
}

fn php_list(available: bool) -> Result<()> {
    let cfg = Config::load()?;
    let installed = php::installed();
    let remote = if available {
        Some(php::available_builds(&cfg)?)
    } else {
        None
    };
    let mut versions: Vec<String> = installed.iter().map(|i| i.minor.clone()).collect();
    if let Some(r) = &remote {
        versions.extend(r.keys().cloned());
    }
    versions.sort_by_key(|v| {
        v.split('.')
            .map(|p| p.parse::<u32>().unwrap_or(0))
            .collect::<Vec<_>>()
    });
    versions.dedup();
    if versions.is_empty() {
        println!("No PHP versions installed. Install one with `bird php:install 8.4`.");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = versions
        .iter()
        .map(|v| {
            let inst = installed.iter().find(|i| &i.minor == v);
            let marker = if cfg.php.as_deref() == Some(v) {
                green("● global")
            } else {
                String::new()
            };
            let version = match inst {
                Some(i) if i.source != php::SOURCE_BIRD => {
                    format!("{} {}", i.version, dim(&format!("({})", i.source)))
                }
                Some(i) => i.version.clone(),
                None => dim("—"),
            };
            let mut row = vec![v.clone(), version, marker];
            if let Some(r) = &remote {
                let source = php::mirror_source(&cfg);
                let update = match (inst, r.get(v)) {
                    (Some(i), Some(latest)) if i.version != latest.version => {
                        yellow(&format!("{} available", latest.version))
                    }
                    (Some(i), Some(latest)) if i.source != source => {
                        yellow(&format!("{} ({source}) available", latest.version))
                    }
                    (Some(i), Some(latest)) if i.outdated(latest, source) => {
                        yellow(&format!("{} (new build) available", latest.version))
                    }
                    (_, Some(latest)) => dim(&latest.version),
                    (_, None) => String::new(),
                };
                row.push(update);
            }
            row
        })
        .collect();
    let mut header = vec!["PHP", "INSTALLED", ""];
    if remote.is_some() {
        header.push("LATEST");
    }
    ui::table(&header, &rows);
    Ok(())
}

fn php_update(versions: &[String]) -> Result<()> {
    let cfg = Config::load()?;
    let targets: Vec<String> = if versions.is_empty() {
        php::installed().into_iter().map(|i| i.minor).collect()
    } else {
        versions
            .iter()
            .map(|v| php::require_normalized(v))
            .collect::<Result<_>>()?
    };
    let remote = php::available_builds(&cfg)?;
    let installed = php::installed();
    let mut updated = Vec::new();
    for v in targets {
        let Some(inst) = installed.iter().find(|i| i.minor == v) else {
            ui::warn(format!("PHP {v} is not installed"));
            continue;
        };
        let current = &inst.version;
        let source = php::mirror_source(&cfg);
        match remote.get(&v) {
            // Also replaces static-php builds from Bird 0.1, and rebuilds of the
            // same version (e.g. with more extensions).
            Some(latest) if inst.outdated(latest, source) => {
                let full = php::install(&cfg, &v)?;
                if inst.source != source {
                    ui::ok(format!(
                        "PHP {current} ({}) → {full} ({source})",
                        inst.source
                    ));
                } else if *current == full {
                    ui::ok(format!("PHP {full}: installed the new build"));
                } else {
                    ui::ok(format!("PHP {current} → {full}"));
                }
                updated.push(v);
            }
            Some(_) => ui::ok(format!("PHP {current} is up to date")),
            None => ui::warn(format!("PHP {v} is not available from {}", cfg.php_mirror)),
        }
    }
    if !updated.is_empty() && control::is_running() {
        for v in &updated {
            let _ = control::send(json!({"cmd": "restart-php", "version": v}));
        }
    }
    Ok(())
}

fn php_remove(version: &str) -> Result<()> {
    let version = php::require_normalized(version)?;
    let mut cfg = Config::load()?;
    if control::is_running() {
        let _ = control::send(json!({"cmd": "restart-php", "version": version}));
    }
    php::remove(&version)?;
    if cfg.php.as_deref() == Some(version.as_str()) {
        cfg.php = php::installed().last().map(|i| i.minor.clone());
        cfg.save()?;
        match &cfg.php {
            Some(v) => ui::warn(format!("global PHP was {version}; it is now {v}")),
            None => ui::warn("no PHP versions left; install one with `bird php:install 8.4`"),
        }
    }
    shim::refresh()?;
    ui::ok(format!("removed PHP {version}"));
    Ok(())
}

fn ini(version: Option<String>, edit: bool) -> Result<()> {
    let cfg = Config::load()?;
    let version = match version {
        Some(v) => php::require_normalized(&v)?,
        None => php::resolve_for_dir(&cfg, &cwd()?)
            .map(|(v, _)| v)
            .context("no PHP version configured")?,
    };
    let path = paths().php_ini(&version);
    if !path.exists() {
        bail!("PHP {version} is not installed");
    }
    if edit {
        let status = match std::env::var("VISUAL").or_else(|_| std::env::var("EDITOR")) {
            Ok(editor) => std::process::Command::new("sh")
                .arg("-c")
                .arg(format!("{editor} \"$1\""))
                .arg("sh")
                .arg(&path)
                .status()?,
            Err(_) if cfg!(target_os = "linux") => crate::linux::open(&path.display().to_string())?,
            Err(_) => std::process::Command::new("open")
                .arg("-t")
                .arg(&path)
                .status()?,
        };
        if status.success() && control::is_running() {
            let _ = control::send(json!({"cmd": "restart-php", "version": version}));
            ui::ok(format!(
                "PHP {version} restarts with the new settings on the next request"
            ));
        }
    } else {
        println!("{}", path.display());
    }
    Ok(())
}

fn set_tld(tld: Option<String>) -> Result<()> {
    let mut cfg = Config::load()?;
    let Some(tld) = tld else {
        println!("{}", cfg.tld);
        return Ok(());
    };
    let tld = tld.trim_start_matches('.').to_ascii_lowercase();
    if tld.is_empty() || !tld.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        bail!("the TLD must be a single label like `test`");
    }
    cfg.tld = tld.clone();
    cfg.save()?;
    ui::ok(format!("sites are now served as <name>.{tld}"));
    // The CA is name-constrained to the TLD, so a new TLD needs a new CA.
    let replaced = crate::tls::ensure_ca(&tld)? != crate::tls::CaStatus::Existing;
    if install::is_system_installed() {
        println!("Updating /etc/resolver and the trusted CA (needs sudo)…");
        install::sudo_system_install()?;
    } else if replaced {
        ui::warn(format!(
            "made a new CA for *.{tld}; restart the daemon and run `bird install` to trust it"
        ));
    }
    Ok(())
}

fn logs(which: Option<String>, follow: bool) -> Result<()> {
    let path = match which.as_deref() {
        None | Some("bird") | Some("daemon") => paths().daemon_log(),
        Some(v) => paths().fpm_log(&php::require_normalized(v)?),
    };
    if !Path::new(&path).exists() {
        bail!("{} does not exist yet", path.display());
    }
    use std::os::unix::process::CommandExt;
    let mut cmd = std::process::Command::new("tail");
    cmd.arg("-n").arg("100");
    if follow {
        cmd.arg("-F");
    }
    Err(cmd.arg(&path).exec()).context("could not run tail")
}
