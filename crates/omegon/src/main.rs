//! Omegon — Rust-native agent loop and lifecycle engine.
//!
//! Phase 0: Headless agent loop for cleave children and standalone use.
//! Phase 1: Process owner with TUI bridge subprocess.
//! Phase 2: Native TUI rendering.
//! Phase 3: Native LLM provider clients.

use clap::Parser;
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

mod bridge;
mod context;
mod conversation;
mod lifecycle;
mod r#loop;
mod tools;

#[derive(Parser)]
#[command(name = "omegon-agent", about = "Omegon agent loop")]
struct Cli {
    /// Working directory
    #[arg(short, long, default_value = ".")]
    cwd: PathBuf,

    /// Prompt to execute (headless mode)
    #[arg(short, long)]
    prompt: Option<String>,

    /// Run in JSON-RPC sidecar mode (for parent Omegon integration)
    #[arg(long)]
    rpc: bool,

    /// Model to use (provider:model format)
    #[arg(short, long, default_value = "anthropic:claude-sonnet-4-20250514")]
    model: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();

    tracing::info!(cwd = %cli.cwd.display(), "omegon-agent starting");

    // TODO: Phase 0 implementation
    // 1. Spawn LLM bridge subprocess
    // 2. Initialize lifecycle store
    // 3. Register core tools
    // 4. Run agent loop (headless or RPC)

    if let Some(prompt) = &cli.prompt {
        tracing::info!(%prompt, "headless mode");
        // TODO: run_headless(prompt, cli.cwd, cli.model).await?;
    } else if cli.rpc {
        tracing::info!("RPC sidecar mode");
        // TODO: run_rpc(cli.cwd).await?;
    } else {
        eprintln!("omegon-agent: specify --prompt for headless mode or --rpc for sidecar mode");
        std::process::exit(1);
    }

    Ok(())
}
