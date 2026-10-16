//! A tiny authoritative DNS server: every name under the configured TLD
//! resolves to the loopback address. macOS sends queries for `*.test` here via
//! `/etc/resolver/test` (which can name a non-privileged port).

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use tokio::net::UdpSocket;

use crate::daemon::State;

const TYPE_A: u16 = 1;
const TYPE_ANY: u16 = 255;
const CLASS_IN: u16 = 1;
const RCODE_NXDOMAIN: u16 = 3;
const RCODE_REFUSED: u16 = 5;
const RCODE_FORMERR: u16 = 1;

pub async fn serve(socket: UdpSocket, state: Arc<State>) {
    let mut buf = [0u8; 1500];
    loop {
        let (len, peer): (usize, SocketAddr) = match socket.recv_from(&mut buf).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("dns recv: {e}");
                continue;
            }
        };
        let (tld, ip) = {
            let cfg = state.config();
            (cfg.tld.clone(), cfg.listen)
        };
        if let Some(reply) = answer(&buf[..len], &tld, ip) {
            let _ = socket.send_to(&reply, peer).await;
        }
    }
}

/// Builds the response for one query packet, or `None` if it is not worth answering.
pub fn answer(query: &[u8], tld: &str, ip: Ipv4Addr) -> Option<Vec<u8>> {
    if query.len() < 12 {
        return None;
    }
    let flags = u16::from_be_bytes([query[2], query[3]]);
    if flags & 0x8000 != 0 {
        return None; // a response, not a query
    }
    let qdcount = u16::from_be_bytes([query[4], query[5]]);
    let rd = flags & 0x0100;

    let reply = |rcode: u16, question: &[u8], answers: &[u8], ancount: u16| {
        let mut out = Vec::with_capacity(12 + question.len() + answers.len());
        out.extend_from_slice(&query[0..2]);
        // QR=1, opcode copied, AA=1, RD copied
        let opcode = flags & 0x7800;
        out.extend_from_slice(&(0x8000 | opcode | 0x0400 | rd | rcode).to_be_bytes());
        out.extend_from_slice(&(if question.is_empty() { 0u16 } else { 1u16 }).to_be_bytes());
        out.extend_from_slice(&ancount.to_be_bytes());
        out.extend_from_slice(&[0, 0, 0, 0]);
        out.extend_from_slice(question);
        out.extend_from_slice(answers);
        out
    };

    if qdcount != 1 || flags & 0x7800 != 0 {
        return Some(reply(RCODE_FORMERR, &[], &[], 0));
    }

    // Parse the question name.
    let mut pos = 12;
    let mut labels: Vec<String> = Vec::new();
    loop {
        let len = *query.get(pos)? as usize;
        pos += 1;
        if len == 0 {
            break;
        }
        if len & 0xC0 != 0 {
            return Some(reply(RCODE_FORMERR, &[], &[], 0));
        }
        let label = query.get(pos..pos + len)?;
        labels.push(String::from_utf8_lossy(label).to_ascii_lowercase());
        pos += len;
    }
    let qtype = u16::from_be_bytes([*query.get(pos)?, *query.get(pos + 1)?]);
    let qclass = u16::from_be_bytes([*query.get(pos + 2)?, *query.get(pos + 3)?]);
    let question = &query[12..pos + 4];

    let in_zone = labels.last().is_some_and(|l| l == tld);
    if !in_zone || qclass != CLASS_IN {
        return Some(reply(RCODE_REFUSED, question, &[], 0));
    }
    if labels.len() < 2 {
        // The bare TLD itself has no address.
        return Some(reply(RCODE_NXDOMAIN, question, &[], 0));
    }
    if qtype == TYPE_A || qtype == TYPE_ANY {
        let mut rr = Vec::with_capacity(16);
        rr.extend_from_slice(&[0xC0, 0x0C]); // pointer to the question name
        rr.extend_from_slice(&TYPE_A.to_be_bytes());
        rr.extend_from_slice(&CLASS_IN.to_be_bytes());
        rr.extend_from_slice(&60u32.to_be_bytes());
        rr.extend_from_slice(&4u16.to_be_bytes());
        rr.extend_from_slice(&ip.octets());
        return Some(reply(0, question, &rr, 1));
    }
    // AAAA and everything else: the name exists, but has no such record.
    // Answering AAAA with nothing makes clients go straight to IPv4.
    Some(reply(0, question, &[], 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(name: &str, qtype: u16) -> Vec<u8> {
        let mut q = vec![0xAB, 0xCD, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0];
        for label in name.split('.') {
            q.push(label.len() as u8);
            q.extend_from_slice(label.as_bytes());
        }
        q.push(0);
        q.extend_from_slice(&qtype.to_be_bytes());
        q.extend_from_slice(&CLASS_IN.to_be_bytes());
        q
    }

    #[test]
    fn answers_a_records_in_zone() {
        let r = answer(&query("MyApp.test", TYPE_A), "test", Ipv4Addr::LOCALHOST).unwrap();
        assert_eq!(&r[0..2], &[0xAB, 0xCD]);
        assert_eq!(r[3] & 0x0F, 0, "rcode");
        assert_eq!(u16::from_be_bytes([r[6], r[7]]), 1, "ancount");
        assert_eq!(&r[r.len() - 4..], &[127, 0, 0, 1]);
    }

    #[test]
    fn empty_aaaa_and_refuses_foreign_names() {
        let r = answer(&query("app.test", 28), "test", Ipv4Addr::LOCALHOST).unwrap();
        assert_eq!(r[3] & 0x0F, 0);
        assert_eq!(u16::from_be_bytes([r[6], r[7]]), 0);

        let r = answer(&query("example.com", TYPE_A), "test", Ipv4Addr::LOCALHOST).unwrap();
        assert_eq!(r[3] & 0x0F, RCODE_REFUSED as u8);
    }

    #[test]
    fn ignores_garbage() {
        assert!(answer(&[1, 2, 3], "test", Ipv4Addr::LOCALHOST).is_none());
        let mut q = query("app.test", TYPE_A);
        q.truncate(q.len() - 3);
        assert!(answer(&q, "test", Ipv4Addr::LOCALHOST).is_none());
    }
}
