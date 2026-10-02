//! Central audit/operation log.
//!
//! `CentralLog` is the append-only JSON Lines store that backs the
//! `anolisa logs` command (launch spec §8.4). Each record is serialised
//! on a single line with a trailing `\n`, so callers can tail/grep the
//! file without needing structured tooling.
//!
//! The schema follows launch spec §8.4 verbatim: every record carries a
//! `kind` discriminator (operation vs. component-reported), the
//! originating `command`/`source`, a `severity`, an `actor`, and the
//! `started_at` timestamp. Operation entries additionally include
//! `operation_id`, `finished_at`, `status`, and the list of `objects`
//! they touched. All new optional fields default-deserialise so the
//! schema can grow without breaking older records.
//!
//! The current implementation is the P1-A skeleton: append uses
//! `OpenOptions::append`, and `query` is a sequential scan with simple
//! filters. Rotation, indexing, and follow-mode are future work.

use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

/// Whether the record describes an ANOLISA operation (tracked via
/// `operation_id`) or a passive component-reported event.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LogKind {
    /// Operation initiated by `anolisa` (enable/disable/install/...).
    Operation,
    /// Event reported by a managed component (agentsight, sec-core, ...).
    Component,
}

/// Severity level. Ordering: `Debug < Info < Warn < Error`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Diagnostic detail useful only when debugging.
    Debug,
    /// Normal operational progress.
    Info,
    /// Non-fatal condition the operator should notice.
    Warn,
    /// Terminal or user-visible failure.
    Error,
}

/// Terminal status for an operation record.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LogStatus {
    /// Completed successfully.
    Ok,
    /// Failed; no rollback performed (or rollback also failed).
    Failed,
    /// Failed and rolled back to the prior state.
    RolledBack,
    /// Partial success — some objects applied, others did not.
    Partial,
}

/// A single line in the central log (launch spec §8.4).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LogRecord {
    /// Operation vs. component-reported event.
    pub kind: LogKind,
    /// Stable operation identifier, e.g. `op-20260601-001`. Component
    /// events typically leave this `None`.
    #[serde(default)]
    pub operation_id: Option<String>,
    /// Human-readable command, e.g. `install agentsight`.
    pub command: String,
    /// Producer, e.g. `anolisa-cli`, `agentsight`, `sec-core`.
    pub source: String,
    /// Component name when the record is component-scoped.
    #[serde(default)]
    pub component: Option<String>,
    /// Severity (`debug` < `info` < `warn` < `error`).
    pub severity: Severity,
    /// Human-readable message.
    pub message: String,
    /// User identity; `cli` when ANOLISA cannot determine it.
    pub actor: String,
    /// Install mode (`system` or `user`).
    #[serde(default)]
    pub install_mode: Option<String>,
    /// ISO8601 UTC timestamp marking when the operation started or when
    /// the component-reported event was observed.
    pub started_at: String,
    /// ISO8601 UTC completion timestamp; `None` for in-flight or
    /// instantaneous records.
    #[serde(default)]
    pub finished_at: Option<String>,
    /// Terminal status for operations; `None` for component events or
    /// records still in flight.
    #[serde(default)]
    pub status: Option<LogStatus>,
    /// Component names involved in the record.
    #[serde(default)]
    pub objects: Vec<String>,
    /// Backup IDs taken by the operation.
    #[serde(default)]
    pub backup_ids: Vec<String>,
    /// Non-fatal warnings collected during the operation.
    #[serde(default)]
    pub warnings: Vec<String>,
    /// Free-form structured payload; defaults to `Null`.
    #[serde(default)]
    pub details: serde_json::Value,
}

