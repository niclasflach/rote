//! Minimal LSP client. Spawns a language server as a subprocess (the same
//! model LazyVim/mason use — `rust-analyzer`, `pyright`, `clangd`, etc. are
//! just binaries on `PATH`) and speaks JSON-RPC 2.0 over stdio with
//! `Content-Length` framing.
//!
//! This gives `rote-app` completion, hover, diagnostics and go-to-definition
//! without reimplementing any language intelligence: `rote-lsp` only owns
//! transport and request/response bookkeeping, `lsp-types` supplies the
//! protocol's data shapes.

mod transport;

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot, Mutex};

pub use lsp_types;

#[derive(Debug, thiserror::Error)]
pub enum LspError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("server closed the connection")]
    Closed,
    #[error("server returned an error response: {0}")]
    Rpc(Value),
    #[error("failed to (de)serialize JSON-RPC payload: {0}")]
    Json(#[from] serde_json::Error),
}

/// A server-sent notification (diagnostics, log messages, etc.) that has no
/// matching request. Delivered on [`LspClient::notifications`].
#[derive(Debug, Clone)]
pub struct ServerNotification {
    pub method: String,
    pub params: Value,
}

type PendingMap = Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value, Value>>>>>;

/// A running language server connection.
pub struct LspClient {
    child: Child,
    stdin: Mutex<tokio::process::ChildStdin>,
    next_id: AtomicI64,
    pending: PendingMap,
    notifications: mpsc::UnboundedReceiver<ServerNotification>,
}

impl LspClient {
    /// Spawn `command args...` and start reading its stdout in the
    /// background. Requests can be sent as soon as this returns; call
    /// [`LspClient::initialize`] first per the LSP spec.
    pub fn spawn(command: &str, args: &[&str]) -> Result<Self, LspError> {
        let mut child = Command::new(command)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;

        let stdin = child.stdin.take().expect("stdin was piped");
        let stdout = child.stdout.take().expect("stdout was piped");

        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let (notif_tx, notif_rx) = mpsc::unbounded_channel();

        tokio::spawn(transport::read_loop(stdout, pending.clone(), notif_tx));

        Ok(Self {
            child,
            stdin: Mutex::new(stdin),
            next_id: AtomicI64::new(1),
            pending,
            notifications: notif_rx,
        })
    }

    /// Send the LSP `initialize` handshake and return the server's
    /// capabilities.
    pub async fn initialize(
        &self,
        params: lsp_types::InitializeParams,
    ) -> Result<lsp_types::InitializeResult, LspError> {
        let result = self.request("initialize", serde_json::to_value(params)?).await?;
        self.notify("initialized", serde_json::json!({})).await?;
        Ok(serde_json::from_value(result)?)
    }

    /// Send a JSON-RPC request and await the matching response.
    pub async fn request(&self, method: &str, params: Value) -> Result<Value, LspError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        self.write_message(&payload).await?;

        match rx.await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(err)) => Err(LspError::Rpc(err)),
            Err(_) => Err(LspError::Closed),
        }
    }

    /// Convenience wrapper around [`LspClient::request`] that deserializes
    /// the result into a typed `lsp-types` response.
    pub async fn request_typed<T: DeserializeOwned>(
        &self,
        method: &str,
        params: Value,
    ) -> Result<T, LspError> {
        let value = self.request(method, params).await?;
        Ok(serde_json::from_value(value)?)
    }

    /// Send a JSON-RPC notification (no response expected).
    pub async fn notify(&self, method: &str, params: Value) -> Result<(), LspError> {
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        self.write_message(&payload).await
    }

    async fn write_message(&self, payload: &Value) -> Result<(), LspError> {
        let mut stdin = self.stdin.lock().await;
        transport::write_message(&mut stdin, payload).await
    }

    /// Pull the next server-initiated notification (diagnostics, log
    /// messages, `workspace/*` requests turned into best-effort notifications).
    pub async fn next_notification(&mut self) -> Option<ServerNotification> {
        self.notifications.recv().await
    }

    pub async fn shutdown(&mut self) -> Result<(), LspError> {
        let _ = self.request("shutdown", Value::Null).await;
        self.notify("exit", Value::Null).await?;
        let _ = self.child.kill().await;
        Ok(())
    }
}
