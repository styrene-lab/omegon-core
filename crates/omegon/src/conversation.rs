//! ConversationState — canonical history, context decay, and IntentDocument.
//!
//! Maintains two views: the canonical (unmodified) history for persistence,
//! and the LLM-facing view with decay applied for context efficiency.

use crate::bridge::LlmMessage;
use omegon_traits::LifecyclePhase;
use indexmap::IndexSet;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

/// A tool call extracted from an assistant message.
#[derive(Debug, Clone)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

/// A tool result entry in the conversation.
#[derive(Debug, Clone)]
pub struct ToolResultEntry {
    pub call_id: String,
    pub tool_name: String,
    pub content: Vec<omegon_traits::ContentBlock>,
    pub is_error: bool,
}

/// An assistant message with parsed content.
#[derive(Debug, Clone)]
pub struct AssistantMessage {
    pub text: String,
    pub thinking: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    pub raw: Value,
}

impl AssistantMessage {
    pub fn text_content(&self) -> &str {
        &self.text
    }

    pub fn tool_calls(&self) -> &[ToolCall] {
        &self.tool_calls
    }
}

/// A message in the canonical conversation history.
#[derive(Debug, Clone)]
pub enum AgentMessage {
    User { text: String },
    Assistant(AssistantMessage),
    ToolResult(ToolResultEntry),
}

/// Structured intent tracking — auto-populated, survives compaction verbatim.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IntentDocument {
    pub current_task: Option<String>,
    pub approach: Option<String>,
    pub lifecycle_phase: LifecyclePhase,

    pub files_read: IndexSet<PathBuf>,
    pub files_modified: IndexSet<PathBuf>,

    pub constraints_discovered: Vec<String>,
    pub failed_approaches: Vec<FailedApproach>,
    pub open_questions: Vec<String>,

    pub stats: SessionStatsAccumulator,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FailedApproach {
    pub description: String,
    pub reason: String,
    pub turn: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionStatsAccumulator {
    pub turns: u32,
    pub tool_calls: u32,
    pub tokens_consumed: u64,
    pub compactions: u32,
}

impl IntentDocument {
    /// Update from tool call activity — automatic population.
    pub fn update_from_tools(
        &mut self,
        calls: &[ToolCall],
        _results: &[ToolResultEntry],
    ) {
        self.stats.tool_calls += calls.len() as u32;

        for call in calls {
            match call.name.as_str() {
                "read" | "understand" => {
                    if let Some(path) = call.arguments.get("path").and_then(|v| v.as_str()) {
                        self.files_read.insert(PathBuf::from(path));
                    }
                }
                "change" | "write" | "edit" => {
                    if let Some(path) = call.arguments.get("path").and_then(|v| v.as_str()) {
                        self.files_modified.insert(PathBuf::from(path));
                    }
                }
                _ => {}
            }
        }
    }
}

/// The full conversation state.
pub struct ConversationState {
    /// Canonical, unmodified history. Source of truth for persistence.
    canonical: Vec<AgentMessage>,

    /// The IntentDocument — survives compaction verbatim.
    pub intent: IntentDocument,

    /// Decay window: messages older than this many turns get decayed.
    decay_window: usize,
}

impl ConversationState {
    pub fn new() -> Self {
        Self {
            canonical: Vec::new(),
            intent: IntentDocument::default(),
            decay_window: 10,
        }
    }

    pub fn push_user(&mut self, text: String) {
        self.canonical.push(AgentMessage::User { text });
    }

    pub fn push_assistant(&mut self, msg: AssistantMessage) {
        self.canonical.push(AgentMessage::Assistant(msg));
    }

    pub fn push_tool_result(&mut self, result: ToolResultEntry) {
        self.canonical.push(AgentMessage::ToolResult(result));
    }

    pub fn turn_count(&self) -> u32 {
        self.intent.stats.turns
    }

    pub fn last_user_prompt(&self) -> &str {
        self.canonical
            .iter()
            .rev()
            .find_map(|m| match m {
                AgentMessage::User { text } => Some(text.as_str()),
                _ => None,
            })
            .unwrap_or("")
    }

    /// Get the last assistant message's text content, if any.
    pub fn last_assistant_text(&self) -> Option<&str> {
        self.canonical.iter().rev().find_map(|m| match m {
            AgentMessage::Assistant(a) if !a.text.is_empty() => Some(a.text.as_str()),
            _ => None,
        })
    }

    /// Build the LLM-facing view with context decay applied.
    /// Recent messages at full fidelity, older messages decayed to skeletons.
    pub fn build_llm_view(&self) -> Vec<LlmMessage> {
        let len = self.canonical.len();
        self.canonical
            .iter()
            .enumerate()
            .map(|(i, msg)| {
                let age = len.saturating_sub(i);
                if age > self.decay_window {
                    self.decay_message(msg)
                } else {
                    self.to_llm_message(msg)
                }
            })
            .collect()
    }

    /// Apply ambient captures from omg: tags.
    pub fn apply_ambient_captures(
        &mut self,
        captures: &[crate::lifecycle::capture::AmbientCapture],
    ) {
        for capture in captures {
            match capture {
                crate::lifecycle::capture::AmbientCapture::Constraint(text) => {
                    self.intent.constraints_discovered.push(text.clone());
                }
                crate::lifecycle::capture::AmbientCapture::Question(text) => {
                    self.intent.open_questions.push(text.clone());
                }
                crate::lifecycle::capture::AmbientCapture::Approach(text) => {
                    self.intent.approach = Some(text.clone());
                }
                crate::lifecycle::capture::AmbientCapture::Failed { description, reason } => {
                    self.intent.failed_approaches.push(FailedApproach {
                        description: description.clone(),
                        reason: reason.clone(),
                        turn: self.intent.stats.turns,
                    });
                }
                _ => {
                    // Decision, Phase — handled by lifecycle engine
                }
            }
        }
    }

    fn decay_message(&self, msg: &AgentMessage) -> LlmMessage {
        match msg {
            AgentMessage::ToolResult(result) => {
                let summary = format!(
                    "[Tool result: {} — {}]",
                    result.tool_name,
                    if result.is_error { "error" } else { "ok" }
                );
                LlmMessage {
                    role: "user".into(),
                    content: serde_json::json!([{"type": "tool_result", "tool_use_id": result.call_id, "content": summary}]),
                    timestamp: None,
                }
            }
            _ => self.to_llm_message(msg),
        }
    }

    fn to_llm_message(&self, msg: &AgentMessage) -> LlmMessage {
        // TODO: proper conversion to provider message format
        match msg {
            AgentMessage::User { text } => LlmMessage {
                role: "user".into(),
                content: serde_json::json!(text),
                timestamp: None,
            },
            AgentMessage::Assistant(a) => LlmMessage {
                role: "assistant".into(),
                content: a.raw.clone(),
                timestamp: None,
            },
            AgentMessage::ToolResult(r) => LlmMessage {
                role: "user".into(),
                content: serde_json::json!([{
                    "type": "tool_result",
                    "tool_use_id": r.call_id,
                    "content": r.content,
                }]),
                timestamp: None,
            },
        }
    }
}
