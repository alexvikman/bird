//! A small FastCGI client (responder role) for talking to PHP-FPM.
//!
//! The request body is streamed into FCGI_STDIN and the response body is
//! streamed back as it arrives, so large uploads and downloads never have to
//! sit in memory.

use anyhow::{Context, Result, bail};
use bytes::{Buf, Bytes, BytesMut};
use http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use http_body_util::BodyExt;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::server::{BoxError, ResponseBody};

const VERSION: u8 = 1;
const BEGIN_REQUEST: u8 = 1;
const END_REQUEST: u8 = 3;
const PARAMS: u8 = 4;
const STDIN: u8 = 5;
const STDOUT: u8 = 6;
const STDERR: u8 = 7;
const ROLE_RESPONDER: u16 = 1;
const REQUEST_ID: u16 = 1;
const MAX_RECORD: usize = 65535;

fn header(kind: u8, len: usize) -> [u8; 8] {
    let id = REQUEST_ID.to_be_bytes();
    let len = (len as u16).to_be_bytes();
    [VERSION, kind, id[0], id[1], len[0], len[1], 0, 0]
}

async fn write_record<W: AsyncWrite + Unpin>(
    w: &mut W,
    kind: u8,
    data: &[u8],
) -> std::io::Result<()> {
    if data.is_empty() {
        return w.write_all(&header(kind, 0)).await;
    }
    for chunk in data.chunks(MAX_RECORD) {
        w.write_all(&header(kind, chunk.len())).await?;
        w.write_all(chunk).await?;
    }
    Ok(())
}

fn encode_len(out: &mut Vec<u8>, len: usize) {
    if len < 128 {
        out.push(len as u8);
    } else {
        out.extend_from_slice(&((len as u32) | 0x8000_0000).to_be_bytes());
    }
}

pub fn encode_params(params: &[(String, String)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (name, value) in params {
        encode_len(&mut out, name.len());
        encode_len(&mut out, value.len());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(value.as_bytes());
    }
    out
}

pub struct FcgiResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: ResponseBody,
}

/// Sends one request over `stream` and returns once the response headers are in.
pub async fn request<S, B>(
    stream: S,
    params: Vec<(String, String)>,
    body: B,
) -> Result<FcgiResponse>
where
    S: AsyncRead + AsyncWrite + Send + 'static,
    B: http_body::Body<Data = Bytes> + Send + Unpin + 'static,
    B::Error: Into<BoxError> + Send,
{
    let (mut reader, mut writer) = tokio::io::split(stream);

    let writer_task = tokio::spawn(async move {
        let mut body = body;
        let sent: std::io::Result<()> = async {
            let mut begin = Vec::with_capacity(8);
            begin.extend_from_slice(&ROLE_RESPONDER.to_be_bytes());
            begin.extend_from_slice(&[0, 0, 0, 0, 0, 0]); // flags = 0: close after request
            write_record(&mut writer, BEGIN_REQUEST, &begin).await?;
            write_record(&mut writer, PARAMS, &encode_params(&params)).await?;
            write_record(&mut writer, PARAMS, &[]).await?;
            while let Some(frame) = body.frame().await {
                let frame = frame.map_err(|e| std::io::Error::other(e.into()))?;
                if let Ok(data) = frame.into_data() {
                    write_record(&mut writer, STDIN, &data).await?;
                }
            }
            write_record(&mut writer, STDIN, &[]).await?;
            writer.flush().await
        }
        .await;
        if let Err(e) = sent {
            // E.g. the client aborted its upload. Dropping the write half would
            // not close the socket (the read half shares it), and php-fpm would
            // wait for the rest of the body forever; half-close it instead, so
            // php-fpm sees EOF, gives the worker back and closes, which also
            // ends the read below.
            let _ = writer.shutdown().await;
            return Err(e);
        }
        // Keep the write half alive until the reader is done.
        Ok::<_, std::io::Error>(writer)
    });

    let mut stdout = BytesMut::new();
    let mut ended = false;
    let head_end = loop {
        if let Some(pos) = find_head_end(&stdout) {
            break Some(pos);
        }
        match read_record(&mut reader).await? {
            Record::Stdout(data) => stdout.extend_from_slice(&data),
            Record::Stderr(data) => log_stderr(&data),
            Record::End => {
                ended = true;
                break find_head_end(&stdout);
            }
            Record::Other => {}
        }
    };
    // If the writer failed (e.g. client aborted the upload), surface that.
    if writer_task.is_finished()
        && let Ok(Err(e)) = writer_task.await
        && !ended
    {
        bail!("writing request to php-fpm failed: {e}");
    }

    let (head, rest) = match head_end {
        Some((head_len, sep_len)) => {
            let head = stdout.split_to(head_len);
            stdout.advance(sep_len);
            (head.freeze(), stdout.freeze())
        }
        None if stdout.is_empty() => (Bytes::new(), Bytes::new()),
        None => bail!("php-fpm sent a response without headers"),
    };
    let (status, headers) = parse_head(&head)?;

    let (mut tx, channel) = http_body_util::channel::Channel::<Bytes, BoxError>::new(8);
    tokio::spawn(async move {
        if !rest.is_empty() && tx.send_data(rest).await.is_err() {
            return;
        }
        if ended {
            return;
        }
        loop {
            match read_record(&mut reader).await {
                Ok(Record::Stdout(data)) => {
                    if !data.is_empty() && tx.send_data(data).await.is_err() {
                        return; // client went away
                    }
                }
                Ok(Record::Stderr(data)) => log_stderr(&data),
                Ok(Record::End) => return,
                Ok(Record::Other) => {}
                Err(e) => {
                    tx.abort(e.into());
                    return;
                }
            }
        }
    });

    Ok(FcgiResponse {
        status,
        headers,
        body: channel.boxed_unsync(),
    })
}

