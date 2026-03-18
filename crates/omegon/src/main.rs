//! Omegon — Rust-native agent loop and lifecycle engine.
#![allow(dead_code)] // Phase 0 scaffold — fields/methods used as implementation fills in
//!
//! Phase 0: Headless agent loop for cleave children and standalone use.
//! Phase 1: Process owner with TUI bridge subprocess.
//! Phase 2: Native TUI rendering.
//! Phase 3: Native LLM provider clients.

use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

mod bridge;
mod cleave;
mod context;
mod conversation;
mod lifecycle;
mod r#loop;
mod prompt;
mod session;
mod setup;
mod tools;
mod tui;

use bridge::SubprocessBridge;
use omegon_traits::AgentEvent;

#[derive(Parser)]
#[command(name = "omegon-agent", about = "Omegon agent loop — headless coding agent")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Working directory
    #[arg(short, long, default_value = ".", global = true)]
    cwd: PathBuf,

    /// Path to the LLM bridge script
    #[arg(long, global = true)]
    bridge: Option<PathBuf>,

    /// Node.js binary path
    #[arg(long, default_value = "node", global = true)]
    node: String,

    /// Model identifier (provider:model format)
    #[arg(short, long, default_value = "anthropic:claude-sonnet-4-20250514", global = true)]
    model: String,

    // ── Agent mode args (used when no subcommand) ───────────────────────

    /// Prompt to execute (headless mode)
    #[arg(short, long)]
    prompt: Option<String>,

    /// Read prompt from a file instead of CLI argument
    #[arg(long)]
    prompt_file: Option<PathBuf>,

    /// Maximum turns before forced stop (0 = no limit)
    #[arg(long, default_value = "50")]
    max_turns: u32,

    /// Max retries on transient LLM errors
    #[arg(long, default_value = "3")]
    max_retries: u32,

    /// Resume a previous session. Without a value, resumes the most recent.
    /// With a value, matches by session ID prefix.
    #[arg(long)]
    resume: Option<Option<String>>,

    /// Disable session auto-save on exit.
    #[arg(long)]
    no_session: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Run interactive TUI session — ratatui-based terminal interface.
    Interactive,

    /// Run a cleave orchestration — dispatch multiple agent children in parallel.
    Cleave {
        /// Path to the plan JSON file
        #[arg(long)]
        plan: String,

        /// The directive (task description)
        #[arg(long)]
        directive: String,

        /// Workspace directory for worktrees and state.
        /// If workspace/state.json exists, it is loaded and resumed
        /// (preserving TS-written worktree paths and task files).
        #[arg(long)]
        workspace: String,

        /// Maximum parallel children
        #[arg(long, default_value = "4")]
        max_parallel: usize,

        /// Per-child wall-clock timeout in seconds
        #[arg(long, default_value = "900")]
        timeout: u64,

        /// Per-child idle timeout in seconds (no stderr output = stalled)
        #[arg(long, default_value = "180")]
        idle_timeout: u64,

        /// Max turns per child agent
        #[arg(long, default_value = "50")]
        max_turns: u32,
    },
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

    match cli.command {
        Some(Commands::Interactive) => run_interactive_command(&cli).await,
        Some(Commands::Cleave {
            ref plan,
            ref directive,
            ref workspace,
            max_parallel,
            timeout,
            idle_timeout,
            max_turns,
        }) => {
            run_cleave_command(
                &cli, Path::new(plan), directive, Path::new(workspace), max_parallel, timeout, idle_timeout, max_turns,
            )
            .await
        }
        None => run_agent_command(&cli).await,
    }
}

