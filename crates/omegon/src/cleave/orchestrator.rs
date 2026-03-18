//! Cleave orchestrator — the main dispatch loop.
//!
//! Spawns omegon-agent children in git worktrees, manages dependency waves,
//! tracks state, and merges results.

use super::plan::CleavePlan;
use super::state::{ChildStatus, CleaveState};
use super::waves::compute_waves;
use super::worktree;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

/// Configuration for a cleave run.
pub struct CleaveConfig {
    pub agent_binary: PathBuf,
    pub bridge_path: PathBuf,
    pub node: String,
    pub model: String,
    pub max_parallel: usize,
    pub timeout_secs: u64,
    pub idle_timeout_secs: u64,
    pub max_turns: u32,
}

/// Result of a cleave run.
pub struct CleaveResult {
    pub state: CleaveState,
    pub merge_results: Vec<(String, MergeOutcome)>,
    pub duration_secs: f64,
}

pub enum MergeOutcome {
    Success,
    Conflict(String),
    Failed(String),
    Skipped(String),
}

/// Run the full cleave orchestration.
pub async fn run_cleave(
    plan: &CleavePlan,
    directive: &str,
    repo_path: &Path,
    workspace_path: &Path,
    config: &CleaveConfig,
    cancel: CancellationToken,
) -> Result<CleaveResult> {
    let started = Instant::now();
    let run_id = format!("clv-{}-{}", nanoid(8), nanoid(4));

    std::fs::create_dir_all(workspace_path)
        .context("Failed to create workspace directory")?;

    let mut state = CleaveState::from_plan(
        &run_id, directive, repo_path, workspace_path, plan, &config.model,
    );
    let state_path = workspace_path.join("state.json");
    state.save(&state_path)?;

    let waves = compute_waves(&plan.children);
    tracing::info!(
        waves = waves.len(),
        children = plan.children.len(),
        "cleave dispatch starting"
    );

    let semaphore = Arc::new(Semaphore::new(config.max_parallel));

    for (wave_idx, wave) in waves.iter().enumerate() {
        if cancel.is_cancelled() {
            tracing::warn!("cleave cancelled");
            break;
        }

        let wave_labels: Vec<&str> = wave.iter().map(|&i| plan.children[i].label.as_str()).collect();
        tracing::info!(wave = wave_idx, children = ?wave_labels, "dispatching wave");

        // ── Prepare children (worktrees, task files, status) ────────────
        struct ChildDispatchInfo {
            child_idx: usize,
            wt_path: PathBuf,
            label: String,
            prompt: String,
        }
        let mut to_dispatch: Vec<ChildDispatchInfo> = Vec::new();

        for &child_idx in wave {
            let label = state.children[child_idx].label.clone();
            let branch = state.children[child_idx].branch.clone().unwrap();

            // Create worktree
            match worktree::create_worktree(repo_path, workspace_path, child_idx, &label, &branch) {
                Ok(wt_path) => {
                    state.children[child_idx].worktree_path = Some(wt_path.to_string_lossy().to_string());

                    let task_path = workspace_path.join(format!("{}-task.md", child_idx));
                    let description = &state.children[child_idx].description;
                    let scope = &state.children[child_idx].scope;
                    let task_content = build_task_file(&label, description, scope, directive);
                    std::fs::write(&task_path, &task_content)?;

                    state.children[child_idx].status = ChildStatus::Running;

                    to_dispatch.push(ChildDispatchInfo {
                        child_idx,
                        wt_path,
                        label,
                        prompt: task_content,
                    });
                }
                Err(e) => {
                    state.children[child_idx].status = ChildStatus::Failed;
                    state.children[child_idx].error = Some(format!("Worktree creation failed: {e}"));
                    tracing::error!(child = %label, "worktree failed: {e}");
                }
            }
        }
        state.save(&state_path)?;

        // ── Dispatch children ───────────────────────────────────────────
        let mut handles = Vec::new();

        for info in to_dispatch {
            let sem = semaphore.clone();
            let child_cancel = cancel.clone();
            let agent_binary = config.agent_binary.clone();
            let bridge_path = config.bridge_path.clone();
            let node = config.node.clone();
            let model = config.model.clone();
            let max_turns = config.max_turns;
            let timeout_secs = config.timeout_secs;
            let idle_timeout_secs = config.idle_timeout_secs;

            let handle = tokio::spawn(async move {
                let _permit = sem.acquire().await.unwrap();
                let result = dispatch_child(
                    &agent_binary,
                    &bridge_path,
                    &node,
                    &model,
                    max_turns,
                    timeout_secs,
                    idle_timeout_secs,
                    &info.wt_path,
                    &info.label,
                    &info.prompt,
                    child_cancel,
                ).await;
                (info.child_idx, result)
            });
            handles.push(handle);
        }

        // ── Harvest results ─────────────────────────────────────────────
        for handle in handles {
            let (child_idx, result) = handle.await?;
            match result {
                Ok(output) => {
                    state.children[child_idx].status = ChildStatus::Completed;
                    state.children[child_idx].duration_secs = Some(output.duration_secs);
                    tracing::info!(
                        child = %state.children[child_idx].label,
                        duration = format!("{:.0}s", output.duration_secs),
                        "child completed"
                    );
                }
                Err(e) => {
                    state.children[child_idx].status = ChildStatus::Failed;
                    state.children[child_idx].error = Some(format!("{e}"));
                    tracing::error!(
                        child = %state.children[child_idx].label,
                        "child failed: {e}"
                    );
                }
            }
        }
        state.save(&state_path)?;
    }

    // ── Merge phase ─────────────────────────────────────────────────────
    tracing::info!("merge phase starting");
    let mut merge_results = Vec::new();

    for child in &state.children {
        if child.status != ChildStatus::Completed {
            merge_results.push((
                child.label.clone(),
                MergeOutcome::Skipped(format!("status: {:?}", child.status)),
            ));
            continue;
        }

        let branch = child.branch.as_deref().unwrap();

        // Remove worktree first so the branch is unlocked
        if let Some(wt) = &child.worktree_path {
            let _ = worktree::remove_worktree(repo_path, Path::new(wt));
        }

        match worktree::merge_branch(repo_path, branch) {
            Ok(worktree::MergeResult::Success) => {
                tracing::info!(child = %child.label, "merged successfully");
                let _ = worktree::delete_branch(repo_path, branch);
                merge_results.push((child.label.clone(), MergeOutcome::Success));
            }
            Ok(worktree::MergeResult::Conflict(detail)) => {
                tracing::warn!(child = %child.label, "merge conflict");
                merge_results.push((child.label.clone(), MergeOutcome::Conflict(detail)));
            }
            Ok(worktree::MergeResult::Failed(detail)) => {
                tracing::error!(child = %child.label, "merge failed");
                merge_results.push((child.label.clone(), MergeOutcome::Failed(detail)));
            }
            Err(e) => {
                merge_results.push((child.label.clone(), MergeOutcome::Failed(format!("{e}"))));
            }
        }
    }

    // Clean up remaining worktrees
    for child in &state.children {
        if let Some(wt) = &child.worktree_path {
            let _ = worktree::remove_worktree(repo_path, Path::new(wt));
        }
    }

    let duration_secs = started.elapsed().as_secs_f64();
    state.save(&state_path)?;

    Ok(CleaveResult {
        state,
        merge_results,
        duration_secs,
    })
}

