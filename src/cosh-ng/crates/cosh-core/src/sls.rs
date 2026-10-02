//! SLS (Simple Log Service) telemetry for cosh-ng.
//!
//! When the ANOLISA unified telemetry channel is active (`cosh.jsonl` exists),
//! records are appended to that file for the unified uploader to ship. When the
//! unified channel is absent (cosh-ng installed standalone), each record is
//! uploaded directly via SLS PutWebtracking using a fire-and-forget `tokio::spawn`.
//!
//! The unified-channel path opens with `O_WRONLY | O_APPEND` (no `O_CREAT`)
//! so it naturally no-ops when the file does not exist.

use fs2::FileExt;
use std::fs::OpenOptions;
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

#[cfg(test)]
use crate::config::ApprovalMode;
use crate::core::CoshCore;

const DEFAULT_SLS_LOG_PATH: &str = "/var/log/anolisa/sls/ops/cosh.jsonl";
const DEFAULT_SYSTEM_TELEMETRY_DISABLED_PATH: &str = "/etc/anolisa/.telemetry_disabled";
const DEFAULT_REGION: &str = "cn-hangzhou";
const HTTP_TIMEOUT_SECS: u64 = 2;
const METADATA_TIMEOUT_SECS: u64 = 1;
const METADATA_PROBE_DEADLINE_SECS: u64 = 1;

/// Resolve the SLS log path. Honors `COSH_SLS_LOG_PATH` for testing.
fn sls_log_path() -> String {
    std::env::var("COSH_SLS_LOG_PATH").unwrap_or_else(|_| DEFAULT_SLS_LOG_PATH.to_string())
}

/// Resolve the per-user telemetry opt-out sentinel path.
///
/// Defaults to `~/.copilot-shell/telemetry_disabled`. Honors
/// `COSH_TELEMETRY_DISABLED_PATH` for testing.
fn telemetry_disabled_path() -> PathBuf {
    if let Ok(p) = std::env::var("COSH_TELEMETRY_DISABLED_PATH") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home)
        .join(".copilot-shell")
        .join("telemetry_disabled")
}

/// Resolve the system-level telemetry opt-out sentinel path.
///
/// Production builds always use `/etc/anolisa/.telemetry_disabled` so a user
/// environment cannot redirect an administrator's machine-wide opt-out.
/// Unit tests may override the path without mutating host configuration.
fn system_telemetry_disabled_path() -> PathBuf {
    #[cfg(test)]
    if let Ok(p) = std::env::var("COSH_SYSTEM_TELEMETRY_DISABLED_PATH") {
        return PathBuf::from(p);
    }
    PathBuf::from(DEFAULT_SYSTEM_TELEMETRY_DISABLED_PATH)
}

/// Returns `true` when telemetry is disabled either per-user or system-wide.
///
/// Only a confirmed `ENOENT` from `symlink_metadata` means the sentinel is
/// absent. Any other error — including a dangling symlink, permission denied,
/// or any filesystem failure — is treated as present so telemetry stays off.
fn is_telemetry_disabled() -> bool {
    is_sentinel_present(&telemetry_disabled_path())
        || is_sentinel_present(&system_telemetry_disabled_path())
}

/// Returns `true` if `path` exists as a file, directory, or symlink.
///
/// Fail-closed: any error other than `NotFound` is interpreted as "present",
/// preventing telemetry when the sentinel cannot be stat'd.
fn is_sentinel_present(path: &Path) -> bool {
    match std::fs::symlink_metadata(path) {
        Ok(_) => true,
        Err(e) if e.kind() == ErrorKind::NotFound => false,
        Err(_) => true,
    }
}

/// Emit a telemetry record.
///
/// - Sentinel files `~/.copilot-shell/telemetry_disabled` or
///   `/etc/anolisa/.telemetry_disabled` exist → silently drop, returns `None`.
/// - `cosh.jsonl` exists → append to it (unified uploader ships it),
///   returns `None`.
/// - `cosh.jsonl` absent → `tokio::spawn` a fire-and-forget POST to SLS,
///   returns `Some(JoinHandle)` so short-lived callers (one-shot prompt
///   mode) can `.await` before process exit to avoid losing telemetry.
///
/// The standalone self-upload path probes the region with async TCP
/// (`tokio::net`) under an absolute deadline inside a cached `OnceCell`,
/// so the probe runs once per process and awaiting it does not occupy a
/// tokio blocking-pool thread. Must be called within a tokio runtime
/// context.
pub async fn emit(record: &serde_json::Value) -> Option<tokio::task::JoinHandle<()>> {
    if is_telemetry_disabled() {
        return None;
    }
    let path = sls_log_path();
    match std::fs::symlink_metadata(&path) {
        Ok(_) => {
            // TOCTOU: if the file is deleted between this stat and the open()
            // inside append_sls_log_to (e.g. log rotation), the append silently
            // fails and the record is lost — it does not fall back to
            // self-upload. Probability is low; the unified uploader recreates
            // the file on its next cycle and subsequent records will append
            // normally again.
            append_sls_log_to(&path, record);
            None
        }
        Err(e) if e.kind() == ErrorKind::NotFound => {
            // Unified channel is not deployed. Probe region and spawn a
            // standalone POST. The deadline and fallback both live inside
            // the cached initialization in `probe_region`, so the fallback
            // is always stored and later calls reuse it.
            let (region, use_internal) = probe_region().await;
            Some(spawn_self_upload(record.clone(), region, use_internal))
        }
        Err(_) => {
            // The channel path is present but inaccessible (EACCES) or hit a
            // transient I/O error. Only NotFound proves the unified channel
            // is absent; any other inspection failure must not silently
            // reroute to the standalone POST path. Drop the record,
            // consistent with the TOCTOU drop above when append fails.
            None
        }
    }
}

fn append_sls_log_to(path: &str, record: &serde_json::Value) {
    let Ok(line) = serde_json::to_string(record) else {
        return;
    };
    let result = OpenOptions::new().append(true).open(path);
    let Ok(mut file) = result else { return };
    let _ = writeln!(file, "{line}");
}

// ── Standalone self-upload (fire-and-forget) ───────────────────────

/// Global HTTP client reused across all spawn_self_upload tasks.
///
/// Construction normally succeeds because this crate already depends on
/// reqwest for provider integration, so the same TLS features are
/// guaranteed present at runtime. If building still fails, return `None`;
/// the upload task will silently drop that record rather than panic,
/// keeping telemetry failures from affecting the main process.
fn http_client() -> Option<reqwest::Client> {
    static CLIENT: OnceLock<Option<reqwest::Client>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
                .build()
                .ok()
        })
        .clone()
}

/// Spawn a background task that POSTs the record to SLS PutWebtracking.
/// Non-blocking; failures are silently dropped. Returns the `JoinHandle`
/// so short-lived callers can await completion before process exit.
fn spawn_self_upload(
    record: serde_json::Value,
    region: String,
    use_internal: bool,
) -> tokio::task::JoinHandle<()> {
    let body = build_upload_body(record);
    tokio::spawn(async move {
        let Some(client) = http_client() else {
            return;
        };
        let url = build_track_url(&region, use_internal);
        let _ = client
            .post(&url)
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await;
    })
}

