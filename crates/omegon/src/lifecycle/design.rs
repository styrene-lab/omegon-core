//! Design-tree read-only parser — frontmatter, sections, tree scanning.
//!
//! Parses markdown documents in docs/ with YAML frontmatter into DesignNode
//! and DocumentSections structs. No mutation support (Phase 1b).

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use super::types::*;

/// Parse YAML frontmatter from a markdown document.
/// Returns None if no frontmatter delimiter found.
pub fn parse_frontmatter(content: &str) -> Option<HashMap<String, FrontmatterValue>> {
    let rest = content.strip_prefix("---\n")?;
    let end = rest.find("\n---")?;
    let yaml = &rest[..end];

    let mut result = HashMap::new();
    let mut current_key: Option<String> = None;
    let mut current_array: Vec<String> = Vec::new();

    for line in yaml.lines() {
        // Array item: "  - something"
        if let Some(item) = line.strip_prefix("  - ").or_else(|| line.strip_prefix("- "))
            && current_key.is_some() {
                current_array.push(strip_quotes(item.trim()));
                continue;
            }

        // Flush previous key
        if let Some(key) = current_key.take() {
            if current_array.is_empty() {
                result.insert(key, FrontmatterValue::List(vec![]));
            } else {
                result.insert(key, FrontmatterValue::List(std::mem::take(&mut current_array)));
            }
        }

        // Key-value pair
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim().to_string();
            let value = value.trim();

            if value.is_empty() {
                current_key = Some(key);
                current_array = Vec::new();
            } else if value == "[]" {
                result.insert(key, FrontmatterValue::List(vec![]));
            } else if value.starts_with('[') && value.ends_with(']') {
                // Inline array: [a, b, c]
                let items: Vec<String> = value[1..value.len() - 1]
                    .split(',')
                    .map(|s| strip_quotes(s.trim()))
                    .filter(|s| !s.is_empty())
                    .collect();
                result.insert(key, FrontmatterValue::List(items));
            } else {
                result.insert(key, FrontmatterValue::Scalar(strip_quotes(value)));
            }
        }
    }

    // Flush final key
    if let Some(key) = current_key {
        result.insert(key, FrontmatterValue::List(current_array));
    }

    Some(result)
}

/// A frontmatter value — either scalar or list.
#[derive(Debug, Clone)]
pub enum FrontmatterValue {
    Scalar(String),
    List(Vec<String>),
}

