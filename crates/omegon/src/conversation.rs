//! ConversationState — canonical history, context decay, and IntentDocument.
//!
//! Maintains two views: the canonical (unmodified) history for persistence,
//! and the LLM-facing view with decay applied for context efficiency.

use crate::bridge::{LlmMessage, WireToolCall};
use indexmap::IndexSet;
use omegon_traits::LifecyclePhase;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

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
    User { text: String, turn: u32 },
    Assistant(AssistantMessage, u32), // (msg, turn)
    ToolResult(ToolResultEntry, u32), // (result, turn)
}

impl AgentMessage {
    fn turn(&self) -> u32 {
        match self {
            AgentMessage::User { turn, .. } => *turn,
            AgentMessage::Assistant(_, turn) => *turn,
            AgentMessage::ToolResult(_, turn) => *turn,
        }
    }
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

/// Serializable session snapshot for save/resume.
#[derive(Debug, Serialize, Deserialize)]
struct SessionSnapshot {
    messages: Vec<LlmMessage>,
    intent: IntentDocument,
    decay_window: usize,
    #[serde(default)]
    compaction_summary: Option<String>,
}

/// The full conversation state.
pub struct ConversationState {
    /// Canonical, unmodified history. Source of truth for persistence.
    canonical: Vec<AgentMessage>,

    /// The IntentDocument — survives compaction verbatim.
    pub intent: IntentDocument,

    /// Decay window: messages older than this many turns get decayed.
    decay_window: usize,

    /// Compaction summary — if set, injected as the first message after compaction.
    /// Replaces evicted messages so the LLM has continuity.
    compaction_summary: Option<String>,
}

impl ConversationState {
    pub fn new() -> Self {
        Self {
            canonical: Vec::new(),
            intent: IntentDocument::default(),
            decay_window: 10,
            compaction_summary: None,
        }
    }

    /// Estimate token count of the LLM-facing view (chars / 4 heuristic).
    /// Good enough for budget decisions — not a precise tokenizer.
    pub fn estimate_tokens(&self) -> usize {
        let view = self.build_llm_view();
        let chars: usize = view.iter().map(|m| m.char_count()).sum();
        chars / 4
    }

    /// Check if compaction is needed given a context budget.
    /// Returns true if estimated tokens exceed the threshold fraction.
    pub fn needs_compaction(&self, context_window: usize, threshold: f32) -> bool {
        let tokens = self.estimate_tokens();
        tokens as f32 > context_window as f32 * threshold
    }

    /// Build the text for an LLM compaction request — the messages that would
    /// be evicted, formatted for summarization.
    pub fn build_compaction_payload(&self) -> Option<(String, usize)> {
        let current_turn = self.intent.stats.turns;
        // Find messages older than the decay window — these are the ones
        // that are already decayed and should be compacted into a summary.
        let evictable: Vec<&AgentMessage> = self.canonical.iter()
            .filter(|m| current_turn.saturating_sub(m.turn()) > self.decay_window as u32)
            .collect();

        if evictable.is_empty() {
            return None;
        }

        let mut payload = String::new();
        payload.push_str("Summarize this conversation excerpt. Preserve:\n");
        payload.push_str("- What was accomplished (files changed, decisions made)\n");
        payload.push_str("- What failed and why\n");
        payload.push_str("- Current task and approach\n");
        payload.push_str("- Key constraints discovered\n");
        payload.push_str("Be concise but preserve actionable context.\n\n---\n\n");

        for msg in &evictable {
            match msg {
                AgentMessage::User { text, turn } => {
                    payload.push_str(&format!("[Turn {turn}] User: {text}\n\n"));
                }
                AgentMessage::Assistant(a, turn) => {
                    let truncated = if a.text.len() > 200 {
                        format!("{}...", &a.text[..200])
                    } else {
                        a.text.clone()
                    };
                    payload.push_str(&format!("[Turn {turn}] Assistant: {truncated}\n"));
                    if !a.tool_calls.is_empty() {
                        let tools: Vec<_> = a.tool_calls.iter().map(|tc| tc.name.as_str()).collect();
                        payload.push_str(&format!("  Tools called: {}\n", tools.join(", ")));
                    }
                    payload.push('\n');
                }
                AgentMessage::ToolResult(r, turn) => {
                    let status = if r.is_error { "ERROR" } else { "ok" };
                    payload.push_str(&format!("[Turn {turn}] Tool {}: {status}\n\n", r.tool_name));
                }
            }
        }

        Some((payload, evictable.len()))
    }

