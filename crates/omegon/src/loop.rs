//! Agent loop state machine.
//!
//! The core prompt → LLM → tool dispatch → repeat cycle.
//! Includes: turn limits, retry with backoff, stuck detection,
//! context wiring, and parallel tool dispatch.

use crate::bridge::{LlmBridge, LlmEvent, StreamOptions};
use crate::context::ContextManager;
use crate::conversation::{AssistantMessage, ConversationState, ToolCall, ToolResultEntry};
use omegon_traits::{AgentEvent, ContentBlock, ToolProvider};
use serde_json::Value;
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::time::Instant;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

/// Configuration for the agent loop.
pub struct LoopConfig {
    /// Maximum turns before forced stop. 0 = no limit.
    pub max_turns: u32,
    /// Turn at which to inject a "you're running long" advisory.
    /// Defaults to max_turns * 2/3.
    pub soft_limit_turns: u32,
    /// Max retries on transient LLM errors.
    pub max_retries: u32,
    /// Initial retry delay in milliseconds.
    pub retry_delay_ms: u64,
    /// Model string to pass to the bridge (e.g. "anthropic:claude-sonnet-4-20250514")
    pub model: String,
}

impl Default for LoopConfig {
    fn default() -> Self {
        Self {
            max_turns: 50,
            soft_limit_turns: 35,
            max_retries: 3,
            retry_delay_ms: 2000,
            model: "anthropic:claude-sonnet-4-20250514".into(),
        }
    }
}

