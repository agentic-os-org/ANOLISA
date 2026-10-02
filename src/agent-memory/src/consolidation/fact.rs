//! Fact data structures for consolidated memories.

use chrono::Utc;
use serde::{Deserialize, Serialize};

/// Categories of extracted facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FactCategory {
    /// Agent worked on a set of files under a common directory.
    WorkingContext,
    /// Agent searched for specific topics.
    Interest,
    /// Agent made targeted edits to a file.
    Change,
    /// Agent encountered an error.
    Lesson,
    /// Agent promoted a file from session scratch to persistent store.
    Promoted,
    /// Summary of the session's activity.
    Summary,
    /// A sequence of tool calls forming a coherent task (episodic memory).
    Episodic,
}

impl std::fmt::Display for FactCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FactCategory::WorkingContext => write!(f, "working-context"),
            FactCategory::Interest => write!(f, "interest"),
            FactCategory::Change => write!(f, "change"),
            FactCategory::Lesson => write!(f, "lesson"),
            FactCategory::Promoted => write!(f, "promoted"),
            FactCategory::Summary => write!(f, "summary"),
            FactCategory::Episodic => write!(f, "episodic"),
        }
    }
}

/// A single consolidated fact — the L1 atomic memory unit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsolidatedFact {
    /// ULID-based unique identifier for this fact.
    pub id: String,
    /// Session that produced this fact.
    pub session_id: String,
    /// Category of the fact.
    pub category: FactCategory,
    /// Short human-readable title.
    pub title: String,
    /// Longer description with context.
    pub content: String,
    /// The tool that produced the underlying evidence.
    pub source_tool: String,
    /// Files/directories referenced by this fact.
    pub related_paths: Vec<String>,
    /// Confidence score 0.0–1.0 (heuristic certainty).
    pub confidence: f64,
    /// When this fact was created (RFC3339 UTC).
    pub created_at: String,
    /// Estimated token count if this fact were injected into context.
    pub token_estimate: usize,
}

impl ConsolidatedFact {
    /// Rough token-count estimate that accounts for CJK characters.
    /// CJK takes ~1-2 tokens per char in most tokenizers (cl100k_base,
    /// DeepSeek, Qwen), while ASCII averages ~4 chars per token.
    pub fn estimate_tokens(text: &str) -> usize {
        let cjk: usize = text.chars().filter(|c| *c > '\u{7f}').count();
        let ascii: usize = text.chars().count() - cjk;
        cjk + (ascii / 4)
    }

    pub fn new(
        session_id: &str,
        category: FactCategory,
        title: String,
        content: String,
        source_tool: String,
        related_paths: Vec<String>,
        confidence: f64,
    ) -> Self {
        let ulid = ulid::Ulid::new();
        let now = Utc::now().to_rfc3339();
        let token_estimate = Self::estimate_tokens(&content);

        Self {
            id: ulid.to_string(),
            session_id: session_id.to_string(),
            category,
            title,
            content,
            source_tool,
            related_paths,
            confidence,
            created_at: now,
            token_estimate,
        }
    }

    /// Serialize as markdown with YAML frontmatter. The output matches the
    /// style used by `memory_observe` — frontmatter + body.
    pub fn to_markdown(&self) -> String {
        let mut out = String::from("---\n");
        out.push_str(&format!("id: {}\n", self.id));
        out.push_str(&format!("session_id: {}\n", self.session_id));
        out.push_str(&format!("category: {}\n", self.category));
        out.push_str(&format!("title: {}\n", sanitize_hint(&self.title)));
        out.push_str(&format!("source_tool: {}\n", self.source_tool));
        if !self.related_paths.is_empty() {
            out.push_str("related_paths:\n");
            for p in &self.related_paths {
                out.push_str(&format!("  - {}\n", sanitize_hint(p)));
            }
        }
        out.push_str(&format!("created_at: {}\n", self.created_at));
        out.push_str(&format!("confidence: {}\n", self.confidence));
        out.push_str("---\n\n");
        let safe_content = self.content.replace("\n---\n", "\n- - -\n");
        out.push_str(&safe_content);
        if !self.content.ends_with('\n') {
            out.push('\n');
        }
        out
    }

    /// Serialize as a single JSONL line (for facts.jsonl).
    pub fn to_jsonl(&self) -> std::result::Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}

