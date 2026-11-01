//! knxide-dashboard: a customisable KNX dashboard served to phones, tablets and desktops.
//!
//! A library only so the server can be tested; not a public API.

// The JSON wire types mirror the page's protocol and are described where they are declared.
#![allow(missing_docs)]

pub mod app;
pub mod config;

use axum::Router;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde::Deserialize;
use tokio::sync::broadcast;

pub use app::App;
use app::ServerMsg;
use config::Dashboard;

const INDEX_HTML: &str = include_str!("../static/index.html");
const APP_CSS: &str = include_str!("../static/app.css");
const APP_JS: &str = include_str!("../static/app.js");
const MANIFEST: &str = include_str!("../static/manifest.webmanifest");
const ICON: &str = include_str!("../static/icon.svg");

/// Browser to server.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMsg {
    Write {
        id: u64,
        ga: String,
        value: String,
        dpt: Option<String>,
    },
    Read {
        ga: String,
    },
    Save {
        id: u64,
        dashboard: Dashboard,
    },
    Suggest {
        id: u64,
        path: Vec<String>,
    },
}

/// The HTTP router.
pub fn router(app: App) -> Router {
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
        .route(
            "/manifest.webmanifest",
            get(|| async { asset("application/manifest+json", MANIFEST) }),
        )
        .route("/icon.svg", get(|| async { asset("image/svg+xml", ICON) }))
        .route("/ws", get(ws))
        .with_state(app)
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

/// Only our own page may open the WebSocket; otherwise any website open in a browser on
/// the home network could switch the lights.
fn same_origin(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|o| o.to_str().ok()) else {
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

async fn ws(upgrade: WebSocketUpgrade, headers: HeaderMap, State(app): State<App>) -> Response {
    if !same_origin(&headers) {
        return (StatusCode::FORBIDDEN, "cross-origin WebSocket refused").into_response();
    }
    upgrade.on_upgrade(move |socket| session(socket, app))
}

async fn session(mut socket: WebSocket, app: App) {
    let mut events = app.subscribe();
    let Ok(hello) = serde_json::to_string(&app.hello()) else {
        return;
    };
    if socket.send(Message::Text(hello.into())).await.is_err() {
        return;
    }
    loop {
        tokio::select! {
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Text(text))) => match serde_json::from_str::<ClientMsg>(&text) {
                    Ok(msg) => {
                        let app = app.clone();
                        tokio::spawn(async move { handle(&app, msg).await });
                    }
                    Err(err) => tracing::debug!(%err, "bad message from browser"),
                },
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
            event = events.recv() => match event {
                Ok(json) => {
                    if socket.send(Message::Text(json.as_str().into())).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => break,
            },
        }
    }
}

async fn handle(app: &App, msg: ClientMsg) {
    let reply = |id, outcome: Result<String, String>| {
        let (ok, message) = match outcome {
            Ok(m) => (true, m),
            Err(m) => (false, m),
        };
        ServerMsg::Result {
            id,
            ok,
            message,
            tiles: None,
        }
    };
    let msg = match msg {
        ClientMsg::Write { id, ga, value, dpt } => {
            reply(id, app.write(&ga, &value, dpt.as_deref()).await)
        }
        ClientMsg::Read { ga } => {
            app.read(&ga).await;
            return;
        }
        ClientMsg::Save { id, dashboard } => reply(id, app.save(dashboard).await),
        ClientMsg::Suggest { id, path } => {
            let tiles = app.suggest(&path);
            let message = format!("{} tiles suggested", tiles.len());
            ServerMsg::Result {
                id,
                ok: true,
                message,
                tiles: Some(tiles),
            }
        }
    };
    app.broadcast(&msg);
}
