//! Reverse proxy for `bird proxy` sites (Vite, Reverb, Docker containers...),
//! including WebSocket upgrades.

use std::sync::Arc;

use anyhow::{Context, Result, bail};
use http::uri::{Authority, Scheme};
use http::{HeaderValue, Request, Response, StatusCode, Uri, header};
use http_body_util::BodyExt;
use hyper::body::Incoming;
use hyper_util::rt::TokioIo;

use crate::daemon::State;
use crate::server::{BoxError, Conn, ResponseBody, empty};

const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

pub fn parse_upstream(upstream: &str) -> Result<(Authority, String)> {
    let with_scheme = if upstream.contains("://") {
        upstream.to_string()
    } else {
        format!("http://{upstream}")
    };
    let uri: Uri = with_scheme
        .parse()
        .with_context(|| format!("invalid upstream '{upstream}'"))?;
    if uri.scheme() != Some(&Scheme::HTTP) {
        bail!("only http:// upstreams are supported (got '{upstream}')");
    }
    let authority = uri.authority().cloned().context("upstream has no host")?;
    let prefix = uri.path().trim_end_matches('/').to_string();
    Ok((authority, prefix))
}

fn is_upgrade<B>(req: &Request<B>) -> bool {
    req.headers().contains_key(header::UPGRADE)
        && req
            .headers()
            .get(header::CONNECTION)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.to_ascii_lowercase().contains("upgrade"))
}

pub async fn forward(
    state: &Arc<State>,
    conn: Conn,
    host: &str,
    upstream: &str,
    mut req: Request<Incoming>,
) -> Result<Response<ResponseBody>> {
    let (authority, prefix) = parse_upstream(upstream)?;
    let path = req
        .uri()
        .path_and_query()
        .map(|p| p.as_str())
        .unwrap_or("/");
    let uri: Uri = format!("http://{authority}{prefix}{path}").parse()?;
    let upgrade = is_upgrade(&req);
    let client_upgrade = upgrade.then(|| hyper::upgrade::on(&mut req));

    let (mut parts, body) = req.into_parts();
    parts.uri = uri;
    parts.version = http::Version::HTTP_11;
    if !upgrade {
        for h in HOP_BY_HOP {
            parts.headers.remove(*h);
        }
    }
    if !parts.headers.contains_key(header::HOST) {
        parts
            .headers
            .insert(header::HOST, HeaderValue::from_str(host)?);
    }
    let forwarded_for = conn.remote.ip().to_string();
    parts
        .headers
        .insert("x-forwarded-for", HeaderValue::from_str(&forwarded_for)?);
    parts
        .headers
        .insert("x-forwarded-host", HeaderValue::from_str(host)?);
    parts.headers.insert(
        "x-forwarded-proto",
        HeaderValue::from_static(if conn.secure { "https" } else { "http" }),
    );

    if let Some(client_upgrade) = client_upgrade {
        return forward_upgrade(
            &authority,
            Request::from_parts(parts, empty()),
            client_upgrade,
        )
        .await;
    }

    let req = Request::from_parts(
        parts,
        body.map_err(|e| -> BoxError { Box::new(e) }).boxed_unsync(),
    );
    let resp = match state.http_client.request(req).await {
        Ok(r) => r,
        Err(e) => {
            return Ok(crate::server::error_page(
                StatusCode::BAD_GATEWAY,
                "Upstream unavailable",
                &crate::server::html_escape(&format!("Could not reach {upstream}: {e}")),
            ));
        }
    };
    let (mut parts, body) = resp.into_parts();
    for h in HOP_BY_HOP {
        parts.headers.remove(*h);
    }
    Ok(Response::from_parts(
        parts,
        body.map_err(|e| -> BoxError { Box::new(e) }).boxed_unsync(),
    ))
}

async fn forward_upgrade(
    authority: &Authority,
    req: Request<ResponseBody>,
    client_upgrade: hyper::upgrade::OnUpgrade,
) -> Result<Response<ResponseBody>> {
    let addr = format!(
        "{}:{}",
        authority.host(),
        authority.port_u16().unwrap_or(80)
    );
    let stream = tokio::net::TcpStream::connect(&addr)
        .await
        .with_context(|| format!("could not connect to {addr}"))?;
    let (mut sender, connection) =
        hyper::client::conn::http1::handshake(TokioIo::new(stream)).await?;
    tokio::spawn(async move {
        if let Err(e) = connection.with_upgrades().await {
            tracing::debug!("upstream upgrade connection: {e}");
        }
    });
    let mut resp = sender.send_request(req).await?;
    if resp.status() != StatusCode::SWITCHING_PROTOCOLS {
        let (parts, body) = resp.into_parts();
        return Ok(Response::from_parts(
            parts,
            body.map_err(|e| -> BoxError { Box::new(e) }).boxed_unsync(),
        ));
    }
    let upstream_upgrade = hyper::upgrade::on(&mut resp);
    tokio::spawn(async move {
        match tokio::try_join!(client_upgrade, upstream_upgrade) {
            Ok((client, upstream)) => {
                let mut client = TokioIo::new(client);
                let mut upstream = TokioIo::new(upstream);
                let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
            }
            Err(e) => tracing::debug!("websocket upgrade failed: {e}"),
        }
    });
    let (parts, _) = resp.into_parts();
    Ok(Response::from_parts(parts, empty()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_upstreams() {
        let (a, p) = parse_upstream("http://127.0.0.1:5173").unwrap();
        assert_eq!(a.as_str(), "127.0.0.1:5173");
        assert_eq!(p, "");
        let (a, p) = parse_upstream("localhost:8080/app/").unwrap();
        assert_eq!(a.as_str(), "localhost:8080");
        assert_eq!(p, "/app");
        assert!(parse_upstream("https://x").is_err());
    }
}
