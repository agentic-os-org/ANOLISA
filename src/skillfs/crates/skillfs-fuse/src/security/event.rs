//! Skill Security normalized events.
//!
//! S0 introduces an event vocabulary that future packages (audit, policy
//! enforcement, drift detection) can consume without rewriting every FUSE
//! callback. The default sink (`NoopEventSink`) does nothing; tests can use
//! `InMemoryEventSink` to capture emissions.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Operation kind for a [`SkillEvent`].
///
/// Kept as a flat enum so future packages can match on it directly. New
/// variants may be added; consumers should treat unknown variants as
/// best-effort.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillEventKind {
    Open,
    Read,
    Write,
    Create,
    Delete,
    Rename,
    Metadata,
    Readlink,
    SymlinkAttempt,
    HardlinkAttempt,
    PolicyDecision,
    PolicyDenied,
    /// Out-of-band change observed in the physical source tree that did not
    /// flow through a SkillFS FUSE callback. Visibility-only — emitted by
    /// drift observers (see [`super::drift`]) so consumers can tell the
    /// difference between a write through the mount and a write that
    /// bypassed it. SkillFS does not enforce, quarantine, or block in
    /// response to this kind.
    SourceChanged,
}

/// Coarse outcome attached to an event. Distinct from `errno` so
/// rejected-by-policy events can be told apart from kernel-level failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillEventAction {
    Allowed,
    Rejected,
    Failed,
}

/// Normalized record of a SkillFS-observed filesystem operation.
///
/// All fields except `kind` are optional because most callbacks only have a
/// subset of the information; the consumer should not assume any field is
/// populated. The struct is intentionally cloneable so an in-memory sink can
/// snapshot it cheaply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillEvent {
    pub kind: SkillEventKind,
    pub skill_name: Option<String>,
    pub relative_path: Option<PathBuf>,
    pub action: Option<SkillEventAction>,
    pub errno: Option<i32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub bytes: Option<u64>,
    pub detail: Option<String>,
}

impl SkillEvent {
    /// Build an event with only the kind populated.
    pub fn new(kind: SkillEventKind) -> Self {
        Self {
            kind,
            skill_name: None,
            relative_path: None,
            action: None,
            errno: None,
            uid: None,
            gid: None,
            bytes: None,
            detail: None,
        }
    }

    pub fn with_skill_name(mut self, name: impl Into<String>) -> Self {
        self.skill_name = Some(name.into());
        self
    }

    pub fn with_optional_skill_name<S: Into<String>>(mut self, name: Option<S>) -> Self {
        self.skill_name = name.map(Into::into);
        self
    }

    pub fn with_relative_path(mut self, path: impl AsRef<Path>) -> Self {
        self.relative_path = Some(path.as_ref().to_path_buf());
        self
    }

    pub fn with_optional_relative_path<P: AsRef<Path>>(mut self, path: Option<P>) -> Self {
        self.relative_path = path.map(|p| p.as_ref().to_path_buf());
        self
    }

    pub fn with_action(mut self, action: SkillEventAction) -> Self {
        self.action = Some(action);
        self
    }

    pub fn with_errno(mut self, errno: i32) -> Self {
        self.errno = Some(errno);
        self
    }

    pub fn with_caller(mut self, uid: u32, gid: u32) -> Self {
        self.uid = Some(uid);
        self.gid = Some(gid);
        self
    }

    pub fn with_bytes(mut self, bytes: u64) -> Self {
        self.bytes = Some(bytes);
        self
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

/// Sink for [`SkillEvent`] records.
///
/// The FUSE layer treats event emission as best-effort: the result of
/// [`SkillEventSink::emit`] is discarded, and an emission call is made on
/// the FUSE thread inside the affected callback. Implementations are
/// expected to be best-effort and should not block or otherwise affect
/// filesystem operations — long work, network I/O, or panics belong off
/// the FUSE thread (e.g. behind a queue or worker). This is a contract
/// the trait cannot enforce; SkillFS will not retry, surface, or
/// compensate for misbehaving sinks.
pub trait SkillEventSink: Send + Sync + 'static {
    fn emit(&self, event: &SkillEvent);
}

/// Default no-op sink. Used unless the caller provides an alternative.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopEventSink;

impl SkillEventSink for NoopEventSink {
    fn emit(&self, _event: &SkillEvent) {}
}

/// Test/helper sink that records every event in memory.
///
/// Intended for tests and small in-process consumers. It is exposed
/// outside `cfg(test)` so integration tests under
/// `crates/skillfs-fuse/tests/` can use it directly. Not suitable as a
/// production audit sink (events are kept in an unbounded `Vec`).
#[derive(Debug, Default)]
pub struct InMemoryEventSink {
    events: Mutex<Vec<SkillEvent>>,
}

impl InMemoryEventSink {
    pub fn new() -> Self {
        Self {
            events: Mutex::new(Vec::new()),
        }
    }

    /// Snapshot of all events recorded so far.
    pub fn events(&self) -> Vec<SkillEvent> {
        self.events.lock().map(|g| g.clone()).unwrap_or_default()
    }

    pub fn len(&self) -> usize {
        self.events.lock().map(|g| g.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Most recent event, if any.
    pub fn last(&self) -> Option<SkillEvent> {
        self.events.lock().ok().and_then(|g| g.last().cloned())
    }

    /// All events that match `kind`.
    pub fn of_kind(&self, kind: SkillEventKind) -> Vec<SkillEvent> {
        self.events
            .lock()
            .map(|g| g.iter().filter(|e| e.kind == kind).cloned().collect())
            .unwrap_or_default()
    }
}

impl SkillEventSink for InMemoryEventSink {
    fn emit(&self, event: &SkillEvent) {
        if let Ok(mut g) = self.events.lock() {
            g.push(event.clone());
        }
    }
}