impl FrontmatterValue {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Scalar(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_list(&self) -> &[String] {
        match self {
            Self::List(v) => v,
            _ => &[],
        }
    }
}

fn strip_quotes(s: &str) -> String {
    let s = s.trim();
    if (s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')) {
        s[1..s.len() - 1].to_string()
    } else {
        // Strip inline YAML comments
        s.split(" #").next().unwrap_or(s).trim().to_string()
    }
}

/// Extract the body after the frontmatter.
pub fn extract_body(content: &str) -> &str {
    if let Some(rest) = content.strip_prefix("---\n") {
        if let Some(end) = rest.find("\n---") {
            let after = &rest[end + 4..];
            after.trim_start_matches('\n')
        } else {
            content
        }
    } else {
        content
    }
}

/// Build a DesignNode from parsed frontmatter.
pub fn node_from_frontmatter(
    fm: &HashMap<String, FrontmatterValue>,
    file_path: PathBuf,
) -> Option<DesignNode> {
    let id = fm.get("id")?.as_str()?.to_string();
    let title = fm.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let status_str = fm.get("status").and_then(|v| v.as_str()).unwrap_or("seed");
    let status = NodeStatus::from_str(status_str).unwrap_or(NodeStatus::Seed);

    Some(DesignNode {
        id,
        title,
        status,
        parent: fm.get("parent").and_then(|v| v.as_str()).map(String::from),
        tags: fm.get("tags").map(|v| v.as_list().to_vec()).unwrap_or_default(),
        dependencies: fm.get("dependencies").map(|v| v.as_list().to_vec()).unwrap_or_default(),
        related: fm.get("related").map(|v| v.as_list().to_vec()).unwrap_or_default(),
        open_questions: fm.get("open_questions").map(|v| v.as_list().to_vec()).unwrap_or_default(),
        branches: fm.get("branches").map(|v| v.as_list().to_vec()).unwrap_or_default(),
        openspec_change: fm.get("openspec_change").and_then(|v| v.as_str()).map(String::from),
        issue_type: fm.get("issue_type").and_then(|v| v.as_str()).and_then(IssueType::from_str),
        priority: fm.get("priority").and_then(|v| v.as_str()).and_then(|s| s.parse().ok()),
        file_path,
    })
}

/// Parse the structured sections from a design document body.
pub fn parse_sections(body: &str) -> DocumentSections {
    let mut sections = DocumentSections::default();

    // Split body into h2 sections
    let mut current_heading = "";
    let mut current_content = String::new();
    let mut h2_blocks: Vec<(&str, String)> = Vec::new();

    for line in body.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            if !current_heading.is_empty() || !current_content.trim().is_empty() {
                h2_blocks.push((current_heading, std::mem::take(&mut current_content)));
            }
            current_heading = heading.trim();
            current_content = String::new();
        } else {
            current_content.push_str(line);
            current_content.push('\n');
        }
    }
    if !current_heading.is_empty() || !current_content.trim().is_empty() {
        h2_blocks.push((current_heading, current_content));
    }

    for (heading, content) in h2_blocks {
        match heading {
            "Overview" => sections.overview = content.trim().to_string(),
            "Research" => sections.research = parse_research(&content),
            "Decisions" => sections.decisions = parse_decisions(&content),
            "Open Questions" => sections.open_questions = parse_questions(&content),
            "Implementation Notes" => parse_impl_notes(&content, &mut sections),
            _ => {} // Acceptance Criteria, extra sections — skip for Phase 1a
        }
    }

    sections
}

fn parse_research(content: &str) -> Vec<ResearchEntry> {
    let mut entries = Vec::new();
    let mut current_heading = String::new();
    let mut current_content = String::new();

    for line in content.lines() {
        if let Some(heading) = line.strip_prefix("### ") {
            if !current_heading.is_empty() {
                entries.push(ResearchEntry {
                    heading: std::mem::take(&mut current_heading),
                    content: std::mem::take(&mut current_content).trim().to_string(),
                });
            }
            current_heading = heading.trim().to_string();
            current_content = String::new();
        } else {
            current_content.push_str(line);
            current_content.push('\n');
        }
    }
    if !current_heading.is_empty() {
        entries.push(ResearchEntry {
            heading: current_heading,
            content: current_content.trim().to_string(),
        });
    }

    entries
}

fn parse_decisions(content: &str) -> Vec<DesignDecision> {
    let mut decisions = Vec::new();
    let mut current_title = String::new();
    let mut current_status = String::new();
    let mut current_rationale = String::new();
    let mut in_decision = false;

    for line in content.lines() {
        if let Some(heading) = line.strip_prefix("### ") {
            // Flush previous
            if in_decision && !current_title.is_empty() {
                decisions.push(DesignDecision {
                    title: std::mem::take(&mut current_title),
                    status: std::mem::take(&mut current_status),
                    rationale: std::mem::take(&mut current_rationale).trim().to_string(),
                });
            }
            current_title = heading.trim().to_string();
            in_decision = true;
        } else if in_decision {
            if let Some(rest) = line.strip_prefix("**Status:**") {
                current_status = rest.trim().to_string();
            } else if let Some(rest) = line.strip_prefix("**Rationale:**") {
                current_rationale = rest.trim().to_string();
                current_rationale.push('\n');
            } else if !current_rationale.is_empty() || line.trim().is_empty() {
                // Continue rationale
                current_rationale.push_str(line);
                current_rationale.push('\n');
            }
        }
    }
    if in_decision && !current_title.is_empty() {
        decisions.push(DesignDecision {
            title: current_title,
            status: current_status,
            rationale: current_rationale.trim().to_string(),
        });
    }

    decisions
}