enum Record {
    Stdout(Bytes),
    Stderr(Bytes),
    End,
    Other,
}

async fn read_record<R: AsyncRead + Unpin>(r: &mut R) -> Result<Record> {
    let mut head = [0u8; 8];
    r.read_exact(&mut head)
        .await
        .context("php-fpm closed the connection")?;
    let kind = head[1];
    let len = u16::from_be_bytes([head[4], head[5]]) as usize;
    let padding = head[6] as usize;
    let mut data = vec![0u8; len + padding];
    r.read_exact(&mut data).await?;
    data.truncate(len);
    Ok(match kind {
        STDOUT => Record::Stdout(Bytes::from(data)),
        STDERR => Record::Stderr(Bytes::from(data)),
        END_REQUEST => Record::End,
        _ => Record::Other,
    })
}

fn log_stderr(data: &[u8]) {
    let text = String::from_utf8_lossy(data);
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        tracing::warn!(target: "php", "{line}");
    }
}

/// Position and length of the blank line separating CGI headers from the body.
fn find_head_end(buf: &[u8]) -> Option<(usize, usize)> {
    let crlf = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|p| (p, 4));
    let lf = buf.windows(2).position(|w| w == b"\n\n").map(|p| (p, 2));
    match (crlf, lf) {
        (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
        (a, b) => a.or(b),
    }
}

