//! HTTP request handling: routes each request to a static file, PHP-FPM or a
//! reverse proxy, depending on the site it is for.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use bytes::Bytes;
use http::{HeaderValue, Method, Request, Response, StatusCode, header};
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::{BodyExt, Empty, Full};
use hyper::body::Incoming;
use tower::ServiceExt;

use crate::config::XdebugMode;
use crate::daemon::State;
use crate::fastcgi;
use crate::php;
use crate::proxy;
use crate::sites::{self, Site, Target};

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;
pub type ResponseBody = UnsyncBoxBody<Bytes, BoxError>;

#[derive(Clone, Copy)]
pub struct Conn {
    pub secure: bool,
    pub remote: SocketAddr,
    pub local: SocketAddr,
}

pub fn full(body: impl Into<Bytes>) -> ResponseBody {
    Full::new(body.into())
        .map_err(|e| match e {})
        .boxed_unsync()
}

pub fn empty() -> ResponseBody {
    Empty::new().map_err(|e| match e {}).boxed_unsync()
}

pub async fn handle(
    state: Arc<State>,
    conn: Conn,
    req: Request<Incoming>,
) -> Result<Response<ResponseBody>, Infallible> {
    let started = Instant::now();
    let method = req.method().clone();
    let host = request_host(&req).unwrap_or_default();
    let path = req.uri().path().to_string();
    let response = match route(&state, conn, &host, req).await {
        Ok(resp) => resp,
        Err(e) => {
            tracing::error!("{method} {host}{path}: {e:#}");
            error_page(
                StatusCode::BAD_GATEWAY,
                "Bad gateway",
                &html_escape(&format!("{e:#}")),
            )
        }
    };
    tracing::info!(
        target: "access",
        "{} {}{} → {} ({} ms)",
        method,
        host,
        path,
        response.status().as_u16(),
        started.elapsed().as_millis()
    );
    Ok(response)
}

fn request_host<B>(req: &Request<B>) -> Option<String> {
    req.headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .map(str::to_string)
        .or_else(|| req.uri().authority().map(|a| a.to_string()))
}

async fn route(
    state: &Arc<State>,
    conn: Conn,
    host: &str,
    req: Request<Incoming>,
) -> Result<Response<ResponseBody>> {
    let cfg = state.config();
    let Some(target) = sites::resolve_host(&cfg, host) else {
        let hint = format!(
            "Bird has no site for <code>{}</code>.<br>Run <code>bird link</code> in the project directory, \
             or <code>bird park</code> in the directory that contains it.",
            html_escape(host)
        );
        return Ok(error_page(StatusCode::NOT_FOUND, "Site not found", &hint));
    };
    match target {
        Target::Proxy(proxy) => {
            if proxy.secure && !conn.secure {
                return Ok(redirect_to_https(host, &req, cfg.https_port));
            }
            proxy::forward(state, conn, host, &proxy.upstream, req).await
        }
        Target::Site(site) => {
            if site.secure && !conn.secure {
                return Ok(redirect_to_https(host, &req, cfg.https_port));
            }
            serve_site(state, conn, host, &site, req).await
        }
    }
}

fn redirect_to_https<B>(host: &str, req: &Request<B>, https_port: u16) -> Response<ResponseBody> {
    let name = host.split(':').next().unwrap_or(host);
    let port = if https_port == 443 {
        String::new()
    } else {
        format!(":{https_port}")
    };
    let path = req
        .uri()
        .path_and_query()
        .map(|p| p.as_str())
        .unwrap_or("/");
    // Temporary on purpose: browsers cache 301s, which would outlive `bird unsecure`.
    Response::builder()
        .status(StatusCode::TEMPORARY_REDIRECT)
        .header(header::LOCATION, format!("https://{name}{port}{path}"))
        .body(empty())
        .unwrap()
}

#[derive(Debug, PartialEq)]
pub enum Located {
    Static(PathBuf),
    Php {
        script: PathBuf,
        script_name: String,
        path_info: String,
    },
    /// Directory requested without a trailing slash.
    AddSlash,
    NotFound,
    Forbidden,
}