/// Subset of fields to filter on during [`CentralLog::query`].
#[derive(Debug, Default, Clone)]
pub struct LogFilter {
    /// Match exact `kind`.
    pub kind: Option<LogKind>,
    /// Match exact `source`.
    pub source: Option<String>,
    /// Match exact `component`.
    pub component: Option<String>,
    /// Match exact `operation_id` (e.g. `op-20260601-001`). Records
    /// whose `operation_id` is `None` never match.
    pub operation_id: Option<String>,
    /// Match records whose severity is `>=` this value.
    pub severity_at_least: Option<Severity>,
    /// Match if the value is in `objects[]`, or — for backward
    /// compatibility with records that only carry `component` — if
    /// `component == Some(value)`.
    pub object: Option<String>,
    /// Lexicographic lower bound on `started_at` (ISO8601 sorts
    /// correctly for UTC).
    pub since: Option<String>,
    /// Cap the returned record count to the most recent N matches
    /// (append-only file order). Results stay chronological: oldest of
    /// that window first. `None` returns every match; `Some(0)` is empty.
    pub limit: Option<usize>,
}

/// Append-only JSONL central log.
#[derive(Debug, Clone)]
pub struct CentralLog {
    path: PathBuf,
    // Parks `query` after the shared flock is dropped so overlap tests
    // can run `append` without a wall-clock race against scheduling.
    #[cfg(test)]
    query_scan_hold: Option<QueryScanHold>,
}

// Rendezvous used by lock-overlap tests: signal that `query` has
// unlocked, then wait until the test finishes `append` before scan.
#[cfg(test)]
#[derive(Clone)]
struct QueryScanHold {
    inner: std::sync::Arc<QueryScanHoldInner>,
}

#[cfg(test)]
struct QueryScanHoldInner {
    phase: std::sync::Mutex<QueryScanPhase>,
    changed: std::sync::Condvar,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum QueryScanPhase {
    Running,
    Unlocked,
    Resume,
}

#[cfg(test)]
impl std::fmt::Debug for QueryScanHold {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("QueryScanHold")
    }
}

#[cfg(test)]
impl QueryScanHold {
    fn new() -> Self {
        Self {
            inner: std::sync::Arc::new(QueryScanHoldInner {
                phase: std::sync::Mutex::new(QueryScanPhase::Running),
                changed: std::sync::Condvar::new(),
            }),
        }
    }

    fn park_after_unlock(&self) {
        let mut phase = self.inner.phase.lock().expect("query scan hold mutex");
        *phase = QueryScanPhase::Unlocked;
        self.inner.changed.notify_all();
        while *phase != QueryScanPhase::Resume {
            phase = self
                .inner
                .changed
                .wait(phase)
                .expect("query scan hold condvar");
        }
    }

    fn wait_until_unlocked(&self) {
        use std::time::{Duration, Instant};

        let mut phase = self.inner.phase.lock().expect("query scan hold mutex");
        let deadline = Instant::now() + Duration::from_secs(10);
        while *phase == QueryScanPhase::Running {
            let now = Instant::now();
            assert!(
                now < deadline,
                "query never parked after dropping the shared flock"
            );
            let (next, timed_out) = self
                .inner
                .changed
                .wait_timeout(phase, deadline.saturating_duration_since(now))
                .expect("query scan hold condvar");
            phase = next;
            if timed_out.timed_out() && *phase == QueryScanPhase::Running {
                panic!("query never parked after dropping the shared flock");
            }
        }
        assert_eq!(
            *phase,
            QueryScanPhase::Unlocked,
            "query left the post-unlock hold before the overlap append"
        );
    }

    fn resume_scan(&self) {
        let mut phase = self.inner.phase.lock().expect("query scan hold mutex");
        *phase = QueryScanPhase::Resume;
        self.inner.changed.notify_all();
    }
}

/// Errors raised by [`CentralLog`].
#[derive(Debug, thiserror::Error)]
pub enum CentralLogError {
    /// Filesystem access failed while opening, locking, reading, or
    /// writing the JSONL file.
    #[error("io error while accessing {path}: {source}")]
    Io {
        /// Path involved in the failed filesystem operation.
        path: PathBuf,
        /// Original I/O error from the OS.
        #[source]
        source: io::Error,
    },
    /// A log record could not be encoded as JSON.
    #[error("failed to serialize log record: {0}")]
    Serialize(#[from] serde_json::Error),
}

impl CentralLog {
    /// Open (does not create) a log handle for `path`. The file is
    /// created lazily on the first `append`.
    pub fn open(path: PathBuf) -> Self {
        Self {
            path,
            #[cfg(test)]
            query_scan_hold: None,
        }
    }

