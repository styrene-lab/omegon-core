//! NDJSON progress events emitted on stdout during cleave orchestration.
//!
//! The TS native-dispatch.ts wrapper reads these line-by-line and maps them
//! to `emitCleaveChildProgress` calls so the dashboard updates live.

use serde::Serialize;
use std::io::Write;

/// Progress events emitted as JSON lines on stdout.
#[derive(Debug, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ProgressEvent {
    WaveStart {
        wave: usize,
        children: Vec<String>,
    },
    ChildSpawned {
        child: String,
        pid: u32,
    },
    ChildStatus {
        child: String,
        status: ChildProgressStatus,
        #[serde(skip_serializing_if = "Option::is_none")]
        duration_secs: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    ChildActivity {
        child: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        turn: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tool: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        target: Option<String>,
    },
    AutoCommit {
        child: String,
        files: usize,
    },
    MergeStart,
    MergeResult {
        child: String,
        success: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    Done {
        completed: usize,
        failed: usize,
        duration_secs: f64,
    },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChildProgressStatus {
    Running,
    Completed,
    Failed,
}

/// Emit a progress event as a JSON line on stdout.
///
/// Uses `println!` for atomic line writes. Stdout is exclusively used
/// for progress events — all tracing/diagnostic output goes to stderr.
pub fn emit_progress(event: &ProgressEvent) {
    if let Ok(json) = serde_json::to_string(event) {
        let _ = std::io::stdout().write_all(json.as_bytes());
        let _ = std::io::stdout().write_all(b"\n");
        let _ = std::io::stdout().flush();
    }
}

/// Parse a child stderr line for tool-call or turn-boundary patterns.
///
/// Returns a `ChildActivity` event if the line matches, or `None`.
/// Recognized patterns:
/// - `→ write path/to/file` → tool="write", target="path/to/file"
/// - `→ bash command...`    → tool="bash", target="command..."
/// - `── Turn N ──`         → turn=N
pub fn parse_child_activity(child: &str, line: &str) -> Option<ProgressEvent> {
    // Strip ANSI escape codes for matching
    let clean = strip_ansi(line);
    let trimmed = clean.trim();

    // Tool call: "→ toolname target"
    if let Some(rest) = trimmed.strip_prefix("→ ").or_else(|| trimmed.strip_prefix("→ ")) {
        let mut parts = rest.splitn(2, ' ');
        let tool = parts.next()?.to_string();
        let target = parts.next().map(|s| s.to_string());
        return Some(ProgressEvent::ChildActivity {
            child: child.to_string(),
            turn: None,
            tool: Some(tool),
            target,
        });
    }

    // Turn boundary: "── Turn N ──" or "Turn N"
    if let Some(turn) = extract_turn_number(trimmed) {
        return Some(ProgressEvent::ChildActivity {
            child: child.to_string(),
            turn: Some(turn),
            tool: None,
            target: None,
        });
    }

    None
}

fn extract_turn_number(s: &str) -> Option<u32> {
    // Match "── Turn N ──" or "Turn N"
    let s = s.trim_start_matches('─').trim_start_matches(' ');
    let s = s.strip_prefix("Turn ")?;
    let num_str: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    num_str.parse().ok()
}

/// Strip ANSI escape sequences from a string.
fn strip_ansi(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Skip until 'm' (SGR) or other terminator
            for c2 in chars.by_ref() {
                if c2.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            result.push(c);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_emit_progress_serialization() {
        let event = ProgressEvent::ChildSpawned {
            child: "test-a".to_string(),
            pid: 1234,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""event":"child_spawned"#));
        assert!(json.contains(r#""child":"test-a"#));
        assert!(json.contains(r#""pid":1234"#));
    }

    #[test]
    fn test_parse_tool_call() {
        let event = parse_child_activity("ch1", "→ write tmp/foo.txt").unwrap();
        match event {
            ProgressEvent::ChildActivity { child, tool, target, turn } => {
                assert_eq!(child, "ch1");
                assert_eq!(tool.unwrap(), "write");
                assert_eq!(target.unwrap(), "tmp/foo.txt");
                assert!(turn.is_none());
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_parse_turn_boundary() {
        let event = parse_child_activity("ch1", "── Turn 3 ──").unwrap();
        match event {
            ProgressEvent::ChildActivity { turn, .. } => {
                assert_eq!(turn, Some(3));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_parse_ansi_tool_call() {
        let line = "\x1b[2m→ bash\x1b[0m ls -la";
        let event = parse_child_activity("ch1", line).unwrap();
        match event {
            ProgressEvent::ChildActivity { tool, .. } => {
                assert_eq!(tool.unwrap(), "bash");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_parse_no_match() {
        assert!(parse_child_activity("ch1", "just some random output").is_none());
    }

    #[test]
    fn test_strip_ansi() {
        assert_eq!(strip_ansi("\x1b[32mhello\x1b[0m"), "hello");
        assert_eq!(strip_ansi("no escapes"), "no escapes");
    }

    #[test]
    fn test_done_event() {
        let event = ProgressEvent::Done {
            completed: 3,
            failed: 1,
            duration_secs: 45.5,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""event":"done"#));
        assert!(json.contains(r#""completed":3"#));
    }
}
