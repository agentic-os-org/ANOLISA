//! Atomic lifecycle transactions with rollback support.
//!
//! A [`Transaction`] is a small journal that lifecycle operations
//! (install / enable / disable / uninstall / purge) can plug into to get
//! crash-safe behaviour without each call site re-implementing the
//! "snapshot → mutate → rollback on error" dance.
//!
//! The shape is intentionally concrete:
//!
//! 1. `begin` mints a sortable `operation_id`, snapshots the existing
//!    `state_path` bytes (if any) into a sidecar file under
//!    `journal_dir/<operation_id>.state.snapshot`, and writes an empty
//!    journal file under `journal_dir/<operation_id>.journal.toml` that
//!    references the sidecar by path and sha256.
//! 2. Each meaningful side effect (writing a file, modifying state,
//!    starting a service, …) records a [`TransactionStep`] up front with
//!    `Planned` status. The journal is rewritten atomically (`tmp` →
//!    `rename`) on every change so the file on disk is never half-written.
//! 3. On success the orchestrator calls [`Transaction::mark_done`]; on
//!    failure it calls [`Transaction::mark_failed`] and walks the journal
//!    backwards calling rollback primitives.
//! 4. After a crash, [`Transaction::load_journal`] reads the file back in
//!    so a later `repair` command can finish or rewind the operation.
//!
//! Journal format is TOML (human-greppable, lines up with `installed.toml`
//! and `enable-plan.toml`) and is rewritten in full on every mutation.
//! That full-rewrite strategy is only cheap under two invariants that
//! every caller must preserve:
//!
//! * **The journal never embeds data that grows with the managed state.**
//!   Byte bodies, per-file inventories, or hash lists must live in
//!   sidecar files referenced by path + sha256 (the state snapshot is the
//!   canonical example — schema v1 embedded it as a TOML integer array,
//!   which inflated a multi-MB state file ~9x and was re-serialised on
//!   every step transition, producing near-quadratic I/O on components
//!   with tens of thousands of owned files).
//! * **Step count never scales with the number of owned files.** Steps
//!   are phase-level (`PlaceFiles`, `RemoveOwnedFiles`, …); per-file
//!   progress belongs in an externally referenced inventory, never in
//!   per-file steps.
//!
//! With both invariants held the journal stays KB-sized, steps lists are
//! short (tens of entries per op at most), the rewrite cost is
//! negligible, and a single rewrite-and-rename guarantees the on-disk
//! file always parses.
//!
//! AgentSight is not mentioned anywhere on purpose: this primitive is
//! shared by every component the package manager knows about.

use std::collections::hash_map::DefaultHasher;
use std::fs::{self, File, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::domain::NativePm;

/// Schema version for the transaction journal on disk. Bump on
/// incompatible changes; journals with a *newer* version are reported as
/// [`TransactionError::CorruptJournal`] so callers don't silently
/// mis-parse them, while every version back to
/// [`JOURNAL_SCHEMA_MIN_VERSION`] still loads.
///
/// v2 moved the state snapshot out of the journal into a sidecar file
/// (the [`StateSnapshotRef`] under `state_snapshot_ref` replaces the
/// embedded `state_snapshot` byte array) and made the record mandatory:
/// every v2 journal states either `kind = "absent"` or a
/// `kind = "sidecar"` reference, so a journal that *lost* the record
/// (truncation, hand-editing, bit rot) is distinguishable from one that
/// recorded "no state existed" and is rejected as corrupt at load. The
/// bump is deliberate downgrade protection, not cosmetics: a pre-v2
/// binary reading a v2 journal would see `state_snapshot = None`,
/// interpret it as "the state file did not exist at begin", and *delete*
/// `installed.toml` during recovery.
/// Refusing with `CorruptJournal` fails closed instead.
pub const JOURNAL_SCHEMA_VERSION: u32 = 2;

/// Oldest journal schema [`Transaction::load_journal`] still accepts.
/// v1 journals carry the snapshot embedded in `state_snapshot`;
/// [`Transaction::restore_state`] keeps honouring those bytes so pending
/// v1 journals written by older binaries remain recoverable.
pub const JOURNAL_SCHEMA_MIN_VERSION: u32 = 1;

/// File-name suffix every journal file carries
/// (`<operation_id>.journal.toml`). The single authority for the naming
/// convention: directory scans (journal inventories, tests) must filter
/// on this constant instead of repeating the literal, so a future rename
/// cannot leave a scanner matching stale names.
pub const JOURNAL_FILE_SUFFIX: &str = ".journal.toml";

/// File-name suffix of the state-snapshot sidecar written by
/// [`Transaction::begin`] (`<operation_id>.state.snapshot`). Lives beside
/// the journal in the same directory; scanners looking for journals must
/// not match it (see [`JOURNAL_FILE_SUFFIX`]).
pub const STATE_SNAPSHOT_FILE_SUFFIX: &str = ".state.snapshot";

/// Lifecycle status for a single recorded step.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TransactionStepStatus {
    /// Step was recorded but the side effect has not yet been performed.
    Planned,
    /// Step completed successfully.
    Done,
    /// Step had been `Done` and was reverted by a rollback primitive.
    RolledBack,
    /// Step failed; rollback may still be required for prior `Done` steps.
    Failed,
    /// Step was intentionally skipped (idempotency, preconditions, …).
    Skipped,
}

/// State mutation a delegated operation expects after reconciling the native
/// package authority.
///
/// Recovery records this separately from transaction steps because a merged
/// native transaction names every package in the batch, while each journal
/// belongs to exactly one component and must preserve that component's
/// management relation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DelegatedRecordAction {
    /// Create or replace the record with a managed relation.
    WriteManaged,
    /// Create or replace the record with an adopted relation.
    WriteAdopted,
    /// Create or replace the record with an observed relation.
    WriteObserved,
    /// Refresh an existing delegated record without changing its relation.
    Refresh,
    /// Remove the record after the native or record-only uninstall converges.
    Drop,
}

/// Per-subject recovery identity for a delegated lifecycle operation.
///
/// `TransactionStep::target` remains an audit description of the external
/// side effect and may therefore contain a whole batch. This context is the
/// wire-level contract repair uses to select the one package and record
/// transition belonging to [`Transaction::subject`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DelegatedRecoveryContext {
    /// Native package manager whose database is authoritative.
    pub pm: NativePm,
    /// Resolved package owned by this journal's subject. Record-only drops
    /// of quarantined state may omit it because no native identity was ever
    /// trusted or acted upon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    /// Record transition intended by the interrupted operation.
    pub record_action: DelegatedRecordAction,
    /// Version-pin contract, present only when the interrupted operation
    /// pinned a specific artifact. Persisted so a `repair` after a crash can
    /// validate the native transaction step against the exact NEVRA and refuse
    /// to record a package whose installed EVR/arch does not match the pin —
    /// the durable counterpart to the in-process check, so recovery never
    /// falls back to whatever version happens to be present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned: Option<DelegatedPinnedArtifact>,
}

/// The exact artifact a version-pinned delegated operation resolved to,
/// carried in the journal so crash recovery can reconstruct the pin without
/// re-resolving the repository (which may have advanced in the meantime).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DelegatedPinnedArtifact {
    /// Exact NEVRA the native transaction targeted.
    pub artifact: String,
    /// EVR the freshly installed package must match before the record commits.
    pub evr: String,
    /// Arch the freshly installed package must match.
    pub arch: String,
}

/// Discriminator for rollback strategies. Each variant pairs with the
/// optional fields on [`RollbackAction`] (e.g. `RestoreFile` expects
/// both `source` and `dest`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RollbackActionKind {
    /// Rewrite `state_path` from the snapshot captured at `begin` — the
    /// v2 sidecar referenced by `Transaction::state_snapshot_ref`, or the
    /// legacy v1 bytes embedded in `Transaction::state_snapshot`. See
    /// [`Transaction::restore_state`].
    RestoreState,
    /// Copy bytes from `source` back to `dest`, optionally checked
    /// against `sha256`.
    RestoreFile,
    /// Delete `dest`. The primitive refuses to touch a path that was not
    /// previously recorded by this transaction; see
    /// [`Transaction::remove_file`].
    RemoveFile,
    /// Recreate `dest` as an empty directory (idempotent).
    RecreateDir,
    /// No-op marker — useful for steps that don't need rollback (e.g.
    /// logging, read-only probes).
    None,
}

/// Concrete parameters for a rollback action. Optional fields are
/// populated based on `kind`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RollbackAction {
    /// Strategy selector; determines which optional fields are required.
    pub kind: RollbackActionKind,
    /// Backup/source path used by restore actions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<PathBuf>,
    /// Destination path that rollback will restore, remove, or recreate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dest: Option<PathBuf>,
    /// Expected digest for [`Self::source`] when restore needs verification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

impl RollbackAction {
    /// No-op rollback — convenience for steps that don't need one.
    pub fn none() -> Self {
        Self {
            kind: RollbackActionKind::None,
            source: None,
            dest: None,
            sha256: None,
        }
    }

    /// Rollback that removes a file the transaction created.
    pub fn remove_file(dest: PathBuf) -> Self {
        Self {
            kind: RollbackActionKind::RemoveFile,
            source: None,
            dest: Some(dest),
            sha256: None,
        }
    }

    /// Rollback that copies `source` back over `dest`, optionally
    /// verifying `source`'s SHA256 first.
    pub fn restore_file(source: PathBuf, dest: PathBuf, sha256: Option<String>) -> Self {
        Self {
            kind: RollbackActionKind::RestoreFile,
            source: Some(source),
            dest: Some(dest),
            sha256,
        }
    }
}

/// One row in the journal. `phase` lets the orchestrator tag groups of
/// steps (`"plan"`, `"backup"`, `"materialise"`, `"persist-state"`, …)
/// for nicer diagnostics and replay heuristics.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TransactionStep {
    /// Orchestrator phase label for diagnostics and replay ordering.
    pub phase: String,
    /// Path, object name, or unit affected by the step.
    pub target: String,
    /// Human-readable action label recorded before the side effect runs.
    pub action: String,
    /// Current journal status for this step.
    pub status: TransactionStepStatus,
    /// Rollback primitive to apply if a later step fails.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollback: Option<RollbackAction>,
    /// Optional human-readable note; populated by `mark_failed` /
    /// `mark_skipped` so a recovery tool can render *why* without re-reading
    /// the central log.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl TransactionStep {
    /// Build a step initialised to `Planned` status.
    pub fn planned(
        phase: impl Into<String>,
        target: impl Into<String>,
        action: impl Into<String>,
        rollback: Option<RollbackAction>,
    ) -> Self {
        Self {
            phase: phase.into(),
            target: target.into(),
            action: action.into(),
            status: TransactionStepStatus::Planned,
            rollback,
            note: None,
        }
    }
}

