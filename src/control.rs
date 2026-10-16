//! Client side of the daemon's control socket.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::paths::paths;

pub fn send(request: Value) -> Result<Value> {
    let path = paths().control_socket();
    let mut stream = UnixStream::connect(&path).context("the bird daemon is not running")?;
    stream.set_read_timeout(Some(Duration::from_secs(15)))?;
    stream.write_all(format!("{request}\n").as_bytes())?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    let reply: Value = serde_json::from_str(&line).context("invalid reply from daemon")?;
    if reply.get("ok").and_then(Value::as_bool) != Some(true) {
        bail!(
            "daemon: {}",
            reply
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("unknown error")
        );
    }
    Ok(reply)
}

pub fn is_running() -> bool {
    send(serde_json::json!({"cmd": "status"})).is_ok()
}