/// Run the agent loop to completion.
pub async fn run(
    bridge: &dyn LlmBridge,
    tools: &[Box<dyn ToolProvider>],
    context: &mut ContextManager,
    conversation: &mut ConversationState,
    events: &broadcast::Sender<AgentEvent>,
    cancel: CancellationToken,
    config: &LoopConfig,
) -> anyhow::Result<()> {
    let tool_defs: Vec<_> = tools.iter().flat_map(|p| p.tools()).collect();

    // Pre-build tool name → provider index lookup
    let mut tool_index: HashMap<String, usize> = HashMap::new();
    for (i, provider) in tools.iter().enumerate() {
        for def in provider.tools() {
            tool_index.insert(def.name, i);
        }
    }

    let stream_options = StreamOptions {
        model: Some(config.model.clone()),
        reasoning: None, // TODO: configurable
    };

    let mut stuck_detector = StuckDetector::new();
    let session_start = Instant::now();
    let mut turn: u32 = 0;
    let mut commit_nudged = false;

    loop {
        if cancel.is_cancelled() {
            break;
        }

        turn += 1;
        conversation.intent.stats.turns = turn;

        // ─── Turn limit enforcement ─────────────────────────────────
        if config.max_turns > 0 && turn > config.max_turns {
            tracing::warn!("Hard turn limit reached ({} turns). Stopping.", config.max_turns);
            let _ = events.send(AgentEvent::TurnStart { turn });
            let _ = events.send(AgentEvent::TurnEnd { turn });
            break;
        }

        if config.soft_limit_turns > 0 && turn == config.soft_limit_turns {
            tracing::info!("Soft turn limit — injecting advisory");
            conversation.push_user(format!(
                "[System: You've been running for {} turns. If you're stuck, \
                 summarize your progress and what's blocking you. If you're \
                 making progress, continue — hard limit is {} turns.]",
                turn, config.max_turns
            ));
        }

        let _ = events.send(AgentEvent::TurnStart { turn });

        // ─── Stuck detection ────────────────────────────────────────
        if let Some(warning) = stuck_detector.check() {
            tracing::info!("Stuck detector: {warning}");
            conversation.push_user(format!("[System: {warning}]"));
        }

        // ─── Compaction check ────────────────────────────────────────
        // If context is getting large, try LLM-driven compaction.
        // The context_window default is 200k tokens (Anthropic models).
        // Trigger at 75% utilization.
        let context_window = 200_000;
        if conversation.needs_compaction(context_window, 0.75)
            && let Some((payload, evict_count)) = conversation.build_compaction_payload() {
                tracing::info!(
                    estimated_tokens = conversation.estimate_tokens(),
                    evict_count,
                    "Context utilization high — requesting LLM compaction"
                );
                // Use the bridge to summarize the evictable messages
                match compact_via_llm(bridge, &payload, &stream_options).await {
                    Ok(summary) => {
                        conversation.apply_compaction(summary);
                    }
                    Err(e) => {
                        tracing::warn!("LLM compaction failed: {e} — continuing with decay only");
                    }
                }
            }

        // ─── Inject IntentDocument if meaningful ─────────────────────
        if conversation.intent.stats.tool_calls > 0
            || conversation.intent.current_task.is_some()
            || conversation.intent.stats.compactions > 0
        {
            let intent_block = conversation.render_intent_for_injection();
            context.inject_intent(intent_block);
        }

        // ─── Build LLM-facing context ───────────────────────────────
        let system_prompt =
            context.build_system_prompt(conversation.last_user_prompt(), conversation);
        let llm_messages = conversation.build_llm_view();

        tracing::debug!(
            turn,
            system_prompt_len = system_prompt.len(),
            messages = llm_messages.len(),
            tools = tool_defs.len(),
            estimated_tokens = conversation.estimate_tokens(),
            "LLM context assembled"
        );

        // ─── Stream LLM response with retry ─────────────────────────
        let assistant_msg = stream_with_retry(
            bridge,
            &system_prompt,
            &llm_messages,
            &tool_defs,
            &stream_options,
            events,
            config,
        )
        .await?;

        // ─── Parse ambient capture blocks (omg: tags) ───────────────
        let captured =
            crate::lifecycle::capture::parse_ambient_blocks(assistant_msg.text_content());
        if !captured.is_empty() {
            conversation.apply_ambient_captures(&captured);
        }

        // Push assistant message to conversation
        conversation.push_assistant(assistant_msg.clone());

        // Extract tool calls
        let tool_calls = assistant_msg.tool_calls();
        if tool_calls.is_empty() {
            // Check if the agent skipped committing.
            // If the conversation has edit/write calls but hasn't been nudged yet,
            // give it one more turn to commit.
            if !commit_nudged && has_mutations(conversation) && turn < config.max_turns {
                commit_nudged = true;
                tracing::info!("Agent stopped without committing — nudging");
                conversation.push_user(
                    "[System: You made file changes but did not run `git add` and `git commit`. \
                     Please commit your work now with a descriptive message, then summarize what you did.]"
                        .to_string(),
                );
                let _ = events.send(AgentEvent::TurnEnd { turn });
                continue; // give it one more turn to commit
            }
            let _ = events.send(AgentEvent::TurnEnd { turn });
            break;
        }

        // ─── Dispatch tool calls ────────────────────────────────────
        let results =
            dispatch_tools(tools, &tool_index, tool_calls, events, cancel.clone()).await;

        // Push tool results to conversation and update intent
        for result in &results {
            conversation.push_tool_result(result.clone());
        }
        conversation.intent.update_from_tools(tool_calls, &results);

        // ─── Wire context signals ───────────────────────────────────
        for call in tool_calls {
            context.record_tool_call(&call.name);
            // Track file access from tool arguments
            if let Some(path) = call.arguments.get("path").and_then(|v| v.as_str()) {
                context.record_file_access(std::path::PathBuf::from(path));
            }
        }
        context.update_phase_from_activity(tool_calls);

        // ─── Feed stuck detector ────────────────────────────────────
        for call in tool_calls {
            let is_error = results
                .iter()
                .find(|r| r.call_id == call.id)
                .is_some_and(|r| r.is_error);
            stuck_detector.record(call, is_error);
        }

        let _ = events.send(AgentEvent::TurnEnd { turn });
    }

    let elapsed = session_start.elapsed();
    tracing::info!(
        turns = turn,
        tool_calls = conversation.intent.stats.tool_calls,
        elapsed_secs = elapsed.as_secs(),
        "Agent loop complete"
    );

    let _ = events.send(AgentEvent::AgentEnd);
    Ok(())
}

