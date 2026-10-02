//! Memory sovereignty tools — user control over what the system remembers.
//!
//! Inspired by Dreaming V3's user memory sovereignty design and EU AI Act
//! transparency requirements. 96% of ChatGPT memories are auto-created
//! without user knowledge — these tools give users visibility and control.
//!
//! Tools:
//! - `memory_about`: "What do you remember about X?" — semantic search
//! - `memory_forget`: "Forget about X" — search + delete + audit
//! - `memory_auto_created`: List all auto-created memories for review
//! - `memory_consent`: Set auto-memory preferences

use std::collections::HashMap;
use std::fs;
use std::os::fd::AsFd;

use serde::Serialize;
use walkdir::WalkDir;

use crate::audit::AuditEntry;
use crate::error::{MemoryError, Result};
use crate::service::MemoryService;

// ── memory_about ────────────────────────────────────────────────

/// Search for all memories related to a topic.
pub fn memory_about(svc: &MemoryService, topic: &str, limit: usize) -> Result<String> {
    let index = svc
        .index
        .as_ref()
        .ok_or_else(|| MemoryError::NotImplemented("index required for memory_about"))?;

    let hits = index.search(topic, limit.max(1))?;

    if hits.is_empty() {
        return Ok(format!("I have no memories about '{topic}'."));
    }

    let mut out = format!("I found {} memories about '{}':\n\n", hits.len(), topic);
    for (i, hit) in hits.iter().enumerate() {
        out.push_str(&format!(
            "{}. **{}** (score: {:.2})\n",
            i + 1,
            hit.path,
            hit.score
        ));
        if !hit.snippet.is_empty() {
            let preview: String = hit.snippet.chars().take(200).collect();
            out.push_str(&format!("   {preview}\n"));
        }
        out.push('\n');
    }

    svc.audit_log(
        AuditEntry::new("memory_about")
            .path(topic.to_string())
            .bytes(hits.len() as u64),
    );

    Ok(out)
}

// ── memory_forget ───────────────────────────────────────────────

/// Search and optionally delete memories about a topic.
pub fn memory_forget(svc: &MemoryService, topic: &str, confirm: bool) -> Result<String> {
    let index = svc
        .index
        .as_ref()
        .ok_or_else(|| MemoryError::NotImplemented("index required for memory_forget"))?;

    let hits = index.search(topic, 20)?;

    // BM25 scores are negative (closer to 0 = better match). We rely on the
    // search engine's own ranking (top-20) rather than a secondary score
    // threshold — a multiplicative threshold on negative scores inverts the
    // intended filter direction and can discard all results.
    if hits.is_empty() {
        return Ok(format!(
            "No memories found about '{topic}'. Nothing to forget."
        ));
    }

    if !confirm {
        let preview: Vec<String> = hits.iter().map(|h| h.path.clone()).collect();
        let mut out = format!("Will forget {} memories about '{}':\n\n", hits.len(), topic);
        for path in &preview {
            out.push_str(&format!("  - {path}\n"));
        }
        out.push_str(
            "\nCall again with confirm=true to proceed. \
             Note: the actual deleted files are re-resolved at confirm time \
             to ensure consistency.",
        );
        return Ok(out);
    }

    // Delete each memory file; report what was actually deleted.
    let mut deleted: Vec<String> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    for hit in &hits {
        match svc.remove(&hit.path, false) {
            Ok(()) => {
                svc.audit_log(
                    AuditEntry::new("memory_forget")
                        .path(hit.path.clone())
                        .bytes(0),
                );
                deleted.push(hit.path.clone());
            }
            Err(e) => {
                tracing::warn!("memory_forget: failed to remove {}: {e}", hit.path);
                skipped.push(hit.path.clone());
            }
        }
    }

    let mut out = format!("Forgot {} memories about '{topic}':\n", deleted.len());
    for path in &deleted {
        out.push_str(&format!("  ✓ {path}\n"));
    }
    if !skipped.is_empty() {
        out.push_str(&format!("\nSkipped {} (errors):\n", skipped.len()));
        for path in &skipped {
            out.push_str(&format!("  ✗ {path}\n"));
        }
    }
    Ok(out)
}

// ── memory_auto_created ─────────────────────────────────────────

