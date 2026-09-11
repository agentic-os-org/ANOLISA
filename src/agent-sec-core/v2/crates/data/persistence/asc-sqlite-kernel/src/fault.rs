//! Fault classification and the injected terminal policy.
//!
//! The kernel decides **what happened**; the policy decides **how to respond**.
//! v1 writes the same eight-step decision tree twice — once fire-and-forget in
//! `security_events/sqlite_writer.py`, once raising in
//! `observability/sqlite_writer.py` — and the two copies differ only in the
//! terminal action. Here the tree exists once, in [`crate::sink`], and the
//! difference is expressed by a [`FaultPolicy`].
//!
//! Four asymmetries between the two v1 flows must stay visible in the policies
//! rather than being smoothed over by the abstraction:
//!
//! 1. **`Malformed` placement.** `security_events` swallows `ValueError`/`TypeError`
//!    inside `repository.insert`, so a malformed record looks like a skipped
//!    write; observability calls `insert_or_raise` and lets it through.
//! 2. **Dispose after a failed corruption retry.** Both flows skip the dispose
//!    when the retry failed *busy*; they differ for a malformed record, where
//!    `security_events` still disposes and `observability` does not.
//! 3. **Write lock.** Only `security_events` serializes writes.
//! 4. **`prune` clock.** Only observability accepts an injected `now` in v1; the
//!    kernel trait takes it for both.

use crate::error::KernelError;

/// What the kernel observed while trying to persist one record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteFault {
    /// The store is permanently disabled after failed corruption cleanup.
    Disabled,
    /// The insert reported that nothing was written.
    Skipped,
    /// `SQLite` stayed busy or locked past the busy timeout.
    Busy,
    /// A repairable schema drift; the kernel already requested a repair.
    Schema,
    /// Any other `SQLite` error that is neither busy, schema drift, nor corruption.
    ///
    /// v1 reaches this through the `DatabaseError` branch that falls past every
    /// specific check. It is distinct from [`WriteFault::Io`] because v1 does
    /// **not** dispose the engine for it.
    Database,
    /// True database corruption.
    Corruption,
    /// A filesystem or driver level fault.
    Io,
    /// The record itself was rejected before `SQLite` was touched.
    Malformed,
}

/// Where in the pipeline the fault surfaced.
///
/// The values map one-to-one onto the `phase` strings v1 puts in its drop
/// diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// First insert attempt. v1 `phase="insert"`.
    Insert,
    /// Corruption cleanup left the store disabled. v1 `phase="corruption_disabled"`.
    CorruptionDisabled,
    /// The post-corruption retry failed. v1 `phase="corruption_retry"`.
    CorruptionRetry,
    /// Filesystem or driver fault. v1 `phase="io"`.
    Io,
}

impl Phase {
    /// Returns the v1 `phase` string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Insert => "insert",
            Self::CorruptionDisabled => "corruption_disabled",
            Self::CorruptionRetry => "corruption_retry",
            Self::Io => "io",
        }
    }
}

/// One fault handed to a [`FaultPolicy`].
#[derive(Debug)]
pub struct Fault<'a> {
    /// What happened.
    pub kind: WriteFault,
    /// Where it happened.
    pub phase: Phase,
    /// Whether the underlying error was busy/locked.
    ///
    /// Carried separately because v1 passes `busy=` into `_log_drop` and uses it
    /// to decide whether to dispose after a failed retry.
    pub busy: bool,
    /// The underlying error, when there was one.
    pub error: Option<&'a KernelError>,
}

/// How the caller should fail, if at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// Fail with this exact message, mirroring v1's `raise OSError("...")`.
    Message(String),
    /// Fail with the original error, mirroring a bare `raise`.
    Original,
}

/// The terminal decision for one fault.
///
/// Kept as a struct rather than an enum because the two decisions are
/// independent in v1: whether to tear down the connection, and whether the
/// caller sees an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// Whether to drop the cached connection.
    pub dispose: bool,
    /// Whether and how to surface the fault.
    pub failure: Option<Failure>,
}

impl Outcome {
    /// Swallows the fault and keeps the connection.
    #[must_use]
    pub const fn swallow() -> Self {
        Self {
            dispose: false,
            failure: None,
        }
    }

    /// Swallows the fault but drops the connection.
    #[must_use]
    pub const fn dispose() -> Self {
        Self {
            dispose: true,
            failure: None,
        }
    }

    /// Fails with `message`.
    #[must_use]
    pub fn fail(message: impl Into<String>) -> Self {
        Self {
            dispose: false,
            failure: Some(Failure::Message(message.into())),
        }
    }

    /// Fails with the original error.
    #[must_use]
    pub const fn propagate() -> Self {
        Self {
            dispose: false,
            failure: Some(Failure::Original),
        }
    }

    /// Returns a copy that also drops the connection.
    #[must_use]
    pub fn with_dispose(mut self) -> Self {
        self.dispose = true;
        self
    }
}

/// The per-stream terminal strategy.
///
/// The record is handed back to the policy because v1's `_log_drop` reports
/// `event_id` / `event_type` / `category` / `trace_id` alongside the error. The
/// kernel stays domain neutral: the record type is an associated type it never
/// inspects.
pub trait FaultPolicy: Send + Sync {
    /// The record type the paired repository persists.
    type Record;

    /// Decides what to do about `fault` while persisting `record`.
    fn on_fault(&self, fault: &Fault<'_>, record: &Self::Record) -> Outcome;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_strings_match_v1() {
        assert_eq!(Phase::Insert.as_str(), "insert");
        assert_eq!(Phase::CorruptionDisabled.as_str(), "corruption_disabled");
        assert_eq!(Phase::CorruptionRetry.as_str(), "corruption_retry");
        assert_eq!(Phase::Io.as_str(), "io");
    }

    #[test]
    fn outcome_constructors_compose() {
        assert_eq!(
            Outcome::swallow(),
            Outcome {
                dispose: false,
                failure: None
            }
        );
        assert_eq!(Outcome::swallow().with_dispose(), Outcome::dispose());
        assert_eq!(
            Outcome::fail("boom").failure,
            Some(Failure::Message("boom".to_owned()))
        );
        assert_eq!(Outcome::propagate().failure, Some(Failure::Original));
        assert!(Outcome::propagate().with_dispose().dispose);
    }
}
