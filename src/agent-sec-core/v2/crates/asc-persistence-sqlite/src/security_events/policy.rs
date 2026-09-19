//! The fire-and-forget fault policy for the security-event stream.
//!
//! Migrated from v1 `security_events/sqlite_writer.py`. Every fault is
//! swallowed, and every fault that reached the repository is reported once
//! through a [`DropSink`] so that a surge of dropped security events stays
//! observable even when nothing reaches stderr.

use asc_security_events::SecurityEvent;
use asc_sqlite_kernel::{Fault, FaultPolicy, Outcome, Phase, WriteFault};

/// One dropped-write diagnostic.
///
/// Field names and the two message strings are taken verbatim from v1
/// `_log_drop`, because the v1 record is what existing log consumers parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteDrop {
    /// `"sqlite busy dropped security event"` or
    /// `"sqlite write dropped security event"`.
    pub message: &'static str,
    /// Always `"security_event_sqlite_write"`.
    pub action: &'static str,
    /// Trace id, reported at the top level in v1 rather than inside `data`.
    pub trace_id: String,
    /// `event.category`.
    pub category: String,
    /// Rendered error text.
    pub error: String,
    /// Error class name, mirroring v1 `type(original).__name__`.
    pub error_type: String,
    /// `event.event_id`.
    pub event_id: String,
    /// `event.event_type`.
    pub event_type: String,
    /// One of `insert` / `corruption_disabled` / `corruption_retry` / `io`.
    pub phase: &'static str,
}

/// v1 `_log_drop`'s `data.action`.
const DROP_ACTION: &str = "security_event_sqlite_write";
/// v1's message when the drop was caused by a busy database.
const BUSY_MESSAGE: &str = "sqlite busy dropped security event";
/// v1's message for every other drop.
const WRITE_MESSAGE: &str = "sqlite write dropped security event";
/// v1 raises `RuntimeError("sqlite write was skipped")` for a skipped insert.
const SKIPPED_ERROR: &str = "sqlite write was skipped";
/// The class name v1 logs for that synthetic error.
const SKIPPED_ERROR_TYPE: &str = "RuntimeError";

/// Where drop diagnostics go.
///
/// v1 routes them through the `agent_sec_cli` logger tree into `cli.jsonl`. v2
/// has no equivalent facility yet, so the destination is injected: production
/// code can pass a stderr sink, and tests can capture and assert the records.
pub trait DropSink: Send + Sync {
    /// Reports one dropped write. Must not panic and must not raise.
    fn on_drop(&self, drop: &WriteDrop);
}

impl<F: Fn(&WriteDrop) + Send + Sync> DropSink for F {
    fn on_drop(&self, drop: &WriteDrop) {
        self(drop);
    }
}

/// A [`DropSink`] that emits one diagnostic to the host tracing subscriber.
/// The daemon routes it to bounded stderr output; the legacy type name is retained.
///
/// Only the phase, error type and message are printed; never the event payload.
#[derive(Debug, Default, Clone, Copy)]
pub struct StderrDropSink;

impl DropSink for StderrDropSink {
    fn on_drop(&self, drop: &WriteDrop) {
        tracing::warn!(target: "asc_process_diagnostic",
            "[security_events] {} phase={} error_type={} error={}",
            drop.message, drop.phase, drop.error_type, drop.error
        );
    }
}

/// v1's terminal strategy for the security-event stream.
pub struct SecurityEventsFaultPolicy<S: DropSink> {
    sink: S,
}

impl<S: DropSink> SecurityEventsFaultPolicy<S> {
    /// Builds a policy reporting drops to `sink`.
    pub const fn new(sink: S) -> Self {
        Self { sink }
    }

    /// Returns the drop sink.
    pub const fn sink(&self) -> &S {
        &self.sink
    }
}

impl Default for SecurityEventsFaultPolicy<StderrDropSink> {
    fn default() -> Self {
        Self::new(StderrDropSink)
    }
}

