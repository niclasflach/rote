use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout};
use tokio::sync::mpsc;

use crate::{PendingMap, ServerNotification};

/// Write one JSON-RPC message with the `Content-Length` header the LSP spec
/// requires (this is the same framing used over stdio by every language
/// server — VS Code, Neovim's built-in client, etc. all speak this).
pub(crate) async fn write_message(
    stdin: &mut ChildStdin,
    payload: &Value,
) -> Result<(), crate::LspError> {
    let body = serde_json::to_vec(payload)?;
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    stdin.write_all(header.as_bytes()).await?;
    stdin.write_all(&body).await?;
    stdin.flush().await?;
    Ok(())
}

/// Background task: read `Content-Length`-framed JSON-RPC messages from the
/// server's stdout for the lifetime of the connection, routing each one to
/// either a pending request's oneshot channel (by `id`) or the notification
/// channel (no `id`).
pub(crate) async fn read_loop(
    stdout: ChildStdout,
    pending: PendingMap,
    notifications: mpsc::UnboundedSender<ServerNotification>,
) {
    let mut reader = BufReader::new(stdout);
    loop {
        match read_message(&mut reader).await {
            Ok(Some(msg)) => route_message(msg, &pending, &notifications).await,
            Ok(None) => break, // server closed stdout
            Err(err) => {
                tracing::warn!("lsp transport error: {err:#}");
                break;
            }
        }
    }
}

async fn read_message(
    reader: &mut BufReader<ChildStdout>,
) -> Result<Option<Value>, crate::LspError> {
    let mut content_length: Option<usize> = None;

    // Headers, one per line, terminated by a blank line.
    loop {
        let line = read_header_line(reader).await?;
        let Some(line) = line else { return Ok(None) };
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length:") {
            content_length = Some(value.trim().parse().unwrap_or(0));
        }
    }

    let len = content_length.unwrap_or(0);
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body).await?;
    Ok(Some(serde_json::from_slice(&body)?))
}

/// Read one `\r\n`-terminated header line by hand — `AsyncBufReadExt::read_line`
/// wants valid UTF-8 and headers are ASCII, but doing it byte-by-byte keeps
/// this robust against any body bytes that follow without a line boundary.
async fn read_header_line(
    reader: &mut BufReader<ChildStdout>,
) -> Result<Option<String>, crate::LspError> {
    let mut bytes = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let n = reader.read(&mut byte).await?;
        if n == 0 {
            return Ok(None);
        }
        if byte[0] == b'\n' {
            if bytes.last() == Some(&b'\r') {
                bytes.pop();
            }
            break;
        }
        bytes.push(byte[0]);
    }
    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
}

async fn route_message(
    msg: Value,
    pending: &PendingMap,
    notifications: &mpsc::UnboundedSender<ServerNotification>,
) {
    let id = msg.get("id").and_then(Value::as_i64);

    if let Some(id) = id {
        if msg.get("method").is_none() {
            // Response to a request we sent.
            if let Some(tx) = pending.lock().await.remove(&id) {
                let result = if let Some(err) = msg.get("error") {
                    Err(err.clone())
                } else {
                    Ok(msg.get("result").cloned().unwrap_or(Value::Null))
                };
                let _ = tx.send(result);
            }
            return;
        }
    }

    // Server->client request or notification.
    if let Some(method) = msg.get("method").and_then(Value::as_str) {
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let _ = notifications.send(ServerNotification {
            method: method.to_string(),
            params,
        });
    }
}