/// Sanitize a value for safe frontmatter inclusion.
///
/// The hand-rolled frontmatter readers (parse_frontmatter_flat /
/// extract_title_and_description) use split_once(": ") +
/// trim_matches('"') and do NOT interpret YAML escapes or double-quoting.
/// YAML-style escaping would be asymmetric and cause round-trip
/// regression on Windows paths containing backslashes.
///
/// Strategy:
/// - Replace newlines (\n, \r) with spaces to prevent premature
///   termination of the "---" frontmatter block marker.
/// - Replace other ASCII control characters (\x00-\x1F) with spaces.
/// - Keep all other characters unchanged, including #, :, ", and \.
fn sanitize_hint(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\n' | '\r' => ' ',
            c if c.is_ascii_control() => ' ',
            other => other,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fact_new_generates_ulid() {
        let f = ConsolidatedFact::new(
            "test-session",
            FactCategory::WorkingContext,
            "Test".into(),
            "Test content".into(),
            "mem_write".into(),
            vec!["notes/a.md".into()],
            0.9,
        );
        assert_eq!(f.id.len(), 26); // ULID is 26 chars
        assert_eq!(f.category, FactCategory::WorkingContext);
    }

    #[test]
    fn fact_markdown_has_frontmatter() {
        let f = ConsolidatedFact::new(
            "sid",
            FactCategory::Lesson,
            "Error occurred".into(),
            "Details here".into(),
            "mem_edit".into(),
            vec!["x.rs".into()],
            0.7,
        );
        let md = f.to_markdown();
        assert!(md.starts_with("---\n"));
        assert!(md.contains("category: lesson"));
        assert!(md.contains("source_tool: mem_edit"));
        assert!(md.contains("x.rs"));
        assert!(md.contains("Details here"));
    }

    #[test]
    fn fact_jsonl_roundtrip() {
        let f = ConsolidatedFact::new(
            "sid",
            FactCategory::Interest,
            "Searched for rust".into(),
            "Agent searched for rust ownership".into(),
            "memory_search".into(),
            vec![],
            0.8,
        );
        let line = f.to_jsonl().unwrap();
        let parsed: ConsolidatedFact = serde_json::from_str(&line).unwrap();
        assert_eq!(parsed.title, "Searched for rust");
        assert_eq!(parsed.category, FactCategory::Interest);
    }

    #[test]
    fn category_display() {
        assert_eq!(FactCategory::WorkingContext.to_string(), "working-context");
        assert_eq!(FactCategory::Lesson.to_string(), "lesson");
    }

    #[test]
    fn sanitize_hint_preserves_normal_text() {
        let result = sanitize_hint("a simple title");
        assert_eq!(result, "a simple title");
    }

    #[test]
    fn sanitize_hint_preserves_hashes() {
        let result = sanitize_hint("fix #123 and #456");
        assert_eq!(result, "fix #123 and #456");
    }

    #[test]
    fn sanitize_hint_preserves_colons() {
        let result = sanitize_hint("note: has a colon");
        assert_eq!(result, "note: has a colon");
    }

    #[test]
    fn sanitize_hint_preserves_quotes_and_backslashes() {
        let result = sanitize_hint("she said \"hello\"");
        assert_eq!(result, "she said \"hello\"");
        let result2 = sanitize_hint("C:\\Users\\admin");
        assert_eq!(result2, "C:\\Users\\admin");
    }

    #[test]
    fn sanitize_hint_replaces_newlines() {
        let result = sanitize_hint("line1\nline2");
        assert_eq!(result, "line1 line2");
    }

    #[test]
    fn sanitize_hint_crlf() {
        let result = sanitize_hint("line1\r\nline2");
        assert_eq!(result, "line1  line2");
    }

    #[test]
    fn sanitize_hint_empty() {
        let result = sanitize_hint("");
        assert_eq!(result, "");
    }

    #[test]
    fn sanitize_hint_control_chars() {
        let result = sanitize_hint("pre\x00mid\x1Fpost");
        assert_eq!(result, "pre mid post");
    }

    #[test]
    fn fact_title_with_special_chars() {
        let f = ConsolidatedFact::new(
            "sid",
            FactCategory::Lesson,
            "Use const\nnot var".into(),
            "Details".into(),
            "mem_edit".into(),
            vec![],
            0.7,
        );
        let md = f.to_markdown();
        // Verify the title is encoded without YAML quoting (round-trip compatible)
        assert!(md.contains("title: Use const not var"));
        // Round-trip: extract title from frontmatter and confirm it matches sanitized input
        let title_line = md
            .lines()
            .skip_while(|l| *l != "---")
            .skip(1) // skip opening ---
            .find(|l| l.starts_with("title: "))
            .expect("title field missing in frontmatter");
        let extracted_title = title_line
            .strip_prefix("title: ")
            .expect("title prefix missing");
        assert_eq!(
            extracted_title, "Use const not var",
            "round-trip: special chars survive write->read"
        );
        // Verify no YAML double-quoting artifact
        assert!(
            !md.contains("title: \""),
            "must not use YAML double-quoting"
        );
    }

    #[test]
    fn fact_path_with_backslashes() {
        let f = ConsolidatedFact::new(
            "sid",
            FactCategory::WorkingContext,
            "Work".into(),
            "Details".into(),
            "mem_write".into(),
            vec!["C:\\Users\\admin\\file.txt".into()],
            0.5,
        );
        let md = f.to_markdown();
        // Verify paths are kept verbatim (no YAML double-quoting)
        assert!(md.contains("C:\\Users\\admin\\file.txt"));
        // Round-trip: extract related_paths from frontmatter and confirm
        let mut paths = Vec::new();
        let mut in_frontmatter = false;
        for line in md.lines() {
            if line == "---" {
                if in_frontmatter {
                    break;
                }
                in_frontmatter = true;
                continue;
            }
            if in_frontmatter && line.starts_with("  - ") {
                paths.push(line.strip_prefix("  - ").unwrap().to_string());
            }
        }
        assert_eq!(
            paths,
            vec!["C:\\Users\\admin\\file.txt"],
            "round-trip: Windows paths survive write->read without escaping artifacts"
        );
        // Verify no YAML double-quoting artifact
        assert!(
            !md.contains("C:\\\"Users"),
            "must not use YAML double-quoting on paths"
        );
    }
}