/// Decides what serves `url_path` inside `docroot`, mirroring the usual
/// nginx `try_files $uri $uri/ /index.php?$query_string` setup.
pub fn locate(docroot: &Path, url_path: &str) -> Located {
    let decoded = percent_decode(url_path);
    let Some(decoded) = decoded else {
        return Located::Forbidden;
    };
    let rel = decoded.trim_start_matches('/');
    let rel_path = Path::new(rel);
    for component in rel_path.components() {
        match component {
            Component::Normal(seg) => {
                let seg = seg.to_string_lossy();
                if seg.starts_with('.') && seg != ".well-known" {
                    return Located::Forbidden;
                }
            }
            Component::CurDir => {}
            _ => return Located::Forbidden,
        }
    }

    // `/foo.php/extra/path` → script foo.php, PATH_INFO /extra/path
    let mut prefix = String::new();
    let segments: Vec<&str> = rel.split('/').collect();
    for (i, seg) in segments.iter().enumerate() {
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(seg);
        // Any case: on a case-insensitive filesystem (macOS) `x.PHP` is `x.php`.
        if seg.to_ascii_lowercase().ends_with(".php") {
            let script = docroot.join(&prefix);
            if script.is_file() {
                let rest = segments[i + 1..].join("/");
                let path_info = if i + 1 < segments.len() {
                    format!("/{rest}")
                } else {
                    String::new()
                };
                return Located::Php {
                    script,
                    script_name: format!("/{prefix}"),
                    path_info,
                };
            }
            break;
        }
    }

    let full = docroot.join(rel_path);
    if !rel.is_empty() && full.is_file() {
        // PHP files are run or refused, never sent as source.
        if full
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("php"))
        {
            return Located::Forbidden;
        }
        return Located::Static(full);
    }
    if full.is_dir() {
        let index_php = full.join("index.php");
        let index_html = full.join("index.html");
        if index_php.is_file() || index_html.is_file() {
            if !rel.is_empty() && !url_path.ends_with('/') {
                return Located::AddSlash;
            }
            if index_php.is_file() {
                let dir = rel.trim_end_matches('/');
                let script_name = if dir.is_empty() {
                    "/index.php".into()
                } else {
                    format!("/{dir}/index.php")
                };
                return Located::Php {
                    script: index_php,
                    script_name,
                    path_info: String::new(),
                };
            }
            return Located::Static(index_html);
        }
    }
    let front = docroot.join("index.php");
    if front.is_file() {
        return Located::Php {
            script: front,
            script_name: "/index.php".into(),
            path_info: String::new(),
        };
    }
    Located::NotFound
}

fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    if out.contains(&0) {
        return None;
    }
    String::from_utf8(out).ok()
}

async fn serve_site(
    state: &Arc<State>,
    conn: Conn,
    host: &str,
    site: &Site,
    req: Request<Incoming>,
) -> Result<Response<ResponseBody>> {
    let docroot = site.docroot();
    match locate(&docroot, req.uri().path()) {
        Located::Static(file) => serve_static(file, req).await,
        Located::Php {
            script,
            script_name,
            path_info,
        } => {
            serve_php(
                state,
                conn,
                host,
                site,
                &docroot,
                script,
                script_name,
                path_info,
                req,
            )
            .await
        }
        Located::AddSlash => {
            let mut location = format!("{}/", req.uri().path());
            if let Some(q) = req.uri().query() {
                location.push('?');
                location.push_str(q);
            }
            Ok(Response::builder()
                .status(StatusCode::MOVED_PERMANENTLY)
                .header(header::LOCATION, location)
                .body(empty())?)
        }
        Located::Forbidden => Ok(error_page(StatusCode::FORBIDDEN, "Forbidden", "")),
        Located::NotFound => Ok(error_page(StatusCode::NOT_FOUND, "Not found", "")),
    }
}

async fn serve_static(file: PathBuf, req: Request<Incoming>) -> Result<Response<ResponseBody>> {
    let resp = tower_http::services::ServeFile::new(file)
        .oneshot(req)
        .await?;
    Ok(resp.map(|b| b.map_err(|e| -> BoxError { Box::new(e) }).boxed_unsync()))
}