/// Hard deadline for the metadata region probe.
///
/// Defaults to 1 second. ECS metadata is an internal VPC service with
/// millisecond-level latency, so 1s is enough for normal operation and for
/// non-ECS hosts it falls back quickly to the public cn-hangzhou endpoint.
/// Honors `COSH_METADATA_PROBE_TIMEOUT_SECS` for testing so regression tests
/// do not wait the full production deadline.
fn metadata_probe_deadline() -> Duration {
    std::env::var("COSH_METADATA_PROBE_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .map(Duration::from_secs)
        .unwrap_or(Duration::from_secs(METADATA_PROBE_DEADLINE_SECS))
}

/// Cached region-id and internal-network flag.
///
/// The cache is stored inside a `tokio::sync::OnceCell` so concurrent first
/// callers share a single in-flight probe instead of each spawning their own.
/// The cell is wrapped in a `Mutex<Option<...>>` so tests can atomically
/// replace the whole cell and reset the cache.
static REGION: std::sync::Mutex<Option<std::sync::Arc<tokio::sync::OnceCell<(String, bool)>>>> =
    std::sync::Mutex::new(None);

/// Return the shared region cache cell, creating it if necessary.
fn region_cache() -> std::sync::Arc<tokio::sync::OnceCell<(String, bool)>> {
    REGION
        .lock()
        .expect("region cache lock")
        .get_or_insert_with(std::sync::Arc::default)
        .clone()
}

/// Probe region-id and whether to use internal network.
///
/// Priority: ECS metadata API → fallback `cn-hangzhou` (public network).
/// The result is cached — the probe runs only on the first call across the
/// process lifetime (and across concurrent callers, which wait on the same
/// in-flight probe).
///
/// The underlying TCP I/O is async and bounded by an absolute deadline via
/// `tokio::time::timeout`, so a slow or drip-feeding metadata endpoint cannot
/// pin a runtime worker or the blocking pool.
async fn probe_region() -> (String, bool) {
    let cell = region_cache();
    cell.get_or_init(|| async {
        tokio::time::timeout(metadata_probe_deadline(), fetch_region_id_from_metadata())
            .await
            .ok()
            .flatten()
            .map(|region| (region, true))
            .unwrap_or_else(|| (DEFAULT_REGION.to_string(), false))
    })
    .await
    .clone()
}

#[cfg(test)]
fn reset_region_cache() {
    *REGION.lock().expect("region cache lock") = None;
}

/// Fetch region-id from the ECS metadata service via a raw TCP connection.
///
/// Uses `tokio::net` so the probe is fully async and cancellable by the
/// outer `tokio::time::timeout`, avoiding a nested blocking runtime. The
/// metadata endpoint is plain HTTP on a fixed IP, so no TLS or DNS is
/// needed.
///
/// Validates that the response status is 2xx and that the body looks like a
/// region id (letters, digits and hyphens). Any malformed response falls back
/// to `None` so the caller uses the default region instead of caching an
/// attacker-controlled value.
async fn fetch_region_id_from_metadata() -> Option<String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    const METADATA_HOST: &str = "100.100.100.200:80";
    const MAX_RESPONSE_BYTES: usize = 1024;

    let host = std::env::var("COSH_METADATA_HOST").unwrap_or_else(|_| METADATA_HOST.to_string());
    let addr: std::net::SocketAddr = host.parse().ok()?;
    let mut stream = TcpStream::connect(addr).await.ok()?;

    let request = "GET /latest/meta-data/region-id HTTP/1.1\r\nHost: 100.100.100.200\r\nConnection: close\r\n\r\n";
    stream.write_all(request.as_bytes()).await.ok()?;

    // Read the complete response. TCP may deliver headers and body in separate
    // packets, so a single read can return headers only. Loop until EOF or the
    // bounded buffer is full. The outer `tokio::time::timeout` provides the
    // absolute deadline and drops this future on expiry, closing the stream.
    let mut buf = Vec::new();
    let mut temp = [0u8; 1024];
    loop {
        let n = stream.read(&mut temp).await.ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&temp[..n]);
        if buf.len() >= MAX_RESPONSE_BYTES {
            break;
        }
    }
    let response = std::str::from_utf8(&buf).ok()?;

    let (status, body) = parse_metadata_response(response)?;
    if !(200..300).contains(&status) {
        return None;
    }
    let region = body.trim();
    if is_valid_region(region) {
        Some(region.to_string())
    } else {
        None
    }
}

/// Parse a raw HTTP response into (status_code, body).
fn parse_metadata_response(response: &str) -> Option<(u16, &str)> {
    let (head, body) = response.split_once("\r\n\r\n")?;
    let status_line = head.lines().next()?;
    let status: u16 = status_line.split_whitespace().nth(1)?.parse().ok()?;
    Some((status, body))
}

/// Returns true for strings that look like an Alibaba Cloud region id.
///
/// Allows letters, digits and hyphens so existing and future regions such as
/// `cn-hangzhou`, `cn-guangzhou`, `ap-northeast-1` and `eu-west-1` pass,
/// while rejecting error pages, JSON bodies or host-injection payloads.
fn is_valid_region(region: &str) -> bool {
    !region.is_empty()
        && region.len() <= 64
        && region
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Build the SLS PutWebtracking URL.
///
/// `project = {SLS_PROJECT_PREFIX}-{region}` (e.g. `anolisa-cn-hangzhou`).
/// Internal: `{project}.{region}-internal.log.aliyuncs.com`.
/// Public:   `{project}.{region}.log.aliyuncs.com`.
///
/// `COSH_SLS_TRACK_URL` overrides the entire URL for integration tests so
/// they can assert the request against a local server instead of the public
/// SLS endpoint.
fn build_track_url(region: &str, use_internal: bool) -> String {
    if let Ok(url) = std::env::var("COSH_SLS_TRACK_URL") {
        return url;
    }
    let prefix = std::env::var("SLS_PROJECT_PREFIX").unwrap_or_else(|_| "anolisa".to_string());
    let project = format!("{prefix}-{region}");
    let host = if use_internal {
        format!("{project}.{region}-internal.log.aliyuncs.com")
    } else {
        format!("{project}.{region}.log.aliyuncs.com")
    };
    format!("https://{host}/logstores/cosh/track")
}

/// Wrap a record into the SLS PutWebtracking `__logs__` body format.
///
/// Each log must carry `__time__` (Unix seconds). The `cosh_upload_source`
/// marker is injected to distinguish self-uploaded records from those shipped
/// via the unified uploader.
fn build_upload_body(record: serde_json::Value) -> serde_json::Value {
    let mut log = record;
    if let Some(obj) = log.as_object_mut() {
        obj.insert(
            "__time__".to_string(),
            serde_json::Value::String(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs().to_string())
                    .unwrap_or_else(|_| "0".to_string()),
            ),
        );
        // Mark as self-uploaded by cosh-ng (not via unified uploader)
        obj.insert(
            "cosh_upload_source".to_string(),
            serde_json::Value::String("cosh-ng-direct".to_string()),
        );
        // SLS PutWebtracking requires every field value in __logs__ to be
        // a string; non-string values produce PostBodyInvalid.
        *obj = stringify_log_values(std::mem::take(obj));
    } else {
        tracing::warn!("SLS record is not a JSON object; __time__ injection skipped");
    }
    serde_json::json!({
        "__logs__": [log],
        "__source__": "cosh-ng",
    })
}

/// Normalize all field values to strings, dropping `null`.
///
/// SLS PutWebtracking only accepts string values inside `__logs__`;
/// numbers, booleans, and nested structures must be serialized as strings.
fn stringify_log_values(
    obj: serde_json::Map<String, serde_json::Value>,
) -> serde_json::Map<String, serde_json::Value> {
    obj.into_iter()
        .filter_map(|(k, v)| match v {
            serde_json::Value::String(s) => Some((k, serde_json::Value::String(s))),
            serde_json::Value::Null => None,
            other => Some((k, serde_json::Value::String(other.to_string()))),
        })
        .collect()
}

/// Returns true if `s` is a valid UUID v4 string (8-4-4-4-12 hex digits).
fn is_valid_uuid(s: &str) -> bool {
    if s.len() != 36 {
        return false;
    }
    for (i, c) in s.chars().enumerate() {
        match i {
            8 | 13 | 18 | 23 => {
                if c != '-' {
                    return false;
                }
            }
            _ => {
                if !c.is_ascii_hexdigit() {
                    return false;
                }
            }
        }
    }
    true
}

