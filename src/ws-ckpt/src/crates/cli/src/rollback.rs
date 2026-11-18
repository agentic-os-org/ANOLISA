//! JSON boundary for the rollback CLI and the daemon's guarded protocol.

use std::io::{self, IsTerminal, Read};

use anyhow::{Context, Result};
use serde_json::{json, Value};
use ws_ckpt_common::{
    ErrorCode, GuardedRollbackEvidenceV2, GuardedRollbackOutcomeV2, Request, Response,
    WorkspaceGenerationTokenV2, GUARDED_CHECKPOINT_PROTOCOL_VERSION_V2,
};

use super::{configured_peer_uid, effective_uid, send_request, IpcFailure};

// CLI JSON contract: docs/design/guarded-checkpoint-v2.md.
const SCHEMA: &str = "ws-ckpt.rollback.v1";
const MAX_CONFIRMATION_BYTES: u64 = 4096;

#[derive(Clone, Copy)]
pub(super) enum Mode {
    Check,
    Preview,
    Execute,
    Result,
}

pub(super) fn succeeded(output: &Value) -> bool {
    matches!(
        output["status"].as_str(),
        Some("compatible" | "preview" | "succeeded" | "result")
    )
}

pub(super) async fn run_json(
    workspace: String,
    snapshot: Option<String>,
    num_ancestors: Option<u32>,
    guarded: bool,
    mode: Mode,
) -> Value {
    let result = if guarded {
        guarded_json(workspace, snapshot, mode).await
    } else {
        ordinary_json(workspace, snapshot, num_ancestors, mode).await
    };
    let mut output = result.unwrap_or_else(|error| {
        let status = if matches!(mode, Mode::Execute) {
            "known_no_effect"
        } else {
            "error"
        };
        failure(status, "PreflightFailed", &format!("{error:#}"))
    });
    output["schema"] = json!(SCHEMA);
    output
}

fn failure(status: &str, code: &str, message: &str) -> Value {
    json!({"status": status, "code": code, "message": message})
}

fn ipc_failure(error: IpcFailure, mode: Mode) -> Value {
    let status = match (mode, &error) {
        (Mode::Execute, IpcFailure::PossiblyApplied(_)) => "unknown",
        (Mode::Execute, _) => "known_no_effect",
        _ => "error",
    };
    failure(status, "IpcError", &format!("{:#}", error.into_error()))
}

async fn ordinary_json(
    workspace: String,
    to: Option<String>,
    num_ancestors: Option<u32>,
    mode: Mode,
) -> Result<Value> {
    anyhow::ensure!(
        to.is_some() || num_ancestors.is_some(),
        "either --snapshot/-s or --num-ancestors/-n must be specified"
    );
    let request = if matches!(mode, Mode::Preview) {
        Request::RollbackPreview {
            workspace,
            to,
            num_ancestors,
        }
    } else {
        Request::Rollback {
            workspace,
            to,
            num_ancestors,
        }
    };
    let output = match (mode, send_request(&request, configured_peer_uid()?).await) {
        (Mode::Preview, Ok(Response::RollbackPreviewOk { to, changes })) => {
            json!({"status": "preview", "snapshot_id": to, "changes": changes})
        }
        (Mode::Execute, Ok(Response::RollbackOk { from, to })) => {
            json!({"status": "succeeded", "workspace": from, "snapshot_id": to})
        }
        (_, Ok(Response::Error { code, message })) => {
            let status = match (mode, &code) {
                (
                    Mode::Execute,
                    ErrorCode::WorkspaceNotFound
                    | ErrorCode::SnapshotNotFound
                    | ErrorCode::CwdOccupied
                    | ErrorCode::CwdScanFailed
                    | ErrorCode::DiskSpaceInsufficient,
                ) => "known_no_effect",
                (Mode::Execute, _) => "unknown",
                _ => "error",
            };
            failure(status, &format!("{code:?}"), &message)
        }
        (_, Err(error)) => ipc_failure(error, mode),
        _ => unexpected(mode),
    };
    Ok(output)
}

