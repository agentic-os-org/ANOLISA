use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::os::fd::AsFd;
use std::path::Path;

use crate::audit::AuditEntry;
use crate::config::MemoryConfig;
use crate::error::{MemoryError, Result};
use crate::service::MemoryService;

const TOOL: &str = "memory_observe";

/// Closed 4-type memory taxonomy inspired by Dreaming V3 and Claude Code memdir.
///
/// Design rationale: maps to a 2×2 matrix (personal/project × subjective/objective).
/// Open taxonomies cause type explosion and classification ambiguity.
///
/// | Type       | Personal × Subjective | Project × Subjective |
/// |------------|----------------------|---------------------|
/// | User       | ✓ (who you are)      |                     |
/// | Feedback   | ✓ (how to behave)    |                     |
/// | Project    |                      | ✓ (what/why/when)   |
/// | Reference  |                      | ✓ (where to find)   |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryType {
    /// Personal × Subjective: user profile, preferences, knowledge background.
    /// "I'm a Rust developer", "I prefer concise responses"
    User,
    /// Personal × Objective: Agent behavior corrections and confirmations.
    /// "Don't use var, use const", "Always run tests before committing"
    Feedback,
    /// Project × Subjective: decisions, status, conventions, deadlines.
    /// "Auth uses JWT for mobile support", "Migration deadline is March 1"
    Project,
    /// Project × Objective: pointers to external resources.
    /// "Grafana dashboard at https://...", "API docs on Confluence"
    Reference,
}

impl MemoryType {
    /// Parse from string, case-insensitive.
    pub fn parse(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "user" => Ok(Self::User),
            "feedback" => Ok(Self::Feedback),
            "project" => Ok(Self::Project),
            "reference" => Ok(Self::Reference),
            _ => Err(MemoryError::InvalidArgument(format!(
                "unknown memory type '{s}'; expected user, feedback, project, or reference"
            ))),
        }
    }
}

impl std::fmt::Display for MemoryType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MemoryType::User => write!(f, "user"),
            MemoryType::Feedback => write!(f, "feedback"),
            MemoryType::Project => write!(f, "project"),
            MemoryType::Reference => write!(f, "reference"),
        }
    }
}

// Non-derivable information principle:
// These categories should NOT be stored as memories because they can
// be obtained in real-time from the codebase or Git history:
//
// - Code patterns, architecture, file structure → `ls`, `grep`
// - Git history → `git log`, `git blame`
// - Debug solutions → commit messages
// - Third-party library versions → package manifests
//
// The `memory_observe` tool description should guide the Agent toward
// storing only non-derivable information.