/// Read or generate the persistent per-user installation ID.
///
/// Stored at `~/.copilot-shell/installation_id` (same path as copilot-shell).
/// The fast path reuses a valid persisted UUID with no locking. When the file
/// is missing or invalid, the slow path opens it, takes an exclusive `flock`,
/// re-checks (another process may have fixed it while we waited), then writes
/// our UUID with `truncate + sync`. The lock serializes all creation and
/// repair so concurrent callers converge on the one persisted UUID; a crashed
/// writer's lock is released by the kernel and the next caller repairs the
/// partial file under its own lock. Returns an empty string when telemetry
/// is disabled and a transient UUID when the path is not writable.
fn installation_id() -> String {
    #[cfg(test)]
    if std::env::var("COSH_INSTALLATION_ID_PATH").is_err() {
        panic!(
            "COSH_INSTALLATION_ID_PATH must be set in tests to avoid writing to ~/.copilot-shell/installation_id"
        );
    }
    let path = installation_id_path();

    // Fast path: reuse a valid persisted UUID without locking. The common case
    // (file already written by a previous run) never blocks on the lock.
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let trimmed = existing.trim();
        if is_valid_uuid(trimmed) {
            return trimmed.to_string();
        }
    }

    // Avoid creating a new telemetry identifier when the user has opted out.
    if is_telemetry_disabled() {
        return String::new();
    }

    // Slow path: ensure the parent directory exists, then open (create if
    // absent) and try to take an exclusive lock. The lock is non-blocking: a
    // concurrent holder (another cosh process, or one paused by a tracer or
    // stuck elsewhere) must not block the request path for best-effort
    // telemetry. On failure we return a transient UUID — the persisted file
    // still converges (the holder writes a single UUID), only this burst's
    // in-memory return values may diverge.
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut file = match OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        // Do not truncate at open: we read the existing content first, and
        // truncate later via set_len only when we must write.
        .truncate(false)
        .open(&path)
    {
        Ok(f) => f,
        // Path not writable: return a transient UUID so this call still has
        // an identifier, even though it will not be persisted.
        Err(_) => return uuid::Uuid::new_v4().to_string(),
    };
    // Non-blocking: if another process already holds the lock, do not wait —
    // return a transient UUID so this call proceeds without blocking the
    // request path. The holder is responsible for persisting a valid UUID.
    if file.try_lock_exclusive().is_err() {
        return uuid::Uuid::new_v4().to_string();
    }

    // Re-check under the lock: another process may have produced a valid UUID
    // before we acquired the lock. Seek to start first because the cursor
    // position after open/create is unspecified.
    let _ = file.seek(SeekFrom::Start(0));
    let mut buf = String::new();
    if Read::read_to_string(&mut file, &mut buf).is_ok() {
        let trimmed = buf.trim();
        if is_valid_uuid(trimmed) {
            return trimmed.to_string();
        }
    }

    // We are the sole writer. Generate a fresh UUID, truncate, write, and
    // fsync. If the process crashes here, the kernel releases the lock and
    // the next caller repairs the partial file.
    let id = uuid::Uuid::new_v4().to_string();
    let _ = file.set_len(0);
    let _ = file.seek(SeekFrom::Start(0));
    let _ = file.write_all(id.as_bytes());
    let _ = file.sync_all();
    id
}

/// Resolve the installation ID file path.
///
/// Honors `COSH_INSTALLATION_ID_PATH` for testing; defaults to
/// `~/.copilot-shell/installation_id`.
fn installation_id_path() -> PathBuf {
    if let Ok(p) = std::env::var("COSH_INSTALLATION_ID_PATH") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home)
        .join(".copilot-shell")
        .join("installation_id")
}