/// Request an LLM-driven compaction summary for old conversation messages.
async fn compact_via_llm(
    bridge: &dyn LlmBridge,
    payload: &str,
    options: &StreamOptions,
) -> anyhow::Result<String> {
    let system = "You are a conversation summarizer. Produce a concise summary \
                  preserving: what was done, what failed, constraints discovered, \
                  and current approach. Output only the summary, no preamble.";

    let messages = vec![crate::bridge::LlmMessage::User {
        content: payload.to_string(),
    }];

    let mut rx = bridge
        .stream(system, &messages, &[], options)
        .await?;

    let mut summary = String::new();
    while let Some(event) = rx.recv().await {
        match event {
            LlmEvent::TextDelta { delta } => summary.push_str(&delta),
            LlmEvent::Done { .. } => break,
            LlmEvent::Error { message } => {
                return Err(anyhow::anyhow!("Compaction LLM error: {message}"));
            }
            _ => {}
        }
    }

    if summary.is_empty() {
        return Err(anyhow::anyhow!("Compaction produced empty summary"));
    }

    tracing::info!(summary_len = summary.len(), "Compaction summary received");
    Ok(summary)
}

/// Stream an LLM response with retry on transient errors.
async fn stream_with_retry(
    bridge: &dyn LlmBridge,
    system_prompt: &str,
    messages: &[crate::bridge::LlmMessage],
    tools: &[omegon_traits::ToolDefinition],
    options: &StreamOptions,
    events: &broadcast::Sender<AgentEvent>,
    config: &LoopConfig,
) -> anyhow::Result<AssistantMessage> {
    let mut attempt = 0;
    let mut delay = config.retry_delay_ms;

    loop {
        attempt += 1;
        let mut rx = bridge
            .stream(system_prompt, messages, tools, options)
            .await?;

        match consume_llm_stream(&mut rx, events).await {
            Ok(msg) => return Ok(msg),
            Err(e) => {
                let err_msg = e.to_string();
                let is_transient = is_transient_error(&err_msg);

                if !is_transient || attempt > config.max_retries {
                    if attempt > 1 {
                        tracing::error!(
                            "LLM error after {attempt} attempts: {err_msg}"
                        );
                    }
                    return Err(e);
                }

                tracing::warn!(
                    attempt,
                    max = config.max_retries,
                    delay_ms = delay,
                    "Transient LLM error, retrying: {err_msg}"
                );
                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                delay = (delay * 2).min(30_000); // exponential backoff, cap at 30s
            }
        }
    }
}

/// Heuristic: is this error message transient (worth retrying)?
///
/// Matches known transient error patterns. HTTP status codes use word-boundary
/// matching to avoid false positives (e.g. "model gpt-500" shouldn't match).
fn is_transient_error(msg: &str) -> bool {
    let lower = msg.to_lowercase();

    // Semantic patterns — safe as substring matches
    if lower.contains("overloaded")
        || lower.contains("rate limit")
        || lower.contains("rate_limit")
        || lower.contains("timeout")
        || lower.contains("server_error")
        || lower.contains("capacity")
        || lower.contains("temporarily")
        || lower.contains("try again")
        || lower.contains("service unavailable")
        || lower.contains("bad gateway")
        || lower.contains("internal server error")
    {
        return true;
    }

    // HTTP status codes — require word boundary (space, punctuation, or start/end)
    // to avoid matching model names like "gpt-500" or version strings
    for code in ["500", "502", "503", "529"] {
        if contains_word(&lower, code) {
            return true;
        }
    }

    false
}

/// Check if `text` contains `word` as a standalone token.
/// Word boundaries: spaces, punctuation, and start/end of string.
/// Hyphens and underscores are treated as word-joining (so "gpt-500" doesn't match "500").
fn contains_word(text: &str, word: &str) -> bool {
    let mut start = 0;
    while let Some(pos) = text[start..].find(word) {
        let abs_pos = start + pos;
        let before_ok = abs_pos == 0 || !is_word_char(text.as_bytes()[abs_pos - 1]);
        let after_pos = abs_pos + word.len();
        let after_ok = after_pos >= text.len() || !is_word_char(text.as_bytes()[after_pos]);
        if before_ok && after_ok {
            return true;
        }
        start = abs_pos + 1;
    }
    false
}

