//! The Streamable HTTP transport, spoken as raw HTTP/1.1.

use std::net::SocketAddr;
use std::sync::Arc;

use knxide_mcp::{Bus, Server, http};
use knxide_project::GroupAddressTable;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

async fn serve(token: Option<&str>) -> SocketAddr {
    let bus = Bus::new(None, GroupAddressTable::default(), None);
    let server = Arc::new(Server::new(bus, false));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = http::router(server, token.map(str::to_string));
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

/// Send one request; returns (status, headers in lower case, body).
async fn send(
    addr: SocketAddr,
    method: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> (u16, String, String) {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let mut request = format!(
        "{method} /mcp HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\
         Content-Type: application/json\r\nAccept: application/json, text/event-stream\r\n\
         Content-Length: {}\r\n",
        body.len()
    );
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");
    request.push_str(body);
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    let (head, body) = response.split_once("\r\n\r\n").unwrap();
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    (status, head.to_lowercase(), body.to_string())
}

fn initialize() -> String {
    json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": { "protocolVersion": "2025-06-18" },
    })
    .to_string()
}

#[tokio::test]
async fn post_returns_json() {
    let addr = serve(None).await;
    let (status, head, body) = send(addr, "POST", &[], &initialize()).await;
    assert_eq!(status, 200);
    assert!(head.contains("content-type: application/json"), "{head}");
    let msg: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(msg["id"], 1);
    assert_eq!(msg["result"]["protocolVersion"], "2025-06-18");

    let note = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }).to_string();
    let (status, _, body) = send(addr, "POST", &[], &note).await;
    assert_eq!(status, 202);
    assert!(body.is_empty());

    let call = json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": { "name": "bus_status" },
    })
    .to_string();
    let (status, _, body) = send(addr, "POST", &[], &call).await;
    assert_eq!(status, 200);
    let msg: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(msg["result"]["structuredContent"]["write_enabled"], false);
}

#[tokio::test]
async fn no_stream_or_session() {
    let addr = serve(None).await;
    for method in ["GET", "DELETE"] {
        let (status, head, _) = send(addr, method, &[], "").await;
        assert_eq!(status, 405, "{method}");
        assert!(head.contains("allow: post"), "{head}");
    }
}

#[tokio::test]
async fn bearer_token_required() {
    let addr = serve(Some("s3cret")).await;
    let (status, head, _) = send(addr, "POST", &[], &initialize()).await;
    assert_eq!(status, 401);
    assert!(head.contains("www-authenticate: bearer"), "{head}");

    let wrong = [("Authorization", "Bearer nope")];
    assert_eq!(send(addr, "POST", &wrong, &initialize()).await.0, 401);

    let right = [("Authorization", "Bearer s3cret")];
    let (status, _, body) = send(addr, "POST", &right, &initialize()).await;
    assert_eq!(status, 200, "{body}");

    // With a token, a browser origin is fine: the token is the guard.
    let browser = [
        ("Authorization", "Bearer s3cret"),
        ("Origin", "http://example.com"),
    ];
    assert_eq!(send(addr, "POST", &browser, &initialize()).await.0, 200);
}

#[tokio::test]
async fn browsers_refused_without_token() {
    let addr = serve(None).await;
    let browser = [("Origin", "http://evil.example")];
    assert_eq!(send(addr, "POST", &browser, &initialize()).await.0, 403);
}
