//! LLM Bridge — subprocess interface to pi-ai providers.
//!
//! Spawns a long-lived Node.js process that imports @styrene-lab/pi-ai
//! and relays streamSimple() calls as ndjson over stdin/stdout.
//! The reverse-sidecar pattern: Rust hosts, Node is the utility.

use async_trait::async_trait;
use omegon_traits::ToolDefinition;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Events streamed from the LLM bridge subprocess.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum LlmEvent {
    #[serde(rename = "start")]
    Start { partial: Value },
    #[serde(rename = "text_delta")]
    TextDelta { text: String },
    #[serde(rename = "thinking_delta")]
    ThinkingDelta { text: String },
    #[serde(rename = "toolcall_start")]
    ToolCallStart { id: String, name: String },
    #[serde(rename = "toolcall_delta")]
    ToolCallDelta { id: String, arguments_delta: String },
    #[serde(rename = "toolcall_end")]
    ToolCallEnd { id: String },
    #[serde(rename = "done")]
    Done { message: Value },
    #[serde(rename = "error")]
    Error { message: String },
}

/// Messages sent to the LLM for context.
#[derive(Debug, Clone, Serialize)]
pub struct LlmMessage {
    pub role: String,
    pub content: Value,
}

/// The bridge trait — abstraction over how we call LLM providers.
/// Primary implementation: subprocess bridge to pi-ai.
/// Test implementation: mock bridge with scripted responses.
#[async_trait]
pub trait LlmBridge: Send + Sync {
    async fn stream(
        &self,
        system_prompt: &str,
        messages: &[LlmMessage],
        tools: &[ToolDefinition],
    ) -> anyhow::Result<tokio::sync::mpsc::Receiver<LlmEvent>>;
}

// TODO: SubprocessBridge implementation
// - Spawn Node.js process with bridge.mjs
// - Send stream requests as ndjson on stdin
// - Parse ndjson responses from stdout
// - Long-lived process, spawned once

// TODO: MockBridge for testing
// - Returns scripted LlmEvent sequences
// - Verifies what was sent to the bridge