struct ChildOutput {
    duration_secs: f64,
    #[allow(dead_code)]
    stdout: String,
}

/// Dispatch a single omegon-agent child process.
async fn dispatch_child(
    agent_binary: &Path,
    bridge_path: &Path,
    node: &str,
    model: &str,
    max_turns: u32,
    timeout_secs: u64,
    idle_timeout_secs: u64,
    cwd: &Path,
    label: &str,
    prompt: &str,
    cancel: CancellationToken,
) -> Result<ChildOutput> {
    let started = Instant::now();

    tracing::info!(child = %label, cwd = %cwd.display(), "spawning omegon-agent");

    let mut child = Command::new(agent_binary)
        .args([
            "--prompt", prompt,
            "--cwd", cwd.to_str().unwrap(),
            "--bridge", bridge_path.to_str().unwrap(),
            "--node", node,
            "--model", model,
            "--max-turns", &max_turns.to_string(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context(format!("Failed to spawn omegon-agent for child '{label}'"))?;

    let pid = child.id().unwrap_or(0);
    tracing::info!(child = %label, pid, "child spawned");

    let stderr = child.stderr.take().unwrap();
    let mut reader = BufReader::new(stderr).lines();

    let wall_timeout = tokio::time::Duration::from_secs(timeout_secs);
    let idle_timeout = tokio::time::Duration::from_secs(idle_timeout_secs);

    let mut last_activity = Instant::now();

    let io_result = tokio::select! {
        _ = tokio::time::sleep(wall_timeout) => {
            tracing::warn!(child = %label, "wall-clock timeout ({timeout_secs}s)");
            Err(anyhow::anyhow!("Wall-clock timeout after {timeout_secs}s"))
        }
        _ = cancel.cancelled() => {
            tracing::warn!(child = %label, "cancelled");
            Err(anyhow::anyhow!("Cancelled"))
        }
        result = async {
            loop {
                match tokio::time::timeout(idle_timeout, reader.next_line()).await {
                    Ok(Ok(Some(line))) => {
                        last_activity = Instant::now();
                        tracing::debug!(child = %label, "{line}");
                    }
                    Ok(Ok(None)) => break, // EOF — process exited
                    Ok(Err(e)) => {
                        tracing::warn!(child = %label, "stderr read error: {e}");
                        break;
                    }
                    Err(_) => {
                        let idle_secs = last_activity.elapsed().as_secs();
                        tracing::warn!(child = %label, idle_secs, "idle timeout");
                        return Err(anyhow::anyhow!(
                            "Idle timeout — no output for {idle_timeout_secs}s"
                        ));
                    }
                }
            }
            Ok::<(), anyhow::Error>(())
        } => {
            result
        }
    };

    // kill_on_drop will handle cleanup, but be explicit
    let _ = child.kill().await;
    let exit = child.wait().await?;

    let mut stdout_buf = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        use tokio::io::AsyncReadExt;
        let _ = stdout.read_to_string(&mut stdout_buf).await;
    }

    let duration_secs = started.elapsed().as_secs_f64();

    match io_result {
        Ok(()) if exit.success() => Ok(ChildOutput {
            duration_secs,
            stdout: stdout_buf,
        }),
        Ok(()) => Err(anyhow::anyhow!(
            "Child exited with code {}",
            exit.code().unwrap_or(-1)
        )),
        Err(e) => Err(e),
    }
}

fn build_task_file(label: &str, description: &str, scope: &[String], directive: &str) -> String {
    let scope_list = scope
        .iter()
        .map(|s| format!("- `{s}`"))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"# Task: {label}

## Directive

{directive}

## Your Assignment

{description}

## File Scope

{scope_list}

## Contract

- Work ONLY within the files listed in File Scope
- Commit your changes with a descriptive message
- If the task is already done or not applicable, commit a no-op and report completion
- Do NOT modify files outside your scope
"#
    )
}

fn nanoid(len: usize) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let chars = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut result = String::with_capacity(len);
    let mut n = seed;
    for _ in 0..len {
        result.push(chars[(n % 35) as usize] as char);
        n = n.wrapping_mul(6364136223846793005).wrapping_add(1);
    }
    result
}