impl<S: DropSink> FaultPolicy for SecurityEventsFaultPolicy<S> {
    type Record = SecurityEvent;

    /// Swallows every fault, reporting all but the pre-insert disabled check.
    ///
    /// v1 returns from `write()` *before* the try block when the store is
    /// disabled, so that one case produces no diagnostic at all. Everything else
    /// gets exactly one record. The connection is torn down only for an I/O
    /// fault and for a non-busy corruption retry — a busy retry keeps it, since
    /// the database is healthy and merely contended.
    fn on_fault(&self, fault: &Fault<'_>, record: &SecurityEvent) -> Outcome {
        if fault.kind == WriteFault::Disabled && fault.phase == Phase::Insert {
            return Outcome::swallow();
        }

        self.sink.on_drop(&drop_for(fault, record));

        let dispose = match fault.kind {
            WriteFault::Io => true,
            _ if fault.phase == Phase::CorruptionRetry => !fault.busy,
            _ => false,
        };
        if dispose {
            Outcome::dispose()
        } else {
            Outcome::swallow()
        }
    }
}

/// Renders the v1 `_log_drop` payload for one fault.
fn drop_for(fault: &Fault<'_>, record: &SecurityEvent) -> WriteDrop {
    let (error, error_type) = match fault.error {
        Some(err) => (err.to_string(), err.error_type().to_owned()),
        // v1 synthesizes a RuntimeError for the "insert wrote nothing" path.
        None => (SKIPPED_ERROR.to_owned(), SKIPPED_ERROR_TYPE.to_owned()),
    };
    WriteDrop {
        message: if fault.busy {
            BUSY_MESSAGE
        } else {
            WRITE_MESSAGE
        },
        action: DROP_ACTION,
        trace_id: record.trace_id.clone(),
        category: record.category.clone(),
        error,
        error_type,
        event_id: record.event_id.clone(),
        event_type: record.event_type.clone(),
        phase: fault.phase.as_str(),
    }
}