/// Entry describing an auto-created memory.
#[derive(Debug, Serialize)]
pub struct AutoCreatedEntry {
    pub path: String,
    pub title: String,
    pub category: String,
    pub source: String,
    pub created_at: String,
}

/// List all memories created automatically (by consolidation or auto-capture).
pub fn memory_auto_created(svc: &MemoryService, limit: usize) -> Result<String> {
    let meta_dir = svc.mount.meta_dir.clone();
    let mut entries: Vec<(String, AutoCreatedEntry)> = Vec::new();

    for dir_entry in WalkDir::new(&svc.mount.root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| !e.path().starts_with(&meta_dir))
    {
        let dir_entry = match dir_entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !dir_entry.file_type().is_file() {
            continue;
        }
        let path = dir_entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }

        let content = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let fm = parse_frontmatter_flat(&content);
        let source = fm.get("source").cloned().unwrap_or_default();

        // Only include auto-created memories
        if !source.starts_with("auto-") {
            continue;
        }

        let rel_path = path
            .strip_prefix(&svc.mount.root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();

        let created_at = fm.get("created_at").cloned().unwrap_or_default();
        let title = fm.get("title").cloned().unwrap_or_else(|| rel_path.clone());
        let category = fm
            .get("category")
            .cloned()
            .unwrap_or_else(|| "uncategorized".into());

        entries.push((
            created_at.clone(),
            AutoCreatedEntry {
                path: rel_path,
                title,
                category,
                source,
                created_at,
            },
        ));
    }

    // Sort by created_at descending
    entries.sort_by(|a, b| b.0.cmp(&a.0));
    let entries: Vec<AutoCreatedEntry> = entries.into_iter().take(limit).map(|(_, e)| e).collect();

    svc.audit_log(
        AuditEntry::new("memory_auto_created")
            .path(format!("{} entries", entries.len()))
            .bytes(0),
    );

    if entries.is_empty() {
        return Ok("(no auto-created memories found)".into());
    }

    serde_json::to_string_pretty(&entries)
        .map_err(|e| MemoryError::Other(format!("serialize: {e}")))
}

// ── memory_consent ──────────────────────────────────────────────

/// Consent action: allow or deny auto-memory operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ConsentAction {
    #[default]
    Allow,
    Deny,
}

impl ConsentAction {
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Allow)
    }
}

/// Consent configuration stored in `.anolisa/consent.toml`.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct ConsentConfig {
    /// Whether auto-consolidation is allowed. Default: allow.
    #[serde(default)]
    pub auto_consolidation: ConsentAction,
    /// Whether auto-capture (hook-based) is allowed. Default: allow.
    #[serde(default)]
    pub auto_capture: ConsentAction,
}

impl Default for ConsentConfig {
    fn default() -> Self {
        Self {
            auto_consolidation: ConsentAction::Allow,
            auto_capture: ConsentAction::Allow,
        }
    }
}

/// Set or query memory consent preferences.
pub fn memory_consent(
    svc: &MemoryService,
    action: Option<&str>,
    scope: Option<&str>,
) -> Result<String> {
    let consent_path = svc.mount.meta_dir.join("consent.toml");

    // Load existing config
    let mut config = if consent_path.exists() {
        let content = fs::read_to_string(&consent_path)?;
        toml::from_str::<ConsentConfig>(&content).unwrap_or_default()
    } else {
        ConsentConfig::default()
    };

    let action = action.unwrap_or("query");

    match action {
        "query" => {
            let json = serde_json::to_string_pretty(&config)
                .map_err(|e| MemoryError::Other(format!("serialize: {e}")))?;
            Ok(format!("Current consent settings:\n{json}"))
        }
        "allow" => {
            let scope = scope.unwrap_or("all");
            if !matches!(scope, "all" | "consolidation" | "capture") {
                return Err(MemoryError::InvalidArgument(format!(
                    "unknown scope '{scope}'; expected all, consolidation, or capture"
                )));
            }
            match scope {
                "consolidation" => config.auto_consolidation = ConsentAction::Allow,
                "capture" => config.auto_capture = ConsentAction::Allow,
                _ => {
                    config.auto_consolidation = ConsentAction::Allow;
                    config.auto_capture = ConsentAction::Allow;
                }
            }
            write_consent(&config, svc)?;
            Ok(format!("Consent updated: {scope} = allow"))
        }
        "deny" => {
            let scope = scope.unwrap_or("all");
            if !matches!(scope, "all" | "consolidation" | "capture") {
                return Err(MemoryError::InvalidArgument(format!(
                    "unknown scope '{scope}'; expected all, consolidation, or capture"
                )));
            }
            match scope {
                "consolidation" => config.auto_consolidation = ConsentAction::Deny,
                "capture" => config.auto_capture = ConsentAction::Deny,
                _ => {
                    config.auto_consolidation = ConsentAction::Deny;
                    config.auto_capture = ConsentAction::Deny;
                }
            }
            write_consent(&config, svc)?;
            Ok(format!("Consent updated: {scope} = deny"))
        }
        _ => Err(MemoryError::InvalidArgument(format!(
            "unknown action '{action}'; expected query, allow, or deny"
        ))),
    }
}