/// Is this byte part of a "word" for boundary detection?
/// Alphanumeric plus hyphen and underscore (common in model names, versions).
fn is_word_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-' || b == b'_'
}

/// Consume LlmEvents from the bridge, build an AssistantMessage.
async fn consume_llm_stream(
    rx: &mut tokio::sync::mpsc::Receiver<LlmEvent>,
    events: &broadcast::Sender<AgentEvent>,
) -> anyhow::Result<AssistantMessage> {
    let mut text_parts: Vec<String> = Vec::new();
    let mut thinking_parts: Vec<String> = Vec::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let mut final_raw: Value = Value::Null;

    let _ = events.send(AgentEvent::MessageStart {
        role: "assistant".into(),
    });

    while let Some(event) = rx.recv().await {
        match event {
            LlmEvent::Start => {} // Initial partial message — ignored
            LlmEvent::TextStart => {}
            LlmEvent::TextDelta { delta } => {
                let _ = events.send(AgentEvent::MessageChunk { text: delta.clone() });
                if let Some(last) = text_parts.last_mut() {
                    last.push_str(&delta);
                } else {
                    text_parts.push(delta);
                }
            }
            LlmEvent::TextEnd => {
                text_parts.push(String::new());
            }
            LlmEvent::ThinkingStart => {}
            LlmEvent::ThinkingDelta { delta } => {
                let _ = events.send(AgentEvent::ThinkingChunk { text: delta.clone() });
                if let Some(last) = thinking_parts.last_mut() {
                    last.push_str(&delta);
                } else {
                    thinking_parts.push(delta);
                }
            }
            LlmEvent::ThinkingEnd => {
                thinking_parts.push(String::new());
            }
            LlmEvent::ToolCallStart => {}
            LlmEvent::ToolCallDelta { .. } => {
                // Deltas accumulated by the bridge — complete tool call in ToolCallEnd
            }
            LlmEvent::ToolCallEnd { tool_call } => {
                tool_calls.push(ToolCall {
                    id: tool_call.id,
                    name: tool_call.name,
                    arguments: tool_call.arguments,
                });
            }
            LlmEvent::Done { message } => {
                final_raw = message.get("raw").cloned().unwrap_or(message);
                break;
            }
            LlmEvent::Error { message } => {
                let _ = events.send(AgentEvent::MessageEnd);
                anyhow::bail!("LLM error: {message}");
            }
        }
    }

    let _ = events.send(AgentEvent::MessageEnd);

    // Detect incomplete streams — if we never got a Done event, the bridge
    // probably died. An empty message with no text and no tool calls is
    // almost certainly a dropped connection, not a valid LLM response.
    if final_raw == Value::Null && text_parts.is_empty() && tool_calls.is_empty() {
        anyhow::bail!(
            "LLM stream ended without a completion event — the bridge may have crashed"
        );
    }

    // Clean up empty trailing parts
    while text_parts.last().is_some_and(|s| s.is_empty()) {
        text_parts.pop();
    }
    while thinking_parts.last().is_some_and(|s| s.is_empty()) {
        thinking_parts.pop();
    }

    let text = text_parts.join("");
    let thinking = if thinking_parts.is_empty() {
        None
    } else {
        Some(thinking_parts.join(""))
    };

    Ok(AssistantMessage {
        text,
        thinking,
        tool_calls,
        raw: final_raw,
    })
}

