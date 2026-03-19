//! Conversation view — scrollable message display with streaming support.
//!
//! Messages are grouped into **turns** for visual coherence:
//! - User message → turn boundary
//! - Assistant text + tool calls → single visual unit with left gutter
//! - System/lifecycle messages → ungrouped, inline
//!
//! Tool calls render as cards with args summary + result preview.
//! Assistant text gets structural markdown highlighting.
//! Code fences render with distinct background styling.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::theme::Theme;
use super::widgets;

// ─── Message model ──────────────────────────────────────────────────

/// A message in the conversation view.
#[derive(Debug, Clone)]
pub(crate) enum Message {
    User(String),
    /// System/info message (from slash commands, etc.)
    System(String),
    /// Assistant text (possibly still streaming).
    Assistant {
        text: String,
        thinking: String,
        complete: bool,
    },
    /// Tool call with args summary and result.
    Tool {
        id: String,
        name: String,
        args_summary: Option<String>,
        is_error: bool,
        complete: bool,
        result_summary: Option<String>,
        /// Full args for detailed view (e.g. complete bash command).
        detail_args: Option<String>,
        /// First lines of result for detailed view.
        detail_result: Option<String>,
    },
    /// Lifecycle event (phase change, decomposition, etc.)
    Lifecycle {
        icon: String,
        text: String,
    },
}

// ─── Conversation state ─────────────────────────────────────────────

pub struct ConversationView {
    messages: Vec<Message>,
    /// Scroll offset from the bottom (0 = at bottom, showing latest).
    scroll: u16,
    /// Whether we're currently receiving streaming text.
    streaming: bool,
    /// Tool display mode — compact (single line) or detailed (bordered cards).
    pub tool_detail: crate::settings::ToolDetail,
}