fn parse_questions(content: &str) -> Vec<String> {
    content
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            trimmed
                .strip_prefix("- ")
                .or_else(|| trimmed.strip_prefix("* "))
                .map(|s| s.to_string())
        })
        .collect()
}

fn parse_impl_notes(content: &str, sections: &mut DocumentSections) {
    let mut in_file_scope = false;
    let mut in_constraints = false;

    for line in content.lines() {
        if line.starts_with("### File Scope") || line.starts_with("### file_scope") {
            in_file_scope = true;
            in_constraints = false;
        } else if line.starts_with("### Constraints") || line.starts_with("### constraints") {
            in_file_scope = false;
            in_constraints = true;
        } else if line.starts_with("### ") {
            in_file_scope = false;
            in_constraints = false;
        } else if in_file_scope {
            // Format: - `path` — description (action)
            if let Some(rest) = line.trim().strip_prefix("- ")
                && let Some((path_part, desc)) = rest.split_once(" — ").or_else(|| rest.split_once(" - ")) {
                    let path = path_part.trim().trim_matches('`').to_string();
                    let (description, action) = if desc.ends_with(')') {
                        if let Some(paren) = desc.rfind('(') {
                            (desc[..paren].trim().to_string(), Some(desc[paren + 1..desc.len() - 1].to_string()))
                        } else {
                            (desc.to_string(), None)
                        }
                    } else {
                        (desc.to_string(), None)
                    };
                    sections.impl_file_scope.push(FileScope { path, description, action });
                }
        } else if in_constraints
            && let Some(rest) = line.trim().strip_prefix("- ") {
                sections.impl_constraints.push(rest.to_string());
            }
    }
}

/// Scan a docs/ directory for design documents and build a tree.
pub fn scan_design_docs(docs_dir: &Path) -> HashMap<String, DesignNode> {
    let mut nodes = HashMap::new();

    let entries = match fs::read_dir(docs_dir) {
        Ok(entries) => entries,
        Err(_) => return nodes,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }

        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        if let Some(fm) = parse_frontmatter(&content)
            && let Some(node) = node_from_frontmatter(&fm, path) {
                nodes.insert(node.id.clone(), node);
            }
    }

    // Also scan docs/design/ subdirectory if it exists
    let design_dir = docs_dir.join("design");
    if design_dir.is_dir()
        && let Ok(entries) = fs::read_dir(&design_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("md") {
                    continue;
                }
                let content = match fs::read_to_string(&path) {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                if let Some(fm) = parse_frontmatter(&content)
                    && let Some(node) = node_from_frontmatter(&fm, path) {
                        nodes.insert(node.id.clone(), node);
                    }
            }
        }

    nodes
}

/// Get children of a node.
pub fn get_children<'a>(
    nodes: &'a HashMap<String, DesignNode>,
    parent_id: &str,
) -> Vec<&'a DesignNode> {
    nodes
        .values()
        .filter(|n| n.parent.as_deref() == Some(parent_id))
        .collect()
}

/// Read and parse the sections of a design node document.
pub fn read_node_sections(node: &DesignNode) -> Option<DocumentSections> {
    let content = fs::read_to_string(&node.file_path).ok()?;
    let body = extract_body(&content);
    Some(parse_sections(body))
}

