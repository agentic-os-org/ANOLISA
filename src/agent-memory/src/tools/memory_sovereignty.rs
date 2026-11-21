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

/// The state a present-but-uninterpretable consent file is already enforcing.
///
/// Both gates deny while the file cannot be interpreted, so repairing one scope
/// must not grant the other by starting from default-allow. Only an absent file
/// keeps the documented default-allow, through `Err(NotFound)` above.
fn repairable_config(config: Option<ConsentConfig>) -> ConsentConfig {
    config.unwrap_or(ConsentConfig {
        auto_consolidation: ConsentAction::Deny,
        auto_capture: ConsentAction::Deny,
    })
}

/// Set or query memory consent preferences.
pub fn memory_consent(
    svc: &MemoryService,
    action: Option<&str>,
    scope: Option<&str>,
) -> Result<String> {
    // Load existing config
    let consent_rel = std::path::Path::new(".anolisa/consent.toml");
    let config = match crate::safe_fs::read_to_string(svc.mount.root_fd.as_fd(), consent_rel) {
        // `None` keeps "present but uninterpretable" distinct from "absent":
        // a query must not report allow for a file it could not read, while a
        // mutation can still repair one.
        //
        // The invariant is about a file that carries *no* content, not about a
        // per-key default: each field is `#[serde(default)]` and defaults to
        // allow, so a file that spells out one scope leaves the other at allow.
        // Narrowing that to "an omitted key means deny" would change behaviour
        // for every store that sets a single scope, so it is deliberately not
        // part of this change.
        Ok(content) if content.trim().is_empty() => None,
        Ok(content) => toml::from_str::<ConsentConfig>(&content).ok(),
        Err(MemoryError::NotFound(_)) => Some(ConsentConfig::default()),
        Err(e) => return Err(e),
    };

    let action = action.unwrap_or("query");

    match action {
        "query" => {
            let Some(config) = config.as_ref() else {
                return Err(MemoryError::InvalidArgument(
                    "cannot report consent: .anolisa/consent.toml is empty or not valid TOML"
                        .into(),
                ));
            };
            let json = serde_json::to_string_pretty(config)
                .map_err(|e| MemoryError::Other(format!("serialize: {e}")))?;
            Ok(format!("Current consent settings:\n{json}"))
        }
        "allow" => {
            let mut config = repairable_config(config);
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
            // A mutation may repair an uninterpretable file; only reporting it
            // is refused.
            let mut config = repairable_config(config);
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
    // Sandbox consistency (openat2 + RESOLVE_BENEATH) is not enough here:
    // `safe_fs::write` truncates before it writes, and an empty file parses as
    // `#[serde(default)]` allow, so a writer killed between truncate and write
    // would turn a recorded deny into a permanent allow. Stage beside the
    // destination and rename, so the file is either the old consent or the new
    // one. A file that has to be created still fails closed if it tears.
    let rel = std::path::Path::new(".anolisa/consent.toml");
    let root_fd = svc.mount.root_fd.as_fd();
    let meta_dir = std::path::Path::new(".anolisa");
    // No `create_dir_all` here: `.anolisa` is created when the mount opens, and
    // both helpers below open their parent through openat2 with
    // RESOLVE_BENEATH|RESOLVE_NO_SYMLINKS, so a symlinked `.anolisa` already
    // fails there. An unsandboxed mkdir would be the only write syscall in this
    // path that follows symlinks, and the probe that would guard it leaves a
    // window between the probe and the call.
    match crate::safe_fs::write_create_new(root_fd, rel, content.as_bytes()) {
        Err(MemoryError::AlreadyExists(_)) => {
            crate::safe_fs::write_atomic(root_fd, meta_dir, rel, content.as_bytes())?;
        }
        result => {
            result?;
        }
    }
    Ok(())
}

/// Read capture consent for each hook invocation, including changes by other clients.
pub(super) fn capture_allowed(svc: &MemoryService) -> Result<bool> {
    // OpenClaw owns a long-lived MCP child; users may change consent through
    // another client. Its cached settings cannot authorize the next capture.
    let path = std::path::Path::new(".anolisa/consent.toml");
    let content = match crate::safe_fs::read_to_string(svc.mount.root_fd.as_fd(), path) {
        Ok(content) => content,
        Err(MemoryError::NotFound(_)) => return Ok(true),
        Err(e) => return Err(e),
    };
    if content.trim().is_empty() {
        // Checked before parsing, as `is_source_allowed` and `memory_consent`
        // both do: an empty document parses fine, so the ordering is what keeps
        // the rule visible to the next edit.
        // A zero-length file is a truncating writer's intermediate state, not a
        // decision. `#[serde(default)]` would read it as allow, which would
        // turn a killed write into a revoked-consent bypass.
        return Err(MemoryError::InvalidArgument(
            "cannot read capture consent from .anolisa/consent.toml: file is empty".into(),
        ));
    }
    let config: ConsentConfig = toml::from_str(&content).map_err(|_| {
        // TOML diagnostics include source text, which must not reach the MCP caller.
        MemoryError::InvalidArgument(
            "cannot read capture consent from .anolisa/consent.toml: invalid TOML".into(),
        )
    })?;
    Ok(config.auto_capture.is_allowed())
}

/// Check whether a given source is allowed by consent config.
///
/// Reads the persisted file on every call instead of caching it. Consent is
/// shared per user store, and another client can change it while this process
/// keeps running; a cache would honour a revoked preference until restart, and
/// would keep a revoked-then-restored one disabled for the same reason.
pub fn is_source_allowed(svc: &MemoryService, source: &str) -> bool {
    // Manual sources never consult consent, so a preference file that cannot be
    // interpreted must not start blocking them either.
    if !matches!(source, "auto-consolidation" | "auto-capture") {
        return true;
    }
    // An absent file is the documented default-allow. A file that is present
    // but cannot be read or parsed is not an absent preference, so it denies.
    // Read through the sandbox anchor like capture_allowed does: a consent.toml
    // symlinked to a file outside the mount must fail closed here too, instead
    // of authorising consolidation off a file the user never wrote.
    let rel = std::path::Path::new(".anolisa/consent.toml");
    let config = match crate::safe_fs::read_to_string(svc.mount.root_fd.as_fd(), rel) {
        Ok(content) => {
            // Empty is not "no preference": this rejects a file with no content
            // at all, not one that omits a key -- an omitted key falls back to
            // `#[serde(default)]` = allow. See the note in `memory_consent`.
            if content.trim().is_empty() {
                return false;
            }
            match toml::from_str::<ConsentConfig>(&content) {
                Ok(config) => config,
                Err(_) => return false,
            }
        }
        Err(MemoryError::NotFound(_)) => ConsentConfig::default(),
        Err(_) => return false,
    };

    match source {
        "auto-consolidation" => config.auto_consolidation.is_allowed(),
        _ => config.auto_capture.is_allowed(),
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

    /// A service whose consent cache is empty, rooted in a throwaway store.
    fn service_in(dir: &std::path::Path) -> MemoryService {
        let mut config = crate::config::AppConfig::default();
        config.memory.paths.base_dir = dir.to_string_lossy().to_string();
        config.memory.index.enabled = false;
        config.memory.git.enabled = false;
        config.memory.session.base_dir = dir.join("sessions").to_string_lossy().to_string();
        MemoryService::new(config).unwrap()
    }

    #[test]
    fn unreadable_consent_denies_automatic_sources() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = service_in(tmp.path());
        let consent_path = svc.mount.meta_dir.join("consent.toml");
        fs::create_dir_all(&consent_path).unwrap();

        // A directory in place of the file fails to read the same way a
        // permission error does, and fails identically for root.
        assert!(!is_source_allowed(&svc, "auto-consolidation"));
        assert!(!is_source_allowed(&svc, "auto-capture"));
        assert!(is_source_allowed(&svc, "manual-observe"));

        // Repairing the file takes effect on the next call: the unreadable
        // state is not cached.
        fs::remove_dir_all(&consent_path).unwrap();
        fs::write(&consent_path, "auto_consolidation = \"deny\"\n").unwrap();
        assert!(!is_source_allowed(&svc, "auto-consolidation"));
        assert!(is_source_allowed(&svc, "manual-observe"));
    }

    #[test]
    fn an_empty_consent_file_denies_instead_of_allowing() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = service_in(tmp.path());
        fs::create_dir_all(&svc.mount.meta_dir).unwrap();
        // Zero bytes is what a truncating writer leaves behind, and
        // `#[serde(default)]` reads that as allow through every path.
        fs::write(svc.mount.meta_dir.join("consent.toml"), "").unwrap();

        assert!(!is_source_allowed(&svc, "auto-consolidation"));
        assert!(!is_source_allowed(&svc, "auto-capture"));
        assert!(capture_allowed(&svc).is_err());
        assert!(memory_consent(&svc, Some("query"), None).is_err());
        // A mutation may still repair the file.
        assert!(memory_consent(&svc, Some("deny"), Some("all")).is_ok());
        assert!(!is_source_allowed(&svc, "auto-consolidation"));
    }

    #[test]
    fn an_update_publishes_by_rename_not_truncation() {
        use std::os::unix::fs::MetadataExt;

        let tmp = tempfile::tempdir().unwrap();
        let svc = service_in(tmp.path());
        fs::create_dir_all(&svc.mount.meta_dir).unwrap();
        let path = svc.mount.meta_dir.join("consent.toml");

        assert!(memory_consent(&svc, Some("allow"), Some("all")).is_ok());
        let before = fs::metadata(&path).unwrap().ino();

        assert!(memory_consent(&svc, Some("deny"), Some("all")).is_ok());
        let after = fs::metadata(&path).unwrap().ino();

        // A truncating write reuses the inode and leaves a window in which the
        // file is empty; a staged rename swaps in a different inode and never
        // exposes a partial body. This is what keeps a torn write from reading
        // as consent.
        assert_ne!(
            before, after,
            "an update replaced the contents in place; a reader can observe an empty file"
        );
        assert!(!is_source_allowed(&svc, "auto-consolidation"));
    }

    #[test]
    fn repairing_one_scope_does_not_grant_the_other() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = service_in(tmp.path());
        fs::create_dir_all(&svc.mount.meta_dir).unwrap();
        // Present but uninterpretable: both gates deny, and the repair path
        // must start from that state rather than from default-allow.
        fs::write(
            svc.mount.meta_dir.join("consent.toml"),
            "auto_consolidation = nope",
        )
        .unwrap();
        assert!(!is_source_allowed(&svc, "auto-consolidation"));

        assert!(memory_consent(&svc, Some("deny"), Some("capture")).is_ok());

        // The caller asked to turn capture off; consolidation was already off
        // and must stay off, not be written back as an affirmative allow.
        assert!(!is_source_allowed(&svc, "auto-consolidation"));
        assert!(!is_source_allowed(&svc, "auto-capture"));
    }

    #[test]
    fn a_single_scope_repair_of_an_empty_file_keeps_the_other_denied() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = service_in(tmp.path());
        fs::create_dir_all(&svc.mount.meta_dir).unwrap();
        fs::write(svc.mount.meta_dir.join("consent.toml"), "").unwrap();

        assert!(memory_consent(&svc, Some("allow"), Some("capture")).is_ok());

        assert!(capture_allowed(&svc).is_ok());
        assert!(!is_source_allowed(&svc, "auto-consolidation"));
    }

    #[test]
    fn symlinked_consent_file_is_never_followed() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = service_in(tmp.path());
        fs::create_dir_all(&svc.mount.meta_dir).unwrap();
        // Anyone able to plant a link in the store can point the consent file
        // at a permissive file of their own; reading through it would grant
        // automatic consolidation and capture without the user's decision.
        let outside = tmp.path().join("outside-consent.toml");
        fs::write(
            &outside,
            "auto_consolidation = \"allow\"\nauto_capture = \"allow\"\n",
        )
        .unwrap();
        std::os::unix::fs::symlink(&outside, svc.mount.meta_dir.join("consent.toml")).unwrap();

        assert!(!is_source_allowed(&svc, "auto-consolidation"));
        assert!(!is_source_allowed(&svc, "auto-capture"));
        assert!(is_source_allowed(&svc, "manual-observe"));
        assert!(matches!(
            memory_consent(&svc, Some("query"), Some("consolidation")),
            Err(MemoryError::PathOutsideMount(_))
        ));
        assert!(capture_allowed(&svc).is_err());
    }

    #[test]
    fn invalid_consent_toml_denies_consolidation_until_repaired() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = service_in(tmp.path());
        fs::create_dir_all(&svc.mount.meta_dir).unwrap();
        let consent_path = svc.mount.meta_dir.join("consent.toml");
        fs::write(&consent_path, "auto_consolidation = nope\n").unwrap();

        assert!(!is_source_allowed(&svc, "auto-consolidation"));
        assert!(is_source_allowed(&svc, "manual-observe"));

        fs::write(&consent_path, "auto_consolidation = \"allow\"\n").unwrap();
        assert!(is_source_allowed(&svc, "auto-consolidation"));
    }

    #[test]
    fn healthy_consent_is_honoured() {
        let tmp = tempfile::tempdir().unwrap();
        let svc = service_in(tmp.path());
        fs::create_dir_all(&svc.mount.meta_dir).unwrap();
        fs::write(
            svc.mount.meta_dir.join("consent.toml"),
            "auto_consolidation = \"deny\"\nauto_capture = \"allow\"\n",
        )
        .unwrap();

        assert!(!is_source_allowed(&svc, "auto-consolidation"));
        assert!(is_source_allowed(&svc, "auto-capture"));
    }

    #[test]
    fn another_client_consent_change_is_honoured() {
        let tmp = tempfile::tempdir().unwrap();
        // Two services over one store, as two clients of the same user would be.
        let first = service_in(tmp.path());
        let second = service_in(tmp.path());

        // Healthy control: consent granted through one is visible to the other.
        memory_consent(&first, Some("allow"), Some("consolidation")).unwrap();
        assert!(is_source_allowed(&second, "auto-consolidation"));

        // The reported defect: a revocation written by the second client stayed
        // invisible to the first, which kept writing automatic facts.
        memory_consent(&second, Some("deny"), Some("consolidation")).unwrap();
        assert!(
            !is_source_allowed(&first, "auto-consolidation"),
            "a revocation written by another client must be honoured"
        );

        // The reverse transition used to leave consolidation disabled for good.
        memory_consent(&second, Some("allow"), Some("consolidation")).unwrap();
        assert!(is_source_allowed(&first, "auto-consolidation"));
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