    /// Path the log writes to.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append a single record, terminated by `\n`. Parent directories
    /// are created on demand.
    ///
    /// An exclusive `flock` is taken on the file across the whole
    /// serialize+write+flush so concurrent appends from multiple
    /// processes or threads cannot interleave a partially-written JSON
    /// line. `write_all` is followed by `flush()` to push the line into
    /// the OS layer; we intentionally skip `sync_all` to avoid the per-
    /// append fsync cost — readers see the record via `query` as soon as
    /// the OS buffer accepts it.
    pub fn append(&self, record: &LogRecord) -> Result<(), CentralLogError> {
        if let Some(parent) = self.path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent).map_err(|source| CentralLogError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }

        let mut line = serde_json::to_string(record)?;
        line.push('\n');

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|source| CentralLogError::Io {
                path: self.path.clone(),
                source,
            })?;
        FileExt::lock_exclusive(&file).map_err(|source| CentralLogError::Io {
            path: self.path.clone(),
            source,
        })?;
        let write_result = file.write_all(line.as_bytes()).and_then(|_| file.flush());
        let unlock_result = FileExt::unlock(&file);
        write_result.map_err(|source| CentralLogError::Io {
            path: self.path.clone(),
            source,
        })?;
        unlock_result.map_err(|source| CentralLogError::Io {
            path: self.path.clone(),
            source,
        })?;
        Ok(())
    }

    /// Sequentially scan the log, returning matching records. Missing
    /// file yields an empty result. `limit` is applied after filtering
    /// and keeps the most recent `N` matches in append-only file order
    /// (oldest of that window first). `None` returns every match;
    /// `Some(0)` is empty.
    ///
    /// A shared `flock` is held only long enough to snapshot a stable
    /// byte length so a concurrent `append` cannot publish a partial
    /// line into that prefix. The lock is then dropped and the scan
    /// reads only those bytes, so a tail query does not block writers
    /// for an O(file-size) deserialize. Later appends extend the file
    /// past the snapshot and are not included.
    ///
    /// # Examples
    ///
    /// Three records with `limit = 2` keep the last two, still in file
    /// order:
    ///
    /// ```
    /// use anolisa_core::central_log::{
    ///     CentralLog, LogFilter, LogKind, LogRecord, LogStatus, Severity,
    /// };
    ///
    /// let dir = tempfile::tempdir().unwrap();
    /// let log = CentralLog::open(dir.path().join("audit.jsonl"));
    /// for (idx, id) in ["op-a", "op-b", "op-c"].iter().enumerate() {
    ///     log.append(&LogRecord {
    ///         kind: LogKind::Operation,
    ///         operation_id: Some((*id).to_string()),
    ///         command: "test".into(),
    ///         source: "anolisa-cli".into(),
    ///         component: None,
    ///         severity: Severity::Info,
    ///         message: "ok".into(),
    ///         actor: "cli".into(),
    ///         install_mode: None,
    ///         started_at: format!("2026-06-01T10:00:0{idx}Z"),
    ///         finished_at: None,
    ///         status: Some(LogStatus::Ok),
    ///         objects: vec![],
    ///         backup_ids: vec![],
    ///         warnings: vec![],
    ///         details: serde_json::Value::Null,
    ///     })
    ///     .unwrap();
    /// }
    /// let hits = log
    ///     .query(&LogFilter {
    ///         limit: Some(2),
    ///         ..Default::default()
    ///     })
    ///     .unwrap();
    /// assert_eq!(hits.len(), 2);
    /// assert_eq!(hits[0].operation_id.as_deref(), Some("op-b"));
    /// assert_eq!(hits[1].operation_id.as_deref(), Some("op-c"));
    /// ```
    pub fn query(&self, filter: &LogFilter) -> Result<Vec<LogRecord>, CentralLogError> {
        if filter.limit == Some(0) {
            return Ok(Vec::new());
        }
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let mut file = File::open(&self.path).map_err(|source| CentralLogError::Io {
            path: self.path.clone(),
            source,
        })?;
        FileExt::lock_shared(&file).map_err(|source| CentralLogError::Io {
            path: self.path.clone(),
            source,
        })?;
        let snapshot = file.metadata().map(|meta| meta.len());
        let unlock_result = FileExt::unlock(&file);
        let len = snapshot.map_err(|source| CentralLogError::Io {
            path: self.path.clone(),
            source,
        })?;
        unlock_result.map_err(|source| CentralLogError::Io {
            path: self.path.clone(),
            source,
        })?;
        #[cfg(test)]
        if let Some(hold) = &self.query_scan_hold {
            hold.park_after_unlock();
        }
        file.seek(SeekFrom::Start(0))
            .map_err(|source| CentralLogError::Io {
                path: self.path.clone(),
                source,
            })?;
        self.scan_reader(file.take(len), filter)
    }

    fn scan_reader<R: Read>(
        &self,
        reader: R,
        filter: &LogFilter,
    ) -> Result<Vec<LogRecord>, CentralLogError> {
        let reader = BufReader::new(reader);
        // Keep a sliding window so `--limit` is a tail cap. Stopping at the
        // first N matches would pin `anolisa logs` to genesis records once
        // the JSONL file grew past the default 50.
        let mut matches: VecDeque<LogRecord> = VecDeque::new();
        for line in reader.lines() {
            let line = line.map_err(|source| CentralLogError::Io {
                path: self.path.clone(),
                source,
            })?;
            if line.trim().is_empty() {
                continue;
            }
            let record: LogRecord = serde_json::from_str(&line)?;
            if record_matches(&record, filter) {
                matches.push_back(record);
                if let Some(limit) = filter.limit
                    && matches.len() > limit
                {
                    matches.pop_front();
                }
            }
        }
        Ok(matches.into_iter().collect())
    }
}