impl CoshCore {
    /// Build the SLS JSONL record from the accumulated turn metrics.
    /// Field names are kept identical to the copilot-shell SLS schema
    /// (`session.*` prefix) so the SLS platform can parse both sources
    /// with the same schema.  Fields not yet available output zero/empty
    /// placeholder values to keep the schema stable.
    pub fn build_sls_record(&self, _duration: Duration) -> serde_json::Value {
        let avg_await = if self.metrics.approval_count > 0 {
            self.metrics.approval_wait_ms as f64 / self.metrics.approval_count as f64 / 1000.0
        } else {
            0.0
        };
        serde_json::json!({
            // Component identification
            "component.name": "cosh",
            "component.version": env!("CARGO_PKG_VERSION"),
            "component.agent_name": "cosh-ng",
            "session.id": self.session_id,
            "installation_id": installation_id(),

            // Session configuration
            "session.model": self.model,
            "session.auth_type": self.config.resolve_provider().provider_type,
            "session.approval_mode": self.config.agent.approval_mode,

            // Audit decision counts
            "session.audit_decision_counts.approve": self.metrics.approval_allow,
            "session.audit_decision_counts.deny": self.metrics.approval_deny,
            "session.audit_decision_counts.modify": 0,  // Phase 2

            // Tool call counts
            "session.tool_call_counts.total": self.metrics.tool_calls_total,
            "session.tool_call_counts.success": self.metrics.tool_calls_success,
            "session.tool_call_counts.fail": self.metrics.tool_calls_fail,
            "session.tool_call_total_duration_seconds":
                (self.metrics.tool_calls_duration_ms as f64 / 1000.0 * 100.0).round() / 100.0,

            // Tool error counts
            "session.tool_error_counts.model_error": 0,      // Phase 2
            "session.tool_error_counts.execution_error": 0,  // Phase 2
            "session.tool_error_counts.denied": self.metrics.approval_deny,

            // Approval wait time
            "session.avg_await_duration_seconds": (avg_await * 100.0).round() / 100.0,

            // File operation stats
            "session.files.lines_added": 0,    // Phase 2
            "session.files.lines_removed": 0,  // Phase 2

            // Sandbox stats
            "session.sandbox.total_runs": self.metrics.sandbox_runs,    // Phase 2: always 0
            "session.sandbox.total_blocked": self.metrics.sandbox_blocked,

            // Token usage
            "session.tokens.input": self.metrics.tokens_input,
            "session.tokens.output": self.metrics.tokens_output,
            "session.tokens.cached": self.metrics.tokens_cached,
            "session.tokens.total": self.metrics.tokens_total,

            // API stats
            "session.api.total_requests": self.metrics.api_requests,
            "session.api.total_errors": self.metrics.api_errors,
            "session.api.total_latency_seconds":
                (self.metrics.api_latency_ms as f64 / 1000.0 * 100.0).round() / 100.0,

            // Environment info
            "os.type": std::env::consts::OS,
            "os.arch": std::env::consts::ARCH,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    /// Global temp directory backing `COSH_INSTALLATION_ID_PATH` so tests
    /// never touch `~/.copilot-shell/installation_id`.
    static TEST_ID_DIR: OnceLock<tempfile::TempDir> = OnceLock::new();

    /// Set `COSH_INSTALLATION_ID_PATH` and both telemetry sentinel paths to
    /// temp paths.
    ///
    /// The sentinel paths point at absent files so tests that expect telemetry
    /// to be enabled are isolated from the host's real opt-out sentinels.
    /// Safe under parallel test execution because `OnceLock` guarantees the
    /// temp directory is created a single time, while `set_var` is idempotent.
    fn init_test_env() {
        let dir = TEST_ID_DIR.get_or_init(|| tempfile::tempdir().expect("temp dir"));
        std::env::set_var(
            "COSH_INSTALLATION_ID_PATH",
            dir.path().join("installation_id"),
        );
        std::env::set_var(
            "COSH_TELEMETRY_DISABLED_PATH",
            dir.path().join("telemetry_disabled"),
        );
        std::env::set_var(
            "COSH_SYSTEM_TELEMETRY_DISABLED_PATH",
            dir.path().join("system_telemetry_disabled"),
        );
    }

    fn test_engine() -> CoshCore {
        init_test_env();
        let config = crate::config::CoreConfig::default();
        let provider = Box::new(crate::provider::mock::MockProvider::text_only("test"));
        let tools = crate::tool::ToolRegistry::new();
        CoshCore::new_legacy(config, provider, tools)
    }

    /// All 28 SLS fields must be present with correct types.
    #[test]
    fn build_sls_record_has_all_fields() {
        let _guard = ENV_TEST_MUTEX.lock().unwrap();
        let engine = test_engine();
        let record = engine.build_sls_record(Duration::from_millis(1234));

        // Component identification
        assert_eq!(record["component.name"], "cosh");
        assert!(record["component.version"].is_string());
        assert_eq!(record["component.agent_name"], "cosh-ng");
        assert!(record["session.id"].is_string());
        assert!(!record["installation_id"].as_str().unwrap_or("").is_empty());

        // Session configuration
        assert!(record["session.model"].is_string());
        assert!(record["session.auth_type"].is_string());
        assert!(record["session.approval_mode"].is_string());

        // Audit decision counts
        assert!(record["session.audit_decision_counts.approve"].is_number());
        assert!(record["session.audit_decision_counts.deny"].is_number());
        assert!(record["session.audit_decision_counts.modify"].is_number());

        // Tool call counts
        assert!(record["session.tool_call_counts.total"].is_number());
        assert!(record["session.tool_call_counts.success"].is_number());
        assert!(record["session.tool_call_counts.fail"].is_number());
        assert!(record["session.tool_call_total_duration_seconds"].is_number());

        // Tool error counts
        assert!(record["session.tool_error_counts.model_error"].is_number());
        assert!(record["session.tool_error_counts.execution_error"].is_number());
        assert!(record["session.tool_error_counts.denied"].is_number());

        // Approval wait time
        assert!(record["session.avg_await_duration_seconds"].is_number());

        // File stats
        assert!(record["session.files.lines_added"].is_number());
        assert!(record["session.files.lines_removed"].is_number());

        // Sandbox stats
        assert!(record["session.sandbox.total_runs"].is_number());
        assert!(record["session.sandbox.total_blocked"].is_number());

        // Token usage
        assert!(record["session.tokens.input"].is_number());
        assert!(record["session.tokens.output"].is_number());
        assert!(record["session.tokens.cached"].is_number());
        assert!(record["session.tokens.total"].is_number());

        // API stats
        assert!(record["session.api.total_requests"].is_number());
        assert!(record["session.api.total_errors"].is_number());
        assert!(record["session.api.total_latency_seconds"].is_number());

        // Environment
        assert!(record["os.type"].is_string());
        assert!(record["os.arch"].is_string());
    }

    /// Metrics accumulation is reflected in the SLS record.
    #[test]
    fn build_sls_record_reflects_metrics() {
        let _guard = ENV_TEST_MUTEX.lock().unwrap();
        let mut engine = test_engine();
        engine.metrics.tokens_input = 100;
        engine.metrics.tokens_output = 50;
        engine.metrics.tokens_total = 150;
        engine.metrics.api_requests = 3;
        engine.metrics.api_errors = 1;
        engine.metrics.api_latency_ms = 5000;
        engine.metrics.tool_calls_total = 4;
        engine.metrics.tool_calls_success = 3;
        engine.metrics.tool_calls_fail = 1;
        engine.metrics.approval_allow = 2;
        engine.metrics.approval_deny = 1;
        engine.metrics.approval_wait_ms = 6000;
        engine.metrics.approval_count = 3;

        let record = engine.build_sls_record(Duration::from_secs(10));

        assert_eq!(record["session.tokens.input"], 100);
        assert_eq!(record["session.tokens.output"], 50);
        assert_eq!(record["session.tokens.total"], 150);
        assert_eq!(record["session.api.total_requests"], 3);
        assert_eq!(record["session.api.total_errors"], 1);
        assert_eq!(record["session.api.total_latency_seconds"], 5.0);
        assert_eq!(record["session.tool_call_counts.total"], 4);
        assert_eq!(record["session.tool_call_counts.success"], 3);
        assert_eq!(record["session.tool_call_counts.fail"], 1);
        assert_eq!(record["session.audit_decision_counts.approve"], 2);
        assert_eq!(record["session.audit_decision_counts.deny"], 1);
        assert_eq!(record["session.avg_await_duration_seconds"], 2.0);
    }

    /// append_sls_log_to writes valid JSONL when file exists.
    #[test]
    fn append_sls_log_writes_jsonl() {
        let dir = tempfile::tempdir().expect("temp dir");
        let log_path = dir.path().join("cosh.jsonl");
        // Pre-create the file (simulates platform provisioning)
        std::fs::write(&log_path, "").unwrap();

        let path_str = log_path.to_str().unwrap();
        let record = serde_json::json!({"test": true, "count": 42});
        append_sls_log_to(path_str, &record);
        append_sls_log_to(path_str, &record);

        let mut content = String::new();
        std::fs::File::open(&log_path)
            .unwrap()
            .read_to_string(&mut content)
            .unwrap();

        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2, "expected 2 JSONL lines");
        for line in &lines {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(v["test"], true);
            assert_eq!(v["count"], 42);
        }
    }

    /// append_sls_log_to silently skips when file does not exist.
    #[test]
    fn append_sls_log_skips_missing_file() {
        let record = serde_json::json!({"test": true});
        // Should not panic
        append_sls_log_to("/nonexistent/path/cosh.jsonl", &record);
    }

    /// Serialize env-var-dependent tests to avoid cross-test interference.
    static ENV_TEST_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// RAII guard that saves an env var's original value on creation and
    /// restores it on drop so tests never leak state even if the test body
    /// panics.
    struct EnvVarGuard {
        key: &'static str,
        old_value: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let old_value = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, old_value }
        }

        fn remove(key: &'static str) -> Self {
            let old_value = std::env::var(key).ok();
            std::env::remove_var(key);
            Self { key, old_value }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.old_value {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    /// RAII guard that restores a directory's permissions on drop so a
    /// mode-0o000 directory cannot strand the whole TempDir tree behind on
    /// cleanup. Drop runs during unwinding too, so a panicking test never
    /// leaks the temp directory.
    #[cfg(unix)]
    struct PermsGuard {
        path: PathBuf,
        original_mode: u32,
    }

    #[cfg(unix)]
    impl Drop for PermsGuard {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(
                &self.path,
                std::fs::Permissions::from_mode(self.original_mode),
            );
        }
    }

    /// Restrict `path` so stat of its children fails with EACCES by setting
    /// mode 0o000. Returns a guard that restores the original permissions on
    /// drop, including during unwinding.
    #[cfg(unix)]
    fn restrict_dir_for_stat_failure(path: &Path) -> PermsGuard {
        use std::os::unix::fs::PermissionsExt;
        let original_mode = std::fs::metadata(path).unwrap().permissions().mode();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o000)).unwrap();
        PermsGuard {
            path: path.to_path_buf(),
            original_mode,
        }
    }