/// Build a context injection string for a focused design node.
/// Includes: overview, key decisions, open questions.
pub fn build_context_injection(node: &DesignNode, sections: &DocumentSections) -> String {
    let mut lines = Vec::new();

    lines.push(format!(
        "[Design: {} {} — {}]",
        node.status.icon(),
        node.id,
        node.title
    ));

    if !sections.overview.is_empty() {
        let overview = if sections.overview.len() > 500 {
            format!("{}...", &sections.overview[..500])
        } else {
            sections.overview.clone()
        };
        lines.push(format!("Overview: {overview}"));
    }

    let decided: Vec<_> = sections
        .decisions
        .iter()
        .filter(|d| d.status == "decided")
        .collect();
    if !decided.is_empty() {
        lines.push("Key decisions:".to_string());
        for d in decided {
            lines.push(format!("  - {} — {}", d.title, truncate(&d.rationale, 150)));
        }
    }

    if !node.open_questions.is_empty() {
        lines.push("Open questions:".to_string());
        for q in &node.open_questions {
            lines.push(format!("  - {q}"));
        }
    }

    lines.join("\n")
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut end = max;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &s[..end])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_DOC: &str = r#"---
id: test-node
title: "Test Node — with special chars"
status: exploring
parent: parent-node
tags: [rust, test, lifecycle]
open_questions:
  - How does X work?
  - What about Y?
dependencies: []
related: [other-node]
branches: [feature/test-node]
openspec_change: test-change
issue_type: feature
priority: 2
---

# Test Node — with special chars

## Overview

This is the overview text.
It spans multiple lines.

## Research

### First research topic

Content of the first topic.

### Second research topic

Content of the second topic.

## Decisions

### Use approach A over B

**Status:** decided

**Rationale:** Approach A is simpler and handles all edge cases.

### Consider approach C

**Status:** exploring

**Rationale:** Approach C might work but needs more investigation.

## Open Questions

- How does X work?
- What about Y?

## Implementation Notes

### File Scope

- `src/foo.rs` — Main implementation (new)
- `src/bar.rs` — Updated types (modified)

### Constraints