fn record_matches(record: &LogRecord, filter: &LogFilter) -> bool {
    if let Some(kind) = filter.kind
        && record.kind != kind
    {
        return false;
    }
    if let Some(source) = &filter.source
        && &record.source != source
    {
        return false;
    }
    if let Some(component) = &filter.component {
        match &record.component {
            Some(record_component) if record_component == component => {}
            _ => return false,
        }
    }
    if let Some(operation_id) = &filter.operation_id {
        match &record.operation_id {
            Some(record_op_id) if record_op_id == operation_id => {}
            _ => return false,
        }
    }
    if let Some(min) = filter.severity_at_least
        && record.severity < min
    {
        return false;
    }
    if let Some(obj) = &filter.object {
        let in_objects = record.objects.iter().any(|candidate| candidate == obj);
        let legacy_component_match = record.component.as_deref() == Some(obj.as_str());
        if !in_objects && !legacy_component_match {
            return false;
        }
    }
    if let Some(since) = &filter.since
        && record.started_at.as_str() < since.as_str()
    {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn operation_record(
        started_at: &str,
        operation_id: &str,
        objects: &[&str],
        severity: Severity,
    ) -> LogRecord {
        LogRecord {
            kind: LogKind::Operation,
            operation_id: Some(operation_id.to_string()),
            command: "install agentsight".to_string(),
            source: "anolisa-cli".to_string(),
            component: None,
            severity,
            message: "operation finished".to_string(),
            actor: "test-actor".to_string(),
            install_mode: Some("user".to_string()),
            started_at: started_at.to_string(),
            finished_at: Some(started_at.to_string()),
            status: Some(LogStatus::Ok),
            objects: objects.iter().map(|s| s.to_string()).collect(),
            backup_ids: Vec::new(),
            warnings: Vec::new(),
            details: serde_json::Value::Null,
        }
    }

    fn component_record(started_at: &str, source: &str, severity: Severity) -> LogRecord {
        LogRecord {
            kind: LogKind::Component,
            operation_id: None,
            command: "report".to_string(),
            source: source.to_string(),
            component: Some(source.to_string()),
            severity,
            message: "component reported".to_string(),
            actor: "cli".to_string(),
            install_mode: None,
            started_at: started_at.to_string(),
            finished_at: None,
            status: None,
            objects: Vec::new(),
            backup_ids: Vec::new(),
            warnings: Vec::new(),
            details: serde_json::Value::Null,
        }
    }

    #[test]
    fn roundtrip_record() {
        let record = LogRecord {
            kind: LogKind::Operation,
            operation_id: Some("op-20260601-001".to_string()),
            command: "install agentsight".to_string(),
            source: "anolisa-cli".to_string(),
            component: Some("agentsight".to_string()),
            severity: Severity::Info,
            message: "install agentsight finished".to_string(),
            actor: "test-actor".to_string(),
            install_mode: Some("user".to_string()),
            started_at: "2026-06-01T10:00:00Z".to_string(),
            finished_at: Some("2026-06-01T10:00:03Z".to_string()),
            status: Some(LogStatus::Ok),
            objects: vec!["agent-observability".to_string(), "agentsight".to_string()],
            backup_ids: vec!["bk-1".to_string()],
            warnings: vec!["systemd reload skipped".to_string()],
            details: json!({"duration_ms": 3000}),
        };

        let line = serde_json::to_string(&record).expect("serialize");
        let parsed: LogRecord = serde_json::from_str(&line).expect("deserialize");
        assert_eq!(record, parsed);
    }

    #[test]
    fn append_then_query_all() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = CentralLog::open(dir.path().join("nested").join("audit.jsonl"));

        log.append(&operation_record(
            "2026-06-01T10:00:00Z",
            "op-1",
            &["agent-observability"],
            Severity::Info,
        ))
        .expect("append 1");
        log.append(&operation_record(
            "2026-06-01T10:00:01Z",
            "op-2",
            &["tokenless"],
            Severity::Info,
        ))
        .expect("append 2");
        log.append(&operation_record(
            "2026-06-01T10:00:02Z",
            "op-3",
            &["ws-ckpt"],
            Severity::Info,
        ))
        .expect("append 3");

        let all = log.query(&LogFilter::default()).expect("query");
        assert_eq!(all.len(), 3);
        let contents = std::fs::read_to_string(log.path()).expect("read");
        assert_eq!(contents.lines().count(), 3);
    }

    #[test]
    fn query_filters_by_kind() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = CentralLog::open(dir.path().join("audit.jsonl"));

        log.append(&operation_record(
            "2026-06-01T10:00:00Z",
            "op-1",
            &[],
            Severity::Info,
        ))
        .expect("append");
        log.append(&operation_record(
            "2026-06-01T10:00:01Z",
            "op-2",
            &[],
            Severity::Info,
        ))
        .expect("append");
        log.append(&component_record(
            "2026-06-01T10:00:02Z",
            "agentsight",
            Severity::Info,
        ))
        .expect("append");
        log.append(&component_record(
            "2026-06-01T10:00:03Z",
            "sec-core",
            Severity::Warn,
        ))
        .expect("append");

        let components = log
            .query(&LogFilter {
                kind: Some(LogKind::Component),
                ..Default::default()
            })
            .expect("query");
        assert_eq!(components.len(), 2);
        assert!(components.iter().all(|r| r.kind == LogKind::Component));
    }

    #[test]
    fn query_filters_by_source() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = CentralLog::open(dir.path().join("audit.jsonl"));

        log.append(&component_record(
            "2026-06-01T10:00:00Z",
            "agentsight",
            Severity::Info,
        ))
        .expect("append");
        log.append(&component_record(
            "2026-06-01T10:00:01Z",
            "sec-core",
            Severity::Info,
        ))
        .expect("append");
        log.append(&component_record(
            "2026-06-01T10:00:02Z",
            "agentsight",
            Severity::Warn,
        ))
        .expect("append");

        let agentsight_only = log
            .query(&LogFilter {
                source: Some("agentsight".to_string()),
                ..Default::default()
            })
            .expect("query");
        assert_eq!(agentsight_only.len(), 2);
        assert!(agentsight_only.iter().all(|r| r.source == "agentsight"));
    }

    #[test]
    fn query_filters_by_operation_id() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = CentralLog::open(dir.path().join("audit.jsonl"));

        log.append(&operation_record(
            "2026-06-01T10:00:00Z",
            "op-20260601-001",
            &["agent-observability"],
            Severity::Info,
        ))
        .expect("append");
        log.append(&operation_record(
            "2026-06-01T10:00:01Z",
            "op-20260601-002",
            &["tokenless"],
            Severity::Info,
        ))
        .expect("append");
        log.append(&operation_record(
            "2026-06-01T10:00:02Z",
            "op-20260601-003",
            &["ws-ckpt"],
            Severity::Info,
        ))
        .expect("append");

        let hits = log
            .query(&LogFilter {
                operation_id: Some("op-20260601-002".to_string()),
                ..Default::default()
            })
            .expect("query");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].operation_id.as_deref(), Some("op-20260601-002"));
    }

    #[test]
    fn query_filters_by_severity_at_least() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = CentralLog::open(dir.path().join("audit.jsonl"));

        log.append(&component_record(
            "2026-06-01T10:00:00Z",
            "agentsight",
            Severity::Debug,
        ))
        .expect("append");
        log.append(&component_record(
            "2026-06-01T10:00:01Z",
            "agentsight",
            Severity::Info,
        ))
        .expect("append");
        log.append(&component_record(
            "2026-06-01T10:00:02Z",
            "agentsight",
            Severity::Warn,
        ))
        .expect("append");
        log.append(&component_record(
            "2026-06-01T10:00:03Z",
            "agentsight",
            Severity::Error,
        ))
        .expect("append");

        let warn_or_above = log
            .query(&LogFilter {
                severity_at_least: Some(Severity::Warn),
                ..Default::default()
            })
            .expect("query");
        assert_eq!(warn_or_above.len(), 2);
        assert!(
            warn_or_above
                .iter()
                .all(|r| r.severity == Severity::Warn || r.severity == Severity::Error)
        );
    }

    #[test]
    fn query_filters_by_object() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = CentralLog::open(dir.path().join("audit.jsonl"));

        log.append(&operation_record(
            "2026-06-01T10:00:00Z",
            "op-1",
            &["agent-observability", "agentsight"],
            Severity::Info,
        ))
        .expect("append");
        log.append(&operation_record(
            "2026-06-01T10:00:01Z",
            "op-2",
            &["tokenless"],
            Severity::Info,
        ))
        .expect("append");
        // Component record carrying only `component` — legacy match.
        log.append(&component_record(
            "2026-06-01T10:00:02Z",
            "agentsight",
            Severity::Info,
        ))
        .expect("append");

        let hits = log
            .query(&LogFilter {
                object: Some("agentsight".to_string()),
                ..Default::default()
            })
            .expect("query");
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn query_limit_applies_after_filter() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = CentralLog::open(dir.path().join("audit.jsonl"));

        for (idx, severity) in [
            Severity::Debug,
            Severity::Info,
            Severity::Warn,
            Severity::Error,
            Severity::Warn,
        ]
        .iter()
        .enumerate()
        {
            log.append(&component_record(
                &format!("2026-06-01T10:00:0{idx}Z"),
                "agentsight",
                *severity,
            ))
            .expect("append");
        }

        let warn_two = log
            .query(&LogFilter {
                severity_at_least: Some(Severity::Warn),
                limit: Some(2),
                ..Default::default()
            })
            .expect("query");
        assert_eq!(warn_two.len(), 2);
        // Limit keeps the last 2 matches (Error, Warn), still oldest-first.
        assert_eq!(warn_two[0].severity, Severity::Error);
        assert_eq!(warn_two[1].severity, Severity::Warn);
        assert_eq!(warn_two[0].started_at, "2026-06-01T10:00:03Z");
        assert_eq!(warn_two[1].started_at, "2026-06-01T10:00:04Z");
    }

    #[test]
    fn query_limit_tails_matches_inside_since_window() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = CentralLog::open(dir.path().join("audit.jsonl"));
        for (idx, op) in ["op-old", "op-a", "op-b", "op-c"].iter().enumerate() {
            log.append(&operation_record(
                &format!("2026-06-01T10:00:0{idx}Z"),
                op,
                &[],
                Severity::Info,
            ))
            .expect("append");
        }

        let hits = log
            .query(&LogFilter {
                since: Some("2026-06-01T10:00:01Z".to_string()),
                limit: Some(2),
                ..Default::default()
            })
            .expect("query");
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].operation_id.as_deref(), Some("op-b"));
        assert_eq!(hits[1].operation_id.as_deref(), Some("op-c"));
    }

    #[test]
    fn query_limit_zero_returns_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = CentralLog::open(dir.path().join("audit.jsonl"));

        log.append(&component_record(
            "2026-06-01T10:00:00Z",
            "agentsight",
            Severity::Warn,
        ))
        .expect("append");

        let none = log
            .query(&LogFilter {
                limit: Some(0),
                ..Default::default()
            })
            .expect("query");
        assert!(none.is_empty());
    }

    #[test]
    fn severity_ordering() {
        assert!(Severity::Debug < Severity::Info);
        assert!(Severity::Info < Severity::Warn);
        assert!(Severity::Warn < Severity::Error);
        assert!(Severity::Error > Severity::Debug);
    }

    #[test]
    fn query_since_uses_lexicographic_lower_bound() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = CentralLog::open(dir.path().join("audit.jsonl"));
        log.append(&operation_record(
            "2026-05-01T00:00:00Z",
            "op-old",
            &[],
            Severity::Info,
        ))
        .expect("append");
        log.append(&operation_record(
            "2026-06-01T00:00:00Z",
            "op-new",
            &[],
            Severity::Info,
        ))
        .expect("append");

        let recent = log
            .query(&LogFilter {
                since: Some("2026-05-15T00:00:00Z".to_string()),
                ..Default::default()
            })
            .expect("query");
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].operation_id.as_deref(), Some("op-new"));
    }

    #[test]
    fn append_is_visible_to_query_immediately() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = CentralLog::open(dir.path().join("audit.jsonl"));

        log.append(&operation_record(
            "2026-06-01T10:00:00Z",
            "op-flush",
            &["agent-observability"],
            Severity::Info,
        ))
        .expect("append");

        let records = log.query(&LogFilter::default()).expect("query");
        assert_eq!(records.len(), 1);
        let raw = std::fs::read_to_string(log.path()).expect("read");
        assert!(raw.ends_with('\n'), "trailing newline must reach disk");
        assert_eq!(raw.lines().count(), 1);
    }

    #[test]
    fn concurrent_appends_do_not_interleave_lines() {
        use std::sync::Arc;
        use std::thread;

        let dir = tempfile::tempdir().expect("tempdir");
        let log = Arc::new(CentralLog::open(dir.path().join("audit.jsonl")));

        let writers: usize = 8;
        let per_writer: usize = 25;
        let mut handles = Vec::new();
        for w in 0..writers {
            let log = Arc::clone(&log);
            handles.push(thread::spawn(move || {
                for i in 0..per_writer {
                    let rec = operation_record(
                        &format!("2026-06-01T10:00:{:02}Z", i % 60),
                        &format!("op-{w}-{i}"),
                        &["agent-observability"],
                        Severity::Info,
                    );
                    log.append(&rec).expect("append");
                }
            }));
        }
        for h in handles {
            h.join().expect("join");
        }

        // Every line must parse — proves no two threads interleaved
        // bytes mid-record.
        let raw = std::fs::read_to_string(log.path()).expect("read");
        assert_eq!(raw.lines().count(), writers * per_writer);
        for line in raw.lines() {
            assert!(!line.is_empty());
            let _: LogRecord = serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("line is not valid JSON: {e}\n{line}"));
        }

        // And the query path agrees on the total count.
        let all = log.query(&LogFilter::default()).expect("query");
        assert_eq!(all.len(), writers * per_writer);
    }

    #[test]
    fn query_concurrent_with_append_never_reads_half_lines() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::thread;

        let dir = tempfile::tempdir().expect("tempdir");
        let log = Arc::new(CentralLog::open(dir.path().join("audit.jsonl")));

        let total_writes: usize = 400;
        let done = Arc::new(AtomicBool::new(false));

        // Writer: hammers append with large-ish records to push the
        // single write_all comfortably past the POSIX atomic-write
        // boundary, so the length snapshot under shared lock matters.
        let writer_log = Arc::clone(&log);
        let writer_done = Arc::clone(&done);
        let writer = thread::spawn(move || {
            let padding = "x".repeat(8 * 1024);
            for i in 0..total_writes {
                let mut rec = operation_record(
                    &format!("2026-06-01T10:00:{:02}Z", i % 60),
                    &format!("op-{i}"),
                    &["agent-observability"],
                    Severity::Info,
                );
                rec.message = padding.clone();
                writer_log.append(&rec).expect("append");
            }
            writer_done.store(true, Ordering::SeqCst);
        });

        // Reader: keeps querying until the writer finishes. Every
        // query must succeed — a `CentralLogError::Serialize` would
        // mean we read a torn line.
        let reader_log = Arc::clone(&log);
        let reader_done = Arc::clone(&done);
        let reader = thread::spawn(move || {
            let mut queries: u64 = 0;
            loop {
                let r = reader_log.query(&LogFilter::default());
                assert!(r.is_ok(), "concurrent query saw a torn line: {r:?}");
                queries += 1;
                if reader_done.load(Ordering::SeqCst) {
                    // Drain once more after the writer signals done.
                    let r = reader_log.query(&LogFilter::default());
                    assert!(r.is_ok());
                    break;
                }
            }
            queries
        });

        writer.join().expect("writer join");
        let _queries = reader.join().expect("reader join");

        let final_records = log.query(&LogFilter::default()).expect("final query");
        assert_eq!(final_records.len(), total_writes);
    }

    #[test]
    fn limited_query_releases_lock_before_scanning_history() {
        use std::sync::{Arc, mpsc};
        use std::thread;
        use std::time::Duration;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("audit.jsonl");
        let hold = QueryScanHold::new();
        let mut log = CentralLog::open(path);
        log.query_scan_hold = Some(hold.clone());
        let log = Arc::new(log);

        log.append(&operation_record(
            "2026-06-01T10:00:00Z",
            "op-before",
            &["agent-observability"],
            Severity::Info,
        ))
        .expect("seed older");
        log.append(&operation_record(
            "2026-06-01T10:00:01Z",
            "op-snapshot-tail",
            &["agent-observability"],
            Severity::Info,
        ))
        .expect("seed newer");

        let query_log = Arc::clone(&log);
        let query = thread::spawn(move || {
            query_log
                .query(&LogFilter {
                    limit: Some(1),
                    ..Default::default()
                })
                .expect("tail query")
        });

        // Query has dropped the shared flock and is parked before scan.
        hold.wait_until_unlocked();
        let (done_tx, done_rx) = mpsc::channel();
        let append_log = Arc::clone(&log);
        thread::spawn(move || {
            append_log
                .append(&operation_record(
                    "2026-06-01T11:00:00Z",
                    "op-during-scan",
                    &["agent-observability"],
                    Severity::Info,
                ))
                .expect("append during tail query");
            done_tx.send(()).expect("append done");
        });
        done_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("append blocked while query was parked; shared flock was not dropped");
        assert!(
            !query.is_finished(),
            "append completed only after the parked scan resumed"
        );
        hold.resume_scan();

        let hits = query.join().expect("query join");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].operation_id.as_deref(), Some("op-snapshot-tail"));
    }

    #[test]
    fn missing_optional_fields_default_on_deserialize() {
        // Minimum payload — all #[serde(default)] fields omitted.
        let line = r#"{
            "kind": "component",
            "command": "report",
            "source": "agentsight",
            "severity": "info",
            "message": "hello",
            "actor": "cli",
            "started_at": "2026-06-01T10:00:00Z"
        }"#;
        let parsed: LogRecord = serde_json::from_str(line).expect("deserialize");
        assert_eq!(parsed.kind, LogKind::Component);
        assert!(parsed.operation_id.is_none());
        assert!(parsed.component.is_none());
        assert!(parsed.install_mode.is_none());
        assert!(parsed.finished_at.is_none());
        assert!(parsed.status.is_none());
        assert!(parsed.objects.is_empty());
        assert!(parsed.backup_ids.is_empty());
        assert!(parsed.warnings.is_empty());
        assert!(parsed.details.is_null());
    }
}
