//! LLM Bridge — subprocess interface to pi-ai providers.
//!
//! Spawns a long-lived Node.js process that imports @styrene-lab/pi-ai
//! and relays streamSimple() calls as ndjson over stdin/stdout.
//! The reverse-sidecar pattern: Rust hosts, Node is the utility.

use async_trait::async_trait;
use omegon_traits::ToolDefinition;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, Mutex};

// ─── Wire types ─────────────────────────────────────────────────────────────

/// Events streamed from the LLM bridge subprocess.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum LlmEvent {
    #[serde(rename = "start")]
    Start { partial: Value },
    #[serde(rename = "text_delta")]
    TextDelta {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        delta: String,
    },
    #[serde(rename = "thinking_delta")]
    ThinkingDelta {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        delta: String,
    },
    #[serde(rename = "text_start")]
    TextStart {
        #[serde(rename = "contentIndex")]
        content_index: usize,
    },
    #[serde(rename = "text_end")]
    TextEnd {
        #[serde(rename = "contentIndex")]
        content_index: usize,
    },
    #[serde(rename = "thinking_start")]
    ThinkingStart {
        #[serde(rename = "contentIndex")]
        content_index: usize,
    },
    #[serde(rename = "thinking_end")]
    ThinkingEnd {
        #[serde(rename = "contentIndex")]
        content_index: usize,
    },
    #[serde(rename = "toolcall_start")]
    ToolCallStart {
        #[serde(rename = "contentIndex")]
        content_index: usize,
    },
    #[serde(rename = "toolcall_delta")]
    ToolCallDelta {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        delta: String,
    },
    #[serde(rename = "toolcall_end")]
    ToolCallEnd {
        #[serde(rename = "contentIndex")]
        content_index: usize,
        #[serde(rename = "toolCall")]
        tool_call: Value,
    },
    #[serde(rename = "done")]
    Done { reason: String, message: Value },
    #[serde(rename = "error")]
    Error { reason: String, error: Value },
}

/// A bridge response line from the subprocess.
#[derive(Debug, Deserialize)]
struct BridgeResponse {
    id: u64,
    #[serde(default)]
    event: Option<LlmEvent>,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<String>,
}

/// A request sent to the bridge subprocess.
#[derive(Serialize)]
struct BridgeRequest {
    id: u64,
    method: String,
    params: Value,
}

/// Messages sent to the LLM for context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmMessage {
    pub role: String,
    pub content: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<u64>,
}

// ─── Bridge trait ───────────────────────────────────────────────────────────

/// The bridge trait — abstraction over how we call LLM providers.
/// Primary implementation: SubprocessBridge (pi-ai via Node.js).
/// Test implementation: MockBridge (scripted responses).
#[async_trait]
pub trait LlmBridge: Send + Sync {
    async fn stream(
        &self,
        system_prompt: &str,
        messages: &[LlmMessage],
        tools: &[ToolDefinition],
    ) -> anyhow::Result<mpsc::Receiver<LlmEvent>>;
}

// ─── Subprocess bridge ─────────────────────────────────────────────────────

/// Bridge implementation that spawns a Node.js subprocess.
pub struct SubprocessBridge {
    stdin: Arc<Mutex<tokio::process::ChildStdin>>,
    next_id: AtomicU64,
    /// Channel for dispatching responses from the reader task.
    response_tx: mpsc::Sender<BridgeResponse>,
    response_rx: Arc<Mutex<mpsc::Receiver<BridgeResponse>>>,
    _child: Child,
}

impl SubprocessBridge {
    /// Spawn the Node.js bridge subprocess.
    ///
    /// `bridge_script` is the path to llm-bridge.mjs.
    /// `node_path` resolves the Node.js binary.
    pub async fn spawn(bridge_script: &Path, node_path: &str) -> anyhow::Result<Self> {
        let mut child = Command::new(node_path)
            .arg(bridge_script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;

        let stdin = child.stdin.take().expect("stdin piped");
        let stdout = child.stdout.take().expect("stdout piped");
        let stderr = child.stderr.take().expect("stderr piped");

        // Log stderr (tracing/diagnostics from the bridge)
        tokio::spawn(async move {
            let reader = BufReader::new(stderr);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(target: "llm_bridge", "{}", line);
            }
        });

        let (response_tx, response_rx) = mpsc::channel(256);
        let tx = response_tx.clone();

        // Reader task: parse ndjson lines from stdout
        tokio::spawn(async move {
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                match serde_json::from_str::<BridgeResponse>(&line) {
                    Ok(resp) => {
                        if tx.send(resp).await.is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        tracing::warn!(target: "llm_bridge", "Failed to parse response: {e}");
                    }
                }
            }
        });