/// Terminal classification of an operation, suitable for CentralLog.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TransactionOutcomeStatus {
    /// `finish` was not (yet) called.
    InFlight,
    /// All recorded steps succeeded or were skipped.
    Ok,
    /// Some step failed; rollback was not performed (or also failed).
    Failed,
    /// Some step failed and prior `Done` steps were rolled back.
    RolledBack,
    /// A mix of `Done` and `Failed` steps with no rollback performed.
    Partial,
}

/// What [`Transaction::begin`] observed at `state_path`, recorded
/// explicitly in every v2 journal.
///
/// An explicit tagged enum — not an `Option` — because the two "nothing
/// here" cases must be distinguishable on disk: `Absent` is a positive
/// assertion that the state file did not exist at `begin` (so rollback
/// deletes it), while a *missing* `state_snapshot_ref` table in a v2
/// journal means the journal lost data (truncation, hand-editing,
/// bit rot) and is rejected as corrupt by [`Transaction::load_journal`]
/// instead of silently deleting `installed.toml` during recovery.
/// The `kind` tag makes each variant self-describing in TOML:
/// `kind = "absent"`, or `kind = "sidecar"` plus both payload fields.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StateSnapshotRef {
    /// The state file did not exist when the transaction began;
    /// rollback restores that by deleting it.
    Absent,
    /// The state file existed; its bytes live in a write-once sidecar.
    /// Both fields are mandatory by shape: the digest is what lets
    /// [`Transaction::restore_state`] refuse a truncated or tampered
    /// sidecar before writing its bytes over the state file, so a path
    /// without a digest is unrepresentable rather than merely invalid.
    Sidecar {
        /// Sidecar file holding the raw `state_path` bytes observed at
        /// `begin`.
        path: PathBuf,
        /// SHA-256 of the sidecar bytes, verified before every restore.
        sha256: String,
    },
}

/// Atomic lifecycle transaction journal.
///
/// One `Transaction` corresponds to one user-facing operation
/// (`enable foo`, `purge bar`, …). It owns its journal file and keeps
/// in-memory state in lockstep with the on-disk file via `persist`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Transaction {
    /// Journal schema version this struct was serialised against.
    #[serde(default = "default_journal_version")]
    pub schema_version: u32,
    /// `op-YYYYMMDDHHMMSS-<6-hex>` — sortable, unique per call, shared
    /// with the rest of the package manager so a journal entry can be
    /// joined against `installed.toml` and the central log.
    pub operation_id: String,
    /// Operation verb. Free-form so future commands can opt in without
    /// schema churn (`install`, `uninstall`, `disable`, `enable`,
    /// `purge`, …).
    pub operation: String,
    /// Object this operation is about (component name), when the caller
    /// declared one. Lets recovery and planning attribute a pending journal
    /// to its subject; journals written before this field exists load as
    /// `None` and are treated as unattributed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// Explicit delegated recovery contract for this subject. Journals
    /// written before this additive field existed load as `None`; repair only
    /// infers their identity when the old step data is unambiguous.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegated_recovery: Option<DelegatedRecoveryContext>,
    /// RFC3339 UTC timestamp captured during `begin`.
    pub started_at: String,
    /// RFC3339 UTC timestamp captured during `finish`. `None` while the
    /// transaction is still in flight.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    /// Path to the state file the snapshot was taken from. Must be the
    /// same path `restore_state` will write back to.
    pub state_path: PathBuf,
    /// Legacy (schema v1) embedded snapshot: bytes of `state_path` as
    /// observed at `begin`. Kept **read-only for deserialising v1
    /// journals** — new journals never populate it, because TOML encodes
    /// `Vec<u8>` as an integer array (~9 bytes per byte) and the whole
    /// journal is rewritten on every step transition, which made journal
    /// I/O scale with the managed state size. `restore_state` still
    /// honours these bytes when present so pending v1 journals recover.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_snapshot: Option<Vec<u8>>,
    /// What `begin` observed at `state_path` (schema v2): either an
    /// explicit [`StateSnapshotRef::Absent`], or a
    /// [`StateSnapshotRef::Sidecar`] written exactly once by `begin` and
    /// read at most once by `restore_state`. `Option` only so v1
    /// journals (which carry [`state_snapshot`](Self::state_snapshot)
    /// instead) can deserialise; [`Transaction::load_journal`] rejects a
    /// v2 journal whose table is missing as corrupt, so a truncated or
    /// hand-edited journal can never masquerade as "state was absent at
    /// begin" and trick rollback into deleting the state file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_snapshot_ref: Option<StateSnapshotRef>,
    /// On-disk location of this journal file.
    pub journal_path: PathBuf,
    /// Recorded steps, ordered by insertion.
    #[serde(default)]
    pub steps: Vec<TransactionStep>,
    /// Overall classification; populated by `finish` and rollback
    /// helpers.
    #[serde(default = "default_outcome_status")]
    pub status: TransactionOutcomeStatus,
}

fn default_journal_version() -> u32 {
    JOURNAL_SCHEMA_VERSION
}

fn default_outcome_status() -> TransactionOutcomeStatus {
    TransactionOutcomeStatus::InFlight
}

/// Errors raised by [`Transaction`] and its rollback primitives.
#[derive(Debug, thiserror::Error)]
pub enum TransactionError {
    /// Filesystem error at the associated path.
    #[error("io error at {0}: {1}")]
    Io(PathBuf, std::io::Error),
    /// Journal content could not be parsed or has an unsupported schema.
    #[error("corrupt journal: {0}")]
    CorruptJournal(String),
    /// A rollback/remove primitive was asked to touch an untracked path.
    #[error("refused to operate on path not tracked by transaction: {0}")]
    UntrackedPath(PathBuf),
    /// Rollback failed after a prior step had already failed.
    #[error("rollback failed: {0}")]
    Rollback(String),
    /// Generic transaction-level failure.
    #[error("transaction failed: {0}")]
    Failed(String),
}

impl Transaction {
    /// Begin a new transaction.
    ///
    /// * `operation` — verb the orchestrator is performing (`install`,
    ///   `enable`, `disable`, `uninstall`, `purge`, …). Stored verbatim.
    /// * `state_path` — path to the state file (`installed.toml` today)
    ///   that the transaction will snapshot. Reading the file is
    ///   non-fatal: a missing file is treated as `state_snapshot = None`
    ///   so first-run installs work.
    /// * `journal_dir` — directory the journal file will be created in.
    ///   The directory is created if it does not exist.
    pub fn begin(
        operation: &str,
        state_path: PathBuf,
        journal_dir: &Path,
    ) -> Result<Self, TransactionError> {
        Self::begin_with_subject(operation, None, state_path, journal_dir)
    }

    /// [`begin`](Self::begin) with a declared subject (component name), so
    /// a pending journal can later be attributed to the object it was
    /// mutating instead of blocking every operation.
    pub fn begin_with_subject(
        operation: &str,
        subject: Option<&str>,
        state_path: PathBuf,
        journal_dir: &Path,
    ) -> Result<Self, TransactionError> {
        let now = Utc::now();
        let operation_id = build_operation_id(operation, &now);
        let started_at = now.to_rfc3339_opts(SecondsFormat::Secs, true);

        // Snapshot the state file. Missing file is OK — first-run case.
        let state_bytes = match fs::read(&state_path) {
            Ok(bytes) => Some(bytes),
            Err(err) if err.kind() == io::ErrorKind::NotFound => None,
            Err(err) => return Err(TransactionError::Io(state_path.clone(), err)),
        };

        if !journal_dir.as_os_str().is_empty() {
            fs::create_dir_all(journal_dir)
                .map_err(|err| TransactionError::Io(journal_dir.to_path_buf(), err))?;
        }

        let journal_path = journal_dir.join(format!("{operation_id}{JOURNAL_FILE_SUFFIX}"));

        // Write the snapshot bytes to a sidecar *before* the journal
        // exists, so a crash between the two writes leaves only a harmless
        // orphan sidecar (no journal references it) and never a journal
        // pointing at a missing snapshot. The write is a hard gate: a
        // transaction whose rollback source cannot be persisted must not
        // start, and falling back to embedding the bytes in the journal
        // would silently reintroduce the size blow-up the sidecar exists
        // to prevent. A missing state file is recorded as an explicit
        // `Absent` — never by omitting the table — so `load_journal` can
        // tell "state did not exist" apart from "journal lost its
        // snapshot record".
        let state_snapshot_ref = match &state_bytes {
            Some(bytes) => {
                let sidecar =
                    journal_dir.join(format!("{operation_id}{STATE_SNAPSHOT_FILE_SUFFIX}"));
                write_atomic(&sidecar, bytes)
                    .map_err(|err| TransactionError::Io(sidecar.clone(), err))?;
                StateSnapshotRef::Sidecar {
                    path: sidecar,
                    sha256: sha256_hex(bytes),
                }
            }
            None => StateSnapshotRef::Absent,
        };

        let tx = Self {
            schema_version: JOURNAL_SCHEMA_VERSION,
            operation_id,
            operation: operation.to_string(),
            subject: subject.map(str::to_string),
            delegated_recovery: None,
            started_at,
            finished_at: None,
            state_path,
            state_snapshot: None,
            state_snapshot_ref: Some(state_snapshot_ref),
            journal_path,
            steps: Vec::new(),
            status: TransactionOutcomeStatus::InFlight,
        };
        tx.persist()?;
        Ok(tx)
    }

    /// Whether this journal records unfinished business: it is still in
    /// flight (crash window) or ended `Partial` (side effects committed
    /// that the record does not reflect). `Ok`, `Failed`, and `RolledBack`
    /// are settled outcomes — nothing is left to recover.
    pub fn is_pending(&self) -> bool {
        matches!(
            self.status,
            TransactionOutcomeStatus::InFlight | TransactionOutcomeStatus::Partial
        )
    }

