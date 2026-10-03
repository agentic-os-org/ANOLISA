//! User profile synthesis — Dreaming V3 "background memory synthesis" equivalent.
//!
//! Scans historical session logs and consolidated facts to build a structured
//! user profile with three dimensions (inspired by Dreaming V3):
//! - **Preferences**: recurring behavioral patterns (coding style, tool choices)
//! - **Constraints**: project rules and boundaries (language, framework, OS)
//! - **Context**: ongoing work and focus areas (active files, topics)
//!
//! The synthesized profile is stored as `.anolisa/user-profile.toml` and
//! can be queried via the `mem_dream` MCP tool.

use std::collections::HashMap;
use std::path::Path;

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::audit::AuditEntry;
use crate::error::Result;
use crate::service::MemoryService;

/// Synthesized user profile.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct UserProfile {
    /// When this profile was last synthesized.
    pub synthesized_at: String,
    /// Number of sessions analyzed.
    pub sessions_analyzed: usize,
    /// Total tool calls analyzed.
    pub tool_calls_analyzed: usize,
    /// Recurring preferences (e.g. "prefers concise code", "uses vim keybindings").
    pub preferences: Vec<ProfileEntry>,
    /// Project constraints (e.g. "Rust 1.89", "Linux only").
    pub constraints: Vec<ProfileEntry>,
    /// Ongoing context (active files, topics, projects).
    pub context: Vec<ProfileEntry>,
}

/// A single profile entry with evidence count.
#[derive(Debug, Serialize, Deserialize)]
pub struct ProfileEntry {
    pub description: String,
    /// How many times this pattern was observed.
    pub evidence_count: usize,
    /// When this was last observed.
    pub last_seen: String,
}

/// Synthesize a user profile from session logs and consolidated facts.
pub fn synthesize_profile(svc: &MemoryService) -> Result<UserProfile> {
    let mut profile = UserProfile {
        synthesized_at: Utc::now().to_rfc3339(),
        ..Default::default()
    };

    // Phase 1: Analyze session logs from .anolisa/session-logs/
    let session_logs_dir = svc.mount.meta_dir.join("session-logs");
    if session_logs_dir.exists() {
        analyze_session_logs(&session_logs_dir, &mut profile)?;
    }

    // Phase 2: Analyze consolidated facts
    let facts_dir = svc.mount.root.join("facts");
    if facts_dir.exists() {
        analyze_facts(&facts_dir, &mut profile)?;
    }

    // Phase 3: Analyze observed notes
    let notes_dir = svc.mount.root.join("notes").join("observed");
    if notes_dir.exists() {
        analyze_notes(&notes_dir, &mut profile)?;
    }

    // Sort each dimension by evidence_count descending
    profile
        .preferences
        .sort_by_key(|e| std::cmp::Reverse(e.evidence_count));
    profile
        .constraints
        .sort_by_key(|e| std::cmp::Reverse(e.evidence_count));
    profile
        .context
        .sort_by_key(|e| std::cmp::Reverse(e.evidence_count));

    // Truncate to top 20 each
    profile.preferences.truncate(20);
    profile.constraints.truncate(20);
    profile.context.truncate(20);

    // Write profile to .anolisa/user-profile.toml
    let profile_path = svc.mount.meta_dir.join("user-profile.toml");
    let content = toml::to_string_pretty(&profile)
        .map_err(|e| crate::error::MemoryError::Other(format!("serialize profile: {e}")))?;
    std::fs::write(&profile_path, &content)?;

    svc.audit_log(
        AuditEntry::new("mem_dream")
            .path(format!(
                "{} preferences, {} constraints, {} context",
                profile.preferences.len(),
                profile.constraints.len(),
                profile.context.len()
            ))
            .bytes(content.len() as u64),
    );

    Ok(profile)
}

/// Analyze session log files for behavioral patterns.
fn analyze_session_logs(dir: &Path, profile: &mut UserProfile) -> Result<()> {
    let mut tool_frequency: HashMap<String, usize> = HashMap::new();
    let mut search_topics: HashMap<String, usize> = HashMap::new();
    let mut edited_files: HashMap<String, usize> = HashMap::new();

    for entry in std::fs::read_dir(dir)? {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        profile.sessions_analyzed += 1;

        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        for line in content.lines() {
            let entry: serde_json::Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            profile.tool_calls_analyzed += 1;

            let tool = entry["tool"].as_str().unwrap_or("");
            let path_str = entry["path"].as_str().unwrap_or("");

            // Track tool frequency
            *tool_frequency.entry(tool.to_string()).or_insert(0) += 1;

            // Track search topics
            if tool == "memory_search" || tool == "mem_grep" {
                // Extract query from path field (format: "mode:query")
                if let Some(query) = path_str.split(':').nth(1) {
                    let query = query.trim().to_lowercase();
                    if query.len() > 3 {
                        *search_topics.entry(query).or_insert(0) += 1;
                    }
                }
            }

            // Track edited files
            if tool == "mem_edit" || tool == "mem_write" {
                if !path_str.is_empty() {
                    *edited_files.entry(path_str.to_string()).or_insert(0) += 1;
                }
            }
        }
    }

    // Extract preferences from tool usage patterns
    for (tool, count) in &tool_frequency {
        if *count >= 5 {
            profile.preferences.push(ProfileEntry {
                description: format!("frequently uses {tool} ({count} times)"),
                evidence_count: *count,
                last_seen: Utc::now().to_rfc3339(),
            });
        }
    }

    // Extract context from recurring search topics
    for (topic, count) in &search_topics {
        if *count >= 2 {
            profile.context.push(ProfileEntry {
                description: format!("interested in: {topic}"),
                evidence_count: *count,
                last_seen: Utc::now().to_rfc3339(),
            });
        }
    }

    // Extract context from frequently edited files
    for (file, count) in &edited_files {
        if *count >= 3 {
            profile.context.push(ProfileEntry {
                description: format!("actively working on: {file}"),
                evidence_count: *count,
                last_seen: Utc::now().to_rfc3339(),
            });
        }
    }

    Ok(())
}

