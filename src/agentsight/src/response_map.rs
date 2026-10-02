//! Response ID → Session ID Mapping
//!
//! Processes FileWrite events from the eBPF filewrite probe to extract
//! `responseId` from JSONL content and map it to `sessionId` (the UUID filename).
//!
//! # Data Flow
//!
//! ```text
//! FileWriteEvent { filename: session file (see shapes below), buf: JSONL bytes }
//!     → parse filename to extract UUID as session_id:
//!         `<UUID>.jsonl`                → the UUID stem
//!         `rollout-<ts>-<UUID>.jsonl`   → the trailing UUID
//!         `.<UUID>.<UUID>.tmp`          → the first UUID
//!     → parse buf lines as JSON, extract "responseId" field
//!     → store responseId → sessionId in LRU cache
//! ```
//!
//! Codex CLI writes its rollout file as `rollout-<ts>-<UUID>.jsonl` and does
//! not embed an LLM `response_id` inside the JSONL, so we instead remember
//! the most-recent UUID per writing pid and look it up by the HTTP call's
//! pid as a fallback.
//!
//! cosh-core persists sessions atomically: content is written to a temp file
//! named `.<session-UUID>.<random-UUID>.tmp` and then renamed to
//! `<session-UUID>.json`, so vfs_write only ever sees the temp name and the
//! session id must be recovered from its first UUID.

use std::num::NonZeroUsize;

use lru::LruCache;
use once_cell::sync::Lazy;
use regex::Regex;

use crate::probes::FileWriteEvent;

/// Maximum number of responseId → sessionId entries kept in memory.
const MAX_RESPONSE_MAP_ENTRIES: usize = 10_000;

/// Regex to match `responseId":"<value>"` or `response_id":"<value>"` in raw text.
/// Supports both camelCase (used by some agents) and snake_case (used by cosh).
static RESPONSE_ID_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"(?:responseId|response_id)":"([^"]+)"#).unwrap());

/// Regex to match Anthropic/Claude Code message id format: `"id":"msg_<uuid>"`.
/// Only matches values starting with `msg_` to avoid false positives from other "id" fields.
static ANTHROPIC_MSG_ID_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#""id":"(msg_[^"]+)"#).unwrap());

/// Regex to extract a trailing 36-char UUID from filenames like
/// `rollout-2026-06-24T20-08-10-019ef987-dbc1-7663-81b0-589cbe5e47e8.jsonl`
/// (Codex CLI session rollouts). Captured group is the UUID itself.
static TRAILING_UUID_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"([0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12})$")
        .unwrap()
});

/// Maximum number of pid → sessionId entries kept for codex-style lookups.
const MAX_PID_MAP_ENTRIES: usize = 1024;

/// Processes FileWrite events to build an in-memory responseId → sessionId mapping.
/// Uses an LRU cache to bound memory usage.
pub struct ResponseSessionMapper {
    /// responseId → sessionId (bounded by LRU eviction)
    map: LruCache<String, String>,
    /// pid → sessionId fallback used by agents (e.g. Codex CLI) whose
    /// per-session rollout file does not embed an LLM `response_id`.
    pid_map: LruCache<u32, String>,
    /// When disabled the mapper is a cheap no-op placeholder; FileWrite events
    /// are ignored and lookups always return None.  Used to gate the session
    /// mapping feature via `agentsight.json`.
    enabled: bool,
}

impl Default for ResponseSessionMapper {
    fn default() -> Self {
        Self::new()
    }
}

impl ResponseSessionMapper {
    /// Create a new empty mapper with default capacity.
    pub fn new() -> Self {
        Self::with_capacity(MAX_RESPONSE_MAP_ENTRIES)
    }

