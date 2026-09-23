use std::fs::OpenOptions;
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;
use ws_ckpt_common::{Request, Response};

const OPS_LOG_PATH: &str = "/var/log/anolisa/sls/ops/ws-ckpt.jsonl";
const TELEMETRY_GATE: &str = "/etc/anolisa/.telemetry_disabled";
const KNOWN_AGENTS: &[&str] = &["user", "hermes", "openclaw"];
static OPS_SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Serialize)]
struct OpsRecord<'a> {
    #[serde(rename = "component.name")]
    component_name: &'static str,
    #[serde(rename = "component.version")]
    component_version: &'static str,
    #[serde(rename = "component.agent_name")]
    component_agent_name: &'a str,
    ops_id: String,
    ops_name: &'static str,
    ckpt_time: u32,
    roll_time: u32,
    diff_time: u32,
    list_time: u32,
    ops_time: u32,
    err_reason: &'a str,
    supply: &'a str,
}

pub fn ops_name_from_request(req: &Request) -> Option<&'static str> {
    match req {
        Request::Checkpoint { .. } | Request::GuardedCheckpointV2 { .. } => Some("ckpt"),
        Request::Rollback { .. } => Some("roll"),
        Request::Diff { .. } => Some("diff"),
        Request::List { .. } | Request::ListOrphans { .. } => Some("list"),
        Request::Config
        | Request::ReloadConfig
        | Request::ReloadGlobalConfig
        | Request::ConfigOverview
        | Request::GetWorkspacePolicy { .. }
        | Request::ResetWorkspacePolicy { .. }
        | Request::PatchWorkspacePolicy { .. }
        | Request::ReloadWorkspacePolicy { .. } => Some("config"),
        _ => None,
    }
}

/// Read `WS_CKPT_AGENT_NAME` from `/proc/{pid}/environ`.
/// Env unset → `"user"` (direct CLI). Env set but not in whitelist → `"unknown"`.
pub fn detect_agent_name(pid: u32) -> String {
    let path = format!("/proc/{pid}/environ");
    let Ok(data) = std::fs::read(&path) else {
        return "user".to_string();
    };
    for entry in data.split(|&b| b == 0) {
        if let Some(val) = entry.strip_prefix(b"WS_CKPT_AGENT_NAME=") {
            return match std::str::from_utf8(val) {
                Ok(s) if KNOWN_AGENTS.contains(&s) => s.to_string(),
                _ => "unknown".to_string(),
            };
        }
    }
    "user".to_string()
}

pub fn log_operation(ops_name: &'static str, agent_name: &str, response: &Response) {
    if std::path::Path::new(TELEMETRY_GATE).exists() || !std::path::Path::new(OPS_LOG_PATH).exists()
    {
        return;
    }

    let err_reason = match response {
        Response::Error { message, .. } | Response::GuardedCheckpointV2Rejected { message, .. } => {
            message.as_str()
        }
        _ => "none",
    };

    let record = OpsRecord {
        component_name: "ws-ckpt",
        component_version: env!("CARGO_PKG_VERSION"),
        component_agent_name: agent_name,
        ops_id: format!(
            "{}-{}-{}",
            chrono::Utc::now().timestamp_millis(),
            std::process::id(),
            OPS_SEQ.fetch_add(1, Ordering::Relaxed),
        ),
        ops_name,
        ckpt_time: u32::from(ops_name == "ckpt"),
        roll_time: u32::from(ops_name == "roll"),
        diff_time: u32::from(ops_name == "diff"),
        list_time: u32::from(ops_name == "list"),
        ops_time: 1,
        err_reason,
        supply: "none",
    };

    if let Err(e) = write_record(&record) {
        tracing::debug!("ops log write failed: {e}");
    }
}