/// Dispatch tool calls to their providers.
///
/// Currently sequential. True parallel dispatch requires `Arc<dyn ToolProvider>`
/// which we'll add when the trait bound is relaxed in Phase 1.
/// The `is_readonly_tool` classification is preserved for that future.
async fn dispatch_tools(
    tools: &[Box<dyn ToolProvider>],
    tool_index: &HashMap<String, usize>,
    tool_calls: &[ToolCall],
    events: &broadcast::Sender<AgentEvent>,
    cancel: CancellationToken,
) -> Vec<ToolResultEntry> {
    let mut results = Vec::with_capacity(tool_calls.len());

    for call in tool_calls {
        let _ = events.send(AgentEvent::ToolStart {
            id: call.id.clone(),
            name: call.name.clone(),
            args: call.arguments.clone(),
        });

        let (result, is_error) = match tool_index.get(&call.name) {
            Some(&provider_idx) => {
                match tools[provider_idx]
                    .execute(&call.name, &call.id, call.arguments.clone(), cancel.clone())
                    .await
                {
                    Ok(result) => (result, false),
                    Err(e) => (
                        omegon_traits::ToolResult {
                            content: vec![ContentBlock::Text {
                                text: e.to_string(),
                            }],
                            details: Value::Null,
                        },
                        true,
                    ),
                }
            }
            None => (
                omegon_traits::ToolResult {
                    content: vec![ContentBlock::Text {
                        text: format!("Tool '{}' not found", call.name),
                    }],
                    details: Value::Null,
                },
                true,
            ),
        };

        let _ = events.send(AgentEvent::ToolEnd {
            id: call.id.clone(),
            result: result.clone(),
            is_error,
        });

        results.push(ToolResultEntry {
            call_id: call.id.clone(),
            tool_name: call.name.clone(),
            content: result.content,
            is_error,
        });
    }

    results
}

/// Is this tool read-only (safe to run in parallel when we add Arc<dyn ToolProvider>)?
/// Note: bash is NOT read-only — commands can have arbitrary side effects.
#[allow(dead_code)]
fn is_readonly_tool(name: &str) -> bool {
    matches!(name, "read" | "understand")
}

/// Check if the conversation contains any file mutations (edit or write calls).
fn has_mutations(conversation: &ConversationState) -> bool {
    !conversation.intent.files_modified.is_empty()
}



// ─── Stuck detection ────────────────────────────────────────────────────────

/// Detects pathological tool-call patterns that indicate the agent is stuck.
struct StuckDetector {
    /// Recent tool calls as (name, args_hash, was_error)
    recent: Vec<(String, u64, bool)>,
    /// Window size for pattern detection
    window: usize,
}

impl StuckDetector {
    fn new() -> Self {
        Self {
            recent: Vec::new(),
            window: 10,
        }
    }

    /// Record a tool call for pattern analysis.
    fn record(&mut self, call: &ToolCall, is_error: bool) {
        let args_hash = hash_value(&call.arguments);
        self.recent
            .push((call.name.clone(), args_hash, is_error));
        if self.recent.len() > self.window * 2 {
            self.recent.drain(..self.window);
        }
    }

    /// Check for stuck patterns. Returns a warning message if detected.
    fn check(&self) -> Option<String> {
        let len = self.recent.len();
        if len < 3 {
            return None;
        }

        let window = &self.recent[len.saturating_sub(self.window)..];

        // Pattern 1: Same tool + same args called 3+ times
        if let Some(repeated) = self.find_repeated_call(window, 3) {
            return Some(format!(
                "You've called `{}` with the same arguments {} times. \
                 If it's not producing the result you need, try a different approach.",
                repeated.0, repeated.1
            ));
        }

        // Pattern 2: Edit failures — repeated error on the same tool
        let recent_errors: Vec<_> = window
            .iter()
            .filter(|(_, _, err)| *err)
            .collect();
        if recent_errors.len() >= 3 {
            let names: Vec<_> = recent_errors.iter().map(|(n, _, _)| n.as_str()).collect();
            if names.windows(3).any(|w| w[0] == w[1] && w[1] == w[2]) {
                return Some(format!(
                    "Your last several `{}` calls returned errors. \
                     Consider reading the current file state before retrying.",
                    recent_errors.last().unwrap().0
                ));
            }
        }

        // Pattern 3: read-without-modify loop — same file read 3+ times
        // without any write/edit to that file
        let reads: Vec<_> = window
            .iter()
            .filter(|(name, _, _)| name == "read")
            .collect();
        if reads.len() >= 3 {
            // Check if the same args hash appears 3+ times
            let mut hash_counts: HashMap<u64, u32> = HashMap::new();
            for (_, h, _) in &reads {
                *hash_counts.entry(*h).or_default() += 1;
            }
            if hash_counts.values().any(|&c| c >= 3) {
                return Some(
                    "You've read the same file multiple times without modifying it. \
                     Consider noting what you need from it, or try a different approach."
                        .into(),
                );
            }
        }

        None
    }