    /// Create a new empty mapper with a custom capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        let cap = NonZeroUsize::new(capacity.max(1))
            .unwrap_or_else(|| NonZeroUsize::new(MAX_RESPONSE_MAP_ENTRIES).unwrap());
        ResponseSessionMapper {
            map: LruCache::new(cap),
            pid_map: LruCache::new(NonZeroUsize::new(MAX_PID_MAP_ENTRIES).unwrap()),
            enabled: true,
        }
    }

    /// Create a disabled placeholder mapper.
    ///
    /// `process_filewrite` is a no-op and `get_session_by_response_id` always
    /// returns None, so the session-mapping feature consumes negligible memory.
    pub fn disabled() -> Self {
        ResponseSessionMapper {
            map: LruCache::new(NonZeroUsize::new(1).unwrap()),
            pid_map: LruCache::new(NonZeroUsize::new(1).unwrap()),
            enabled: false,
        }
    }

    /// Process a FileWriteEvent:
    /// 1. Extract UUID from filename as session_id
    /// 2. Parse buf as UTF-8, split by '\n'
    /// 3. For each line, try JSON parse and extract "responseId"
    /// 4. Insert responseId → sessionId into the map
    pub fn process_filewrite(&mut self, event: &FileWriteEvent) {
        if !self.enabled {
            return;
        }

        let session_id = match Self::extract_session_id(&event.filename) {
            Some(id) => id,
            None => {
                log::trace!(
                    "ResponseSessionMapper: filename not a recognized session file \
                     (UUID.jsonl, rollout-*.jsonl, or .<UUID>.<UUID>.tmp): {}",
                    event.filename
                );
                return;
            }
        };

        let text = match std::str::from_utf8(&event.buf) {
            Ok(s) => s,
            Err(e) => {
                log::trace!(
                    "ResponseSessionMapper: buf is not valid UTF-8 for {}: {}",
                    event.filename,
                    e
                );
                return;
            }
        };

        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            if let Some(response_id) = Self::extract_response_id(line) {
                log::debug!(
                    "ResponseSessionMapper: responseId={response_id} → sessionId={session_id}"
                );
                self.map.put(response_id, session_id.clone());
            }
        }

        // Always record the pid → session_id association so agents whose
        // rollout file does not embed an LLM response_id (e.g. Codex CLI)
        // can still resolve the session via their writing pid.
        self.pid_map.put(event.pid, session_id);
    }

    /// Look up sessionId by responseId.
    /// Uses `peek` (no LRU promotion) since each responseId is typically looked up only once.
    pub fn get_session_by_response_id(&self, response_id: &str) -> Option<&str> {
        self.map.peek(response_id).map(|s| s.as_str())
    }

    /// Look up sessionId by writing pid. Used as a fallback for agents
    /// (e.g. Codex CLI) whose rollout file does not embed a response_id.
    pub fn get_session_by_pid(&self, pid: u32) -> Option<&str> {
        self.pid_map.peek(&pid).map(|s| s.as_str())
    }

    /// Extract UUID from a filename like `<UUID>.jsonl` or `/path/to/<UUID>.jsonl`.
    /// Also recognizes Codex CLI's `rollout-<ts>-<UUID>.jsonl` form (trailing
    /// 36-char UUID) and cosh-core's atomic-write temp form
    /// `.<session-UUID>.<random-UUID>.tmp` (first UUID is the session id).
    fn extract_session_id(filename: &str) -> Option<String> {
        // Take the last path component
        let basename = filename.rsplit('/').next().unwrap_or(filename);

        // cosh-core atomic writes: the final `<UUID>.json` is produced by
        // rename and never hits vfs_write, so the temp name is the only place
        // the session id appears. Match the full pattern strictly (mirrors the
        // BPF-side filter) to avoid picking up unrelated dotfile temp names.
        if let Some(stem) = basename
            .strip_prefix('.')
            .and_then(|rest| rest.strip_suffix(".tmp"))
        {
            // stem = "<UUID>.<UUID>" (36 + 1 + 36 chars)
            if stem.len() == 73 && stem.as_bytes()[36] == b'.' {
                let (session, random) = (&stem[..36], &stem[37..]);
                if Self::is_uuid(session) && Self::is_uuid(random) {
                    return Some(session.to_string());
                }
            }
            return None;
        }

        // Strip .jsonl suffix
        let stem = basename.strip_suffix(".jsonl")?;

        // Plain `<UUID>.jsonl` (OpenClaw / Cosh / Claude Code)
        if stem.len() == 36 {
            return Some(stem.to_string());
        }

        // Codex CLI: `rollout-<ts>-<UUID>.jsonl` — pull the trailing UUID.
        TRAILING_UUID_RE
            .captures(stem)
            .and_then(|cap| cap.get(1))
            .map(|m| m.as_str().to_string())
    }

    /// Canonical 36-char UUID shape check: hyphens at 8/13/18/23, hex elsewhere.
    /// Mirrors `is_uuid` in `filewrite.bpf.c` so both sides accept the same set.
    fn is_uuid(s: &str) -> bool {
        s.len() == 36
            && s.bytes().enumerate().all(|(i, b)| match i {
                8 | 13 | 18 | 23 => b == b'-',
                _ => b.is_ascii_hexdigit(),
            })
    }

    /// Extract "responseId" or "response_id" value from a single JSONL line using regex.
    /// Matches patterns like `responseId":"chatcmpl-xxxx"` or `response_id":"chatcmpl-xxxx"`.
    /// Also matches Anthropic/Claude Code format: `"id":"msg_xxxx"`.
    fn extract_response_id(line: &str) -> Option<String> {
        // Try OpenAI-style responseId / response_id first
        if let Some(id) = RESPONSE_ID_RE
            .captures(line)
            .and_then(|cap| cap.get(1))
            .map(|m| m.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
        {
            return Some(id);
        }

        // Fallback: try Anthropic/Claude Code message id ("id":"msg_xxx")
        ANTHROPIC_MSG_ID_RE
            .captures(line)
            .and_then(|cap| cap.get(1))
            .map(|m| m.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_session_id_simple() {
        let id =
            ResponseSessionMapper::extract_session_id("550e8400-e29b-41d4-a716-446655440000.jsonl");
        assert_eq!(id.as_deref(), Some("550e8400-e29b-41d4-a716-446655440000"));
    }

    #[test]
    fn test_extract_session_id_codex_rollout() {
        // Codex CLI writes `rollout-<ts>-<UUID>.jsonl`; we want the trailing UUID.
        let id = ResponseSessionMapper::extract_session_id(
            "/root/.codex/sessions/2026/06/24/rollout-2026-06-24T20-08-10-019ef987-dbc1-7663-81b0-589cbe5e47e8.jsonl",
        );
        assert_eq!(id.as_deref(), Some("019ef987-dbc1-7663-81b0-589cbe5e47e8"));
    }

    #[test]
    fn test_codex_pid_to_session_lookup() {
        // Codex CLI's rollout file does not embed an LLM response_id, so the
        // mapper must fall back to a pid-based lookup.
        let mut mapper = ResponseSessionMapper::new();
        let event = FileWriteEvent {
            pid: 391658,
            tid: 391658,
            uid: 0,
            timestamp_ns: 0,
            write_size: 0,
            comm: "codex".to_string(),
            filename:
                "/root/.codex/sessions/2026/06/24/rollout-2026-06-24T20-08-10-019ef987-dbc1-7663-81b0-589cbe5e47e8.jsonl"
                    .to_string(),
            cgroup_id: 0,
            buf: br#"{"timestamp":"2026-06-24T12:08:10.991Z","type":"session_meta","payload":{"id":"019ef987-dbc1-7663-81b0-589cbe5e47e8"}}
"#
            .to_vec(),
        };
        mapper.process_filewrite(&event);
        assert_eq!(
            mapper.get_session_by_pid(391658),
            Some("019ef987-dbc1-7663-81b0-589cbe5e47e8"),
        );
        assert!(mapper.get_session_by_pid(99999).is_none());
    }

    #[test]
    fn test_extract_session_id_with_path() {
        let id = ResponseSessionMapper::extract_session_id(
            "/home/user/.agent/550e8400-e29b-41d4-a716-446655440000.jsonl",
        );
        assert_eq!(id.as_deref(), Some("550e8400-e29b-41d4-a716-446655440000"));
    }

    #[test]
    fn test_extract_session_id_not_jsonl() {
        assert!(ResponseSessionMapper::extract_session_id("file.txt").is_none());
    }

    #[test]
    fn test_extract_session_id_wrong_length() {
        assert!(ResponseSessionMapper::extract_session_id("short.jsonl").is_none());
    }

    #[test]
    fn test_extract_session_id_cosh_tmp() {
        // cosh-core atomic-write temp file: first UUID is the session id.
        let id = ResponseSessionMapper::extract_session_id(
            ".550e8400-e29b-41d4-a716-446655440000.0198f00d-1a2b-4c3d-8e4f-556677889900.tmp",
        );
        assert_eq!(id.as_deref(), Some("550e8400-e29b-41d4-a716-446655440000"));
    }

    #[test]
    fn test_extract_session_id_cosh_tmp_with_path() {
        let id = ResponseSessionMapper::extract_session_id(
            "/home/user/.cosh/sessions/.550e8400-e29b-41d4-a716-446655440000.0198f00d-1a2b-4c3d-8e4f-556677889900.tmp",
        );
        assert_eq!(id.as_deref(), Some("550e8400-e29b-41d4-a716-446655440000"));
    }

    #[test]
    fn test_extract_session_id_cosh_tmp_requires_leading_dot() {
        // Same shape without the leading dot must not match the tmp branch.
        assert!(
            ResponseSessionMapper::extract_session_id(
                "550e8400-e29b-41d4-a716-446655440000.0198f00d-1a2b-4c3d-8e4f-556677889900.tmp",
            )
            .is_none()
        );
    }

    #[test]
    fn test_extract_session_id_cosh_tmp_rejects_bad_uuid_length() {
        // First segment is 35 chars (truncated UUID) — must not match.
        assert!(
            ResponseSessionMapper::extract_session_id(
                ".550e8400-e29b-41d4-a716-44665544000.0198f00d-1a2b-4c3d-8e4f-556677889900.tmp",
            )
            .is_none()
        );
    }

    #[test]
    fn test_extract_session_id_cosh_tmp_rejects_non_hex_uuid() {
        // Second segment contains 'g' — not a valid UUID.
        assert!(
            ResponseSessionMapper::extract_session_id(
                ".550e8400-e29b-41d4-a716-446655440000.0198f00g-1a2b-4c3d-8e4f-556677889900.tmp",
            )
            .is_none()
        );
    }

    #[test]
    fn test_extract_session_id_cosh_tmp_rejects_wrong_suffix() {
        assert!(
            ResponseSessionMapper::extract_session_id(
                ".550e8400-e29b-41d4-a716-446655440000.0198f00d-1a2b-4c3d-8e4f-556677889900.txt",
            )
            .is_none()
        );
    }

    #[test]
    fn test_process_and_query_cosh_tmp() {
        // End-to-end: a cosh-core temp-file write maps response_id to the
        // session UUID taken from the temp filename's first UUID.
        let mut mapper = ResponseSessionMapper::new();
        let event = FileWriteEvent {
            pid: 4321,
            tid: 4321,
            uid: 1000,
            timestamp_ns: 0,
            write_size: 0,
            comm: "cosh".to_string(),
            filename: ".550e8400-e29b-41d4-a716-446655440000.0198f00d-1a2b-4c3d-8e4f-556677889900.tmp"
                .to_string(),
            cgroup_id: 0,
            buf: br#"{"response_id":"chatcmpl-11112222-3333-4444-5555-666677778888","model":"qwen-plus"}
"#
            .to_vec(),
        };
        mapper.process_filewrite(&event);

        assert_eq!(
            mapper.get_session_by_response_id("chatcmpl-11112222-3333-4444-5555-666677778888"),
            Some("550e8400-e29b-41d4-a716-446655440000")
        );
        assert_eq!(
            mapper.get_session_by_pid(4321),
            Some("550e8400-e29b-41d4-a716-446655440000")
        );
    }

    #[test]
    fn test_extract_response_id() {
        let line =
            r#"{"responseId":"chatcmpl-03a158a1-8982-90cd-adb1-6c8a1176f1f8","other":"data"}"#;
        let id = ResponseSessionMapper::extract_response_id(line);
        assert_eq!(
            id.as_deref(),
            Some("chatcmpl-03a158a1-8982-90cd-adb1-6c8a1176f1f8")
        );
    }

    #[test]
    fn test_extract_response_id_snake_case() {
        // cosh writes response_id in snake_case
        let line = r#"{"response_id":"chatcmpl-f2748a8e-85d0-9058-b28f-c70e6f5fd590","model":"qwen-plus"}"#;
        let id = ResponseSessionMapper::extract_response_id(line);
        assert_eq!(
            id.as_deref(),
            Some("chatcmpl-f2748a8e-85d0-9058-b28f-c70e6f5fd590")
        );
    }

    #[test]
    fn test_extract_response_id_missing() {
        let line = r#"{"other":"data"}"#;
        assert!(ResponseSessionMapper::extract_response_id(line).is_none());
    }

    #[test]
    fn test_extract_response_id_empty() {
        let line = r#"{"responseId":""}"#;
        assert!(ResponseSessionMapper::extract_response_id(line).is_none());
    }

    #[test]
    fn test_extract_response_id_invalid_json() {
        assert!(ResponseSessionMapper::extract_response_id("not json").is_none());
    }

    #[test]
    fn test_process_and_query() {
        let mut mapper = ResponseSessionMapper::new();
        let event = FileWriteEvent {
            pid: 1234,
            tid: 1234,
            uid: 1000,
            timestamp_ns: 0,
            write_size: 0,
            comm: "agent".to_string(),
            filename: "550e8400-e29b-41d4-a716-446655440000.jsonl".to_string(),
            cgroup_id: 0,
            buf: br#"{"responseId":"chatcmpl-abc123","content":"hello"}
{"responseId":"chatcmpl-def456","content":"world"}
"#
            .to_vec(),
        };
        mapper.process_filewrite(&event);

        assert_eq!(
            mapper.get_session_by_response_id("chatcmpl-abc123"),
            Some("550e8400-e29b-41d4-a716-446655440000")
        );
        assert_eq!(
            mapper.get_session_by_response_id("chatcmpl-def456"),
            Some("550e8400-e29b-41d4-a716-446655440000")
        );
        assert!(mapper.get_session_by_response_id("nonexistent").is_none());
    }

    #[test]
    fn test_process_and_query_snake_case() {
        // cosh writes response_id (snake_case) in ui_telemetry records
        let mut mapper = ResponseSessionMapper::new();
        let event = FileWriteEvent {
            pid: 5678,
            tid: 5678,
            uid: 1000,
            timestamp_ns: 0,
            write_size: 0,
            comm: "node".to_string(),
            filename: "a1b2c3d4-e5f6-7890-abcd-ef1234567890.jsonl".to_string(),
            cgroup_id: 0,
            buf: br#"{"type":"system","subtype":"ui_telemetry","systemPayload":{"uiEvent":{"event.name":"api_response","response_id":"chatcmpl-f2748a8e-85d0-9058-b28f-c70e6f5fd590","model":"qwen-plus"}}}
"#
            .to_vec(),
        };
        mapper.process_filewrite(&event);

        assert_eq!(
            mapper.get_session_by_response_id("chatcmpl-f2748a8e-85d0-9058-b28f-c70e6f5fd590"),
            Some("a1b2c3d4-e5f6-7890-abcd-ef1234567890")
        );
    }

    #[test]
    fn test_extract_response_id_anthropic_msg_id() {
        // Claude Code writes message id as "id":"msg_xxx" inside a "message" object
        let line = r#"{"message":{"model":"glm-5.1","id":"msg_72b84528-120a-4857-8c20-a3d1747c062b","role":"assistant"}}"#;
        let id = ResponseSessionMapper::extract_response_id(line);
        assert_eq!(
            id.as_deref(),
            Some("msg_72b84528-120a-4857-8c20-a3d1747c062b")
        );
    }

    #[test]
    fn test_extract_response_id_non_msg_id_ignored() {
        // Regular "id" fields (not starting with msg_) should NOT be matched
        let line = r#"{"id":"550e8400-e29b-41d4-a716-446655440000","type":"user"}"#;
        assert!(ResponseSessionMapper::extract_response_id(line).is_none());
    }

    #[test]
    fn test_process_and_query_claude_code() {
        // Claude Code writes assistant messages with "id":"msg_xxx"
        let mut mapper = ResponseSessionMapper::new();
        let event = FileWriteEvent {
            pid: 9999,
            tid: 9999,
            uid: 1000,
            timestamp_ns: 0,
            write_size: 0,
            comm: "claude".to_string(),
            filename: "002b93c6-fbc3-4c66-9a8e-4a157715c049.jsonl".to_string(),
            cgroup_id: 0,
            buf: br#"{"message":{"model":"glm-5.1","id":"msg_72b84528-120a-4857-8c20-a3d1747c062b","role":"assistant","content":[]},"type":"assistant","sessionId":"002b93c6-fbc3-4c66-9a8e-4a157715c049"}
"#
            .to_vec(),
        };
        mapper.process_filewrite(&event);

        assert_eq!(
            mapper.get_session_by_response_id("msg_72b84528-120a-4857-8c20-a3d1747c062b"),
            Some("002b93c6-fbc3-4c66-9a8e-4a157715c049")
        );
    }

    #[test]
    fn test_disabled_mapper_ignores_events() {
        let mut mapper = ResponseSessionMapper::disabled();
        let event = FileWriteEvent {
            pid: 1,
            tid: 1,
            uid: 0,
            timestamp_ns: 0,
            write_size: 0,
            comm: String::new(),
            filename: "550e8400-e29b-41d4-a716-446655440000.jsonl".to_string(),
            cgroup_id: 0,
            buf: br#"{"responseId":"resp_123"}"#.to_vec(),
        };
        mapper.process_filewrite(&event);
        assert_eq!(mapper.get_session_by_response_id("resp_123"), None);
    }

    #[test]
    fn test_with_capacity_creates_enabled_mapper() {
        let mapper = ResponseSessionMapper::with_capacity(100);
        // enabled mapper should be able to store mappings
        assert_eq!(mapper.get_session_by_response_id("nonexistent"), None);
    }

    #[test]
    fn test_disabled_mapper_get_session_by_pid_returns_none() {
        let mapper = ResponseSessionMapper::disabled();
        assert_eq!(mapper.get_session_by_pid(12345), None);
    }
}
