//! Conversation state — manages the segment list and push/mutation methods.
//!
//! This module holds the data model. Rendering is handled by
//! `conv_widget::ConversationWidget`.

use super::segments::Segment;
use super::conv_widget::ConvState;

/// Conversation view state — segment list + scroll.
pub struct ConversationView {
    segments: Vec<Segment>,
    /// Whether we're currently receiving streaming text.
    streaming: bool,
    /// Scroll + height cache state — shared with the widget.
    pub conv_state: ConvState,
}

impl ConversationView {
    pub fn new() -> Self {
        Self {
            segments: Vec::new(),
            streaming: false,
            conv_state: ConvState::new(),
        }
    }

    /// Access segments for rendering.
    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    /// Split borrow — immutable segments + mutable state.
    /// Needed because ConversationWidget borrows segments immutably
    /// while render_stateful_widget needs mutable state.
    pub fn segments_and_state(&mut self) -> (&[Segment], &mut ConvState) {
        (&self.segments, &mut self.conv_state)
    }

    // ─── Push methods ───────────────────────────────────────────

    pub fn push_user(&mut self, text: &str) {
        // Turn separator before user messages (except first)
        if !self.segments.is_empty() {
            self.segments.push(Segment::TurnSeparator);
        }
        self.segments.push(Segment::UserPrompt { text: text.to_string() });
        self.conv_state.invalidate();
        self.conv_state.force_scroll_to_bottom();
    }

    pub fn push_system(&mut self, text: &str) {
        self.segments.push(Segment::SystemNotification { text: text.to_string() });
        self.conv_state.invalidate();
        self.conv_state.force_scroll_to_bottom();
    }

    pub fn push_lifecycle(&mut self, icon: &str, text: &str) {
        self.segments.push(Segment::LifecycleEvent {
            icon: icon.to_string(),
            text: text.to_string(),
        });
        self.conv_state.invalidate();
        self.conv_state.auto_scroll_to_bottom();
    }

    pub fn append_streaming(&mut self, delta: &str) {
        if !self.streaming {
            self.segments.push(Segment::AssistantText {
                text: String::new(),
                thinking: String::new(),
                complete: false,
            });
            self.streaming = true;
        }

        if let Some(Segment::AssistantText { text, .. }) = self.segments.last_mut() {
            text.push_str(delta);
        }
        self.conv_state.invalidate();
        self.conv_state.auto_scroll_to_bottom();
    }

    pub fn append_thinking(&mut self, delta: &str) {
        if !self.streaming {
            self.segments.push(Segment::AssistantText {
                text: String::new(),
                thinking: String::new(),
                complete: false,
            });
            self.streaming = true;
        }

        if let Some(Segment::AssistantText { thinking, .. }) = self.segments.last_mut() {
            thinking.push_str(delta);
        }
        self.conv_state.invalidate();
        self.conv_state.auto_scroll_to_bottom();
    }

    pub fn push_tool_start(&mut self, id: &str, name: &str, args_summary: Option<&str>, detail_args: Option<&str>) {
        self.segments.push(Segment::ToolCard {
            id: id.to_string(),
            name: name.to_string(),
            args_summary: args_summary.map(|s| s.to_string()),
            detail_args: detail_args.map(|s| s.to_string()),
            result_summary: None,
            detail_result: None,
            is_error: false,
            complete: false,
        });
        self.conv_state.invalidate();
        self.conv_state.auto_scroll_to_bottom();
    }

    pub fn push_tool_end(&mut self, id: &str, is_error: bool, result_text: Option<&str>) {
        for seg in self.segments.iter_mut().rev() {
            if let Segment::ToolCard {
                id: tool_id,
                complete: c,
                is_error: e,
                result_summary: r,
                detail_result: dr,
                ..
            } = seg
                && tool_id == id && !*c
            {
                *c = true;
                *e = is_error;
                *r = result_text.and_then(|text| {
                    let line = text.lines()
                        .find(|l| {
                            let t = l.trim();
                            !t.is_empty() && !t.starts_with("```") && !t.starts_with("---")
                        })
                        .unwrap_or("").trim();
                    if line.is_empty() { None }
                    else if line.len() > 100 { Some(format!("{}…", &line[..99])) }
                    else { Some(line.to_string()) }
                });
                *dr = result_text.map(|text| {
                    let lines: Vec<&str> = text.lines().take(12).collect();
                    let mut result = lines.join("\n");
                    if text.lines().count() > 12 {
                        result.push_str(&format!("\n  … {} lines total", text.lines().count()));
                    }
                    result
                });
                break;
            }
        }
        self.conv_state.invalidate();
    }

