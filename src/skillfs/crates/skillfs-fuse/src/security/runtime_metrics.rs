//! Real-time runtime metric delta events for the SLS ops log.
//!
//! Unlike the shutdown-only [`super::session_stats`] summary, this module emits
//! one JSONL record per metric event **while the mount is alive**, so the SLS /
//! product side can aggregate deltas continuously instead of waiting for the
//! mount to exit.
//!
//! Records carry `record_type = "runtime_metric"` to distinguish them from the
//! CLI ops records and the legacy session summary that share the same file:
//!   `/var/log/anolisa/sls/ops/skillfs.jsonl`
//!
//! Design contract (identical to the other SkillFS SLS writers):
//!
//! * The target file is owned by the deployment/SLS component. This writer only
//!   appends: when the file does not exist the write is silently skipped
//!   ("SLS collection not active"). It never creates the file or parent dir.
//! * Every write opens the file, appends one JSONL line, and closes the handle.
//! * All emission is best-effort: serialization or write failures are logged via
//!   `tracing::warn` and swallowed. Emission never panics, never blocks on retry,
//!   and never changes FUSE or CLI behavior.
//!
//! Each metric field is a **delta** (e.g. `skill_hit_times = 1` per event); the
//! aggregator sums them per session.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use serde::Serialize;
use tracing::warn;

use super::telemetry_gate::{TELEMETRY_DISABLED_SENTINEL, telemetry_allowed_at};

/// Default ops log path (shared with CLI ops and the legacy session summary).
pub const SKILLFS_RUNTIME_METRICS_LOG_PATH: &str = "/var/log/anolisa/sls/ops/skillfs.jsonl";

/// One runtime metric delta record.
///
/// Common fields are always present; delta fields are sparse — only the metric(s)
/// updated by a given event are serialized (`skip_serializing_if = Option::is_none`).
#[derive(Debug, Clone, Serialize)]
pub struct RuntimeMetricRecord {
    #[serde(rename = "component.name")]
    pub component_name: String,
    #[serde(rename = "component.version")]
    pub component_version: String,
    #[serde(rename = "component.agent_name")]
    pub agent_name: String,
    pub record_type: String,
    pub session_id: String,
    pub event_name: String,
    pub err_reason: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub mount_times: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skill_hit_times: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy_allow_times: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy_fallback_times: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy_denied_times: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pruned_skill_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_token_saved_estimate: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mount_duration_ms: Option<u64>,
}

impl RuntimeMetricRecord {
    fn new(
        component_version: String,
        agent_name: String,
        session_id: String,
        event_name: &str,
        err_reason: &str,
    ) -> Self {
        Self {
            component_name: "skillfs".to_string(),
            component_version,
            agent_name,
            record_type: "runtime_metric".to_string(),
            session_id,
            event_name: event_name.to_string(),
            err_reason: err_reason.to_string(),
            mount_times: None,
            skill_hit_times: None,
            policy_allow_times: None,
            policy_fallback_times: None,
            policy_denied_times: None,
            pruned_skill_count: None,
            prompt_token_saved_estimate: None,
            mount_duration_ms: None,
        }
    }
}

/// Best-effort append-only JSONL writer for runtime metric records.
pub struct RuntimeMetricsWriter {
    path: PathBuf,
    /// Telemetry disable sentinel. Pinned to [`TELEMETRY_DISABLED_SENTINEL`] in
    /// production (no override path exists); tests inject a temp path so the
    /// gate does not depend on the host's real sentinel.
    sentinel: PathBuf,
}

impl RuntimeMetricsWriter {
    /// Create a writer targeting `path`.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            sentinel: PathBuf::from(TELEMETRY_DISABLED_SENTINEL),
        }
    }

    /// Create a writer targeting the default deployment path.
    pub fn default_path() -> Self {
        Self::new(SKILLFS_RUNTIME_METRICS_LOG_PATH)
    }

    /// Override the telemetry sentinel path (tests only) so writer tests do not
    /// depend on the host's real `/etc/anolisa/.telemetry_disabled`.
    #[cfg(test)]
    fn with_sentinel(mut self, sentinel: impl Into<PathBuf>) -> Self {
        self.sentinel = sentinel.into();
        self
    }

    /// The target file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one record as a JSONL line. Skips silently when the file is
    /// absent; serialization/write failures are logged and swallowed.
    pub fn write(&self, record: &RuntimeMetricRecord) {
        // Re-check the disable sentinel on every write (before serialization
        // and open) so creating/removing it takes effect immediately; disabled
        // is a normal state, so skip silently.
        if !telemetry_allowed_at(&self.sentinel) {
            return;
        }
        if !self.path.exists() {
            return;
        }
        let mut line = match serde_json::to_string(record) {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "skillfs runtime metrics: failed to serialize record");
                return;
            }
        };
        line.push('\n');

        let mut opts = std::fs::OpenOptions::new();
        opts.append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // A legit SLS file is never a symlink; O_NOFOLLOW blocks a
            // swap-to-symlink between the existence check and the open.
            opts.custom_flags(libc::O_NOFOLLOW);
        }
        if let Err(e) = opts
            .open(&self.path)
            .and_then(|mut f| f.write_all(line.as_bytes()))
        {
            warn!(
                error = %e,
                path = %self.path.display(),
                "skillfs runtime metrics: failed to append record (non-fatal)"
            );
        }
    }
}

