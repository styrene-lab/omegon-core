//! Omegon — Rust-native agent loop and lifecycle engine.
#![allow(dead_code)] // Phase 0 scaffold — fields/methods used as implementation fills in
//!
//! Phase 0: Headless agent loop for cleave children and standalone use.
//! Phase 1: Process owner with TUI bridge subprocess.
//! Phase 2: Native TUI rendering.
//! Phase 3: Native LLM provider clients.

use clap::Parser;
use std::path::PathBuf;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

mod bridge;
mod context;
mod conversation;
mod lifecycle;
mod r#loop;
mod prompt;
mod tools;

use bridge::SubprocessBridge;
use context::ContextManager;
use conversation::ConversationState;
use omegon_traits::AgentEvent;
use tools::CoreTools;

#[derive(Parser)]
#[command(name = "omegon-agent", about = "Omegon agent loop — headless coding agent")]
struct Cli {
    /// Working directory
    #[arg(short, long, default_value = ".")]
    cwd: PathBuf,

    /// Prompt to execute (headless mode)
    #[arg(short, long)]
    prompt: Option<String>,

    /// Path to the LLM bridge script
    #[arg(long)]
    bridge: Option<PathBuf>,

    /// Node.js binary path
    #[arg(long, default_value = "node")]
    node: String,

    /// Model identifier (provider:model format)
    #[arg(short, long, default_value = "anthropic:claude-sonnet-4-20250514")]
    model: String,

    /// Maximum turns before forced stop (0 = no limit)
    #[arg(long, default_value = "50")]
    max_turns: u32,

    /// Max retries on transient LLM errors
    #[arg(long, default_value = "3")]
    max_retries: u32,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();

    let cwd = std::fs::canonicalize(&cli.cwd)?;
    tracing::info!(cwd = %cwd.display(), model = %cli.model, "omegon-agent starting");

    let Some(prompt_text) = &cli.prompt else {
        eprintln!("Usage: omegon-agent --prompt \"<task>\" [--cwd <path>]");
        eprintln!();
        eprintln!("Headless coding agent — executes a task and exits.");
        eprintln!();
        eprintln!("Options:");
        eprintln!("  -m, --model <MODEL>     Model identifier (default: anthropic:claude-sonnet-4-20250514)");
        eprintln!("  --max-turns <N>         Maximum turns (default: 50, 0 = unlimited)");
        eprintln!("  --max-retries <N>       Max LLM error retries (default: 3)");
        std::process::exit(1);
    };

    // ─── Build loop config ──────────────────────────────────────────────
    let loop_config = r#loop::LoopConfig {
        max_turns: cli.max_turns,
        soft_limit_turns: if cli.max_turns > 0 {
            cli.max_turns * 2 / 3
        } else {
            0
        },
        max_retries: cli.max_retries,
        retry_delay_ms: 2000,
        model: cli.model.clone(),
    };

    // ─── Spawn LLM bridge ───────────────────────────────────────────────
    let bridge_path = cli
        .bridge
        .unwrap_or_else(SubprocessBridge::default_bridge_path);

    tracing::info!(bridge = %bridge_path.display(), "spawning LLM bridge");
    let bridge = SubprocessBridge::spawn(&bridge_path, &cli.node).await?;
    tracing::info!("LLM bridge ready");

    // ─── Set up tools ───────────────────────────────────────────────────
    let core_tools = CoreTools::new(cwd.clone());
    let tools: Vec<Box<dyn omegon_traits::ToolProvider>> = vec![Box::new(core_tools)];

    // ─── Build system prompt ────────────────────────────────────────────
    let tool_defs: Vec<_> = tools.iter().flat_map(|p| p.tools()).collect();
    let base_prompt = prompt::build_base_prompt(&cwd, &tool_defs);

    // ─── Set up context manager ─────────────────────────────────────────
    let mut context_manager = ContextManager::new(base_prompt, vec![]);

    // ─── Set up conversation ────────────────────────────────────────────
    let mut conversation = ConversationState::new();
    conversation.push_user(prompt_text.clone());

    // ─── Event channel ──────────────────────────────────────────────────
    let (events_tx, mut events_rx) = broadcast::channel::<AgentEvent>(256);

    // ─── Event printer (headless mode: print to stderr) ─────────────────
    tokio::spawn(async move {
        while let Ok(event) = events_rx.recv().await {
            match event {
                AgentEvent::TurnStart { turn } => {
                    tracing::info!("── Turn {turn} ──");
                }
                AgentEvent::MessageChunk { text } => {
                    eprint!("{text}");
                }
                AgentEvent::ThinkingChunk { text } => {
                    eprint!("\x1b[2m{text}\x1b[0m");
                }
                AgentEvent::ToolStart { name, .. } => {
                    tracing::info!("→ {name}");
                }
                AgentEvent::ToolEnd {
                    id: _,
                    result,
                    is_error,
                } => {
                    let status = if is_error { "✗" } else { "✓" };
                    let text = result
                        .content
                        .first()
                        .map(|c| match c {
                            omegon_traits::ContentBlock::Text { text } => {
                                if text.len() > 200 {
                                    format!("{}...", &text[..200])
                                } else {
                                    text.clone()
                                }
                            }
                            omegon_traits::ContentBlock::Image { .. } => "[image]".into(),
                        })
                        .unwrap_or_default();
                    tracing::info!("  {status} {text}");
                }
                AgentEvent::TurnEnd { turn } => {
                    tracing::info!("── Turn {turn} complete ──");
                }
                AgentEvent::AgentEnd => {
                    tracing::info!("Agent complete");
                }
                _ => {}
            }
        }
    });

    // ─── Run the loop ───────────────────────────────────────────────────
    let cancel = CancellationToken::new();

    let cancel_clone = cancel.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        tracing::warn!("Interrupted — cancelling");
        cancel_clone.cancel();
    });

    let result = r#loop::run(
        &bridge,
        &tools,
        &mut context_manager,
        &mut conversation,
        &events_tx,
        cancel,
        &loop_config,
    )
    .await;

    // Graceful bridge shutdown — send "shutdown" before kill_on_drop fires
    bridge.shutdown().await;

    match &result {
        Ok(()) => {
            if let Some(last_text) = conversation.last_assistant_text() {
                println!("{last_text}");
            }
        }
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    }

    result
}