    pub fn finalize_message(&mut self) {
        if let Some(Segment::AssistantText { complete, .. }) = self.segments.last_mut() {
            *complete = true;
        }
        self.streaming = false;
        self.conv_state.invalidate();
        self.conv_state.user_scrolled = false;
        self.conv_state.scroll_offset = 0;
    }

    // ─── Scroll ─────────────────────────────────────────────────

    pub fn scroll_up(&mut self, amount: u16) {
        self.conv_state.scroll_up(amount);
    }

    pub fn scroll_down(&mut self, amount: u16) {
        self.conv_state.scroll_down(amount);
    }

    /// Clear all segments (for /clear command).
    pub fn clear(&mut self) {
        self.segments.clear();
        self.conv_state = ConvState::new();
        self.streaming = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::theme::Alpharius;
    use ratatui::prelude::*;

    #[test]
    fn user_message_creates_segments() {
        let mut cv = ConversationView::new();
        cv.push_user("hello");
        // First user message: just the prompt (no separator)
        assert_eq!(cv.segments.len(), 1);
        assert!(matches!(&cv.segments[0], Segment::UserPrompt { text } if text == "hello"));
    }

    #[test]
    fn second_user_message_adds_separator() {
        let mut cv = ConversationView::new();
        cv.push_user("first");
        cv.push_user("second");
        // separator + prompt
        assert_eq!(cv.segments.len(), 3);
        assert!(matches!(&cv.segments[1], Segment::TurnSeparator));
    }

    #[test]
    fn streaming_creates_assistant_segment() {
        let mut cv = ConversationView::new();
        cv.append_streaming("Hello ");
        cv.append_streaming("world");
        assert_eq!(cv.segments.len(), 1);
        if let Segment::AssistantText { text, complete, .. } = &cv.segments[0] {
            assert_eq!(text, "Hello world");
            assert!(!complete);
        } else {
            panic!("expected AssistantText");
        }
    }

    #[test]
    fn finalize_marks_complete() {
        let mut cv = ConversationView::new();
        cv.append_streaming("Done");
        cv.finalize_message();
        if let Segment::AssistantText { complete, .. } = &cv.segments[0] {
            assert!(complete);
        }
    }

    #[test]
    fn tool_lifecycle() {
        let mut cv = ConversationView::new();
        cv.push_tool_start("tc1", "read", Some("src/main.rs"), Some("src/main.rs"));
        cv.push_tool_end("tc1", false, Some("fn main() {}\n// 245 lines"));
        if let Segment::ToolCard { complete, is_error, detail_result, .. } = &cv.segments[0] {
            assert!(complete);
            assert!(!is_error);
            assert!(detail_result.is_some());
        }
    }

    #[test]
    fn scroll_up_sets_user_scrolled() {
        let mut cv = ConversationView::new();
        cv.scroll_up(3);
        assert!(cv.conv_state.user_scrolled);
    }

    #[test]
    fn push_user_forces_scroll_to_bottom() {
        let mut cv = ConversationView::new();
        cv.scroll_up(10);
        cv.push_user("new prompt");
        assert_eq!(cv.conv_state.scroll_offset, 0);
        assert!(!cv.conv_state.user_scrolled);
    }

    #[test]
    fn finalize_resets_scroll() {
        let mut cv = ConversationView::new();
        cv.append_streaming("text");
        cv.scroll_up(10);
        cv.finalize_message();
        assert!(!cv.conv_state.user_scrolled);
        assert_eq!(cv.conv_state.scroll_offset, 0);
    }

    #[test]
    fn segments_render_via_widget() {
        let mut cv = ConversationView::new();
        cv.push_user("hello");
        cv.append_streaming("response");
        cv.finalize_message();
        cv.push_tool_start("t1", "bash", Some("echo hi"), Some("echo hi"));
        cv.push_tool_end("t1", false, Some("hi"));

        let area = Rect::new(0, 0, 80, 40);
        let mut buf = Buffer::empty(area);
        let (segments, state) = cv.segments_and_state();
        let widget = super::super::conv_widget::ConversationWidget::new(segments, &Alpharius);
        widget.render(area, &mut buf, state);

        // Verify segments were rendered
        let mut found_hello = false;
        let mut found_bash = false;
        for y in 0..40 {
            let mut row = String::new();
            for x in 0..80 { row.push_str(buf[(x, y)].symbol()); }
            if row.contains("hello") { found_hello = true; }
            if row.contains("bash") { found_bash = true; }
        }
        assert!(found_hello, "should render user prompt");
        assert!(found_bash, "should render tool card");
    }
}