/// Emits runtime metric delta events for a single mount session.
///
/// Thread-safe (`Send + Sync`): timing state is held behind a `Mutex` and all
/// emission is best-effort. Cloneable references are shared between the CLI mount
/// lifecycle (start/heartbeat/end) and the FUSE runtime (skill hits, policy
/// outcomes) via `Arc`.
pub struct RuntimeMetricsSink {
    writer: RuntimeMetricsWriter,
    component_version: String,
    agent_name: String,
    session_id: String,
    /// Instant of the last emitted heartbeat (or `mount_start`). Used to compute
    /// the `mount_duration_ms` delta for heartbeat and `mount_end`.
    last_mark: Mutex<Option<Instant>>,
}

impl RuntimeMetricsSink {
    /// Create a sink writing to `writer`, tagged with `session_id` / `agent_name`.
    pub fn new(writer: RuntimeMetricsWriter, session_id: String, agent_name: String) -> Self {
        Self {
            writer,
            component_version: env!("CARGO_PKG_VERSION").to_string(),
            agent_name,
            session_id,
            last_mark: Mutex::new(None),
        }
    }

    fn record(&self, event_name: &str, err_reason: &str) -> RuntimeMetricRecord {
        RuntimeMetricRecord::new(
            self.component_version.clone(),
            self.agent_name.clone(),
            self.session_id.clone(),
            event_name,
            err_reason,
        )
    }

    /// Snapshot `now`, returning the delta in ms since the previous mark and
    /// updating the mark. Falls back to 0 when no previous mark exists.
    fn take_duration_ms(&self) -> u64 {
        let now = Instant::now();
        let mut guard = self.last_mark.lock().unwrap_or_else(|e| e.into_inner());
        let delta = guard
            .map(|prev| {
                now.duration_since(prev)
                    .as_millis()
                    .min(u128::from(u64::MAX)) as u64
            })
            .unwrap_or(0);
        *guard = Some(now);
        delta
    }

    /// `mount_start`: emitted after the mount task spawns successfully.
    pub fn emit_mount_start(&self) {
        {
            let mut guard = self.last_mark.lock().unwrap_or_else(|e| e.into_inner());
            *guard = Some(Instant::now());
        }
        let mut rec = self.record("mount_start", "none");
        rec.mount_times = Some(1);
        self.writer.write(&rec);
    }

    /// `view_pruned`: emitted once at startup after views are computed.
    pub fn emit_view_pruned(&self, pruned_skill_count: u64, prompt_token_saved_estimate: u64) {
        let mut rec = self.record("view_pruned", "none");
        rec.pruned_skill_count = Some(pruned_skill_count);
        rec.prompt_token_saved_estimate = Some(prompt_token_saved_estimate);
        self.writer.write(&rec);
    }

    /// `skill_hit`: one agent-visible skill open.
    pub fn record_skill_hit(&self) {
        let mut rec = self.record("skill_hit", "none");
        rec.skill_hit_times = Some(1);
        self.writer.write(&rec);
    }

    /// `policy_allow`: a security/policy decision serving the current/allowed view.
    pub fn record_policy_allow(&self) {
        let mut rec = self.record("policy_allow", "none");
        rec.policy_allow_times = Some(1);
        self.writer.write(&rec);
    }

    /// `policy_fallback`: a security/policy decision serving the fallback/snapshot view.
    pub fn record_policy_fallback(&self) {
        let mut rec = self.record("policy_fallback", "none");
        rec.policy_fallback_times = Some(1);
        self.writer.write(&rec);
    }

    /// `policy_denied`: a security/policy denial or rejection.
    pub fn record_policy_denied(&self) {
        let mut rec = self.record("policy_denied", "none");
        rec.policy_denied_times = Some(1);
        self.writer.write(&rec);
    }

    /// `mount_heartbeat`: periodic liveness tick carrying the duration delta
    /// since the last heartbeat / start.
    pub fn emit_heartbeat(&self) {
        let delta = self.take_duration_ms();
        let mut rec = self.record("mount_heartbeat", "none");
        rec.mount_duration_ms = Some(delta);
        self.writer.write(&rec);
    }

    /// `mount_end`: clean exit; carries the remaining duration since the last mark.
    pub fn emit_mount_end(&self) {
        let delta = self.take_duration_ms();
        let mut rec = self.record("mount_end", "none");
        rec.mount_duration_ms = Some(delta);
        self.writer.write(&rec);
    }