fn write_record(record: &OpsRecord<'_>) -> std::io::Result<()> {
    let mut line = serde_json::to_string(record)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    line.push('\n');

    let mut file = OpenOptions::new().append(true).open(OPS_LOG_PATH)?;
    file.write_all(line.as_bytes())?;
    file.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ops_name_mapping() {
        let ckpt = Request::Checkpoint {
            workspace: "w".into(),
            id: "id".into(),
            message: None,
            metadata: None,
            pin: false,
        };
        assert_eq!(ops_name_from_request(&ckpt), Some("ckpt"));

        let roll = Request::Rollback {
            workspace: "w".into(),
            to: None,
            num_ancestors: None,
        };
        assert_eq!(ops_name_from_request(&roll), Some("roll"));

        let diff = Request::Diff {
            workspace: "w".into(),
            from: "a".into(),
            to: None,
        };
        assert_eq!(ops_name_from_request(&diff), Some("diff"));

        let list = Request::List {
            workspace: Some("w".into()),
            format: None,
        };
        assert_eq!(ops_name_from_request(&list), Some("list"));

        assert_eq!(ops_name_from_request(&Request::Config), Some("config"));
        assert_eq!(
            ops_name_from_request(&Request::ReloadConfig),
            Some("config")
        );
        assert_eq!(
            ops_name_from_request(&Request::ConfigOverview),
            Some("config")
        );

        let init = Request::Init {
            workspace: "w".into(),
        };
        assert_eq!(ops_name_from_request(&init), None);

        assert_eq!(ops_name_from_request(&Request::HealthAdvisory), None);

        let guarded = Request::GuardedCheckpointV2 {
            ws_id: "ws-abcdef".into(),
            expected_generation: ws_ckpt_common::WorkspaceGenerationTokenV2::from_bytes([1; 32]),
            checkpoint_id: "checkpoint-1".into(),
            operation_digest: [2; 32],
            message: None,
            metadata: None,
            pin: false,
        };
        assert_eq!(ops_name_from_request(&guarded), Some("ckpt"));

        let identity = Request::WorkspaceIdentityV2 {
            registration_path: "/workspace".into(),
        };
        assert_eq!(ops_name_from_request(&identity), None);

        let evidence = Request::CheckpointEvidenceV2 {
            ws_id: "ws-abcdef".into(),
            expected_generation: ws_ckpt_common::WorkspaceGenerationTokenV2::from_bytes([1; 32]),
            checkpoint_id: "checkpoint-1".into(),
            operation_digest: [2; 32],
        };
        assert_eq!(ops_name_from_request(&evidence), None);
    }

    #[test]
    fn record_serialization() {
        let record = OpsRecord {
            component_name: "ws-ckpt",
            component_version: env!("CARGO_PKG_VERSION"),
            component_agent_name: "user",
            ops_id: "1719100800000-1234".to_string(),
            ops_name: "ckpt",
            ckpt_time: 1,
            roll_time: 0,
            diff_time: 0,
            list_time: 0,
            ops_time: 1,
            err_reason: "none",
            supply: "none",
        };

        let json = serde_json::to_string(&record).expect("serialize");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("parse");

        assert_eq!(parsed["component.name"], "ws-ckpt");
        assert_eq!(parsed["component.version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(parsed["component.agent_name"], "user");
        assert_eq!(parsed["ops_name"], "ckpt");
        assert_eq!(parsed["ckpt_time"], 1);
        assert_eq!(parsed["roll_time"], 0);
        assert_eq!(parsed["ops_time"], 1);
        assert_eq!(parsed["err_reason"], "none");
    }

    #[test]
    fn record_with_error() {
        let record = OpsRecord {
            component_name: "ws-ckpt",
            component_version: env!("CARGO_PKG_VERSION"),
            component_agent_name: "hermes",
            ops_id: "1719100800000-1234".to_string(),
            ops_name: "roll",
            ckpt_time: 0,
            roll_time: 1,
            diff_time: 0,
            list_time: 0,
            ops_time: 1,
            err_reason: "snapshot not found",
            supply: "none",
        };

        let json = serde_json::to_string(&record).expect("serialize");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("parse");

        assert_eq!(parsed["component.agent_name"], "hermes");
        assert_eq!(parsed["ops_name"], "roll");
        assert_eq!(parsed["roll_time"], 1);
        assert_eq!(parsed["ckpt_time"], 0);
        assert_eq!(parsed["err_reason"], "snapshot not found");
    }

    #[test]
    fn write_to_tempfile() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("ws-ckpt.jsonl");
        std::fs::File::create(&path).expect("create");

        let record = OpsRecord {
            component_name: "ws-ckpt",
            component_version: env!("CARGO_PKG_VERSION"),
            component_agent_name: "user",
            ops_id: "test-id".to_string(),
            ops_name: "list",
            ckpt_time: 0,
            roll_time: 0,
            diff_time: 0,
            list_time: 1,
            ops_time: 1,
            err_reason: "none",
            supply: "none",
        };

        let mut line = serde_json::to_string(&record).expect("serialize");
        line.push('\n');
        let mut file = OpenOptions::new().append(true).open(&path).expect("open");
        file.write_all(line.as_bytes()).expect("write");
        file.flush().expect("flush");

        let contents = std::fs::read_to_string(&path).expect("read");
        assert_eq!(contents.lines().count(), 1);
        let parsed: serde_json::Value =
            serde_json::from_str(contents.lines().next().unwrap()).expect("parse");
        assert_eq!(parsed["ops_name"], "list");
        assert_eq!(parsed["list_time"], 1);
    }
}
