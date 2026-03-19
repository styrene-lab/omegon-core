//! Segment types and per-type rendering for the conversation widget.
//!
//! Each segment renders as an independent widget with its own Block,
//! background, borders, and internal layout. The ConversationWidget
//! composes these into a scrollable view.

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, BorderType, Padding, Paragraph, Wrap};

use super::theme::Theme;

// ═══════════════════════════════════════════════════════════════════════════
// Segment enum — the typed conversation model
// ═══════════════════════════════════════════════════════════════════════════

/// A segment in the conversation — each renders as its own widget.
#[derive(Debug, Clone)]
pub enum Segment {
    /// User's input prompt.
    UserPrompt { text: String },

    /// Assistant's response (may be streaming).
    AssistantText {
        text: String,
        thinking: String,
        complete: bool,
    },

    /// Tool call with args and result.
    ToolCard {
        id: String,
        name: String,
        args_summary: Option<String>,
        detail_args: Option<String>,
        result_summary: Option<String>,
        detail_result: Option<String>,
        is_error: bool,
        complete: bool,
    },

    /// System notification (slash command response, info message).
    SystemNotification { text: String },

    /// Lifecycle event (phase change, decomposition).
    LifecycleEvent { icon: String, text: String },

    /// Visual separator between turns.
    TurnSeparator,
}

// ═══════════════════════════════════════════════════════════════════════════
// Rendering — each segment type knows how to render into a Rect
// ═══════════════════════════════════════════════════════════════════════════

impl Segment {
    /// Render this segment into the given area of the buffer.
    pub fn render(&self, area: Rect, buf: &mut Buffer, t: &dyn Theme) {
        match self {
            Self::UserPrompt { text } => render_user_prompt(text, area, buf, t),
            Self::AssistantText { text, thinking, complete } => {
                render_assistant_text(text, thinking, *complete, area, buf, t);
            }
            Self::ToolCard {
                name, detail_args, detail_result, is_error, complete, ..
            } => {
                render_tool_card(name, detail_args.as_deref(), detail_result.as_deref(),
                    *is_error, *complete, area, buf, t);
            }
            Self::SystemNotification { text } => render_system(text, area, buf, t),
            Self::LifecycleEvent { icon, text } => render_lifecycle(icon, text, area, buf, t),
            Self::TurnSeparator => render_separator(area, buf, t),
        }
    }