fn parse_head(head: &[u8]) -> Result<(StatusCode, HeaderMap)> {
    let mut status = None;
    let mut headers = HeaderMap::new();
    for line in head.split(|&b| b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            continue;
        }
        let Some(colon) = line.iter().position(|&b| b == b':') else {
            continue;
        };
        let name = &line[..colon];
        let value = line[colon + 1..].trim_ascii();
        if name.eq_ignore_ascii_case(b"status") {
            let code = value.split(|&b| b == b' ').next().unwrap_or_default();
            status = std::str::from_utf8(code)
                .ok()
                .and_then(|c| c.parse::<u16>().ok())
                .and_then(|c| StatusCode::from_u16(c).ok());
            continue;
        }
        if let (Ok(name), Ok(value)) =
            (HeaderName::from_bytes(name), HeaderValue::from_bytes(value))
        {
            headers.append(name, value);
        }
    }
    let status = status.unwrap_or(if headers.contains_key(http::header::LOCATION) {
        StatusCode::FOUND
    } else {
        StatusCode::OK
    });
    Ok((status, headers))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn param_lengths() {
        let long = "x".repeat(200);
        let enc = encode_params(&[("A".into(), "b".into()), ("L".into(), long.clone())]);
        assert_eq!(&enc[..4], &[1, 1, b'A', b'b']);
        assert_eq!(enc[4], 1);
        assert_eq!(&enc[5..9], &(200u32 | 0x8000_0000).to_be_bytes());
        assert_eq!(enc.len(), 4 + 1 + 4 + 1 + 200);
    }

    #[test]
    fn parses_cgi_headers() {
        let (status, h) = parse_head(b"Status: 404 Not Found\r\nSet-Cookie: a=1\r\nSet-Cookie: b=2\r\nContent-Type: text/html").unwrap();
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(h.get_all("set-cookie").iter().count(), 2);

        let (status, _) = parse_head(b"Location: /login").unwrap();
        assert_eq!(status, StatusCode::FOUND);
    }

    #[test]
    fn finds_header_separator() {
        assert_eq!(find_head_end(b"A: b\r\n\r\nbody"), Some((4, 4)));
        assert_eq!(find_head_end(b"A: b\n\nbody"), Some((4, 2)));
        assert_eq!(find_head_end(b"A: b\r\n"), None);
    }

    #[tokio::test]
    async fn round_trip_against_fake_fpm() {
        let (client, mut server) = tokio::io::duplex(1 << 16);
        let fake = tokio::spawn(async move {
            // Read until the empty STDIN record, collecting the body.
            let mut body = Vec::new();
            loop {
                let mut h = [0u8; 8];
                server.read_exact(&mut h).await.unwrap();
                let len = u16::from_be_bytes([h[4], h[5]]) as usize;
                let mut d = vec![0u8; len + h[6] as usize];
                server.read_exact(&mut d).await.unwrap();
                if h[1] == STDIN {
                    if len == 0 {
                        break;
                    }
                    body.extend_from_slice(&d[..len]);
                }
            }
            let out = format!(
                "Content-Type: text/plain\r\n\r\ngot {}",
                String::from_utf8(body).unwrap()
            );
            write_record(&mut server, STDOUT, out.as_bytes())
                .await
                .unwrap();
            write_record(&mut server, STDOUT, b" bytes").await.unwrap();
            write_record(&mut server, END_REQUEST, &[0; 8])
                .await
                .unwrap();
        });
        let resp = request(
            client,
            vec![("A".into(), "b".into())],
            http_body_util::Full::new(Bytes::from("hello")),
        )
        .await
        .unwrap();
        assert_eq!(resp.status, StatusCode::OK);
        let body = resp.body.collect().await.unwrap().to_bytes();
        assert_eq!(&body[..], b"got hello bytes");
        fake.await.unwrap();
    }

    #[tokio::test]
    async fn an_aborted_upload_ends_the_request() {
        let (client, mut server) = tokio::io::duplex(1 << 16);
        // php-fpm reads the request until the connection is half-closed, then
        // closes it, as it does when its input ends early.
        let fake = tokio::spawn(async move {
            let mut got = Vec::new();
            server.read_to_end(&mut got).await.unwrap();
        });
        // A body that fails after its first chunk, like an aborted upload.
        let (mut tx, body) = http_body_util::channel::Channel::<Bytes, BoxError>::new(1);
        tokio::spawn(async move {
            tx.send_data(Bytes::from("part of a file")).await.unwrap();
            tx.abort("client went away".into());
        });
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            request(client, vec![("A".into(), "b".into())], body),
        )
        .await
        .expect("the request must end, not wait for the rest of the body");
        assert!(result.is_err());
        fake.await.unwrap();
    }
}
