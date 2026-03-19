//! WebSocket handler — bidirectional agent protocol.
//!
//! This is the **full agent interface**. Any web UI can connect to
//! ws://localhost:PORT/ws and drive the agent as a black box.
//!
//! # Server → Client (events)
//!
//! All events are JSON with a `type` field:
//!
//! - `state_snapshot` — full state on connect and on request
//! - `turn_start` — agent turn began
//! - `turn_end` — agent turn completed
//! - `message_chunk` — streaming assistant text
//! - `thinking_chunk` — streaming thinking text
//! - `message_end` — assistant message complete
//! - `tool_start` — tool call initiated
//! - `tool_end` — tool call completed with result
//! - `agent_end` — agent finished (no more turns)
//! - `phase_changed` — lifecycle phase transition
//! - `decomposition_started` — cleave children dispatched
//! - `decomposition_child_completed` — one child done
//! - `decomposition_completed` — cleave finished
//! - `system_notification` — system message (not from agent)
//!
//! # Client → Server (commands)
//!
//! - `user_prompt` — send a user message to the agent
//! - `slash_command` — execute a slash command
//! - `cancel` — cancel the current agent turn
//! - `request_snapshot` — ask for a fresh state_snapshot

use axum::extract::ws::{Message, WebSocket};
use axum::extract::{State, WebSocketUpgrade};
use axum::response::IntoResponse;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};

use super::{WebCommand, WebState};
use super::api::build_snapshot;
use omegon_traits::AgentEvent;

/// Upgrade handler — accepts the WebSocket connection.
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<WebState>,
) -> impl IntoResponse {
    ws.on_upgrade(|socket| handle_socket(socket, state))
}

/// Handle a single WebSocket connection.
async fn handle_socket(socket: WebSocket, state: WebState) {
    let (mut ws_tx, mut ws_rx) = socket.split();

    // Send initial state snapshot
    let snapshot = build_snapshot(&state);
    let init_msg = json!({
        "type": "state_snapshot",
        "data": snapshot,
    });
    if ws_tx.send(Message::Text(init_msg.to_string().into())).await.is_err() {
        return;
    }

    // Subscribe to agent events
    let mut events_rx = state.events_tx.subscribe();
    let command_tx = state.command_tx.clone();
    let state_for_snapshot = state.clone();

    // Spawn a task to forward agent events to the WebSocket
    let mut send_task = tokio::spawn(async move {
        while let Ok(event) = events_rx.recv().await {
            let msg = serialize_agent_event(&event);
            if ws_tx.send(Message::Text(msg.to_string().into())).await.is_err() {
                break;
            }
        }
    });

    // Process inbound messages from the WebSocket client
    let mut recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = ws_rx.next().await {
            match msg {
                Message::Text(text) => {
                    if let Ok(cmd) = serde_json::from_str::<Value>(&text) {
                        handle_client_command(&cmd, &command_tx, &state_for_snapshot).await;
                    }
                }
                Message::Close(_) => break,
                _ => {} // ignore binary, ping, pong
            }
        }
    });

    // Wait for either task to finish — then abort the other
    tokio::select! {
        _ = &mut send_task => { recv_task.abort(); }
        _ = &mut recv_task => { send_task.abort(); }
    }

    tracing::debug!("WebSocket client disconnected");
}

/// Process a command from a WebSocket client.
async fn handle_client_command(
    cmd: &Value,
    command_tx: &tokio::sync::mpsc::Sender<WebCommand>,
    state: &WebState,
) {
    let cmd_type = cmd["type"].as_str().unwrap_or("");

    match cmd_type {
        "user_prompt" => {
            if let Some(text) = cmd["text"].as_str() {
                let _ = command_tx.send(WebCommand::UserPrompt(text.to_string())).await;
            }
        }
        "slash_command" => {
            let name = cmd["name"].as_str().unwrap_or("").to_string();
            let args = cmd["args"].as_str().unwrap_or("").to_string();
            let _ = command_tx.send(WebCommand::SlashCommand { name, args }).await;
        }
        "cancel" => {
            let _ = command_tx.send(WebCommand::Cancel).await;
        }
        "request_snapshot" => {
            // The snapshot is sent via the send_task's broadcast channel.
            // For now, we emit a synthetic event. In the future, this could
            // push directly to the client that requested it.
            let _snapshot = build_snapshot(state);
            // We can't easily push to a specific client from here without
            // changing the architecture. For MVP, the client can just re-fetch /api/state.
            tracing::debug!("Snapshot requested via WebSocket — client should fetch /api/state");
        }
        other => {
            tracing::debug!("Unknown WebSocket command: {other}");
        }
    }
}