impl<S: DropSink> std::fmt::Debug for SecurityEventsFaultPolicy<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecurityEventsFaultPolicy")
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use asc_sqlite_kernel::KernelError;
    use serde_json::Map;

    #[derive(Default)]
    struct Recorder {
        drops: Mutex<Vec<WriteDrop>>,
    }

    impl DropSink for Recorder {
        fn on_drop(&self, drop: &WriteDrop) {
            self.drops
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(drop.clone());
        }
    }

    fn event() -> SecurityEvent {
        let mut event = SecurityEvent::new("sandbox_prehook", "exec", Map::new());
        event.event_id = "evt-1".to_owned();
        "trace-1".clone_into(&mut event.trace_id);
        event
    }

    fn policy() -> SecurityEventsFaultPolicy<Recorder> {
        SecurityEventsFaultPolicy::new(Recorder::default())
    }

    fn fault(kind: WriteFault, phase: Phase, busy: bool) -> Fault<'static> {
        Fault {
            kind,
            phase,
            busy,
            error: None,
        }
    }

    fn drops(policy: &SecurityEventsFaultPolicy<Recorder>) -> Vec<WriteDrop> {
        policy
            .sink()
            .drops
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    #[test]
    fn every_outcome_is_swallowed() {
        let policy = policy();
        for kind in [
            WriteFault::Disabled,
            WriteFault::Skipped,
            WriteFault::Busy,
            WriteFault::Schema,
            WriteFault::Database,
            WriteFault::Corruption,
            WriteFault::Io,
            WriteFault::Malformed,
        ] {
            let outcome = policy.on_fault(&fault(kind, Phase::Insert, false), &event());
            assert_eq!(outcome.failure, None, "{kind:?} must never surface");
        }
    }

    #[test]
    fn the_pre_insert_disabled_check_logs_nothing() {
        let policy = policy();
        policy.on_fault(&fault(WriteFault::Disabled, Phase::Insert, false), &event());
        assert!(
            drops(&policy).is_empty(),
            "v1 returns before the try block, so there is no diagnostic"
        );
    }

    #[test]
    fn a_skipped_insert_logs_the_synthetic_runtime_error() {
        let policy = policy();
        policy.on_fault(&fault(WriteFault::Skipped, Phase::Insert, false), &event());

        let recorded = drops(&policy);
        assert_eq!(recorded.len(), 1);
        let drop = &recorded[0];
        assert_eq!(drop.message, WRITE_MESSAGE);
        assert_eq!(drop.action, DROP_ACTION);
        assert_eq!(drop.phase, "insert");
        assert_eq!(drop.error, SKIPPED_ERROR);
        assert_eq!(drop.error_type, SKIPPED_ERROR_TYPE);
        assert_eq!(drop.trace_id, "trace-1");
        assert_eq!(drop.category, "exec");
        assert_eq!(drop.event_id, "evt-1");
        assert_eq!(drop.event_type, "sandbox_prehook");
    }

    #[test]
    fn a_busy_fault_switches_the_message() {
        let policy = policy();
        policy.on_fault(&fault(WriteFault::Busy, Phase::Insert, true), &event());
        assert_eq!(drops(&policy)[0].message, BUSY_MESSAGE);
    }

    #[test]
    fn an_io_fault_disposes() {
        let policy = policy();
        let err = KernelError::io(
            "write to",
            std::path::Path::new("/nowhere/db"),
            std::io::Error::other("disk gone"),
        );
        let outcome = policy.on_fault(
            &Fault {
                kind: WriteFault::Io,
                phase: Phase::Io,
                busy: false,
                error: Some(&err),
            },
            &event(),
        );
        assert!(outcome.dispose);

        let drop = &drops(&policy)[0];
        assert_eq!(drop.phase, "io");
        assert_eq!(drop.error_type, "OSError");
        assert!(drop.error.contains("disk gone"));
    }

    #[test]
    fn a_busy_corruption_retry_keeps_the_connection() {
        let policy = policy();
        let outcome = policy.on_fault(
            &fault(WriteFault::Busy, Phase::CorruptionRetry, true),
            &event(),
        );
        assert!(
            !outcome.dispose,
            "a contended but healthy database must keep its connection"
        );
        assert_eq!(drops(&policy)[0].phase, "corruption_retry");
    }

    #[test]
    fn a_malformed_corruption_retry_disposes() {
        let policy = policy();
        let outcome = policy.on_fault(
            &fault(WriteFault::Malformed, Phase::CorruptionRetry, false),
            &event(),
        );
        assert!(
            outcome.dispose,
            "this is the rung where the two v1 flows disagree"
        );
    }

    #[test]
    fn corruption_disabled_reports_but_does_not_dispose() {
        let policy = policy();
        let outcome = policy.on_fault(
            &fault(WriteFault::Corruption, Phase::CorruptionDisabled, false),
            &event(),
        );
        assert!(!outcome.dispose);
        assert_eq!(drops(&policy)[0].phase, "corruption_disabled");
    }

    #[test]
    fn schema_and_database_faults_keep_the_connection() {
        let policy = policy();
        for kind in [WriteFault::Schema, WriteFault::Database] {
            let outcome = policy.on_fault(&fault(kind, Phase::Insert, false), &event());
            assert!(!outcome.dispose, "{kind:?} must not tear down the engine");
        }
    }

    #[test]
    fn a_closure_can_serve_as_a_drop_sink() {
        let seen = Mutex::new(0_usize);
        let policy = SecurityEventsFaultPolicy::new(|_: &WriteDrop| {
            *seen
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) += 1;
        });
        policy.on_fault(&fault(WriteFault::Skipped, Phase::Insert, false), &event());
        assert_eq!(
            *seen
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            1
        );
    }
}
