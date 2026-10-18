//! The long-running Bird process: DNS, HTTP(S) and PHP-FPM supervision.
//!
//! Under launchd (macOS) the privileged listening sockets (127.0.0.1:80/443)
//! are created by launchd and handed over, so the daemon itself runs as the
//! user. Under systemd (Linux) it runs as the user with CAP_NET_BIND_SERVICE
//! only, binds the ports itself and drops the capability right after.
//! Started by hand, it binds the configured ports itself.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result};
use hyper::service::service_fn;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, UdpSocket, UnixListener};
use tokio::sync::Notify;
use tokio_rustls::TlsAcceptor;

use crate::config::Config;
use crate::fpm::FpmManager;
use crate::paths::paths;
use crate::server::{self, Conn, ResponseBody};
use crate::{dns, tls};

pub struct State {
    config: RwLock<Arc<Config>>,
    config_checked: Mutex<(Instant, Option<SystemTime>)>,
    pub fpm: FpmManager,
    pub http_client: Client<HttpConnector, ResponseBody>,
    pub started: Instant,
    /// Ports actually listened on; port changes in the config need a restart.
    ports: (u16, u16),
    shutdown: Notify,
    /// Whether this runs as the installed service (launchd or systemd).
    service: std::sync::atomic::AtomicBool,
}

fn config_mtime() -> Option<SystemTime> {
    std::fs::metadata(paths().config())
        .and_then(|m| m.modified())
        .ok()
}

