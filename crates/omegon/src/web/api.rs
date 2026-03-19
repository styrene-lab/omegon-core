//! JSON API endpoints for the web dashboard.
//!
//! GET /api/state — full agent state snapshot.
//! Designed to be the canonical state shape that any web UI consumes.

use axum::extract::State;
use axum::Json;
use serde::Serialize;

use super::WebState;
use crate::lifecycle::types::*;

/// Full agent state snapshot — the canonical shape for web consumers.
#[derive(Serialize)]
pub struct StateSnapshot {
    pub design: DesignSnapshot,
    pub openspec: OpenSpecSnapshot,
    pub cleave: CleaveSnapshot,
    pub session: SessionSnapshot,
}

#[derive(Serialize)]
pub struct DesignSnapshot {
    pub counts: DesignCounts,
    pub focused: Option<FocusedNode>,
    pub implementing: Vec<NodeBrief>,
    pub actionable: Vec<NodeBrief>,
    pub all_nodes: Vec<NodeBrief>,
}

#[derive(Serialize)]
pub struct DesignCounts {
    pub total: usize,
    pub seed: usize,
    pub exploring: usize,
    pub resolved: usize,
    pub decided: usize,
    pub implementing: usize,
    pub implemented: usize,
    pub blocked: usize,
    pub deferred: usize,
    pub open_questions: usize,
}

#[derive(Serialize)]
pub struct FocusedNode {
    pub id: String,
    pub title: String,
    pub status: String,
    pub open_questions: Vec<String>,
    pub decisions: usize,
    pub children: usize,
}

#[derive(Clone, Serialize)]
pub struct NodeBrief {
    pub id: String,
    pub title: String,
    pub status: String,
    pub parent: Option<String>,
    pub open_questions: usize,
    pub openspec_change: Option<String>,
    pub dependencies: Vec<String>,
}

#[derive(Serialize)]
pub struct OpenSpecSnapshot {
    pub changes: Vec<ChangeSnapshot>,
    pub total_tasks: usize,
    pub done_tasks: usize,
}

#[derive(Serialize)]
pub struct ChangeSnapshot {
    pub name: String,
    pub stage: String,
    pub has_specs: bool,
    pub has_tasks: bool,
    pub total_tasks: usize,
    pub done_tasks: usize,
}

#[derive(Serialize)]
pub struct CleaveSnapshot {
    pub active: bool,
    pub total_children: usize,
    pub completed: usize,
    pub failed: usize,
    pub children: Vec<ChildSnapshot>,
}

#[derive(Serialize)]
pub struct ChildSnapshot {
    pub label: String,
    pub status: String,
    pub duration_secs: Option<f64>,
}

#[derive(Serialize)]
pub struct SessionSnapshot {
    pub turns: u32,
    pub tool_calls: u32,
    pub compactions: u32,
}

/// GET /api/state — build a full snapshot from the shared handles.
pub async fn get_state(State(state): State<WebState>) -> Json<StateSnapshot> {
    let snapshot = build_snapshot(&state);
    Json(snapshot)
}

