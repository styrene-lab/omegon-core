//! Footer zone — summary cards rendered directly from system state.
//!
//! Four cards matching the TS dashboard HUD:
//!   context — token usage gauge, model, turn count
//!   models  — driver model info
//!   memory  — fact count, injection stats
//!   system  — cwd, session
//!
//! Unlike the TS version, this reads directly from ConversationState,
//! MemoryProvider, and LifecycleContext — no shared-state indirection.

use ratatui::prelude::*;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use super::theme::Theme;

/// Data collected from the agent systems for footer rendering.
/// Populated by the App on each frame from the live system state.
#[derive(Default)]
pub struct FooterData {
    // Context card
    pub context_percent: f32,
    pub context_window: usize,
    pub estimated_tokens: usize,
    pub turn: u32,

    // Model card
    pub model_id: String,
    pub model_provider: String,
    pub is_oauth: bool,

    // Memory card
    pub total_facts: usize,
    pub injected_facts: usize,
    pub working_memory: usize,
    pub memory_tokens_est: usize,

    // System card
    pub cwd: String,
    pub compactions: u32,
    pub tool_calls: u32,
}

impl FooterData {
    /// Render the footer zone as a horizontal strip of summary cards.
    pub fn render(&self, area: Rect, frame: &mut Frame, t: &dyn Theme) {
        let width = area.width as usize;

        if width < 60 {
            // Narrow: single-line minimal footer
            self.render_narrow(area, frame, t);
            return;
        }

        // Split into 4 columns
        let col_w = width / 4;
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Ratio(1, 4),
                Constraint::Ratio(1, 4),
                Constraint::Ratio(1, 4),
                Constraint::Ratio(1, 4),
            ])
            .split(area);

        self.render_context_card(cols[0], frame, t);
        self.render_model_card(cols[1], frame, t);
        self.render_memory_card(cols[2], frame, t);
        self.render_system_card(cols[3], frame, t);
    }

    fn render_narrow(&self, area: Rect, frame: &mut Frame, t: &dyn Theme) {
        let model_short = self.model_id.split(':').next_back()
            .or_else(|| self.model_id.split('/').next_back())
            .unwrap_or(&self.model_id);
        let pct = self.context_percent as u32;
        let line = Line::from(vec![
            Span::styled(" Ω ", t.style_accent_bold()),
            Span::styled(format!("{model_short} "), Style::default().fg(t.muted())),
            Span::styled("│ ", Style::default().fg(t.dim())),
            Span::styled(format!("{pct}% "), Style::default().fg(
                if pct > 70 { t.error() } else if pct > 45 { t.warning() } else { t.muted() }
            )),
            Span::styled("│ ", Style::default().fg(t.dim())),
            Span::styled(format!("T·{} ", self.turn), Style::default().fg(t.muted())),
        ]);
        let widget = Paragraph::new(line).style(Style::default().bg(t.card_bg()));
        frame.render_widget(widget, area);
    }

    fn render_context_card(&self, area: Rect, frame: &mut Frame, t: &dyn Theme) {
        let inner_w = area.width.saturating_sub(4) as usize;
        let mut lines: Vec<Line<'static>> = Vec::new();

        // Section divider
        lines.push(card_header("context", inner_w, t));

        // Gauge bar
        let bar_w = inner_w.min(20);
        let pct = self.context_percent.min(100.0);
        let filled = ((pct / 100.0) * bar_w as f32) as usize;
        let empty = bar_w.saturating_sub(filled);

        let bar_color = if pct > 70.0 { t.error() } else if pct > 45.0 { t.warning() } else { t.accent_muted() };
        let mut bar_spans = vec![
            Span::styled("  ", Style::default()),
            Span::styled("▐", Style::default().fg(t.dim())),
        ];
        if filled > 0 {
            bar_spans.push(Span::styled("█".repeat(filled), Style::default().fg(bar_color)));
        }
        if empty > 0 {
            bar_spans.push(Span::styled("░".repeat(empty), Style::default().fg(t.border())));
        }
        bar_spans.push(Span::styled("▌ ", Style::default().fg(t.dim())));

        let pct_str = format!("{}%", pct as u32);
        bar_spans.push(Span::styled(pct_str, Style::default().fg(
            if pct > 70.0 { t.error() } else if pct > 45.0 { t.warning() } else { t.muted() }
        )));

        if self.context_window > 0 {
            bar_spans.push(Span::styled(
                format!(" / {}", format_tokens(self.context_window)),
                Style::default().fg(t.dim()),
            ));
        }
        if self.turn > 0 {
            bar_spans.push(Span::styled(format!("  T·{}", self.turn), Style::default().fg(t.dim())));
        }
        lines.push(Line::from(bar_spans));

        // Model line
        let model_short = self.model_id.split(':').next_back()
            .or_else(|| self.model_id.split('/').next_back())
            .unwrap_or(&self.model_id);
        lines.push(Line::from(vec![
            Span::styled("  ▸ ", Style::default().fg(t.accent())),
            Span::styled(
                format!("{}/{}", self.model_provider, model_short),
                Style::default().fg(t.muted()),
            ),
        ]));

        let block = Block::default().borders(Borders::NONE);
        let widget = Paragraph::new(lines).block(block);
        frame.render_widget(widget, area);
    }

    fn render_model_card(&self, area: Rect, frame: &mut Frame, t: &dyn Theme) {
        let inner_w = area.width.saturating_sub(4) as usize;
        let mut lines: Vec<Line<'static>> = Vec::new();

        lines.push(card_header("models", inner_w, t));

        // Driver model
        let model_short = self.model_id.split(':').next_back()
            .or_else(|| self.model_id.split('/').next_back())
            .unwrap_or(&self.model_id);
        let auth_type = if self.is_oauth { "subscription" } else { "api-key" };
        lines.push(Line::from(vec![
            Span::styled("  Driver ", Style::default().fg(t.fg()).add_modifier(Modifier::BOLD)),
            Span::styled(format!("{model_short}"), Style::default().fg(t.muted())),
            Span::styled(" · ", Style::default().fg(t.dim())),
            Span::styled("native", Style::default().fg(t.success())),
        ]));

        // Auth info
        lines.push(Line::from(vec![
            Span::styled("  Auth ", Style::default().fg(t.muted())),
            Span::styled(auth_type, Style::default().fg(t.muted())),
        ]));

        let widget = Paragraph::new(lines);
        frame.render_widget(widget, area);
    }

    fn render_memory_card(&self, area: Rect, frame: &mut Frame, t: &dyn Theme) {
        let inner_w = area.width.saturating_sub(4) as usize;
        let mut lines: Vec<Line<'static>> = Vec::new();

        lines.push(card_header("memory", inner_w, t));

        let sep = Span::styled(" · ", Style::default().fg(t.dim()));
        let mut parts: Vec<Span<'static>> = vec![
            Span::styled("  ", Style::default()),
            Span::styled("⌗ ", Style::default().fg(t.accent())),
            Span::styled(format!("{}", self.total_facts), Style::default().fg(t.muted())),
        ];

        if self.injected_facts > 0 {
            parts.push(sep.clone());
            parts.push(Span::styled("inj ", Style::default().fg(t.dim())));
            parts.push(Span::styled(format!("{}", self.injected_facts), Style::default().fg(t.muted())));
        }

        if self.working_memory > 0 {
            parts.push(sep.clone());
            parts.push(Span::styled("wm ", Style::default().fg(t.dim())));
            parts.push(Span::styled(format!("{}", self.working_memory), Style::default().fg(t.muted())));
        }

        if self.memory_tokens_est > 0 {
            parts.push(sep);
            parts.push(Span::styled(
                format!("~{}", format_tokens(self.memory_tokens_est)),
                Style::default().fg(t.dim()),
            ));
        }

        lines.push(Line::from(parts));

        let widget = Paragraph::new(lines);
        frame.render_widget(widget, area);
    }

    fn render_system_card(&self, area: Rect, frame: &mut Frame, t: &dyn Theme) {
        let inner_w = area.width.saturating_sub(4) as usize;
        let mut lines: Vec<Line<'static>> = Vec::new();

        lines.push(card_header("system", inner_w, t));

        // cwd — shorten home dir
        let home = dirs::home_dir().map(|h| h.to_string_lossy().to_string()).unwrap_or_default();
        let display_cwd = if !home.is_empty() && self.cwd.starts_with(&home) {
            format!("~{}", &self.cwd[home.len()..])
        } else {
            self.cwd.clone()
        };
        lines.push(Line::from(vec![
            Span::styled("  ⌂ ", Style::default().fg(t.dim())),
            Span::styled(display_cwd, Style::default().fg(t.muted())),
        ]));

        let widget = Paragraph::new(lines);
        frame.render_widget(widget, area);
    }
}

/// Render a card header: `── label ──────────`
fn card_header<'a>(label: &str, width: usize, t: &dyn Theme) -> Line<'a> {
    let prefix = "── ";
    let suffix_len = width.saturating_sub(prefix.len() + label.len() + 2);
    Line::from(vec![
        Span::styled(prefix, Style::default().fg(t.dim())),
        Span::styled(label.to_string(), Style::default().fg(t.muted())),
        Span::styled(format!(" {}", "─".repeat(suffix_len)), Style::default().fg(t.dim())),
    ])
}

fn format_tokens(count: usize) -> String {
    if count < 1000 {
        count.to_string()
    } else if count < 10_000 {
        format!("{:.1}k", count as f64 / 1000.0)
    } else if count < 1_000_000 {
        format!("{}k", count / 1000)
    } else {
        format!("{:.1}M", count as f64 / 1_000_000.0)
    }
}
