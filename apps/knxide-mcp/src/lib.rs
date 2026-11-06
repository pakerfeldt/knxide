//! `knxide-mcp`: a Model Context Protocol server for a KNX bus.
//!
//! Speaks MCP's stdio transport (newline-delimited JSON-RPC 2.0 on stdin and stdout, see
//! [`Server::serve`]) or Streamable HTTP ([`http::router`]). Only the parts a tools-only server
//! needs are implemented: `initialize`, `ping`, `tools/list` and
//! `tools/call`. Requests are handled concurrently, so a slow bus read does not hold up
//! other calls.
//!
//! Tools: `bus_status`, `find_group_addresses`, `read_group_address`, `recent_telegrams`, and
//! `write_group_address` when writing is enabled.

mod bus;
pub mod http;
mod tools;

use std::sync::Arc;

use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;

pub use bus::{Bus, Link};

/// Protocol versions this server can speak, newest first.
const PROTOCOL_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// JSON-RPC error codes.
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

const INSTRUCTIONS: &str = "Tools for a KNX building automation bus. Group addresses are the \
    datapoints of the building (a light's switch, a room's temperature, a blind's position). \
    Use find_group_addresses to look one up by room or function before reading or writing it. \
    Values are text in the format the address's datapoint type accepts (shown as `accepts`).";

/// The MCP server: tool dispatch on top of a [`Bus`].
pub struct Server {
    bus: Bus,
    allow_write: bool,
}

impl Server {
    /// A server for `bus`. `allow_write` adds the `write_group_address` tool.
    pub fn new(bus: Bus, allow_write: bool) -> Self {
        Self { bus, allow_write }
    }

    /// The bus.
    pub fn bus(&self) -> &Bus {
        &self.bus
    }

    /// Serve newline-delimited JSON-RPC from `input` to `output` until `input` ends, then
    /// wait for requests still running.
    pub async fn serve<R, W>(self: Arc<Self>, input: R, mut output: W) -> std::io::Result<()>
    where
        R: AsyncBufRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
        let writer = async move {
            while let Some(message) = rx.recv().await {
                let mut line = message.to_string();
                line.push('\n');
                output.write_all(line.as_bytes()).await?;
                output.flush().await?;
            }
            Ok::<_, std::io::Error>(())
        };
        let reader = async move {
            let mut lines = input.lines();
            while let Some(line) = lines.next_line().await? {
                if line.trim().is_empty() {
                    continue;
                }
                let (server, tx) = (self.clone(), tx.clone());
                tokio::spawn(async move {
                    if let Some(response) = server.handle(&line).await {
                        let _ = tx.send(response);
                    }
                });
            }
            Ok::<_, std::io::Error>(())
        };
        let (read, write) = tokio::join!(reader, writer);
        read.and(write)
    }

    /// Handle one JSON-RPC message. Returns the response, or `None` for notifications.
    pub async fn handle(&self, line: &str) -> Option<Value> {
        let message: Value = match serde_json::from_str(line) {
            Ok(message) => message,
            Err(err) => {
                return Some(error(
                    Value::Null,
                    PARSE_ERROR,
                    &format!("parse error: {err}"),
                ));
            }
        };
        let Some(obj) = message.as_object() else {
            return Some(error(
                Value::Null,
                INVALID_REQUEST,
                "expected a JSON object",
            ));
        };
        let id = obj.get("id").cloned();
        let Some(method) = obj.get("method").and_then(Value::as_str) else {
            // A response to a request of ours; we send none, so ignore it.
            return id
                .is_none()
                .then(|| error(Value::Null, INVALID_REQUEST, "no method"));
        };
        let empty = Map::new();
        let params = obj
            .get("params")
            .and_then(Value::as_object)
            .unwrap_or(&empty);
        let Some(id) = id else {
            // Notification: `notifications/initialized`, `notifications/cancelled`, …
            tracing::debug!(method, "notification");
            return None;
        };
        let result = match method {
            "initialize" => Ok(initialize(params)),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tools::definitions(self.allow_write) })),
            "tools/call" => self.call_tool(params).await,
            _ => Err((METHOD_NOT_FOUND, format!("method {method:?} not found"))),
        };
        Some(match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err((code, message)) => error(id, code, &message),
        })
    }

    async fn call_tool(&self, params: &Map<String, Value>) -> Result<Value, (i64, String)> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or((INVALID_PARAMS, "tools/call needs a tool name".into()))?;
        let empty = Map::new();
        let args = match params.get("arguments") {
            None | Some(Value::Null) => &empty,
            Some(Value::Object(args)) => args,
            Some(_) => return Err((INVALID_PARAMS, "arguments must be an object".into())),
        };
        tracing::info!(name, ?args, "tool call");
        let outcome = tools::call(&self.bus, self.allow_write, name, args)
            .await
            .ok_or_else(|| {
                let hint = if name == "write_group_address" {
                    " (writing is disabled; start knxide-mcp with --allow-write)"
                } else {
                    ""
                };
                (INVALID_PARAMS, format!("unknown tool {name:?}{hint}"))
            })?;
        Ok(match outcome {
            Ok(value) => json!({
                "content": [{ "type": "text", "text": value.to_string() }],
                "structuredContent": value,
                "isError": false,
            }),
            Err(message) => {
                tracing::info!(name, %message, "tool failed");
                json!({
                    "content": [{ "type": "text", "text": message }],
                    "isError": true,
                })
            }
        })
    }
}

fn initialize(params: &Map<String, Value>) -> Value {
    let requested = params.get("protocolVersion").and_then(Value::as_str);
    let version = requested
        .and_then(|r| PROTOCOL_VERSIONS.iter().find(|v| **v == r))
        .or(PROTOCOL_VERSIONS.first())
        .copied()
        .unwrap_or_default();
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": {
            "name": "knxide-mcp",
            "title": "KNX bus",
            "version": env!("CARGO_PKG_VERSION"),
        },
        "instructions": INSTRUCTIONS,
    })
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
}