async fn run_cleave_command(
    cli: &Cli,
    plan_path: &Path,
    directive: &str,
    workspace: &Path,
    max_parallel: usize,
    timeout: u64,
    idle_timeout: u64,
    max_turns: u32,
) -> anyhow::Result<()> {
    let repo_path = std::fs::canonicalize(&cli.cwd)?;
    let plan_json = std::fs::read_to_string(plan_path)?;
    let plan = cleave::CleavePlan::from_json(&plan_json)?;

    tracing::info!(
        children = plan.children.len(),
        max_parallel,
        model = %cli.model,
        "cleave orchestration starting"
    );

    // Resolve self binary path for spawning children
    let agent_binary = std::env::current_exe()?;
    let bridge_path = cli
        .bridge
        .clone()
        .unwrap_or_else(SubprocessBridge::default_bridge_path);

    let config = cleave::orchestrator::CleaveConfig {
        agent_binary,
        bridge_path,
        node: cli.node.clone(),
        model: cli.model.clone(),
        max_parallel,
        timeout_secs: timeout,
        idle_timeout_secs: idle_timeout,
        max_turns,
    };

    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        tracing::warn!("Interrupted — cancelling cleave");
        cancel_clone.cancel();
    });

    let result = cleave::run_cleave(&plan, directive, &repo_path, workspace, &config, cancel).await?;

    // Print report
    eprintln!("\n## Cleave Report: {}", result.state.run_id);
    eprintln!("**Duration:** {:.0}s", result.duration_secs);
    eprintln!();

    let completed = result.state.children.iter().filter(|c| c.status == cleave::state::ChildStatus::Completed).count();
    let failed = result.state.children.iter().filter(|c| c.status == cleave::state::ChildStatus::Failed).count();
    eprintln!("**Children:** {} completed, {} failed of {}", completed, failed, result.state.children.len());
    eprintln!();

    for child in &result.state.children {
        let icon = match child.status {
            cleave::state::ChildStatus::Completed => "✓",
            cleave::state::ChildStatus::Failed => "✗",
            cleave::state::ChildStatus::Running => "⏳",
            cleave::state::ChildStatus::Pending => "○",
        };
        let dur = child.duration_secs.map(|d| format!(" ({:.0}s)", d)).unwrap_or_default();
        eprintln!("  {} **{}**{}: {:?}", icon, child.label, dur, child.status);
        if let Some(err) = &child.error {
            eprintln!("    Error: {}", err);
        }
    }

    eprintln!("\n### Merge Results");
    for (label, outcome) in &result.merge_results {
        match outcome {
            cleave::orchestrator::MergeOutcome::Success => eprintln!("  ✓ {} merged", label),
            cleave::orchestrator::MergeOutcome::Conflict(d) => eprintln!("  ✗ {} CONFLICT: {}", label, d.lines().next().unwrap_or("")),
            cleave::orchestrator::MergeOutcome::Failed(d) => eprintln!("  ✗ {} FAILED: {}", label, d.lines().next().unwrap_or("")),
            cleave::orchestrator::MergeOutcome::Skipped(reason) => eprintln!("  ○ {} skipped ({})", label, reason),
        }
    }

    // Post-merge guardrails (CLI only — TS wrapper runs its own)
    let all_merged = result.merge_results.iter().all(|(_, o)| matches!(o, cleave::orchestrator::MergeOutcome::Success));
    if all_merged && failed == 0 {
        let checks = cleave::guardrails::discover_guardrails(&repo_path);
        if !checks.is_empty() {
            let report = cleave::guardrails::run_guardrails(&repo_path, &checks);
            eprintln!("\n### Post-Merge Guardrails\n{report}");
        }
    }

    // Exit with error if any children failed
    if failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}


