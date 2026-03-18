//! Cleave run state — persisted to state.json during execution.

use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Instant;

/// Overall cleave run state.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CleaveState {
    pub run_id: String,
    pub directive: String,
    pub repo_path: String,
    pub workspace_path: String,
    pub children: Vec<ChildState>,
    pub plan: serde_json::Value,
    #[serde(skip)]
    pub started_at: Option<Instant>,
}

/// Per-child state.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChildState {
    pub child_id: usize,
    pub label: String,
    pub description: String,
    pub scope: Vec<String>,
    pub depends_on: Vec<String>,
    pub status: ChildStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<String>,
    pub backend: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execute_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_secs: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ChildStatus {
    Pending,
    Running,
    Completed,
    Failed,
}

impl CleaveState {
    /// Save state to disk.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    /// Load state from disk.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let json = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&json)?)
    }

    /// Build initial state from a plan.
    pub fn from_plan(
        run_id: &str,
        directive: &str,
        repo_path: &Path,
        workspace_path: &Path,
        plan: &super::plan::CleavePlan,
        model: &str,
    ) -> Self {
        let children = plan
            .children
            .iter()
            .enumerate()
            .map(|(i, c)| ChildState {
                child_id: i,
                label: c.label.clone(),
                description: c.description.clone(),
                scope: c.scope.clone(),
                depends_on: c.depends_on.clone(),
                status: ChildStatus::Pending,
                error: None,
                branch: Some(format!("cleave/{}-{}", i, c.label)),
                worktree_path: None,
                backend: "native".to_string(),
                execute_model: Some(model.to_string()),
                duration_secs: None,
            })
            .collect();

        Self {
            run_id: run_id.to_string(),
            directive: directive.to_string(),
            repo_path: repo_path.to_string_lossy().to_string(),
            workspace_path: workspace_path.to_string_lossy().to_string(),
            children,
            plan: serde_json::to_value(plan).unwrap_or_default(),
            started_at: Some(Instant::now()),
        }
    }
}
