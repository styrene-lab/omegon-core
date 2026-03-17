//! Agent loop state machine.
//!
//! The core prompt → LLM → tool dispatch → repeat cycle.
//! Consumes LlmEvents from the bridge, dispatches tool calls,
//! emits AgentEvents to subscribers.

use crate::bridge::{LlmBridge, LlmEvent};
use crate::context::ContextManager;
use crate::conversation::{AssistantMessage, ConversationState, ToolCall, ToolResultEntry};
use omegon_traits::{AgentEvent, ContentBlock, ToolProvider};
use serde_json::Value;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

/// Run the agent loop to completion.
pub async fn run(
    bridge: &dyn LlmBridge,
    tools: &[Box<dyn ToolProvider>],
    context: &mut ContextManager,
    conversation: &mut ConversationState,
    events: &broadcast::Sender<AgentEvent>,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    let tool_defs: Vec<_> = tools.iter().flat_map(|p| p.tools()).collect();
    let mut turn: u32 = 0;

    loop {
        if cancel.is_cancelled() {
            break;
        }

        turn += 1;
        conversation.intent.stats.turns = turn;
        let _ = events.send(AgentEvent::TurnStart { turn });

        // Build LLM-facing context
        let system_prompt =
            context.build_system_prompt(conversation.last_user_prompt(), conversation);
        let llm_messages = conversation.build_llm_view();

        // Stream LLM response via bridge
        let mut rx = bridge.stream(&system_prompt, &llm_messages, &tool_defs).await?;

        // Consume the stream, building the assistant message
        let assistant_msg = consume_llm_stream(&mut rx, events).await?;

        // Parse ambient capture blocks (omg: tags) from assistant text
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
            let _ = events.send(AgentEvent::TurnEnd { turn });
            break;
        }

        // Dispatch tool calls
        let results = dispatch_tools(tools, tool_calls, events, cancel.clone()).await;

        // Push tool results to conversation and update intent
        for result in &results {
            conversation.push_tool_result(result.clone());
        }
        conversation.intent.update_from_tools(tool_calls, &results);

        // Update lifecycle phase from tool activity
        context.update_phase_from_activity(tool_calls);

        let _ = events.send(AgentEvent::TurnEnd { turn });
    }

    let _ = events.send(AgentEvent::AgentEnd);
    Ok(())
}

/// Consume LlmEvents from the bridge, build an AssistantMessage.
async fn consume_llm_stream(
    rx: &mut tokio::sync::mpsc::Receiver<LlmEvent>,
    events: &broadcast::Sender<AgentEvent>,
) -> anyhow::Result<AssistantMessage> {
    let mut text_parts: Vec<String> = Vec::new();
    let mut thinking_parts: Vec<String> = Vec::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();

    // Per-content-index accumulators for tool call argument deltas
    let mut toolcall_args: std::collections::HashMap<usize, String> = std::collections::HashMap::new();
    let mut toolcall_names: std::collections::HashMap<usize, String> = std::collections::HashMap::new();
    let mut toolcall_ids: std::collections::HashMap<usize, String> = std::collections::HashMap::new();

    let mut final_message: Option<Value> = None;
    let mut _stop_reason = String::from("stop");

    let _ = events.send(AgentEvent::MessageStart {
        role: "assistant".into(),
    });

    while let Some(event) = rx.recv().await {
        match event {
            LlmEvent::Start { .. } => {
                // Initial partial — we build our own from deltas
            }
            LlmEvent::TextStart { .. } => {}
            LlmEvent::TextDelta { delta, .. } => {
                let _ = events.send(AgentEvent::MessageChunk { text: delta.clone() });
                // Accumulate into the current text part
                if let Some(last) = text_parts.last_mut() {
                    last.push_str(&delta);
                } else {
                    text_parts.push(delta);
                }
            }
            LlmEvent::TextEnd { .. } => {
                // Start a new text part on next text_start
                text_parts.push(String::new());
            }
            LlmEvent::ThinkingStart { .. } => {}
            LlmEvent::ThinkingDelta { delta, .. } => {
                let _ = events.send(AgentEvent::ThinkingChunk { text: delta.clone() });
                if let Some(last) = thinking_parts.last_mut() {
                    last.push_str(&delta);
                } else {
                    thinking_parts.push(delta);
                }
            }
            LlmEvent::ThinkingEnd { .. } => {
                thinking_parts.push(String::new());
            }
            LlmEvent::ToolCallStart { content_index } => {
                toolcall_args.insert(content_index, String::new());
            }
            LlmEvent::ToolCallDelta {
                content_index,
                delta,
            } => {
                if let Some(args) = toolcall_args.get_mut(&content_index) {
                    args.push_str(&delta);
                }
            }
            LlmEvent::ToolCallEnd {
                content_index,
                tool_call,
            } => {
                // Parse the tool call from the bridge event
                let id = tool_call
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let name = tool_call
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let arguments = tool_call
                    .get("arguments")
                    .cloned()
                    .unwrap_or(Value::Object(serde_json::Map::new()));

                toolcall_ids.insert(content_index, id.clone());
                toolcall_names.insert(content_index, name.clone());

                tool_calls.push(ToolCall {
                    id,
                    name,
                    arguments,
                });
            }
            LlmEvent::Done { reason, message } => {
                _stop_reason = reason;
                final_message = Some(message);
                break;
            }
            LlmEvent::Error { reason, error } => {
                _stop_reason = reason;
                // Extract error message
                let err_msg = error
                    .get("errorMessage")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Unknown error")
                    .to_string();
                final_message = Some(error);

                // If it's a real error (not just stop), bail
                if _stop_reason == "error" {
                    let _ = events.send(AgentEvent::MessageEnd);
                    anyhow::bail!("LLM error: {}", err_msg);
                }
                break;
            }
        }
    }

    let _ = events.send(AgentEvent::MessageEnd);

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
        raw: final_message.unwrap_or(Value::Null),
    })
}

/// Dispatch tool calls to the appropriate ToolProvider.
async fn dispatch_tools(
    tools: &[Box<dyn ToolProvider>],
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

        // Find the provider that owns this tool
        let provider = tools.iter().find(|p| p.tools().iter().any(|t| t.name == call.name));

        let (result, is_error) = match provider {
            Some(provider) => {
                match provider
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