async fn workspace_identity(path: &str, uid: u32) -> Result<(String, WorkspaceGenerationTokenV2)> {
    match send_request(
        &Request::WorkspaceIdentityV2 {
            registration_path: path.to_owned(),
        },
        Some(uid),
    )
    .await
    .map_err(IpcFailure::into_error)?
    {
        Response::WorkspaceIdentityV2Ok {
            protocol_version,
            ws_id,
            registered_path,
            generation,
        } => {
            anyhow::ensure!(
                protocol_version == GUARDED_CHECKPOINT_PROTOCOL_VERSION_V2,
                "unsupported guarded rollback protocol version {protocol_version}"
            );
            anyhow::ensure!(registered_path == path, "workspace registration mismatch");
            Ok((ws_id, generation))
        }
        Response::Error { code, message } => anyhow::bail!("{code:?}: {message}"),
        _ => anyhow::bail!("unexpected response while checking guarded rollback protocol"),
    }
}

async fn guarded_json(path: String, snapshot: Option<String>, mode: Mode) -> Result<Value> {
    let uid = effective_uid()?;
    Ok(match mode {
        Mode::Check => {
            let (ws_id, _) = workspace_identity(&path, uid).await?;
            json!({"status": "compatible", "ws_id": ws_id, "registered_path": path, "diff_json": true})
        }
        Mode::Preview => {
            let snapshot = snapshot.context("guarded preview requires --snapshot/-s")?;
            let (ws_id, generation) = workspace_identity(&path, uid).await?;
            let request = Request::GuardedRollbackPreviewV2 {
                registered_path: path.clone(),
                ws_id: ws_id.clone(),
                expected_generation: generation,
                target_snapshot_id: snapshot.clone(),
            };
            match send_request(&request, Some(uid)).await {
                Ok(Response::GuardedRollbackPreviewV2Ok {
                    protocol_version,
                    registered_path,
                    ws_id: actual_ws_id,
                    generation: actual_generation,
                    target_snapshot_id,
                    diff_digest,
                    changes,
                    caller_uid,
                }) if protocol_version == GUARDED_CHECKPOINT_PROTOCOL_VERSION_V2
                    && registered_path == path
                    && actual_ws_id == ws_id
                    && actual_generation == generation
                    && target_snapshot_id == snapshot
                    && caller_uid == uid =>
                {
                    json!({
                        "status": "preview", "ws_id": ws_id, "registered_path": path,
                        "snapshot_id": snapshot, "generation": generation, "diff_digest": diff_digest,
                        "changes": changes
                    })
                }
                response => guarded_reply(response, mode),
            }
        }
        Mode::Execute => {
            let snapshot = snapshot.context("guarded rollback requires --snapshot/-s")?;
            let confirmation = Confirmation::from_stdin()?;
            let (ws_id, _) = workspace_identity(&path, uid).await?;
            anyhow::ensure!(ws_id == confirmation.ws_id, "workspace ID mismatch");
            let request = Request::GuardedRollbackV2 {
                registered_path: path.clone(),
                ws_id,
                expected_generation: confirmation.generation,
                target_snapshot_id: snapshot.clone(),
                expected_diff_digest: confirmation.diff_digest,
                operation_id: confirmation.operation_id.clone(),
                operation_digest: confirmation.operation_digest,
            };
            match send_request(&request, Some(uid)).await {
                Ok(Response::GuardedRollbackV2Ok { evidence })
                    if confirmation.matches(&evidence, uid)
                        && evidence.registered_path == path
                        && evidence.target_snapshot_id == snapshot
                        && matches!(
                            evidence.outcome,
                            GuardedRollbackOutcomeV2::Succeeded { .. }
                        ) =>
                {
                    json!({"status": "succeeded", "evidence": evidence})
                }
                response => guarded_reply(response, mode),
            }
        }
        Mode::Result => {
            let confirmation = Confirmation::from_stdin()?;
            anyhow::ensure!(path == confirmation.ws_id, "workspace ID mismatch");
            let request = Request::GuardedRollbackEvidenceV2 {
                ws_id: path,
                operation_id: confirmation.operation_id.clone(),
                operation_digest: confirmation.operation_digest,
            };
            match send_request(&request, Some(uid)).await {
                Ok(Response::GuardedRollbackEvidenceV2Ok { evidence })
                    if evidence
                        .as_ref()
                        .is_none_or(|evidence| confirmation.matches(evidence, uid)) =>
                {
                    json!({"status": "result", "evidence": evidence})
                }
                response => guarded_reply(response, mode),
            }
        }
    })
}