fn write_consent(config: &ConsentConfig, svc: &MemoryService) -> Result<()> {
    let content = toml::to_string_pretty(config)
        .map_err(|e| MemoryError::Other(format!("serialize consent: {e}")))?;
    // Write via safe_fs (openat2 + RESOLVE_BENEATH) for sandbox consistency.
    let rel = std::path::Path::new(".anolisa/consent.toml");
    crate::safe_fs::write(svc.mount.root_fd.as_fd(), rel, content.as_bytes())?;

    // Update cache
    let mut cache = svc.consent_cache.lock().unwrap_or_else(|e| e.into_inner());
    *cache = Some(config.clone());

    Ok(())
}

/// Check whether a given source is allowed by consent config.
/// Uses cached consent config from MemoryService for performance.
pub fn is_source_allowed(svc: &MemoryService, source: &str) -> bool {
    // Hold lock throughout to avoid TOCTOU: another thread could modify
    // the consent file between our cache miss and our file read.
    let config = {
        let mut cache = svc.consent_cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(ref cfg) = *cache {
            cfg.clone()
        } else {
            // Not cached yet — load from disk while holding the lock.
            let consent_path = svc.mount.meta_dir.join("consent.toml");
            let config = if consent_path.exists() {
                fs::read_to_string(&consent_path)
                    .ok()
                    .and_then(|c| toml::from_str::<ConsentConfig>(&c).ok())
                    .unwrap_or_default()
            } else {
                ConsentConfig::default()
            };
            *cache = Some(config.clone());
            config
        }
    };

    match source {
        "auto-consolidation" => config.auto_consolidation.is_allowed(),
        "auto-capture" => config.auto_capture.is_allowed(),
        _ => true, // Manual sources are always allowed
    }
}

// ── Helpers ─────────────────────────────────────────────────────

fn parse_frontmatter_flat(content: &str) -> HashMap<String, String> {
    let mut fm = HashMap::new();

    // Handle both LF and CRLF line endings
    let content = content.replace("\r\n", "\n");

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consent_default_is_allow() {
        let config = ConsentConfig::default();
        assert_eq!(config.auto_consolidation, ConsentAction::Allow);
        assert_eq!(config.auto_capture, ConsentAction::Allow);
    }

    #[test]
    fn consent_serialize_roundtrip() {
        let config = ConsentConfig {
            auto_consolidation: ConsentAction::Deny,
            auto_capture: ConsentAction::Allow,
        };
        let toml_str = toml::to_string_pretty(&config).unwrap();
        let parsed: ConsentConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.auto_consolidation, ConsentAction::Deny);
    }

    #[test]
    fn is_source_allowed_default() {
        // Without a consent file, everything is allowed
        let tmp = tempfile::tempdir().unwrap();
        let mut config = crate::config::AppConfig::default();
        config.memory.paths.base_dir = tmp.path().to_string_lossy().to_string();
        config.memory.index.enabled = false;
        config.memory.git.enabled = false;
        config.memory.session.base_dir = tmp.path().join("sessions").to_string_lossy().to_string();
        let svc = MemoryService::new(config).unwrap();
        assert!(is_source_allowed(&svc, "auto-consolidation"));
        assert!(is_source_allowed(&svc, "auto-capture"));
        assert!(is_source_allowed(&svc, "manual-observe"));
    }
}
