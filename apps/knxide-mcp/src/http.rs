//! MCP's Streamable HTTP transport, the stateless subset: `POST /mcp` with one JSON-RPC
//! message, answered with plain JSON (no SSE stream). `GET` and `DELETE` get 405, which tells
//! clients there is no server-to-client stream and no session to end.
//!
//! All clients share one [`Server`], so the bus connection and its telegram history outlive
//! any one client.

use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;

use crate::Server;

/// Path the MCP endpoint is served at.
pub const PATH: &str = "/mcp";

struct Http {
    server: Arc<Server>,
    token: Option<String>,
}

/// The HTTP app. With `token`, every request needs `Authorization: Bearer <token>`. Without
/// one, requests carrying an `Origin` header (browsers) are refused, as the MCP spec asks of
/// servers that guard against DNS rebinding.
pub fn router(server: Arc<Server>, token: Option<String>) -> Router {
    let state = Arc::new(Http { server, token });
    Router::new()
        .route(PATH, post(handle).get(not_allowed).delete(not_allowed))
        .with_state(state)
}

async fn handle(State(http): State<Arc<Http>>, headers: HeaderMap, body: String) -> Response {
    match &http.token {
        Some(token) => {
            let given = headers
                .get(header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer "));
            if !given.is_some_and(|g| constant_time_eq(g.trim().as_bytes(), token.as_bytes())) {
                return (
                    StatusCode::UNAUTHORIZED,
                    [(header::WWW_AUTHENTICATE, "Bearer")],
                    "missing or wrong bearer token\n",
                )
                    .into_response();
            }
        }
        None if headers.contains_key(header::ORIGIN) => {
            return (StatusCode::FORBIDDEN, "browser requests are not allowed\n").into_response();
        }
        None => {}
    }
    match http.server.handle(&body).await {
        Some(response) => (
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            )],
            response.to_string(),
        )
            .into_response(),
        // Notification or a response from the client.
        None => StatusCode::ACCEPTED.into_response(),
    }
}

async fn not_allowed() -> Response {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        [(header::ALLOW, "POST")],
        "this server offers no event stream; POST JSON-RPC messages\n",
    )
        .into_response()
}

/// Compare without leaking how long a matching prefix is.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}
