//! Phase 6.5: optional fan-out to systemd-journald.
//!
//! When `[memory.audit].journald = true`, every AuditEntry is sent to
//! journald in addition to the durable on-disk `audit.log`. journald in
//! turn forwards to auditd via its standard rules, which is more
//! portable than punching auditctl from inside a user-namespace.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

use libsystemd::logging::{Priority, journal_send};

use crate::audit::AuditEntry;

/// SYSLOG_IDENTIFIER value for `journalctl -t agent-memory`.
const SYSLOG_IDENTIFIER: &str = "agent-memory";

/// Set after the first send failure so operators see one warning per
/// process lifetime instead of either (a) zero feedback or (b) a flood
/// of warns on every audit entry when the socket is permanently down.
static SEND_FAILURE_WARNED: AtomicBool = AtomicBool::new(false);

/// Best-effort startup probe: send one Info entry so operators get an
/// early warning if journald=true is configured but the socket is
/// unreachable. Subsequent fanout() failures will be debug-level and
/// the warn-once latch silences the rest.
pub fn probe() {
    let pairs: [(&str, &str); 2] = [
        ("SYSLOG_IDENTIFIER", SYSLOG_IDENTIFIER),
        ("ANOLISA_PROBE", "startup"),
    ];
    if let Err(e) = journal_send(
        Priority::Info,
        "agent-memory journald audit fan-out enabled",
        pairs.iter().map(|&(k, v)| (k, v)),
    ) {
        // First-time failure: warn loudly so operators don't silently
        // lose the journald stream they explicitly enabled.
        tracing::warn!("journald probe failed: {e} — fan-out may not reach journalctl");
        SEND_FAILURE_WARNED.store(true, Ordering::Relaxed);
    }
}

/// Best-effort: send `entry` to journald. The durable on-disk audit log
/// is the source of truth; transient send failures are demoted to
/// `debug!` after the first one is warned about by `probe()` /
/// the warn-once latch below.
pub fn fanout(entry: &AuditEntry) {
    let mut fields: HashMap<&str, String> = HashMap::new();
    fields.insert("ANOLISA_TOOL", entry.tool.to_string());
    if !entry.path.is_empty() {
        fields.insert("ANOLISA_PATH", entry.path.clone());
    }
    if let Some(b) = entry.bytes {
        fields.insert("ANOLISA_BYTES", b.to_string());
    }
    fields.insert("ANOLISA_OK", entry.ok.to_string());
    if let Some(ref e) = entry.error {
        fields.insert("ANOLISA_ERROR", e.clone());
    }
    if let Some(ref t) = entry.trace_id {
        fields.insert("ANOLISA_TRACE_ID", t.clone());
    }
    fields.insert("SYSLOG_IDENTIFIER", SYSLOG_IDENTIFIER.into());

    let priority = if entry.ok {
        Priority::Info
    } else {
        Priority::Warning
    };
    let summary = if entry.ok {
        format!("{} {}", entry.tool, entry.path)
    } else {
        format!(
            "{} {} FAILED: {}",
            entry.tool,
            entry.path,
            entry.error.as_deref().unwrap_or("(no message)")
        )
    };
    let pairs: Vec<(&str, &str)> = fields.iter().map(|(k, v)| (*k, v.as_str())).collect();
    if let Err(e) = journal_send(priority, &summary, pairs.into_iter()) {
        // Warn once per process lifetime, then go silent — we mustn't
        // flood the foreground tracing pipe on a sustained outage.
        if !SEND_FAILURE_WARNED.swap(true, Ordering::Relaxed) {
            tracing::warn!(
                "journald fan-out failed: {e} — switching to debug-level for subsequent entries"
            );
        } else {
            tracing::debug!("journald fan-out drop: {e}");
        }
    }
}