/// Analyze consolidated facts for profile dimensions.
fn analyze_facts(dir: &Path, profile: &mut UserProfile) -> Result<()> {
    for category_entry in std::fs::read_dir(dir)? {
        let category_entry = match category_entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let ft = match category_entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if !ft.is_dir() {
            continue;
        }
        let category = category_entry.file_name().to_string_lossy().to_string();

        let dir_entries = match std::fs::read_dir(category_entry.path()) {
            Ok(d) => d,
            Err(_) => continue,
        };
        for file_entry in dir_entries {
            let file_entry = match file_entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let path = file_entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };

            // Parse frontmatter for category classification
            let fm = parse_frontmatter_flat(&content);
            let fact_category = fm.get("category").cloned().unwrap_or(category.clone());

            // Route to appropriate profile dimension
            match fact_category.as_str() {
                "lesson" | "change" => {
                    // Lessons and changes → preferences (behavioral patterns)
                    let body = extract_body(&content);
                    if !body.is_empty() {
                        let preview: String = body.chars().take(100).collect();
                        profile.preferences.push(ProfileEntry {
                            description: preview,
                            evidence_count: 1,
                            last_seen: fm.get("created_at").cloned().unwrap_or_default(),
                        });
                    }
                }
                "interest" | "working-context" => {
                    // Interests and working context → context dimension
                    let body = extract_body(&content);
                    if !body.is_empty() {
                        let preview: String = body.chars().take(100).collect();
                        profile.context.push(ProfileEntry {
                            description: preview,
                            evidence_count: 1,
                            last_seen: fm.get("created_at").cloned().unwrap_or_default(),
                        });
                    }
                }
                "promoted" | "summary" => {
                    // Promoted items and summaries → constraints (important decisions)
                    let body = extract_body(&content);
                    if !body.is_empty() {
                        let preview: String = body.chars().take(100).collect();
                        profile.constraints.push(ProfileEntry {
                            description: preview,
                            evidence_count: 1,
                            last_seen: fm.get("created_at").cloned().unwrap_or_default(),
                        });
                    }
                }
                _ => {}
            }
        }
    }

    Ok(())
}

/// Analyze observed notes for additional context.
fn analyze_notes(dir: &Path, profile: &mut UserProfile) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let fm = parse_frontmatter_flat(&content);
        let hint = fm.get("hint").cloned().unwrap_or_default();

        // Route by hint
        match hint.to_lowercase().as_str() {
            "preference" | "style" | "convention" => {
                let body = extract_body(&content);
                let preview: String = body.chars().take(100).collect();
                if !preview.is_empty() {
                    profile.preferences.push(ProfileEntry {
                        description: preview,
                        evidence_count: 1,
                        last_seen: fm.get("created_at").cloned().unwrap_or_default(),
                    });
                }
            }
            "decision" | "architecture" | "constraint" => {
                let body = extract_body(&content);
                let preview: String = body.chars().take(100).collect();
                if !preview.is_empty() {
                    profile.constraints.push(ProfileEntry {
                        description: preview,
                        evidence_count: 1,
                        last_seen: fm.get("created_at").cloned().unwrap_or_default(),
                    });
                }
            }
            _ => {
                let body = extract_body(&content);
                let preview: String = body.chars().take(80).collect();
                if !preview.is_empty() {
                    profile.context.push(ProfileEntry {
                        description: preview,
                        evidence_count: 1,
                        last_seen: fm.get("created_at").cloned().unwrap_or_default(),
                    });
                }
            }
        }
    }

    Ok(())
}

fn parse_frontmatter_flat(content: &str) -> HashMap<String, String> {
    let mut fm = HashMap::new();
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

fn extract_body(content: &str) -> String {
    if let Some(rest) = content.strip_prefix("---\n") {
        if let Some(end) = rest.find("\n---\n") {
            return rest[end + 5..].trim().to_string();
        }
    }
    content.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_frontmatter_basic() {
        let content = "---\ncategory: lesson\nhint: preference\n---\nBody text";
        let fm = parse_frontmatter_flat(content);
        assert_eq!(fm.get("category").unwrap(), "lesson");
        assert_eq!(fm.get("hint").unwrap(), "preference");
    }

    #[test]
    fn extract_body_with_frontmatter() {
        let content = "---\nid: abc\n---\n\nThis is the body.";
        let body = extract_body(content);
        assert_eq!(body, "This is the body.");
    }

    #[test]
    fn extract_body_without_frontmatter() {
        let content = "Just plain text.";
        let body = extract_body(content);
        assert_eq!(body, "Just plain text.");
    }

    #[test]
    fn profile_entry_serialize() {
        let entry = ProfileEntry {
            description: "test".into(),
            evidence_count: 5,
            last_seen: "2026-06-11T00:00:00Z".into(),
        };
        let toml_str = toml::to_string(&entry).unwrap();
        assert!(toml_str.contains("test"));
        assert!(toml_str.contains("5"));
    }
}