    /// Calculate the height this segment needs at the given width.
    /// Renders into a throw-away buffer to measure — avoids duplicating
    /// wrapping logic.
    pub fn height(&self, width: u16, t: &dyn Theme) -> u16 {
        if width == 0 { return 1; }

        // Quick paths for simple types
        match self {
            Self::TurnSeparator => return 1,
            Self::LifecycleEvent { .. } => return 1,
            _ => {}
        }

        // Render into a temp buffer to measure
        let height_guess = match self {
            Self::UserPrompt { text } => (text.len() as u16 / width).max(1) + 2,
            Self::AssistantText { text, thinking, .. } => {
                let lines = text.lines().count() + thinking.lines().count();
                (lines as u16).max(1) + 4
            }
            Self::ToolCard { detail_result, detail_args, .. } => {
                let arg_lines = detail_args.as_ref().map(|a| a.lines().count()).unwrap_or(0);
                let result_lines = detail_result.as_ref().map(|r| r.lines().count().min(12)).unwrap_or(0);
                (arg_lines + result_lines + 4) as u16 // borders + separator + padding
            }
            Self::SystemNotification { text } => (text.lines().count() as u16).max(1) + 1,
            _ => 3,
        };

        // Render into temp buffer for exact measurement
        let temp_area = Rect::new(0, 0, width, height_guess.max(20));
        let mut temp_buf = Buffer::empty(temp_area);
        self.render(temp_area, &mut temp_buf, t);

        // Find the last non-empty row
        let mut last_row = 0u16;
        for y in 0..temp_area.height {
            for x in 0..temp_area.width {
                let cell = &temp_buf[(x, y)];
                if cell.symbol() != " " || cell.bg != Color::Reset {
                    last_row = y;
                    break;
                }
            }
        }

        (last_row + 2).min(height_guess) // +1 for the row itself, +1 for spacing
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Per-type renderers
// ═══════════════════════════════════════════════════════════════════════════

fn render_user_prompt(text: &str, area: Rect, buf: &mut Buffer, t: &dyn Theme) {
    let block = Block::default()
        .padding(Padding::horizontal(1));
    let inner = block.inner(area);
    block.render(area, buf);

    let content = Line::from(vec![
        Span::styled("▸ ", t.style_accent_bold()),
        Span::styled(text.to_string(), t.style_user_input()),
    ]);
    Paragraph::new(content)
        .wrap(Wrap { trim: false })
        .render(inner, buf);
}

fn render_assistant_text(
    text: &str, thinking: &str, complete: bool,
    area: Rect, buf: &mut Buffer, t: &dyn Theme,
) {
    let block = Block::default()
        .padding(Padding::horizontal(1));
    let inner = block.inner(area);
    block.render(area, buf);

    let mut lines: Vec<Line<'_>> = Vec::new();

    // Thinking block
    if !thinking.is_empty() {
        lines.push(Line::from(Span::styled(
            "◌ thinking…",
            Style::default().fg(t.dim()).add_modifier(Modifier::ITALIC),
        )));
        for line in thinking.lines().take(20) {
            lines.push(Line::from(Span::styled(
                line.to_string(), t.style_dim(),
            )));
        }
        lines.push(Line::from(""));
    }

    // Assistant text with markdown structural highlighting
    let mut in_code_fence = false;
    for line in text.lines() {
        if line.starts_with("```") {
            in_code_fence = !in_code_fence;
            lines.push(Line::from(Span::styled(
                line.to_string(),
                Style::default().fg(t.dim()).bg(t.surface_bg()),
            )));
        } else if in_code_fence {
            lines.push(Line::from(Span::styled(
                line.to_string(),
                Style::default().fg(t.accent_muted()).bg(t.surface_bg()),
            )));
        } else {
            lines.push(super::widgets::highlight_line(line, t));
        }
    }

    // Streaming cursor
    if !complete && text.is_empty() && thinking.is_empty() {
        lines.push(Line::from(Span::styled("…", t.style_dim())));
    }

    Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .render(inner, buf);
}

fn render_tool_card(
    name: &str, detail_args: Option<&str>, detail_result: Option<&str>,
    is_error: bool, complete: bool,
    area: Rect, buf: &mut Buffer, t: &dyn Theme,
) {
    let (icon, status_color) = if complete {
        if is_error { ("✗", t.error()) } else { ("✓", t.success()) }
    } else {
        ("⟳", t.warning())
    };

    // ── Card block with rounded borders ─────────────────────────
    let title = Line::from(vec![
        Span::styled(format!(" {icon} "), Style::default().fg(status_color)),
        Span::styled(
            format!("{name} "),
            Style::default().fg(status_color).add_modifier(Modifier::BOLD),
        ),
    ]);

    let card_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(t.border_dim()).bg(t.card_bg()))
        .title(title)
        .style(Style::default().bg(t.card_bg()));

    let card_inner = card_block.inner(area);
    card_block.render(area, buf);

    if card_inner.height == 0 || card_inner.width == 0 {
        return;
    }

    let mut lines: Vec<Line<'_>> = Vec::new();

    // ── Args section ────────────────────────────────────────────
    if let Some(args) = detail_args {
        match name {
            "bash" => {
                for (i, line) in args.lines().take(4).enumerate() {
                    let prefix = if i == 0 { "$ " } else { "  " };
                    lines.push(Line::from(vec![
                        Span::styled(prefix, Style::default().fg(t.dim()).bg(t.card_bg())),
                        Span::styled(line.to_string(), Style::default().fg(t.fg()).bg(t.card_bg())),
                    ]));
                }
            }
            "edit" => {
                lines.push(Line::from(vec![
                    Span::styled("▸ edit ", Style::default().fg(t.accent_muted()).bg(t.card_bg())),
                    Span::styled(args.to_string(), Style::default().fg(t.dim()).bg(t.card_bg())),
                ]));
            }
            _ => {
                lines.push(Line::from(Span::styled(
                    args.to_string(),
                    Style::default().fg(t.dim()).bg(t.card_bg()),
                )));
            }
        }
    }

    // ── Result section with distinct background ─────────────────
    if let Some(result) = detail_result {
        if !lines.is_empty() {
            // Separator line
            lines.push(Line::from(Span::styled(
                "─".repeat(card_inner.width as usize),
                Style::default().fg(t.border_dim()).bg(t.surface_bg()),
            )));
        }

        let result_style = if is_error {
            Style::default().fg(t.error()).bg(t.surface_bg())
        } else {
            Style::default().fg(t.muted()).bg(t.surface_bg())
        };

        let result_lines: Vec<&str> = result.lines().collect();
        let show = result_lines.len().min(12);
        for line in &result_lines[..show] {
            lines.push(Line::from(Span::styled(
                line.to_string(), result_style,
            )));
        }
        if result_lines.len() > show {
            lines.push(Line::from(Span::styled(
                format!("  … {} lines total", result_lines.len()),
                Style::default().fg(t.dim()).bg(t.surface_bg()),
            )));
        }
    }

    Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .render(card_inner, buf);
}

fn render_system(text: &str, area: Rect, buf: &mut Buffer, t: &dyn Theme) {
    let block = Block::default()
        .padding(Padding::horizontal(1));
    let inner = block.inner(area);
    block.render(area, buf);

    let mut lines: Vec<Line<'_>> = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let style = if i == 0 && line.starts_with('Ω') {
            t.style_accent_bold()
        } else if line.starts_with("  ▸") || line.starts_with("  /") || line.starts_with("  Ctrl") {
            Style::default().fg(t.muted())
        } else {
            Style::default().fg(t.accent_muted())
        };
        lines.push(Line::from(Span::styled(line.to_string(), style)));
    }

    Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .render(inner, buf);
}