    /// `mount_error`: mount startup/runtime failure; carries a concise reason and
    /// the elapsed duration since the last mark.
    pub fn emit_mount_error(&self, err_reason: &str) {
        let delta = self.take_duration_ms();
        let mut rec = self.record("mount_error", err_reason);
        rec.mount_duration_ms = Some(delta);
        self.writer.write(&rec);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sink_to(path: &Path) -> RuntimeMetricsSink {
        // Point the gate at a sentinel beside the log file that tests never
        // create, isolating them from the host's real sentinel.
        let sentinel = path
            .parent()
            .map(|p| p.join(".telemetry_disabled"))
            .unwrap_or_else(|| PathBuf::from("/nonexistent/.telemetry_disabled"));
        RuntimeMetricsSink::new(
            RuntimeMetricsWriter::new(path).with_sentinel(sentinel),
            "sess-1".to_string(),
            "agent".to_string(),
        )
    }

    fn read_records(path: &Path) -> Vec<serde_json::Value> {
        let content = std::fs::read_to_string(path).unwrap_or_default();
        content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).expect("valid JSON"))
            .collect()
    }

    #[test]
    fn record_has_common_fields_and_sparse_deltas() {
        let rec = RuntimeMetricRecord::new(
            "9.9.9".to_string(),
            "agent".to_string(),
            "sess".to_string(),
            "skill_hit",
            "none",
        );
        let mut rec = rec;
        rec.skill_hit_times = Some(1);
        let obj: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&rec).unwrap()).unwrap();

        assert_eq!(obj["component.name"], "skillfs");
        assert_eq!(obj["component.version"], "9.9.9");
        assert_eq!(obj["component.agent_name"], "agent");
        assert_eq!(obj["record_type"], "runtime_metric");
        assert_eq!(obj["session_id"], "sess");
        assert_eq!(obj["event_name"], "skill_hit");
        assert_eq!(obj["err_reason"], "none");
        assert_eq!(obj["skill_hit_times"], 1);
        // Unset deltas must be omitted.
        assert!(obj.as_object().unwrap().get("policy_allow_times").is_none());
        assert!(obj.as_object().unwrap().get("mount_duration_ms").is_none());
    }

    #[test]
    fn writer_appends_to_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("skillfs.jsonl");
        std::fs::File::create(&path).unwrap();

        let sink = sink_to(&path);
        sink.emit_mount_start();
        sink.record_skill_hit();

        let records = read_records(&path);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0]["event_name"], "mount_start");
        assert_eq!(records[0]["mount_times"], 1);
        assert_eq!(records[1]["event_name"], "skill_hit");
        assert_eq!(records[1]["skill_hit_times"], 1);
    }

    #[test]
    fn writer_skips_missing_file_without_creating() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("absent.jsonl");
        let sink = sink_to(&path);
        sink.record_policy_denied();
        assert!(!path.exists(), "must not create the missing file");
    }

    #[test]
    fn writer_non_fatal_on_bad_path() {
        let sink = sink_to(Path::new("/nonexistent/deep/dir/skillfs.jsonl"));
        sink.record_skill_hit(); // no panic
    }

    #[test]
    fn disabled_sentinel_suppresses_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("skillfs.jsonl");
        std::fs::File::create(&path).unwrap();

        // Sentinel present -> the writer must not append despite the log file
        // existing.
        let sentinel = dir.path().join(".telemetry_disabled");
        std::fs::File::create(&sentinel).unwrap();
        let writer = RuntimeMetricsWriter::new(&path).with_sentinel(&sentinel);
        let sink = RuntimeMetricsSink::new(writer, "sess".to_string(), "agent".to_string());

        sink.emit_mount_start();
        sink.record_skill_hit();

        assert!(
            std::fs::read_to_string(&path).unwrap().is_empty(),
            "disabled telemetry must not append any record"
        );
    }

    #[test]
    fn each_event_sets_expected_delta() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("skillfs.jsonl");
        std::fs::File::create(&path).unwrap();

        let sink = sink_to(&path);
        sink.emit_view_pruned(24, 1200);
        sink.record_policy_allow();
        sink.record_policy_fallback();
        sink.record_policy_denied();
        sink.emit_mount_end();

        let r = read_records(&path);
        assert_eq!(r[0]["event_name"], "view_pruned");
        assert_eq!(r[0]["pruned_skill_count"], 24);
        assert_eq!(r[0]["prompt_token_saved_estimate"], 1200);
        assert_eq!(r[1]["event_name"], "policy_allow");
        assert_eq!(r[1]["policy_allow_times"], 1);
        assert_eq!(r[2]["event_name"], "policy_fallback");
        assert_eq!(r[2]["policy_fallback_times"], 1);
        assert_eq!(r[3]["event_name"], "policy_denied");
        assert_eq!(r[3]["policy_denied_times"], 1);
        assert_eq!(r[4]["event_name"], "mount_end");
        assert!(r[4]["mount_duration_ms"].is_u64());
    }

    #[test]
    fn mount_error_carries_reason() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("skillfs.jsonl");
        std::fs::File::create(&path).unwrap();

        let sink = sink_to(&path);
        sink.emit_mount_error("mount failed: boom");

        let r = read_records(&path);
        assert_eq!(r[0]["event_name"], "mount_error");
        assert_eq!(r[0]["err_reason"], "mount failed: boom");
        assert!(r[0]["mount_duration_ms"].is_u64());
    }
}
