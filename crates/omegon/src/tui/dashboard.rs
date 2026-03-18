//! Dashboard panel — design-tree + openspec state display.
//!
//! Rendered as a right-side panel when terminal width >= 100 columns.
//! Shows: focused design node, active openspec changes, session stats.

use ratatui::prelude::*;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::lifecycle::types::*;

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
        let block = Block::default()
            .borders(Borders::LEFT)
            .border_style(Style::default().fg(Color::DarkGray))
            .title(Span::styled(" Ω Dashboard ", Style::default().fg(Color::Cyan)));

        let mut lines: Vec<Line<'static>> = Vec::new();

        // ─── Focused Node ───────────────────────────────────────────
        if let Some(ref node) = self.focused_node {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{} ", node.status.icon()),
                    Style::default().fg(status_color(node.status)),
                ),
                Span::styled(
                    node.id.clone(),
                    Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
                ),
            ]));
            // Title (truncated)
            let title = if node.title.len() > 35 {
                format!("{}…", &node.title[..34])
            } else {
                node.title.clone()
            };
            lines.push(Line::from(Span::styled(
                format!("  {title}"),
                Style::default().fg(Color::Gray),
            )));
            if node.decisions > 0 || node.open_questions > 0 {
                lines.push(Line::from(Span::styled(
                    format!("  {}● {}? ", node.decisions, node.open_questions),
                    Style::default().fg(Color::DarkGray),
                )));
            }
            lines.push(Line::from(""));
        }

        // ─── Active Changes ─────────────────────────────────────────
        if !self.active_changes.is_empty() {
            lines.push(Line::from(Span::styled(
                "OpenSpec",
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            )));
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
                    ChangeStage::Implementing => Color::Yellow,
                    ChangeStage::Verifying => Color::Green,
                    _ => Color::DarkGray,
                };
                lines.push(Line::from(vec![
                    Span::styled(format!("  {icon} "), Style::default().fg(color)),
                    Span::styled(change.name.clone(), Style::default().fg(color)),
                    Span::styled(progress, Style::default().fg(Color::DarkGray)),
                ]));
            }
            lines.push(Line::from(""));
        }

        // ─── Session Stats ──────────────────────────────────────────
        lines.push(Line::from(Span::styled(
            "Session",
            Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            format!("  {} turns, {} tools", self.turns, self.tool_calls),
            Style::default().fg(Color::DarkGray),
        )));
        if self.compactions > 0 {
            lines.push(Line::from(Span::styled(
                format!("  {} compactions", self.compactions),
                Style::default().fg(Color::DarkGray),
            )));
        }

        let widget = Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: true });
        frame.render_widget(widget, area);
    }
}

fn status_color(status: NodeStatus) -> Color {
    match status {
        NodeStatus::Seed => Color::DarkGray,
        NodeStatus::Exploring => Color::Cyan,
        NodeStatus::Resolved | NodeStatus::Decided | NodeStatus::Implemented => Color::Green,
        NodeStatus::Implementing => Color::Yellow,
        NodeStatus::Blocked => Color::Red,
        NodeStatus::Deferred => Color::Yellow,
    }
}