impl State {
    fn new(config: Config) -> Self {
        let ports = (config.http_port, config.https_port);
        Self {
            config: RwLock::new(Arc::new(config)),
            config_checked: Mutex::new((Instant::now(), config_mtime())),
            fpm: FpmManager::default(),
            http_client: Client::builder(TokioExecutor::new()).build_http(),
            started: Instant::now(),
            ports,
            shutdown: Notify::new(),
            service: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Current config; re-read from disk when the file changed (checked at most every 250 ms).
    pub fn config(&self) -> Arc<Config> {
        {
            let mut checked = self.config_checked.lock().unwrap();
            if checked.0.elapsed() >= Duration::from_millis(250) {
                checked.0 = Instant::now();
                let mtime = config_mtime();
                if mtime != checked.1 {
                    checked.1 = mtime;
                    drop(checked);
                    self.reload();
                }
            }
        }
        self.config.read().unwrap().clone()
    }

    pub fn reload(&self) {
        match Config::load() {
            Ok(mut cfg) => {
                (cfg.http_port, cfg.https_port) = self.ports;
                *self.config.write().unwrap() = Arc::new(cfg);
                tracing::info!("configuration reloaded");
            }
            Err(e) => tracing::error!("keeping previous configuration: {e:#}"),
        }
    }
}

pub fn run(http_port: Option<u16>, https_port: Option<u16>) -> Result<()> {
    let level = std::env::var("BIRD_LOG")
        .ok()
        .and_then(|l| l.parse().ok())
        .unwrap_or(tracing::Level::INFO);
    tracing_subscriber::fmt()
        .with_max_level(level)
        .with_target(false)
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .init();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(main(http_port, https_port))
}

async fn main(http_port: Option<u16>, https_port: Option<u16>) -> Result<()> {
    paths().ensure_dirs()?;
    // A new version may need to migrate ~/.bird before the config is read.
    // If that fails, serve anyway: a daemon that exits takes every site down
    // and launchd/systemd would restart it in a loop.
    match crate::migrate::run() {
        Ok(migrations) => {
            for id in &migrations.applied {
                tracing::info!("applied migration {id}");
            }
            if !migrations.unknown.is_empty() {
                tracing::warn!(
                    "~/.bird was migrated by a newer Bird ({}); update this one",
                    migrations.unknown.join(", ")
                );
            }
        }
        Err(e) => tracing::warn!("could not migrate ~/.bird: {e:#}"),
    }
    let mut cfg = Config::load()?;
    tls::ensure_ca_exists(&cfg.tld)?;
    if let Some(p) = http_port {
        cfg.http_port = p;
    }
    if let Some(p) = https_port {
        cfg.https_port = p;
    }
    let state = Arc::new(State::new(cfg.clone()));
    let tls_config = tls::server_config(tls::CertResolver::new(tls::Ca::load()?, state.clone()))?;
    let acceptor = TlsAcceptor::from(Arc::new(tls_config));

    let (http, from_launchd) =
        listener("http", SocketAddr::from((cfg.listen, cfg.http_port))).await?;
    let (https, _) = listener("https", SocketAddr::from((cfg.listen, cfg.https_port))).await?;
    // systemd sets INVOCATION_ID for the processes of a unit.
    let under_systemd = std::env::var_os("INVOCATION_ID").is_some();
    state.service.store(
        from_launchd || under_systemd,
        std::sync::atomic::Ordering::Relaxed,
    );
    #[cfg(target_os = "linux")]
    drop_bind_capability();
    let dns_socket = UdpSocket::bind(("127.0.0.1", cfg.dns_port))
        .await
        .with_context(|| format!("could not bind DNS to 127.0.0.1:{}", cfg.dns_port))?;
    let control = control_listener()?;

    tracing::info!(
        "bird {} ready — http {} · https {} · dns 127.0.0.1:{} · *.{}",
        env!("CARGO_PKG_VERSION"),
        http.local_addr()?,
        https.local_addr()?,
        cfg.dns_port,
        cfg.tld
    );

    tokio::spawn(accept_loop(http, state.clone(), None));
    tokio::spawn(accept_loop(https, state.clone(), Some(acceptor)));
    tokio::spawn(dns::serve(dns_socket, state.clone()));
    tokio::spawn(control_loop(control, state.clone()));
    tokio::spawn(idle_reaper(state.clone()));
    tokio::spawn(log_janitor());

    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
        _ = state.shutdown.notified() => {}
    }
    tracing::info!("shutting down");
    state.fpm.stop(None).await;
    let _ = std::fs::remove_file(paths().control_socket());
    Ok(())
}

/// A listener handed over by launchd (socket activation), or one we bind
/// ourselves. The flag says which.
async fn listener(name: &str, addr: SocketAddr) -> Result<(TcpListener, bool)> {
    #[cfg(target_os = "macos")]
    if let Some(fd) = launchd::socket(name).into_iter().next() {
        use std::os::fd::FromRawFd;
        // SAFETY: launchd made this socket for Bird and hands it over once;
        // nothing else holds or closes the fd, so the listener may own it.
        let std_listener = unsafe { std::net::TcpListener::from_raw_fd(fd) };
        std_listener.set_nonblocking(true)?;
        return Ok((TcpListener::from_std(std_listener)?, true));
    }
    let listener = TcpListener::bind(addr).await.with_context(|| {
        format!(
            "could not listen on {addr} for {name}. Ports below 1024 need `bird install` \
             (or run the daemon with --http-port/--https-port)"
        )
    })?;
    Ok((listener, false))
}

async fn accept_loop(listener: TcpListener, state: Arc<State>, tls: Option<TlsAcceptor>) {
    loop {
        let (stream, remote) = match listener.accept().await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("accept: {e}");
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
        };
        let _ = stream.set_nodelay(true);
        let local = stream.local_addr().unwrap_or(remote);
        let state = state.clone();
        let tls = tls.clone();
        tokio::spawn(async move {
            let conn = Conn {
                secure: tls.is_some(),
                remote,
                local,
            };
            let service = service_fn(move |req| server::handle(state.clone(), conn, req));
            let builder = auto::Builder::new(TokioExecutor::new());
            let result = match tls {
                Some(acceptor) => match acceptor.accept(stream).await {
                    Ok(tls_stream) => {
                        builder
                            .serve_connection_with_upgrades(TokioIo::new(tls_stream), service)
                            .await
                    }
                    Err(e) => {
                        tracing::debug!("tls handshake from {remote}: {e}");
                        return;
                    }
                },
                None => {
                    builder
                        .serve_connection_with_upgrades(TokioIo::new(stream), service)
                        .await
                }
            };
            if let Err(e) = result {
                tracing::debug!("connection {remote}: {e}");
            }
        });
    }
}

async fn idle_reaper(state: Arc<State>) {
    loop {
        tokio::time::sleep(Duration::from_secs(60)).await;
        let minutes = state.config().php_idle_minutes;
        if minutes > 0 {
            state.fpm.reap_idle(Duration::from_secs(minutes * 60)).await;
        }
    }
}