    /// emit() drops the record when the opt-out sentinel file exists.
    #[tokio::test]
    // Holding the std mutex across await is intentional: this test mutates
    // env vars and must be serialized against other tests that read them.
    #[allow(clippy::await_holding_lock)]
    async fn emit_drops_when_opt_out() {
        let _guard = ENV_TEST_MUTEX.lock().unwrap();
        init_test_env();
        let dir = tempfile::tempdir().expect("temp dir");
        let log_path = dir.path().join("cosh.jsonl");
        std::fs::write(&log_path, "").unwrap();

        // Create the sentinel file to opt out.
        let sentinel_path = dir.path().join("telemetry_disabled");
        std::fs::write(&sentinel_path, "").unwrap();

        let _log_env = EnvVarGuard::set("COSH_SLS_LOG_PATH", log_path.to_str().unwrap());
        let _sentinel_env = EnvVarGuard::set(
            "COSH_TELEMETRY_DISABLED_PATH",
            sentinel_path.to_str().unwrap(),
        );
        let _ = emit(&serde_json::json!({"test": true})).await;

        let content = std::fs::read_to_string(&log_path).unwrap();
        assert!(content.is_empty(), "record should be dropped on opt-out");
    }

    /// emit() drops the record when the system-level opt-out sentinel exists,
    /// even when the unified channel file is absent (the standalone path).
    #[tokio::test]
    // Holding the std mutex across await is intentional: this test mutates
    // env vars and must be serialized against other tests that read them.
    #[allow(clippy::await_holding_lock)]
    async fn emit_drops_when_system_opt_out() {
        let _guard = ENV_TEST_MUTEX.lock().unwrap();
        init_test_env();
        let dir = tempfile::tempdir().expect("temp dir");

        // No unified channel file: emit() would otherwise self-upload.
        let log_path = dir.path().join("cosh.jsonl");

        // Create the system-level sentinel file to opt out.
        let system_sentinel_path = dir.path().join("system_telemetry_disabled");
        std::fs::write(&system_sentinel_path, "").unwrap();

        let _log_env = EnvVarGuard::set("COSH_SLS_LOG_PATH", log_path.to_str().unwrap());
        let _system_sentinel_env = EnvVarGuard::set(
            "COSH_SYSTEM_TELEMETRY_DISABLED_PATH",
            system_sentinel_path.to_str().unwrap(),
        );
        let handle = emit(&serde_json::json!({"test": true})).await;

        assert!(
            handle.is_none(),
            "record should be dropped when system-level opt-out is set"
        );
    }

    /// The opt-out sentinel check must be fail-closed: only a confirmed
    /// ENOENT allows telemetry. Dangling symlinks, permission errors, and
    /// any other stat failure are treated as present (opted out).
    #[test]
    fn opt_out_sentinel_is_fail_closed() {
        let _guard = ENV_TEST_MUTEX.lock().unwrap();
        let dir = tempfile::tempdir().expect("temp dir");

        // Isolate from the host's system-level opt-out: point the system
        // sentinel at a controlled absent path so is_telemetry_disabled()
        // only reflects the per-user sentinel this test manipulates. The
        // system sentinel itself is exercised by emit_drops_when_system_opt_out.
        let absent_system = dir.path().join("absent_system_sentinel");
        let _system_env = EnvVarGuard::set(
            "COSH_SYSTEM_TELEMETRY_DISABLED_PATH",
            absent_system.to_str().unwrap(),
        );

        // Case 1: dangling symlink → treated as present.
        #[cfg(unix)]
        {
            let symlink_path = dir.path().join("dangling_symlink");
            std::os::unix::fs::symlink(dir.path().join("nonexistent_target"), &symlink_path)
                .unwrap();
            let _env = EnvVarGuard::set(
                "COSH_TELEMETRY_DISABLED_PATH",
                symlink_path.to_str().unwrap(),
            );
            assert!(is_telemetry_disabled(), "dangling symlink must opt out");
        }

        // Case 2: fresh install (parent directory absent) → telemetry allowed.
        let absent_parent = dir.path().join("no_such_dir").join("telemetry_disabled");
        let _env = EnvVarGuard::set(
            "COSH_TELEMETRY_DISABLED_PATH",
            absent_parent.to_str().unwrap(),
        );
        assert!(
            !is_telemetry_disabled(),
            "absent sentinel on fresh install must allow telemetry"
        );

        // Case 3: permission denied on parent directory → fail closed.
        let restricted_dir = dir.path().join("restricted");
        std::fs::create_dir(&restricted_dir).unwrap();
        let restricted_sentinel = restricted_dir.join("telemetry_disabled");
        std::fs::write(&restricted_sentinel, "").unwrap();
        // The guard restores the original mode on drop (including during
        // unwinding) so the mode-0o000 directory never strands the TempDir
        // tree behind on cleanup.
        #[cfg(unix)]
        let _perms = restrict_dir_for_stat_failure(&restricted_dir);
        #[cfg(not(unix))]
        {
            let mut perms = std::fs::metadata(&restricted_dir).unwrap().permissions();
            perms.set_readonly(true);
            std::fs::set_permissions(&restricted_dir, perms).unwrap();
        }
        let _env = EnvVarGuard::set(
            "COSH_TELEMETRY_DISABLED_PATH",
            restricted_sentinel.to_str().unwrap(),
        );
        assert!(
            is_telemetry_disabled(),
            "permission denied must fail closed"
        );

        // Restore permissions explicitly so we can assert the temp tree is
        // still removable; the guard also runs on drop if this is not reached.
        #[cfg(unix)]
        {
            drop(_perms);
            assert!(
                std::fs::remove_dir_all(dir.path()).is_ok(),
                "temp tree must remain removable after restoring permissions"
            );
        }
    }

    /// emit() writes to cosh.jsonl when it exists (unified channel).
    #[tokio::test]
    // Holding the std mutex across await is intentional: this test mutates
    // env vars and must be serialized against other tests that read them.
    #[allow(clippy::await_holding_lock)]
    async fn emit_writes_to_unified_channel() {
        let _guard = ENV_TEST_MUTEX.lock().unwrap();
        init_test_env();
        let dir = tempfile::tempdir().expect("temp dir");
        let log_path = dir.path().join("cosh.jsonl");
        std::fs::write(&log_path, "").unwrap();

        let _log_env = EnvVarGuard::set("COSH_SLS_LOG_PATH", log_path.to_str().unwrap());
        // Ensure the sentinel does not exist so telemetry stays enabled.
        let _sentinel_env = EnvVarGuard::set(
            "COSH_TELEMETRY_DISABLED_PATH",
            dir.path().join("nonexistent").to_str().unwrap(),
        );
        let _ = emit(&serde_json::json!({"test": true})).await;

        let content = std::fs::read_to_string(&log_path).unwrap();
        assert!(
            !content.is_empty(),
            "record should be written to unified channel"
        );
        let v: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
        assert_eq!(v["test"], true);
    }

