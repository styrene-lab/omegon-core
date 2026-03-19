//! Segment types and per-type rendering for the conversation widget.
//!
//! Each segment renders as an independent widget with its own Block,
//! background, borders, and internal layout. The ConversationWidget
//! composes these into a scrollable view.

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, BorderType, Padding, Paragraph, Wrap};
use tui_syntax_highlight::Highlighter as SyntaxHighlighter;

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
    /// Uses line counting rather than render-and-measure — simpler and
    /// avoids temp buffer allocation.
    pub fn height(&self, width: u16, _t: &dyn Theme) -> u16 {
        if width == 0 { return 1; }
        let w = width as usize;

        match self {
            Self::TurnSeparator => 1,
            Self::LifecycleEvent { .. } => 1,

            Self::UserPrompt { text } => {
                // ▸ prefix + text, wrapped, + 1 blank line after
                let text_lines = wrapped_line_count(text, w.saturating_sub(4));
                (text_lines as u16).max(1) + 1
            }

            Self::AssistantText { text, thinking, .. } => {
                let mut h: u16 = 0;
                if !thinking.is_empty() {
                    h += 1; // "◌ thinking…" header
                    h += thinking.lines().count().min(20) as u16;
                    h += 1; // gap
                }
                // Each text line may wrap
                for line in text.lines() {
                    h += wrapped_line_count(line, w.saturating_sub(2)) as u16;
                }
                if text.is_empty() && thinking.is_empty() {
                    h += 1; // streaming cursor "…"
                }
                h.max(1) + 1 // +1 spacing after
            }

            Self::ToolCard { name, detail_args, detail_result, .. } => {
                let mut h: u16 = 2; // top border + bottom border
                let inner_w = w.saturating_sub(6); // borders + padding

                // Args
                if let Some(args) = detail_args {
                    let lines = if *name == "bash" {
                        args.lines().count().min(4)
                    } else {
                        1 // single line for path-based tools
                    };
                    h += lines as u16;
                }
                // Separator
                if detail_args.is_some() && detail_result.is_some() {
                    h += 1;
                }
                // Result (capped at 12 + truncation notice)
                if let Some(result) = detail_result {
                    let total = result.lines().count();
                    let show = total.min(12);
                    // Account for wrapping in result lines
                    for line in result.lines().take(show) {
                        h += wrapped_line_count(line, inner_w) as u16;
                    }
                    if total > 12 { h += 1; } // "… N lines" notice
                }
                h.max(3) + 1 // +1 spacing after card
            }

            Self::SystemNotification { text } => {
                let lines = text.lines().count();
                (lines as u16).max(1) + 1
            }
        }
    }
}

/// How many terminal rows a line of text occupies when wrapped at `width`.
fn wrapped_line_count(text: &str, width: usize) -> usize {
    if width == 0 || text.is_empty() { return 1; }
    let len = text.chars().count();
    len.div_ceil(width).max(1)
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

    // Use the status color for the border — makes the card visually pop
    let border_color = if !complete {
        t.warning()
    } else if is_error {
        t.error()
    } else {
        t.border()
    };

    let card_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color).bg(t.card_bg()))
        .title(title)
        .padding(Padding::horizontal(1))
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

        let result_lines: Vec<&str> = result.lines().collect();
        let show = result_lines.len().min(12);
        let display_text = result_lines[..show].join("\n");

        // Try syntax highlighting based on file extension from args
        let highlighted = if !is_error {
            try_highlight(&display_text, detail_args, name, t)
        } else {
            None
        };

        if let Some(highlighted_lines) = highlighted {
            for line in highlighted_lines {
                // Apply surface_bg to each span
                let spans: Vec<Span<'_>> = line.spans.into_iter().map(|mut s| {
                    s.style = s.style.bg(t.surface_bg());
                    s
                }).collect();
                lines.push(Line::from(spans));
            }
        } else {
            let result_style = if is_error {
                Style::default().fg(t.error()).bg(t.surface_bg())
            } else {
                Style::default().fg(t.muted()).bg(t.surface_bg())
            };
            for line in &result_lines[..show] {
                lines.push(Line::from(Span::styled(
                    line.to_string(), result_style,
                )));
            }
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

/// Attempt syntax highlighting for tool result text.
/// Returns None if no syntax can be detected.
fn try_highlight<'a>(
    text: &str,
    detail_args: Option<&str>,
    tool_name: &str,
    _t: &dyn Theme,
) -> Option<Vec<Line<'a>>> {
    // Determine syntax from file extension or tool type
    let syntax_name = if tool_name == "read" || tool_name == "edit" || tool_name == "write" {
        // detail_args is the file path — extract extension
        detail_args.and_then(|path| {
            let ext = path.rsplit('.').next()?;
            match ext {
                "rs" => Some("Rust"),
                "ts" | "tsx" => Some("TypeScript"),
                "js" | "jsx" => Some("JavaScript"),
                "json" => Some("JSON"),
                "toml" => Some("TOML"),
                "yaml" | "yml" => Some("YAML"),
                "py" => Some("Python"),
                "go" => Some("Go"),
                "sh" | "bash" | "zsh" => Some("Bourne Again Shell (bash)"),
                "md" | "markdown" => Some("Markdown"),
                "html" | "htm" => Some("HTML"),
                "css" => Some("CSS"),
                "sql" => Some("SQL"),
                "xml" => Some("XML"),
                "c" | "h" => Some("C"),
                "cpp" | "cc" | "cxx" | "hpp" => Some("C++"),
                "java" => Some("Java"),
                "rb" => Some("Ruby"),
                "swift" => Some("Swift"),
                "kt" | "kts" => Some("Kotlin"),
                "dockerfile" | "Dockerfile" => Some("Dockerfile"),
                _ => None,
            }
        })
    } else if tool_name == "bash" {
        Some("Bourne Again Shell (bash)")
    } else {
        None
    }?;

    let ss = syntect::parsing::SyntaxSet::load_defaults_newlines();
    let ts = syntect::highlighting::ThemeSet::load_defaults();
    let theme = ts.themes.get("base16-ocean.dark")?;
    let syntax = ss.find_syntax_by_name(syntax_name)?;
    let highlighter = SyntaxHighlighter::new(theme.clone());
    let text_lines: Vec<&str> = text.lines().collect();
    let highlighted = highlighter.highlight_lines(text_lines, syntax, &ss).ok()?;
    Some(highlighted.lines.into_iter().map(|line| {
        Line::from(line.spans.into_iter().map(|span| {
            Span::styled(span.content.to_string(), span.style)
        }).collect::<Vec<_>>())
    }).collect())
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
