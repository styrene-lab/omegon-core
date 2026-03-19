//! Embedded web server — localhost HTTP + WebSocket for the agent dashboard.
//!
//! Started on demand by `/dash open`. Serves:
//! - `GET /` — embedded single-page dashboard (Preact + HTM)
//! - `GET /api/state` — full agent state snapshot (JSON)
//! - `WS /ws` — bidirectional agent protocol (JSON-over-WebSocket)
//!
//! The WebSocket protocol is the **full agent interface** — any web UI can
//! connect and drive the agent as a black box. The embedded dashboard is
//! just the first consumer.
//!
//! # Protocol
//!
//! Server → Client (events):
//! ```json
//! {"type": "turn_start", "turn": 5}
//! {"type": "message_chunk", "text": "Here is..."}
//! {"type": "tool_start", "id": "tc1", "name": "bash", "args": {...}}
//! {"type": "tool_end", "id": "tc1", "result": "...", "is_error": false}
//! {"type": "state_snapshot", "data": {...}}  // sent on connect + periodically
//! ```
//!
//! Client → Server (commands):
//! ```json
//! {"type": "user_prompt", "text": "Fix the bug in auth.rs"}
//! {"type": "slash_command", "name": "focus", "args": "my-node"}
//! {"type": "cancel"}
//! {"type": "request_snapshot"}
//! ```

pub mod api;
pub mod ws;

use std::net::SocketAddr;

use axum::Router;
use tokio::sync::broadcast;

use crate::tui::dashboard::DashboardHandles;

/// Shared state accessible to all web handlers.
#[derive(Clone)]
pub struct WebState {
    /// Dashboard data handles (same Arc<Mutex<>> the TUI reads).
    pub handles: DashboardHandles,
    /// Broadcast channel for AgentEvents → WebSocket push.
    pub events_tx: broadcast::Sender<omegon_traits::AgentEvent>,
    /// Channel for WebSocket commands → main loop.
    pub command_tx: tokio::sync::mpsc::Sender<WebCommand>,
}

/// Commands received from WebSocket clients, forwarded to the main loop.
#[derive(Debug, Clone)]
pub enum WebCommand {
    /// User typed a prompt in the web UI.
    UserPrompt(String),
    /// Slash command from the web UI.
    SlashCommand { name: String, args: String },
    /// Cancel the current agent turn.
    Cancel,
}

/// Start the embedded web server. Returns the actual bound address.
///
/// The server runs as a background tokio task. Call this from `/dash open`.
/// Returns the address so we can open the browser.
pub async fn start_server(
    state: WebState,
    preferred_port: u16,
) -> anyhow::Result<SocketAddr> {
    let app = Router::new()
        .route("/api/state", axum::routing::get(api::get_state))
        .route("/ws", axum::routing::get(ws::ws_handler))
        .route("/", axum::routing::get(serve_dashboard))
        .layer(
            tower_http::cors::CorsLayer::permissive(),
        )
        .with_state(state);

    // Try preferred port, then auto-increment up to 10 times
    let addr = find_available_port(preferred_port).await?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let bound = listener.local_addr()?;

    tracing::info!(port = bound.port(), "web dashboard server starting");

    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!("web server error: {e}");
        }
    });

    Ok(bound)
}

/// Serve the embedded dashboard HTML.
async fn serve_dashboard() -> axum::response::Html<&'static str> {
    axum::response::Html(include_str!("assets/dashboard.html"))
}

/// Find an available port starting from `preferred`, incrementing on failure.
async fn find_available_port(preferred: u16) -> anyhow::Result<SocketAddr> {
    for offset in 0..10 {
        let port = preferred + offset;
        let addr: SocketAddr = ([127, 0, 0, 1], port).into();
        match tokio::net::TcpListener::bind(addr).await {
            Ok(listener) => {
                // Port is available — drop the listener so start_server can bind
                drop(listener);
                return Ok(addr);
            }
            Err(_) => continue,
        }
    }
    anyhow::bail!("No available port found in range {preferred}-{}", preferred + 9)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn find_port_succeeds() {
        let addr = find_available_port(18000).await.unwrap();
        assert!(addr.port() >= 18000);
    }
}