#[allow(clippy::too_many_arguments)]
async fn serve_php(
    state: &Arc<State>,
    conn: Conn,
    host: &str,
    site: &Site,
    docroot: &Path,
    script: PathBuf,
    script_name: String,
    path_info: String,
    req: Request<Incoming>,
) -> Result<Response<ResponseBody>> {
    let cfg = state.config();
    let Some((version, _)) = site.php_version(&cfg) else {
        return Ok(error_page(
            StatusCode::BAD_GATEWAY,
            "No PHP version",
            "No PHP version is configured. Run <code>bird php:install 8.4</code>.",
        ));
    };
    let xdebug = match cfg.xdebug {
        XdebugMode::Off => false,
        XdebugMode::On => true,
        XdebugMode::Auto => wants_xdebug(req.headers(), req.uri()),
    };
    // Builds without Xdebug (static-php ones from Bird 0.1) serve normally.
    let xdebug = xdebug && php::has_xdebug(&version);
    let socket = match state.fpm.socket_for(&version, xdebug).await {
        Ok(s) => s,
        Err(e) => {
            return Ok(error_page(
                StatusCode::BAD_GATEWAY,
                &format!("PHP {version} unavailable"),
                &html_escape(&format!("{e:#}")),
            ));
        }
    };

    let (parts, body) = req.into_parts();
    let content_length = parts
        .headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    // FPM needs CONTENT_LENGTH up front; buffer bodies that don't declare one.
    let (body, length): (ResponseBody, u64) = match content_length {
        Some(len) => (
            body.map_err(|e| -> BoxError { Box::new(e) }).boxed_unsync(),
            len,
        ),
        None if http_body::Body::size_hint(&body).exact() == Some(0) => (empty(), 0),
        None => match http_body_util::Limited::new(body, MAX_BUFFERED_BODY)
            .collect()
            .await
        {
            Ok(collected) => {
                let bytes = collected.to_bytes();
                let len = bytes.len() as u64;
                (full(bytes), len)
            }
            Err(e) if e.is::<http_body_util::LengthLimitError>() => {
                return Ok(error_page(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "Request too large",
                    "Requests without a Content-Length are buffered in memory, up to 256 MB.",
                ));
            }
            Err(e) => return Err(anyhow::anyhow!(e)),
        },
    };

    let server_name = host.split(':').next().unwrap_or(host).to_string();
    let uri = &parts.uri;
    let request_uri = uri
        .path_and_query()
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| "/".into());
    let protocol = match parts.version {
        http::Version::HTTP_2 => "HTTP/2.0",
        http::Version::HTTP_10 => "HTTP/1.0",
        _ => "HTTP/1.1",
    };
    let mut params: Vec<(String, String)> = vec![
        ("GATEWAY_INTERFACE".into(), "CGI/1.1".into()),
        (
            "SERVER_SOFTWARE".into(),
            concat!("bird/", env!("CARGO_PKG_VERSION")).into(),
        ),
        ("SERVER_PROTOCOL".into(), protocol.into()),
        ("SERVER_NAME".into(), server_name),
        ("SERVER_ADDR".into(), conn.local.ip().to_string()),
        ("SERVER_PORT".into(), conn.local.port().to_string()),
        ("REMOTE_ADDR".into(), conn.remote.ip().to_string()),
        ("REMOTE_PORT".into(), conn.remote.port().to_string()),
        ("REQUEST_METHOD".into(), parts.method.to_string()),
        (
            "REQUEST_SCHEME".into(),
            if conn.secure { "https" } else { "http" }.into(),
        ),
        ("REQUEST_URI".into(), request_uri),
        ("QUERY_STRING".into(), uri.query().unwrap_or("").into()),
        ("DOCUMENT_ROOT".into(), docroot.display().to_string()),
        ("DOCUMENT_URI".into(), format!("{script_name}{path_info}")),
        ("SCRIPT_NAME".into(), script_name),
        ("SCRIPT_FILENAME".into(), script.display().to_string()),
        ("PATH_INFO".into(), path_info),
        (
            "CONTENT_LENGTH".into(),
            if length > 0 || parts.method == Method::POST {
                length.to_string()
            } else {
                String::new()
            },
        ),
        (
            "CONTENT_TYPE".into(),
            parts
                .headers
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .into(),
        ),
        ("REDIRECT_STATUS".into(), "200".into()),
        ("BIRD_SITE".into(), site.name.clone()),
    ];
    if conn.secure {
        params.push(("HTTPS".into(), "on".into()));
    }
    let mut have_host = false;
    for name in parts.headers.keys() {
        let key = name.as_str();
        if key == "content-type" || key == "content-length" || key == "proxy" {
            continue; // `Proxy` is ignored to defuse httpoxy
        }
        have_host |= key == "host";
        let sep = if key == "cookie" { "; " } else { ", " };
        let value = parts
            .headers
            .get_all(name)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .collect::<Vec<_>>()
            .join(sep);
        params.push((
            format!("HTTP_{}", key.to_ascii_uppercase().replace('-', "_")),
            value,
        ));
    }
    if !have_host {
        params.push(("HTTP_HOST".into(), host.to_string()));
    }

    let stream = match tokio::net::UnixStream::connect(&socket).await {
        Ok(s) => s,
        Err(e) => {
            state.fpm.mark_broken(&version, xdebug).await;
            anyhow::bail!("could not connect to php-fpm {version}: {e}");
        }
    };
    let resp = fastcgi::request(stream, params, body).await?;
    let mut builder = Response::builder().status(resp.status);
    if let Some(h) = builder.headers_mut() {
        *h = resp.headers;
        h.entry(header::CONTENT_TYPE)
            .or_insert(HeaderValue::from_static("text/html; charset=UTF-8"));
    }
    let body = if parts.method == Method::HEAD {
        empty()
    } else {
        resp.body
    };
    Ok(builder.body(body)?)
}