    /// An inaccessible unified-channel path (EACCES, not NotFound) must not
    /// silently reroute to the standalone POST path. Only a confirmed-absent
    /// channel selects self-upload; other inspection failures drop the record,
    /// consistent with the TOCTOU drop when append fails.
    #[tokio::test]
    // Holding the std mutex across await is intentional: this test mutates
    // env vars and must be serialized against other tests that read them.
    #[allow(clippy::await_holding_lock)]
    async fn emit_drops_when_channel_path_is_inaccessible() {
        let _guard = ENV_TEST_MUTEX.lock().unwrap();
        init_test_env();
        let dir = tempfile::tempdir().expect("temp dir");

        // Telemetry enabled: both opt-out sentinels point at absent paths.
        let _user_env = EnvVarGuard::set(
            "COSH_TELEMETRY_DISABLED_PATH",
            dir.path().join("absent_user").to_str().unwrap(),
        );
        let _system_env = EnvVarGuard::set(
            "COSH_SYSTEM_TELEMETRY_DISABLED_PATH",
            dir.path().join("absent_system").to_str().unwrap(),
        );
        // Non-routable metadata host so a regression that reaches the probe
        // fails fast (connection refused) instead of a 1s timeout.
        let _metadata_env = EnvVarGuard::set("COSH_METADATA_HOST", "127.0.0.1:1");

        // Channel file exists, but its parent dir is mode 0o000 so
        // symlink_metadata() returns EACCES, not NotFound.
        let restricted = dir.path().join("restricted");
        std::fs::create_dir(&restricted).unwrap();
        let channel = restricted.join("cosh.jsonl");
        std::fs::write(&channel, "").unwrap();
        let _log_env = EnvVarGuard::set("COSH_SLS_LOG_PATH", channel.to_str().unwrap());
        // The guard restores the original mode on drop (including during
        // unwinding) so the mode-0o000 directory never strands the TempDir
        // tree behind on cleanup.
        #[cfg(unix)]
        let _perms = restrict_dir_for_stat_failure(&restricted);
        #[cfg(not(unix))]
        {
            let mut perms = std::fs::metadata(&restricted).unwrap().permissions();
            perms.set_readonly(true);
            std::fs::set_permissions(&restricted, perms).unwrap();
        }
        // Running as root bypasses mode 0o000 on unix, so metadata still
        // succeeds and the test cannot exercise EACCES — skip. The guard
        // still restores permissions on drop.
        #[cfg(unix)]
        {
            if std::fs::metadata(&channel).is_ok() {
                return;
            }
        }

        let handle = emit(&serde_json::json!({"test": true})).await;
        assert!(
            handle.is_none(),
            "inaccessible channel path must not reroute to standalone self-upload"
        );

        // Restore permissions explicitly so we can assert the temp tree is
        // still removable; the guard also runs on drop if this is not reached.
        #[cfg(unix)]
        {
            drop(_perms);
            assert!(
                std::fs::remove_dir_all(dir.path()).is_ok(),
                "temp tree must remain removable after restoring permissions"
            );
        }
    }

    /// A dangling unified-channel symlink is a provisioned but unavailable
    /// channel, not proof that the deployment has no unified uploader. It must
    /// be dropped instead of rerouted to standalone self-upload.
    #[cfg(unix)]
    #[tokio::test]
    // Holding the std mutex across await is intentional: this test mutates
    // env vars and must be serialized against other tests that read them.
    #[allow(clippy::await_holding_lock)]
    async fn emit_drops_when_channel_is_dangling_symlink() {
        let _guard = ENV_TEST_MUTEX.lock().unwrap();
        init_test_env();
        let dir = tempfile::tempdir().expect("temp dir");

        let _user_env = EnvVarGuard::set(
            "COSH_TELEMETRY_DISABLED_PATH",
            dir.path().join("absent_user").to_str().unwrap(),
        );
        let _system_env = EnvVarGuard::set(
            "COSH_SYSTEM_TELEMETRY_DISABLED_PATH",
            dir.path().join("absent_system").to_str().unwrap(),
        );
        let _metadata_env = EnvVarGuard::set("COSH_METADATA_HOST", "127.0.0.1:1");
        let _upload_env = EnvVarGuard::set(
            "COSH_SLS_TRACK_URL",
            "http://127.0.0.1:1/logstores/cosh/track",
        );

        let channel = dir.path().join("cosh.jsonl");
        std::os::unix::fs::symlink(dir.path().join("missing-target"), &channel).unwrap();
        assert!(
            std::fs::symlink_metadata(&channel).is_ok(),
            "fixture must contain a channel directory entry"
        );
        assert_eq!(
            std::fs::metadata(&channel).unwrap_err().kind(),
            ErrorKind::NotFound,
            "following the dangling channel symlink must report NotFound"
        );
        let _log_env = EnvVarGuard::set("COSH_SLS_LOG_PATH", channel.to_str().unwrap());

        let handle = emit(&serde_json::json!({"test": true})).await;
        assert!(
            handle.is_none(),
            "dangling unified-channel symlink must not reroute to standalone self-upload"
        );
    }

    /// A stalled metadata server must not prevent timers on a single-worker
    /// tokio runtime from advancing, because the probe awaits an async TCP
    /// read bounded by `tokio::time::timeout`.
    #[tokio::test(flavor = "current_thread")]
    // Holding the std mutex across await is intentional: this test mutates
    // env vars and must be serialized against other tests that read them.
    #[allow(clippy::await_holding_lock)]
    async fn emit_probe_does_not_block_runtime_worker() {
        let _guard = ENV_TEST_MUTEX.lock().unwrap();
        init_test_env();
        reset_region_cache();

        // Metadata server accepts the TCP connection but never responds,
        // so the probe waits on read() until its deadline.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let _metadata_env =
            EnvVarGuard::set("COSH_METADATA_HOST", &format!("127.0.0.1:{}", addr.port()));
        let _deadline_env = EnvVarGuard::set("COSH_METADATA_PROBE_TIMEOUT_SECS", "1");
        // Point the upload target at a local non-routable address so the
        // spawned POST cannot reach production SLS.
        let _upload_env = EnvVarGuard::set(
            "COSH_SLS_TRACK_URL",
            "http://127.0.0.1:1/logstores/cosh/track",
        );

        std::thread::spawn(move || {
            if let Ok((_, _)) = listener.accept() {
                // Hold the connection open to stall the read.
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
        });

        let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag_clone = flag.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            flag_clone.store(true, std::sync::atomic::Ordering::SeqCst);
        });

        let start = std::time::Instant::now();
        let _ = emit(&serde_json::json!({"test": true})).await;
        let elapsed = start.elapsed();