/// Build a StateSnapshot from the shared handles.
/// Also used by the WebSocket handler for initial snapshots.
pub fn build_snapshot(state: &WebState) -> StateSnapshot {
    let mut design = DesignSnapshot {
        counts: DesignCounts {
            total: 0, seed: 0, exploring: 0, resolved: 0, decided: 0,
            implementing: 0, implemented: 0, blocked: 0, deferred: 0,
            open_questions: 0,
        },
        focused: None,
        implementing: Vec::new(),
        actionable: Vec::new(),
        all_nodes: Vec::new(),
    };

    let mut openspec = OpenSpecSnapshot {
        changes: Vec::new(),
        total_tasks: 0,
        done_tasks: 0,
    };

    // Read lifecycle state
    if let Some(ref lp_lock) = state.handles.lifecycle
        && let Ok(lp) = lp_lock.lock() {
            let nodes = lp.all_nodes();
            design.counts.total = nodes.len();

            for node in nodes.values() {
                match node.status {
                    NodeStatus::Seed => design.counts.seed += 1,
                    NodeStatus::Exploring => design.counts.exploring += 1,
                    NodeStatus::Resolved => design.counts.resolved += 1,
                    NodeStatus::Decided => design.counts.decided += 1,
                    NodeStatus::Implementing => design.counts.implementing += 1,
                    NodeStatus::Implemented => design.counts.implemented += 1,
                    NodeStatus::Blocked => design.counts.blocked += 1,
                    NodeStatus::Deferred => design.counts.deferred += 1,
                }
                design.counts.open_questions += node.open_questions.len();

                let brief = NodeBrief {
                    id: node.id.clone(),
                    title: node.title.clone(),
                    status: node.status.as_str().to_string(),
                    parent: node.parent.clone(),
                    open_questions: node.open_questions.len(),
                    openspec_change: node.openspec_change.clone(),
                    dependencies: node.dependencies.clone(),
                };

                if matches!(node.status, NodeStatus::Implementing) {
                    design.implementing.push(brief.clone());
                }
                if matches!(node.status, NodeStatus::Seed | NodeStatus::Exploring)
                    && !node.open_questions.is_empty()
                {
                    design.actionable.push(brief.clone());
                }
                design.all_nodes.push(brief);
            }

            // Focused node
            if let Some(id) = lp.focused_node_id()
                && let Some(node) = lp.get_node(id) {
                    let sections = crate::lifecycle::design::read_node_sections(node);
                    let children = crate::lifecycle::design::get_children(nodes, id);
                    design.focused = Some(FocusedNode {
                        id: node.id.clone(),
                        title: node.title.clone(),
                        status: node.status.as_str().to_string(),
                        open_questions: node.open_questions.clone(),
                        decisions: sections.map(|s| s.decisions.len()).unwrap_or(0),
                        children: children.len(),
                    });
            }

            // OpenSpec changes
            for change in lp.changes() {
                if matches!(change.stage, ChangeStage::Archived) { continue; }
                openspec.total_tasks += change.total_tasks;
                openspec.done_tasks += change.done_tasks;
                openspec.changes.push(ChangeSnapshot {
                    name: change.name.clone(),
                    stage: change.stage.as_str().to_string(),
                    has_specs: change.has_specs,
                    has_tasks: change.has_tasks,
                    total_tasks: change.total_tasks,
                    done_tasks: change.done_tasks,
                });
            }
    }

    // Read cleave state
    let cleave = if let Some(ref cp_lock) = state.handles.cleave {
        if let Ok(cp) = cp_lock.lock() {
            CleaveSnapshot {
                active: cp.active,
                total_children: cp.total_children,
                completed: cp.completed,
                failed: cp.failed,
                children: cp.children.iter().map(|c| ChildSnapshot {
                    label: c.label.clone(),
                    status: c.status.clone(),
                    duration_secs: c.duration_secs,
                }).collect(),
            }
        } else {
            CleaveSnapshot { active: false, total_children: 0, completed: 0, failed: 0, children: Vec::new() }
        }
    } else {
        CleaveSnapshot { active: false, total_children: 0, completed: 0, failed: 0, children: Vec::new() }
    };

    StateSnapshot {
        design,
        openspec,
        cleave,
        session: SessionSnapshot {
            turns: 0, // TODO: wire from shared state
            tool_calls: 0,
            compactions: 0,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::dashboard::DashboardHandles;

    #[test]
    fn empty_snapshot() {
        let state = WebState {
            handles: DashboardHandles::default(),
            events_tx: tokio::sync::broadcast::channel(16).0,
            command_tx: tokio::sync::mpsc::channel(16).0,
        };
        let snap = build_snapshot(&state);
        assert_eq!(snap.design.counts.total, 0);
        assert!(snap.design.focused.is_none());
        assert!(snap.openspec.changes.is_empty());
        assert!(!snap.cleave.active);
    }
}