    /// Persist a delegated recovery contract and its first steps atomically.
    ///
    /// A recovery identity without steps cannot prove which external side
    /// effects may have run. Keeping both in one journal revision prevents a
    /// crash between those writes from turning an incomplete intent into an
    /// apparently recoverable operation. Repeating the same contract may add
    /// later steps; rebinding it to another identity is rejected.
    ///
    /// # Errors
    ///
    /// Returns [`TransactionError::Failed`] when `steps` is empty or the
    /// journal already carries a different contract. Persistence failures
    /// leave the in-memory transaction unchanged.
    pub fn record_delegated_steps(
        &mut self,
        context: DelegatedRecoveryContext,
        steps: impl IntoIterator<Item = TransactionStep>,
    ) -> Result<(), TransactionError> {
        let steps = steps.into_iter().collect::<Vec<_>>();
        if steps.is_empty() {
            return Err(TransactionError::Failed(format!(
                "delegated recovery intent for operation {} has no steps",
                self.operation_id
            )));
        }
        if let Some(existing) = &self.delegated_recovery
            && existing != &context
        {
            return Err(TransactionError::Failed(format!(
                "refused to replace delegated recovery context for operation {}",
                self.operation_id
            )));
        }
        let previous_context = self.delegated_recovery.clone();
        let previous_step_count = self.steps.len();
        self.delegated_recovery.get_or_insert(context);
        self.steps.extend(steps);
        if let Err(err) = self.persist() {
            self.delegated_recovery = previous_context;
            self.steps.truncate(previous_step_count);
            return Err(err);
        }
        Ok(())
    }

    /// Append a step to the journal and persist.
    pub fn record_step(&mut self, step: TransactionStep) -> Result<(), TransactionError> {
        self.steps.push(step);
        self.persist()
    }

    /// Append several steps and persist them as one journal revision.
    ///
    /// Use this when recovery requires the complete initial step contract to
    /// become visible atomically. If the process exits during persistence,
    /// readers observe either the previous journal or the complete batch.
    pub fn record_steps(
        &mut self,
        steps: impl IntoIterator<Item = TransactionStep>,
    ) -> Result<(), TransactionError> {
        self.steps.extend(steps);
        self.persist()
    }

    /// Mark `idx` as [`TransactionStepStatus::Done`] and persist.
    pub fn mark_done(&mut self, idx: usize) -> Result<(), TransactionError> {
        self.set_step_status(idx, TransactionStepStatus::Done, None)
    }

    /// Mark `idx` as [`TransactionStepStatus::Failed`] and persist the
    /// supplied error message under `note` for later diagnostics.
    pub fn mark_failed(&mut self, idx: usize, err: &str) -> Result<(), TransactionError> {
        self.set_step_status(idx, TransactionStepStatus::Failed, Some(err.to_string()))
    }

    /// Mark `idx` as [`TransactionStepStatus::Skipped`] with a reason.
    pub fn mark_skipped(&mut self, idx: usize, reason: &str) -> Result<(), TransactionError> {
        self.set_step_status(
            idx,
            TransactionStepStatus::Skipped,
            Some(reason.to_string()),
        )
    }

    /// Mark `idx` as [`TransactionStepStatus::RolledBack`]. Called by the
    /// orchestrator after a successful `restore_file` / `restore_state`
    /// inside the rollback walk so the journal records the terminal
    /// per-step status (not just `Done`) for forensic reads.
    pub fn mark_rolled_back(&mut self, idx: usize) -> Result<(), TransactionError> {
        self.set_step_status(idx, TransactionStepStatus::RolledBack, None)
    }

    /// Stamp `finished_at` and a terminal `status`, then persist.
    pub fn finish(&mut self, status: TransactionOutcomeStatus) -> Result<(), TransactionError> {
        self.finished_at = Some(Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true));
        self.status = status;
        self.persist()
    }

    /// Restore `state_path` from the snapshot captured at `begin`.
    ///
    /// Sources, checked in order:
    ///
    /// 1. a legacy v1 embedded snapshot (`state_snapshot`) is written back
    ///    verbatim;
    /// 2. a v2 [`StateSnapshotRef::Sidecar`] is read, verified against
    ///    its recorded sha256, and written back. A missing, unreadable,
    ///    or digest-mismatched sidecar is a [`TransactionError::Rollback`]
    ///    — it must **never** fall through to the deletion branch, because
    ///    deleting the state file on a broken sidecar would destroy the
    ///    very state the snapshot exists to protect;
    /// 3. an explicit [`StateSnapshotRef::Absent`] (v2) or nothing at all
    ///    (v1): the pre-op state was "did not exist", so the state file
    ///    is removed. A v2 journal can only reach the "nothing at all"
    ///    case through in-memory construction — [`Self::load_journal`]
    ///    rejects v2 journals without a snapshot record as corrupt.
    ///
    /// All errors are wrapped in [`TransactionError::Rollback`] so callers
    /// can distinguish a rollback failure from the original failure.
    pub fn restore_state(&self) -> Result<(), TransactionError> {
        if let Some(bytes) = &self.state_snapshot {
            return write_atomic(&self.state_path, bytes)
                .map_err(|err| TransactionError::Rollback(err.to_string()));
        }
        if let Some(StateSnapshotRef::Sidecar { path, sha256 }) = &self.state_snapshot_ref {
            let bytes = fs::read(path).map_err(|err| {
                TransactionError::Rollback(format!(
                    "read state snapshot sidecar {}: {err}",
                    path.display()
                ))
            })?;
            let actual = sha256_hex(&bytes);
            if actual != *sha256 {
                return Err(TransactionError::Rollback(format!(
                    "sha256 mismatch restoring state snapshot {}: expected {sha256}, got {actual}",
                    path.display()
                )));
            }
            return write_atomic(&self.state_path, &bytes)
                .map_err(|err| TransactionError::Rollback(err.to_string()));
        }
        match fs::remove_file(&self.state_path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(TransactionError::Rollback(format!(
                "remove {}: {err}",
                self.state_path.display()
            ))),
        }
    }

    /// Remove `path`, but only if the transaction recorded it as a file
    /// it owns.
    ///
    /// The check is deliberately strict: `path` must appear as the
    /// `dest` of at least one step whose status is `Done` or `Planned`
    /// AND whose rollback kind is [`RollbackActionKind::RemoveFile`].
    /// Anything else is rejected with [`TransactionError::UntrackedPath`]
    /// so a buggy caller cannot turn this into `rm -f` for an arbitrary
    /// path. Matching `Planned` lets a forward pass that has not yet
    /// flipped its step to `Done` still call this helper from a `Drop`
    /// guard.
    pub fn remove_file(&self, path: &Path) -> Result<(), TransactionError> {
        let tracked = self.steps.iter().any(|step| match &step.rollback {
            Some(rb) if rb.kind == RollbackActionKind::RemoveFile => {
                rb.dest.as_deref() == Some(path)
                    && matches!(
                        step.status,
                        TransactionStepStatus::Done | TransactionStepStatus::Planned
                    )
            }
            _ => false,
        });
        if !tracked {
            return Err(TransactionError::UntrackedPath(path.to_path_buf()));
        }
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(TransactionError::Io(path.to_path_buf(), err)),
        }
    }

    /// Copy bytes from `rollback.source` to `rollback.dest`. If
    /// `rollback.sha256` is set, the source bytes are verified first;
    /// a mismatch returns [`TransactionError::Rollback`].
    ///
    /// A backup that is itself a symlink (a managed `FileKind::Symlink`
    /// entry backed up as a link) is restored by recreating an identical
    /// link at `dest` — its bytes are never read through, so `sha256` is
    /// not applicable and is ignored.
    ///
    /// A journaled action carries no mode, so the restored file takes the
    /// umask default. In-process compensation restores the observed mode
    /// by calling [`restore_backup_file`] directly.
    pub fn restore_file(&self, rollback: &RollbackAction) -> Result<(), TransactionError> {
        if rollback.kind != RollbackActionKind::RestoreFile {
            return Err(TransactionError::Rollback(format!(
                "restore_file called with {:?}",
                rollback.kind
            )));
        }
        let source = rollback.source.as_ref().ok_or_else(|| {
            TransactionError::Rollback("restore_file: missing source".to_string())
        })?;
        let dest = rollback
            .dest
            .as_ref()
            .ok_or_else(|| TransactionError::Rollback("restore_file: missing dest".to_string()))?;

        restore_backup_file(source, dest, rollback.sha256.as_deref(), None).map(|_| ())
    }

    /// Load a previously-written journal. Returns
    /// [`TransactionError::CorruptJournal`] (not `Failed`) when the file
    /// exists but cannot be parsed, so callers can distinguish a
    /// genuinely broken journal from a missing one.
    pub fn load_journal(path: &Path) -> Result<Self, TransactionError> {
        let bytes = fs::read(path).map_err(|err| TransactionError::Io(path.to_path_buf(), err))?;
        let text = std::str::from_utf8(&bytes).map_err(|err| {
            TransactionError::CorruptJournal(format!("{}: invalid utf-8: {err}", path.display()))
        })?;
        let tx: Self = toml::from_str(text).map_err(|err| {
            TransactionError::CorruptJournal(format!("{}: {err}", path.display()))
        })?;
        if tx.schema_version < JOURNAL_SCHEMA_MIN_VERSION
            || tx.schema_version > JOURNAL_SCHEMA_VERSION
        {
            return Err(TransactionError::CorruptJournal(format!(
                "{}: unsupported journal schema_version {}",
                path.display(),
                tx.schema_version
            )));
        }
        if tx.delegated_recovery.is_some() && tx.steps.is_empty() {
            return Err(TransactionError::CorruptJournal(format!(
                "{}: recovery context has no operation steps",
                path.display()
            )));
        }
        // Version-gated structural checks. From v2 on, `begin` always
        // records what it observed at `state_path` — an explicit `Absent`
        // or a `Sidecar` — so a v2 journal with no snapshot record has
        // lost data (truncation, hand-editing, bit rot). It must not
        // load: `restore_state` would read the gap as "state was absent
        // at begin" and delete the state file it exists to protect. The
        // reverse mixture is equally inconsistent: a v2 journal never
        // embeds snapshot bytes, and a v1 journal cannot carry a v2
        // snapshot record.
        if tx.schema_version >= 2 {
            if tx.state_snapshot_ref.is_none() {
                return Err(TransactionError::CorruptJournal(format!(
                    "{}: v{} journal is missing its state_snapshot_ref record",
                    path.display(),
                    tx.schema_version
                )));
            }
            if tx.state_snapshot.is_some() {
                return Err(TransactionError::CorruptJournal(format!(
                    "{}: v{} journal must not embed state_snapshot bytes",
                    path.display(),
                    tx.schema_version
                )));
            }
        } else if tx.state_snapshot_ref.is_some() {
            return Err(TransactionError::CorruptJournal(format!(
                "{}: v{} journal must not carry a state_snapshot_ref record",
                path.display(),
                tx.schema_version
            )));
        }
        Ok(tx)
    }

    fn set_step_status(
        &mut self,
        idx: usize,
        status: TransactionStepStatus,
        note: Option<String>,
    ) -> Result<(), TransactionError> {
        let len = self.steps.len();
        let step = self.steps.get_mut(idx).ok_or_else(|| {
            TransactionError::Failed(format!("step index {idx} out of range (have {len} steps)"))
        })?;
        step.status = status;
        if note.is_some() {
            step.note = note;
        }
        self.persist()
    }

    /// Rewrite the journal file atomically. We rewrite (rather than
    /// append) on every mutation so the file always parses; step lists
    /// are short enough that this is cheaper than a JSONL append plus
    /// a recovery step.
    fn persist(&self) -> Result<(), TransactionError> {
        if let Some(parent) = self.journal_path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)
                .map_err(|err| TransactionError::Io(parent.to_path_buf(), err))?;
        }
        let content = toml::to_string_pretty(self).map_err(|err| {
            TransactionError::Failed(format!(
                "serialise journal {}: {err}",
                self.journal_path.display()
            ))
        })?;
        write_atomic(&self.journal_path, content.as_bytes())
            .map_err(|err| TransactionError::Io(self.journal_path.clone(), err))
    }
}

