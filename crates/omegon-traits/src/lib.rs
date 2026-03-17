//! Shared trait definitions for the Omegon agent loop.
//!
//! Feature crates implement these traits to participate in the agent runtime.
//! The traits are the Rust replacement for pi's extension API (registerTool,
//! registerCommand, pi.on events, ctx.ui).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

// ─── Tool Results ───────────────────────────────────────────────────────────

/// Content block in a tool result — text or image.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image")]
    Image { url: String, media_type: String },
}

/// Result returned from a tool execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub content: Vec<ContentBlock>,
    #[serde(default)]
    pub details: Value,
}

/// JSON Schema definition for a tool's parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub label: String,
    pub description: String,
    pub parameters: Value, // JSON Schema
}

// ─── Agent Events ───────────────────────────────────────────────────────────

/// Events emitted by the agent loop. Rendering backends subscribe to these.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    TurnStart { turn: u32 },
    MessageStart { role: String },
    MessageChunk { text: String },
    ThinkingChunk { text: String },
    MessageEnd,
    ToolStart { id: String, name: String, args: Value },
    ToolUpdate { id: String, partial: ToolResult },
    ToolEnd { id: String, result: ToolResult, is_error: bool },
    TurnEnd { turn: u32 },
    AgentEnd,
    // Lifecycle events
    PhaseChanged { phase: LifecyclePhase },
    DecompositionStarted { children: Vec<String> },
    DecompositionChildCompleted { label: String, success: bool },
    DecompositionCompleted { merged: bool },
}

/// The lifecycle phase the agent loop is currently in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LifecyclePhase {
    /// No structured lifecycle active. Simple tasks.
    Idle,
    /// Exploring a design question — open questions, research, options.
    Exploring { node_id: Option<String> },
    /// Specifying what to build — Given/When/Then scenarios.
    Specifying { change_id: Option<String> },
    /// Decomposing a task into parallel children.
    Decomposing,
    /// Implementing against a spec.
    Implementing { change_id: Option<String> },
    /// Verifying implementation against spec scenarios.
    Verifying { change_id: Option<String> },
}

impl Default for LifecyclePhase {
    fn default() -> Self {
        Self::Idle
    }
}

// ─── Context Injection ──────────────────────────────────────────────────────

/// Signals available to ContextProviders for deciding what to inject.
#[derive(Debug)]
pub struct ContextSignals<'a> {
    pub user_prompt: &'a str,
    pub recent_tools: &'a [String],
    pub recent_files: &'a [PathBuf],
    pub lifecycle_phase: &'a LifecyclePhase,
    pub turn_number: u32,
    pub context_budget_tokens: usize,
}

/// A piece of context to inject into the system prompt.
#[derive(Debug, Clone)]
pub struct ContextInjection {
    pub source: String,
    pub content: String,
    pub priority: u8,       // higher = more important, injected first
    pub ttl_turns: u32,     // how long to keep if not re-injected
}

// ─── Session Configuration ──────────────────────────────────────────────────

/// Configuration available to SessionHook implementations at startup.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    pub cwd: PathBuf,
    pub session_id: String,
}

/// Stats available to SessionHook implementations at shutdown.
#[derive(Debug, Clone)]
pub struct SessionStats {
    pub turns: u32,
    pub tool_calls: u32,
    pub duration_secs: f64,
}

// ─── The Four Traits ────────────────────────────────────────────────────────

/// Provides tools to the agent loop.
///
/// Each feature crate that exposes agent-callable tools implements this trait.
/// The agent loop collects all ToolProviders at startup and dispatches tool
/// calls to the provider that owns the requested tool name.
#[async_trait]
pub trait ToolProvider: Send + Sync {
    /// Return the tool definitions this provider offers.
    fn tools(&self) -> Vec<ToolDefinition>;

    /// Execute a tool call. Only called for tools returned by `tools()`.
    async fn execute(
        &self,
        tool_name: &str,
        call_id: &str,
        args: Value,
        cancel: tokio_util::sync::CancellationToken,
    ) -> anyhow::Result<ToolResult>;
}

/// Provides dynamic context for the ContextManager.
///
/// Called once per turn before the LLM request. Return `None` to inject
/// nothing this turn. The ContextManager collects all injections, sorts
/// by priority, and fits within the token budget.
pub trait ContextProvider: Send + Sync {
    fn provide_context(&self, signals: &ContextSignals<'_>) -> Option<ContextInjection>;
}

/// Reacts to agent events for side effects (logging, dashboard, etc.)
///
/// Must not block — events are broadcast via tokio::broadcast channel.
pub trait EventSubscriber: Send + Sync {
    fn on_event(&self, event: &AgentEvent);
}

/// Session lifecycle hooks.
#[async_trait]
pub trait SessionHook: Send + Sync {
    async fn on_session_start(&mut self, _config: &SessionConfig) -> anyhow::Result<()> {
        Ok(())
    }
    async fn on_session_end(&mut self, _stats: &SessionStats) -> anyhow::Result<()> {
        Ok(())
    }
}