/// Names Xdebug treats as triggers, in a cookie or the query string.
const XDEBUG_TRIGGERS: &[&str] = &[
    "XDEBUG_SESSION",
    "XDEBUG_SESSION_START",
    "XDEBUG_PROFILE",
    "XDEBUG_TRACE",
    "XDEBUG_TRIGGER",
];

/// Whether a request asks for Xdebug: the browser extension's cookie, or
/// `?XDEBUG_SESSION=1` / `?XDEBUG_PROFILE=1` / … in the URL.
pub fn wants_xdebug(headers: &http::HeaderMap, uri: &http::Uri) -> bool {
    let in_query = uri.query().is_some_and(|q| {
        q.split('&')
            .map(|pair| pair.split('=').next().unwrap_or(""))
            .any(|key| XDEBUG_TRIGGERS.contains(&key))
    });
    let in_cookie = headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .map(|c| c.trim().split('=').next().unwrap_or(""))
        .any(|name| XDEBUG_TRIGGERS.contains(&name) && name != "XDEBUG_SESSION_START");
    in_query || in_cookie
}

/// The most of a request body without Content-Length that is held in memory
/// to count it (FastCGI needs the length first). Above PHP's own post_max_size
/// default, so only runaway or endless bodies are refused.
const MAX_BUFFERED_BODY: usize = 256 << 20;

