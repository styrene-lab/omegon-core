//! ConversationState — canonical history, context decay, and IntentDocument.
//!
//! Maintains two views: the canonical (unmodified) history for persistence,
//! and the LLM-facing view with decay applied for context efficiency.

use crate::bridge::{LlmMessage, WireToolCall};
use indexmap::IndexSet;
use omegon_traits::LifecyclePhase;
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
    /// The complete provider response — opaque, preserved for multi-turn continuity
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
    pub fn update_from_tools(&mut self, calls: &[ToolCall], _results: &[ToolResultEntry]) {
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

    pub fn last_assistant_text(&self) -> Option<&str> {
        self.canonical.iter().rev().find_map(|m| match m {
            AgentMessage::Assistant(a) if !a.text.is_empty() => Some(a.text.as_str()),
            _ => None,
        })
    }

    /// Build the LLM-facing view with context decay applied.
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
                crate::lifecycle::capture::AmbientCapture::Failed {
                    description,
                    reason,
                } => {
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

    /// Decay a message to a skeleton — strip bulk content, keep metadata.
    fn decay_message(&self, msg: &AgentMessage) -> LlmMessage {
        match msg {
            AgentMessage::ToolResult(result) => {
                let summary = if result.is_error {
                    format!("[Tool {} errored]", result.tool_name)
                } else {
                    format!("[Tool {} completed successfully]", result.tool_name)
                };
                LlmMessage::ToolResult {
                    call_id: result.call_id.clone(),
                    tool_name: result.tool_name.clone(),
                    content: summary,
                    is_error: result.is_error,
                }
            }
            AgentMessage::Assistant(a) => {
                // Decay: strip thinking entirely, truncate long text, preserve tool calls
                let decayed_text = if a.text.len() > 500 {
                    let mut end = 500;
                    while end > 0 && !a.text.is_char_boundary(end) {
                        end -= 1;
                    }
                    format!("{}...[truncated]", &a.text[..end])
                } else {
                    a.text.clone()
                };
                LlmMessage::Assistant {
                    text: if decayed_text.is_empty() {
                        vec![]
                    } else {
                        vec![decayed_text]
                    },
                    thinking: vec![], // Strip thinking blocks entirely on decay
                    tool_calls: a
                        .tool_calls
                        .iter()
                        .map(|tc| WireToolCall {
                            id: tc.id.clone(),
                            name: tc.name.clone(),
                            arguments: tc.arguments.clone(),
                        })
                        .collect(),
                    raw: None, // Don't preserve raw for decayed messages
                }
            }
            // User messages are small — don't decay
            AgentMessage::User { text } => LlmMessage::User {
                content: text.clone(),
            },
        }
    }

    /// Convert a canonical message to Omegon's wire format.
    fn to_llm_message(&self, msg: &AgentMessage) -> LlmMessage {
        match msg {
            AgentMessage::User { text } => LlmMessage::User {
                content: text.clone(),
            },
            AgentMessage::Assistant(a) => LlmMessage::Assistant {
                text: if a.text.is_empty() {
                    vec![]
                } else {
                    vec![a.text.clone()]
                },
                thinking: a
                    .thinking
                    .as_ref()
                    .map(|t| vec![t.clone()])
                    .unwrap_or_default(),
                tool_calls: a
                    .tool_calls
                    .iter()
                    .map(|tc| WireToolCall {
                        id: tc.id.clone(),
                        name: tc.name.clone(),
                        arguments: tc.arguments.clone(),
                    })
                    .collect(),
                raw: Some(a.raw.clone()),
            },
            AgentMessage::ToolResult(r) => {
                // Flatten content blocks to text
                let text = r
                    .content
                    .iter()
                    .filter_map(|c| match c {
                        omegon_traits::ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");

                LlmMessage::ToolResult {
                    call_id: r.call_id.clone(),
                    tool_name: r.tool_name.clone(),
                    content: text,
                    is_error: r.is_error,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assistant_decay_strips_thinking() {
        let mut conv = ConversationState::new();
        conv.decay_window = 0; // Force all messages to decay

        conv.push_assistant(AssistantMessage {
            text: "short response".into(),
            thinking: Some("very long internal thinking...".repeat(100)),
            tool_calls: vec![],
            raw: serde_json::Value::Null,
        });

        let view = conv.build_llm_view();
        assert_eq!(view.len(), 1);
        if let LlmMessage::Assistant { thinking, .. } = &view[0] {
            assert!(thinking.is_empty(), "Thinking should be stripped on decay");
        } else {
            panic!("Expected Assistant message");
        }
    }

    #[test]
    fn assistant_decay_truncates_long_text() {
        let mut conv = ConversationState::new();
        conv.decay_window = 0;

        conv.push_assistant(AssistantMessage {
            text: "x".repeat(1000),
            thinking: None,
            tool_calls: vec![],
            raw: serde_json::Value::Null,
        });

        let view = conv.build_llm_view();
        if let LlmMessage::Assistant { text, .. } = &view[0] {
            let combined: String = text.join("");
            assert!(combined.len() < 600, "Text should be truncated, got {} bytes", combined.len());
            assert!(combined.contains("[truncated]"));
        } else {
            panic!("Expected Assistant message");
        }
    }

    #[test]
    fn tool_result_decay_preserves_metadata() {
        let mut conv = ConversationState::new();
        conv.decay_window = 0;

        conv.push_tool_result(ToolResultEntry {
            call_id: "t1".into(),
            tool_name: "read".into(),
            content: vec![omegon_traits::ContentBlock::Text {
                text: "x".repeat(5000),
            }],
            is_error: false,
        });

        let view = conv.build_llm_view();
        if let LlmMessage::ToolResult { content, tool_name, .. } = &view[0] {
            assert_eq!(tool_name, "read");
            assert!(content.contains("completed successfully"), "got: {content}");
        } else {
            panic!("Expected ToolResult message");
        }
    }
}
