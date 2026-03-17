//! ContextManager — dynamic per-turn system prompt injection.
//!
//! Starts with a minimal base prompt (~500 tokens) and injects
//! context based on deterministic signals: recent tools, file types,
//! lifecycle phase, memory facts, explicit declarations.
//!
//! Includes built-in providers:
//! - SessionHud: ambient awareness of session state (turn, budget, files, duration)

use omegon_traits::{ContextInjection, ContextProvider, ContextSignals, LifecyclePhase};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Instant;

use crate::conversation::ConversationState;

/// Manages dynamic system prompt assembly.
pub struct ContextManager {
    base_prompt: String,
    providers: Vec<Box<dyn ContextProvider>>,
    active_injections: Vec<ActiveInjection>,
    recent_tools: VecDeque<String>,
    recent_files: VecDeque<PathBuf>,
    phase: LifecyclePhase,
    session_start: Instant,
}

struct ActiveInjection {
    injection: ContextInjection,
    remaining_turns: u32,
}

impl ContextManager {
    pub fn new(base_prompt: String, providers: Vec<Box<dyn ContextProvider>>) -> Self {
        Self {
            base_prompt,
            providers,
            active_injections: Vec::new(),
            recent_tools: VecDeque::with_capacity(10),
            recent_files: VecDeque::with_capacity(20),
            phase: LifecyclePhase::default(),
            session_start: Instant::now(),
        }
    }

    /// Build the system prompt for this turn.
    /// Called once per LLM request, runs in <1ms.
    pub fn build_system_prompt(
        &mut self,
        user_prompt: &str,
        conversation: &ConversationState,
    ) -> String {
        self.decay_expired();

        let recent_tools_vec: Vec<String> =
            self.recent_tools.iter().cloned().collect();
        let recent_files_vec: Vec<PathBuf> =
            self.recent_files.iter().cloned().collect();

        let signals = ContextSignals {
            user_prompt,
            recent_tools: &recent_tools_vec,
            recent_files: &recent_files_vec,
            lifecycle_phase: &self.phase,
            turn_number: conversation.turn_count(),
            context_budget_tokens: 4000, // TODO: compute from remaining budget
        };

        // Collect injections from all providers
        for provider in &self.providers {
            if let Some(injection) = provider.provide_context(&signals) {
                self.active_injections.push(ActiveInjection {
                    remaining_turns: injection.ttl_turns,
                    injection,
                });
            }
        }

        // Inject session HUD (high priority, always present, refreshed each turn)
        let hud = self.build_session_hud(conversation);
        // Remove previous HUD injection (it's re-built each turn)
        self.active_injections
            .retain(|a| a.injection.source != "session-hud");
        self.active_injections.push(ActiveInjection {
            remaining_turns: 1,
            injection: ContextInjection {
                source: "session-hud".into(),
                content: hud,
                priority: 200, // High — but after base prompt
                ttl_turns: 1,
            },
        });

        self.assemble()
    }

    /// Build the session HUD line.
    fn build_session_hud(&self, conversation: &ConversationState) -> String {
        let intent = &conversation.intent;
        let elapsed = self.session_start.elapsed();
        let elapsed_str = if elapsed.as_secs() >= 3600 {
            format!("{}h{}m", elapsed.as_secs() / 3600, (elapsed.as_secs() % 3600) / 60)
        } else if elapsed.as_secs() >= 60 {
            format!("{}m{}s", elapsed.as_secs() / 60, elapsed.as_secs() % 60)
        } else {
            format!("{}s", elapsed.as_secs())
        };

        let files_read = intent.files_read.len();
        let files_modified = intent.files_modified.len();

        format!(
            "[Session: turn {} | {} tool calls | {} files read, {} modified | {}]",
            intent.stats.turns,
            intent.stats.tool_calls,
            files_read,
            files_modified,
            elapsed_str,
        )
    }

    /// Record a tool call for signal tracking.
    pub fn record_tool_call(&mut self, tool_name: &str) {
        self.recent_tools.push_back(tool_name.to_string());
        if self.recent_tools.len() > 10 {
            self.recent_tools.pop_front();
        }
    }

    /// Record a file access for signal tracking.
    pub fn record_file_access(&mut self, path: PathBuf) {
        // Deduplicate consecutive accesses to the same file
        if self.recent_files.back() != Some(&path) {
            self.recent_files.push_back(path);
            if self.recent_files.len() > 20 {
                self.recent_files.pop_front();
            }
        }
    }

    /// Update lifecycle phase based on tool activity.
    pub fn update_phase_from_activity(&mut self, tool_calls: &[crate::conversation::ToolCall]) {
        for call in tool_calls {
            match call.name.as_str() {
                "change" | "write" | "edit" => {
                    if !matches!(self.phase, LifecyclePhase::Implementing { .. }) {
                        self.phase = LifecyclePhase::Implementing { change_id: None };
                    }
                }
                "understand" | "read" => {
                    if matches!(self.phase, LifecyclePhase::Idle) {
                        self.phase = LifecyclePhase::Exploring { node_id: None };
                    }
                }
                _ => {}
            }
        }
    }

    fn decay_expired(&mut self) {
        self.active_injections.retain_mut(|a| {
            a.remaining_turns = a.remaining_turns.saturating_sub(1);
            a.remaining_turns > 0
        });
    }

    fn assemble(&self) -> String {
        let mut prompt = self.base_prompt.clone();

        // Sort injections by priority (highest first)
        let mut sorted: Vec<_> = self.active_injections.iter().collect();
        sorted.sort_by(|a, b| b.injection.priority.cmp(&a.injection.priority));

        for active in sorted {
            prompt.push_str("\n\n");
            prompt.push_str(&active.injection.content);
        }

        prompt
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_hud_format() {
        let cm = ContextManager::new("base".into(), vec![]);
        let conv = ConversationState::new();
        let hud = cm.build_session_hud(&conv);
        assert!(hud.starts_with("[Session:"));
        assert!(hud.contains("turn 0"));
        assert!(hud.contains("0 tool calls"));
        assert!(hud.ends_with(']'));
    }

    #[test]
    fn context_manager_includes_hud() {
        let mut cm = ContextManager::new("You are an assistant.".into(), vec![]);
        let conv = ConversationState::new();
        let prompt = cm.build_system_prompt("hello", &conv);
        assert!(prompt.contains("You are an assistant."));
        assert!(prompt.contains("[Session:"));
    }

    #[test]
    fn recent_files_dedup_consecutive() {
        let mut cm = ContextManager::new("base".into(), vec![]);
        cm.record_file_access(PathBuf::from("foo.rs"));
        cm.record_file_access(PathBuf::from("foo.rs"));
        cm.record_file_access(PathBuf::from("bar.rs"));
        cm.record_file_access(PathBuf::from("foo.rs"));
        assert_eq!(cm.recent_files.len(), 3); // foo, bar, foo (not 4)
    }
}