/// Tier B: record an observation with optional type classification.
/// The OS picks a stable filename under `notes/observed/<ulid>.md` and
/// writes frontmatter (type + hint + created_at) + body.
pub fn memory_observe(
    svc: &MemoryService,
    content: &str,
    hint: Option<&str>,
    memory_type: Option<&str>,
    config: &MemoryConfig,
) -> Result<String> {
    let ulid = ulid::Ulid::new();
    let path = format!("notes/observed/{ulid}.md");

    // Parse and validate memory type, defaulting to User
    let parsed_type = match memory_type {
        Some(t) => MemoryType::parse(t)?,
        None => MemoryType::User,
    };

    let mut body = String::new();
    body.push_str("---\n");

    // Always write type (defaults to "user")
    body.push_str(&format!("type: {parsed_type}\n"));

    if let Some(h) = hint {
        // Enforce max_hint_bytes limit: a rogue or misconfigured model could
        // inject an arbitrarily long hint, exhausting disk and JSON-RPC buffer
        // budget. Reject early before we construct the frontmatter body.
        if h.len() as u64 > config.max_hint_bytes {
            return Err(MemoryError::InvalidArgument(format!(
                "hint byte length {} exceeds max_hint_bytes {}",
                h.len(),
                config.max_hint_bytes
            )));
        }
        // Sanitize the hint for safe frontmatter inclusion. The only genuine
        // risk is a newline (\n or \r\n) prematurely terminating the YAML
        // frontmatter block marker ("---"). The hand-rolled readers
        // (parse_frontmatter_flat / extract_title_and_description) use
        // split_once(": ") + trim_matches('"') and do NOT parse YAML
        // escapes, so we must not apply YAML double-quote or backslash
        // escaping — that would be asymmetric and cause a round-trip
        // regression on Windows paths containing backslashes.
        // Replace newlines and ASCII control chars with spaces.
        let safe = sanitize_hint(h);
        body.push_str(&format!("hint: {safe}\n"));
    }

    // Mark source as manual-observe for sovereignty tracking
    body.push_str("source: manual-observe\n");
    body.push_str(&format!("created_at: {}\n", Utc::now().to_rfc3339()));
    body.push_str("---\n\n");
    body.push_str(content);
    if !content.ends_with('\n') {
        body.push('\n');
    }

    let n = svc.write(&path, &body, false)?;

    // Synchronously upsert the new observation into the BM25 index so it
    // is immediately searchable by auto-recall hooks. Without this, the
    // notify watcher's debounce (~200 ms) creates a window where a
    // `before_prompt_build` hook firing right after `memory_observe`
    // returns empty results and silently skips injection (#1462).
    // Resource impact: memory_observe is called once per user observation
    // (interactive, low frequency), not in bulk import scenarios. The synchronous
    // reindex is a one-time operation per observation; the notify watcher handles
    // any subsequent updates asynchronously.
    if let Some(ref index) = svc.index {
        // Read the actual file mtime from the filesystem rather than
        // using Utc::now(). The watcher's flush path uses
        // store::mtime_ms_of(&meta) for time-decay scoring; using a
        // synthetic timestamp here would create a mismatch with the
        // subsequent redundant upsert and skew decay-based ranking.
        let mtime_ms = crate::safe_fs::metadata(svc.mount.root_fd.as_fd(), Path::new(&path))
            .map(|m| crate::index::store::mtime_ms_of(&m))
            .unwrap_or_else(|_| Utc::now().timestamp_millis());
        if let Err(e) = index.reindex_file(&path, &body, mtime_ms, n) {
            tracing::warn!("synchronous reindex after observe failed for {path}: {e}");
        }
    }

    svc.audit_log(AuditEntry::new(TOOL).path(path.clone()).bytes(n));
    Ok(path)
}