impl ConversationView {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
            scroll: 0,
            streaming: false,
            tool_detail: crate::settings::ToolDetail::Compact,
        }
    }

    // ─── Push methods ───────────────────────────────────────────

    pub fn push_user(&mut self, text: &str) {
        self.messages.push(Message::User(text.to_string()));
        self.scroll = 0;
    }

    pub fn push_system(&mut self, text: &str) {
        self.messages.push(Message::System(text.to_string()));
        self.scroll = 0;
    }

    pub fn push_lifecycle(&mut self, icon: &str, text: &str) {
        self.messages.push(Message::Lifecycle {
            icon: icon.to_string(),
            text: text.to_string(),
        });
        self.scroll = 0;
    }

    pub fn append_streaming(&mut self, delta: &str) {
        if !self.streaming {
            self.messages.push(Message::Assistant {
                text: String::new(),
                thinking: String::new(),
                complete: false,
            });
            self.streaming = true;
        }

        if let Some(Message::Assistant { text, .. }) = self.messages.last_mut() {
            text.push_str(delta);
        }
        self.scroll = 0;
    }

    pub fn append_thinking(&mut self, delta: &str) {
        if !self.streaming {
            self.messages.push(Message::Assistant {
                text: String::new(),
                thinking: String::new(),
                complete: false,
            });
            self.streaming = true;
        }

        if let Some(Message::Assistant { thinking, .. }) = self.messages.last_mut() {
            thinking.push_str(delta);
        }
    }

    pub fn push_tool_start(&mut self, id: &str, name: &str, args_summary: Option<&str>, detail_args: Option<&str>) {
        self.messages.push(Message::Tool {
            id: id.to_string(),
            name: name.to_string(),
            args_summary: args_summary.map(|s| s.to_string()),
            is_error: false,
            complete: false,
            result_summary: None,
            detail_args: detail_args.map(|s| s.to_string()),
            detail_result: None,
        });
        self.scroll = 0;
    }

    pub fn push_tool_end(&mut self, id: &str, is_error: bool, result_text: Option<&str>) {
        for msg in self.messages.iter_mut().rev() {
            if let Message::Tool {
                id: tool_id,
                complete: c,
                is_error: e,
                result_summary: r,
                detail_result: dr,
                ..
            } = msg
                && tool_id == id && !*c
            {
                *c = true;
                *e = is_error;
                // Compact summary: first meaningful line
                *r = result_text.and_then(|text| {
                    let line = text.lines()
                        .find(|l| {
                            let t = l.trim();
                            !t.is_empty()
                                && !t.starts_with("```")
                                && !t.starts_with("---")
                        })
                        .unwrap_or("").trim();
                    if line.is_empty() { None }
                    else if line.len() > 100 {
                        Some(format!("{}…", &line[..99]))
                    } else {
                        Some(line.to_string())
                    }
                });
                // Detailed result: first 8 lines
                *dr = result_text.map(|text| {
                    let lines: Vec<&str> = text.lines().take(8).collect();
                    let mut result = lines.join("\n");
                    if text.lines().count() > 8 {
                        result.push_str("\n  …");
                    }
                    result
                });
                break;
            }
        }
    }

    pub fn finalize_message(&mut self) {
        if let Some(Message::Assistant { complete, .. }) = self.messages.last_mut() {
            *complete = true;
        }
        self.streaming = false;
    }

    // ─── Scroll ─────────────────────────────────────────────────

    pub fn scroll_up(&mut self, amount: u16) {
        self.scroll = self.scroll.saturating_add(amount);
    }

    pub fn scroll_down(&mut self, amount: u16) {
        self.scroll = self.scroll.saturating_sub(amount);
    }

    pub fn scroll_offset(&self) -> u16 {
        self.scroll
    }

    // ─── Rendering ──────────────────────────────────────────────

    /// Render conversation to ratatui Lines (test fallback).
    pub fn render_text(&self) -> Vec<Line<'static>> {
        self.render_themed(&super::theme::Alpharius)
    }

    /// Render conversation to ratatui Lines with theme colors.
    #[allow(unused_assignments)] // in_code_fence reset is read by next iteration
    pub fn render_themed(&self, t: &dyn Theme) -> Vec<Line<'static>> {
        let mut lines: Vec<Line<'static>> = Vec::new();
        let mut in_code_fence = false;

        for msg in &self.messages {
            match msg {
                Message::User(text) => {
                    // Thin separator before user messages (except first)
                    if !lines.is_empty() {
                        lines.push(Line::from(""));
                    }
                    lines.push(Line::from(vec![
                        Span::styled("▸ ", t.style_accent_bold()),
                        Span::styled(text.clone(), t.style_user_input()),
                    ]));
                    lines.push(Line::from(""));
                }

                Message::System(text) => {
                    for (i, line) in text.lines().enumerate() {
                        let style = if i == 0 && line.starts_with('Ω') {
                            // Welcome header — bold accent
                            t.style_accent_bold()
                        } else if line.starts_with("  ▸") || line.starts_with("  /") || line.starts_with("  Ctrl") {
                            // Structured info lines — dim
                            Style::default().fg(t.muted())
                        } else {
                            Style::default().fg(t.accent_muted())
                        };
                        lines.push(Line::from(Span::styled(line.to_string(), style)));
                    }
                    lines.push(Line::from(""));
                }

                Message::Assistant { text, thinking, complete } => {
                    // Thinking block (dimmed, with header)
                    if !thinking.is_empty() {
                        lines.push(Line::from(Span::styled(
                            "◌ thinking…",
                            Style::default().fg(t.dim()).add_modifier(Modifier::ITALIC),
                        )));
                        for line in thinking.lines() {
                            lines.push(Line::from(Span::styled(
                                line.to_string(),
                                t.style_dim(),
                            )));
                        }
                        lines.push(Line::from("")); // gap before response
                    }

                    // Assistant text with structural highlighting
                    in_code_fence = false;
                    for line in text.lines() {
                        if line.starts_with("```") {
                            if in_code_fence {
                                // Closing fence
                                in_code_fence = false;
                                lines.push(Line::from(Span::styled(
                                    line.to_string(),
                                    Style::default().fg(t.dim()).bg(t.surface_bg()),
                                )));
                            } else {
                                // Opening fence
                                in_code_fence = true;
                                lines.push(Line::from(Span::styled(
                                    line.to_string(),
                                    Style::default().fg(t.dim()).bg(t.surface_bg()),
                                )));
                            }
                        } else if in_code_fence {
                            // Code block content
                            lines.push(Line::from(Span::styled(
                                line.to_string(),
                                Style::default().fg(t.accent_muted()).bg(t.surface_bg()),
                            )));
                        } else {
                            // Regular text with markdown highlighting
                            lines.push(widgets::highlight_line(line, t));
                        }
                    }

                    if !*complete && text.is_empty() && thinking.is_empty() {
                        lines.push(Line::from(Span::styled("...", t.style_dim())));
                    }
                    lines.push(Line::from(""));
                }

                Message::Tool {
                    name,
                    args_summary,
                    is_error,
                    complete,
                    result_summary,
                    detail_args,
                    detail_result,
                    ..
                } => {
                    match self.tool_detail {
                        crate::settings::ToolDetail::Detailed => {
                            // Bordered card with full args + result
                            lines.extend(widgets::tool_card_detailed(
                                name,
                                *is_error,
                                *complete,
                                detail_args.as_deref().or(args_summary.as_deref()),
                                detail_result.as_deref().or(result_summary.as_deref()),
                                t,
                            ));
                        }
                        crate::settings::ToolDetail::Compact => {
                            lines.push(widgets::tool_card(
                                name,
                                *is_error,
                                *complete,
                                args_summary.as_deref(),
                                result_summary.as_deref(),
                                t,
                            ));
                        }
                    }
                }

                Message::Lifecycle { icon, text } => {
                    lines.push(widgets::lifecycle_event(icon, text, t));
                }
            }
        }

        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_message_renders() {
        let mut cv = ConversationView::new();
        cv.push_user("hello");
        let lines = cv.render_text();
        assert!(!lines.is_empty());
        let text: String = lines[0].spans.iter().map(|s| s.content.to_string()).collect();
        assert!(text.contains("▸"));
        assert!(text.contains("hello"));
    }

    #[test]
    fn streaming_appends_to_same_message() {
        let mut cv = ConversationView::new();
        cv.append_streaming("Hello ");
        cv.append_streaming("world");
        assert_eq!(cv.messages.len(), 1);
        if let Message::Assistant { text, .. } = &cv.messages[0] {
            assert_eq!(text, "Hello world");
        }
    }

    #[test]
    fn finalize_marks_complete() {
        let mut cv = ConversationView::new();
        cv.append_streaming("Done");
        cv.finalize_message();
        if let Message::Assistant { complete, .. } = &cv.messages[0] {
            assert!(complete);
        }
    }

    #[test]
    fn tool_lifecycle() {
        let mut cv = ConversationView::new();
        cv.push_tool_start("tc1", "read", Some("src/main.rs"), None);
        cv.push_tool_end("tc1", false, Some("245 lines"));
        if let Message::Tool {
            complete, is_error, args_summary, result_summary, ..
        } = &cv.messages[0]
        {
            assert!(complete);
            assert!(!is_error);
            assert_eq!(args_summary.as_deref(), Some("src/main.rs"));
            assert!(result_summary.is_some());
        }
    }

    #[test]
    fn tool_card_renders_with_bar() {
        let mut cv = ConversationView::new();
        cv.push_tool_start("t1", "edit", Some("lib.rs"), None);
        cv.push_tool_end("t1", false, Some("Applied edit"));
        let lines = cv.render_text();
        assert!(!lines.is_empty(), "should render tool card");
        let all: String = lines.iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.to_string())
            .collect();
        assert!(all.contains("▎"), "should have left bar: {all}");
        assert!(all.contains("✓"), "should have checkmark: {all}");
        assert!(all.contains("edit"), "should have tool name: {all}");
        assert!(all.contains("lib.rs"), "should have args: {all}");
    }

    #[test]
    fn tool_card_detailed_view() {
        let mut cv = ConversationView::new();
        cv.tool_detail = crate::settings::ToolDetail::Detailed;
        cv.push_tool_start("t1", "bash", Some("ls -la"), Some("ls -la /Users/cwilson/workspace"));
        cv.push_tool_end("t1", false, Some("total 42\ndrwxr-xr-x  5 user  staff  160 Mar 18 10:00 .\ndrwxr-xr-x  3 user  staff   96 Mar 18 09:00 .."));
        let lines = cv.render_text();
        // Should have bordered card with header, command, separator, output, footer
        assert!(lines.len() >= 5, "detailed card should have multiple lines, got {}", lines.len());
        let all_text: String = lines.iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.to_string())
            .collect::<Vec<_>>().join(" ");
        assert!(all_text.contains("bash"), "should show tool name");
        assert!(all_text.contains("ls -la"), "should show full command");
        assert!(all_text.contains("total 42"), "should show output");
    }

    #[test]
    fn thinking_block_renders() {
        let mut cv = ConversationView::new();
        cv.append_thinking("Let me think...");
        cv.append_streaming("Here's my answer.");
        cv.finalize_message();
        let lines = cv.render_text();
        let all_text: String = lines.iter().flat_map(|l| l.spans.iter()).map(|s| s.content.to_string()).collect();
        assert!(all_text.contains("thinking"));
        assert!(all_text.contains("Let me think"));
        assert!(all_text.contains("Here's my answer"));
    }

    #[test]
    fn lifecycle_event_renders() {
        let mut cv = ConversationView::new();
        cv.push_lifecycle("⚡", "Cleave: 3 children dispatched");
        let lines = cv.render_text();
        let text: String = lines[0].spans.iter().map(|s| s.content.to_string()).collect();
        assert!(text.contains("⚡"));
        assert!(text.contains("Cleave"));
    }

    #[test]
    fn code_fence_highlighting() {
        let mut cv = ConversationView::new();
        cv.append_streaming("Here's some code:\n```rust\nfn main() {}\n```\nDone.");
        cv.finalize_message();
        let lines = cv.render_text();
        // The code line should have surface_bg
        let code_line = lines.iter().find(|l| {
            l.spans.iter().any(|s| s.content.contains("fn main"))
        });
        assert!(code_line.is_some(), "should find the code line");
        let code_span = code_line.unwrap().spans.iter()
            .find(|s| s.content.contains("fn main")).unwrap();
        let t = super::super::theme::Alpharius;
        assert_eq!(code_span.style.bg, Some(t.surface_bg()), "code should have surface_bg");
    }

    #[test]
    fn markdown_headers_highlighted() {
        let mut cv = ConversationView::new();
        cv.append_streaming("# Big Header\n## Sub Header\nPlain text");
        cv.finalize_message();
        let lines = cv.render_text();
        // First content line should be the header
        let header_line = lines.iter().find(|l| {
            l.spans.iter().any(|s| s.content.contains("Big Header"))
        });
        assert!(header_line.is_some());
        let span = header_line.unwrap().spans.iter()
            .find(|s| s.content.contains("Big Header")).unwrap();
        assert!(span.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn scroll_operations() {
        let mut cv = ConversationView::new();
        assert_eq!(cv.scroll_offset(), 0);
        cv.scroll_up(5);
        assert_eq!(cv.scroll_offset(), 5);
        cv.scroll_down(3);
        assert_eq!(cv.scroll_offset(), 2);
        cv.scroll_down(10);
        assert_eq!(cv.scroll_offset(), 0);
    }
}