        Ok(Self {
            stdin: Arc::new(Mutex::new(stdin)),
            next_id: AtomicU64::new(1),
            response_tx,
            response_rx: Arc::new(Mutex::new(response_rx)),
            _child: child,
        })
    }

    /// Send a request to the bridge subprocess.
    async fn send_request(&self, method: &str, params: Value) -> anyhow::Result<u64> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let req = BridgeRequest {
            id,
            method: method.to_string(),
            params,
        };
        let mut line = serde_json::to_string(&req)?;
        line.push('\n');

        let mut stdin = self.stdin.lock().await;
        stdin.write_all(line.as_bytes()).await?;
        stdin.flush().await?;
        Ok(id)
    }

    /// Find the bridge script relative to the binary location.
    pub fn default_bridge_path() -> PathBuf {
        // In the npm package: ../bridge/llm-bridge.mjs relative to the binary
        // In dev: core/bridge/llm-bridge.mjs relative to cwd
        let exe = std::env::current_exe().unwrap_or_default();
        let exe_dir = exe.parent().unwrap_or(Path::new("."));

        // Try relative to executable first (installed mode)
        let installed = exe_dir.join("../bridge/llm-bridge.mjs");
        if installed.exists() {
            return installed;
        }

        // Try relative to cwd (dev mode)
        let dev = PathBuf::from("core/bridge/llm-bridge.mjs");
        if dev.exists() {
            return dev;
        }

        // Fallback
        installed
    }
}

#[async_trait]
impl LlmBridge for SubprocessBridge {
    async fn stream(
        &self,
        system_prompt: &str,
        messages: &[LlmMessage],
        tools: &[ToolDefinition],
    ) -> anyhow::Result<mpsc::Receiver<LlmEvent>> {
        // Build the pi-ai compatible context
        let tool_schemas: Vec<Value> = tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters,
                })
            })
            .collect();

        let context = serde_json::json!({
            "systemPrompt": system_prompt,
            "messages": messages,
            "tools": tool_schemas,
        });

        // TODO: resolve model from configuration
        let model = serde_json::json!({
            "id": "claude-sonnet-4-20250514",
            "name": "Claude Sonnet 4",
            "api": "anthropic",
            "provider": "anthropic",
            "baseUrl": "https://api.anthropic.com",
            "reasoning": true,
            "input": ["text", "image"],
            "cost": { "input": 3.0, "output": 15.0, "cacheRead": 0.3, "cacheWrite": 3.75 },
            "contextWindow": 200000,
            "maxTokens": 16384,
        });

        let params = serde_json::json!({
            "model": model,
            "context": context,
            "options": {
                "reasoning": "medium",
            },
        });

        let req_id = self.send_request("stream", params).await?;

        // Create a channel for this stream's events
        let (event_tx, event_rx) = mpsc::channel(64);
        let response_rx = self.response_rx.clone();

        // Spawn a task that routes responses for this request ID
        tokio::spawn(async move {
            let mut rx = response_rx.lock().await;
            while let Some(resp) = rx.recv().await {
                if resp.id != req_id {
                    // TODO: handle multiplexed requests properly
                    continue;
                }

                if let Some(event) = resp.event {
                    let is_terminal = matches!(
                        event,
                        LlmEvent::Done { .. } | LlmEvent::Error { .. }
                    );
                    let _ = event_tx.send(event).await;
                    if is_terminal {
                        break;
                    }
                }

                if resp.result.is_some() || resp.error.is_some() {
                    break;
                }
            }
        });

        Ok(event_rx)
    }
}

// ─── Mock bridge for testing ────────────────────────────────────────────────

/// Mock bridge that returns scripted LlmEvent sequences.
#[cfg(test)]
pub struct MockBridge {
    pub events: Vec<LlmEvent>,
}

#[cfg(test)]
#[async_trait]
impl LlmBridge for MockBridge {
    async fn stream(
        &self,
        _system_prompt: &str,
        _messages: &[LlmMessage],
        _tools: &[ToolDefinition],
    ) -> anyhow::Result<mpsc::Receiver<LlmEvent>> {
        let (tx, rx) = mpsc::channel(64);
        let events = self.events.clone();
        tokio::spawn(async move {
            for event in events {
                let _ = tx.send(event).await;
            }
        });
        Ok(rx)
    }
}