    /// Find a (tool_name, count) where the same tool+args appears N+ times in the window.
    fn find_repeated_call(&self, window: &[(String, u64, bool)], threshold: usize) -> Option<(String, usize)> {
        let mut counts: HashMap<(String, u64), usize> = HashMap::new();
        for (name, hash, _) in window {
            let key = (name.clone(), *hash);
            *counts.entry(key).or_default() += 1;
        }
        counts
            .into_iter()
            .find(|(_, count)| *count >= threshold)
            .map(|((name, _), count)| (name, count))
    }
}

/// Hash a serde_json::Value for comparison (not cryptographic — just dedup).
fn hash_value(v: &Value) -> u64 {
    let mut hasher = DefaultHasher::new();
    let s = v.to_string();
    s.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_error_detection() {
        // Should match: known transient patterns
        assert!(is_transient_error("503 Service Unavailable"));
        assert!(is_transient_error("Request rate limit exceeded"));
        assert!(is_transient_error("Server is overloaded"));
        assert!(is_transient_error("transient_server_error"));
        assert!(is_transient_error("temporarily unavailable, try again later"));
        assert!(is_transient_error("HTTP 500 Internal Server Error"));
        assert!(is_transient_error("error 529: capacity exceeded"));
        assert!(is_transient_error("502 Bad Gateway"));
        assert!(is_transient_error("service unavailable"));

        // Should NOT match: permanent errors
        assert!(!is_transient_error("Invalid API key"));
        assert!(!is_transient_error("Model not found"));

        // Should NOT match: status codes embedded in non-error contexts
        assert!(!is_transient_error("model gpt-500 not found"));
        assert!(!is_transient_error("using port 5029"));
        assert!(!is_transient_error("version 5.0.3 released"));
    }

    #[test]
    fn contains_word_boundary() {
        // Standalone status codes — should match
        assert!(contains_word("error 500 occurred", "500"));
        assert!(contains_word("500 error", "500"));
        assert!(contains_word("error: 500", "500"));
        assert!(contains_word("HTTP/1.1 503", "503"));

        // Hyphen-joined (model names, identifiers) — should NOT match
        assert!(!contains_word("gpt-500", "500"));
        assert!(!contains_word("model-500x", "500"));
        assert!(!contains_word("error_code_500x", "500"));

        // Embedded in larger numbers — should NOT match
        assert!(!contains_word("port5003", "500"));
        assert!(!contains_word("50000 items", "500"));
    }

    #[test]
    fn stuck_detector_repeated_calls() {
        let mut detector = StuckDetector::new();
        let call = ToolCall {
            id: "1".into(),
            name: "read".into(),
            arguments: serde_json::json!({"path": "foo.rs"}),
        };

        detector.record(&call, false);
        detector.record(&call, false);
        assert!(detector.check().is_none());

        detector.record(&call, false);
        let warning = detector.check();
        assert!(warning.is_some());
        assert!(warning.unwrap().contains("same arguments"));
    }

    #[test]
    fn stuck_detector_repeated_errors() {
        let mut detector = StuckDetector::new();
        let call = ToolCall {
            id: "1".into(),
            name: "edit".into(),
            arguments: serde_json::json!({"path": "foo.rs", "oldText": "a", "newText": "b"}),
        };

        detector.record(&call, true);
        detector.record(&call, true);
        detector.record(&call, true);

        // This triggers the repeated-call pattern (same args 3x)
        let warning = detector.check();
        assert!(warning.is_some());
    }
}