fn render_lifecycle(icon: &str, text: &str, area: Rect, buf: &mut Buffer, t: &dyn Theme) {
    let line = Line::from(vec![
        Span::styled(" │ ", Style::default().fg(t.border_dim())),
        Span::styled(format!("{icon} "), Style::default().fg(t.accent_muted())),
        Span::styled(text.to_string(), Style::default().fg(t.muted())),
    ]);
    Paragraph::new(line).render(area, buf);
}

fn render_separator(area: Rect, buf: &mut Buffer, t: &dyn Theme) {
    if area.height == 0 { return; }
    let line = Line::from(Span::styled(
        " ".repeat(area.width as usize),
        Style::default().fg(t.border_dim()),
    ));
    Paragraph::new(line).render(area, buf);
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::theme::Alpharius;

    fn make_buf(w: u16, h: u16) -> (Rect, Buffer) {
        let area = Rect::new(0, 0, w, h);
        (area, Buffer::empty(area))
    }

    fn buf_text(buf: &Buffer, area: Rect) -> String {
        let mut text = String::new();
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                text.push_str(buf[(x, y)].symbol());
            }
            text.push('\n');
        }
        text
    }

    #[test]
    fn user_prompt_renders() {
        let seg = Segment::UserPrompt { text: "hello world".into() };
        let (area, mut buf) = make_buf(40, 5);
        seg.render(area, &mut buf, &Alpharius);
        let text = buf_text(&buf, area);
        assert!(text.contains("▸"), "should have prompt icon");
        assert!(text.contains("hello world"), "should have text");
    }

    #[test]
    fn tool_card_has_borders() {
        let seg = Segment::ToolCard {
            id: "1".into(), name: "bash".into(),
            args_summary: Some("ls -la".into()),
            detail_args: Some("ls -la".into()),
            result_summary: Some("total 42".into()),
            detail_result: Some("total 42\ndrwxr-xr-x  5 user staff".into()),
            is_error: false, complete: true,
        };
        let (area, mut buf) = make_buf(60, 10);
        seg.render(area, &mut buf, &Alpharius);
        let text = buf_text(&buf, area);
        assert!(text.contains("╭"), "should have top border: {text}");
        assert!(text.contains("╰"), "should have bottom border: {text}");
        assert!(text.contains("bash"), "should have tool name: {text}");
        assert!(text.contains("✓"), "should have checkmark: {text}");
    }

    #[test]
    fn tool_card_error_styling() {
        let seg = Segment::ToolCard {
            id: "1".into(), name: "write".into(),
            args_summary: None, detail_args: Some("/tmp/test".into()),
            result_summary: None, detail_result: Some("permission denied".into()),
            is_error: true, complete: true,
        };
        let (area, mut buf) = make_buf(60, 8);
        seg.render(area, &mut buf, &Alpharius);
        let text = buf_text(&buf, area);
        assert!(text.contains("✗"), "should have error icon: {text}");
    }

    #[test]
    fn assistant_text_with_code_fence() {
        let seg = Segment::AssistantText {
            text: "Here's code:\n```rust\nfn main() {}\n```\nDone.".into(),
            thinking: String::new(),
            complete: true,
        };
        let (area, mut buf) = make_buf(60, 10);
        seg.render(area, &mut buf, &Alpharius);
        let text = buf_text(&buf, area);
        assert!(text.contains("fn main"), "should have code: {text}");
    }

    #[test]
    fn height_calculation() {
        let t = Alpharius;
        let sep = Segment::TurnSeparator;
        assert_eq!(sep.height(80, &t), 1);

        let user = Segment::UserPrompt { text: "short".into() };
        let h = user.height(80, &t);
        assert!(h >= 2 && h <= 5, "user prompt height: {h}");

        let tool = Segment::ToolCard {
            id: "1".into(), name: "bash".into(),
            args_summary: None, detail_args: Some("echo hello".into()),
            result_summary: None, detail_result: Some("hello".into()),
            is_error: false, complete: true,
        };
        let h = tool.height(80, &t);
        assert!(h >= 4, "tool card height should be >= 4, got {h}");
    }

    #[test]
    fn system_notification_renders() {
        let seg = Segment::SystemNotification { text: "Tool display → detailed".into() };
        let (area, mut buf) = make_buf(60, 3);
        seg.render(area, &mut buf, &Alpharius);
        let text = buf_text(&buf, area);
        assert!(text.contains("detailed"), "should show text: {text}");
    }
}