    /// Apply a compaction summary — evict old messages and replace with summary.
    pub fn apply_compaction(&mut self, summary: String) {
        let current_turn = self.intent.stats.turns;
        // Remove all messages older than the decay window
        self.canonical.retain(|m| {
            current_turn.saturating_sub(m.turn()) <= self.decay_window as u32
        });
        self.compaction_summary = Some(summary);
        self.intent.stats.compactions += 1;
        tracing::info!(
            compactions = self.intent.stats.compactions,
            remaining_messages = self.canonical.len(),
            "Compaction applied"
        );
    }

    /// Render the IntentDocument as a context injection block.
    pub fn render_intent_for_injection(&self) -> String {
        let intent = &self.intent;
        let mut lines = Vec::new();
        lines.push("[Intent — session state]".to_string());

        if let Some(task) = &intent.current_task {
            lines.push(format!("Task: {task}"));
        }
        if let Some(approach) = &intent.approach {
            lines.push(format!("Approach: {approach}"));
        }
        if !intent.files_modified.is_empty() {
            let files: Vec<_> = intent.files_modified.iter()
                .map(|p| p.display().to_string()).collect();
            lines.push(format!("Files modified: {}", files.join(", ")));
        }
        if !intent.constraints_discovered.is_empty() {
            lines.push(format!("Constraints: {}", intent.constraints_discovered.join("; ")));
        }
        if !intent.failed_approaches.is_empty() {
            lines.push("Failed approaches:".to_string());
            for fa in &intent.failed_approaches {
                lines.push(format!("  - {}: {} (turn {})", fa.description, fa.reason, fa.turn));
            }
        }
        lines.push(format!(
            "Stats: {} turns, {} tool calls, {} compactions",
            intent.stats.turns, intent.stats.tool_calls, intent.stats.compactions
        ));

        lines.join("\n")
    }

    pub fn push_user(&mut self, text: String) {
        let turn = self.intent.stats.turns;
        self.canonical.push(AgentMessage::User { text, turn });
    }

    pub fn push_assistant(&mut self, msg: AssistantMessage) {
        let turn = self.intent.stats.turns;
        self.canonical.push(AgentMessage::Assistant(msg, turn));
    }

    pub fn push_tool_result(&mut self, result: ToolResultEntry) {
        let turn = self.intent.stats.turns;
        self.canonical.push(AgentMessage::ToolResult(result, turn));
    }

    pub fn turn_count(&self) -> u32 {
        self.intent.stats.turns
    }

