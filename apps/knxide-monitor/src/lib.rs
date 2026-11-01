//! The knxide-monitor server: one page, one WebSocket, one shared KNX connection.

mod bus;
mod health;
mod protocol;

use axum::Router;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
pub use bus::Bus;
pub use health::HealthConfig;
pub use protocol::Transport;
use protocol::{ClientMsg, ServerMsg};
use tokio::sync::broadcast;

const INDEX_HTML: &str = include_str!("../static/index.html");
const APP_CSS: &str = include_str!("../static/app.css");
const APP_JS: &str = include_str!("../static/app.js");

/// The HTTP router.
pub fn app(bus: Bus) -> Router {
    Router::new()
        .route(
            "/",
            get(|| async { asset("text/html; charset=utf-8", INDEX_HTML) }),
        )
        .route(
            "/app.css",
            get(|| async { asset("text/css; charset=utf-8", APP_CSS) }),
        )
        .route(
            "/app.js",
            get(|| async { asset("text/javascript; charset=utf-8", APP_JS) }),
        )
        .route("/ws", get(ws))
        .with_state(bus)
}

fn asset(content_type: &'static str, body: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        body,
    )
        .into_response()
}

/// Accept WebSockets only from our own page. Without this, any website open in the browser
/// could connect to `ws://localhost:8080/ws` and write to the bus.
fn same_origin(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|o| o.to_str().ok()) else {
        // Not a browser (command-line tools, tests): no cross-site risk.
        return true;
    };
    let host = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or_default();
    origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
        == Some(host)
}

async fn ws(upgrade: WebSocketUpgrade, headers: HeaderMap, State(bus): State<Bus>) -> Response {
    if !same_origin(&headers) {
        return (StatusCode::FORBIDDEN, "cross-origin WebSocket refused").into_response();
    }
    upgrade.on_upgrade(move |socket| session(socket, bus))
}

async fn session(mut socket: WebSocket, bus: Bus) {
    // Subscribe before the snapshot so nothing falls between them.
    let mut events = bus.subscribe();
    if send(&mut socket, &bus.hello()).await.is_err() {
        return;
    }
    loop {
        tokio::select! {
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    match serde_json::from_str::<ClientMsg>(&text) {
                        Ok(msg) => {
                            // Long-running requests (connect, read) run on their own so the
                            // socket keeps streaming meanwhile.
                            let bus = bus.clone();
                            tokio::spawn(async move { handle(&bus, msg).await });
                        }
                        Err(err) => tracing::debug!(%err, "bad message from browser"),
                    }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
            event = events.recv() => match event {
                Ok(json) => {
                    if socket.send(Message::Text(json.as_str().into())).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(n, "browser too slow; events skipped");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
        }
    }
}

/// Run one browser request. Replies go to every tab; each tab matches the ids it sent.
async fn handle(bus: &Bus, msg: ClientMsg) {
    let (id, outcome) = match msg {
        ClientMsg::Connect { gateway, transport } => {
            let _ = bus.connect(&gateway, transport).await;
            return;
        }
        ClientMsg::Disconnect => {
            bus.disconnect().await;
            return;
        }
        ClientMsg::Write {
            id,
            address,
            value,
            dpt,
        } => (id, bus.write(&address, &value, dpt.as_deref()).await),
        ClientMsg::Read { id, address } => (id, bus.read(&address).await),
        ClientMsg::Restart { id, address } => (id, bus.restart(&address).await),
        ClientMsg::SetDpt { id, address, dpt } => (id, bus.set_dpt(&address, dpt.as_deref())),
        ClientMsg::DismissIssue { key } => {
            bus.dismiss_issue(&key);
            return;
        }
        ClientMsg::LoadProject { id, name, content } => (id, bus.load_project(&name, &content)),
    };
    let (ok, message) = match outcome {
        Ok(message) => (true, message),
        Err(message) => (false, message),
    };
    bus.reply(ServerMsg::Result { id, ok, message });
}

async fn send(socket: &mut WebSocket, msg: &ServerMsg) -> Result<(), ()> {
    let json = serde_json::to_string(msg).map_err(drop)?;
    socket.send(Message::Text(json.into())).await.map_err(drop)
}
