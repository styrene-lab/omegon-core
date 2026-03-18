//! Git worktree management for cleave children.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Create a git worktree for a child branch.
///
/// 1. Creates the branch from HEAD
/// 2. Creates the worktree at `<workspace>/<child_id>-<label>`
pub fn create_worktree(
    repo_path: &Path,
    workspace_path: &Path,
    child_id: usize,
    label: &str,
    branch: &str,
) -> Result<PathBuf> {
    let worktree_dir = workspace_path.join(format!("{}-wt-{}", child_id, label));

    // Create branch from HEAD (ignore error if it already exists)
    let _ = Command::new("git")
        .args(["branch", branch, "HEAD"])
        .current_dir(repo_path)
        .output();

    // Create worktree
    let output = Command::new("git")
        .args(["worktree", "add", worktree_dir.to_str().unwrap(), branch])
        .current_dir(repo_path)
        .output()
        .context("Failed to run git worktree add")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // If worktree already exists, that's fine
        if !stderr.contains("already exists") {
            anyhow::bail!("git worktree add failed: {}", stderr.trim());
        }
    }

    Ok(worktree_dir)
}

/// Remove a git worktree.
pub fn remove_worktree(repo_path: &Path, worktree_path: &Path) -> Result<()> {
    let output = Command::new("git")
        .args([
            "worktree",
            "remove",
            "--force",
            worktree_path.to_str().unwrap(),
        ])
        .current_dir(repo_path)
        .output()
        .context("Failed to run git worktree remove")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::warn!("git worktree remove warning: {}", stderr.trim());
    }

    // Also delete the branch
    // (don't fail if branch deletion fails — worktree removal is the priority)
    let _ = Command::new("git")
        .args(["branch", "-D"])
        .current_dir(repo_path)
        .output();

    Ok(())
}

/// Merge a child branch back into the current branch.
pub fn merge_branch(repo_path: &Path, branch: &str) -> Result<MergeResult> {
    let output = Command::new("git")
        .args(["merge", "--no-ff", "-m", &format!("cleave: merge {}", branch), branch])
        .current_dir(repo_path)
        .output()
        .context("Failed to run git merge")?;

    if output.status.success() {
        Ok(MergeResult::Success)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("CONFLICT") || stderr.contains("conflict") {
            // Abort the merge
            let _ = Command::new("git")
                .args(["merge", "--abort"])
                .current_dir(repo_path)
                .output();
            Ok(MergeResult::Conflict(stderr.to_string()))
        } else {
            let _ = Command::new("git")
                .args(["merge", "--abort"])
                .current_dir(repo_path)
                .output();
            Ok(MergeResult::Failed(stderr.to_string()))
        }
    }
}

/// Delete a branch after merge.
pub fn delete_branch(repo_path: &Path, branch: &str) -> Result<()> {
    let output = Command::new("git")
        .args(["branch", "-D", branch])
        .current_dir(repo_path)
        .output()
        .context("Failed to delete branch")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::warn!("branch delete warning: {}", stderr.trim());
    }
    Ok(())
}

#[derive(Debug)]
pub enum MergeResult {
    Success,
    Conflict(String),
    Failed(String),
}