/// Sanitize a hint string for safe inclusion in frontmatter.
///
/// The frontmatter is read back by hand-rolled line parsers that use
/// `split_once(": ")` + `trim_matches('"')`. They do NOT interpret YAML
/// escapes, so we must not apply YAML double-quote or backslash escaping.
///
/// Strategy:
/// - Replace newlines (\n, \r) with spaces to prevent premature
///   termination of the `---` frontmatter block marker.
/// - Replace other ASCII control characters (\x00-\x1F) with spaces.
/// - Keep all other characters unchanged, including `#`, `:`, `"`, and `\\`.
fn sanitize_hint(hint: &str) -> String {
    hint.chars()
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
    fn memory_type_parse_valid() {
        assert_eq!(MemoryType::parse("user").unwrap(), MemoryType::User);
        assert_eq!(MemoryType::parse("Feedback").unwrap(), MemoryType::Feedback);
        assert_eq!(MemoryType::parse("PROJECT").unwrap(), MemoryType::Project);
        assert_eq!(
            MemoryType::parse("reference").unwrap(),
            MemoryType::Reference
        );
    }

    #[test]
    fn memory_type_parse_invalid() {
        assert!(MemoryType::parse("architecture").is_err());
        assert!(MemoryType::parse("bug").is_err());
        assert!(MemoryType::parse("").is_err());
    }

    #[test]
    fn memory_type_display() {
        assert_eq!(MemoryType::User.to_string(), "user");
        assert_eq!(MemoryType::Feedback.to_string(), "feedback");
        assert_eq!(MemoryType::Project.to_string(), "project");
        assert_eq!(MemoryType::Reference.to_string(), "reference");
    }

    #[test]
    fn memory_type_serialize() {
        let json = serde_json::to_string(&MemoryType::Feedback).unwrap();
        assert_eq!(json, r#""feedback""#);
    }

    #[test]
    fn memory_type_deserialize() {
        let mt: MemoryType = serde_json::from_str(r#""project""#).unwrap();
        assert_eq!(mt, MemoryType::Project);
    }

    // ── sanitize_hint tests ──────────────────────────────────────

    #[test]
    fn sanitize_hint_preserves_hashes() {
        // Hashes are safe: the hand-rolled reader uses split_once(": ")
        // and does not treat '#' as a YAML comment.
        let result = sanitize_hint("fix #123 and #456");
        assert_eq!(result, "fix #123 and #456");
    }

    #[test]
    fn sanitize_hint_preserves_colons() {
        // The reader splits on the first ": " (key-value delimiter),
        // not on subsequent colons within the value.
        let result = sanitize_hint("note: this has a colon");
        assert_eq!(result, "note: this has a colon");
    }

    #[test]
    fn sanitize_hint_preserves_quotes_and_backslashes() {
        // Double-quotes and backslashes must pass through unchanged.
        // YAML-style escaping would be asymmetric because the reader
        // only does trim_matches('"'), never unescapes \\ or \".
        let result = sanitize_hint("she said \"hello\"");
        assert_eq!(result, "she said \"hello\"");
        let result2 = sanitize_hint("C:\\Users\\admin");
        assert_eq!(result2, "C:\\Users\\admin");
    }

    #[test]
    fn sanitize_hint_replaces_newlines() {
        // Newlines break the frontmatter block marker ("---").
        let result = sanitize_hint("line1\nline2");
        assert_eq!(result, "line1 line2");
    }

    #[test]
    fn sanitize_hint_crlf_becomes_two_spaces() {
        // \r\n: \r and \n each become a space, yielding two spaces.
        let result = sanitize_hint("line1\nline2\r\nline3");
        assert_eq!(result, "line1 line2  line3");
    }

    #[test]
    fn sanitize_hint_empty() {
        let result = sanitize_hint("");
        assert_eq!(result, "");
    }

    #[test]
    fn sanitize_hint_control_chars() {
        // ASCII control characters (\x00-\x1F, except \n/\r) become spaces.
        let result = sanitize_hint("pre\x00mid\x1Fpost");
        assert_eq!(result, "pre mid post");
    }

    #[test]
    fn sanitize_hint_normal_text() {
        let result = sanitize_hint("a simple observation note");
        assert_eq!(result, "a simple observation note");
    }

    // ── Round-trip test: write then read with real parser ─────────

    #[test]
    fn hint_round_trips_through_real_reader() {
        // Verify that hints written by memory_observe survive the
        // actual read path (parse_frontmatter_flat in user_profile.rs
        // and memory_sovereignty.rs). This catches asymmetric escaping
        // that unit tests on the writer alone would miss.
        fn parse_frontmatter_flat(content: &str) -> std::collections::HashMap<String, String> {
            let mut fm = std::collections::HashMap::new();
            if let Some(rest) = content.strip_prefix("---\n") {
                if let Some(end) = rest.find("\n---") {
                    for line in rest[..end].lines() {
                        if let Some((key, value)) = line.split_once(": ") {
                            let key = key.trim();
                            let value = value.trim().trim_matches('"');
                            if !key.starts_with(' ') && !key.starts_with('-') {
                                fm.insert(key.to_string(), value.to_string());
                            }
                        }
                    }
                }
            }
            fm
        }

        // NOTE: the hand-rolled reader uses trim_matches('"') which
        // strips leading/trailing quotes unconditionally. This is a reader
        // limitation, not a writer bug: hints containing internal quotes
        // will lose a trailing quote. Sanitize_hint() avoids making this
        // worse with YAML escaping (which also broke backslashes).
        let cases: &[(&str, &str)] = &[
            // (hint_input, expected_readback)
            ("fix #123", "fix #123"),
            ("note: colon here", "note: colon here"),
            // Quotes: trim_matches('"') strips trailing " as it was
            // designed for fully-quoted values like "plain hint", not
            // for values with internal quotes. This is a known reader
            // quirk; sanitize_hint() does not introduce escaping that
            // would compound the issue.
            ("she said \"hello\"", "she said \"hello"),
            ("C:\\Users\\admin", "C:\\Users\\admin"),
            ("line1\nline2", "line1 line2"),
            ("plain hint", "plain hint"),
        ];

        for (hint_input, expected) in cases {
            let safe = sanitize_hint(hint_input);
            let frontmatter = format!(
                "---\ntype: user\nhint: {}\nsource: manual-observe\ncreated_at: 2026-01-01T00:00:00Z\n---\n\nbody",
                safe
            );
            let parsed = parse_frontmatter_flat(&frontmatter);
            let got = parsed.get("hint").expect("hint key missing");
            assert_eq!(
                got, expected,
                "round-trip failed: input '{}' -> sanitized '{}' -> parsed '{}'",
                hint_input, safe, got
            );
        }
    }
}
