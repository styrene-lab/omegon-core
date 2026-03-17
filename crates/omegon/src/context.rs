//! ContextManager — dynamic per-turn system prompt injection.
//!
//! Starts with a minimal base prompt (~500 tokens) and injects
//! context based on deterministic signals: recent tools, file types,
//! lifecycle phase, memory facts, explicit declarations.

use omegon_traits::{ContextInjection, ContextProvider, ContextSignals, LifecyclePhase};
use std::collections::VecDeque;
use std::path::PathBuf;

use crate::conversation::ConversationState;

/// Manages dynamic system prompt assembly.
pub struct ContextManager {
    base_prompt: String,
    providers: Vec<Box<dyn ContextProvider>>,
    active_injections: Vec<ActiveInjection>,
    recent_tools: VecDeque<String>,
    recent_files: VecDeque<PathBuf>,
    phase: LifecyclePhase,
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

        let signals = ContextSignals {
            user_prompt,
            recent_tools: &self.recent_tools.iter().cloned().collect::<Vec<_>>(),
            recent_files: &self.recent_files.iter().cloned().collect::<Vec<_>>(),
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

        self.assemble()
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
        self.recent_files.push_back(path);
        if self.recent_files.len() > 20 {
            self.recent_files.pop_front();
        }
    }

    /// Update lifecycle phase based on tool activity.
    pub fn update_phase_from_activity(&mut self, tool_calls: &[crate::conversation::ToolCall]) {
        // Self-correcting phase detection:
        // change/write/edit calls → Implementing
        // understand with broad queries → Exploring
        // decompose → Decomposing
        for call in tool_calls {
            match call.name.as_str() {
                "change" | "write" | "edit" => {
                    if !matches!(self.phase, LifecyclePhase::Implementing { .. }) {
                        self.phase = LifecyclePhase::Implementing { change_id: None };
                    }
                }
                "understand" => {
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