- Must handle UTF-8 correctly
- No external YAML parser dependency
"#;

    #[test]
    fn parse_frontmatter_full() {
        let fm = parse_frontmatter(SAMPLE_DOC).unwrap();
        assert_eq!(fm.get("id").unwrap().as_str(), Some("test-node"));
        assert_eq!(fm.get("title").unwrap().as_str(), Some("Test Node — with special chars"));
        assert_eq!(fm.get("status").unwrap().as_str(), Some("exploring"));
        assert_eq!(fm.get("parent").unwrap().as_str(), Some("parent-node"));
        assert_eq!(fm.get("tags").unwrap().as_list(), &["rust", "test", "lifecycle"]);
        assert_eq!(fm.get("open_questions").unwrap().as_list().len(), 2);
        assert_eq!(fm.get("dependencies").unwrap().as_list().len(), 0);
        assert_eq!(fm.get("related").unwrap().as_list(), &["other-node"]);
        assert_eq!(fm.get("branches").unwrap().as_list(), &["feature/test-node"]);
        assert_eq!(fm.get("openspec_change").unwrap().as_str(), Some("test-change"));
        assert_eq!(fm.get("issue_type").unwrap().as_str(), Some("feature"));
        assert_eq!(fm.get("priority").unwrap().as_str(), Some("2"));
    }

    #[test]
    fn node_from_frontmatter_full() {
        let fm = parse_frontmatter(SAMPLE_DOC).unwrap();
        let node = node_from_frontmatter(&fm, PathBuf::from("docs/test-node.md")).unwrap();
        assert_eq!(node.id, "test-node");
        assert_eq!(node.status, NodeStatus::Exploring);
        assert_eq!(node.parent.as_deref(), Some("parent-node"));
        assert_eq!(node.tags, vec!["rust", "test", "lifecycle"]);
        assert_eq!(node.open_questions.len(), 2);
        assert_eq!(node.issue_type, Some(IssueType::Feature));
        assert_eq!(node.priority, Some(2));
        assert_eq!(node.openspec_change.as_deref(), Some("test-change"));
    }

    #[test]
    fn parse_sections_full() {
        let body = extract_body(SAMPLE_DOC);
        let sections = parse_sections(body);

        assert!(sections.overview.contains("overview text"));
        assert_eq!(sections.research.len(), 2);
        assert_eq!(sections.research[0].heading, "First research topic");
        assert!(sections.research[0].content.contains("first topic"));

        assert_eq!(sections.decisions.len(), 2);
        assert_eq!(sections.decisions[0].title, "Use approach A over B");
        assert_eq!(sections.decisions[0].status, "decided");
        assert!(sections.decisions[0].rationale.contains("simpler"));
        assert_eq!(sections.decisions[1].status, "exploring");

        assert_eq!(sections.open_questions.len(), 2);
        assert_eq!(sections.open_questions[0], "How does X work?");

        assert_eq!(sections.impl_file_scope.len(), 2);
        assert_eq!(sections.impl_file_scope[0].path, "src/foo.rs");
        assert_eq!(sections.impl_file_scope[0].action.as_deref(), Some("new"));
        assert_eq!(sections.impl_file_scope[1].action.as_deref(), Some("modified"));

        assert_eq!(sections.impl_constraints.len(), 2);
        assert!(sections.impl_constraints[0].contains("UTF-8"));
    }

    #[test]
    fn extract_body_strips_frontmatter() {
        let body = extract_body(SAMPLE_DOC);
        assert!(body.starts_with("# Test Node"));
        assert!(!body.contains("---\nid:"));
    }

    #[test]
    fn context_injection_format() {
        let node = DesignNode {
            id: "test".into(),
            title: "Test Node".into(),
            status: NodeStatus::Decided,
            parent: None,
            tags: vec![],
            dependencies: vec![],
            related: vec![],
            open_questions: vec!["How?".into()],
            branches: vec![],
            openspec_change: None,
            issue_type: None,
            priority: None,
            file_path: PathBuf::new(),
        };
        let sections = DocumentSections {
            overview: "Short overview".into(),
            decisions: vec![DesignDecision {
                title: "Use X".into(),
                status: "decided".into(),
                rationale: "Because Y".into(),
            }],
            ..Default::default()
        };
        let injection = build_context_injection(&node, &sections);
        assert!(injection.contains("● test — Test Node"));
        assert!(injection.contains("Short overview"));
        assert!(injection.contains("Use X"));
        assert!(injection.contains("How?"));
    }

    #[test]
    fn empty_frontmatter_returns_none() {
        assert!(parse_frontmatter("No frontmatter here").is_none());
    }

    #[test]
    fn frontmatter_missing_id_returns_none() {
        let content = "---\ntitle: No ID\nstatus: seed\n---\n# No ID";
        let fm = parse_frontmatter(content).unwrap();
        assert!(node_from_frontmatter(&fm, PathBuf::new()).is_none());
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn scan_real_docs_directory() {
        // Test against the actual Omegon docs/ directory
        let docs_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent().unwrap()
            .parent().unwrap()
            .join("docs");

        if !docs_dir.exists() {
            eprintln!("Skipping: docs/ not found at {}", docs_dir.display());
            return;
        }

        let nodes = scan_design_docs(&docs_dir);
        assert!(!nodes.is_empty(), "Should find at least one design node in docs/");

        // Verify known nodes exist
        let known_ids = ["rust-phase-1", "rust-compaction", "rust-lifecycle-crates"];
        for id in &known_ids {
            assert!(nodes.contains_key(*id), "Missing expected node: {id}");
        }

        // Verify all nodes have valid status
        for (id, node) in &nodes {
            assert!(!id.is_empty(), "Node has empty id");
            assert!(!node.title.is_empty(), "Node {id} has empty title");
            // Status was parsed from frontmatter so it's guaranteed valid
        }

        // Parse sections for a known node
        if let Some(node) = nodes.get("rust-lifecycle-crates") {
            let sections = read_node_sections(node).unwrap();
            assert!(!sections.overview.is_empty(), "rust-lifecycle-crates should have overview");
            assert!(!sections.decisions.is_empty(), "rust-lifecycle-crates should have decisions");
        }

        eprintln!("Scanned {} design nodes from {}", nodes.len(), docs_dir.display());
    }
}