/// Serialize an AgentEvent to a JSON event message.
fn serialize_agent_event(event: &AgentEvent) -> Value {
    match event {
        AgentEvent::TurnStart { turn } => json!({
            "type": "turn_start",
            "turn": turn,
        }),
        AgentEvent::TurnEnd { turn } => json!({
            "type": "turn_end",
            "turn": turn,
        }),
        AgentEvent::MessageChunk { text } => json!({
            "type": "message_chunk",
            "text": text,
        }),
        AgentEvent::ThinkingChunk { text } => json!({
            "type": "thinking_chunk",
            "text": text,
        }),
        AgentEvent::MessageEnd => json!({
            "type": "message_end",
        }),
        AgentEvent::ToolStart { id, name, args } => json!({
            "type": "tool_start",
            "id": id,
            "name": name,
            "args": args,
        }),
        AgentEvent::ToolEnd { id, result, is_error } => {
            let result_text = result.content.first().and_then(|c| c.as_text()).unwrap_or("");
            json!({
                "type": "tool_end",
                "id": id,
                "result": result_text,
                "is_error": is_error,
            })
        }
        AgentEvent::AgentEnd => json!({
            "type": "agent_end",
        }),
        AgentEvent::PhaseChanged { phase } => json!({
            "type": "phase_changed",
            "phase": format!("{phase:?}"),
        }),
        AgentEvent::DecompositionStarted { children } => json!({
            "type": "decomposition_started",
            "children": children,
        }),
        AgentEvent::DecompositionChildCompleted { label, success } => json!({
            "type": "decomposition_child_completed",
            "label": label,
            "success": success,
        }),
        AgentEvent::DecompositionCompleted { merged } => json!({
            "type": "decomposition_completed",
            "merged": merged,
        }),
        AgentEvent::SystemNotification { message } => json!({
            "type": "system_notification",
            "message": message,
        }),
        AgentEvent::MessageStart { role } => json!({
            "type": "message_start",
            "role": role,
        }),
        AgentEvent::ToolUpdate { id, partial } => {
            let text = partial.content.first().and_then(|c| c.as_text()).unwrap_or("");
            json!({
                "type": "tool_update",
                "id": id,
                "partial": text,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_turn_start() {
        let event = AgentEvent::TurnStart { turn: 5 };
        let json = serialize_agent_event(&event);
        assert_eq!(json["type"], "turn_start");
        assert_eq!(json["turn"], 5);
    }

    #[test]
    fn serialize_message_chunk() {
        let event = AgentEvent::MessageChunk { text: "Hello world".into() };
        let json = serialize_agent_event(&event);
        assert_eq!(json["type"], "message_chunk");
        assert_eq!(json["text"], "Hello world");
    }

    #[test]
    fn serialize_tool_start() {
        let event = AgentEvent::ToolStart {
            id: "tc1".into(),
            name: "bash".into(),
            args: serde_json::json!({"command": "ls"}),
        };
        let json = serialize_agent_event(&event);
        assert_eq!(json["type"], "tool_start");
        assert_eq!(json["name"], "bash");
    }

    #[test]
    fn serialize_tool_end() {
        let event = AgentEvent::ToolEnd {
            id: "tc1".into(),
            result: omegon_traits::ToolResult {
                content: vec![omegon_traits::ContentBlock::Text { text: "file.rs".into() }],
                details: serde_json::json!(null),
            },
            is_error: false,
        };
        let json = serialize_agent_event(&event);
        assert_eq!(json["type"], "tool_end");
        assert_eq!(json["result"], "file.rs");
        assert_eq!(json["is_error"], false);
    }

    #[test]
    fn serialize_all_event_types() {
        // Ensure every AgentEvent variant serializes without panic
        let events = vec![
            AgentEvent::TurnStart { turn: 1 },
            AgentEvent::TurnEnd { turn: 1 },
            AgentEvent::MessageChunk { text: "hi".into() },
            AgentEvent::ThinkingChunk { text: "hmm".into() },
            AgentEvent::MessageEnd,
            AgentEvent::ToolStart { id: "1".into(), name: "read".into(), args: serde_json::json!({}) },
            AgentEvent::ToolEnd {
                id: "1".into(),
                result: omegon_traits::ToolResult {
                    content: vec![omegon_traits::ContentBlock::Text { text: "ok".into() }],
                    details: serde_json::json!(null),
                },
                is_error: false,
            },
            AgentEvent::AgentEnd,
            AgentEvent::PhaseChanged { phase: omegon_traits::LifecyclePhase::Idle },
            AgentEvent::DecompositionStarted { children: vec!["a".into()] },
            AgentEvent::DecompositionChildCompleted { label: "a".into(), success: true },
            AgentEvent::DecompositionCompleted { merged: true },
            AgentEvent::SystemNotification { message: "test".into() },
        ];
        for event in &events {
            let json = serialize_agent_event(event);
            assert!(json["type"].is_string(), "event should have a type field");
        }
    }
}
