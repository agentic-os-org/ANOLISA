//! The raising fault policy for the observability stream.
//!
//! Migrated from v1 `observability/sqlite_writer.py::write_or_raise`. Every
//! fault surfaces; the three `OSError` messages are wire text for anything that
//! matches on them, so they are reproduced verbatim.

use asc_observability::ObservabilityRecord;
use asc_sqlite_kernel::{Fault, FaultPolicy, Outcome, Phase, WriteFault};

/// v1's message when the store is permanently disabled.
pub const DISABLED_MESSAGE: &str = "observability SQLite store is disabled";
/// v1's message when the database is contended.
pub const BUSY_MESSAGE: &str = "observability SQLite database is busy";
/// v1's message when the insert wrote nothing.
pub const SKIPPED_MESSAGE: &str = "observability SQLite write was skipped";

/// v1's terminal strategy for the observability stream.
#[derive(Debug, Default, Clone, Copy)]
pub struct ObservabilityFaultPolicy;

impl FaultPolicy for ObservabilityFaultPolicy {
    type Record = ObservabilityRecord;

    /// Surfaces every fault, disposing only for genuine I/O or driver faults.
    ///
    /// The dispose rules are where this differs from the security-event policy:
    ///
    /// * A malformed record propagates untouched — the repository never reached
    ///   `SQLite`, so tearing down the connection would punish a caller bug.
    /// * A **busy** corruption retry keeps the connection (as in the other
    ///   stream), but a **malformed** corruption retry keeps it too, whereas the
    ///   security-event stream disposes. This is the one genuinely asymmetric
    ///   rung in v1.
    fn on_fault(&self, fault: &Fault<'_>, _record: &ObservabilityRecord) -> Outcome {
        match fault.kind {
            // Both the pre-insert check and the post-rebuild check use the same
            // v1 message, so the phase does not change the text.
            WriteFault::Disabled | WriteFault::Corruption => {
                Outcome::fail(DISABLED_MESSAGE.to_owned())
            }
            WriteFault::Skipped => Outcome::fail(SKIPPED_MESSAGE.to_owned()),
            WriteFault::Busy => Outcome::fail(BUSY_MESSAGE.to_owned()),
            // A caller bug never disposes, on either attempt. Schema drift also
            // re-raises untouched: the kernel already scheduled the repair.
            WriteFault::Malformed | WriteFault::Schema => Outcome::propagate(),
            WriteFault::Io => Outcome::propagate().with_dispose(),
            WriteFault::Database => {
                if fault.phase == Phase::CorruptionRetry {
                    Outcome::propagate().with_dispose()
                } else {
                    Outcome::propagate()
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use asc_observability::{ObservabilityHook, ObservabilityMetadata};
    use asc_sqlite_kernel::{Failure, KernelError};
    use chrono::{FixedOffset, TimeZone};
    use serde_json::{Map, json};

    use super::*;

    fn record() -> ObservabilityRecord {
        let hook = ObservabilityHook::BeforeAgentRun;
        let first = hook.metric_names()[0];
        let mut metrics = Map::new();
        metrics.insert(first.to_owned(), json!("value"));
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

    fn fault(kind: WriteFault, phase: Phase, busy: bool) -> Fault<'static> {
        Fault {
            kind,
            phase,
            busy,
            error: None,
        }
    }

    fn message(outcome: &Outcome) -> Option<&str> {
        match &outcome.failure {
            Some(Failure::Message(text)) => Some(text.as_str()),
            _ => None,
        }
    }

    #[test]
    fn every_fault_surfaces() {
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
            let outcome =
                ObservabilityFaultPolicy.on_fault(&fault(kind, Phase::Insert, false), &record());
            assert!(
                outcome.failure.is_some(),
                "{kind:?} must never be swallowed here"
            );
        }
    }

    #[test]
    fn the_three_messages_match_v1_verbatim() {
        let policy = ObservabilityFaultPolicy;
        assert_eq!(
            message(&policy.on_fault(
                &fault(WriteFault::Disabled, Phase::Insert, false),
                &record()
            )),
            Some(DISABLED_MESSAGE)
        );
        assert_eq!(
            message(&policy.on_fault(&fault(WriteFault::Busy, Phase::Insert, true), &record())),
            Some(BUSY_MESSAGE)
        );
        assert_eq!(
            message(&policy.on_fault(&fault(WriteFault::Skipped, Phase::Insert, false), &record())),
            Some(SKIPPED_MESSAGE)
        );
    }

    #[test]
    fn the_post_rebuild_disabled_check_reuses_the_disabled_message() {
        let outcome = ObservabilityFaultPolicy.on_fault(
            &fault(WriteFault::Corruption, Phase::CorruptionDisabled, false),
            &record(),
        );
        assert_eq!(message(&outcome), Some(DISABLED_MESSAGE));
        assert!(!outcome.dispose);
    }

    #[test]
    fn a_malformed_record_propagates_without_disposing() {
        for phase in [Phase::Insert, Phase::CorruptionRetry] {
            let outcome = ObservabilityFaultPolicy
                .on_fault(&fault(WriteFault::Malformed, phase, false), &record());
            assert_eq!(outcome.failure, Some(Failure::Original));
            assert!(
                !outcome.dispose,
                "a caller bug must not tear down the connection ({phase:?})"
            );
        }
    }

    #[test]
    fn a_busy_corruption_retry_keeps_the_connection() {
        let outcome = ObservabilityFaultPolicy.on_fault(
            &fault(WriteFault::Busy, Phase::CorruptionRetry, true),
            &record(),
        );
        assert_eq!(message(&outcome), Some(BUSY_MESSAGE));
        assert!(!outcome.dispose);
    }

    #[test]
    fn a_failed_corruption_retry_disposes_only_for_a_database_fault() {
        let outcome = ObservabilityFaultPolicy.on_fault(
            &fault(WriteFault::Database, Phase::CorruptionRetry, false),
            &record(),
        );
        assert!(outcome.dispose);

        let first_attempt = ObservabilityFaultPolicy.on_fault(
            &fault(WriteFault::Database, Phase::Insert, false),
            &record(),
        );
        assert!(
            !first_attempt.dispose,
            "the first attempt re-raises without touching the pool"
        );
    }

    #[test]
    fn an_io_fault_disposes_and_propagates() {
        let err = KernelError::io(
            "write to",
            std::path::Path::new("/nowhere/db"),
            std::io::Error::other("disk gone"),
        );
        let outcome = ObservabilityFaultPolicy.on_fault(
            &Fault {
                kind: WriteFault::Io,
                phase: Phase::Io,
                busy: false,
                error: Some(&err),
            },
            &record(),
        );
        assert!(outcome.dispose);
        assert_eq!(outcome.failure, Some(Failure::Original));
    }

    #[test]
    fn schema_drift_propagates_without_disposing() {
        let outcome = ObservabilityFaultPolicy
            .on_fault(&fault(WriteFault::Schema, Phase::Insert, false), &record());
        assert_eq!(outcome.failure, Some(Failure::Original));
        assert!(!outcome.dispose);
    }
}