/// Restore one backup without following a destination leaf symlink.
///
/// Regular-file bytes are optionally verified and installed with an atomic
/// sibling rename. A symlink backup is recreated as the same link. This is
/// the shared rollback primitive for journaled and in-process compensation.
///
/// Rollback must return the destination to its pre-operation state *mode
/// for mode*, not just byte for byte: the atomic sibling would otherwise
/// land at `0666 & ~umask`, silently stripping the executable bit off a
/// restored binary and leaving the component unusable after a rollback the
/// CLI reported as clean. `mode` is what [`crate::lifecycle::prepare_backup`]
/// observed on the source; `None` restores the bytes and leaves the mode to
/// the umask, as it always did.
///
/// Only the permission bits (`0o777`) are reproduced. Restore writes a new
/// inode owned by the *restoring* process and cannot put back the original
/// uid/gid, so replaying setuid/setgid onto it would mint a setuid binary
/// owned by root that the pre-operation state never had. Those bits are
/// dropped and reported in the returned warnings instead.
///
/// Applying the mode is best-effort: on a filesystem that cannot chmod, the
/// bytes still land and the trouble is returned as a warning. Losing the
/// file to protect its metadata inverts what a rollback is for.
///
/// # Errors
///
/// Returns an IO error when the backup cannot be read or the destination
/// cannot be replaced, and [`TransactionError::Rollback`] when a supplied
/// digest does not match the backup bytes.
pub fn restore_backup_file(
    source: &Path,
    dest: &Path,
    expected_sha256: Option<&str>,
    mode: Option<u32>,
) -> Result<Vec<String>, TransactionError> {
    let meta = fs::symlink_metadata(source)
        .map_err(|err| TransactionError::Io(source.to_path_buf(), err))?;
    if meta.file_type().is_symlink() {
        let referent =
            fs::read_link(source).map_err(|err| TransactionError::Io(source.to_path_buf(), err))?;
        if let Some(parent) = dest.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)
                .map_err(|err| TransactionError::Io(parent.to_path_buf(), err))?;
        }
        match fs::remove_file(dest) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(TransactionError::Io(dest.to_path_buf(), err)),
        }
        std::os::unix::fs::symlink(&referent, dest)
            .map_err(|err| TransactionError::Io(dest.to_path_buf(), err))?;
        return Ok(Vec::new());
    }

    let bytes = fs::read(source).map_err(|err| TransactionError::Io(source.to_path_buf(), err))?;
    if let Some(expected) = expected_sha256 {
        let actual = sha256_hex(&bytes);
        if actual != expected {
            return Err(TransactionError::Rollback(format!(
                "sha256 mismatch restoring {}: expected {expected}, got {actual}",
                source.display()
            )));
        }
    }

    let mut warnings = Vec::new();
    let mode = mode.map(|mode| {
        if mode & 0o7000 != 0 {
            warnings.push(format!(
                "{} was {:04o} before the operation; restoring it as {:03o} \
                 because rollback cannot reproduce its original owner and a \
                 setuid or setgid file owned by the restoring user would grant \
                 more than the original did",
                dest.display(),
                mode,
                mode & 0o777
            ));
        }
        mode & 0o777
    });
    if let Some(err) = write_atomic_with_mode(dest, &bytes, mode)
        .map_err(|err| TransactionError::Rollback(err.to_string()))?
    {
        warnings.push(format!(
            "restored {} but could not set its mode to {:03o}: {err}",
            dest.display(),
            mode.unwrap_or_default()
        ));
    }
    Ok(warnings)
}

/// Mint a fresh operation id outside a journal, in the same format
/// [`Transaction::begin`] uses. Batch orchestrations use this for the parent
/// id that groups their members' per-component operations: the parent never
/// owns a journal (each member journals for itself), but its id must sort
/// and grep exactly like every other id in `installed.toml::operations`.
pub fn mint_operation_id(operation: &str) -> String {
    build_operation_id(operation, &Utc::now())
}

/// `op-YYYYMMDDHHMMSS-<6-hex>` — matches the operation-id format used by
/// the rest of anolisa so journal ids round-trip 1:1 with
/// `installed.toml::operations[].id` and the central log.
fn build_operation_id(operation: &str, now: &DateTime<Utc>) -> String {
    let ts = now.format("%Y%m%d%H%M%S").to_string();
    let nanos = now.timestamp_nanos_opt().unwrap_or_else(|| now.timestamp());
    let mut hasher = DefaultHasher::new();
    nanos.hash(&mut hasher);
    let suffix = hasher.finish() & 0xff_ffff;
    // Embed the operation verb so ids self-classify (op-update-…, op-uninstall-…)
    // and audit-log prefix filters can group by operation.
    format!("op-{operation}-{ts}-{suffix:06x}")
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let out = hasher.finalize();
    let mut s = String::with_capacity(64);
    for b in out {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// `tmp` + `rename` write so a crash mid-write cannot leave a truncated
/// file. Uses the same atomic-write guarantees as `StateStore::save`.
///
/// Security-critical: the tmp sibling is opened with `O_CREAT|O_EXCL`
/// (plus `O_NOFOLLOW` on Unix) by [`open_excl_nofollow`] so a pre-placed
/// `.{file_name}.<...>.tmp` symlink — or any other existing entry at the
/// tmp path — fails the open instead of letting us write through it to a
/// path outside the journal directory. The tmp name itself is salted
/// with the writer's pid, a process-wide monotonic counter and a
/// nanosecond timestamp so two concurrent `record_step` writers on the
/// same operation_id (or a stale tmp left behind by an earlier process)
/// cannot collide on the same path.
fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    // Journal and state writes take whatever the umask gives them; only
    // the rollback path has a mode it must reproduce.
    write_atomic_with_mode(path, bytes, None).map(|_| ())
}

/// [`write_atomic`] that lands the destination on `mode`.
///
/// The tmp sibling is created owner-only rather than at the umask default,
/// because it holds the same bytes as the destination for the whole write:
/// restoring a `0600` secret through a tmp that a crash could strand at
/// `0644` would leak it. `fchmod` widens the tmp to `mode` only once the
/// bytes are down, so the file is never more permissive than its final
/// mode at any point. Mirrors `install_runner::write_dest_atomic`, which
/// applies the manifest layout mode the same way on the install path —
/// restore has to match it or a rollback downgrades the file.
///
/// Returns the `fchmod` error instead of propagating it: on a filesystem
/// that cannot chmod, a restore that lands the bytes with the wrong mode
/// still beats one that deletes the staged content and leaves nothing.
fn write_atomic_with_mode(
    path: &Path,
    bytes: &[u8],
    mode: Option<u32>,
) -> io::Result<Option<io::Error>> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }
    let tmp = tmp_path_for(path);
    // Owner-only while the bytes are in flight when we know the target
    // mode; `None` keeps the historical umask default for journal writes.
    let mut f = open_excl_nofollow(&tmp, mode.map(|_| 0o600))?;
    if let Err(err) = f.write_all(bytes) {
        // Drop the half-written tmp so we don't leak it.
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }
    // Best-effort durability: matches the pattern in download.rs /
    // install_runner.rs — a sync_all failure here is not fatal because
    // the rename below is the actual atomicity guarantee.
    let _ = f.sync_all();
    let mut mode_error = None;
    if let Some(mode) = mode {
        use std::os::unix::fs::PermissionsExt;
        // `fchmod` on the descriptor, not the tmp path: nothing can be
        // swapped in underneath between the write and the mode change.
        if let Err(err) = f.set_permissions(fs::Permissions::from_mode(mode)) {
            mode_error = Some(err);
        }
    }
    // Close before rename so the bytes are fully flushed to the
    // descriptor before another process can observe the renamed file.
    drop(f);
    if let Err(err) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }
    Ok(mode_error)
}

/// Open `tmp` for writing with `O_CREAT|O_EXCL` (+ `O_NOFOLLOW` on Unix),
/// at `create_mode` when the caller wants something tighter than the umask
/// default.
///
/// Extracted as a named helper so the symlink/TOCTOU hardening can be
/// exercised directly from tests without having to race the random tmp
/// suffix produced by [`tmp_path_for`]. Mirrors the pattern used by
/// `download::stream_reader_and_hash` and
/// `install_runner::stream_write_and_hash`.
fn open_excl_nofollow(tmp: &Path, create_mode: Option<u32>) -> io::Result<File> {
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(nix::libc::O_NOFOLLOW);
        if let Some(mode) = create_mode {
            opts.mode(mode);
        }
    }
    let file = opts.open(tmp)?;
    #[cfg(unix)]
    if let Some(mode) = create_mode {
        // The `open(2)` creation mask is umask-filtered, so it can only be
        // narrowed — under `umask 0400` a `0600` request lands `0200`. That
        // is harmless for the staging window itself, but it would make the
        // helper's "created at exactly `create_mode`" contract a lie for
        // anyone reading back through this descriptor. `fchmod` is not
        // umask-filtered. Best-effort: callers that need the final mode
        // enforced apply and report it themselves.
        use std::os::unix::fs::PermissionsExt;
        let _ = file.set_permissions(fs::Permissions::from_mode(mode));
    }
    Ok(file)
}