        assert!(
            flag.load(std::sync::atomic::Ordering::SeqCst),
            "timer must advance while emit awaits the async probe"
        );
        assert!(
            elapsed < std::time::Duration::from_millis(1500),
            "emit should return within the probe deadline, took {elapsed:?}"
        );
    }

    /// A slow-drip metadata server (one byte per read) must still be bounded
    /// by the absolute probe deadline and must not consume the blocking pool.
    /// Concurrent `emit()` calls share the in-flight probe; the second call
    /// returns immediately from cache.
    #[tokio::test(flavor = "current_thread")]
    // Holding the std mutex across await is intentional: this test mutates
    // env vars and must be serialized against other tests that read them.
    #[allow(clippy::await_holding_lock)]
    async fn emit_probe_respects_deadline_against_slow_drip() {
        let _guard = ENV_TEST_MUTEX.lock().unwrap();
        init_test_env();
        reset_region_cache();

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let _metadata_env =
            EnvVarGuard::set("COSH_METADATA_HOST", &format!("127.0.0.1:{}", addr.port()));
        let _deadline_env = EnvVarGuard::set("COSH_METADATA_PROBE_TIMEOUT_SECS", "1");
        let _upload_env = EnvVarGuard::set(
            "COSH_SLS_TRACK_URL",
            "http://127.0.0.1:1/logstores/cosh/track",
        );

        // Drip server: accept repeatedly so every cache-reset probe across the
        // loop below gets a fresh slow connection. One byte every 200 ms keeps
        // a synchronous read alive, but the absolute async deadline inside the
        // cached initialization must still abort the probe. A write error (peer
        // closed after timeout) ends the per-connection drip.
        std::thread::spawn(move || {
            while let Ok((mut stream, _)) = listener.accept() {
                for _ in 0..20 {
                    if stream.write_all(b" ").is_err() {
                        break;
                    }
                    let _ = stream.flush();
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
            }
        });

        // Repeat the probe + cached-reuse pair several times to catch timing
        // races around the deadline. The fallback must always be stored on the
        // first probe; every subsequent emit must hit the cache fast.
        for i in 0..5 {
            reset_region_cache();

            let start = std::time::Instant::now();
            let _ = emit(&serde_json::json!({"test": true})).await;
            let elapsed = start.elapsed();
            assert!(
                elapsed < std::time::Duration::from_millis(1500),
                "iter {i}: slow-drip probe must respect deadline, took {elapsed:?}"
            );

            // A second emit must reuse the cached fallback region and return
            // immediately, proving the in-flight probe was shared and stored.
            let start2 = std::time::Instant::now();
            let _ = emit(&serde_json::json!({"test": true})).await;
            let elapsed2 = start2.elapsed();
            assert!(
                elapsed2 < std::time::Duration::from_millis(200),
                "iter {i}: second emit must use cached region, took {elapsed2:?}"
            );
        }
    }

    /// build_track_url formats correctly for internal and public endpoints.
    #[test]
    fn build_track_url_internal_and_public() {
        let _guard = ENV_TEST_MUTEX.lock().unwrap();
        let _prefix_env = EnvVarGuard::remove("SLS_PROJECT_PREFIX");

        let url = build_track_url("cn-hangzhou", true);
        assert!(url.contains("anolisa-cn-hangzhou.cn-hangzhou-internal.log.aliyuncs.com"));
        assert!(url.ends_with("/logstores/cosh/track"));

        let url = build_track_url("us-west-1", false);
        assert!(url.contains("anolisa-us-west-1.us-west-1.log.aliyuncs.com"));
        assert!(!url.contains("-internal"));
    }

    /// fetch_region_id_from_metadata accepts a 200 response with a valid region.
    #[tokio::test]
    // Holding the std mutex across await is intentional: this test mutates
    // env vars and must be serialized against other tests that read them.
    #[allow(clippy::await_holding_lock)]
    async fn fetch_region_id_accepts_valid_region() {
        let _guard = ENV_TEST_MUTEX.lock().unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let _env = EnvVarGuard::set("COSH_METADATA_HOST", &format!("127.0.0.1:{}", addr.port()));

        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 256];
                let _ = stream.read(&mut buf);
                let _ =
                    stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 12\r\n\r\ncn-hangzhou");
            }
        });

        assert_eq!(
            fetch_region_id_from_metadata().await,
            Some("cn-hangzhou".to_string())
        );
    }

    /// fetch_region_id_from_metadata rejects non-2xx responses.
    #[tokio::test]
    // Holding the std mutex across await is intentional: this test mutates
    // env vars and must be serialized against other tests that read them.
    #[allow(clippy::await_holding_lock)]
    async fn fetch_region_id_rejects_403() {
        let _guard = ENV_TEST_MUTEX.lock().unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let _env = EnvVarGuard::set("COSH_METADATA_HOST", &format!("127.0.0.1:{}", addr.port()));

        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 256];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n");
            }
        });

        assert_eq!(fetch_region_id_from_metadata().await, None);
    }

    /// fetch_region_id_from_metadata rejects bodies that are not valid region ids.
    #[tokio::test]
    // Holding the std mutex across await is intentional: this test mutates
    // env vars and must be serialized against other tests that read them.
    #[allow(clippy::await_holding_lock)]
    async fn fetch_region_id_rejects_invalid_body() {
        let _guard = ENV_TEST_MUTEX.lock().unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let _env = EnvVarGuard::set("COSH_METADATA_HOST", &format!("127.0.0.1:{}", addr.port()));

        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 256];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 25\r\n\r\ncn-hangzhou-internal.evil",
                );
            }
        });

        assert_eq!(fetch_region_id_from_metadata().await, None);
    }

    /// fetch_region_id_from_metadata reads headers and body even when they are
    /// delivered in separate TCP packets.
    #[tokio::test]
    // Holding the std mutex across await is intentional: this test mutates
    // env vars and must be serialized against other tests that read them.
    #[allow(clippy::await_holding_lock)]
    async fn fetch_region_id_reads_split_response() {
        let _guard = ENV_TEST_MUTEX.lock().unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let _env = EnvVarGuard::set("COSH_METADATA_HOST", &format!("127.0.0.1:{}", addr.port()));

        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 256];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 12\r\n\r\n");
                let _ = stream.flush();
                // Sleep to ensure the client receives headers before the body.
                std::thread::sleep(std::time::Duration::from_millis(50));
                let _ = stream.write_all(b"cn-hangzhou");
            }
        });

        assert_eq!(
            fetch_region_id_from_metadata().await,
            Some("cn-hangzhou".to_string())
        );
    }

    /// build_upload_body wraps record in __logs__ with __time__ and cosh_upload_source.
    #[test]
    fn build_upload_body_has_required_fields() {
        let record =
            serde_json::json!({"component.name": "cosh", "installation_id": "test-uuid-123"});
        let body = build_upload_body(record);

        assert!(body["__logs__"].is_array());
        assert_eq!(body["__source__"], "cosh-ng");

        let log = &body["__logs__"][0];
        assert!(log["__time__"].is_string());
        assert_eq!(log["cosh_upload_source"], "cosh-ng-direct");
        assert_eq!(log["installation_id"], "test-uuid-123");
        assert_eq!(log["component.name"], "cosh");
    }

    /// installation_id persists and reuses the same UUID across calls.
    #[test]
    fn installation_id_persists_and_reuses() {
        let _guard = ENV_TEST_MUTEX.lock().unwrap();
        init_test_env();
        let dir = tempfile::tempdir().expect("temp dir");
        let id_path = dir.path().join("installation_id");
        let _env = EnvVarGuard::set("COSH_INSTALLATION_ID_PATH", id_path.to_str().unwrap());

        // Write a known valid UUID and verify it reads back.
        let known = "12345678-1234-1234-1234-123456789abc";
        std::fs::write(&id_path, known).unwrap();
        let id = installation_id();
        assert_eq!(id, known);

        // Missing file → generates a new UUID
        std::fs::remove_file(&id_path).unwrap();
        let id2 = installation_id();
        assert!(!id2.is_empty());
        assert_ne!(id2, known);
        // Now persisted
        let id3 = installation_id();
        assert_eq!(id2, id3, "second read must return persisted UUID");
    }

    /// installation_id repairs an empty or invalid persisted file.
    #[test]
    fn installation_id_repairs_empty_or_invalid_file() {
        let _guard = ENV_TEST_MUTEX.lock().unwrap();
        init_test_env();
        let dir = tempfile::tempdir().expect("temp dir");
        let id_path = dir.path().join("installation_id");
        let _env = EnvVarGuard::set("COSH_INSTALLATION_ID_PATH", id_path.to_str().unwrap());

        // Empty file should be repaired.
        std::fs::write(&id_path, "").unwrap();
        let id = installation_id();
        assert!(
            is_valid_uuid(&id),
            "empty file must be replaced with a valid UUID"
        );
        let persisted = std::fs::read_to_string(&id_path).unwrap();
        assert_eq!(id, persisted.trim());

        // Invalid (partial) UUID should also be repaired.
        std::fs::write(&id_path, "12345678-1234-1234-1234-123456789ab").unwrap();
        let id2 = installation_id();
        assert!(
            is_valid_uuid(&id2),
            "invalid file must be replaced with a valid UUID"
        );
        assert_ne!(id, id2, "repair should generate a fresh UUID");
        let persisted2 = std::fs::read_to_string(&id_path).unwrap();
        assert_eq!(id2, persisted2.trim());
    }

    /// installation_id() must not create the file when telemetry is disabled.
    #[test]
    fn installation_id_not_created_when_opted_out() {
        let _guard = ENV_TEST_MUTEX.lock().unwrap();
        init_test_env();
        let dir = tempfile::tempdir().expect("temp dir");
        let id_path = dir.path().join("installation_id");
        let sentinel_path = dir.path().join("telemetry_disabled");
        std::fs::write(&sentinel_path, "").unwrap();

        let _id_env = EnvVarGuard::set("COSH_INSTALLATION_ID_PATH", id_path.to_str().unwrap());
        let _sentinel_env = EnvVarGuard::set(
            "COSH_TELEMETRY_DISABLED_PATH",
            sentinel_path.to_str().unwrap(),
        );

        let id = installation_id();
        assert!(id.is_empty(), "opt-out must return empty installation_id");
        assert!(
            !id_path.exists(),
            "opt-out must not create installation_id file"
        );
    }

    /// Concurrent first-start calls: the lock holder persists a single UUID;
    /// concurrent losers (try-lock fails) return transient UUIDs without
    /// blocking. Asserts the persisted file converges on one valid UUID and
    /// the holder's return matches it.
    #[test]
    fn installation_id_converges_on_concurrent_first_start() {
        let _guard = ENV_TEST_MUTEX.lock().unwrap();
        init_test_env();
        let dir = tempfile::tempdir().expect("temp dir");
        let id_path = dir.path().join("installation_id");
        let _env = EnvVarGuard::set("COSH_INSTALLATION_ID_PATH", id_path.to_str().unwrap());

        let mut handles = Vec::new();
        for _ in 0..10 {
            handles.push(std::thread::spawn(installation_id));
        }
        let ids: Vec<String> = handles.into_iter().map(|h| h.join().unwrap()).collect();

        // Every call returns a valid UUID: the holder returns the persisted
        // one; losers return transient UUIDs without blocking on the lock.
        assert!(
            ids.iter().all(|id| is_valid_uuid(id)),
            "all ids must be valid UUIDs: {ids:?}"
        );

        // The persisted file converges on a single UUID written by the holder,
        // and the holder's return value matches it.
        let persisted = std::fs::read_to_string(&id_path).unwrap();
        let persisted = persisted.trim();
        assert!(
            is_valid_uuid(persisted),
            "persisted id must be valid: {persisted}"
        );
        assert!(
            ids.iter().any(|id| id.as_str() == persisted),
            "at least one call must return the persisted id: persisted={persisted}, ids={ids:?}"
        );
    }

    /// Concurrent callers when the existing file is broken (simulating a
    /// creator that crashed mid-write): the lock holder repairs and persists a
    /// valid UUID; concurrent losers (try-lock fails) return transient UUIDs
    /// without blocking. Asserts the persisted file converges on one repaired
    /// UUID and the holder's return matches it.
    #[test]
    fn installation_id_converges_on_concurrent_repair_of_broken_file() {
        let _guard = ENV_TEST_MUTEX.lock().unwrap();
        init_test_env();
        let dir = tempfile::tempdir().expect("temp dir");
        let id_path = dir.path().join("installation_id");
        let _env = EnvVarGuard::set("COSH_INSTALLATION_ID_PATH", id_path.to_str().unwrap());

        // Simulate a crashed creator: the file exists but is empty.
        std::fs::write(&id_path, "").unwrap();

        let mut handles = Vec::new();
        for _ in 0..10 {
            handles.push(std::thread::spawn(installation_id));
        }
        let ids: Vec<String> = handles.into_iter().map(|h| h.join().unwrap()).collect();

        // Every call returns a valid UUID; the holder repairs and persists
        // one, losers return transient UUIDs without blocking.
        assert!(
            ids.iter().all(|id| is_valid_uuid(id)),
            "all ids must be valid UUIDs: {ids:?}"
        );

        let persisted = std::fs::read_to_string(&id_path).unwrap();
        let persisted = persisted.trim();
        assert!(
            is_valid_uuid(persisted),
            "repaired persisted id must be valid: {persisted}"
        );
        assert!(
            ids.iter().any(|id| id.as_str() == persisted),
            "at least one call must return the repaired persisted id: persisted={persisted}, ids={ids:?}"
        );
    }

    /// When another process already holds the exclusive lock on the ID file,
    /// installation_id() must return a transient UUID promptly instead of
    /// blocking on `flock`. Guards against a stuck or paused holder keeping
    /// the one-shot CLI from exiting or the interactive loop from accepting
    /// its next input.
    #[test]
    fn installation_id_returns_promptly_when_lock_held() {
        let _guard = ENV_TEST_MUTEX.lock().unwrap();
        init_test_env();
        let dir = tempfile::tempdir().expect("temp dir");
        let id_path = dir.path().join("installation_id");
        let _env = EnvVarGuard::set("COSH_INSTALLATION_ID_PATH", id_path.to_str().unwrap());

        // No valid persisted UUID → slow path. A helper opens the file and
        // holds an exclusive flock so try-lock must fail; installation_id()
        // must not block waiting for it.
        let helper = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&id_path)
            .unwrap();
        assert!(helper.lock_exclusive().is_ok(), "helper must hold the lock");

        let start = std::time::Instant::now();
        let id = installation_id();
        let elapsed = start.elapsed();

        assert!(
            elapsed < std::time::Duration::from_millis(500),
            "installation_id must not block when the lock is held, took {elapsed:?}"
        );
        assert!(
            is_valid_uuid(&id),
            "transient id must be a valid UUID, got {id}"
        );
    }

    /// stringify_log_values converts non-string types to strings and drops null.
    #[test]
    fn build_upload_body_stringifies_non_string_values() {
        let record = serde_json::json!({
            "count": 42,
            "flag": true,
            "nullable": null,
            "name": "cosh"
        });
        let body = build_upload_body(record);
        let log = &body["__logs__"][0];

        assert!(log["count"].is_string());
        assert_eq!(log["count"], "42");
        assert!(log["flag"].is_string());
        assert_eq!(log["flag"], "true");
        assert!(log.get("nullable").is_none(), "null values must be dropped");
        assert!(log["name"].is_string());
        assert_eq!(log["name"], "cosh");
    }

    /// Multiple provider calls in one turn (tool call triggers second call)
    /// must accumulate cached_tokens across calls. Also covers the single-call
    /// case: if the first call's value (30) were dropped, the sum would be 20
    /// instead of 50.
    #[tokio::test]
    // Holding the std mutex across await is intentional: this test reads
    // COSH_INSTALLATION_ID_PATH and must be serialized against other tests
    // that mutate env vars.
    #[allow(clippy::await_holding_lock)]
    async fn sls_record_accumulates_cached_tokens_across_tool_calls() {
        use crate::provider::GenerateEvent;
        use tokio::io::{AsyncBufReadExt, BufReader};

        let _guard = ENV_TEST_MUTEX.lock().unwrap();
        init_test_env();

        let provider = crate::provider::mock::MockProvider::new(vec![
            vec![
                GenerateEvent::TextDelta("Running".to_string()),
                GenerateEvent::ToolCallStart {
                    index: 0,
                    id: "call-1".to_string(),
                    name: "shell".to_string(),
                },
                GenerateEvent::ToolCallDelta {
                    index: 0,
                    arguments_delta: r#"{"command":"echo hello"}"#.to_string(),
                },
                GenerateEvent::ToolCallEnd { index: 0 },
                GenerateEvent::Usage {
                    prompt_tokens: 100,
                    completion_tokens: 10,
                    total_tokens: 110,
                    cached_tokens: 30,
                },
                GenerateEvent::MessageEnd,
            ],
            vec![
                GenerateEvent::TextDelta("Done".to_string()),
                GenerateEvent::Usage {
                    prompt_tokens: 200,
                    completion_tokens: 20,
                    total_tokens: 220,
                    cached_tokens: 20,
                },
                GenerateEvent::MessageEnd,
            ],
        ]);

        let mut config = crate::config::CoreConfig::default();
        config.agent.approval_mode = ApprovalMode::Trust;
        let tools = crate::tool::ToolRegistry::with_defaults_for_test();
        let mut engine = CoshCore::new_legacy(config, Box::new(provider), tools);

        let mut reader = BufReader::new(&b""[..]).lines();
        let mut output = Vec::new();
        engine
            .handle_user_message("run echo hello", &mut reader, &mut output)
            .await
            .unwrap();

        let record = engine.build_sls_record(Duration::from_secs(2));
        assert_eq!(record["session.tokens.cached"], 50);
        assert_eq!(record["session.tokens.input"], 300);
        assert_eq!(record["session.tokens.output"], 30);
    }
}