async fn run_interactive_command(cli: &Cli) -> anyhow::Result<()> {
    tracing::info!(model = %cli.model, "omegon interactive starting");

    // ─── Shared setup ───────────────────────────────────────────────────
    let resume = cli.resume.as_ref().map(|r| r.as_deref());
    let mut agent = setup::AgentSetup::new(&cli.cwd, resume).await?;

    // ─── Spawn LLM bridge ───────────────────────────────────────────────
    let bridge_path = cli
        .bridge
        .clone()
        .unwrap_or_else(SubprocessBridge::default_bridge_path);
    let bridge = SubprocessBridge::spawn(&bridge_path, &cli.node).await?;

    // ─── Event channel ──────────────────────────────────────────────────
    let (events_tx, events_rx) = broadcast::channel::<AgentEvent>(256);
    let (command_tx, mut command_rx) = tokio::sync::mpsc::channel::<tui::TuiCommand>(16);

    // ─── Launch TUI ─────────────────────────────────────────────────────
    let tui_model = cli.model.clone();
    let tui_handle = tokio::spawn(async move {
        if let Err(e) = tui::run_tui(events_rx, command_tx, tui_model).await {
            tracing::error!("TUI error: {e}");
        }
    });

    // ─── Interactive agent loop ─────────────────────────────────────────
    // Each prompt gets its own cancellation token so Ctrl+C only cancels
    // the current turn, not future turns.
    let mut active_cancel: Option<CancellationToken> = None;

    loop {
        // Wait for user input from TUI
        let cmd = match command_rx.recv().await {
            Some(cmd) => cmd,
            None => break, // TUI channel closed
        };

        match cmd {
            tui::TuiCommand::Quit => break,
            tui::TuiCommand::Cancel => {
                if let Some(ref cancel) = active_cancel {
                    cancel.cancel();
                }
            }
            tui::TuiCommand::UserPrompt(text) => {
                agent.conversation.push_user(text);

                let loop_config = r#loop::LoopConfig {
                    max_turns: cli.max_turns,
                    soft_limit_turns: if cli.max_turns > 0 { cli.max_turns * 2 / 3 } else { 0 },
                    max_retries: cli.max_retries,
                    retry_delay_ms: 2000,
                    model: cli.model.clone(),
                };

                let cancel = CancellationToken::new();
                active_cancel = Some(cancel.clone());
                if let Err(e) = r#loop::run(
                    &bridge,
                    &agent.tools,
                    &mut agent.context_manager,
                    &mut agent.conversation,
                    &events_tx,
                    cancel,
                    &loop_config,
                ).await {
                    tracing::error!("Agent loop error: {e}");
                }
                active_cancel.take();
            }
        }
    }

    // Save session
    if !cli.no_session {
        if let Err(e) = session::save_session(&agent.conversation, &agent.cwd) {
            tracing::debug!("Session save failed: {e}");
        }
    }

    bridge.shutdown().await;
    tui_handle.abort();
    Ok(())
}

async fn run_agent_command(cli: &Cli) -> anyhow::Result<()> {
    tracing::info!(model = %cli.model, "omegon-agent starting");

    // Resolve prompt from --prompt or --prompt-file
    let prompt_text = match (&cli.prompt, &cli.prompt_file) {
        (Some(p), _) => p.clone(),
        (None, Some(path)) => {
            std::fs::read_to_string(path)
                .map_err(|e| anyhow::anyhow!("Failed to read prompt file {}: {}", path.display(), e))?
        }
        (None, None) => {
            eprintln!("Usage: omegon-agent --prompt \"<task>\" [--cwd <path>]");
            eprintln!("       omegon-agent --prompt-file <path> [--cwd <path>]");
            eprintln!("       omegon-agent cleave --plan <plan.json> --directive \"<task>\" --workspace <dir>");
            eprintln!();
            eprintln!("Headless coding agent — executes a task and exits.");
            std::process::exit(1);
        }
    };

    // ─── Shared setup ───────────────────────────────────────────────────
    let resume = cli.resume.as_ref().map(|r| r.as_deref());
    let mut agent = setup::AgentSetup::new(&cli.cwd, resume).await?;
    agent.conversation.push_user(prompt_text.clone());

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
        .clone()
        .unwrap_or_else(SubprocessBridge::default_bridge_path);

    tracing::info!(bridge = %bridge_path.display(), "spawning LLM bridge");
    let bridge = SubprocessBridge::spawn(&bridge_path, &cli.node).await?;
    tracing::info!("LLM bridge ready");

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
        &agent.tools,
        &mut agent.context_manager,
        &mut agent.conversation,
        &events_tx,
        cancel,
        &loop_config,
    )
    .await;

    // ─── Save session ────────────────────────────────────────────────────
    if !cli.no_session {
        if agent.cwd.join(".cleave-prompt.md").exists() {
            // Cleave child: save to worktree-local file
            let session_path = agent.cwd.join(".cleave-session.json");
            if let Err(e) = agent.conversation.save_session(&session_path) {
                tracing::debug!("Cleave session save failed (non-fatal): {e}");
            }
        } else {
            // Standalone agent: save to ~/.pi/agent/sessions/
            match session::save_session(&agent.conversation, &agent.cwd) {
                Ok(path) => tracing::info!(path = %path.display(), "Session saved"),
                Err(e) => tracing::debug!("Session save failed (non-fatal): {e}"),
            }
        }
    }

    // Graceful bridge shutdown
    bridge.shutdown().await;

    match &result {
        Ok(()) => {
            if let Some(last_text) = agent.conversation.last_assistant_text() {
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