    pub fn last_user_prompt(&self) -> &str {
        self.canonical
            .iter()
            .rev()
            .find_map(|m| match m {
                AgentMessage::User { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .unwrap_or("")
    }

    pub fn last_assistant_text(&self) -> Option<&str> {
        self.canonical.iter().rev().find_map(|m| match m {
            AgentMessage::Assistant(a, _) if !a.text.is_empty() => Some(a.text.as_str()),
            _ => None,
        })
    }

    /// Build the LLM-facing view with context decay applied.
    /// Messages older than `decay_window` turns are decayed to skeletons.
    /// If a compaction summary exists, it's injected as the first message.
    pub fn build_llm_view(&self) -> Vec<LlmMessage> {
        let current_turn = self.intent.stats.turns;
        let mut messages: Vec<LlmMessage> = Vec::new();

        // Inject compaction summary as first message if present
        if let Some(summary) = &self.compaction_summary {
            messages.push(LlmMessage::User {
                content: format!(
                    "[Previous conversation summary]\n{summary}\n\n{}\n[End summary — continue from here]",
                    self.render_intent_for_injection()
                ),
            });
        }

        for msg in &self.canonical {
            let turn_age = current_turn.saturating_sub(msg.turn());
            if turn_age > self.decay_window as u32 {
                messages.push(self.decay_message(msg));
            } else {
                messages.push(self.to_llm_message(msg));
            }
        }

        messages
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
            AgentMessage::ToolResult(result, _) => {
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
            AgentMessage::Assistant(a, _) => {
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
            AgentMessage::User { text, .. } => LlmMessage::User {
                content: text.clone(),
            },
        }
    }

    // ── Session persistence ──────────────────────────────────────────────

    /// Save conversation state to a JSON file for later resumption.
    /// Persists: the LLM-facing view (not canonical — raw may contain
    /// non-serializable handles), the intent document, and turn count.
    pub fn save_session(&self, path: &Path) -> anyhow::Result<()> {
        let view = self.build_llm_view();
        let session = SessionSnapshot {
            messages: view,
            intent: self.intent.clone(),
            decay_window: self.decay_window,
            compaction_summary: self.compaction_summary.clone(),
        };
        let json = serde_json::to_string_pretty(&session)?;
        std::fs::write(path, json)?;
        tracing::info!(path = %path.display(), turns = self.intent.stats.turns, "session saved");
        Ok(())
    }

    /// Load a previously saved session. The loaded messages become the
    /// canonical history (since they were already decay-processed at save time).
    pub fn load_session(path: &Path) -> anyhow::Result<Self> {
        let json = std::fs::read_to_string(path)?;
        let snapshot: SessionSnapshot = serde_json::from_str(&json)?;
        tracing::info!(
            path = %path.display(),
            turns = snapshot.intent.stats.turns,
            messages = snapshot.messages.len(),
            "session loaded"
        );

        // Reconstruct canonical from the saved LLM view.
        // Assign all messages to the last turn so they stay within the decay window
        // on the next build_llm_view() call. (Original per-message turns are lost
        // through the LLM view serialization, but that's acceptable — the saved
        // view was already decay-processed at save time.)
        let last_turn = snapshot.intent.stats.turns;
        let canonical: Vec<AgentMessage> = snapshot
            .messages
            .into_iter()
            .map(|msg| {
                let turn = last_turn;
                match msg {
                    LlmMessage::User { content } => AgentMessage::User { text: content, turn },
                    LlmMessage::Assistant { text, thinking, tool_calls, raw } => {
                        AgentMessage::Assistant(
                            AssistantMessage {
                                text: text.join("\n"),
                                thinking: if thinking.is_empty() { None } else { Some(thinking.join("\n")) },
                                tool_calls: tool_calls.into_iter().map(|tc| ToolCall {
                                    id: tc.id,
                                    name: tc.name,
                                    arguments: tc.arguments,
                                }).collect(),
                                raw: raw.unwrap_or(Value::Null),
                            },
                            turn,
                        )
                    }
                    LlmMessage::ToolResult { call_id, tool_name, content, is_error } => {
                        AgentMessage::ToolResult(
                            ToolResultEntry {
                                call_id,
                                tool_name,
                                content: vec![omegon_traits::ContentBlock::Text { text: content }],
                                is_error,
                            },
                            turn,
                        )
                    }
                }
            })
            .collect();

        Ok(Self {
            canonical,
            intent: snapshot.intent,
            decay_window: snapshot.decay_window,
            compaction_summary: snapshot.compaction_summary,
        })
    }

    /// Convert a canonical message to Omegon's wire format.
    fn to_llm_message(&self, msg: &AgentMessage) -> LlmMessage {
        match msg {
            AgentMessage::User { text, .. } => LlmMessage::User {
                content: text.clone(),
            },
            AgentMessage::Assistant(a, _) => LlmMessage::Assistant {
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
            AgentMessage::ToolResult(r, _) => {
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
        conv.decay_window = 0; // Decay everything older than current turn

        // Push message at turn 0, then advance to turn 1 so it's "old"
        conv.push_assistant(AssistantMessage {
            text: "short response".into(),
            thinking: Some("very long internal thinking...".repeat(100)),
            tool_calls: vec![],
            raw: serde_json::Value::Null,
        });
        conv.intent.stats.turns = 1; // Advance turn so the message is old

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
        conv.intent.stats.turns = 1;

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
        conv.intent.stats.turns = 1;

        let view = conv.build_llm_view();
        if let LlmMessage::ToolResult { content, tool_name, .. } = &view[0] {
            assert_eq!(tool_name, "read");
            assert!(content.contains("completed successfully"), "got: {content}");
        } else {
            panic!("Expected ToolResult message");
        }
    }

    #[test]
    fn decay_is_turn_based_not_message_based() {
        let mut conv = ConversationState::new();
        conv.decay_window = 2; // Keep last 2 turns fresh
        conv.intent.stats.turns = 1;

        // Turn 1: push multiple messages (simulates a turn with 3 tool calls)
        conv.push_user("do something".into());
        conv.push_assistant(AssistantMessage {
            text: "I'll help".into(),
            thinking: Some("detailed thinking here...".repeat(50)),
            tool_calls: vec![],
            raw: serde_json::Value::Null,
        });
        conv.push_tool_result(ToolResultEntry {
            call_id: "t1".into(),
            tool_name: "read".into(),
            content: vec![omegon_traits::ContentBlock::Text { text: "big content".repeat(100) }],
            is_error: false,
        });

        // Still on turn 1 — everything should be fresh
        let view = conv.build_llm_view();
        if let LlmMessage::Assistant { thinking, .. } = &view[1] {
            assert!(!thinking.is_empty(), "Turn 1 at turn 1: should NOT be decayed");
        }

        // Advance to turn 4 — turn 1 is now 3 turns old, outside decay_window=2
        conv.intent.stats.turns = 4;
        let view = conv.build_llm_view();
        if let LlmMessage::Assistant { thinking, .. } = &view[1] {
            assert!(thinking.is_empty(), "Turn 1 at turn 4: should be decayed (age 3 > window 2)");
        }
    }

    #[test]
    fn session_save_load_round_trip() {
        let mut conv = ConversationState::new();
        conv.push_user("Fix the bug".into());
        conv.push_assistant(AssistantMessage {
            text: "I'll fix it".into(),
            thinking: None,
            tool_calls: vec![ToolCall {
                id: "tc1".into(),
                name: "edit".into(),
                arguments: serde_json::json!({"path": "src/foo.rs"}),
            }],
            raw: serde_json::Value::Null,
        });
        conv.push_tool_result(ToolResultEntry {
            call_id: "tc1".into(),
            tool_name: "edit".into(),
            content: vec![omegon_traits::ContentBlock::Text { text: "Edited successfully".into() }],
            is_error: false,
        });
        conv.intent.stats.turns = 1;
        conv.intent.current_task = Some("Fix the auth bug".into());
        conv.intent.files_modified.insert(PathBuf::from("src/foo.rs"));

        // Save
        let tmp = std::env::temp_dir().join("omegon-test-session.json");
        conv.save_session(&tmp).unwrap();

        // Load
        let loaded = ConversationState::load_session(&tmp).unwrap();
        assert_eq!(loaded.intent.stats.turns, 1);
        assert_eq!(loaded.intent.current_task.as_deref(), Some("Fix the auth bug"));
        assert!(loaded.intent.files_modified.contains(&PathBuf::from("src/foo.rs")));

        let view = loaded.build_llm_view();
        assert_eq!(view.len(), 3); // user + assistant + tool_result

        // Cleanup
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn estimate_tokens_chars_div_4() {
        let mut conv = ConversationState::new();
        conv.push_user("hello world".into()); // 11 chars
        let tokens = conv.estimate_tokens();
        // "hello world" = 11 chars → 11/4 = 2 tokens (integer division)
        assert!(tokens >= 2 && tokens <= 4, "got {tokens}");
    }

    #[test]
    fn needs_compaction_threshold() {
        let mut conv = ConversationState::new();
        // Push a message under threshold: 400k chars → ~100k tokens, threshold at 150k
        conv.push_user("x".repeat(400_000));
        assert!(!conv.needs_compaction(200_000, 0.75), "100k tokens should be under 150k threshold");
        // Push more to exceed: 800k chars → ~200k tokens, threshold at 150k
        conv.push_user("y".repeat(400_000));
        assert!(conv.needs_compaction(200_000, 0.75), "200k tokens should exceed 150k threshold");
    }

    #[test]
    fn build_compaction_payload_only_evictable() {
        let mut conv = ConversationState::new();
        conv.decay_window = 2;

        // Turn 0 messages (will be evictable at turn 5)
        conv.push_user("old task".into());
        conv.push_assistant(AssistantMessage {
            text: "working on it".into(),
            thinking: None,
            tool_calls: vec![],
            raw: Value::Null,
        });

        // Advance to turn 5 so turn-0 messages are outside decay window
        conv.intent.stats.turns = 5;
        conv.push_user("new task".into());

        let (payload, count) = conv.build_compaction_payload().unwrap();
        assert_eq!(count, 2, "Should evict 2 old messages");
        assert!(payload.contains("old task"));
        assert!(!payload.contains("new task"), "Recent messages should not be in payload");
    }

    #[test]
    fn apply_compaction_evicts_and_sets_summary() {
        let mut conv = ConversationState::new();
        conv.decay_window = 2;

        // Old messages
        conv.push_user("old".into());
        conv.push_assistant(AssistantMessage {
            text: "old reply".into(),
            thinking: None,
            tool_calls: vec![],
            raw: Value::Null,
        });

        // Advance and add recent
        conv.intent.stats.turns = 5;
        conv.push_user("recent".into());

        conv.apply_compaction("Summary of old conversation.".into());

        assert_eq!(conv.intent.stats.compactions, 1);
        assert!(conv.compaction_summary.is_some());
        // Old messages should be evicted
        assert_eq!(conv.canonical.len(), 1, "Only the recent message should remain");

        // The LLM view should have the summary + the recent message
        let view = conv.build_llm_view();
        assert_eq!(view.len(), 2); // summary pseudo-message + recent
        if let LlmMessage::User { content } = &view[0] {
            assert!(content.contains("Summary of old conversation"));
            assert!(content.contains("[Intent"));
        }
    }

    #[test]
    fn render_intent_for_injection() {
        let mut conv = ConversationState::new();
        conv.intent.current_task = Some("Fix auth flow".into());
        conv.intent.approach = Some("Token rotation".into());
        conv.intent.files_modified.insert(PathBuf::from("src/auth.rs"));
        conv.intent.constraints_discovered.push("30-minute TTL".into());
        conv.intent.failed_approaches.push(FailedApproach {
            description: "Direct replacement".into(),
            reason: "Cache holds stale refs".into(),
            turn: 5,
        });
        conv.intent.stats.turns = 10;
        conv.intent.stats.tool_calls = 25;

        let block = conv.render_intent_for_injection();
        assert!(block.contains("Fix auth flow"));
        assert!(block.contains("Token rotation"));
        assert!(block.contains("src/auth.rs"));
        assert!(block.contains("30-minute TTL"));
        assert!(block.contains("Direct replacement"));
        assert!(block.contains("Cache holds stale refs"));
    }

    #[test]
    fn intent_tracks_files_from_tool_calls() {
        let mut intent = IntentDocument::default();
        let calls = vec![
            ToolCall { id: "1".into(), name: "read".into(), arguments: serde_json::json!({"path": "src/foo.rs"}) },
            ToolCall { id: "2".into(), name: "edit".into(), arguments: serde_json::json!({"path": "src/bar.rs"}) },
            ToolCall { id: "3".into(), name: "write".into(), arguments: serde_json::json!({"path": "src/new.rs"}) },
            ToolCall { id: "4".into(), name: "bash".into(), arguments: serde_json::json!({"command": "ls"}) },
        ];
        intent.update_from_tools(&calls, &[]);
        assert!(intent.files_read.contains(&PathBuf::from("src/foo.rs")));
        assert!(intent.files_modified.contains(&PathBuf::from("src/bar.rs")));
        assert!(intent.files_modified.contains(&PathBuf::from("src/new.rs")));
        assert_eq!(intent.files_read.len(), 1);
        assert_eq!(intent.files_modified.len(), 2);
        assert_eq!(intent.stats.tool_calls, 4);
    }

    #[test]
    fn loaded_session_messages_stay_within_decay_window() {
        let mut conv = ConversationState::new();
        conv.intent.stats.turns = 10;
        conv.push_user("old task".into());
        conv.push_assistant(AssistantMessage {
            text: "long response with thinking".into(),
            thinking: Some("deep reasoning here".into()),
            tool_calls: vec![],
            raw: serde_json::Value::Null,
        });

        let tmp = std::env::temp_dir().join("omegon-test-decay-session.json");
        conv.save_session(&tmp).unwrap();

        let loaded = ConversationState::load_session(&tmp).unwrap();
        // After load, all messages are at last_turn=10.
        // With decay_window=10, messages at turn 10 with current_turn=10 → age 0 → NOT decayed.
        let view = loaded.build_llm_view();
        if let LlmMessage::Assistant { thinking, .. } = &view[1] {
            // Thinking should be PRESERVED (not decayed) because the message
            // is within the decay window after load
            assert!(!thinking.is_empty(), "thinking should be preserved after load, got empty");
        } else {
            panic!("expected assistant message at index 1");
        }

        let _ = std::fs::remove_file(&tmp);
    }
}
