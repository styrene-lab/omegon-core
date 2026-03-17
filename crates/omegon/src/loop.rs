//! Agent loop state machine.
//!
//! The core prompt → LLM → tool dispatch → repeat cycle.
//! Consumes LlmEvents from the bridge, dispatches tool calls,
//! emits AgentEvents to subscribers.

use crate::bridge::LlmBridge;
use crate::context::ContextManager;
use crate::conversation::ConversationState;
use omegon_traits::{AgentEvent, ToolDefinition, ToolProvider, ToolResult};
use serde_json::Value;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

/// Run the agent loop to completion.
///
/// This is the core state machine. Everything else in the binary
/// is support structure for this function.
pub async fn run(
    bridge: &dyn LlmBridge,
    tools: &[Box<dyn ToolProvider>],
    context: &mut ContextManager,
    conversation: &mut ConversationState,
    events: &broadcast::Sender<AgentEvent>,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    let tool_defs = collect_tool_definitions(tools);
    let mut turn: u32 = 0;

    loop {
        turn += 1;
        let _ = events.send(AgentEvent::TurnStart { turn });

        // Build the LLM-facing view with decay applied
        let system_prompt = context.build_system_prompt(
            conversation.last_user_prompt(),
            conversation,
        );
        let llm_messages = conversation.build_llm_view();

        // Stream LLM response via bridge
        let mut rx = bridge
            .stream(&system_prompt, &llm_messages, &tool_defs)
            .await?;

        // Consume the stream, building the assistant message
        let assistant_msg = consume_llm_stream(&mut rx, events).await?;

        // Parse ambient capture blocks (omg: tags) from assistant text
        let captured = crate::lifecycle::capture::parse_ambient_blocks(
            assistant_msg.text_content(),
        );
        if !captured.is_empty() {
            conversation.apply_ambient_captures(&captured);
        }

        // Push assistant message to conversation
        conversation.push_assistant(assistant_msg.clone());

        // Extract tool calls
        let tool_calls = assistant_msg.tool_calls();
        if tool_calls.is_empty() {
            // No tool calls — turn complete, agent done
            let _ = events.send(AgentEvent::TurnEnd { turn });
            break;
        }

        // Dispatch tool calls
        let results = dispatch_tools(tools, &tool_calls, events, cancel.clone()).await;

        // Push tool results to conversation
        for result in &results {
            conversation.push_tool_result(result.clone());
        }

        // Update intent document from tool calls
        conversation.intent.update_from_tools(&tool_calls, &results);

        // Update lifecycle phase from tool activity
        context.update_phase_from_activity(&tool_calls);

        let _ = events.send(AgentEvent::TurnEnd { turn });

        if cancel.is_cancelled() {
            break;
        }
    }

    let _ = events.send(AgentEvent::AgentEnd);
    Ok(())
}

fn collect_tool_definitions(tools: &[Box<dyn ToolProvider>]) -> Vec<ToolDefinition> {
    tools.iter().flat_map(|p| p.tools()).collect()
}

async fn consume_llm_stream(
    _rx: &mut tokio::sync::mpsc::Receiver<crate::bridge::LlmEvent>,
    _events: &broadcast::Sender<AgentEvent>,
) -> anyhow::Result<crate::conversation::AssistantMessage> {
    // TODO: consume events, emit AgentEvents, build AssistantMessage
    todo!("Phase 0: implement LLM stream consumption")
}

async fn dispatch_tools(
    _tools: &[Box<dyn ToolProvider>],
    _tool_calls: &[crate::conversation::ToolCall],
    _events: &broadcast::Sender<AgentEvent>,
    _cancel: CancellationToken,
) -> Vec<crate::conversation::ToolResultEntry> {
    // TODO: find the provider for each tool call, execute, emit events
    todo!("Phase 0: implement tool dispatch")
}
