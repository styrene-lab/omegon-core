//! Dashboard panel — design-tree + openspec state display.
//!
//! Rendered as a right-side panel when terminal width >= 100 columns.
//! Shows: focused design node, active openspec changes, session stats.

use ratatui::prelude::*;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::lifecycle::types::*;
use super::theme::Theme;

/// Dashboard state — updated from lifecycle scanning.
#[derive(Default)]
pub struct DashboardState {
    /// Focused design node summary.
    pub focused_node: Option<FocusedNodeSummary>,
    /// Active openspec changes.
    pub active_changes: Vec<ChangeSummary>,
    /// Session stats.
    pub turns: u32,
    pub tool_calls: u32,
    pub compactions: u32,
}

pub struct FocusedNodeSummary {
    pub id: String,
    pub title: String,
    pub status: NodeStatus,
    pub open_questions: usize,
    pub decisions: usize,
}

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

        let mut lines: Vec<Line<'static>> = Vec::new();

        // ─── Focused Node ───────────────────────────────────────────
        if let Some(ref node) = self.focused_node {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{} ", node.status.icon()),
                    Style::default().fg(status_color_themed(node.status, t)),
                ),
                Span::styled(node.id.clone(), t.style_heading()),
            ]));
            let title = if node.title.len() > 35 {
                format!("{}…", &node.title[..34])
            } else {
                node.title.clone()
            };
            lines.push(Line::from(Span::styled(format!("  {title}"), t.style_muted())));
            if node.decisions > 0 || node.open_questions > 0 {
                lines.push(Line::from(Span::styled(
                    format!("  {}● {}? ", node.decisions, node.open_questions),
                    t.style_dim(),
                )));
            }
            lines.push(Line::from(""));
        }

        // ─── Active Changes ─────────────────────────────────────────
        if !self.active_changes.is_empty() {
            lines.push(Line::from(Span::styled("OpenSpec", t.style_heading())));
            for change in &self.active_changes {
                let icon = match change.stage {
                    ChangeStage::Proposed => "◌",
                    ChangeStage::Specified => "◐",
                    ChangeStage::Planned => "▸",
                    ChangeStage::Implementing => "⟳",
                    ChangeStage::Verifying => "◉",
                    ChangeStage::Archived => "✓",
                };
                let progress = if change.total_tasks > 0 {
                    format!(" {}/{}", change.done_tasks, change.total_tasks)
                } else {
                    String::new()
                };
                let color = match change.stage {
                    ChangeStage::Implementing => t.warning(),
                    ChangeStage::Verifying => t.success(),
                    _ => t.dim(),
                };
                lines.push(Line::from(vec![
                    Span::styled(format!("  {icon} "), Style::default().fg(color)),
                    Span::styled(change.name.clone(), Style::default().fg(color)),
                    Span::styled(progress, t.style_dim()),
                ]));
            }
            lines.push(Line::from(""));
        }

        // ─── Session Stats ──────────────────────────────────────────
        lines.push(Line::from(Span::styled("Session", t.style_heading())));
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

fn status_color_themed(status: NodeStatus, t: &dyn Theme) -> Color {
    match status {
        NodeStatus::Seed => t.dim(),
        NodeStatus::Exploring => t.accent(),
        NodeStatus::Resolved | NodeStatus::Decided | NodeStatus::Implemented => t.success(),
        NodeStatus::Implementing => t.warning(),
        NodeStatus::Blocked => t.error(),
        NodeStatus::Deferred => t.caution(),
    }
}
