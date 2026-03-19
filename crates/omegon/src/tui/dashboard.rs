//! Dashboard panel — design-tree + openspec state display.
//!
//! Rendered as a right-side panel when terminal width >= 100 columns.
//! Shows: focused design node, active openspec changes, session stats.
//! Uses shared widget primitives from `widgets.rs`.

use ratatui::prelude::*;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::lifecycle::types::*;
use super::theme::Theme;
use super::widgets;

use crate::features::cleave::CleaveProgress;

/// Dashboard state — updated from lifecycle scanning.
#[derive(Default)]
pub struct DashboardState {
    pub focused_node: Option<FocusedNodeSummary>,
    pub active_changes: Vec<ChangeSummary>,
    pub cleave: Option<CleaveProgress>,
    pub turns: u32,
    pub tool_calls: u32,
    pub compactions: u32,
}

#[derive(Clone)]
pub struct FocusedNodeSummary {
    pub id: String,
    pub title: String,
    pub status: NodeStatus,
    pub open_questions: usize,
    pub decisions: usize,
}

#[derive(Clone)]
pub struct ChangeSummary {
    pub name: String,
    pub stage: ChangeStage,
    pub done_tasks: usize,
    pub total_tasks: usize,
}

impl DashboardState {
    pub fn render(&self, area: Rect, frame: &mut Frame) {
        self.render_themed(area, frame, &super::theme::Alpharius);
    }

    pub fn render_themed(&self, area: Rect, frame: &mut Frame, t: &dyn Theme) {
        let block = Block::default()
            .borders(Borders::LEFT)
            .border_style(t.style_border())
            .title(Span::styled(" Ω Dashboard ", t.style_accent_bold()));

        let inner_w = area.width.saturating_sub(3) as usize; // left border + padding
        let mut lines: Vec<Line<'static>> = Vec::new();

        // ─── Focused Node ───────────────────────────────────────
        if let Some(ref node) = self.focused_node {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{} ", node.status.icon()),
                    Style::default().fg(status_color(node.status, t)),
                ),
                Span::styled(node.id.clone(), t.style_heading()),
            ]));
            let title = widgets::truncate_str(&node.title, inner_w.saturating_sub(2), "…");
            lines.push(Line::from(Span::styled(format!("  {title}"), t.style_muted())));
            if node.decisions > 0 || node.open_questions > 0 {
                let mut parts: Vec<Span<'static>> = vec![Span::styled("  ", Style::default())];
                if node.decisions > 0 {
                    parts.extend(widgets::badge("●", &node.decisions.to_string(), t.success()));
                    parts.push(Span::styled(" ", Style::default()));
                }
                if node.open_questions > 0 {
                    parts.extend(widgets::badge("?", &node.open_questions.to_string(), t.warning()));
                }
                lines.push(Line::from(parts));
            }
            lines.push(Line::from(""));
        }

        // ─── Active Changes ─────────────────────────────────────
        if !self.active_changes.is_empty() {
            lines.push(widgets::section_divider("openspec", inner_w, t));
            for change in &self.active_changes {
                let (icon, color) = stage_badge(change.stage, t);
                let progress = if change.total_tasks > 0 {
                    format!(" {}/{}", change.done_tasks, change.total_tasks)
                } else {
                    String::new()
                };
                let mut spans: Vec<Span<'static>> = vec![Span::styled("  ", Style::default())];
                spans.extend(widgets::badge(icon, &change.name, color));
                if !progress.is_empty() {
                    spans.push(Span::styled(progress, Style::default().fg(t.dim())));
                }
                lines.push(Line::from(spans));
            }
            lines.push(Line::from(""));
        }

        // ─── Cleave Progress ─────────────────────────────────────
        if let Some(ref cleave) = self.cleave
            && (cleave.active || cleave.total_children > 0) {
                lines.push(widgets::section_divider("cleave", inner_w, t));
                if cleave.active {
                    let done = cleave.completed + cleave.failed;
                    lines.push(Line::from(Span::styled(
                        format!("  ⟳ {}/{} children", done, cleave.total_children),
                        Style::default().fg(t.warning()),
                    )));
                } else {
                    lines.push(Line::from(Span::styled(
                        format!("  ✓ {} ok, {} failed", cleave.completed, cleave.failed),
                        Style::default().fg(if cleave.failed > 0 { t.error() } else { t.success() }),
                    )));
                }
                for child in &cleave.children {
                    let (icon, color) = match child.status.as_str() {
                        "completed" => ("✓", t.success()),
                        "failed" => ("✗", t.error()),
                        "running" => ("⟳", t.warning()),
                        _ => ("○", t.dim()),
                    };
                    let dur = child.duration_secs.map(|d| format!(" {:.0}s", d)).unwrap_or_default();
                    lines.push(Line::from(vec![
                        Span::styled(format!("  {icon} "), Style::default().fg(color)),
                        Span::styled(
                            widgets::truncate_str(&child.label, inner_w.saturating_sub(8), "…").to_string(),
                            Style::default().fg(t.muted()),
                        ),
                        Span::styled(dur, Style::default().fg(t.dim())),
                    ]));
                }
                lines.push(Line::from(""));
        }

        // ─── Session Stats ──────────────────────────────────────
        lines.push(widgets::section_divider("session", inner_w, t));
        lines.push(Line::from(Span::styled(
            format!("  {} turns, {} tool calls", self.turns, self.tool_calls),
            t.style_muted(),
        )));
        if self.compactions > 0 {
            lines.push(Line::from(Span::styled(
                format!("  {} compactions", self.compactions),
                t.style_muted(),
            )));
        }

        let widget = Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: true });
        frame.render_widget(widget, area);
    }
}

fn status_color(status: NodeStatus, t: &dyn Theme) -> Color {
    match status {
        NodeStatus::Seed => t.dim(),
        NodeStatus::Exploring => t.accent(),
        NodeStatus::Resolved | NodeStatus::Decided | NodeStatus::Implemented => t.success(),
        NodeStatus::Implementing => t.warning(),
        NodeStatus::Blocked => t.error(),
        NodeStatus::Deferred => t.caution(),
    }
}

fn stage_badge(stage: ChangeStage, t: &dyn Theme) -> (&'static str, Color) {
    match stage {
        ChangeStage::Proposed => ("◌", t.dim()),
        ChangeStage::Specified => ("◐", t.dim()),
        ChangeStage::Planned => ("▸", t.muted()),
        ChangeStage::Implementing => ("⟳", t.warning()),
        ChangeStage::Verifying => ("◉", t.success()),
        ChangeStage::Archived => ("✓", t.success()),
    }
}
