//! Shared helpers for this crate's unit tests.
//!
//! Every test in this crate touches process-wide state, so they must not run
//! concurrently with each other even though the suite stays on the default
//! parallel harness. [`serial`] is that gate.

use std::sync::{Mutex, MutexGuard, PoisonError};

use asc_observability::{ObservabilityHook, ObservabilityMetadata, ObservabilityRecord};
use asc_security_events::SecurityEvent;
use chrono::{FixedOffset, TimeZone};
use serde_json::{Map, json};
use tempfile::TempDir;

static SINK_STATE: Mutex<()> = Mutex::new(());

/// Serializes access to the process-wide slots.
///
/// A poisoned lock is recovered rather than propagated: the guarded data is the
/// unit value, so an earlier panic cannot have left it inconsistent.
pub fn serial() -> MutexGuard<'static, ()> {
    SINK_STATE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Returns a fresh temporary directory for sink files.
pub fn temp_dir() -> TempDir {
    TempDir::new().expect("temp dir")
}

/// Builds a security event with a fixed `event_id`.
pub fn event(id: &str) -> SecurityEvent {
    let mut event = SecurityEvent::new("sandbox_prehook", "exec", Map::new());
    id.clone_into(&mut event.event_id);
    event
}

/// Builds an observability record for session `s-1` / run `r-1`.
pub fn record() -> ObservabilityRecord {
    let hook = ObservabilityHook::BeforeAgentRun;
    let mut metrics = Map::new();
    metrics.insert(hook.metric_names()[0].to_owned(), json!("x"));
    let observed_at = FixedOffset::east_opt(0)
        .expect("utc offset")
        .with_ymd_and_hms(2026, 1, 1, 0, 0, 0)
        .single()
        .expect("timestamp");
    ObservabilityRecord::new(
        hook,
        observed_at,
        ObservabilityMetadata::new("s-1", "r-1"),
        metrics,
    )
    .expect("record")
}