fn guarded_reply(response: std::result::Result<Response, IpcFailure>, mode: Mode) -> Value {
    match response {
        Ok(Response::GuardedRollbackV2Rejected { code, message }) => {
            let status = if matches!(mode, Mode::Execute) {
                "known_no_effect"
            } else {
                "error"
            };
            failure(status, &format!("{code:?}"), &message)
        }
        Ok(Response::GuardedRollbackV2Uncertain { evidence }) if matches!(mode, Mode::Execute) => {
            let message = match &evidence.outcome {
                GuardedRollbackOutcomeV2::Unknown { reason } => reason,
                _ => "daemon could not confirm the rollback outcome",
            };
            failure("unknown", "UncertainOutcome", message)
        }
        Ok(Response::Error { code, message }) => {
            let status = if matches!(mode, Mode::Execute) {
                "unknown"
            } else {
                "error"
            };
            failure(status, &format!("{code:?}"), &message)
        }
        Err(error) => ipc_failure(error, mode),
        _ => unexpected(mode),
    }
}

fn unexpected(mode: Mode) -> Value {
    let status = if matches!(mode, Mode::Execute) {
        "unknown"
    } else {
        "error"
    };
    failure(
        status,
        "UnexpectedResponse",
        "unexpected daemon rollback response",
    )
}

struct Confirmation {
    ws_id: String,
    generation: WorkspaceGenerationTokenV2,
    diff_digest: [u8; 32],
    operation_id: String,
    operation_digest: [u8; 32],
}

impl Confirmation {
    fn from_stdin() -> Result<Self> {
        let stdin = io::stdin();
        anyhow::ensure!(
            !stdin.is_terminal(),
            "read the guarded confirmation JSON from stdin"
        );
        let mut input = Vec::new();
        stdin
            .take(MAX_CONFIRMATION_BYTES + 1)
            .read_to_end(&mut input)?;
        anyhow::ensure!(
            input.len() as u64 <= MAX_CONFIRMATION_BYTES,
            "guarded confirmation exceeds {MAX_CONFIRMATION_BYTES} bytes"
        );
        let mut fields: serde_json::Map<String, Value> =
            serde_json::from_slice(&input).context("invalid guarded confirmation JSON object")?;
        Ok(Self {
            ws_id: serde_json::from_value(fields.remove("ws_id").unwrap_or_default())
                .context("invalid ws_id")?,
            generation: serde_json::from_value(fields.remove("generation").unwrap_or_default())
                .context("invalid generation")?,
            diff_digest: serde_json::from_value(fields.remove("diff_digest").unwrap_or_default())
                .context("invalid diff_digest")?,
            operation_id: serde_json::from_value(fields.remove("operation_id").unwrap_or_default())
                .context("invalid operation_id")?,
            operation_digest: serde_json::from_value(
                fields.remove("operation_digest").unwrap_or_default(),
            )
            .context("invalid operation_digest")?,
        })
    }

    fn matches(&self, evidence: &GuardedRollbackEvidenceV2, uid: u32) -> bool {
        evidence.ws_id == self.ws_id
            && evidence.expected_generation == self.generation
            && evidence.expected_diff_digest == self.diff_digest
            && evidence.operation_id == self.operation_id
            && evidence.operation_digest == self.operation_digest
            && evidence.caller_uid == uid
    }
}