pub fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// `detail` is inserted as HTML; escape anything user-controlled first.
pub fn error_page(status: StatusCode, title: &str, detail: &str) -> Response<ResponseBody> {
    let html = format!(
        "<!doctype html><html><head><meta charset=utf-8><title>{code} {title}</title>\
         <style>body{{font:16px/1.5 -apple-system,system-ui,sans-serif;max-width:40rem;margin:4rem auto;padding:0 1rem;color:#222}}\
         h1{{font-size:1.4rem}}code,pre{{background:#f2f2f2;padding:.1rem .3rem;border-radius:4px;white-space:pre-wrap}}\
         small{{color:#888}}@media(prefers-color-scheme:dark){{body{{background:#161616;color:#ddd}}code,pre{{background:#2a2a2a}}}}</style></head>\
         <body><h1>{code} · {title}</h1><p>{detail}</p><small>bird</small></body></html>",
        code = status.as_u16(),
        title = html_escape(title),
    );
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-store")
        .body(full(html))
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> PathBuf {
        let root = std::env::temp_dir().join(format!("bird-locate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for dir in ["css", "wp-admin", "docs", "empty", "lib.php"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        for file in [
            "index.php",
            "css/app.css",
            "wp-admin/index.php",
            "docs/index.html",
            "info.php",
            "Config.PHP",
            "lib.php/secret.PHP",
            ".env",
        ] {
            std::fs::write(root.join(file), "x").unwrap();
        }
        root
    }

    #[test]
    fn locates_requests() {
        let root = fixture();
        assert_eq!(
            locate(&root, "/css/app.css"),
            Located::Static(root.join("css/app.css"))
        );
        assert_eq!(
            locate(&root, "/css/app%2Ecss"),
            Located::Static(root.join("css/app.css"))
        );
        assert_eq!(
            locate(&root, "/"),
            Located::Php {
                script: root.join("index.php"),
                script_name: "/index.php".into(),
                path_info: "".into()
            }
        );
        assert_eq!(
            locate(&root, "/users/42"),
            Located::Php {
                script: root.join("index.php"),
                script_name: "/index.php".into(),
                path_info: "".into()
            }
        );
        assert_eq!(
            locate(&root, "/info.php/a/b"),
            Located::Php {
                script: root.join("info.php"),
                script_name: "/info.php".into(),
                path_info: "/a/b".into()
            }
        );
        assert_eq!(locate(&root, "/wp-admin"), Located::AddSlash);
        assert_eq!(
            locate(&root, "/wp-admin/"),
            Located::Php {
                script: root.join("wp-admin/index.php"),
                script_name: "/wp-admin/index.php".into(),
                path_info: "".into()
            }
        );
        assert_eq!(
            locate(&root, "/docs/"),
            Located::Static(root.join("docs/index.html"))
        );
        // PHP in any case is run or refused, never served as source.
        assert_eq!(
            locate(&root, "/Config.PHP"),
            Located::Php {
                script: root.join("Config.PHP"),
                script_name: "/Config.PHP".into(),
                path_info: "".into()
            }
        );
        assert_eq!(locate(&root, "/lib.php/secret.PHP"), Located::Forbidden);
        assert_eq!(locate(&root, "/.env"), Located::Forbidden);
        assert_eq!(locate(&root, "/css/../.env"), Located::Forbidden);
        assert_eq!(locate(&root, "/%2e%2e/etc/passwd"), Located::Forbidden);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn detects_xdebug_triggers() {
        let uri = |s: &str| s.parse::<http::Uri>().unwrap();
        let cookies = |c: &str| {
            let mut h = http::HeaderMap::new();
            h.insert(header::COOKIE, c.parse().unwrap());
            h
        };
        let none = http::HeaderMap::new();
        assert!(wants_xdebug(
            &cookies("a=1; XDEBUG_SESSION=PHPSTORM"),
            &uri("/")
        ));
        assert!(wants_xdebug(&none, &uri("/x?a=1&XDEBUG_SESSION=1")));
        assert!(wants_xdebug(&none, &uri("/x?XDEBUG_PROFILE")));
        assert!(wants_xdebug(&none, &uri("/x?XDEBUG_SESSION_START=vscode")));
        assert!(!wants_xdebug(
            &none,
            &uri("/x?xdebug=1&MY_XDEBUG_SESSION=1")
        ));
        assert!(!wants_xdebug(
            &cookies("laravel_session=abc; XDEBUG_SESSIONX=1"),
            &uri("/")
        ));
    }

    #[test]
    fn decodes_percent() {
        assert_eq!(percent_decode("/a%20b").as_deref(), Some("/a b"));
        assert_eq!(percent_decode("/a%00"), None);
        assert_eq!(percent_decode("/100%").as_deref(), Some("/100%"));
    }
}
