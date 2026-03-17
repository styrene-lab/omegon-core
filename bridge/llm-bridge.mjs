#!/usr/bin/env node
/**
 * LLM Bridge — Node.js subprocess that relays pi-ai streamSimple() as ndjson.
 *
 * Protocol: ndjson over stdin/stdout.
 *   Rust → Node: {"id":1,"method":"stream","params":{model,context,options}}
 *   Node → Rust: {"id":1,"event":{type:"text_delta",delta:"...", ...}}  (one per line, streamed)
 *   Node → Rust: {"id":1,"result":{...final message...}}  (terminal)
 *   Node → Rust: {"id":1,"error":"message"}  (terminal, on failure)
 *
 * Stderr is used for tracing/diagnostics — never parsed by Rust.
 *
 * The bridge is long-lived: spawned once, handles multiple stream requests.
 */

import { createInterface } from "readline";
import { streamSimple } from "@styrene-lab/pi-ai";

const rl = createInterface({ input: process.stdin, crlfDelay: Infinity });

function send(obj) {
  process.stdout.write(JSON.stringify(obj) + "\n");
}

rl.on("line", async (line) => {
  let req;
  try {
    req = JSON.parse(line);
  } catch (e) {
    send({ id: null, error: `Invalid JSON: ${e.message}` });
    return;
  }

  const { id, method, params } = req;

  if (method === "shutdown") {
    send({ id, result: "ok" });
    process.exit(0);
  }

  if (method !== "stream") {
    send({ id, error: `Unknown method: ${method}` });
    return;
  }

  const { model, context, options } = params;

  try {
    const eventStream = streamSimple(model, context, options ?? {});

    for await (const event of eventStream) {
      // Strip the partial AssistantMessage from delta events — it's large
      // and redundant (Rust builds its own from the deltas).
      // Keep it only for start (initial shape) and done/error (final message).
      const slim = slimEvent(event);
      send({ id, event: slim });
    }

    // Get the final message
    const finalMessage = await eventStream.result();
    send({ id, result: finalMessage });
  } catch (e) {
    send({ id, error: e.message ?? String(e) });
  }
});

rl.on("close", () => {
  process.exit(0);
});

/**
 * Slim down streaming events to reduce ndjson bandwidth.
 * Delta events carry the full partial AssistantMessage — redundant since
 * Rust builds the message incrementally from the deltas.
 */
function slimEvent(event) {
  switch (event.type) {
    case "text_delta":
      return { type: "text_delta", contentIndex: event.contentIndex, delta: event.delta };
    case "thinking_delta":
      return { type: "thinking_delta", contentIndex: event.contentIndex, delta: event.delta };
    case "toolcall_delta":
      return { type: "toolcall_delta", contentIndex: event.contentIndex, delta: event.delta };
    case "toolcall_end":
      return { type: "toolcall_end", contentIndex: event.contentIndex, toolCall: event.toolCall };
    case "text_start":
    case "text_end":
    case "thinking_start":
    case "thinking_end":
    case "toolcall_start":
      return { type: event.type, contentIndex: event.contentIndex };
    case "start":
      // Send the partial for initial message shape
      return { type: "start", partial: event.partial };
    case "done":
      return { type: "done", reason: event.reason, message: event.message };
    case "error":
      return { type: "error", reason: event.reason, error: event.error };
    default:
      return event;
  }
}

// Signal readiness
process.stderr.write("llm-bridge: ready\n");