/// Monotonic, process-wide counter mixed into [`tmp_path_for`] so that
/// concurrent writers on the same `path` don't pick the same tmp name.
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Generate a unique tmp sibling path for `path`.
///
/// Pattern: `.{file_name}.{pid}.{counter}.{nanos}.tmp`. The pid keeps
/// cross-process writes disjoint; the atomic counter keeps same-process
/// concurrent writes disjoint; the nanosecond timestamp adds entropy in
/// case the counter wraps. Combined with `O_CREAT|O_EXCL` in
/// [`open_excl_nofollow`] this means a stale tmp (or a hostile plant) at
/// the *exact* generated path is a hard error, not a silent overwrite.
fn tmp_path_for(path: &Path) -> PathBuf {
    let mut tmp = path.to_path_buf();
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "journal.toml".to_string());
    let pid = std::process::id();
    let counter = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    tmp.set_file_name(format!(".{file_name}.{pid}.{counter}.{nanos}.tmp"));
    tmp
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs as std_fs;
    use tempfile::tempdir;

    fn fresh(tmp: &tempfile::TempDir) -> (PathBuf, PathBuf) {
        let state_path = tmp.path().join("installed.toml");
        let journal_dir = tmp.path().join("journal");
        (state_path, journal_dir)
    }

    /// Unwrap the `Sidecar` variant or fail the test loudly.
    fn sidecar_of(tx: &Transaction) -> (PathBuf, String) {
        match tx.state_snapshot_ref.as_ref().expect("snapshot ref") {
            StateSnapshotRef::Sidecar { path, sha256 } => (path.clone(), sha256.clone()),
            other => panic!("expected sidecar snapshot, got {other:?}"),
        }
    }

    #[test]
    fn begin_creates_journal_file() {
        let tmp = tempdir().expect("tempdir");
        let (state_path, journal_dir) = fresh(&tmp);

        let tx = Transaction::begin("install", state_path, &journal_dir).expect("begin");
        assert!(tx.journal_path.exists(), "journal file must be created");
        assert!(tx.journal_path.starts_with(&journal_dir));
        assert!(tx.operation_id.starts_with("op-"));
        let on_disk = std_fs::read_to_string(&tx.journal_path).expect("read journal");
        assert!(on_disk.contains(&tx.operation_id));
        assert!(on_disk.contains("operation = \"install\""));
    }

    #[test]
    fn begin_with_missing_state_records_explicit_absent() {
        let tmp = tempdir().expect("tempdir");
        let (state_path, journal_dir) = fresh(&tmp);

        let tx = Transaction::begin("enable", state_path.clone(), &journal_dir).expect("begin");
        assert!(tx.state_snapshot.is_none());
        // "State did not exist" must be a positive record, never an
        // omitted table — load_journal treats an absent record in a v2
        // journal as corruption.
        assert_eq!(tx.state_snapshot_ref, Some(StateSnapshotRef::Absent));
        assert!(!state_path.exists());
    }

    #[test]
    fn begin_captures_existing_state_bytes_in_sidecar() {
        let tmp = tempdir().expect("tempdir");
        let (state_path, journal_dir) = fresh(&tmp);
        std_fs::write(&state_path, b"prior bytes").expect("seed state");

        let tx = Transaction::begin("disable", state_path, &journal_dir).expect("begin");

        // v2 journals never embed the bytes — they live in the sidecar.
        assert!(tx.state_snapshot.is_none());
        let (sidecar, sha256) = sidecar_of(&tx);
        assert!(sidecar.starts_with(&journal_dir));
        assert_eq!(
            std_fs::read(&sidecar).expect("read sidecar"),
            b"prior bytes"
        );
        assert_eq!(sha256, sha256_hex(b"prior bytes"));
    }

    #[test]
    fn journal_stays_small_when_state_is_large() {
        // Size guard for the full-rewrite persistence strategy: the
        // journal must never grow with the managed state. A multi-MB
        // state file previously ballooned every journal rewrite to tens
        // of MB (TOML integer-array encoding), producing near-quadratic
        // I/O on components with tens of thousands of owned files. If
        // this test starts failing, something is embedding bulk data in
        // the journal again — externalise it as a sidecar instead.
        const JOURNAL_SIZE_CEILING: u64 = 16 * 1024;

        let tmp = tempdir().expect("tempdir");
        let (state_path, journal_dir) = fresh(&tmp);
        let big_state = vec![0xabu8; 5 * 1024 * 1024];
        std_fs::write(&state_path, &big_state).expect("seed large state");

        let mut tx = Transaction::begin("uninstall", state_path, &journal_dir).expect("begin");
        for idx in 0..20 {
            tx.record_step(TransactionStep::planned(
                "owned-files",
                format!("phase-{idx}"),
                "remove_owned_files",
                None,
            ))
            .expect("record step");
            tx.mark_done(idx).expect("mark done");
        }
        tx.finish(TransactionOutcomeStatus::Ok).expect("finish");

        let journal_len = std_fs::metadata(&tx.journal_path)
            .expect("journal metadata")
            .len();
        assert!(
            journal_len < JOURNAL_SIZE_CEILING,
            "journal grew to {journal_len} bytes — bulk data must live in a sidecar, not the journal"
        );
        let (sidecar, _) = sidecar_of(&tx);
        assert_eq!(
            std_fs::read(&sidecar).expect("read sidecar"),
            big_state,
            "sidecar must hold the exact state bytes"
        );
    }

    #[test]
    fn record_step_persists_to_journal() {
        let tmp = tempdir().expect("tempdir");
        let (state_path, journal_dir) = fresh(&tmp);
        let mut tx = Transaction::begin("install", state_path, &journal_dir).expect("begin");

        tx.record_step(TransactionStep::planned(
            "materialise",
            "/opt/anolisa/bin/foo",
            "install_file",
            Some(RollbackAction::remove_file(PathBuf::from(
                "/opt/anolisa/bin/foo",
            ))),
        ))
        .expect("record step");

        let reloaded = Transaction::load_journal(&tx.journal_path).expect("load");
        assert_eq!(reloaded.steps.len(), 1);
        assert_eq!(reloaded.steps[0].action, "install_file");
        assert_eq!(reloaded.steps[0].status, TransactionStepStatus::Planned);
    }

    #[test]
    fn record_steps_persists_complete_batch() {
        let tmp = tempdir().expect("tempdir");
        let (state_path, journal_dir) = fresh(&tmp);
        let mut tx = Transaction::begin("install", state_path, &journal_dir).expect("begin");

        tx.record_steps([
            TransactionStep::planned("rpm-install", "pkg", "dnf-install", None),
            TransactionStep::planned("rpm-state", "component", "commit-state", None),
        ])
        .expect("record step batch");

        let reloaded = Transaction::load_journal(&tx.journal_path).expect("load");
        assert_eq!(reloaded.steps, tx.steps);
        assert_eq!(reloaded.steps.len(), 2);
    }

    #[test]
    fn delegated_recovery_intent_persists_context_and_steps_together() {
        let tmp = tempdir().expect("tempdir");
        let (state_path, journal_dir) = fresh(&tmp);
        let mut tx =
            Transaction::begin_with_subject("install", Some("cosh"), state_path, &journal_dir)
                .expect("begin");
        let context = DelegatedRecoveryContext {
            pm: NativePm::Rpm,
            package: Some("copilot-shell".to_string()),
            record_action: DelegatedRecordAction::WriteManaged,
            pinned: None,
        };
        let steps = [TransactionStep::planned(
            "delegated-txn",
            "copilot-shell",
            "install",
            None,
        )];

        let err = tx
            .record_delegated_steps(context.clone(), [])
            .expect_err("context-only intent must fail");
        assert!(matches!(err, TransactionError::Failed(_)));
        let empty = Transaction::load_journal(&tx.journal_path).expect("load empty journal");
        assert_eq!(empty.delegated_recovery, None);
        assert!(empty.steps.is_empty());

        tx.record_delegated_steps(context.clone(), steps)
            .expect("persist delegated intent");

        let reloaded = Transaction::load_journal(&tx.journal_path).expect("load");
        assert_eq!(reloaded.delegated_recovery, Some(context));
        assert_eq!(reloaded.steps, tx.steps);
        assert_eq!(reloaded.steps.len(), 1);

        let before = tx.clone();
        let err = tx
            .record_delegated_steps(
                DelegatedRecoveryContext {
                    pm: NativePm::Rpm,
                    package: Some("another-package".to_string()),
                    record_action: DelegatedRecordAction::WriteManaged,
                    pinned: None,
                },
                [TransactionStep::planned(
                    "delegated-record",
                    "state",
                    "write-delegated-managed",
                    None,
                )],
            )
            .expect_err("rebind must fail");
        assert!(matches!(err, TransactionError::Failed(_)));
        assert_eq!(tx, before, "a rejected rebind must not append steps");
    }

    #[test]
    fn load_journal_rejects_recovery_context_without_steps() {
        let tmp = tempdir().expect("tempdir");
        let (state_path, journal_dir) = fresh(&tmp);
        let mut tx =
            Transaction::begin_with_subject("install", Some("skillfs"), state_path, &journal_dir)
                .expect("begin");
        tx.delegated_recovery = Some(DelegatedRecoveryContext {
            pm: NativePm::Rpm,
            package: Some("skillfs".to_string()),
            record_action: DelegatedRecordAction::WriteManaged,
            pinned: None,
        });
        tx.persist().expect("persist malformed fixture");

        let err = Transaction::load_journal(&tx.journal_path)
            .expect_err("context-only journal must be corrupt");

        assert!(matches!(err, TransactionError::CorruptJournal(_)));
    }

    #[test]
    fn legacy_journal_without_recovery_context_still_loads() {
        let tmp = tempdir().expect("tempdir");
        let (state_path, journal_dir) = fresh(&tmp);
        let tx = Transaction::begin_with_subject("install", Some("cosh"), state_path, &journal_dir)
            .expect("begin");

        let reloaded = Transaction::load_journal(&tx.journal_path).expect("load");
        assert_eq!(reloaded.delegated_recovery, None);
    }

    #[test]
    fn restore_state_from_snapshot_writes_bytes() {
        let tmp = tempdir().expect("tempdir");
        let (state_path, journal_dir) = fresh(&tmp);
        std_fs::write(&state_path, b"original").expect("seed");

        let tx = Transaction::begin("install", state_path.clone(), &journal_dir).expect("begin");
        std_fs::write(&state_path, b"mutated").expect("simulate mid-op write");

        tx.restore_state().expect("restore");
        assert_eq!(std_fs::read(&state_path).expect("read"), b"original");
    }

    #[test]
    fn restore_state_survives_journal_round_trip() {
        // Recovery never sees the in-memory transaction — it reloads the
        // journal from disk. The sidecar reference must round-trip.
        let tmp = tempdir().expect("tempdir");
        let (state_path, journal_dir) = fresh(&tmp);
        std_fs::write(&state_path, b"original").expect("seed");

        let tx = Transaction::begin("install", state_path.clone(), &journal_dir).expect("begin");
        std_fs::write(&state_path, b"mutated").expect("simulate mid-op write");

        let reloaded = Transaction::load_journal(&tx.journal_path).expect("load");
        reloaded.restore_state().expect("restore after reload");
        assert_eq!(std_fs::read(&state_path).expect("read"), b"original");
    }

    #[test]
    fn restore_state_rejects_tampered_sidecar() {
        let tmp = tempdir().expect("tempdir");
        let (state_path, journal_dir) = fresh(&tmp);
        std_fs::write(&state_path, b"original").expect("seed");

        let tx = Transaction::begin("install", state_path.clone(), &journal_dir).expect("begin");
        let (sidecar, _) = sidecar_of(&tx);
        std_fs::write(&sidecar, b"forged snapshot").expect("tamper sidecar");
        std_fs::write(&state_path, b"mutated").expect("simulate mid-op write");

        let err = tx.restore_state().expect_err("tampered sidecar must fail");
        assert!(matches!(err, TransactionError::Rollback(_)));
        assert_eq!(
            std_fs::read(&state_path).expect("read"),
            b"mutated",
            "a failed restore must leave the state file untouched"
        );
    }

    #[test]
    fn restore_state_fails_closed_on_missing_sidecar() {
        // A referenced-but-missing sidecar must be a hard error, never a
        // fall-through to the "state did not exist" deletion branch.
        let tmp = tempdir().expect("tempdir");
        let (state_path, journal_dir) = fresh(&tmp);
        std_fs::write(&state_path, b"original").expect("seed");

        let tx = Transaction::begin("install", state_path.clone(), &journal_dir).expect("begin");
        let (sidecar, _) = sidecar_of(&tx);
        std_fs::remove_file(&sidecar).expect("drop sidecar");

        let err = tx.restore_state().expect_err("missing sidecar must fail");
        assert!(matches!(err, TransactionError::Rollback(_)));
        assert!(
            state_path.exists(),
            "a broken sidecar must never delete the state file"
        );
    }

    #[test]
    fn v1_journal_with_embedded_snapshot_still_loads_and_restores() {
        // Journals written by pre-v2 binaries embed the snapshot bytes
        // directly. They must keep loading and restoring so pending v1
        // operations survive an upgrade of the anolisa binary.
        //
        // The hand-written `state_snapshot = [<int>, ...]` line below is
        // deliberate: it pins the on-disk v1 wire encoding (TOML integer
        // array, one element per byte) independently of how serde happens
        // to serialise `Vec<u8>` today. Any future change to the field's
        // type or serialisation strategy that stops accepting this shape
        // breaks this test loudly instead of silently orphaning pending
        // v1 journals in the field.
        let tmp = tempdir().expect("tempdir");
        let (state_path, journal_dir) = fresh(&tmp);
        std_fs::create_dir_all(&journal_dir).expect("mkdir journal");
        let journal_path = journal_dir.join("op-install-20260101000000-abcdef.journal.toml");
        let embedded: Vec<String> = b"legacy bytes".iter().map(|b| b.to_string()).collect();
        let text = format!(
            "schema_version = 1\n\
             operation_id = \"op-install-20260101000000-abcdef\"\n\
             operation = \"install\"\n\
             started_at = \"2026-01-01T00:00:00Z\"\n\
             state_path = {state_path:?}\n\
             state_snapshot = [{embedded}]\n\
             journal_path = {journal_path:?}\n",
            state_path = state_path.display().to_string(),
            embedded = embedded.join(", "),
            journal_path = journal_path.display().to_string(),
        );
        std_fs::write(&journal_path, text).expect("seed v1 journal");

        let tx = Transaction::load_journal(&journal_path).expect("v1 journal must load");
        assert_eq!(tx.schema_version, 1);
        assert_eq!(tx.state_snapshot.as_deref(), Some(b"legacy bytes".as_ref()));
        assert!(tx.state_snapshot_ref.is_none());

        std_fs::write(&state_path, b"mutated").expect("simulate write");
        tx.restore_state().expect("v1 restore");
        assert_eq!(std_fs::read(&state_path).expect("read"), b"legacy bytes");
    }

    #[test]
    fn v2_journal_with_incomplete_snapshot_record_is_corrupt() {
        // The tagged `StateSnapshotRef` enum makes half-configured
        // snapshots unrepresentable in memory; this test guards the
        // on-disk side of that invariant. A hand-edited (or bit-rotted)
        // snapshot table that lost its tag or one of the sidecar fields
        // must be rejected at load as corrupt — parsing it would defer
        // the failure to rollback time, long after doctor/repair could
        // have surfaced it.
        let tmp = tempdir().expect("tempdir");
        let (state_path, journal_dir) = fresh(&tmp);
        std_fs::create_dir_all(&journal_dir).expect("mkdir journal");
        let journal_path = journal_dir.join("op-install-20260101000000-abcdef.journal.toml");
        for snapshot_table in [
            // sidecar without its digest
            "kind = \"sidecar\"\npath = \"/var/lib/anolisa/journal/op.state.snapshot\"\n",
            // sidecar without its path
            "kind = \"sidecar\"\nsha256 = \"0000000000000000000000000000000000000000000000000000000000000000\"\n",
            // untagged pre-enum shape (both fields, no kind)
            "path = \"/var/lib/anolisa/journal/op.state.snapshot\"\nsha256 = \"0000000000000000000000000000000000000000000000000000000000000000\"\n",
            // unknown tag
            "kind = \"mystery\"\n",
        ] {
            let text = format!(
                "schema_version = 2\n\
                 operation_id = \"op-install-20260101000000-abcdef\"\n\
                 operation = \"install\"\n\
                 started_at = \"2026-01-01T00:00:00Z\"\n\
                 state_path = {state_path:?}\n\
                 journal_path = {journal_path:?}\n\
                 \n\
                 [state_snapshot_ref]\n\
                 {snapshot_table}",
                state_path = state_path.display().to_string(),
                journal_path = journal_path.display().to_string(),
            );
            std_fs::write(&journal_path, text).expect("seed incomplete journal");

            let err = Transaction::load_journal(&journal_path)
                .expect_err("incomplete snapshot ref must not load");
            assert!(
                matches!(err, TransactionError::CorruptJournal(_)),
                "expected CorruptJournal for table {snapshot_table:?}, got: {err:?}"
            );
        }
    }

    #[test]
    fn v2_journal_missing_snapshot_record_entirely_is_corrupt() {
        // The failure mode a truncated / hand-edited / bit-rotted v2
        // journal actually produces: the whole `[state_snapshot_ref]`
        // table is gone. Deserialisation alone cannot catch this — the
        // field is `Option` so v1 journals can load — so load_journal
        // must refuse it structurally. Without that check the gap would
        // read as "state was absent at begin" and recovery would delete
        // `installed.toml`.
        let tmp = tempdir().expect("tempdir");
        let (state_path, journal_dir) = fresh(&tmp);
        std_fs::create_dir_all(&journal_dir).expect("mkdir journal");
        std_fs::write(&state_path, b"must survive").expect("seed state");
        let journal_path = journal_dir.join("op-install-20260101000000-abcdef.journal.toml");
        let text = format!(
            "schema_version = 2\n\
             operation_id = \"op-install-20260101000000-abcdef\"\n\
             operation = \"install\"\n\
             started_at = \"2026-01-01T00:00:00Z\"\n\
             state_path = {state_path:?}\n\
             journal_path = {journal_path:?}\n",
            state_path = state_path.display().to_string(),
            journal_path = journal_path.display().to_string(),
        );
        std_fs::write(&journal_path, text).expect("seed truncated journal");

        let err = Transaction::load_journal(&journal_path)
            .expect_err("v2 journal without snapshot record must not load");
        match &err {
            TransactionError::CorruptJournal(msg) => {
                assert!(
                    msg.contains("missing its state_snapshot_ref record"),
                    "unexpected message: {msg}"
                );
            }
            other => panic!("expected CorruptJournal, got: {other:?}"),
        }
        assert_eq!(
            std_fs::read(&state_path).expect("read state"),
            b"must survive",
            "refusing to load must leave the state file untouched"
        );
    }

    #[test]
    fn journal_with_mixed_version_snapshot_fields_is_corrupt() {
        // Cross-version mixtures are structurally impossible for our
        // writers, so they can only mean corruption or tampering:
        // a v2 journal never embeds snapshot bytes, and a v1 journal
        // cannot carry a v2 snapshot record.
        let tmp = tempdir().expect("tempdir");
        let (state_path, journal_dir) = fresh(&tmp);
        std_fs::create_dir_all(&journal_dir).expect("mkdir journal");
        let journal_path = journal_dir.join("op-install-20260101000000-abcdef.journal.toml");
        for (schema_version, extra) in [
            // v2 with v1 embedded bytes
            (2u32, "state_snapshot = [1, 2, 3]\n".to_string()),
            // v1 with a v2 snapshot record
            (
                1u32,
                "\n[state_snapshot_ref]\nkind = \"absent\"\n".to_string(),
            ),
        ] {
            let text = format!(
                "schema_version = {schema_version}\n\
                 operation_id = \"op-install-20260101000000-abcdef\"\n\
                 operation = \"install\"\n\
                 started_at = \"2026-01-01T00:00:00Z\"\n\
                 state_path = {state_path:?}\n\
                 journal_path = {journal_path:?}\n\
                 {extra}",
                state_path = state_path.display().to_string(),
                journal_path = journal_path.display().to_string(),
            );
            std_fs::write(&journal_path, text).expect("seed mixed journal");

            let err = Transaction::load_journal(&journal_path)
                .expect_err("mixed-version snapshot fields must not load");
            assert!(
                matches!(err, TransactionError::CorruptJournal(_)),
                "expected CorruptJournal for v{schema_version} mixture, got: {err:?}"
            );
        }
    }

    #[test]
    fn restore_state_without_snapshot_removes_file() {
        let tmp = tempdir().expect("tempdir");
        let (state_path, journal_dir) = fresh(&tmp);

        let tx = Transaction::begin("install", state_path.clone(), &journal_dir).expect("begin");
        assert!(tx.state_snapshot.is_none());
        assert_eq!(tx.state_snapshot_ref, Some(StateSnapshotRef::Absent));

        std_fs::write(&state_path, b"mutated").expect("simulate write");
        tx.restore_state().expect("restore");
        assert!(
            !state_path.exists(),
            "missing-snapshot rollback deletes state file"
        );
    }

    #[test]
    fn restore_state_with_no_snapshot_and_no_file_is_noop() {
        let tmp = tempdir().expect("tempdir");
        let (state_path, journal_dir) = fresh(&tmp);
        let tx = Transaction::begin("install", state_path, &journal_dir).expect("begin");
        tx.restore_state().expect("restore idempotent");
    }

    #[test]
    fn remove_file_refuses_untracked_path() {
        let tmp = tempdir().expect("tempdir");
        let (state_path, journal_dir) = fresh(&tmp);
        let stranger = tmp.path().join("stranger.bin");
        std_fs::write(&stranger, b"do not touch").expect("seed");

        let tx = Transaction::begin("install", state_path, &journal_dir).expect("begin");
        let err = tx.remove_file(&stranger).expect_err("must refuse");
        match err {
            TransactionError::UntrackedPath(p) => assert_eq!(p, stranger),
            other => panic!("unexpected error: {other:?}"),
        }
        assert!(stranger.exists(), "untracked path must NOT be deleted");
    }

    #[test]
    fn remove_file_removes_tracked_path() {
        let tmp = tempdir().expect("tempdir");
        let (state_path, journal_dir) = fresh(&tmp);
        let owned = tmp.path().join("owned.bin");
        std_fs::write(&owned, b"anolisa-managed").expect("seed");

        let mut tx = Transaction::begin("install", state_path, &journal_dir).expect("begin");
        tx.record_step(TransactionStep::planned(
            "materialise",
            owned.to_string_lossy(),
            "install_file",
            Some(RollbackAction::remove_file(owned.clone())),
        ))
        .expect("record");
        tx.mark_done(0).expect("done");

        tx.remove_file(&owned).expect("remove tracked");
        assert!(!owned.exists());
    }

    #[test]
    fn mark_failed_persists_and_round_trips() {
        let tmp = tempdir().expect("tempdir");
        let (state_path, journal_dir) = fresh(&tmp);
        let mut tx = Transaction::begin("install", state_path, &journal_dir).expect("begin");
        tx.record_step(TransactionStep::planned(
            "precheck",
            "agent-observability",
            "env-check",
            None,
        ))
        .expect("record");
        tx.mark_failed(0, "env-check failed: kernel too old")
            .expect("mark failed");

        let reloaded = Transaction::load_journal(&tx.journal_path).expect("load");
        assert_eq!(reloaded.steps[0].status, TransactionStepStatus::Failed);
        assert_eq!(
            reloaded.steps[0].note.as_deref(),
            Some("env-check failed: kernel too old")
        );
    }

    #[test]
    fn mark_skipped_persists() {
        let tmp = tempdir().expect("tempdir");
        let (state_path, journal_dir) = fresh(&tmp);
        let mut tx = Transaction::begin("install", state_path, &journal_dir).expect("begin");
        tx.record_step(TransactionStep::planned(
            "materialise",
            "/opt/a",
            "install",
            None,
        ))
        .expect("record");
        tx.mark_skipped(0, "already up to date").expect("skip");

        let reloaded = Transaction::load_journal(&tx.journal_path).expect("load");
        assert_eq!(reloaded.steps[0].status, TransactionStepStatus::Skipped);
        assert_eq!(
            reloaded.steps[0].note.as_deref(),
            Some("already up to date")
        );
    }

    #[test]
    fn load_journal_on_corrupt_content_returns_corrupt_journal() {
        let tmp = tempdir().expect("tempdir");
        let bad = tmp.path().join("bad.journal.toml");
        std_fs::write(&bad, b"= not valid toml =").expect("seed");

        let err = Transaction::load_journal(&bad).expect_err("must fail");
        match err {
            TransactionError::CorruptJournal(_) => {}
            other => panic!("expected CorruptJournal, got {other:?}"),
        }
    }

    #[test]
    fn load_journal_rejects_unknown_schema_version() {
        let tmp = tempdir().expect("tempdir");
        let bad = tmp.path().join("future.journal.toml");
        std_fs::write(
            &bad,
            br#"schema_version = 999
operation_id = "op-x"
operation = "install"
started_at = "2026-01-01T00:00:00Z"
state_path = "/dev/null"
journal_path = "/tmp/x.journal.toml"
"#,
        )
        .expect("seed");

        let err = Transaction::load_journal(&bad).expect_err("must fail");
        match err {
            TransactionError::CorruptJournal(msg) => {
                assert!(msg.contains("schema_version"), "msg: {msg}");
            }
            other => panic!("expected CorruptJournal, got {other:?}"),
        }
    }

    #[test]
    fn load_journal_rejects_schema_below_minimum() {
        let tmp = tempdir().expect("tempdir");
        let bad = tmp.path().join("ancient.journal.toml");
        std_fs::write(
            &bad,
            br#"schema_version = 0
operation_id = "op-x"
operation = "install"
started_at = "2026-01-01T00:00:00Z"
state_path = "/dev/null"
journal_path = "/tmp/x.journal.toml"
"#,
        )
        .expect("seed");

        let err = Transaction::load_journal(&bad).expect_err("must fail");
        match err {
            TransactionError::CorruptJournal(msg) => {
                assert!(msg.contains("schema_version"), "msg: {msg}");
            }
            other => panic!("expected CorruptJournal, got {other:?}"),
        }
    }

    #[test]
    fn restore_file_copies_bytes_and_verifies_sha256() {
        let tmp = tempdir().expect("tempdir");
        let (state_path, journal_dir) = fresh(&tmp);
        let tx = Transaction::begin("install", state_path, &journal_dir).expect("begin");

        let backup = tmp.path().join("backup/foo.conf");
        let dest = tmp.path().join("etc/foo.conf");
        std_fs::create_dir_all(backup.parent().expect("parent")).expect("mkdir");
        std_fs::write(&backup, b"original config").expect("seed backup");

        let rb = RollbackAction::restore_file(
            backup.clone(),
            dest.clone(),
            Some(sha256_hex(b"original config")),
        );
        tx.restore_file(&rb).expect("restore_file");
        assert_eq!(std_fs::read(&dest).expect("read"), b"original config");

        // Sha mismatch surfaces as Rollback error.
        let mut bad = rb.clone();
        bad.sha256 = Some("deadbeef".to_string());
        let err = tx.restore_file(&bad).expect_err("mismatch");
        match err {
            TransactionError::Rollback(_) => {}
            other => panic!("expected Rollback, got {other:?}"),
        }
    }

    #[test]
    #[cfg(unix)]
    fn restore_file_recreates_symlink_backup_as_link() {
        let tmp = tempdir().expect("tempdir");
        let (state_path, journal_dir) = fresh(&tmp);
        let tx = Transaction::begin("uninstall", state_path, &journal_dir).expect("begin");

        let referent = tmp.path().join("libexec/rtk");
        std_fs::create_dir_all(referent.parent().expect("parent")).expect("mkdir");
        std_fs::write(&referent, b"rtk bytes").expect("seed referent");
        let backup = tmp.path().join("backup/0.bak");
        std_fs::create_dir_all(backup.parent().expect("parent")).expect("mkdir");
        std::os::unix::fs::symlink(&referent, &backup).expect("seed backup link");
        let dest = tmp.path().join("bin/rtk");

        let rb = RollbackAction::restore_file(backup, dest.clone(), None);
        tx.restore_file(&rb).expect("restore_file");

        let meta = std_fs::symlink_metadata(&dest).expect("dest exists");
        assert!(meta.file_type().is_symlink(), "dest must be a link");
        assert_eq!(std_fs::read_link(&dest).expect("read_link"), referent);
    }

    #[test]
    #[cfg(unix)]
    fn restore_backup_file_replaces_a_destination_leaf_symlink() {
        let tmp = tempdir().expect("tempdir");
        let backup = tmp.path().join("backup/0.bak");
        let dest = tmp.path().join("bin/tool");
        let outside = tmp.path().join("outside-target");
        std_fs::create_dir_all(backup.parent().expect("backup parent")).expect("mkdir backup");
        std_fs::create_dir_all(dest.parent().expect("dest parent")).expect("mkdir dest");
        std_fs::write(&backup, b"owned bytes").expect("seed backup");
        std_fs::write(&outside, b"outside bytes").expect("seed outside target");
        std::os::unix::fs::symlink(&outside, &dest).expect("plant destination symlink");

        restore_backup_file(&backup, &dest, Some(&sha256_hex(b"owned bytes")), None)
            .expect("restore backup");

        assert_eq!(
            std_fs::read(&outside).expect("read outside target"),
            b"outside bytes",
            "restore must not follow a destination leaf symlink"
        );
        assert!(
            !std_fs::symlink_metadata(&dest)
                .expect("stat restored destination")
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std_fs::read(&dest).expect("read restored file"),
            b"owned bytes"
        );
    }

    /// Rollback restores mode for mode, not only byte for byte: the atomic
    /// sibling lands at `0666 & ~umask`, so without an explicit chmod a
    /// restored `0755` binary comes back non-executable.
    #[test]
    #[cfg(unix)]
    fn restore_backup_file_reapplies_the_recorded_mode() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempdir().expect("tempdir");
        let backup = tmp.path().join("backup/0.bak");
        let dest = tmp.path().join("bin/tool");
        std_fs::create_dir_all(backup.parent().expect("backup parent")).expect("mkdir backup");
        std_fs::create_dir_all(dest.parent().expect("dest parent")).expect("mkdir dest");
        std_fs::write(&backup, b"owned bytes").expect("seed backup");
        std_fs::write(&dest, b"replaced bytes").expect("seed dest");

        let warnings = restore_backup_file(
            &backup,
            &dest,
            Some(&sha256_hex(b"owned bytes")),
            Some(0o755),
        )
        .expect("restore backup");

        assert!(warnings.is_empty(), "clean restore must not warn");
        assert_eq!(
            std_fs::read(&dest).expect("read restored file"),
            b"owned bytes"
        );
        assert_eq!(
            std_fs::metadata(&dest)
                .expect("stat restored file")
                .permissions()
                .mode()
                & 0o7777,
            0o755,
            "the recorded mode must survive the tmp+rename"
        );
    }

    /// Restore writes a new inode owned by the restoring process, so
    /// replaying setuid/setgid would hand out privileges the pre-operation
    /// file never granted. Those bits are dropped, and the operator is told
    /// rather than left to discover it from `ls`.
    #[test]
    #[cfg(unix)]
    fn restore_backup_file_drops_setuid_and_says_so() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempdir().expect("tempdir");
        let backup = tmp.path().join("backup/0.bak");
        let dest = tmp.path().join("libexec/helper");
        std_fs::create_dir_all(backup.parent().expect("backup parent")).expect("mkdir backup");
        std_fs::create_dir_all(dest.parent().expect("dest parent")).expect("mkdir dest");
        std_fs::write(&backup, b"helper bytes").expect("seed backup");

        let warnings =
            restore_backup_file(&backup, &dest, None, Some(0o4755)).expect("restore backup");

        let mode = std_fs::metadata(&dest)
            .expect("stat restored file")
            .permissions()
            .mode()
            & 0o7777;
        assert_eq!(
            mode, 0o755,
            "the executable bits come back, the setuid bit does not: {mode:04o}"
        );
        assert_eq!(warnings.len(), 1, "the dropped bit must be reported");
        assert!(
            warnings[0].contains("4755") && warnings[0].contains("755"),
            "the warning must name both modes: {}",
            warnings[0]
        );
    }

    /// With no recorded mode the bytes still land; the destination simply
    /// takes the umask default, exactly as it did before modes were carried.
    #[test]
    #[cfg(unix)]
    fn restore_backup_file_without_a_mode_restores_bytes_only() {
        let tmp = tempdir().expect("tempdir");
        let backup = tmp.path().join("backup/0.bak");
        let dest = tmp.path().join("bin/tool");
        std_fs::create_dir_all(backup.parent().expect("backup parent")).expect("mkdir backup");
        std_fs::create_dir_all(dest.parent().expect("dest parent")).expect("mkdir dest");
        std_fs::write(&backup, b"owned bytes").expect("seed backup");

        let warnings = restore_backup_file(&backup, &dest, None, None).expect("restore backup");

        assert!(warnings.is_empty());
        assert_eq!(
            std_fs::read(&dest).expect("read restored file"),
            b"owned bytes"
        );
    }

    /// The tmp sibling holds the same bytes as the destination for the whole
    /// write, so it must never be created at the umask default when the
    /// caller knows the target mode — a crash between write and rename would
    /// otherwise strand a world-readable copy of a private file.
    #[test]
    #[cfg(unix)]
    fn write_atomic_with_mode_stages_the_tmp_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempdir().expect("tempdir");
        let target = tmp.path().join("etc/secret.toml");
        std_fs::create_dir_all(target.parent().expect("parent")).expect("mkdir");

        let staged = open_excl_nofollow(&tmp_path_for(&target), Some(0o600)).expect("open tmp");
        let mode = staged.metadata().expect("stat tmp").permissions().mode() & 0o7777;

        assert_eq!(
            mode, 0o600,
            "the staged tmp must be owner-only regardless of umask: {mode:04o}"
        );
    }

    #[test]
    fn journal_preserves_step_statuses() {
        let tmp = tempdir().expect("tempdir");
        let (state_path, journal_dir) = fresh(&tmp);
        let mut tx = Transaction::begin("install", state_path, &journal_dir).expect("begin");
        for action in ["a", "b", "c", "d"] {
            tx.record_step(TransactionStep::planned("p", action, "do", None))
                .expect("record");
        }
        tx.mark_done(0).expect("done");
        tx.mark_done(1).expect("done");
        tx.mark_failed(2, "boom").expect("fail");
        tx.mark_skipped(3, "skip").expect("skip");
        tx.finish(TransactionOutcomeStatus::Partial)
            .expect("finish");

        let loaded = Transaction::load_journal(&tx.journal_path).expect("load journal");
        assert_eq!(loaded.operation_id, tx.operation_id);
        assert_eq!(loaded.operation, "install");
        assert_eq!(
            loaded
                .steps
                .iter()
                .map(|step| step.status)
                .collect::<Vec<_>>(),
            vec![
                TransactionStepStatus::Done,
                TransactionStepStatus::Done,
                TransactionStepStatus::Failed,
                TransactionStepStatus::Skipped,
            ]
        );
        assert_eq!(loaded.status, TransactionOutcomeStatus::Partial);
        assert_eq!(loaded.finished_at, tx.finished_at);
        assert!(loaded.finished_at.is_some());
    }

    #[test]
    fn finish_persists_status_and_finished_at() {
        let tmp = tempdir().expect("tempdir");
        let (state_path, journal_dir) = fresh(&tmp);
        let mut tx = Transaction::begin("install", state_path, &journal_dir).expect("begin");
        tx.finish(TransactionOutcomeStatus::Ok).expect("finish");

        let reloaded = Transaction::load_journal(&tx.journal_path).expect("load");
        assert_eq!(reloaded.status, TransactionOutcomeStatus::Ok);
        assert!(reloaded.finished_at.is_some());
    }

    // --- write_atomic hardening: tmp-symlink TOCTOU regression suite.
    //
    // Testing approach (documented inline because it's a bit non-obvious):
    //
    // The random suffix in `tmp_path_for` means we can't race-fully plant a
    // symlink at the exact tmp path that the production code will pick.
    // Instead we exercise the two invariants directly:
    //
    //   1. `open_excl_nofollow` is extracted as a private helper and tested
    //      against a pre-placed symlink at a *known* path. This is the
    //      primitive that closes the TOCTOU hole; if it ever regresses to
    //      following symlinks the test fires immediately.
    //
    //   2. `tmp_path_for` is exercised end-to-end via `write_atomic` to
    //      confirm (a) two back-to-back writes don't collide and (b) a
    //      symlink planted at the *final* target gets atomically replaced
    //      by the rename (which is the documented unix `rename(2)` behaviour
    //      — the symlink itself, not its target, is replaced).

    #[test]
    fn tmp_path_for_includes_random_suffix_and_does_not_collide() {
        let p = Path::new("/tmp/x/journal.toml");
        let a = tmp_path_for(p);
        let b = tmp_path_for(p);
        let an = a.file_name().expect("tmp file_name").to_string_lossy();
        let bn = b.file_name().expect("tmp file_name").to_string_lossy();
        assert!(an.starts_with(".journal.toml."));
        assert!(an.ends_with(".tmp"));
        assert_ne!(an, bn, "two tmp paths for the same target must differ");
    }

    #[cfg(unix)]
    #[test]
    fn open_excl_nofollow_refuses_existing_symlink() {
        // Direct test of the primitive: a pre-placed symlink at the tmp path
        // must error rather than letting the open follow it through to the
        // victim. Without O_NOFOLLOW + O_EXCL this would silently truncate
        // `victim`.
        let dir = tempdir().expect("tempdir");
        let outside = tempdir().expect("outside tempdir");
        let victim = outside.path().join("victim");
        std_fs::write(&victim, b"do not touch").expect("seed victim");

        let tmp_plant = dir.path().join(".target.tmp");
        std::os::unix::fs::symlink(&victim, &tmp_plant).expect("plant symlink");

        let err = open_excl_nofollow(&tmp_plant, None).expect_err("must refuse symlink");
        // Either ELOOP (NOFOLLOW kicked in) or EEXIST (EXCL kicked in) is
        // acceptable; both mean the bytes never touched the victim.
        let kind = err.kind();
        assert!(
            kind == io::ErrorKind::AlreadyExists || err.raw_os_error() == Some(nix::libc::ELOOP),
            "expected EEXIST or ELOOP, got {err:?}",
        );
        let victim_bytes = std_fs::read(&victim).expect("victim still readable");
        assert_eq!(
            victim_bytes, b"do not touch",
            "symlinked tmp must never be written through",
        );
    }

    #[test]
    fn open_excl_nofollow_refuses_existing_regular_file() {
        // create_new semantics: simulating "a previous tmp file is already
        // sitting at the exact generated path" must surface as EEXIST so
        // we never blindly overwrite arbitrary on-disk state.
        let dir = tempdir().expect("tempdir");
        let tmp_plant = dir.path().join(".already-here.tmp");
        std_fs::write(&tmp_plant, b"stale").expect("seed stale tmp");

        let err = open_excl_nofollow(&tmp_plant, None).expect_err("must refuse existing file");
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn write_atomic_back_to_back_calls_both_succeed() {
        // Verifies the random suffix is doing its job: two write_atomic
        // calls in quick succession must both succeed without an EEXIST
        // collision on the tmp path. Catches the regression where the
        // tmp name was fixed and a leftover tmp from call N would make
        // call N+1 fail.
        let dir = tempdir().expect("tempdir");
        let target = dir.path().join("journal.toml");

        write_atomic(&target, b"one").expect("first write");
        assert_eq!(std_fs::read(&target).expect("read"), b"one");
        write_atomic(&target, b"two").expect("second write");
        assert_eq!(std_fs::read(&target).expect("read"), b"two");

        // No tmp file should linger in the parent dir.
        let leftovers: Vec<_> = std_fs::read_dir(dir.path())
            .expect("read parent dir")
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "write_atomic must not leak tmp siblings: {leftovers:?}",
        );
    }

    #[cfg(unix)]
    #[test]
    fn write_atomic_replaces_symlinked_target_without_touching_victim() {
        // If the *final* path is a symlink to a victim outside the journal
        // dir, `rename(2)` replaces the symlink itself (not the target).
        // The victim must be untouched and the journal dir must end up
        // holding a regular file with the new bytes.
        let dir = tempdir().expect("tempdir");
        let outside = tempdir().expect("outside tempdir");
        let victim = outside.path().join("victim");
        std_fs::write(&victim, b"do not touch").expect("seed victim");

        let target = dir.path().join("journal.toml");
        std::os::unix::fs::symlink(&victim, &target).expect("plant symlink at target");

        write_atomic(&target, b"fresh bytes").expect("write_atomic over symlink");

        // Final target is now a regular file with our bytes.
        let meta = std_fs::symlink_metadata(&target).expect("stat target");
        assert!(
            meta.file_type().is_file(),
            "target must be a regular file after rename, was {:?}",
            meta.file_type(),
        );
        assert_eq!(std_fs::read(&target).expect("read target"), b"fresh bytes");

        // Victim outside the journal dir is unchanged.
        let victim_bytes = std_fs::read(&victim).expect("read victim");
        assert_eq!(
            victim_bytes, b"do not touch",
            "rename must replace the symlink, not write through it",
        );
    }
}
