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
mod tools;

use bridge::SubprocessBridge;
use context::ContextManager;
use conversation::ConversationState;
use omegon_traits::AgentEvent;
use omegon_memory::MemoryBackend as _; // bring trait methods into scope
use tools::CoreTools;

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
}

#[derive(Subcommand)]
enum Commands {
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

/// Find the project root by walking up from cwd looking for .git (directory, not file).
/// For cleave worktrees (.git is a file), follows the gitdir to the real repo.
/// Falls back to cwd if no .git found.
fn find_project_root(cwd: &std::path::Path) -> std::path::PathBuf {
    let mut dir = cwd.to_path_buf();
    loop {
        let git_path = dir.join(".git");
        if git_path.is_dir() {
            return dir;
        }
        if git_path.is_file() {
            // Worktree: .git file contains "gitdir: /main/repo/.git/worktrees/name"
            if let Ok(content) = std::fs::read_to_string(&git_path) {
                if let Some(gitdir) = content.strip_prefix("gitdir: ") {
                    let gitdir = gitdir.trim();
                    let gitdir_path = if std::path::Path::new(gitdir).is_absolute() {
                        std::path::PathBuf::from(gitdir)
                    } else {
                        dir.join(gitdir)
                    };
                    // .git/worktrees/<name> → .git → repo root
                    if let Some(repo) = gitdir_path.parent()
                        .and_then(|p| p.parent())
                        .and_then(|p| p.parent())
                    {
                        return repo.to_path_buf();
                    }
                }
            }
            return dir; // fallback
        }
        if !dir.pop() { break; }
    }
    cwd.to_path_buf()
}

async fn run_agent_command(cli: &Cli) -> anyhow::Result<()> {
    let cwd = std::fs::canonicalize(&cli.cwd)?;
    tracing::info!(cwd = %cwd.display(), model = %cli.model, "omegon-agent starting");

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

    // ─── Set up tools ───────────────────────────────────────────────────
    let core_tools = CoreTools::new(cwd.clone());
    let mut tools: Vec<Box<dyn omegon_traits::ToolProvider>> = vec![Box::new(core_tools)];

    // ─── Set up memory ──────────────────────────────────────────────────
    // Mind name — use "default" to match the TS factstore convention.
    // The TS extension always uses "default" as the mind for project-local facts.
    let mind = "default".to_string();

    // DB path: <cwd>/.pi/memory/facts.db — matches TS factstore convention.
    // For cleave children (cwd is a worktree), walk up to the git repo root
    // to find the project's .pi/memory/ directory.
    let project_root = find_project_root(&cwd);
    let memory_dir = project_root.join(".pi").join("memory");
    let _ = std::fs::create_dir_all(&memory_dir);
    let db_path = memory_dir.join("facts.db");

    let jsonl_path = memory_dir.join("facts.jsonl");
    match omegon_memory::SqliteBackend::open(&db_path) {
        Ok(backend) => {
            tracing::info!(mind = %mind, db = %db_path.display(), "memory backend loaded");

            // Import JSONL on startup if DB is empty and facts.jsonl exists
            // (bootstraps the Rust DB from the TS-managed JSONL git-sync file)
            let stats = backend.stats(&mind).await.ok();
            if stats.as_ref().map_or(true, |s| s.active_facts == 0) && jsonl_path.exists() {
                if let Ok(jsonl) = std::fs::read_to_string(&jsonl_path) {
                    match backend.import_jsonl(&jsonl).await {
                        Ok(import) => tracing::info!(
                            imported = import.imported,
                            reinforced = import.reinforced,
                            skipped = import.skipped,
                            "imported facts.jsonl into empty DB"
                        ),
                        Err(e) => tracing::warn!("JSONL import failed (non-fatal): {e}"),
                    }
                }
            }

            let provider = omegon_memory::MemoryProvider::new(
                backend,
                omegon_memory::MarkdownRenderer,
                mind.clone(),
            );
            tools.push(Box::new(provider));
        }
        Err(e) => {
            tracing::warn!("memory backend failed to open (non-fatal): {e}");
        }
    }

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

    // Save session for potential resume — only if running inside a cleave worktree
    // (detected by the presence of .cleave-prompt.md written by the orchestrator).
    if cwd.join(".cleave-prompt.md").exists() {
        let session_path = cwd.join(".cleave-session.json");
        if let Err(e) = conversation.save_session(&session_path) {
            tracing::debug!("Session save failed (non-fatal): {e}");
        }
    }

    // JSONL export is intentional, not automatic.
    // The DB is the live mutable store; facts.jsonl is the tracked transport snapshot.
    // Export only happens via explicit memory_export or lifecycle reconciliation.
    // See design: memory-branch-aware-facts-transport

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