/// Keeps ~/.bird/log bounded (see `logs`); cheap enough to run every 10 minutes.
async fn log_janitor() {
    loop {
        let dir = paths().log();
        match tokio::task::spawn_blocking(move || crate::logs::tidy(&dir)).await {
            Ok(Err(e)) => tracing::warn!("could not tidy the logs: {e}"),
            Err(e) => tracing::warn!("could not tidy the logs: {e}"),
            Ok(Ok(())) => {}
        }
        tokio::time::sleep(Duration::from_secs(10 * 60)).await;
    }
}

// ---------------------------------------------------------------------------
// Control socket: newline-delimited JSON, one request per connection.

fn control_listener() -> Result<UnixListener> {
    let path = paths().control_socket();
    if std::os::unix::net::UnixStream::connect(&path).is_ok() {
        anyhow::bail!(
            "another bird daemon is already running ({})",
            path.display()
        );
    }
    let _ = std::fs::remove_file(&path);
    UnixListener::bind(&path).with_context(|| format!("could not bind {}", path.display()))
}

async fn control_loop(listener: UnixListener, state: Arc<State>) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            continue;
        };
        let state = state.clone();
        tokio::spawn(async move {
            let (read, mut write) = stream.into_split();
            let mut line = String::new();
            if BufReader::new(read).read_line(&mut line).await.is_err() {
                return;
            }
            let reply = match serde_json::from_str::<serde_json::Value>(&line) {
                Ok(req) => control_command(&state, &req).await,
                Err(e) => json!({"ok": false, "error": e.to_string()}),
            };
            let _ = write.write_all(format!("{reply}\n").as_bytes()).await;
        });
    }
}

async fn control_command(state: &Arc<State>, req: &serde_json::Value) -> serde_json::Value {
    let version = req.get("version").and_then(|v| v.as_str());
    match req.get("cmd").and_then(|c| c.as_str()) {
        Some("status") => {
            let cfg = state.config();
            json!({
                "ok": true,
                "version": env!("CARGO_PKG_VERSION"),
                "pid": std::process::id(),
                "uptime_secs": state.started.elapsed().as_secs(),
                "tld": cfg.tld,
                "http_port": cfg.http_port,
                "https_port": cfg.https_port,
                "service": state.service.load(std::sync::atomic::Ordering::Relaxed),
                "fpm": state.fpm.status().await,
            })
        }
        Some("reload") => {
            state.reload();
            json!({"ok": true})
        }
        Some("restart-php") => {
            state.reload();
            let stopped = state.fpm.stop(version).await;
            json!({"ok": true, "stopped": stopped})
        }
        Some("shutdown") => {
            state.shutdown.notify_one();
            json!({"ok": true})
        }
        other => json!({"ok": false, "error": format!("unknown command {other:?}")}),
    }
}

/// The ports are bound; nothing needs to bind privileged ports any more.
/// Clears the capability the systemd unit grants, for this thread; php-fpm
/// is started from tokio's worker threads, which still hold it, so fpm.rs
/// clears it again in each php-fpm process before exec.
#[cfg(target_os = "linux")]
fn drop_bind_capability() {
    if let Err(e) = crate::sys::drop_capabilities() {
        tracing::warn!("could not drop capabilities: {e}");
    }
}

#[cfg(target_os = "macos")]
mod launchd {
    use std::ffi::CString;
    use std::os::fd::RawFd;

    // As declared in <launch.h> (macOS 10.10 and later).
    unsafe extern "C" {
        fn launch_activate_socket(
            name: *const libc::c_char,
            fds: *mut *mut libc::c_int,
            cnt: *mut libc::size_t,
        ) -> libc::c_int;
    }

    /// File descriptors launchd created for the `Sockets` entry `name`, if any.
    pub fn socket(name: &str) -> Vec<RawFd> {
        let name = CString::new(name).unwrap();
        let mut fds: *mut libc::c_int = std::ptr::null_mut();
        let mut count: libc::size_t = 0;
        // SAFETY: name is NUL-terminated and outlives the call; fds and count
        // are valid places for launchd to write its answer.
        let rc = unsafe { launch_activate_socket(name.as_ptr(), &mut fds, &mut count) };
        if rc != 0 || fds.is_null() {
            return Vec::new();
        }
        // SAFETY: on success fds points to `count` ints that launchd
        // allocated with malloc; they are copied out before being freed.
        let out = unsafe { std::slice::from_raw_parts(fds, count) }.to_vec();
        // SAFETY: the array is ours to free, freed once, and not used after.
        unsafe { libc::free(fds.cast()) };
        out
    }
}
