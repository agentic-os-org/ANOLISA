// SPDX-License-Identifier: Apache-2.0
//! UDS HTTP API server.
//!
//! Routing is a hand-rolled `match` on `(method, path-segments)` rather
//! than a router framework — the surface is small (~17 endpoints) and
//! the cost of a fresh dependency outweighs the readability win.

use std::collections::HashMap;
use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use blaze_core::backend::{BackendKind, BackendStatus, select_backend};
use blaze_core::checkpoint::CheckpointMetadata;
use blaze_core::lifecycle::{SandboxInstance, StartPath};
use blaze_core::policy::{ImageMetadata, RuntimeDecision, WorkloadClass};
use http_body_util::{BodyExt, Full};
use hyper::body::{Body, Bytes, Incoming};
use hyper::header::CONTENT_TYPE;
use hyper::{Method, Request, Response, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::error::{BlazeDaemonError, Result};
use crate::guest::MAX_GUEST_FILE_BYTES;
use crate::sandbox::{
    CreateSandbox, HibernateSandbox, RestoreSandbox, RestoreSandboxResult, ResumeSandbox,
};
use crate::state::ServerState;

const MAX_EXEC_TIMEOUT_SECS: u32 = 20;
const MAX_GUEST_HTTP_BODY_BYTES: usize = 22 * 1024 * 1024;

/// Top-level request handler. Always returns `Ok(Response)`; internal
/// errors are turned into JSON error bodies so hyper never sees a panic.
pub async fn handle(
    req: Request<Incoming>,
    state: Arc<ServerState>,
) -> std::result::Result<Response<Full<Bytes>>, Infallible> {
    handle_request(req, state).await
}

async fn handle_request<B>(
    req: Request<B>,
    state: Arc<ServerState>,
) -> std::result::Result<Response<Full<Bytes>>, Infallible>
where
    B: Body<Data = Bytes> + Unpin,
    B::Error: std::fmt::Display,
{
    state.metrics.inc(&state.metrics.requests_total);

    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let query = req.uri().query().unwrap_or("").to_string();

    let response = if ignored_body_route(&method, &path) {
        // Go Blaze does not read the prune body. Drop this stream without
        // polling it so an oversized or indefinitely streamed body cannot
        // delay pruning or consume daemon memory.
        drop(req);
        dispatch(&method, &path, &query, Vec::new(), &state).await
    } else {
        let limit = guest_body_route(&method, &path).then_some(MAX_GUEST_HTTP_BODY_BYTES);
        match collect_body(req, limit).await {
            Ok(body) => dispatch(&method, &path, &query, body, &state).await,
            Err(e) => Err(e),
        }
    };

    let resp = match response {
        Ok(r) => r,
        Err(e) => error_response(&e),
    };
    Ok(resp)
}

fn guest_body_route(method: &Method, path: &str) -> bool {
    if method != Method::POST {
        return false;
    }
    let parts = path
        .trim_start_matches('/')
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    matches!(
        parts.as_slice(),
        ["v1", "sandboxes", _, "exec" | "read" | "write"]
    )
}

fn ignored_body_route(method: &Method, path: &str) -> bool {
    if method != Method::POST {
        return false;
    }
    let parts = path
        .trim_start_matches('/')
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    matches!(
        parts.as_slice(),
        ["v1", "sandboxes", _, "checkpoints", "prune"]
    )
}

async fn collect_body<B>(req: Request<B>, limit: Option<usize>) -> Result<Vec<u8>>
where
    B: Body<Data = Bytes> + Unpin,
    B::Error: std::fmt::Display,
{
    let mut body = req.into_body();
    let mut collected = Vec::new();
    while let Some(frame) = body.frame().await {
        let frame = frame
            .map_err(|error| BlazeDaemonError::BadRequest(format!("request body: {error}")))?;
        let Ok(data) = frame.into_data() else {
            continue;
        };
        if let Some(limit) = limit
            && collected.len().saturating_add(data.len()) > limit
        {
            return Err(crate::guest::GuestError::PayloadTooLarge {
                actual: collected.len().saturating_add(data.len()),
                limit,
            }
            .into());
        }
        collected.extend_from_slice(&data);
    }
    Ok(collected)
}

const fn max_base64_len(decoded_bytes: usize) -> usize {
    decoded_bytes
        .saturating_add(2)
        .saturating_div(3)
        .saturating_mul(4)
}

async fn dispatch(
    method: &Method,
    path: &str,
    _query: &str,
    body: Vec<u8>,
    state: &Arc<ServerState>,
) -> Result<Response<Full<Bytes>>> {
    let parts: Vec<&str> = path
        .trim_start_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    let m = method.as_str();

    match (m, parts.as_slice()) {
        ("GET", ["v1", "health"]) => health(state),
        ("GET", ["v1", "sandboxes"]) => list_sandboxes(state),
        ("POST", ["v1", "sandboxes"]) => create_sandbox(state, &body).await,
        ("GET", ["v1", "sandboxes", id]) => get_sandbox(state, id),
        ("POST", ["v1", "sandboxes", id, "exec"]) => exec_sandbox(state, id, &body).await,
        ("POST", ["v1", "sandboxes", id, "read"]) => read_sandbox_file(state, id, &body).await,
        ("POST", ["v1", "sandboxes", id, "write"]) => write_sandbox_file(state, id, &body).await,
        ("POST", ["v1", "sandboxes", id, "checkpoint"]) => checkpoint(state, id).await,
        ("GET", ["v1", "sandboxes", id, "checkpoints"]) => list_checkpoints(state, id).await,
        ("POST", ["v1", "sandboxes", id, "checkpoints", "prune"]) => {
            prune_checkpoints(state, id).await
        }
        ("POST", ["v1", "sandboxes", id, "rollback", checkpoint_id]) => {
            rollback(state, id, checkpoint_id).await
        }
        ("POST", ["v1", "sandboxes", id, "hibernate"]) => hibernate(state, id).await,
        ("POST", ["v1", "sandboxes", id, "resume"]) => resume(state, id).await,
        ("DELETE", ["v1", "sandboxes", id]) => destroy_sandbox(state, id).await,
        ("GET", ["v1", "pools"])
        | ("GET", ["v1", "pools", _, _])
        | ("POST", ["v1", "pools", _, _, "drain"])
        | ("PUT", ["v1", "pools", _, _, "sizing"]) => pool_operation_unavailable(),
        ("GET", ["v1", "templates"]) => list_templates(state).await,
        ("GET", ["v1", "templates", name]) => get_template(state, name).await,
        ("POST", ["v1", "templates", "import"]) => import_template(state, &body).await,
        ("GET", ["v1", "policies"]) => list_policies(state),
        ("GET", ["v1", "hooks"]) => list_hooks(state),
        ("GET", ["v1", "metrics"]) => metrics(state),
        ("POST", ["v1", "admin", "reload"]) => admin_reload(state),
        _ => Err(BlazeDaemonError::NotFound(format!("{method} {path}"))),
    }
}

// ---------------------------------------------------------------------------
// Health / metrics / admin
// ---------------------------------------------------------------------------

fn health(state: &Arc<ServerState>) -> Result<Response<Full<Bytes>>> {
    let pool_status = state.storage.pool_status();
    json_ok(&json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "storage_pool": pool_status,
    }))
}

fn metrics(state: &Arc<ServerState>) -> Result<Response<Full<Bytes>>> {
    let body = state.metrics.render();
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "text/plain; version=0.0.4")
        .body(Full::new(Bytes::from(body)))?)
}

fn admin_reload(state: &Arc<ServerState>) -> Result<Response<Full<Bytes>>> {
    let policy_dir = {
        let cfg = state
            .config
            .lock()
            .map_err(|_| BlazeDaemonError::Internal("config lock poisoned".into()))?;
        cfg.policy.dir.clone()
    };
    let new_engine = blaze_core::policy::PolicyEngine::load_dir(&policy_dir)?;
    let count = new_engine.policies().len();
    {
        let mut engine = state
            .policy
            .lock()
            .map_err(|_| BlazeDaemonError::Internal("policy lock poisoned".into()))?;
        *engine = new_engine;
    }
    tracing::info!(policies = count, "policy engine reloaded");
    json_ok(&json!({ "reloaded": true, "policies": count }))
}

// ---------------------------------------------------------------------------
// Sandboxes
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct CreateInstanceReq {
    workload_class: WorkloadClass,
    image_digest: String,
    #[serde(default)]
    labels: HashMap<String, String>,
    #[serde(default)]
    kernel_version: Option<String>,
    /// Optional published template to restore this sandbox from.
    #[serde(default)]
    template: Option<String>,
}

#[derive(Debug, Serialize)]
struct CreateInstanceResp {
    instance: SandboxInstance,
    decision: RuntimeDecision,
    start_path: StartPath,
    selected_backend: BackendKind,
}

#[derive(Debug, Serialize)]
struct CheckpointResp {
    checkpoint_id: String,
    instance_id: Uuid,
    #[serde(flatten)]
    checkpoint: CheckpointMetadata,
}

fn list_sandboxes(state: &Arc<ServerState>) -> Result<Response<Full<Bytes>>> {
    json_ok(&state.manager.list()?)
}

fn get_sandbox(state: &Arc<ServerState>, id: &str) -> Result<Response<Full<Bytes>>> {
    json_ok(&state.manager.get(parse_uuid(id)?)?)
}

async fn create_sandbox(state: &Arc<ServerState>, body: &[u8]) -> Result<Response<Full<Bytes>>> {
    let req: CreateInstanceReq = serde_json::from_slice(body)
        .map_err(|e| BlazeDaemonError::BadRequest(format!("invalid create body: {e}")))?;

    let image = ImageMetadata {
        digest: req.image_digest.clone(),
        workload_class: Some(req.workload_class),
        kernel_version: req.kernel_version.clone(),
    };
    let decision = {
        let engine = state
            .policy
            .lock()
            .map_err(|_| BlazeDaemonError::Internal("policy lock poisoned".into()))?;
        match engine.evaluate(&req.labels, &image) {
            Ok(decision) => decision,
            Err(error) => {
                state.metrics.inc(&state.metrics.policy_eval_failures);
                return Err(error.into());
            }
        }
    };

    // Constrain availability to the implementation selected at daemon boot.
    let availability: Vec<BackendStatus> = {
        let config = state
            .config
            .lock()
            .map_err(|_| BlazeDaemonError::Internal("config lock poisoned".into()))?;
        decision
            .backend_priority
            .iter()
            .map(|kind| {
                let available = *kind == state.active_backend
                    && (state.active_backend == BackendKind::Mock
                        || config
                            .backends
                            .get(kind.as_str())
                            .map(|path| path.exists())
                            .unwrap_or(false));
                BackendStatus {
                    kind: *kind,
                    available,
                    version: None,
                }
            })
            .collect()
    };
    let policy_backend = match select_backend(&decision.backend_priority, &availability) {
        Ok(backend) => backend,
        Err(_) if state.active_backend == BackendKind::Mock => {
            *decision.backend_priority.first().ok_or_else(|| {
                BlazeDaemonError::Internal("policy has empty backend_priority".into())
            })?
        }
        Err(error) => return Err(error.into()),
    };
    let runtime_backend = if state.active_backend == BackendKind::Mock {
        BackendKind::Mock
    } else {
        policy_backend
    };
    let binary_path = state
        .config
        .lock()
        .map_err(|_| BlazeDaemonError::Internal("config lock poisoned".into()))?
        .backends
        .get(state.active_backend.as_str())
        .cloned()
        .unwrap_or_default();

    let created = state
        .manager
        .create(CreateSandbox {
            decision: decision.clone(),
            image_digest: req.image_digest,
            runtime_backend,
            binary_path,
            template: req.template,
        })
        .await?;
    json_created(&CreateInstanceResp {
        start_path: created.instance.start_path,
        instance: created.instance,
        decision,
        selected_backend: created.selected_backend,
    })
}

async fn checkpoint(state: &Arc<ServerState>, id: &str) -> Result<Response<Full<Bytes>>> {
    let uuid = parse_uuid(id)?;
    let checkpoint = state.manager.checkpoint(uuid).await?;
    json_ok(&CheckpointResp {
        checkpoint_id: checkpoint.id.clone(),
        instance_id: checkpoint.sandbox_id,
        checkpoint,
    })
}

async fn list_checkpoints(state: &Arc<ServerState>, id: &str) -> Result<Response<Full<Bytes>>> {
    json_ok(&state.manager.list_checkpoints(parse_uuid(id)?).await?)
}

async fn prune_checkpoints(state: &Arc<ServerState>, id: &str) -> Result<Response<Full<Bytes>>> {
    let removed = state.manager.prune_checkpoints(parse_uuid(id)?).await?;
    json_ok(&json!({
        "status": "pruned",
        "removed_count": removed.len(),
        "removed": removed,
    }))
}

async fn rollback(
    state: &Arc<ServerState>,
    id: &str,
    checkpoint_id: &str,
) -> Result<Response<Full<Bytes>>> {
    let uuid = parse_uuid(id)?;
    let instance = state.manager.get(uuid)?;
    let binary_path = state
        .config
        .lock()
        .map_err(|_| BlazeDaemonError::Internal("config lock poisoned".into()))?
        .backends
        .get(instance.backend.as_str())
        .cloned()
        .unwrap_or_default();
    let restored: RestoreSandboxResult = state
        .manager
        .restore(
            uuid,
            RestoreSandbox {
                checkpoint_id: checkpoint_id.to_string(),
                binary_path,
            },
        )
        .await?;
    json_ok(&json!({
        "instance_id": restored.instance.id,
        "checkpoint_id": restored.checkpoint_id,
        "restored": true,
        "state": restored.instance.state,
    }))
}

async fn hibernate(state: &Arc<ServerState>, id: &str) -> Result<Response<Full<Bytes>>> {
    let uuid = parse_uuid(id)?;
    let instance = state.manager.get(uuid)?;
    let binary_path = configured_backend_path(state, instance.backend)?;
    json_ok(
        &state
            .manager
            .hibernate(uuid, HibernateSandbox { binary_path })
            .await?,
    )
}

async fn resume(state: &Arc<ServerState>, id: &str) -> Result<Response<Full<Bytes>>> {
    let uuid = parse_uuid(id)?;
    let instance = state.manager.get(uuid)?;
    let binary_path = configured_backend_path(state, instance.backend)?;
    json_ok(
        &state
            .manager
            .resume(uuid, ResumeSandbox { binary_path })
            .await?,
    )
}

fn configured_backend_path(
    state: &ServerState,
    backend: BackendKind,
) -> Result<std::path::PathBuf> {
    Ok(state
        .config
        .lock()
        .map_err(|_| BlazeDaemonError::Internal("config lock poisoned".into()))?
        .backends
        .get(backend.as_str())
        .cloned()
        .unwrap_or_default())
}

async fn destroy_sandbox(state: &Arc<ServerState>, id: &str) -> Result<Response<Full<Bytes>>> {
    let uuid = parse_uuid(id)?;
    state.manager.destroy(uuid).await?;
    json_ok(&json!({
        "destroyed": true,
        "instance_id": uuid,
    }))
}

#[derive(Debug, Deserialize)]
struct ExecRequest {
    cmd: String,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    env: Option<HashMap<String, String>>,
    #[serde(default)]
    timeout: Option<u32>,
}

async fn exec_sandbox(
    state: &Arc<ServerState>,
    id: &str,
    body: &[u8],
) -> Result<Response<Full<Bytes>>> {
    let request: ExecRequest = serde_json::from_slice(body)
        .map_err(|error| BlazeDaemonError::BadRequest(format!("invalid exec body: {error}")))?;
    if request.cmd.is_empty() {
        return Err(BlazeDaemonError::BadRequest(
            "exec command is required".to_string(),
        ));
    }
    let timeout = request.timeout.unwrap_or(MAX_EXEC_TIMEOUT_SECS);
    if timeout == 0 || timeout > MAX_EXEC_TIMEOUT_SECS {
        return Err(BlazeDaemonError::BadRequest(format!(
            "exec timeout must be between 1 and {MAX_EXEC_TIMEOUT_SECS} seconds"
        )));
    }
    let result = state
        .manager
        .exec(
            parse_uuid(id)?,
            request.cmd,
            request.cwd,
            request.env,
            timeout,
        )
        .await?;
    json_ok(&json!({
        "exit_code": result.exit_code,
        "stdout_b64": BASE64.encode(result.stdout),
        "stderr_b64": BASE64.encode(result.stderr),
    }))
}

#[derive(Debug, Deserialize)]
struct FileRequest {
    path: String,
    #[serde(default)]
    data_b64: Option<String>,
}

async fn read_sandbox_file(
    state: &Arc<ServerState>,
    id: &str,
    body: &[u8],
) -> Result<Response<Full<Bytes>>> {
    let request: FileRequest = serde_json::from_slice(body)
        .map_err(|error| BlazeDaemonError::BadRequest(format!("invalid read body: {error}")))?;
    let data = state
        .manager
        .read_file(parse_uuid(id)?, request.path)
        .await?;
    json_ok(&json!({"data_b64": BASE64.encode(data)}))
}

async fn write_sandbox_file(
    state: &Arc<ServerState>,
    id: &str,
    body: &[u8],
) -> Result<Response<Full<Bytes>>> {
    let request: FileRequest = serde_json::from_slice(body)
        .map_err(|error| BlazeDaemonError::BadRequest(format!("invalid write body: {error}")))?;
    let encoded = request
        .data_b64
        .ok_or_else(|| BlazeDaemonError::BadRequest("data_b64 is required".to_string()))?;
    let data = decode_guest_file(&encoded, MAX_GUEST_FILE_BYTES)?;
    state
        .manager
        .write_file(parse_uuid(id)?, request.path, &data)
        .await?;
    json_ok(&json!({"written": true, "bytes": data.len()}))
}

fn decode_guest_file(encoded: &str, limit: usize) -> Result<Vec<u8>> {
    let encoded_limit = max_base64_len(limit);
    if encoded.len() > encoded_limit {
        return Err(crate::guest::GuestError::PayloadTooLarge {
            actual: encoded.len(),
            limit: encoded_limit,
        }
        .into());
    }
    let data = BASE64
        .decode(encoded)
        .map_err(|error| BlazeDaemonError::BadRequest(format!("invalid base64: {error}")))?;
    if data.len() > limit {
        return Err(crate::guest::GuestError::PayloadTooLarge {
            actual: data.len(),
            limit,
        }
        .into());
    }
    Ok(data)
}

fn pool_operation_unavailable() -> Result<Response<Full<Bytes>>> {
    Err(BlazeDaemonError::UnsupportedOperation(
        "warm pool management is not implemented".to_string(),
    ))
}

// ---------------------------------------------------------------------------
// Templates
// ---------------------------------------------------------------------------

async fn list_templates(state: &Arc<ServerState>) -> Result<Response<Full<Bytes>>> {
    json_bytes_ok(state.manager.list_templates().await?)
}

async fn get_template(state: &Arc<ServerState>, name: &str) -> Result<Response<Full<Bytes>>> {
    json_bytes_ok(state.manager.get_template(name.to_string()).await?)
}

#[derive(Debug, Deserialize)]
struct ImportTemplateRequest {
    name: String,
    source: PathBuf,
    #[serde(default)]
    description: String,
}

async fn import_template(state: &Arc<ServerState>, body: &[u8]) -> Result<Response<Full<Bytes>>> {
    let request: ImportTemplateRequest = serde_json::from_slice(body).map_err(|error| {
        BlazeDaemonError::BadRequest(format!("invalid runtime template import body: {error}"))
    })?;
    let imported = state
        .manager
        .import_template(request.name, request.source, request.description)
        .await?;
    json_response(StatusCode::CREATED, &imported)
}

// ---------------------------------------------------------------------------
// Policies / hooks
// ---------------------------------------------------------------------------

fn list_policies(state: &Arc<ServerState>) -> Result<Response<Full<Bytes>>> {
    let engine = state
        .policy
        .lock()
        .map_err(|_| BlazeDaemonError::Internal("policy lock poisoned".into()))?;
    let names: Vec<_> = engine
        .policies()
        .iter()
        .map(|p| {
            json!({
                "name": p.policy_name,
                "priority": p.priority,
                "workload_class": p.match_.workload_class.as_str(),
            })
        })
        .collect();
    json_ok(&names)
}

fn list_hooks(state: &Arc<ServerState>) -> Result<Response<Full<Bytes>>> {
    let reg = state
        .hook
        .lock()
        .map_err(|_| BlazeDaemonError::Internal("hook lock poisoned".into()))?;
    json_ok(&reg.list())
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn parse_uuid(s: &str) -> Result<Uuid> {
    Uuid::parse_str(s).map_err(|e| BlazeDaemonError::BadRequest(format!("invalid uuid: {e}")))
}

fn json_ok<T: Serialize>(value: &T) -> Result<Response<Full<Bytes>>> {
    json_response(StatusCode::OK, value)
}

fn json_bytes_ok(body: Bytes) -> Result<Response<Full<Bytes>>> {
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "application/json")
        .body(Full::new(body))?)
}

fn json_created<T: Serialize>(value: &T) -> Result<Response<Full<Bytes>>> {
    json_response(StatusCode::CREATED, value)
}

fn json_response<T: Serialize>(status: StatusCode, value: &T) -> Result<Response<Full<Bytes>>> {
    let body = serde_json::to_vec_pretty(value)?;
    Ok(Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(body)))?)
}

fn error_response(err: &BlazeDaemonError) -> Response<Full<Bytes>> {
    let status =
        StatusCode::from_u16(err.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut body = json!({
        "error": err.to_string(),
        "status": status.as_u16(),
    });
    if let Some(code) = err.api_code() {
        body["code"] = json!(code);
    }
    let bytes = serde_json::to_vec_pretty(&body)
        .unwrap_or_else(|_| br#"{"error":"serialize_failed"}"#.to_vec());
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(bytes)))
        .unwrap_or_else(|_| {
            // Hyper's builder can fail on invalid header values; this branch
            // should be unreachable. Fall back to a status-only response.
            Response::new(Full::new(Bytes::from_static(b"{}")))
        })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    #[cfg(feature = "test-failpoints")]
    use std::time::Duration;

    use async_trait::async_trait;
    use blaze_core::BlazeError;
    use blaze_core::backend::BackendKind;
    #[cfg(feature = "test-failpoints")]
    use blaze_core::backend::SnapshotKind;
    #[cfg(feature = "test-failpoints")]
    use blaze_core::checkpoint::CommitCheckpoint;
    use blaze_core::config::DaemonConfig;
    use blaze_core::kernel::HookRegistry;
    #[cfg(feature = "test-failpoints")]
    use blaze_core::lifecycle::OperationPhase;
    use blaze_core::lifecycle::{BackendOwnership, OperationKind, SandboxState};
    use blaze_core::policy::{
        BackendConfigs, FallbackOnMissingHook, PolicyEngine, PolicyFile, PolicyHooks, PolicyMatch,
        PolicySelect, WorkloadClass,
    };
    use blaze_core::storage::{
        AcquireOpts, PoolStatus, StorageAcquireError, StorageProvider, StorageSlot,
    };
    use sha2::{Digest, Sha256};

    #[cfg(feature = "test-failpoints")]
    use crate::checkpoint_store::CheckpointStore;
    use crate::file_provider::FileStorageProvider;
    #[cfg(target_os = "linux")]
    use crate::spawner::BubblewrapSpawner;
    use crate::spawner::{
        BackendInstance, BackendSpawnRequest, BackendSpawner, DynBackendInstance, DynSpawner,
        GuestMockSpawner, MockSpawner, SpawnFailure, SpawnResult, SpawnerRegistry,
    };
    use crate::state::ServerState;
    use crate::state_store::OwnedRunDir;
    #[cfg(target_os = "linux")]
    use tokio::sync::Notify;

    use super::*;

    fn spawners(kind: BackendKind, spawner: DynSpawner) -> SpawnerRegistry {
        let mut registry = SpawnerRegistry::new();
        registry.insert(kind, spawner);
        registry
    }

    fn test_config(temp: &tempfile::TempDir) -> DaemonConfig {
        let mut config = DaemonConfig::default();
        config.daemon.state_dir = temp.path().join("state");
        config.storage.images_dir = temp.path().join("images");
        config.storage.instances_dir = temp.path().join("instances");
        config.template.dir = temp.path().join("templates");
        std::fs::create_dir_all(&config.daemon.state_dir).expect("state");
        std::fs::create_dir_all(&config.storage.images_dir).expect("images");
        std::fs::create_dir_all(&config.storage.instances_dir).expect("instances");
        config
    }

    fn test_policy(kind: BackendKind) -> PolicyFile {
        PolicyFile {
            manifest_version: 1,
            policy_name: "ownership-test".into(),
            priority: 100,
            match_: PolicyMatch {
                workload_class: WorkloadClass::AgentTool,
                image_labels: HashMap::new(),
            },
            select: PolicySelect {
                backend_priority: vec![kind],
                kernel_hooks: vec![],
                templates: vec![],
                fallback_on_missing_hook: FallbackOnMissingHook::default(),
            },
            pool: None,
            checkpoint: None,
            quota: None,
            hooks: PolicyHooks::default(),
            backend: BackendConfigs::default(),
            vm: None,
        }
    }

    fn test_request() -> Vec<u8> {
        serde_json::to_vec(&json!({
            "workload_class": "agent-tool",
            "image_digest": "sha256:ownership-test"
        }))
        .expect("request")
    }

    fn configured_state_dir(state: &ServerState) -> PathBuf {
        state
            .config
            .lock()
            .expect("config")
            .daemon
            .state_dir
            .clone()
    }

    fn build_test_state(
        config: DaemonConfig,
        policy: PolicyFile,
        registry: SpawnerRegistry,
        active_backend: BackendKind,
        storage: Arc<dyn StorageProvider>,
    ) -> Arc<ServerState> {
        Arc::new(
            ServerState::build(
                config,
                PolicyEngine::with_policies(vec![policy]),
                HookRegistry::new(),
                registry,
                active_backend,
                storage,
            )
            .expect("state"),
        )
    }

    fn mock_state(temp: &tempfile::TempDir) -> Arc<ServerState> {
        mock_state_from_config(test_config(temp))
    }

    fn mock_state_from_config(config: DaemonConfig) -> Arc<ServerState> {
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        build_test_state(
            config,
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, Arc::new(MockSpawner)),
            BackendKind::Mock,
            storage,
        )
    }

    #[cfg(feature = "test-failpoints")]
    fn guest_mock_state(temp: &tempfile::TempDir) -> Arc<ServerState> {
        let config = test_config(temp);
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        build_test_state(
            config,
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, Arc::new(GuestMockSpawner)),
            BackendKind::Mock,
            storage,
        )
    }

    async fn created_json(state: &Arc<ServerState>, request: &[u8]) -> serde_json::Value {
        let response = create_sandbox(state, request).await.expect("create");
        serde_json::from_slice(
            &response
                .into_body()
                .collect()
                .await
                .expect("body")
                .to_bytes(),
        )
        .expect("created json")
    }

    async fn write_checkpoint_fixture(state: &Arc<ServerState>, id: &str) -> StorageSlot {
        let slot = state.storage.reconstruct(id).await.expect("storage slot");
        tokio::fs::write(&slot.rootfs_path, b"checkpoint-rootfs")
            .await
            .expect("rootfs");
        slot
    }

    #[cfg(feature = "test-failpoints")]
    async fn cancel_checkpoint_request_at(
        state: &Arc<ServerState>,
        id: Uuid,
        failpoint: &'static str,
        expected_state: SandboxState,
        expected_phase: OperationPhase,
    ) -> String {
        let hook = crate::failpoint::TestFailpoint::new(&[failpoint]);
        let capture_state = state.clone();
        let capture_hook = hook.clone();
        let capture =
            tokio::spawn(
                async move { capture_hook.run(capture_state.manager.checkpoint(id)).await },
            );
        hook.wait_until_paused().await;
        let interrupted = state.manager.get(id).expect("interrupted lifecycle");
        let lock_was_retained = state.manager.operation_lock(id).try_lock().is_err();
        capture.abort();
        let cancelled = capture
            .await
            .expect_err("checkpoint task must be cancelled");
        hook.release();

        assert!(cancelled.is_cancelled());
        assert_eq!(interrupted.state, expected_state);
        assert_eq!(
            interrupted.operation.and_then(|journal| journal.phase),
            Some(expected_phase)
        );
        assert!(
            lock_was_retained,
            "the detached supervisor must retain checkpoint ownership"
        );

        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let lifecycle = state.manager.get(id).expect("checkpoint lifecycle");
                if lifecycle.state == SandboxState::Running
                    && lifecycle.operation.is_none()
                    && lifecycle.last_checkpoint.is_some()
                    && state.manager.operation_lock(id).try_lock().is_ok()
                {
                    return lifecycle.last_checkpoint.expect("completed checkpoint");
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("detached checkpoint supervisor must converge")
    }

    #[cfg(feature = "test-failpoints")]
    async fn persist_crashed_checkpoint_phase(
        state: &Arc<ServerState>,
        id: Uuid,
        phase: OperationPhase,
    ) -> String {
        let store = CheckpointStore::new(state.state_store.clone());
        let stage = store.begin(id).expect("checkpoint stage");
        let checkpoint_id = stage.id().to_string();
        let mut instance = state.manager.get(id).expect("running lifecycle");
        instance
            .begin_checkpoint_operation(checkpoint_id.clone())
            .expect("checkpoint journal");
        if !matches!(phase, OperationPhase::CheckpointPreparing) {
            instance
                .transition(SandboxState::Paused)
                .expect("paused lifecycle");
            instance
                .advance_checkpoint_phase(OperationPhase::CheckpointPaused)
                .expect("paused journal");
        }
        if matches!(
            phase,
            OperationPhase::CheckpointPublished | OperationPhase::CheckpointHeadUpdated
        ) {
            for (path, contents) in [
                (
                    stage.backend_payload_dir().join("vmstate.snap"),
                    b"crashed-vmstate".as_slice(),
                ),
                (
                    stage.backend_payload_dir().join("memory.snap"),
                    b"crashed-memory".as_slice(),
                ),
                (
                    stage.storage_payload_dir().join("rootfs.snap"),
                    b"crashed-rootfs".as_slice(),
                ),
            ] {
                std::fs::write(path, contents).expect("checkpoint artifact");
            }
            store
                .publish(
                    &stage,
                    CommitCheckpoint {
                        parent: None,
                        policy_name: instance.policy_name.clone(),
                        image_digest: instance.image_digest.clone(),
                        backend: instance.backend,
                        backend_version: Some("mock-v1".to_string()),
                        snapshot_kind: SnapshotKind::Full,
                    },
                )
                .expect("published checkpoint");
            instance
                .advance_checkpoint_phase(OperationPhase::CheckpointPublished)
                .expect("published journal");
        }
        if phase == OperationPhase::CheckpointHeadUpdated {
            store.set_head(id, &checkpoint_id).expect("checkpoint HEAD");
            instance
                .advance_checkpoint_phase(OperationPhase::CheckpointHeadUpdated)
                .expect("HEAD-updated journal");
        }
        state
            .state_store
            .persist(&instance)
            .expect("persist crashed checkpoint phase");
        state
            .manager
            .backend_owner(id)
            .expect("backend owner")
            .kill()
            .await
            .expect("stop process owned by crashed daemon");
        checkpoint_id
    }

    struct NoCheckpointStorage {
        inner: FileStorageProvider,
    }

    #[async_trait]
    impl StorageProvider for NoCheckpointStorage {
        async fn probe(&self) -> blaze_core::Result<bool> {
            self.inner.probe().await
        }

        async fn acquire(
            &self,
            opts: &AcquireOpts,
        ) -> std::result::Result<StorageSlot, StorageAcquireError> {
            self.inner.acquire(opts).await
        }

        async fn release(&self, slot: StorageSlot) -> blaze_core::Result<()> {
            self.inner.release(slot).await
        }

        async fn release_by_id(&self, instance_id: &str) -> blaze_core::Result<()> {
            self.inner.release_by_id(instance_id).await
        }

        async fn reconstruct(&self, instance_id: &str) -> blaze_core::Result<StorageSlot> {
            self.inner.reconstruct(instance_id).await
        }

        async fn sync_artifacts(&self, slot: &StorageSlot) -> blaze_core::Result<()> {
            self.inner.sync_artifacts(slot).await
        }

        fn pool_status(&self) -> PoolStatus {
            self.inner.pool_status()
        }
    }

    async fn dispatched_json(
        state: &Arc<ServerState>,
        method: Method,
        path: &str,
        body: Vec<u8>,
    ) -> (StatusCode, serde_json::Value) {
        let response = dispatch(&method, path, "", body, state)
            .await
            .expect("dispatch");
        let status = response.status();
        let body = response
            .into_body()
            .collect()
            .await
            .expect("response body")
            .to_bytes();
        let value = serde_json::from_slice(&body).expect("response json");
        (status, value)
    }

    async fn handled_json(
        state: &Arc<ServerState>,
        method: Method,
        path: &str,
        body: Vec<u8>,
    ) -> (StatusCode, serde_json::Value) {
        let request = Request::builder()
            .method(method)
            .uri(path)
            .header(hyper::header::CONTENT_LENGTH, body.len())
            .body(Full::new(Bytes::from(body)))
            .expect("request");
        let response = handle_request(request, state.clone())
            .await
            .expect("infallible response");
        let status = response.status();
        let body = response
            .into_body()
            .collect()
            .await
            .expect("response body")
            .to_bytes();
        let value = serde_json::from_slice(&body).expect("response json");
        (status, value)
    }

    struct BodyThatMustNotBeRead;

    impl Body for BodyThatMustNotBeRead {
        type Data = Bytes;
        type Error = Infallible;

        fn poll_frame(
            self: std::pin::Pin<&mut Self>,
            _context: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<std::result::Result<hyper::body::Frame<Bytes>, Infallible>>>
        {
            let _ = self;
            panic!("ignored prune body was polled")
        }
    }

    struct OwnershipObservingStorage {
        inner: FileStorageProvider,
        state_dir: PathBuf,
        observed: Arc<AtomicBool>,
    }

    #[async_trait]
    impl StorageProvider for OwnershipObservingStorage {
        async fn probe(&self) -> blaze_core::Result<bool> {
            self.inner.probe().await
        }

        async fn acquire(
            &self,
            opts: &AcquireOpts,
        ) -> std::result::Result<StorageSlot, StorageAcquireError> {
            let id = Uuid::parse_str(&opts.instance_id).expect("stable instance ID");
            let instance = SandboxInstance::load(&self.state_dir, id).expect("ownership published");
            assert_eq!(instance.state, SandboxState::Creating);
            assert_eq!(instance.backend_ownership, BackendOwnership::NotStarted);
            assert_eq!(
                instance.operation.as_ref().map(|operation| operation.kind),
                Some(OperationKind::Create)
            );
            self.observed.store(true, Ordering::Release);
            self.inner.acquire(opts).await
        }

        async fn release(&self, slot: StorageSlot) -> blaze_core::Result<()> {
            self.inner.release(slot).await
        }

        async fn release_by_id(&self, instance_id: &str) -> blaze_core::Result<()> {
            self.inner.release_by_id(instance_id).await
        }

        async fn reconstruct(&self, instance_id: &str) -> blaze_core::Result<StorageSlot> {
            self.inner.reconstruct(instance_id).await
        }

        async fn sync_artifacts(&self, slot: &StorageSlot) -> blaze_core::Result<()> {
            self.inner.sync_artifacts(slot).await
        }

        fn pool_status(&self) -> PoolStatus {
            self.inner.pool_status()
        }
    }

    struct FailOnceOwner {
        instance_id: Uuid,
        attempts: AtomicUsize,
    }

    #[async_trait]
    impl BackendInstance for FailOnceOwner {
        fn backend(&self) -> BackendKind {
            BackendKind::Mock
        }

        async fn try_wait(&self) -> blaze_core::Result<Option<SpawnResult>> {
            Ok(None)
        }

        async fn kill(&self) -> blaze_core::Result<()> {
            if self.attempts.fetch_add(1, Ordering::AcqRel) == 0 {
                return Err(BlazeError::BackendError {
                    msg: format!("instance {} termination deferred", self.instance_id),
                });
            }
            Ok(())
        }
    }

    struct PartialSpawnSpawner;

    #[async_trait]
    impl BackendSpawner for PartialSpawnSpawner {
        async fn spawn(
            &self,
            request: BackendSpawnRequest,
        ) -> std::result::Result<DynBackendInstance, SpawnFailure> {
            let owner: DynBackendInstance = Arc::new(FailOnceOwner {
                instance_id: request.instance_id,
                attempts: AtomicUsize::new(0),
            });
            Err(SpawnFailure::with_owner(
                BlazeError::BackendError {
                    msg: "backend readiness failed".into(),
                },
                owner,
            ))
        }

        async fn probe(&self, _binary_path: &Path) -> blaze_core::Result<bool> {
            Ok(true)
        }

        async fn cleanup_orphan(
            &self,
            _instance_id: Uuid,
            _run_dir: &OwnedRunDir,
        ) -> blaze_core::Result<()> {
            Err(BlazeError::BackendError {
                msg: "partial owner must remain registered".into(),
            })
        }
    }

    #[cfg(target_os = "linux")]
    struct PreSpawnBoundarySpawner {
        reached: Arc<Notify>,
    }

    #[cfg(target_os = "linux")]
    #[async_trait]
    impl BackendSpawner for PreSpawnBoundarySpawner {
        async fn prepare_spawn(&self, run_dir: &OwnedRunDir) -> blaze_core::Result<()> {
            BubblewrapSpawner.prepare_spawn(run_dir).await
        }

        async fn spawn(
            &self,
            _request: BackendSpawnRequest,
        ) -> std::result::Result<DynBackendInstance, SpawnFailure> {
            self.reached.notify_one();
            std::future::pending().await
        }

        async fn probe(&self, _binary_path: &Path) -> blaze_core::Result<bool> {
            Ok(true)
        }

        async fn cleanup_orphan(
            &self,
            instance_id: Uuid,
            run_dir: &OwnedRunDir,
        ) -> blaze_core::Result<()> {
            BubblewrapSpawner.cleanup_orphan(instance_id, run_dir).await
        }
    }

    struct RecordingSpawner {
        cleanup_count: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl BackendSpawner for RecordingSpawner {
        async fn spawn(
            &self,
            _request: BackendSpawnRequest,
        ) -> std::result::Result<DynBackendInstance, SpawnFailure> {
            Err(SpawnFailure::clean(BlazeError::BackendError {
                msg: "spawn not used".into(),
            }))
        }

        async fn probe(&self, _binary_path: &Path) -> blaze_core::Result<bool> {
            Ok(true)
        }

        async fn cleanup_orphan(
            &self,
            _instance_id: Uuid,
            _run_dir: &OwnedRunDir,
        ) -> blaze_core::Result<()> {
            self.cleanup_count.fetch_add(1, Ordering::AcqRel);
            Ok(())
        }
    }

    struct SelectiveCleanupSpawner {
        failed_id: Uuid,
        cleanup_count: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl BackendSpawner for SelectiveCleanupSpawner {
        async fn spawn(
            &self,
            request: BackendSpawnRequest,
        ) -> std::result::Result<DynBackendInstance, SpawnFailure> {
            MockSpawner.spawn(request).await
        }

        async fn probe(&self, _binary_path: &Path) -> blaze_core::Result<bool> {
            Ok(true)
        }

        async fn cleanup_orphan(
            &self,
            instance_id: Uuid,
            _run_dir: &OwnedRunDir,
        ) -> blaze_core::Result<()> {
            self.cleanup_count.fetch_add(1, Ordering::AcqRel);
            if instance_id == self.failed_id {
                return Err(BlazeError::BackendError {
                    msg: "cleanup deferred".into(),
                });
            }
            Ok(())
        }
    }

    struct CountingOwner {
        instance_id: Uuid,
        kill_count: Arc<AtomicUsize>,
        killed: AtomicBool,
    }

    #[async_trait]
    impl BackendInstance for CountingOwner {
        fn backend(&self) -> BackendKind {
            BackendKind::Mock
        }

        async fn try_wait(&self) -> blaze_core::Result<Option<SpawnResult>> {
            Ok(self.killed.load(Ordering::Acquire).then_some(SpawnResult {
                instance_id: self.instance_id,
                exit_code: Some(0),
                signal: None,
            }))
        }

        async fn kill(&self) -> blaze_core::Result<()> {
            if !self.killed.swap(true, Ordering::AcqRel) {
                self.kill_count.fetch_add(1, Ordering::AcqRel);
            }
            Ok(())
        }
    }

    struct CountingSpawner {
        kill_count: Arc<AtomicUsize>,
        orphan_cleanup_count: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl BackendSpawner for CountingSpawner {
        async fn spawn(
            &self,
            request: BackendSpawnRequest,
        ) -> std::result::Result<DynBackendInstance, SpawnFailure> {
            Ok(Arc::new(CountingOwner {
                instance_id: request.instance_id,
                kill_count: self.kill_count.clone(),
                killed: AtomicBool::new(false),
            }))
        }

        async fn probe(&self, _binary_path: &Path) -> blaze_core::Result<bool> {
            Ok(true)
        }

        async fn cleanup_orphan(
            &self,
            _instance_id: Uuid,
            _run_dir: &OwnedRunDir,
        ) -> blaze_core::Result<()> {
            self.orphan_cleanup_count.fetch_add(1, Ordering::AcqRel);
            Ok(())
        }
    }

    struct CaptureOnlyMockSpawner;

    #[async_trait]
    impl BackendSpawner for CaptureOnlyMockSpawner {
        async fn spawn(
            &self,
            request: BackendSpawnRequest,
        ) -> std::result::Result<DynBackendInstance, SpawnFailure> {
            MockSpawner.spawn(request).await
        }

        async fn probe(&self, _binary_path: &Path) -> blaze_core::Result<bool> {
            Ok(true)
        }

        async fn cleanup_orphan(
            &self,
            instance_id: Uuid,
            run_dir: &OwnedRunDir,
        ) -> blaze_core::Result<()> {
            MockSpawner.cleanup_orphan(instance_id, run_dir).await
        }
    }

    /// Spawns owners that expose the guest transport but restores owners that
    /// silently drop it, exercising the restore readiness contract.
    struct TransportDroppingRestoreSpawner;

    #[async_trait]
    impl BackendSpawner for TransportDroppingRestoreSpawner {
        async fn spawn(
            &self,
            request: BackendSpawnRequest,
        ) -> std::result::Result<DynBackendInstance, SpawnFailure> {
            GuestMockSpawner.spawn(request).await
        }

        async fn restore_capability(
            &self,
            _executable: Option<&crate::spawner::PinnedExecutable>,
        ) -> blaze_core::Result<Option<blaze_core::backend::RestoreCapability>> {
            // Match the identity the guest-mock owner freezes into the
            // checkpoint so the sweep reaches the readiness contract instead of
            // stopping at the version comparison.
            Ok(Some(blaze_core::backend::RestoreCapability {
                backend: BackendKind::Mock,
                version: Some("guest-mock-v1".to_string()),
                snapshot_kind: blaze_core::backend::SnapshotKind::Full,
            }))
        }

        async fn restore(
            &self,
            request: crate::spawner::BackendRestoreRequest,
        ) -> crate::spawner::RestoreResult {
            // Start an owner through the plain mock spawn path so the
            // replacement deliberately lacks the guest transport the captured
            // runtime exposed. `MockSpawner::restore` would reject the
            // guest-mock version identity before reaching this point.
            let spawn = BackendSpawnRequest::new(
                blaze_core::backend::SpawnRequest {
                    instance_id: request.instance_id,
                    binary_path: request.binary_path.clone(),
                    storage: request.storage.clone(),
                    backend: blaze_core::policy::BackendConfigs::default(),
                    vm: None,
                },
                request.run_dir.clone(),
            )
            .map_err(SpawnFailure::clean)?;
            MockSpawner.spawn(spawn).await
        }

        async fn probe(&self, _binary_path: &Path) -> blaze_core::Result<bool> {
            Ok(true)
        }

        async fn cleanup_orphan(
            &self,
            instance_id: Uuid,
            run_dir: &OwnedRunDir,
        ) -> blaze_core::Result<()> {
            GuestMockSpawner.cleanup_orphan(instance_id, run_dir).await
        }
    }

    struct StalledGuestOwner {
        instance_id: Uuid,
        socket: PathBuf,
        kill_count: Arc<AtomicUsize>,
        killed: AtomicBool,
    }

    #[async_trait]
    impl BackendInstance for StalledGuestOwner {
        fn backend(&self) -> BackendKind {
            BackendKind::Mock
        }

        fn guest_socket_path(&self) -> &Path {
            &self.socket
        }

        async fn try_wait(&self) -> blaze_core::Result<Option<SpawnResult>> {
            Ok(self.killed.load(Ordering::Acquire).then_some(SpawnResult {
                instance_id: self.instance_id,
                exit_code: Some(0),
                signal: None,
            }))
        }

        async fn kill(&self) -> blaze_core::Result<()> {
            if !self.killed.swap(true, Ordering::AcqRel) {
                self.kill_count.fetch_add(1, Ordering::AcqRel);
            }
            Ok(())
        }
    }

    struct CountingStorage {
        inner: FileStorageProvider,
        release_count: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl StorageProvider for CountingStorage {
        async fn probe(&self) -> blaze_core::Result<bool> {
            self.inner.probe().await
        }

        async fn acquire(
            &self,
            opts: &AcquireOpts,
        ) -> std::result::Result<StorageSlot, StorageAcquireError> {
            self.inner.acquire(opts).await
        }

        async fn release(&self, slot: StorageSlot) -> blaze_core::Result<()> {
            self.release_count.fetch_add(1, Ordering::AcqRel);
            self.inner.release(slot).await
        }

        async fn release_by_id(&self, instance_id: &str) -> blaze_core::Result<()> {
            self.release_count.fetch_add(1, Ordering::AcqRel);
            self.inner.release_by_id(instance_id).await
        }

        async fn reconstruct(&self, instance_id: &str) -> blaze_core::Result<StorageSlot> {
            self.inner.reconstruct(instance_id).await
        }

        async fn sync_artifacts(&self, slot: &StorageSlot) -> blaze_core::Result<()> {
            self.inner.sync_artifacts(slot).await
        }

        fn pool_status(&self) -> PoolStatus {
            self.inner.pool_status()
        }
    }

    #[cfg(feature = "test-failpoints")]
    fn counting_state(
        temp: &tempfile::TempDir,
    ) -> (
        Arc<ServerState>,
        Arc<AtomicUsize>,
        Arc<AtomicUsize>,
        Arc<AtomicUsize>,
    ) {
        let config = test_config(temp);
        let kill_count = Arc::new(AtomicUsize::new(0));
        let orphan_cleanup_count = Arc::new(AtomicUsize::new(0));
        let release_count = Arc::new(AtomicUsize::new(0));
        let storage: Arc<dyn StorageProvider> = Arc::new(CountingStorage {
            inner: FileStorageProvider::with_images(
                config.storage.images_dir.clone(),
                config.storage.instances_dir.clone(),
            ),
            release_count: release_count.clone(),
        });
        let state = build_test_state(
            config,
            test_policy(BackendKind::Mock),
            spawners(
                BackendKind::Mock,
                Arc::new(CountingSpawner {
                    kill_count: kill_count.clone(),
                    orphan_cleanup_count: orphan_cleanup_count.clone(),
                }),
            ),
            BackendKind::Mock,
            storage,
        );
        (state, kill_count, orphan_cleanup_count, release_count)
    }

    #[tokio::test]
    async fn sandbox_routes_cover_lifecycle_and_guest_operations() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let state = build_test_state(
            config,
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, Arc::new(GuestMockSpawner)),
            BackendKind::Mock,
            storage,
        );

        let (status, created) =
            dispatched_json(&state, Method::POST, "/v1/sandboxes", test_request()).await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(created["instance"]["state"], "running");
        assert!(created["decision"].is_object());
        assert_eq!(created["start_path"], "cold");
        assert_eq!(created["selected_backend"], "mock");
        let id = created["instance"]["id"]
            .as_str()
            .expect("sandbox id")
            .to_string();
        let item = format!("/v1/sandboxes/{id}");

        let (status, sandboxes) =
            dispatched_json(&state, Method::GET, "/v1/sandboxes", Vec::new()).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            sandboxes
                .as_array()
                .expect("sandbox list")
                .iter()
                .any(|candidate| candidate["id"] == id)
        );

        let (status, fetched) = dispatched_json(&state, Method::GET, &item, Vec::new()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(fetched["id"], id);

        let (status, executed) = dispatched_json(
            &state,
            Method::POST,
            &format!("{item}/exec"),
            serde_json::to_vec(&json!({"cmd": "printf sandbox", "timeout": 5}))
                .expect("exec request"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(executed["exit_code"], 0);

        let encoded = BASE64.encode(b"sandbox");
        let (status, written) = dispatched_json(
            &state,
            Method::POST,
            &format!("{item}/write"),
            serde_json::to_vec(&json!({"path": "/tmp/sandbox", "data_b64": encoded}))
                .expect("write request"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(written["bytes"], 7);

        let (status, read) = dispatched_json(
            &state,
            Method::POST,
            &format!("{item}/read"),
            serde_json::to_vec(&json!({"path": "/tmp/sandbox"})).expect("read request"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(read["data_b64"], encoded);

        let (status, destroyed) = dispatched_json(&state, Method::DELETE, &item, Vec::new()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(destroyed["destroyed"], true);
        assert_eq!(destroyed["instance_id"], id);
    }

    #[tokio::test]
    async fn reserved_pool_routes_return_not_implemented() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let state = build_test_state(
            config,
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, Arc::new(MockSpawner)),
            BackendKind::Mock,
            storage,
        );

        for (method, path) in [
            (Method::GET, "/v1/pools"),
            (Method::GET, "/v1/pools/mock/agent-tool"),
            (Method::POST, "/v1/pools/mock/agent-tool/drain"),
            (Method::PUT, "/v1/pools/mock/agent-tool/sizing"),
        ] {
            let (status, body) = handled_json(&state, method, path, Vec::new()).await;
            assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{path}");
            assert_eq!(body["status"], 501, "{path}");
            assert!(
                body["error"]
                    .as_str()
                    .expect("error")
                    .contains("warm pool management is not implemented"),
                "{path}"
            );
        }

        let (status, body) = handled_json(&state, Method::GET, "/v1/pools/mock", Vec::new()).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["status"], 404);
    }

    #[tokio::test]
    async fn health_keeps_storage_pool_status() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let state = build_test_state(
            config,
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, Arc::new(MockSpawner)),
            BackendKind::Mock,
            storage,
        );

        let (status, body) = handled_json(&state, Method::GET, "/v1/health", Vec::new()).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["status"], "ok");
        assert_eq!(body["storage_pool"]["ready"], 0);
        assert_eq!(body["storage_pool"]["capacity"], 0);
        assert_eq!(body["storage_pool"]["pending"], 0);
        assert_eq!(body["storage_pool"]["quarantined"], 0);
    }

    #[tokio::test]
    async fn unregistered_sandbox_actions_return_not_found() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let state = build_test_state(
            config,
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, Arc::new(MockSpawner)),
            BackendKind::Mock,
            storage,
        );

        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"]
            .as_str()
            .expect("sandbox id")
            .to_string();
        let uuid = Uuid::parse_str(&id).expect("uuid");

        let routes = [
            (Method::POST, format!("/v1/sandboxes/{id}/reset")),
            (Method::POST, format!("/v1/sandboxes/{id}/destroy")),
        ];

        for (method, path) in routes {
            let (status, body) = handled_json(&state, method, &path, Vec::new()).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
            assert_eq!(body["status"], 404, "{path}");
            assert!(
                body["error"]
                    .as_str()
                    .expect("error message")
                    .contains(&path),
                "{path}"
            );
            assert_eq!(
                state.manager.get(uuid).expect("unchanged state").state,
                SandboxState::Running,
                "{path}"
            );
        }

        let (status, destroyed) = dispatched_json(
            &state,
            Method::DELETE,
            &format!("/v1/sandboxes/{id}"),
            Vec::new(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(destroyed["instance_id"], id);
        assert_eq!(
            state.manager.get(uuid).expect("destroyed state").state,
            SandboxState::Destroyed
        );
        assert!(matches!(
            state.state_store.run_dir(uuid),
            Err(BlazeDaemonError::NotFound(_))
        ));
    }

    /// When multiple backend binaries exist on disk but the daemon probed
    /// Firecracker at boot, only Firecracker should be reported available
    /// and selected — even if policy prioritizes bubblewrap higher.
    #[tokio::test]
    async fn availability_constrained_to_active_backend() {
        // Create temp files to simulate both binaries existing.
        let tmp = std::env::temp_dir().join("blaze-test-active-backend");
        let _ = std::fs::create_dir_all(&tmp);
        let fc_bin = tmp.join("firecracker");
        let bwrap_bin = tmp.join("bwrap");
        std::fs::write(&fc_bin, b"fake-fc").unwrap();
        std::fs::write(&bwrap_bin, b"fake-bwrap").unwrap();

        // Minimal config with both backends present.
        let mut config = DaemonConfig::default();
        config.daemon.state_dir = tmp.join("state");
        config.template.dir = tmp.join("templates");
        let _ = std::fs::create_dir_all(&config.daemon.state_dir);
        config.backends.insert("firecracker".into(), fc_bin.clone());
        config
            .backends
            .insert("bubblewrap".into(), bwrap_bin.clone());

        // Policy that prioritizes bubblewrap over firecracker.
        let policy_file = PolicyFile {
            manifest_version: 1,
            policy_name: "test-multi-backend".into(),
            priority: 100,
            match_: PolicyMatch {
                workload_class: WorkloadClass::AgentRl,
                image_labels: HashMap::new(),
            },
            select: PolicySelect {
                backend_priority: vec![BackendKind::Bubblewrap, BackendKind::Firecracker],
                kernel_hooks: vec![],
                templates: vec![],
                fallback_on_missing_hook: FallbackOnMissingHook::default(),
            },
            pool: None,
            checkpoint: None,
            quota: None,
            hooks: PolicyHooks::default(),
            backend: BackendConfigs::default(),
            vm: None,
        };
        let engine = PolicyEngine::with_policies(vec![policy_file]);

        // Build state with active_backend = Firecracker (simulating probe
        // selected FC at boot) but using MockSpawner for test portability.
        let spawner: DynSpawner = Arc::new(MockSpawner);
        let storage_dir = tmp.join("storage");
        let _ = std::fs::create_dir_all(&storage_dir);
        let storage: Arc<dyn blaze_core::storage::StorageProvider> =
            Arc::new(FileStorageProvider::new(storage_dir));
        let state = Arc::new(
            ServerState::build(
                config,
                engine,
                HookRegistry::new(),
                spawners(BackendKind::Firecracker, spawner),
                BackendKind::Firecracker,
                storage,
            )
            .expect("state"),
        );

        // Create instance request for AgentRl workload.
        let req_body = serde_json::to_vec(&serde_json::json!({
            "workload_class": "agent-rl",
            "image_digest": "sha256:abc123",
        }))
        .unwrap();

        let resp = create_sandbox(&state, &req_body).await.unwrap();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let resp_json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        // The instance should be created with backend = firecracker,
        // NOT bubblewrap (even though bwrap was higher priority in policy)
        // because only the active backend is reported as available.
        assert_eq!(
            resp_json["instance"]["backend"].as_str().unwrap(),
            "firecracker",
            "instance backend should be the active backend (firecracker), \
             not the higher-priority bubblewrap"
        );

        // Cleanup.
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn checkpoint_rejects_unsupported_storage_without_mutation() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let storage: Arc<dyn StorageProvider> = Arc::new(NoCheckpointStorage {
            inner: FileStorageProvider::with_images(
                config.storage.images_dir.clone(),
                config.storage.instances_dir.clone(),
            ),
        });
        let state = build_test_state(
            config,
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, Arc::new(MockSpawner)),
            BackendKind::Mock,
            storage,
        );
        let request = test_request();
        let created = created_json(&state, &request).await;
        let id = created["instance"]["id"].as_str().expect("id");
        let uuid = Uuid::parse_str(id).expect("uuid");
        let state_path = configured_state_dir(&state).join(id).join("state.json");
        let persisted_before = std::fs::read(&state_path).expect("persisted state");

        let error = checkpoint(&state, id)
            .await
            .expect_err("checkpoint without backend and storage capture must fail closed");

        assert!(matches!(error, BlazeDaemonError::UnsupportedOperation(_)));
        assert_eq!(error.status_code(), 501);
        assert_eq!(
            state.instances.lock().expect("instances")[&uuid].state,
            SandboxState::Running
        );
        assert!(
            state.instances.lock().expect("instances")[&uuid]
                .operation
                .is_none()
        );
        assert_eq!(
            std::fs::read(state_path).expect("persisted state"),
            persisted_before
        );
        assert!(
            !configured_state_dir(&state)
                .join("checkpoints")
                .join(id)
                .exists()
        );
        assert!(state.manager.backend_owner(uuid).is_some());
    }

    #[tokio::test]
    async fn checkpoint_rejects_unsupported_backend_without_mutation() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let kill_count = Arc::new(AtomicUsize::new(0));
        let state = build_test_state(
            config,
            test_policy(BackendKind::Mock),
            spawners(
                BackendKind::Mock,
                Arc::new(CountingSpawner {
                    kill_count: kill_count.clone(),
                    orphan_cleanup_count: Arc::new(AtomicUsize::new(0)),
                }),
            ),
            BackendKind::Mock,
            storage,
        );
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id");
        let uuid = Uuid::parse_str(id).expect("uuid");
        let state_path = configured_state_dir(&state).join(id).join("state.json");
        let persisted_before = std::fs::read(&state_path).expect("persisted state");

        let error = checkpoint(&state, id)
            .await
            .expect_err("checkpoint without backend capture must fail closed");

        assert!(matches!(error, BlazeDaemonError::UnsupportedOperation(_)));
        assert_eq!(error.status_code(), 501);
        let lifecycle = state.manager.get(uuid).expect("lifecycle");
        assert_eq!(lifecycle.state, SandboxState::Running);
        assert!(lifecycle.operation.is_none());

        assert_eq!(
            std::fs::read(state_path).expect("persisted state"),
            persisted_before
        );
        assert!(
            !configured_state_dir(&state)
                .join("checkpoints")
                .join(id)
                .exists()
        );
        assert_eq!(kill_count.load(Ordering::Acquire), 0);
        assert!(state.manager.backend_owner(uuid).is_some());
    }

    #[tokio::test]
    async fn checkpoint_routes_capture_and_list_live_state() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let state = build_test_state(
            config,
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, Arc::new(MockSpawner)),
            BackendKind::Mock,
            storage,
        );
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id");
        let uuid = Uuid::parse_str(id).expect("uuid");
        let slot = write_checkpoint_fixture(&state, id).await;

        let (status, checkpoint) = dispatched_json(
            &state,
            Method::POST,
            &format!("/v1/sandboxes/{id}/checkpoint"),
            Vec::new(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let checkpoint_id = checkpoint["id"].as_str().expect("checkpoint id");
        assert_eq!(checkpoint["checkpoint_id"], checkpoint["id"]);
        assert_eq!(checkpoint["instance_id"], id);
        assert_eq!(checkpoint["snapshot_kind"], "full");
        assert_eq!(checkpoint["sandbox_id"], id);
        let captured_rootfs = configured_state_dir(&state)
            .join("checkpoints")
            .join(id)
            .join(checkpoint_id)
            .join("storage/rootfs.snap");
        assert_eq!(
            tokio::fs::read(&captured_rootfs)
                .await
                .expect("captured rootfs"),
            b"checkpoint-rootfs"
        );

        tokio::fs::write(&slot.rootfs_path, b"changed-after-checkpoint")
            .await
            .expect("mutate live rootfs");
        assert_eq!(
            tokio::fs::read(&captured_rootfs)
                .await
                .expect("independent captured rootfs"),
            b"checkpoint-rootfs"
        );
        let (status, checkpoints) = dispatched_json(
            &state,
            Method::GET,
            &format!("/v1/sandboxes/{id}/checkpoints"),
            Vec::new(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(checkpoints.as_array().expect("checkpoint list").len(), 1);
        assert_eq!(checkpoints[0]["id"], checkpoint_id);
        assert_eq!(checkpoints[0]["is_head"], true);
        assert_eq!(checkpoints[0]["on_head_chain"], true);

        let lifecycle = state.manager.get(uuid).expect("lifecycle");
        assert_eq!(lifecycle.state, SandboxState::Running);
        assert!(lifecycle.operation.is_none());
        assert_eq!(lifecycle.last_checkpoint.as_deref(), Some(checkpoint_id));
        assert!(state.manager.backend_owner(uuid).is_some());

        state.manager.destroy(uuid).await.expect("destroy sandbox");
        assert!(
            state
                .manager
                .list_checkpoints(uuid)
                .await
                .expect("removed checkpoint history")
                .is_empty()
        );
        assert!(
            !configured_state_dir(&state)
                .join("checkpoints")
                .join(id)
                .exists(),
            "destroy must remove the complete checkpoint namespace"
        );
    }

    #[tokio::test]
    async fn prune_route_ignores_bodies_and_returns_go_compatible_response() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let state = build_test_state(
            config,
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, Arc::new(MockSpawner)),
            BackendKind::Mock,
            storage,
        );
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id");
        let uuid = Uuid::parse_str(id).expect("uuid");
        let slot = write_checkpoint_fixture(&state, id).await;
        let root = state
            .manager
            .checkpoint(uuid)
            .await
            .expect("root checkpoint")
            .id;
        tokio::fs::write(&slot.rootfs_path, b"second-rootfs")
            .await
            .expect("second rootfs");
        let head = state
            .manager
            .checkpoint(uuid)
            .await
            .expect("head checkpoint")
            .id;
        tokio::fs::write(&slot.rootfs_path, b"unreachable-rootfs")
            .await
            .expect("unreachable rootfs");
        let unreachable = state
            .manager
            .checkpoint(uuid)
            .await
            .expect("unreachable checkpoint")
            .id;
        state
            .manager
            .restore(
                uuid,
                RestoreSandbox {
                    checkpoint_id: head.clone(),
                    binary_path: PathBuf::new(),
                },
            )
            .await
            .expect("move HEAD away from the unreachable branch");

        let empty_object = serde_json::to_vec(&json!({})).expect("empty object");
        let (status, response) = handled_json(
            &state,
            Method::POST,
            &format!("/v1/sandboxes/{id}/checkpoints/prune"),
            empty_object,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            response,
            json!({
                "status": "pruned",
                "removed_count": 1,
                "removed": [unreachable.clone()],
            })
        );

        let obsolete_body = serde_json::to_vec(&json!({
            "protected": [unreachable.clone()],
        }))
        .expect("obsolete prune body");
        for ignored_body in [obsolete_body, b"not-json".to_vec()] {
            let (status, response) = handled_json(
                &state,
                Method::POST,
                &format!("/v1/sandboxes/{id}/checkpoints/prune"),
                ignored_body,
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(
                response,
                json!({
                    "status": "pruned",
                    "removed_count": 0,
                    "removed": [],
                })
            );
        }

        let unread_request = Request::builder()
            .method(Method::POST)
            .uri(format!("/v1/sandboxes/{id}/checkpoints/prune"))
            .header(hyper::header::CONTENT_LENGTH, u64::MAX)
            .body(BodyThatMustNotBeRead)
            .expect("unread request");
        let unread_response = handle_request(unread_request, state.clone())
            .await
            .expect("infallible response");
        assert_eq!(unread_response.status(), StatusCode::OK);
        let unread_body = unread_response
            .into_body()
            .collect()
            .await
            .expect("response body")
            .to_bytes();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&unread_body).expect("response json"),
            json!({
                "status": "pruned",
                "removed_count": 0,
                "removed": [],
            })
        );
        let remaining: std::collections::HashSet<String> = state
            .manager
            .list_checkpoints(uuid)
            .await
            .expect("list after prune")
            .into_iter()
            .map(|checkpoint| checkpoint.id)
            .collect();
        assert!(remaining.contains(&root));
        assert!(remaining.contains(&head));
        assert!(!remaining.contains(&unreachable));
        let lifecycle = state.manager.get(uuid).expect("lifecycle");
        assert_eq!(lifecycle.state, SandboxState::Running);
        assert!(lifecycle.operation.is_none());
    }

    #[tokio::test]
    async fn prune_catalog_error_clears_operation_without_deleting_history() {
        let temp = tempfile::tempdir().expect("temp");
        let state = mock_state(&temp);
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id");
        let uuid = Uuid::parse_str(id).expect("uuid");
        write_checkpoint_fixture(&state, id).await;
        let head = state
            .manager
            .checkpoint(uuid)
            .await
            .expect("head checkpoint")
            .id;
        let namespace = configured_state_dir(&state).join("checkpoints").join(id);
        let checkpoint = namespace.join(&head);
        tokio::fs::write(checkpoint.join("metadata.json"), b"{")
            .await
            .expect("corrupt checkpoint metadata");

        let (status, error) = handled_json(
            &state,
            Method::POST,
            &format!("/v1/sandboxes/{id}/checkpoints/prune"),
            Vec::new(),
        )
        .await;

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(error["status"], 500);
        assert!(
            error["error"]
                .as_str()
                .expect("error")
                .contains("checkpoint metadata")
        );
        let lifecycle = state.manager.get(uuid).expect("lifecycle after failure");
        assert_eq!(lifecycle.state, SandboxState::Running);
        assert!(lifecycle.operation.is_none());
        assert!(checkpoint.is_dir());
        assert_eq!(
            std::fs::read_to_string(namespace.join("HEAD"))
                .expect("checkpoint HEAD")
                .trim(),
            head
        );
    }

    #[tokio::test]
    async fn prune_rejects_a_vanished_namespace_after_a_checkpoint() {
        let temp = tempfile::tempdir().expect("temp");
        let state = mock_state(&temp);
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id");
        let uuid = Uuid::parse_str(id).expect("uuid");
        write_checkpoint_fixture(&state, id).await;
        let checkpoint_id = state.manager.checkpoint(uuid).await.expect("checkpoint").id;
        let namespace = configured_state_dir(&state).join("checkpoints").join(id);
        tokio::fs::remove_dir_all(&namespace)
            .await
            .expect("remove checkpoint namespace");

        let (status, error) = handled_json(
            &state,
            Method::POST,
            &format!("/v1/sandboxes/{id}/checkpoints/prune"),
            Vec::new(),
        )
        .await;

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(error["status"], 500);
        assert!(
            error["error"]
                .as_str()
                .expect("error")
                .contains("checkpoint namespace is missing")
        );
        let lifecycle = state.manager.get(uuid).expect("lifecycle after failure");
        assert_eq!(lifecycle.state, SandboxState::Running);
        assert!(lifecycle.operation.is_none());
        assert_eq!(
            lifecycle.last_checkpoint.as_deref(),
            Some(checkpoint_id.as_str())
        );
    }

    #[tokio::test]
    async fn prune_rejects_a_nonempty_catalog_without_head() {
        let temp = tempfile::tempdir().expect("temp");
        let state = mock_state(&temp);
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id");
        let uuid = Uuid::parse_str(id).expect("uuid");
        write_checkpoint_fixture(&state, id).await;
        let checkpoint_id = state.manager.checkpoint(uuid).await.expect("checkpoint").id;
        let namespace = configured_state_dir(&state).join("checkpoints").join(id);
        tokio::fs::remove_file(namespace.join("HEAD"))
            .await
            .expect("remove checkpoint HEAD");

        let (status, error) = handled_json(
            &state,
            Method::POST,
            &format!("/v1/sandboxes/{id}/checkpoints/prune"),
            Vec::new(),
        )
        .await;

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(error["status"], 500);
        assert!(
            error["error"]
                .as_str()
                .expect("error")
                .contains("committed checkpoints but no HEAD")
        );
        let lifecycle = state.manager.get(uuid).expect("lifecycle after failure");
        assert_eq!(lifecycle.state, SandboxState::Running);
        assert!(lifecycle.operation.is_none());
        assert!(namespace.join(checkpoint_id).is_dir());
        assert!(!namespace.join("HEAD").exists());
    }

    #[tokio::test]
    async fn prune_route_rejects_a_hibernated_sandbox_without_mutation() {
        let temp = tempfile::tempdir().expect("temp");
        let state = mock_state(&temp);
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id");
        let uuid = Uuid::parse_str(id).expect("uuid");
        {
            let mut instances = state.instances.lock().expect("instances");
            let instance = instances.get_mut(&uuid).expect("instance");
            instance.state = SandboxState::Hibernated;
            instance.backend_ownership = BackendOwnership::Stopped;
        }

        let (status, body) = handled_json(
            &state,
            Method::POST,
            &format!("/v1/sandboxes/{id}/checkpoints/prune"),
            Vec::new(),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["status"], 409);
        let lifecycle = state.manager.get(uuid).expect("lifecycle");
        assert_eq!(lifecycle.state, SandboxState::Hibernated);
        assert!(lifecycle.operation.is_none());

        {
            let mut instances = state.instances.lock().expect("instances");
            let instance = instances.get_mut(&uuid).expect("instance");
            instance.state = SandboxState::RecoveryRequired;
        }
        let (status, body) = handled_json(
            &state,
            Method::POST,
            &format!("/v1/sandboxes/{id}/checkpoints/prune"),
            Vec::new(),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["status"], 409);
        let lifecycle = state.manager.get(uuid).expect("recovery lifecycle");
        assert_eq!(lifecycle.state, SandboxState::RecoveryRequired);
        assert!(lifecycle.operation.is_none());
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn interrupted_prune_retains_a_recovery_record_and_destroy_cleans_it() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let state = build_test_state(
            config,
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, Arc::new(MockSpawner)),
            BackendKind::Mock,
            storage,
        );
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id");
        let uuid = Uuid::parse_str(id).expect("uuid");
        let slot = state.storage.reconstruct(id).await.expect("storage slot");
        tokio::fs::write(&slot.rootfs_path, b"first-rootfs")
            .await
            .expect("first rootfs");
        let first = state
            .manager
            .checkpoint(uuid)
            .await
            .expect("first checkpoint");
        tokio::fs::write(&slot.rootfs_path, b"second-rootfs")
            .await
            .expect("second rootfs");
        let second = state
            .manager
            .checkpoint(uuid)
            .await
            .expect("second checkpoint");
        state
            .manager
            .restore(
                uuid,
                RestoreSandbox {
                    checkpoint_id: first.id,
                    binary_path: PathBuf::new(),
                },
            )
            .await
            .expect("move HEAD to the first checkpoint");

        let hook = crate::failpoint::TestFailpoint::new(&["checkpoint-prune-after-tombstone"]);
        let error = hook
            .run(state.manager.prune_checkpoints(uuid))
            .await
            .expect_err("interrupted cleanup must require recovery");
        assert!(matches!(error, BlazeDaemonError::RecoveryRequired(_)));
        let lifecycle = state.manager.get(uuid).expect("lifecycle");
        assert_eq!(lifecycle.state, SandboxState::RecoveryRequired);
        assert_eq!(
            lifecycle.operation.as_ref().map(|operation| operation.kind),
            Some(OperationKind::Prune)
        );
        let checkpoint_namespace = configured_state_dir(&state).join("checkpoints").join(id);
        assert!(!checkpoint_namespace.join(second.id).exists());
        assert!(
            std::fs::read_dir(&checkpoint_namespace)
                .expect("checkpoint namespace")
                .any(|entry| entry
                    .expect("checkpoint entry")
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".prune."))
        );

        state.manager.destroy(uuid).await.expect("destroy recovery");
        assert!(!checkpoint_namespace.exists());
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn cancelled_prune_finishes_before_destroy() {
        let temp = tempfile::tempdir().expect("temp");
        let state = mock_state(&temp);
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id").to_string();
        let uuid = Uuid::parse_str(&id).expect("uuid");
        let slot = write_checkpoint_fixture(&state, &id).await;
        let first = state
            .manager
            .checkpoint(uuid)
            .await
            .expect("first checkpoint");
        tokio::fs::write(&slot.rootfs_path, b"second-rootfs")
            .await
            .expect("second rootfs");
        let _second = state
            .manager
            .checkpoint(uuid)
            .await
            .expect("second checkpoint");
        state
            .manager
            .restore(
                uuid,
                RestoreSandbox {
                    checkpoint_id: first.id,
                    binary_path: PathBuf::new(),
                },
            )
            .await
            .expect("move HEAD to the first checkpoint");

        let hook = crate::failpoint::TestFailpoint::new(&["checkpoint-before-store-prune"]);
        let prune_state = state.clone();
        let prune_hook = hook.clone();
        let prune = tokio::spawn(async move {
            prune_hook
                .run(prune_state.manager.prune_checkpoints(uuid))
                .await
        });
        hook.wait_until_paused().await;
        let interrupted = state.manager.get(uuid).expect("prune lifecycle");
        assert_eq!(interrupted.state, SandboxState::Running);
        assert_eq!(
            interrupted
                .operation
                .as_ref()
                .map(|operation| operation.kind),
            Some(OperationKind::Prune)
        );

        prune.abort();
        assert!(
            prune
                .await
                .expect_err("outer prune request must be cancelled")
                .is_cancelled()
        );
        assert!(
            state.manager.operation_lock(uuid).try_lock().is_err(),
            "the detached prune supervisor must retain checkpoint ownership"
        );

        let destroy_state = state.clone();
        let mut destroy = tokio::spawn(async move { destroy_state.manager.destroy(uuid).await });
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut destroy)
                .await
                .is_err(),
            "destroy must wait for the detached prune supervisor"
        );

        hook.release();
        tokio::time::timeout(Duration::from_secs(2), &mut destroy)
            .await
            .expect("detached prune supervisor and queued destroy must converge")
            .expect("destroy task")
            .expect("destroy after detached prune");
        let destroyed = state.manager.get(uuid).expect("destroyed lifecycle");
        assert_eq!(destroyed.state, SandboxState::Destroyed);
        assert!(destroyed.operation.is_none());
        assert!(
            !configured_state_dir(&state)
                .join("checkpoints")
                .join(&id)
                .exists()
        );
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn checkpoint_cleanup_failure_keeps_destroy_recoverable() {
        let temp = tempfile::tempdir().expect("temp");
        let state = mock_state(&temp);
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id");
        let uuid = Uuid::parse_str(id).expect("uuid");
        write_checkpoint_fixture(&state, id).await;
        state
            .manager
            .checkpoint(uuid)
            .await
            .expect("seed checkpoint");
        let checkpoint_namespace = configured_state_dir(&state).join("checkpoints").join(id);
        let hook = crate::failpoint::TestFailpoint::new(&[
            "checkpoint-store-sandbox-remove-before-unlink",
        ]);

        let error = hook
            .run(state.manager.destroy(uuid))
            .await
            .expect_err("checkpoint namespace cleanup must fail");

        assert!(matches!(error, BlazeDaemonError::RecoveryRequired(_)));
        assert_eq!(
            state.manager.get(uuid).expect("recovery lifecycle").state,
            SandboxState::RecoveryRequired
        );
        assert!(checkpoint_namespace.is_dir());
        assert_eq!(
            std::fs::read_dir(&checkpoint_namespace)
                .expect("retained checkpoint namespace")
                .count(),
            0,
            "partial cleanup must leave no committed checkpoint payload"
        );

        state.manager.destroy(uuid).await.expect("destroy retry");
        assert_eq!(
            state.manager.get(uuid).expect("destroyed lifecycle").state,
            SandboxState::Destroyed
        );
        assert!(!checkpoint_namespace.exists());
    }

    #[tokio::test]
    async fn hibernate_releases_the_backend_and_resume_survives_restart() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let state = build_test_state(
            config.clone(),
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, Arc::new(GuestMockSpawner)),
            BackendKind::Mock,
            storage,
        );
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id");
        let uuid = Uuid::parse_str(id).expect("uuid");
        state
            .manager
            .write_file(uuid, "/tmp/value".to_string(), b"hibernate-memory")
            .await
            .expect("write guest state");

        let (status, hibernated) = dispatched_json(
            &state,
            Method::POST,
            &format!("/v1/sandboxes/{id}/hibernate"),
            Vec::new(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(hibernated["state"], "hibernated");
        assert_eq!(hibernated["backend_ownership"], "stopped");
        assert!(state.manager.backend_owner(uuid).is_none());
        let hibernate_dir = config.daemon.state_dir.join(id).join("hibernate");
        // The guest mock captures a directory-shaped payload into its own
        // subtree; the manifest inventories it beside the payload root.
        for name in [
            "manifest.json",
            "backend/image/checkpoint.img",
            "backend/image/pages.bin",
            "backend/bundle/config.json",
        ] {
            assert!(hibernate_dir.join(name).is_file(), "{name} is missing");
        }
        let report = state.manager.reconcile_startup().await;
        assert_eq!(report.attempted, 0);
        assert!(report.failures.is_empty());
        drop(state);

        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let restarted = build_test_state(
            config,
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, Arc::new(GuestMockSpawner)),
            BackendKind::Mock,
            storage,
        );
        assert_eq!(
            restarted.manager.get(uuid).expect("loaded state").state,
            SandboxState::Hibernated
        );
        let report = restarted.manager.reconcile_startup().await;
        assert_eq!(report.attempted, 0);
        assert!(report.failures.is_empty());

        let (status, resumed) = dispatched_json(
            &restarted,
            Method::POST,
            &format!("/v1/sandboxes/{id}/resume"),
            Vec::new(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(resumed["state"], "running");
        assert_eq!(
            restarted
                .manager
                .read_file(uuid, "/tmp/value".to_string())
                .await
                .expect("read resumed guest state"),
            b"hibernate-memory"
        );
        assert!(
            hibernate_dir.is_dir(),
            "the last hibernation image remains available until replacement or destroy"
        );
        assert!(restarted.manager.destroy(uuid).await.expect("destroy"));
        assert!(!hibernate_dir.exists());
    }

    #[tokio::test]
    async fn hibernate_rejects_a_capture_only_backend_before_state_mutation() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let state = build_test_state(
            config,
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, Arc::new(CaptureOnlyMockSpawner)),
            BackendKind::Mock,
            storage,
        );
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id");
        let uuid = Uuid::parse_str(id).expect("uuid");
        let owner = state.manager.backend_owner(uuid).expect("owner");

        let error = state
            .manager
            .hibernate(
                uuid,
                HibernateSandbox {
                    binary_path: PathBuf::new(),
                },
            )
            .await
            .expect_err("resume capability is required");

        assert!(matches!(error, BlazeDaemonError::UnsupportedOperation(_)));
        let retained = state.manager.backend_owner(uuid).expect("retained owner");
        assert!(Arc::ptr_eq(&owner, &retained));
        let lifecycle = state.manager.get(uuid).expect("lifecycle");
        assert_eq!(lifecycle.state, SandboxState::Running);
        assert!(lifecycle.operation.is_none());
    }

    #[tokio::test]
    async fn resume_rejects_corrupted_hibernation_artifacts_without_starting_a_backend() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let state = build_test_state(
            config.clone(),
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, Arc::new(MockSpawner)),
            BackendKind::Mock,
            storage,
        );
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id");
        let uuid = Uuid::parse_str(id).expect("uuid");
        state
            .manager
            .hibernate(
                uuid,
                HibernateSandbox {
                    binary_path: PathBuf::new(),
                },
            )
            .await
            .expect("hibernate");
        tokio::fs::write(
            config
                .daemon
                .state_dir
                .join(id)
                .join("hibernate/backend/memory.snap"),
            b"corrupted",
        )
        .await
        .expect("corrupt artifact");

        let error = state
            .manager
            .resume(
                uuid,
                ResumeSandbox {
                    binary_path: PathBuf::new(),
                },
            )
            .await
            .expect_err("corrupted artifact must fail closed");

        assert!(matches!(error, BlazeDaemonError::RecoveryRequired(_)));
        assert!(state.manager.backend_owner(uuid).is_none());
        let lifecycle = state.manager.get(uuid).expect("lifecycle");
        assert_eq!(lifecycle.state, SandboxState::RecoveryRequired);
        assert!(lifecycle.operation.is_none());
        assert!(state.manager.destroy(uuid).await.expect("destroy"));
    }

    #[tokio::test]
    async fn startup_retains_an_interrupted_hibernation_for_explicit_cleanup() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let mut instance = SandboxInstance::new(
            BackendKind::Mock,
            WorkloadClass::AgentTool,
            "sha256:ownership-test".into(),
            "ownership-test".into(),
        );
        instance
            .transition(SandboxState::Creating)
            .expect("creating");
        instance.transition(SandboxState::Running).expect("running");
        instance.backend_ownership = BackendOwnership::Running;
        instance
            .begin_hibernate_operation()
            .expect("begin hibernation");
        instance
            .transition(SandboxState::Hibernating)
            .expect("hibernating");
        instance.persist(&config.daemon.state_dir).expect("persist");
        storage
            .acquire(&AcquireOpts {
                instance_id: instance.id.to_string(),
                rootfs_size: 4096,
                mem_size: 4096,
            })
            .await
            .expect("storage");
        let id = instance.id;
        let state = build_test_state(
            config,
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, Arc::new(MockSpawner)),
            BackendKind::Mock,
            storage,
        );

        let report = state.manager.reconcile_startup().await;
        assert_eq!(report.attempted, 0);
        assert!(report.failures.is_empty());
        let retained = state.manager.get(id).expect("retained lifecycle");
        assert_eq!(retained.state, SandboxState::RecoveryRequired);
        assert_eq!(
            retained.operation.as_ref().map(|operation| operation.kind),
            Some(OperationKind::Hibernate)
        );
        assert!(state.manager.destroy(id).await.expect("explicit destroy"));
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn hibernate_snapshot_failure_resumes_the_existing_backend() {
        let temp = tempfile::tempdir().expect("temp");
        let state = mock_state(&temp);
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id");
        let uuid = Uuid::parse_str(id).expect("uuid");
        let owner = state.manager.backend_owner(uuid).expect("owner");
        let hook = crate::failpoint::TestFailpoint::new(&["hibernate-snapshot"]);

        hook.run(state.manager.hibernate(
            uuid,
            HibernateSandbox {
                binary_path: PathBuf::new(),
            },
        ))
        .await
        .expect_err("snapshot failure");

        let retained = state.manager.backend_owner(uuid).expect("retained owner");
        assert!(Arc::ptr_eq(&owner, &retained));
        let lifecycle = state.manager.get(uuid).expect("lifecycle");
        assert_eq!(lifecycle.state, SandboxState::Running);
        assert_eq!(lifecycle.backend_ownership, BackendOwnership::Running);
        assert!(lifecycle.operation.is_none());
        let names = std::fs::read_dir(temp.path().join("state").join(id))
            .expect("instance directory")
            .map(|entry| entry.expect("entry").file_name())
            .collect::<Vec<_>>();
        assert!(
            names
                .iter()
                .all(|name| !name.to_string_lossy().starts_with(".hibernate."))
        );
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn hibernate_compensation_requires_guest_readiness() {
        let temp = tempfile::tempdir().expect("temp");
        let state = guest_mock_state(&temp);
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id");
        let uuid = Uuid::parse_str(id).expect("uuid");
        let hook =
            crate::failpoint::TestFailpoint::new(&["hibernate-snapshot", "resume-guest-ready"]);

        let error = hook
            .run(state.manager.hibernate(
                uuid,
                HibernateSandbox {
                    binary_path: PathBuf::new(),
                },
            ))
            .await
            .expect_err("guest readiness must fail closed");

        assert!(matches!(error, BlazeDaemonError::RecoveryRequired(_)));
        assert!(state.manager.backend_owner(uuid).is_some());
        let lifecycle = state.manager.get(uuid).expect("lifecycle");
        assert_eq!(lifecycle.state, SandboxState::RecoveryRequired);
        assert_eq!(lifecycle.backend_ownership, BackendOwnership::Unknown);
        assert_eq!(
            lifecycle.operation.as_ref().map(|operation| operation.kind),
            Some(OperationKind::Hibernate)
        );
        assert!(state.manager.destroy(uuid).await.expect("destroy"));
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn uncertain_hibernate_stop_retains_the_existing_owner() {
        let temp = tempfile::tempdir().expect("temp");
        let state = mock_state(&temp);
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id");
        let uuid = Uuid::parse_str(id).expect("uuid");
        let owner = state.manager.backend_owner(uuid).expect("owner");
        let hook = crate::failpoint::TestFailpoint::new(&["hibernate-backend-stop"]);

        let error = hook
            .run(state.manager.hibernate(
                uuid,
                HibernateSandbox {
                    binary_path: PathBuf::new(),
                },
            ))
            .await
            .expect_err("uncertain stop must retain ownership");

        assert!(matches!(error, BlazeDaemonError::RecoveryRequired(_)));
        let retained = state.manager.backend_owner(uuid).expect("retained owner");
        assert!(Arc::ptr_eq(&owner, &retained));
        let lifecycle = state.manager.get(uuid).expect("lifecycle");
        assert_eq!(lifecycle.state, SandboxState::RecoveryRequired);
        assert_eq!(lifecycle.backend_ownership, BackendOwnership::Unknown);
        assert_eq!(
            lifecycle
                .operation
                .as_ref()
                .and_then(|operation| operation.phase),
            Some(OperationPhase::HibernateArtifactsSynced)
        );
        assert!(state.manager.destroy(uuid).await.expect("destroy"));
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn hibernate_publish_failure_retains_stopped_ownership_for_destroy() {
        let temp = tempfile::tempdir().expect("temp");
        let state = mock_state(&temp);
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id");
        let uuid = Uuid::parse_str(id).expect("uuid");
        let hook = crate::failpoint::TestFailpoint::new(&["hibernate-publish"]);

        let error = hook
            .run(state.manager.hibernate(
                uuid,
                HibernateSandbox {
                    binary_path: PathBuf::new(),
                },
            ))
            .await
            .expect_err("publish failure follows backend stop");

        assert!(matches!(error, BlazeDaemonError::RecoveryRequired(_)));
        assert!(state.manager.backend_owner(uuid).is_none());
        let lifecycle = state.manager.get(uuid).expect("lifecycle");
        assert_eq!(lifecycle.state, SandboxState::RecoveryRequired);
        assert_eq!(lifecycle.backend_ownership, BackendOwnership::Stopped);
        assert_eq!(
            lifecycle
                .operation
                .as_ref()
                .and_then(|operation| operation.phase),
            Some(OperationPhase::HibernateBackendStopped)
        );
        assert!(state.manager.destroy(uuid).await.expect("destroy"));
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn resume_start_failure_preserves_retryable_hibernation() {
        let temp = tempfile::tempdir().expect("temp");
        let state = mock_state(&temp);
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id");
        let uuid = Uuid::parse_str(id).expect("uuid");
        state
            .manager
            .hibernate(
                uuid,
                HibernateSandbox {
                    binary_path: PathBuf::new(),
                },
            )
            .await
            .expect("hibernate");
        let hook = crate::failpoint::TestFailpoint::new(&["resume-backend-start"]);

        hook.run(state.manager.resume(
            uuid,
            ResumeSandbox {
                binary_path: PathBuf::new(),
            },
        ))
        .await
        .expect_err("resume start failure");

        assert!(state.manager.backend_owner(uuid).is_none());
        let lifecycle = state.manager.get(uuid).expect("lifecycle");
        assert_eq!(lifecycle.state, SandboxState::Hibernated);
        assert_eq!(lifecycle.backend_ownership, BackendOwnership::Stopped);
        assert!(lifecycle.operation.is_none());
        state
            .manager
            .resume(
                uuid,
                ResumeSandbox {
                    binary_path: PathBuf::new(),
                },
            )
            .await
            .expect("retry resume");
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn resume_readiness_failure_cleans_the_replacement_backend() {
        let temp = tempfile::tempdir().expect("temp");
        let state = guest_mock_state(&temp);
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id");
        let uuid = Uuid::parse_str(id).expect("uuid");
        state
            .manager
            .hibernate(
                uuid,
                HibernateSandbox {
                    binary_path: PathBuf::new(),
                },
            )
            .await
            .expect("hibernate");
        let hook = crate::failpoint::TestFailpoint::new(&["resume-guest-ready"]);

        hook.run(state.manager.resume(
            uuid,
            ResumeSandbox {
                binary_path: PathBuf::new(),
            },
        ))
        .await
        .expect_err("readiness failure");

        assert!(state.manager.backend_owner(uuid).is_none());
        let lifecycle = state.manager.get(uuid).expect("lifecycle");
        assert_eq!(lifecycle.state, SandboxState::Hibernated);
        assert_eq!(lifecycle.backend_ownership, BackendOwnership::Stopped);
        assert!(lifecycle.operation.is_none());
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn resume_cleanup_failure_retains_the_replacement_owner() {
        let temp = tempfile::tempdir().expect("temp");
        let state = guest_mock_state(&temp);
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id");
        let uuid = Uuid::parse_str(id).expect("uuid");
        state
            .manager
            .hibernate(
                uuid,
                HibernateSandbox {
                    binary_path: PathBuf::new(),
                },
            )
            .await
            .expect("hibernate");
        let hook =
            crate::failpoint::TestFailpoint::new(&["resume-guest-ready", "resume-backend-stop"]);

        let error = hook
            .run(state.manager.resume(
                uuid,
                ResumeSandbox {
                    binary_path: PathBuf::new(),
                },
            ))
            .await
            .expect_err("failed cleanup must retain ownership");

        assert!(matches!(error, BlazeDaemonError::RecoveryRequired(_)));
        assert!(state.manager.backend_owner(uuid).is_some());
        let lifecycle = state.manager.get(uuid).expect("lifecycle");
        assert_eq!(lifecycle.state, SandboxState::RecoveryRequired);
        assert_eq!(lifecycle.backend_ownership, BackendOwnership::Unknown);
        assert_eq!(
            lifecycle.operation.as_ref().map(|operation| operation.kind),
            Some(OperationKind::Resume)
        );
        assert_eq!(
            lifecycle
                .operation
                .as_ref()
                .and_then(|operation| operation.phase),
            Some(OperationPhase::ResumeBackendStarted)
        );
        assert!(state.manager.destroy(uuid).await.expect("destroy"));
    }

    #[tokio::test]
    async fn rollback_replaces_runtime_state_without_rewriting_capture_history() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let state = build_test_state(
            config,
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, Arc::new(MockSpawner)),
            BackendKind::Mock,
            storage,
        );
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id");
        let uuid = Uuid::parse_str(id).expect("uuid");
        let slot = state.storage.reconstruct(id).await.expect("storage slot");

        tokio::fs::write(&slot.rootfs_path, b"first-rootfs")
            .await
            .expect("first rootfs");
        let (_, first) = dispatched_json(
            &state,
            Method::POST,
            &format!("/v1/sandboxes/{id}/checkpoint"),
            Vec::new(),
        )
        .await;
        let first_id = first["id"].as_str().expect("first checkpoint");

        tokio::fs::write(&slot.rootfs_path, b"second-rootfs")
            .await
            .expect("second rootfs");
        let (_, second) = dispatched_json(
            &state,
            Method::POST,
            &format!("/v1/sandboxes/{id}/checkpoint"),
            Vec::new(),
        )
        .await;
        let second_id = second["id"].as_str().expect("second checkpoint");

        tokio::fs::write(&slot.rootfs_path, b"third-rootfs")
            .await
            .expect("third rootfs");

        let (status, restored) = dispatched_json(
            &state,
            Method::POST,
            &format!("/v1/sandboxes/{id}/rollback/{first_id}"),
            Vec::new(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(restored["instance_id"], id);
        assert_eq!(restored["checkpoint_id"], first_id);
        assert_eq!(restored["restored"], true);
        assert_eq!(restored["state"], "running");
        assert_eq!(
            tokio::fs::read(&slot.rootfs_path)
                .await
                .expect("restored rootfs"),
            b"first-rootfs"
        );
        let lifecycle = state.manager.get(uuid).expect("lifecycle");
        assert_eq!(lifecycle.state, SandboxState::Running);
        assert!(lifecycle.operation.is_none());
        assert_eq!(lifecycle.last_checkpoint.as_deref(), Some(second_id));
        assert_eq!(
            state
                .manager
                .list_checkpoints(uuid)
                .await
                .expect("checkpoint list")
                .iter()
                .find(|checkpoint| checkpoint.is_head)
                .map(|checkpoint| checkpoint.id.as_str()),
            Some(first_id)
        );
        assert!(state.manager.backend_owner(uuid).is_some());
        for name in [
            ".rootfs.restore-copying",
            ".rootfs.restore-staged",
            ".rootfs.restore-backup",
            ".rootfs.restore-discard",
            ".rootfs.restore.json",
            ".rootfs.restore-journal.tmp",
        ] {
            assert!(!slot.instance_dir.join(name).exists(), "{name} remains");
        }
    }

    #[tokio::test]
    async fn rollback_rejects_an_unavailable_adapter_before_mutation() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let state = build_test_state(
            config,
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, Arc::new(CaptureOnlyMockSpawner)),
            BackendKind::Mock,
            storage,
        );
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id");
        let uuid = Uuid::parse_str(id).expect("uuid");
        let slot = write_checkpoint_fixture(&state, id).await;
        let checkpoint = state.manager.checkpoint(uuid).await.expect("checkpoint");
        tokio::fs::write(&slot.rootfs_path, b"current-rootfs")
            .await
            .expect("current rootfs");
        let owner = state.manager.backend_owner(uuid).expect("backend owner");

        let error = state
            .manager
            .restore(
                uuid,
                RestoreSandbox {
                    checkpoint_id: checkpoint.id,
                    binary_path: PathBuf::new(),
                },
            )
            .await
            .expect_err("restore must require an adapter");

        assert!(matches!(error, BlazeDaemonError::UnsupportedOperation(_)));
        assert_eq!(
            tokio::fs::read(&slot.rootfs_path)
                .await
                .expect("unchanged rootfs"),
            b"current-rootfs"
        );
        let retained = state.manager.backend_owner(uuid).expect("retained owner");
        assert!(Arc::ptr_eq(&owner, &retained));
        let lifecycle = state.manager.get(uuid).expect("lifecycle");
        assert_eq!(lifecycle.state, SandboxState::Running);
        assert!(lifecycle.operation.is_none());
    }

    #[tokio::test]
    async fn rollback_missing_checkpoint_returns_not_found_without_mutation() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let state = build_test_state(
            config,
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, Arc::new(MockSpawner)),
            BackendKind::Mock,
            storage,
        );
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id");
        let uuid = Uuid::parse_str(id).expect("uuid");
        let slot = state.storage.reconstruct(id).await.expect("storage slot");
        tokio::fs::write(&slot.rootfs_path, b"current-rootfs")
            .await
            .expect("current rootfs");
        let owner = state.manager.backend_owner(uuid).expect("backend owner");

        let missing = format!("ckpt-{}", Uuid::new_v4());
        let (status, body) = handled_json(
            &state,
            Method::POST,
            &format!("/v1/sandboxes/{id}/rollback/{missing}"),
            Vec::new(),
        )
        .await;

        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "an absent checkpoint must not surface as a retriable server failure"
        );
        assert_eq!(body["status"], 404);
        assert_eq!(
            tokio::fs::read(&slot.rootfs_path)
                .await
                .expect("unchanged rootfs"),
            b"current-rootfs"
        );
        let retained = state.manager.backend_owner(uuid).expect("retained owner");
        assert!(Arc::ptr_eq(&owner, &retained));
        let lifecycle = state.manager.get(uuid).expect("lifecycle");
        assert_eq!(lifecycle.state, SandboxState::Running);
        assert!(lifecycle.operation.is_none());
    }

    #[tokio::test]
    async fn rollback_rejects_a_replacement_that_drops_the_guest_transport() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let state = build_test_state(
            config,
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, Arc::new(TransportDroppingRestoreSpawner)),
            BackendKind::Mock,
            storage,
        );
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id");
        let uuid = Uuid::parse_str(id).expect("uuid");
        // The captured runtime exposes a guest socket.
        assert!(
            !state
                .manager
                .backend_owner(uuid)
                .expect("backend owner")
                .guest_socket_path()
                .as_os_str()
                .is_empty(),
            "the captured runtime must expose the guest transport"
        );
        write_checkpoint_fixture(&state, id).await;
        let checkpoint = state.manager.checkpoint(uuid).await.expect("checkpoint");

        let error = state
            .manager
            .restore(
                uuid,
                RestoreSandbox {
                    checkpoint_id: checkpoint.id,
                    binary_path: PathBuf::new(),
                },
            )
            .await
            .expect_err("a replacement without the guest transport must not publish");

        assert!(
            matches!(error, BlazeDaemonError::RecoveryRequired(_)),
            "expected RecoveryRequired, got {error:?}"
        );
        let lifecycle = state.manager.get(uuid).expect("lifecycle");
        assert_eq!(
            lifecycle.state,
            SandboxState::RecoveryRequired,
            "the sandbox must not be published as running without its transport"
        );
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn restore_stage_failure_keeps_the_current_runtime_running() {
        let temp = tempfile::tempdir().expect("temp");
        let state = mock_state(&temp);
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id");
        let uuid = Uuid::parse_str(id).expect("uuid");
        let slot = write_checkpoint_fixture(&state, id).await;
        let checkpoint = state.manager.checkpoint(uuid).await.expect("checkpoint");
        tokio::fs::write(&slot.rootfs_path, b"current-rootfs")
            .await
            .expect("current rootfs");
        let owner = state.manager.backend_owner(uuid).expect("backend owner");
        let hook = crate::failpoint::TestFailpoint::new(&["restore-storage-stage"]);

        hook.run(state.manager.restore(
            uuid,
            RestoreSandbox {
                checkpoint_id: checkpoint.id,
                binary_path: PathBuf::new(),
            },
        ))
        .await
        .expect_err("stage failure");

        let retained = state.manager.backend_owner(uuid).expect("retained owner");
        assert!(Arc::ptr_eq(&owner, &retained));
        assert_eq!(
            tokio::fs::read(&slot.rootfs_path)
                .await
                .expect("unchanged rootfs"),
            b"current-rootfs"
        );
        let lifecycle = state.manager.get(uuid).expect("lifecycle");
        assert_eq!(lifecycle.state, SandboxState::Running);
        assert!(lifecycle.operation.is_none());
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn uncertain_backend_stop_retains_the_current_owner_and_rootfs() {
        let temp = tempfile::tempdir().expect("temp");
        let state = mock_state(&temp);
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id");
        let uuid = Uuid::parse_str(id).expect("uuid");
        let slot = write_checkpoint_fixture(&state, id).await;
        let checkpoint = state.manager.checkpoint(uuid).await.expect("checkpoint");
        tokio::fs::write(&slot.rootfs_path, b"current-rootfs")
            .await
            .expect("current rootfs");
        let owner = state.manager.backend_owner(uuid).expect("backend owner");
        let hook = crate::failpoint::TestFailpoint::new(&["restore-backend-stop"]);

        let error = hook
            .run(state.manager.restore(
                uuid,
                RestoreSandbox {
                    checkpoint_id: checkpoint.id,
                    binary_path: PathBuf::new(),
                },
            ))
            .await
            .expect_err("backend stop outcome must require recovery");

        assert!(matches!(error, BlazeDaemonError::RecoveryRequired(_)));
        let retained = state.manager.backend_owner(uuid).expect("retained owner");
        assert!(Arc::ptr_eq(&owner, &retained));
        assert_eq!(
            tokio::fs::read(&slot.rootfs_path)
                .await
                .expect("unchanged rootfs"),
            b"current-rootfs"
        );
        let lifecycle = state.manager.get(uuid).expect("lifecycle");
        assert_eq!(lifecycle.state, SandboxState::RecoveryRequired);
        assert_eq!(lifecycle.backend_ownership, BackendOwnership::Unknown);
        assert_eq!(
            lifecycle
                .operation
                .as_ref()
                .and_then(|operation| operation.phase),
            Some(OperationPhase::RestoreStorageStaged)
        );
        for name in [
            ".rootfs.restore-staged",
            ".rootfs.restore-backup",
            ".rootfs.restore.json",
        ] {
            assert!(!slot.instance_dir.join(name).exists(), "{name} remains");
        }
        assert!(state.manager.destroy(uuid).await.expect("destroy"));
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn uncertain_head_update_retains_the_replacement_owner() {
        let temp = tempfile::tempdir().expect("temp");
        let state = mock_state(&temp);
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id");
        let uuid = Uuid::parse_str(id).expect("uuid");
        let slot = write_checkpoint_fixture(&state, id).await;
        let checkpoint = state.manager.checkpoint(uuid).await.expect("checkpoint");
        tokio::fs::write(&slot.rootfs_path, b"later-checkpoint-rootfs")
            .await
            .expect("later checkpoint rootfs");
        let latest = state
            .manager
            .checkpoint(uuid)
            .await
            .expect("later checkpoint");
        tokio::fs::write(&slot.rootfs_path, b"current-rootfs")
            .await
            .expect("current rootfs");
        let hook = crate::failpoint::TestFailpoint::new(&["checkpoint-store-head-after-rename"]);

        let error = hook
            .run(state.manager.restore(
                uuid,
                RestoreSandbox {
                    checkpoint_id: checkpoint.id.clone(),
                    binary_path: PathBuf::new(),
                },
            ))
            .await
            .expect_err("HEAD update must be reported");

        assert!(matches!(error, BlazeDaemonError::RecoveryRequired(_)));
        assert_eq!(
            tokio::fs::read(&slot.rootfs_path)
                .await
                .expect("selected rootfs"),
            b"checkpoint-rootfs"
        );
        assert!(state.manager.backend_owner(uuid).is_some());
        let lifecycle = state.manager.get(uuid).expect("lifecycle");
        assert_eq!(lifecycle.state, SandboxState::RecoveryRequired);
        assert_eq!(lifecycle.backend_ownership, BackendOwnership::Running);
        assert_eq!(
            lifecycle
                .operation
                .as_ref()
                .and_then(|operation| operation.phase),
            Some(OperationPhase::RestoreBackendStarted)
        );
        assert_eq!(
            lifecycle.last_checkpoint.as_deref(),
            Some(latest.id.as_str())
        );
        assert_eq!(
            state
                .manager
                .list_checkpoints(uuid)
                .await
                .expect("observable checkpoint catalog")
                .iter()
                .find(|item| item.is_head)
                .map(|item| item.id.as_str()),
            Some(checkpoint.id.as_str())
        );

        assert!(state.manager.destroy(uuid).await.expect("destroy"));
        assert_eq!(
            state.manager.get(uuid).expect("destroyed").state,
            SandboxState::Destroyed
        );
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn final_state_failure_keeps_the_committed_restore_journal() {
        let temp = tempfile::tempdir().expect("temp");
        let state = mock_state(&temp);
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id");
        let uuid = Uuid::parse_str(id).expect("uuid");
        let slot = write_checkpoint_fixture(&state, id).await;
        let checkpoint = state.manager.checkpoint(uuid).await.expect("checkpoint");
        tokio::fs::write(&slot.rootfs_path, b"current-rootfs")
            .await
            .expect("current rootfs");
        let hook = crate::failpoint::TestFailpoint::new(&["restore-final-state"]);

        let error = hook
            .run(state.manager.restore(
                uuid,
                RestoreSandbox {
                    checkpoint_id: checkpoint.id.clone(),
                    binary_path: PathBuf::new(),
                },
            ))
            .await
            .expect_err("final state failure");

        assert!(matches!(error, BlazeDaemonError::RecoveryRequired(_)));
        assert_eq!(
            tokio::fs::read(&slot.rootfs_path)
                .await
                .expect("committed rootfs"),
            b"checkpoint-rootfs"
        );
        let lifecycle = state.manager.get(uuid).expect("lifecycle");
        assert_eq!(lifecycle.state, SandboxState::RecoveryRequired);
        assert_eq!(lifecycle.backend_ownership, BackendOwnership::Running);
        assert_eq!(
            lifecycle
                .operation
                .as_ref()
                .map(|operation| (operation.checkpoint_id.as_deref(), operation.phase)),
            Some((
                Some(checkpoint.id.as_str()),
                Some(OperationPhase::RestoreStorageCommitted)
            ))
        );
        assert_eq!(
            state
                .manager
                .list_checkpoints(uuid)
                .await
                .expect("checkpoint list")
                .iter()
                .find(|item| item.is_head)
                .map(|item| item.id.as_str()),
            Some(checkpoint.id.as_str())
        );
        assert!(state.manager.backend_owner(uuid).is_some());
        assert!(state.manager.destroy(uuid).await.expect("destroy"));
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn cancelled_restore_after_head_finishes_in_detached_supervisor() {
        let temp = tempfile::tempdir().expect("temp");
        let state = mock_state(&temp);
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id").to_string();
        let uuid = Uuid::parse_str(&id).expect("uuid");
        write_checkpoint_fixture(&state, &id).await;
        let checkpoint = state.manager.checkpoint(uuid).await.expect("checkpoint");
        let hook = crate::failpoint::TestFailpoint::new(&["restore-after-head"]);
        let restore_state = state.clone();
        let restore_hook = hook.clone();
        let restore = tokio::spawn(async move {
            restore_hook
                .run(restore_state.manager.restore(
                    uuid,
                    RestoreSandbox {
                        checkpoint_id: checkpoint.id,
                        binary_path: PathBuf::new(),
                    },
                ))
                .await
        });
        hook.wait_until_paused().await;

        let persisted = SandboxInstance::load(&configured_state_dir(&state), uuid)
            .expect("persisted restore journal");
        assert_eq!(persisted.state, SandboxState::Restoring);
        assert_eq!(
            persisted.operation.and_then(|operation| operation.phase),
            Some(OperationPhase::RestoreHeadUpdated)
        );
        assert_eq!(persisted.backend_ownership, BackendOwnership::Running);
        assert!(state.manager.backend_owner(uuid).is_some());

        restore.abort();
        assert!(restore.await.expect_err("cancelled restore").is_cancelled());
        let destroy_state = state.clone();
        let mut destroy = tokio::spawn(async move { destroy_state.manager.destroy(uuid).await });
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut destroy)
                .await
                .is_err(),
            "destroy must wait for the detached restore supervisor"
        );

        hook.release();
        tokio::time::timeout(Duration::from_secs(2), &mut destroy)
            .await
            .expect("detached restore supervisor and queued destroy must converge")
            .expect("destroy task")
            .expect("destroy completed restore");
        assert_eq!(
            state.manager.get(uuid).expect("destroyed").state,
            SandboxState::Destroyed
        );
        assert!(
            !state
                .config
                .lock()
                .expect("config")
                .storage
                .instances_dir
                .join(id)
                .exists()
        );
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn checkpoint_snapshot_failure_resumes_and_clears_the_journal() {
        let temp = tempfile::tempdir().expect("temp");
        let state = mock_state(&temp);
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id");
        let uuid = Uuid::parse_str(id).expect("uuid");
        let hook = crate::failpoint::TestFailpoint::new(&["checkpoint-snapshot"]);

        let error = hook
            .run(state.manager.checkpoint(uuid))
            .await
            .expect_err("snapshot failure");

        assert!(matches!(
            error,
            BlazeDaemonError::Core(BlazeError::BackendError { .. })
        ));
        let lifecycle = state.manager.get(uuid).expect("lifecycle");
        assert_eq!(lifecycle.state, SandboxState::Running);
        assert!(lifecycle.operation.is_none());
        assert_eq!(
            state
                .state_store
                .load(uuid)
                .expect("persisted lifecycle")
                .operation,
            None
        );
        let checkpoint_dir = configured_state_dir(&state).join("checkpoints").join(id);
        let staging = std::fs::read_dir(checkpoint_dir)
            .expect("checkpoint directory")
            .filter_map(std::result::Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(".ckpt-"))
            .count();
        assert_eq!(staging, 0);
        assert!(state.manager.backend_owner(uuid).is_some());
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test(flavor = "current_thread")]
    async fn checkpoint_compensation_cleanup_uses_the_blocking_pool() {
        let temp = tempfile::tempdir().expect("temp");
        let state = mock_state(&temp);
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id");
        let uuid = Uuid::parse_str(id).expect("uuid");
        write_checkpoint_fixture(&state, id).await;
        let hook = crate::failpoint::TestFailpoint::new(&[
            "checkpoint-rootfs-capture",
            "checkpoint-before-stage-abort",
        ]);
        let guard_hook = hook.clone();
        let (guard_cancel, guard_cancelled) = std::sync::mpsc::channel();
        let release_guard = std::thread::spawn(move || {
            if guard_cancelled
                .recv_timeout(Duration::from_secs(1))
                .is_err()
            {
                guard_hook.release();
            }
        });
        let started = std::time::Instant::now();
        let checkpoint_state = state.clone();
        let checkpoint_hook = hook.clone();
        let checkpoint = tokio::spawn(async move {
            checkpoint_hook
                .run(checkpoint_state.manager.checkpoint(uuid))
                .await
        });

        hook.wait_until_paused().await;
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "staging cleanup must not occupy the async runtime worker"
        );
        tokio::time::timeout(
            Duration::from_millis(250),
            tokio::time::sleep(Duration::from_millis(1)),
        )
        .await
        .expect("the async runtime must remain responsive during staging cleanup");
        assert!(
            state.manager.operation_lock(uuid).try_lock().is_err(),
            "the sandbox operation lock must remain held during staging cleanup"
        );

        hook.release();
        guard_cancel.send(()).expect("cancel release guard");
        release_guard.join().expect("release guard");
        let error = checkpoint
            .await
            .expect("checkpoint task")
            .expect_err("rootfs capture failure");
        assert!(matches!(
            error,
            BlazeDaemonError::Core(BlazeError::StorageError { .. })
        ));
        let lifecycle = state.manager.get(uuid).expect("lifecycle");
        assert_eq!(lifecycle.state, SandboxState::Running);
        assert!(lifecycle.operation.is_none());
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn checkpoint_prepublication_failure_discards_the_stage() {
        for failpoint in [
            "checkpoint-publish",
            "checkpoint-store-publish-before-rename",
        ] {
            let temp = tempfile::tempdir().expect("temp");
            let state = mock_state(&temp);
            let created = created_json(&state, &test_request()).await;
            let id = created["instance"]["id"].as_str().expect("id");
            let uuid = Uuid::parse_str(id).expect("uuid");
            write_checkpoint_fixture(&state, id).await;
            let hook = crate::failpoint::TestFailpoint::new(&[failpoint]);

            let error = hook
                .run(state.manager.checkpoint(uuid))
                .await
                .expect_err("publication must fail before the rename boundary");

            assert!(
                !matches!(error, BlazeDaemonError::RecoveryRequired(_)),
                "{failpoint} must remain a compensated failure: {error}"
            );
            let lifecycle = state.manager.get(uuid).expect("lifecycle");
            assert_eq!(lifecycle.state, SandboxState::Running);
            assert!(lifecycle.operation.is_none());
            assert_eq!(
                state
                    .state_store
                    .load(uuid)
                    .expect("persisted lifecycle")
                    .operation,
                None
            );
            assert!(
                state
                    .manager
                    .list_checkpoints(uuid)
                    .await
                    .expect("checkpoint catalog")
                    .is_empty()
            );
            let checkpoint_dir = configured_state_dir(&state).join("checkpoints").join(id);
            let staging = std::fs::read_dir(checkpoint_dir)
                .expect("checkpoint directory")
                .filter_map(std::result::Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().starts_with(".ckpt-"))
                .count();
            assert_eq!(staging, 0, "{failpoint} must remove the staging owner");
            assert!(state.manager.backend_owner(uuid).is_some());
        }
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn checkpoint_head_pre_rename_failure_resumes_without_moving_head() {
        let temp = tempfile::tempdir().expect("temp");
        let state = mock_state(&temp);
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id");
        let uuid = Uuid::parse_str(id).expect("uuid");
        write_checkpoint_fixture(&state, id).await;
        let existing_head = state
            .manager
            .checkpoint(uuid)
            .await
            .expect("establish existing HEAD")
            .id;
        let hook = crate::failpoint::TestFailpoint::new(&["checkpoint-store-head-before-rename"]);

        let error = hook
            .run(state.manager.checkpoint(uuid))
            .await
            .expect_err("HEAD update must fail before rename");

        assert!(
            !matches!(error, BlazeDaemonError::RecoveryRequired(_)),
            "known-unchanged HEAD failure must be compensated: {error}"
        );
        let lifecycle = state.manager.get(uuid).expect("lifecycle");
        assert_eq!(lifecycle.state, SandboxState::Running);
        assert!(lifecycle.operation.is_none());
        assert_eq!(
            lifecycle.last_checkpoint.as_deref(),
            Some(existing_head.as_str())
        );
        let persisted = state.state_store.load(uuid).expect("persisted lifecycle");
        assert_eq!(persisted.state, SandboxState::Running);
        assert!(persisted.operation.is_none());
        assert_eq!(
            persisted.last_checkpoint.as_deref(),
            Some(existing_head.as_str())
        );
        assert!(state.manager.backend_owner(uuid).is_some());

        let checkpoints = state
            .manager
            .list_checkpoints(uuid)
            .await
            .expect("published checkpoint");
        assert_eq!(checkpoints.len(), 2);
        assert!(
            checkpoints
                .iter()
                .any(|checkpoint| checkpoint.id == existing_head && checkpoint.is_head)
        );
        assert!(
            checkpoints
                .iter()
                .any(|checkpoint| checkpoint.id != existing_head && !checkpoint.is_head)
        );
        let checkpoint_dir = configured_state_dir(&state).join("checkpoints").join(id);
        assert_eq!(
            std::fs::read_to_string(checkpoint_dir.join("HEAD"))
                .expect("existing checkpoint HEAD")
                .trim(),
            existing_head
        );
        assert!(
            std::fs::read_dir(checkpoint_dir)
                .expect("checkpoint directory")
                .filter_map(std::result::Result::ok)
                .all(|entry| !entry.file_name().to_string_lossy().starts_with(".HEAD.")),
            "compensated HEAD failure must not retain temporary scratch"
        );
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn checkpoint_head_cleanup_failure_requires_recovery() {
        let temp = tempfile::tempdir().expect("temp");
        let state = mock_state(&temp);
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id");
        let uuid = Uuid::parse_str(id).expect("uuid");
        write_checkpoint_fixture(&state, id).await;
        let hook = crate::failpoint::TestFailpoint::new(&[
            "checkpoint-store-head-before-rename",
            "checkpoint-store-head-cleanup",
        ]);

        let error = hook
            .run(state.manager.checkpoint(uuid))
            .await
            .expect_err("failed temporary HEAD cleanup must require recovery");

        assert!(matches!(error, BlazeDaemonError::RecoveryRequired(_)));
        let lifecycle = state.manager.get(uuid).expect("lifecycle");
        assert_eq!(lifecycle.state, SandboxState::RecoveryRequired);
        assert_eq!(
            lifecycle.operation.and_then(|journal| journal.phase),
            Some(OperationPhase::CheckpointPublished)
        );
        assert!(state.manager.backend_owner(uuid).is_some());
        let checkpoints = state
            .manager
            .list_checkpoints(uuid)
            .await
            .expect("published checkpoint");
        assert_eq!(checkpoints.len(), 1);
        assert!(!checkpoints[0].is_head);
        let checkpoint_dir = configured_state_dir(&state).join("checkpoints").join(id);
        assert_eq!(
            std::fs::read_dir(checkpoint_dir)
                .expect("checkpoint directory")
                .filter_map(std::result::Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().starts_with(".HEAD."))
                .count(),
            1
        );
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn checkpoint_state_failures_retain_the_reached_durable_phase() {
        for (failpoint, expected_phase, expected_head) in [
            (
                "checkpoint-published-state",
                OperationPhase::CheckpointPublished,
                false,
            ),
            (
                "checkpoint-head-state",
                OperationPhase::CheckpointHeadUpdated,
                true,
            ),
        ] {
            let temp = tempfile::tempdir().expect("temp");
            let state = mock_state(&temp);
            let created = created_json(&state, &test_request()).await;
            let id = created["instance"]["id"].as_str().expect("id");
            let uuid = Uuid::parse_str(id).expect("uuid");
            write_checkpoint_fixture(&state, id).await;
            let hook = crate::failpoint::TestFailpoint::new(&[failpoint]);

            let error = hook
                .run(state.manager.checkpoint(uuid))
                .await
                .expect_err("state commit must fail");

            assert!(matches!(error, BlazeDaemonError::RecoveryRequired(_)));
            let lifecycle = state.manager.get(uuid).expect("lifecycle");
            assert_eq!(lifecycle.state, SandboxState::RecoveryRequired);
            assert_eq!(
                lifecycle
                    .operation
                    .as_ref()
                    .and_then(|journal| journal.phase),
                Some(expected_phase)
            );
            let checkpoints = state
                .manager
                .list_checkpoints(uuid)
                .await
                .expect("published checkpoint");
            assert_eq!(checkpoints.len(), 1);
            assert_eq!(checkpoints[0].is_head, expected_head);
            assert!(state.manager.backend_owner(uuid).is_some());
        }
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn checkpoint_intent_and_stage_cleanup_failure_retain_recovery_ownership() {
        let temp = tempfile::tempdir().expect("temp");
        let state = mock_state(&temp);
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id");
        let uuid = Uuid::parse_str(id).expect("uuid");
        write_checkpoint_fixture(&state, id).await;
        let hook = crate::failpoint::TestFailpoint::new(&[
            "checkpoint-begin-state-commit",
            "checkpoint-store-abort-before-rename",
        ]);

        let error = hook
            .run(state.manager.checkpoint(uuid))
            .await
            .expect_err("intent commit and staging cleanup must fail");

        assert!(matches!(error, BlazeDaemonError::RecoveryRequired(_)));
        assert!(
            error
                .to_string()
                .contains("checkpoint intent state commit failed")
        );
        assert!(
            error
                .to_string()
                .contains("checkpoint staging cleanup failed")
        );

        let lifecycle = state.manager.get(uuid).expect("lifecycle");
        assert_eq!(lifecycle.state, SandboxState::RecoveryRequired);
        let journal = lifecycle.operation.as_ref().expect("checkpoint journal");
        assert_eq!(journal.kind, OperationKind::Checkpoint);
        assert_eq!(journal.phase, Some(OperationPhase::CheckpointPreparing));

        let persisted = state.state_store.load(uuid).expect("persisted lifecycle");
        assert_eq!(persisted.state, SandboxState::RecoveryRequired);
        assert_eq!(persisted.operation, lifecycle.operation);

        let checkpoint_dir = configured_state_dir(&state).join("checkpoints").join(id);
        let stages = std::fs::read_dir(&checkpoint_dir)
            .expect("checkpoint directory")
            .filter_map(std::result::Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".ckpt-") && name.ends_with(".tmp"))
            .collect::<Vec<_>>();
        assert_eq!(stages.len(), 1);
        assert_eq!(
            journal.checkpoint_id.as_deref(),
            stages[0]
                .strip_prefix('.')
                .and_then(|name| name.strip_suffix(".tmp"))
        );

        let retry = state
            .manager
            .checkpoint(uuid)
            .await
            .expect_err("recovery-owned staging must block another checkpoint");
        assert!(matches!(retry, BlazeDaemonError::RecoveryRequired(_)));
        assert_eq!(
            std::fs::read_dir(checkpoint_dir)
                .expect("checkpoint directory after retry")
                .filter_map(std::result::Result::ok)
                .filter(|entry| {
                    let name = entry.file_name();
                    let name = name.to_string_lossy();
                    name.starts_with(".ckpt-") && name.ends_with(".tmp")
                })
                .count(),
            1
        );
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn checkpoint_begin_cleanup_failure_retains_recovery_ownership() {
        let temp = tempfile::tempdir().expect("temp");
        let state = mock_state(&temp);
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id");
        let uuid = Uuid::parse_str(id).expect("uuid");
        write_checkpoint_fixture(&state, id).await;
        let hook = crate::failpoint::TestFailpoint::new(&[
            "checkpoint-store-stage-parent-sync",
            "checkpoint-store-abort-before-rename",
        ]);

        let error = hook
            .run(state.manager.checkpoint(uuid))
            .await
            .expect_err("stage synchronization and cleanup must fail");

        assert!(matches!(error, BlazeDaemonError::RecoveryRequired(_)));
        assert!(
            error
                .to_string()
                .contains("checkpoint stage creation failed and cleanup could not be confirmed")
        );

        let lifecycle = state.manager.get(uuid).expect("lifecycle");
        assert_eq!(lifecycle.state, SandboxState::RecoveryRequired);
        let journal = lifecycle.operation.as_ref().expect("checkpoint journal");
        assert_eq!(journal.kind, OperationKind::Checkpoint);
        assert_eq!(journal.phase, Some(OperationPhase::CheckpointPreparing));

        let persisted = state.state_store.load(uuid).expect("persisted lifecycle");
        assert_eq!(persisted.state, SandboxState::RecoveryRequired);
        assert_eq!(persisted.operation, lifecycle.operation);

        let checkpoint_dir = configured_state_dir(&state).join("checkpoints").join(id);
        let stages = std::fs::read_dir(&checkpoint_dir)
            .expect("checkpoint directory")
            .filter_map(std::result::Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".ckpt-") && name.ends_with(".tmp"))
            .collect::<Vec<_>>();
        assert_eq!(stages.len(), 1);
        assert_eq!(
            journal.checkpoint_id.as_deref(),
            stages[0]
                .strip_prefix('.')
                .and_then(|name| name.strip_suffix(".tmp"))
        );

        let retry = state
            .manager
            .checkpoint(uuid)
            .await
            .expect_err("recovery-owned staging must block another checkpoint");
        assert!(matches!(retry, BlazeDaemonError::RecoveryRequired(_)));
        assert_eq!(
            std::fs::read_dir(checkpoint_dir)
                .expect("checkpoint directory after retry")
                .filter_map(std::result::Result::ok)
                .filter(|entry| {
                    let name = entry.file_name();
                    let name = name.to_string_lossy();
                    name.starts_with(".ckpt-") && name.ends_with(".tmp")
                })
                .count(),
            1
        );
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn checkpoint_stage_open_cleanup_failure_retains_recovery_ownership() {
        let temp = tempfile::tempdir().expect("temp");
        let state = mock_state(&temp);
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id");
        let uuid = Uuid::parse_str(id).expect("uuid");
        write_checkpoint_fixture(&state, id).await;
        let hook = crate::failpoint::TestFailpoint::new(&[
            "checkpoint-store-stage-open",
            "checkpoint-store-stage-open-cleanup-before-unlink",
        ]);

        let error = hook
            .run(state.manager.checkpoint(uuid))
            .await
            .expect_err("stage opening and cleanup must fail");

        assert!(matches!(error, BlazeDaemonError::RecoveryRequired(_)));
        assert!(
            error
                .to_string()
                .contains("checkpoint stage creation failed and cleanup could not be confirmed")
        );

        let lifecycle = state.manager.get(uuid).expect("lifecycle");
        assert_eq!(lifecycle.state, SandboxState::RecoveryRequired);
        let journal = lifecycle.operation.as_ref().expect("checkpoint journal");
        assert_eq!(journal.kind, OperationKind::Checkpoint);
        assert_eq!(journal.phase, Some(OperationPhase::CheckpointPreparing));

        let persisted = state.state_store.load(uuid).expect("persisted lifecycle");
        assert_eq!(persisted.state, SandboxState::RecoveryRequired);
        assert_eq!(persisted.operation, lifecycle.operation);

        let checkpoint_dir = configured_state_dir(&state).join("checkpoints").join(id);
        let stages = std::fs::read_dir(&checkpoint_dir)
            .expect("checkpoint directory")
            .filter_map(std::result::Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".ckpt-") && name.ends_with(".tmp"))
            .collect::<Vec<_>>();
        assert_eq!(stages.len(), 1);
        assert_eq!(
            journal.checkpoint_id.as_deref(),
            stages[0]
                .strip_prefix('.')
                .and_then(|name| name.strip_suffix(".tmp"))
        );

        let retry = state
            .manager
            .checkpoint(uuid)
            .await
            .expect_err("recovery-owned staging must block another checkpoint");
        assert!(matches!(retry, BlazeDaemonError::RecoveryRequired(_)));
        assert_eq!(
            std::fs::read_dir(checkpoint_dir)
                .expect("checkpoint directory after retry")
                .filter_map(std::result::Result::ok)
                .filter(|entry| {
                    let name = entry.file_name();
                    let name = name.to_string_lossy();
                    name.starts_with(".ckpt-") && name.ends_with(".tmp")
                })
                .count(),
            1
        );
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn checkpoint_stage_open_cleanup_sync_failure_retains_recovery_ownership() {
        let temp = tempfile::tempdir().expect("temp");
        let state = mock_state(&temp);
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id");
        let uuid = Uuid::parse_str(id).expect("uuid");
        write_checkpoint_fixture(&state, id).await;
        let hook = crate::failpoint::TestFailpoint::new(&[
            "checkpoint-store-stage-open",
            "checkpoint-store-stage-open-cleanup-parent-sync",
        ]);

        let error = hook
            .run(state.manager.checkpoint(uuid))
            .await
            .expect_err("stage opening and cleanup synchronization must fail");

        assert!(matches!(error, BlazeDaemonError::RecoveryRequired(_)));
        assert!(
            error
                .to_string()
                .contains("checkpoint stage creation failed and cleanup could not be confirmed")
        );

        let lifecycle = state.manager.get(uuid).expect("lifecycle");
        assert_eq!(lifecycle.state, SandboxState::RecoveryRequired);
        let journal = lifecycle.operation.as_ref().expect("checkpoint journal");
        assert_eq!(journal.kind, OperationKind::Checkpoint);
        assert_eq!(journal.phase, Some(OperationPhase::CheckpointPreparing));
        assert!(
            journal
                .checkpoint_id
                .as_deref()
                .is_some_and(|id| id.starts_with("ckpt-"))
        );

        let persisted = state.state_store.load(uuid).expect("persisted lifecycle");
        assert_eq!(persisted.state, SandboxState::RecoveryRequired);
        assert_eq!(persisted.operation, lifecycle.operation);

        let checkpoint_dir = configured_state_dir(&state).join("checkpoints").join(id);
        let stage_count = || {
            std::fs::read_dir(&checkpoint_dir)
                .expect("checkpoint directory")
                .filter_map(std::result::Result::ok)
                .filter(|entry| {
                    let name = entry.file_name();
                    let name = name.to_string_lossy();
                    name.starts_with(".ckpt-") && name.ends_with(".tmp")
                })
                .count()
        };
        assert_eq!(stage_count(), 0);

        let retry = state
            .manager
            .checkpoint(uuid)
            .await
            .expect_err("uncertain cleanup durability must block another checkpoint");
        assert!(matches!(retry, BlazeDaemonError::RecoveryRequired(_)));
        assert_eq!(stage_count(), 0);
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn checkpoint_store_boundary_failures_preserve_observable_catalog_truth() {
        for (failpoint, expected_phase, expected_head) in [
            (
                "checkpoint-store-publish-after-rename",
                OperationPhase::CheckpointPaused,
                false,
            ),
            (
                "checkpoint-store-head-after-rename",
                OperationPhase::CheckpointPublished,
                true,
            ),
        ] {
            let temp = tempfile::tempdir().expect("temp");
            let state = mock_state(&temp);
            let created = created_json(&state, &test_request()).await;
            let id = created["instance"]["id"].as_str().expect("id");
            let uuid = Uuid::parse_str(id).expect("uuid");
            write_checkpoint_fixture(&state, id).await;
            let hook = crate::failpoint::TestFailpoint::new(&[failpoint]);

            let error = hook
                .run(state.manager.checkpoint(uuid))
                .await
                .expect_err("durability boundary must report an uncertain result");

            assert!(matches!(error, BlazeDaemonError::RecoveryRequired(_)));
            let lifecycle = state.manager.get(uuid).expect("lifecycle");
            assert_eq!(lifecycle.state, SandboxState::RecoveryRequired);
            assert_eq!(
                lifecycle
                    .operation
                    .as_ref()
                    .and_then(|journal| journal.phase),
                Some(expected_phase)
            );
            let checkpoints = state
                .manager
                .list_checkpoints(uuid)
                .await
                .expect("observable checkpoint catalog");
            assert_eq!(checkpoints.len(), 1);
            assert_eq!(checkpoints[0].is_head, expected_head);
            assert!(state.manager.backend_owner(uuid).is_some());
        }
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn checkpoint_resume_failure_keeps_head_and_runtime_ownership() {
        let temp = tempfile::tempdir().expect("temp");
        let state = mock_state(&temp);
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id");
        let uuid = Uuid::parse_str(id).expect("uuid");
        write_checkpoint_fixture(&state, id).await;
        let hook = crate::failpoint::TestFailpoint::new(&["checkpoint-resume"]);

        let error = hook
            .run(state.manager.checkpoint(uuid))
            .await
            .expect_err("resume failure");

        assert!(matches!(error, BlazeDaemonError::RecoveryRequired(_)));
        let lifecycle = state.manager.get(uuid).expect("lifecycle");
        assert_eq!(lifecycle.state, SandboxState::RecoveryRequired);
        assert_eq!(
            lifecycle
                .operation
                .as_ref()
                .and_then(|journal| journal.phase),
            Some(OperationPhase::CheckpointHeadUpdated)
        );
        assert!(state.manager.backend_owner(uuid).is_some());
        let checkpoints = state
            .manager
            .list_checkpoints(uuid)
            .await
            .expect("committed checkpoint");
        assert_eq!(checkpoints.len(), 1);
        assert!(checkpoints[0].is_head);

        state.manager.destroy(uuid).await.expect("destroy retry");
        assert_eq!(
            state.manager.get(uuid).expect("destroyed").state,
            SandboxState::Destroyed
        );
        assert_eq!(
            state
                .manager
                .list_checkpoints(uuid)
                .await
                .expect("removed checkpoint history")
                .len(),
            0
        );
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn cancelled_parent_validation_precedes_mutation_and_supervisor_converges() {
        let temp = tempfile::tempdir().expect("temp");
        let state = mock_state(&temp);
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id").to_string();
        let uuid = Uuid::parse_str(&id).expect("uuid");
        write_checkpoint_fixture(&state, &id).await;
        let existing_head = state
            .manager
            .checkpoint(uuid)
            .await
            .expect("seed checkpoint")
            .id;
        let before = state.manager.get(uuid).expect("running lifecycle");
        let persisted_before = state
            .state_store
            .load(uuid)
            .expect("persisted running lifecycle");
        let state_path = configured_state_dir(&state).join(&id).join("state.json");
        let state_bytes_before = std::fs::read(&state_path).expect("persisted state bytes");
        let checkpoint_root = configured_state_dir(&state).join("checkpoints").join(&id);

        let hook = crate::failpoint::TestFailpoint::new(&["checkpoint-before-read-head"]);
        let capture_state = state.clone();
        let capture_hook = hook.clone();
        let capture = tokio::spawn(async move {
            capture_hook
                .run(capture_state.manager.checkpoint(uuid))
                .await
        });
        tokio::time::timeout(Duration::from_secs(2), hook.wait_until_paused())
            .await
            .expect("parent validation pause");

        tokio::time::timeout(
            Duration::from_millis(250),
            tokio::time::sleep(Duration::from_millis(1)),
        )
        .await
        .expect("parent validation must not occupy the async runtime worker");
        assert!(state.manager.operation_lock(uuid).try_lock().is_err());
        assert_eq!(
            serde_json::to_value(state.manager.get(uuid).expect("unchanged lifecycle"))
                .expect("serialize current lifecycle"),
            serde_json::to_value(&before).expect("serialize prior lifecycle")
        );
        assert_eq!(
            serde_json::to_value(
                state
                    .state_store
                    .load(uuid)
                    .expect("unchanged persisted lifecycle")
            )
            .expect("serialize current persisted lifecycle"),
            serde_json::to_value(&persisted_before).expect("serialize prior persisted lifecycle")
        );
        assert_eq!(
            std::fs::read(&state_path).expect("state bytes during parent validation"),
            state_bytes_before
        );
        assert!(
            std::fs::read_dir(&checkpoint_root)
                .expect("checkpoint catalog")
                .filter_map(std::result::Result::ok)
                .all(|entry| !entry.file_name().to_string_lossy().starts_with(".ckpt-")),
            "parent validation must precede staging and checkpoint journaling"
        );

        capture.abort();
        assert!(
            capture
                .await
                .expect_err("outer checkpoint request must be cancelled")
                .is_cancelled()
        );
        assert!(state.manager.operation_lock(uuid).try_lock().is_err());

        hook.release();
        let operation = tokio::time::timeout(
            Duration::from_secs(2),
            state.manager.operation_lock(uuid).lock_owned(),
        )
        .await
        .expect("parent validation must finish and release the operation lock");
        drop(operation);

        let after = state
            .manager
            .get(uuid)
            .expect("running lifecycle after cancellation");
        assert_eq!(after.state, SandboxState::Running);
        assert!(after.operation.is_none());
        let completed_head = after
            .last_checkpoint
            .expect("detached supervisor checkpoint");
        assert_ne!(completed_head, existing_head);
        let checkpoints = state
            .manager
            .list_checkpoints(uuid)
            .await
            .expect("completed checkpoint catalog");
        assert_eq!(checkpoints.len(), 2);
        assert!(
            checkpoints
                .iter()
                .any(|checkpoint| checkpoint.id == existing_head && !checkpoint.is_head)
        );
        assert!(
            checkpoints
                .iter()
                .any(|checkpoint| checkpoint.id == completed_head && checkpoint.is_head)
        );
        assert!(state.manager.backend_owner(uuid).is_some());
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn published_checkpoint_holds_the_operation_lock_until_head_commit() {
        let temp = tempfile::tempdir().expect("temp");
        let state = mock_state(&temp);
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id");
        let uuid = Uuid::parse_str(id).expect("uuid");
        write_checkpoint_fixture(&state, id).await;
        let hook = crate::failpoint::TestFailpoint::new(&["checkpoint-after-publish-before-head"]);
        let capture_state = state.clone();
        let capture_hook = hook.clone();
        let capture = tokio::spawn(async move {
            capture_hook
                .run(capture_state.manager.checkpoint(uuid))
                .await
        });
        hook.wait_until_paused().await;

        let persisted = state
            .state_store
            .load(uuid)
            .expect("persisted checkpoint journal");
        assert_eq!(persisted.state, SandboxState::Paused);
        assert_eq!(
            persisted.operation.and_then(|journal| journal.phase),
            Some(OperationPhase::CheckpointPublished)
        );
        let list_state = state.clone();
        let mut list = tokio::spawn(async move { list_state.manager.list_checkpoints(uuid).await });
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut list)
                .await
                .is_err(),
            "checkpoint listing must wait for a consistent catalog boundary"
        );
        let destroy_state = state.clone();
        let mut destroy = tokio::spawn(async move { destroy_state.manager.destroy(uuid).await });
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut destroy)
                .await
                .is_err(),
            "destroy must wait for checkpoint ownership"
        );

        hook.release();
        capture
            .await
            .expect("capture task")
            .expect("checkpoint capture");
        let checkpoints = list.await.expect("list task").expect("checkpoint list");
        assert_eq!(checkpoints.len(), 1);
        assert!(checkpoints[0].is_head);
        assert!(destroy.await.expect("destroy task").expect("destroy"));
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn cancelled_storage_capture_retains_ownership_until_publication() {
        struct FailpointReleaseGuard<'a>(&'a crate::failpoint::TestFailpoint);

        impl Drop for FailpointReleaseGuard<'_> {
            fn drop(&mut self) {
                self.0.release();
            }
        }

        let temp = tempfile::tempdir().expect("temp");
        let state = mock_state(&temp);
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id").to_string();
        let uuid = Uuid::parse_str(&id).expect("uuid");
        let slot = write_checkpoint_fixture(&state, &id).await;
        let hook = crate::failpoint::TestFailpoint::new(&["storage-capture-before-publish"]);
        let release_guard = FailpointReleaseGuard(&hook);
        let capture_state = state.clone();
        let capture_hook = hook.clone();
        let capture = tokio::spawn(async move {
            capture_hook
                .run(capture_state.manager.checkpoint(uuid))
                .await
        });
        hook.wait_until_paused().await;

        let interrupted = state.manager.get(uuid).expect("checkpoint lifecycle");
        assert_eq!(interrupted.state, SandboxState::Paused);
        let checkpoint_id = interrupted
            .operation
            .as_ref()
            .and_then(|journal| journal.checkpoint_id.clone())
            .expect("checkpoint id");
        assert_eq!(
            interrupted.operation.and_then(|journal| journal.phase),
            Some(OperationPhase::CheckpointPaused)
        );
        assert!(state.manager.operation_lock(uuid).try_lock().is_err());

        let staging = configured_state_dir(&state)
            .join("checkpoints")
            .join(&id)
            .join(format!(".{checkpoint_id}.tmp"));
        let stage_entries = |subtree: &str| {
            let mut entries = std::fs::read_dir(staging.join(subtree))
                .expect("checkpoint staging directory")
                .map(|entry| {
                    entry
                        .expect("checkpoint staging entry")
                        .file_name()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect::<Vec<_>>();
            entries.sort();
            entries
        };
        let backend_before_cancel = stage_entries("backend");
        assert!(
            backend_before_cancel
                .iter()
                .any(|name| name == "vmstate.snap")
        );
        assert!(
            backend_before_cancel
                .iter()
                .any(|name| name == "memory.snap")
        );
        let storage_before_cancel = stage_entries("storage");
        assert!(
            storage_before_cancel
                .iter()
                .any(|name| name.starts_with(".rootfs.snap.capture-") && name.ends_with(".tmp"))
        );
        assert!(
            !storage_before_cancel
                .iter()
                .any(|name| name == "rootfs.snap")
        );
        assert!(slot.rootfs_path.exists());

        capture.abort();
        assert!(
            capture
                .await
                .expect_err("outer checkpoint request must be cancelled")
                .is_cancelled()
        );
        assert!(state.manager.operation_lock(uuid).try_lock().is_err());

        let list_state = state.clone();
        let mut list = tokio::spawn(async move { list_state.manager.list_checkpoints(uuid).await });
        let destroy_state = state.clone();
        let mut destroy = tokio::spawn(async move { destroy_state.manager.destroy(uuid).await });
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut list)
                .await
                .is_err(),
            "checkpoint listing must wait for blocking storage capture"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut destroy)
                .await
                .is_err(),
            "destroy must wait for blocking storage capture"
        );
        assert_eq!(stage_entries("backend"), backend_before_cancel);
        assert_eq!(stage_entries("storage"), storage_before_cancel);
        assert!(slot.rootfs_path.exists());

        hook.release();
        drop(release_guard);
        let checkpoints = tokio::time::timeout(Duration::from_secs(2), &mut list)
            .await
            .expect("detached supervisor must release checkpoint listing")
            .expect("checkpoint list task")
            .expect("checkpoint list");
        assert_eq!(checkpoints.len(), 1);
        assert_eq!(checkpoints[0].id, checkpoint_id);
        assert!(checkpoints[0].is_head);
        assert!(
            tokio::time::timeout(Duration::from_secs(2), &mut destroy)
                .await
                .expect("detached supervisor must release destroy")
                .expect("destroy task")
                .expect("destroy completed checkpoint")
        );
        let destroyed = state.manager.get(uuid).expect("destroyed lifecycle");
        assert_eq!(destroyed.state, SandboxState::Destroyed);
        assert!(destroyed.operation.is_none());
        assert!(!staging.exists());
        assert!(!slot.rootfs_path.exists());
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn cancelled_blocking_publish_finishes_before_unlocking() {
        let temp = tempfile::tempdir().expect("temp");
        let state = mock_state(&temp);
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id").to_string();
        let uuid = Uuid::parse_str(&id).expect("uuid");
        write_checkpoint_fixture(&state, &id).await;
        let hook =
            crate::failpoint::TestFailpoint::new(&["checkpoint-after-store-publish-before-state"]);
        let capture_state = state.clone();
        let capture_hook = hook.clone();
        let capture = tokio::spawn(async move {
            capture_hook
                .run(capture_state.manager.checkpoint(uuid))
                .await
        });
        hook.wait_until_paused().await;

        tokio::time::timeout(
            Duration::from_millis(250),
            tokio::time::sleep(Duration::from_millis(1)),
        )
        .await
        .expect("blocking publication must not occupy the async runtime worker");
        let persisted = state
            .state_store
            .load(uuid)
            .expect("persisted paused checkpoint journal");
        assert_eq!(persisted.state, SandboxState::Paused);
        assert_eq!(
            persisted
                .operation
                .as_ref()
                .and_then(|journal| journal.phase),
            Some(OperationPhase::CheckpointPaused)
        );
        let checkpoint_id = persisted
            .operation
            .as_ref()
            .and_then(|journal| journal.checkpoint_id.clone())
            .expect("checkpoint id");
        let checkpoint_root = configured_state_dir(&state).join("checkpoints").join(&id);
        assert!(checkpoint_root.join(&checkpoint_id).is_dir());
        assert!(!checkpoint_root.join("HEAD").exists());

        capture.abort();
        assert!(
            capture
                .await
                .expect_err("outer checkpoint request must be cancelled")
                .is_cancelled()
        );
        assert!(state.manager.operation_lock(uuid).try_lock().is_err());

        hook.release();
        let operation = tokio::time::timeout(
            Duration::from_secs(2),
            state.manager.operation_lock(uuid).lock_owned(),
        )
        .await
        .expect("publication must finish and release the operation lock");
        let completed = state
            .state_store
            .load(uuid)
            .expect("persisted completed checkpoint");
        assert_eq!(completed.state, SandboxState::Running);
        assert!(completed.operation.is_none());
        assert_eq!(
            completed.last_checkpoint.as_deref(),
            Some(checkpoint_id.as_str())
        );
        drop(operation);

        let checkpoints = state
            .manager
            .list_checkpoints(uuid)
            .await
            .expect("published checkpoint catalog");
        assert_eq!(checkpoints.len(), 1);
        assert_eq!(checkpoints[0].id, checkpoint_id);
        assert!(checkpoints[0].is_head);
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn cancelled_blocking_head_update_finishes_before_unlocking() {
        let temp = tempfile::tempdir().expect("temp");
        let state = mock_state(&temp);
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id").to_string();
        let uuid = Uuid::parse_str(&id).expect("uuid");
        write_checkpoint_fixture(&state, &id).await;
        let hook =
            crate::failpoint::TestFailpoint::new(&["checkpoint-after-store-head-before-state"]);
        let capture_state = state.clone();
        let capture_hook = hook.clone();
        let capture = tokio::spawn(async move {
            capture_hook
                .run(capture_state.manager.checkpoint(uuid))
                .await
        });
        hook.wait_until_paused().await;

        tokio::time::timeout(
            Duration::from_millis(250),
            tokio::time::sleep(Duration::from_millis(1)),
        )
        .await
        .expect("blocking HEAD update must not occupy the async runtime worker");
        let persisted = state
            .state_store
            .load(uuid)
            .expect("persisted published checkpoint journal");
        assert_eq!(persisted.state, SandboxState::Paused);
        assert_eq!(
            persisted
                .operation
                .as_ref()
                .and_then(|journal| journal.phase),
            Some(OperationPhase::CheckpointPublished)
        );
        let checkpoint_id = persisted
            .operation
            .as_ref()
            .and_then(|journal| journal.checkpoint_id.clone())
            .expect("checkpoint id");
        let head_path = configured_state_dir(&state)
            .join("checkpoints")
            .join(&id)
            .join("HEAD");
        assert_eq!(
            std::fs::read_to_string(&head_path)
                .expect("published checkpoint HEAD")
                .trim(),
            checkpoint_id
        );

        capture.abort();
        assert!(
            capture
                .await
                .expect_err("outer checkpoint request must be cancelled")
                .is_cancelled()
        );
        assert!(state.manager.operation_lock(uuid).try_lock().is_err());

        hook.release();
        let operation = tokio::time::timeout(
            Duration::from_secs(2),
            state.manager.operation_lock(uuid).lock_owned(),
        )
        .await
        .expect("HEAD update must finish and release the operation lock");
        let completed = state
            .state_store
            .load(uuid)
            .expect("persisted completed checkpoint");
        assert_eq!(completed.state, SandboxState::Running);
        assert!(completed.operation.is_none());
        assert_eq!(
            completed.last_checkpoint.as_deref(),
            Some(checkpoint_id.as_str())
        );
        drop(operation);

        let checkpoints = state
            .manager
            .list_checkpoints(uuid)
            .await
            .expect("checkpoint catalog with HEAD");
        assert_eq!(checkpoints.len(), 1);
        assert_eq!(checkpoints[0].id, checkpoint_id);
        assert!(checkpoints[0].is_head);
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn cancelled_blocking_list_holds_the_operation_lock_until_scan_completion() {
        let temp = tempfile::tempdir().expect("temp");
        let state = mock_state(&temp);
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id").to_string();
        let uuid = Uuid::parse_str(&id).expect("uuid");
        write_checkpoint_fixture(&state, &id).await;
        state
            .manager
            .checkpoint(uuid)
            .await
            .expect("seed checkpoint");

        let hook = crate::failpoint::TestFailpoint::new(&["checkpoint-before-store-list"]);
        let list_state = state.clone();
        let list_hook = hook.clone();
        let list = tokio::spawn(async move {
            list_hook
                .run(list_state.manager.list_checkpoints(uuid))
                .await
        });
        hook.wait_until_paused().await;
        list.abort();
        assert!(
            list.await
                .expect_err("outer checkpoint list request must be cancelled")
                .is_cancelled()
        );
        assert!(state.manager.operation_lock(uuid).try_lock().is_err());

        let destroy_state = state.clone();
        let mut destroy = tokio::spawn(async move { destroy_state.manager.destroy(uuid).await });
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut destroy)
                .await
                .is_err(),
            "destroy must wait for the detached catalog scan"
        );

        hook.release();
        destroy
            .await
            .expect("destroy task")
            .expect("destroy after checkpoint scan");
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test(flavor = "current_thread")]
    async fn checkpoint_cleanup_does_not_block_the_async_runtime_worker() {
        struct FailpointReleaseGuard<'a>(&'a crate::failpoint::TestFailpoint);

        impl Drop for FailpointReleaseGuard<'_> {
            fn drop(&mut self) {
                self.0.release();
            }
        }

        let temp = tempfile::tempdir().expect("temp");
        let state = mock_state(&temp);
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id").to_string();
        let uuid = Uuid::parse_str(&id).expect("uuid");
        write_checkpoint_fixture(&state, &id).await;
        state
            .manager
            .checkpoint(uuid)
            .await
            .expect("seed checkpoint");

        let hook = crate::failpoint::TestFailpoint::new(&["checkpoint-before-store-remove"]);
        let release_guard = FailpointReleaseGuard(&hook);
        let destroy_state = state.clone();
        let destroy_hook = hook.clone();
        let destroy =
            tokio::spawn(
                async move { destroy_hook.run(destroy_state.manager.destroy(uuid)).await },
            );
        hook.wait_until_paused().await;

        tokio::time::timeout(
            Duration::from_millis(250),
            tokio::time::sleep(Duration::from_millis(1)),
        )
        .await
        .expect("checkpoint cleanup must not occupy the async runtime worker");
        assert!(state.manager.operation_lock(uuid).try_lock().is_err());

        destroy.abort();
        assert!(
            destroy
                .await
                .expect_err("cancel the outer destroy request")
                .is_cancelled()
        );
        assert!(state.manager.operation_lock(uuid).try_lock().is_err());

        let list_state = state.clone();
        let mut list = tokio::spawn(async move { list_state.manager.list_checkpoints(uuid).await });
        let retry_state = state.clone();
        let mut retry = tokio::spawn(async move { retry_state.manager.destroy(uuid).await });
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut list)
                .await
                .is_err(),
            "checkpoint listing must wait for detached destruction"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut retry)
                .await
                .is_err(),
            "a destroy retry must wait for detached destruction"
        );

        hook.release();
        drop(release_guard);
        assert!(
            !retry
                .await
                .expect("retry task")
                .expect("retry after detached destruction")
        );
        assert!(
            list.await
                .expect("list task")
                .expect("list after detached destruction")
                .is_empty()
        );
        let destroyed = state.manager.get(uuid).expect("destroyed lifecycle");
        assert_eq!(destroyed.state, SandboxState::Destroyed);
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn cancelled_published_checkpoint_finishes_before_destroy() {
        let temp = tempfile::tempdir().expect("temp");
        let state = mock_state(&temp);
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id").to_string();
        let uuid = Uuid::parse_str(&id).expect("uuid");
        write_checkpoint_fixture(&state, &id).await;
        let hook = crate::failpoint::TestFailpoint::new(&["checkpoint-after-publish-before-head"]);
        let capture_state = state.clone();
        let capture_hook = hook.clone();
        let capture = tokio::spawn(async move {
            capture_hook
                .run(capture_state.manager.checkpoint(uuid))
                .await
        });
        hook.wait_until_paused().await;
        capture.abort();
        let cancelled = capture
            .await
            .expect_err("client checkpoint task must be cancelled");

        let interrupted = state.manager.get(uuid).expect("interrupted lifecycle");
        assert_eq!(interrupted.state, SandboxState::Paused);
        assert_eq!(
            interrupted.operation.and_then(|journal| journal.phase),
            Some(OperationPhase::CheckpointPublished)
        );
        assert!(
            !configured_state_dir(&state)
                .join("checkpoints")
                .join(&id)
                .join("HEAD")
                .exists()
        );

        let destroy_state = state.clone();
        let mut destroy = tokio::spawn(async move { destroy_state.manager.destroy(uuid).await });
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut destroy)
                .await
                .is_err(),
            "destroy must wait for the detached checkpoint supervisor"
        );

        hook.release();
        assert!(cancelled.is_cancelled());
        tokio::time::timeout(Duration::from_secs(2), &mut destroy)
            .await
            .expect("detached supervisor and queued destroy must converge")
            .expect("destroy task")
            .expect("destroy completed checkpoint");
        let checkpoints = state
            .manager
            .list_checkpoints(uuid)
            .await
            .expect("removed checkpoint history");
        assert!(checkpoints.is_empty());
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn cancelled_checkpoint_requests_finish_in_detached_supervisors() {
        for (failpoint, expected_state, expected_phase) in [
            (
                "checkpoint-after-begin",
                SandboxState::Running,
                OperationPhase::CheckpointPreparing,
            ),
            (
                "checkpoint-after-pause",
                SandboxState::Paused,
                OperationPhase::CheckpointPaused,
            ),
            (
                "checkpoint-after-head",
                SandboxState::Paused,
                OperationPhase::CheckpointHeadUpdated,
            ),
        ] {
            let temp = tempfile::tempdir().expect("temp");
            let state = mock_state(&temp);
            let created = created_json(&state, &test_request()).await;
            let id = created["instance"]["id"].as_str().expect("id").to_string();
            let uuid = Uuid::parse_str(&id).expect("uuid");
            write_checkpoint_fixture(&state, &id).await;
            let checkpoint_id = cancel_checkpoint_request_at(
                &state,
                uuid,
                failpoint,
                expected_state,
                expected_phase,
            )
            .await;

            let completed = state.manager.get(uuid).expect("completed lifecycle");
            assert_eq!(completed.state, SandboxState::Running);
            assert!(completed.operation.is_none());
            assert_eq!(
                completed.last_checkpoint.as_deref(),
                Some(checkpoint_id.as_str())
            );
            let checkpoints = state
                .manager
                .list_checkpoints(uuid)
                .await
                .expect("completed checkpoint history");
            assert_eq!(checkpoints.len(), 1);
            assert_eq!(checkpoints[0].id, checkpoint_id);
            assert!(checkpoints[0].is_head);
            state
                .manager
                .destroy(uuid)
                .await
                .expect("destroy after detached checkpoint completion");
            let destroyed = state.manager.get(uuid).expect("destroyed lifecycle");
            assert_eq!(destroyed.state, SandboxState::Destroyed);
            assert!(destroyed.operation.is_none());
        }
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn crashed_checkpoint_phases_are_reconciled_from_durable_state() {
        for (phase, expected_state) in [
            (OperationPhase::CheckpointPreparing, SandboxState::Running),
            (OperationPhase::CheckpointPaused, SandboxState::Paused),
            (OperationPhase::CheckpointPublished, SandboxState::Paused),
            (OperationPhase::CheckpointHeadUpdated, SandboxState::Paused),
        ] {
            let restart_temp = tempfile::tempdir().expect("restart temp");
            let config = test_config(&restart_temp);
            let restart_state = mock_state_from_config(config.clone());
            let created = created_json(&restart_state, &test_request()).await;
            let restart_id = created["instance"]["id"]
                .as_str()
                .expect("restart id")
                .to_string();
            let restart_uuid = Uuid::parse_str(&restart_id).expect("restart uuid");
            write_checkpoint_fixture(&restart_state, &restart_id).await;
            persist_crashed_checkpoint_phase(&restart_state, restart_uuid, phase).await;
            drop(restart_state);

            let restarted = mock_state_from_config(config);
            let interrupted = restarted
                .manager
                .get(restart_uuid)
                .expect("scanned interrupted lifecycle");
            assert_eq!(interrupted.state, expected_state);
            assert_eq!(
                interrupted.operation.and_then(|journal| journal.phase),
                Some(phase)
            );
            assert!(restarted.manager.backend_owner(restart_uuid).is_none());

            let report = restarted.manager.reconcile_startup().await;
            assert_eq!(report.attempted, 1);
            assert_eq!(report.completed, 1);
            assert!(report.failures.is_empty());
            let destroyed = restarted
                .manager
                .get(restart_uuid)
                .expect("reconciled lifecycle");
            assert_eq!(destroyed.state, SandboxState::Destroyed);
            assert!(destroyed.operation.is_none());
            let checkpoints = restarted
                .manager
                .list_checkpoints(restart_uuid)
                .await
                .expect("reconciled checkpoint history");
            assert!(checkpoints.is_empty());
            let checkpoint_dir = configured_state_dir(&restarted)
                .join("checkpoints")
                .join(&restart_id);
            assert!(!checkpoint_dir.exists());
        }
    }
    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn guest_operations_wait_for_checkpoint_publication() {
        let temp = tempfile::tempdir().expect("temp");
        let state = guest_mock_state(&temp);
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id");
        let uuid = Uuid::parse_str(id).expect("uuid");
        write_checkpoint_fixture(&state, id).await;
        state
            .manager
            .write_file(uuid, "/tmp/existing".into(), b"before")
            .await
            .expect("seed guest file");
        let hook = crate::failpoint::TestFailpoint::new(&["checkpoint-after-publish-before-head"]);
        let capture_state = state.clone();
        let capture_hook = hook.clone();
        let capture = tokio::spawn(async move {
            capture_hook
                .run(capture_state.manager.checkpoint(uuid))
                .await
        });
        hook.wait_until_paused().await;

        let exec_state = state.clone();
        let mut exec = tokio::spawn(async move {
            exec_state
                .manager
                .exec(uuid, "printf locked".into(), None, None, 5)
                .await
        });
        let read_state = state.clone();
        let mut read = tokio::spawn(async move {
            read_state
                .manager
                .read_file(uuid, "/tmp/existing".into())
                .await
        });
        let write_state = state.clone();
        let mut write = tokio::spawn(async move {
            write_state
                .manager
                .write_file(uuid, "/tmp/after".into(), b"after")
                .await
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut exec)
                .await
                .is_err(),
            "guest exec must wait for checkpoint ownership"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut read)
                .await
                .is_err(),
            "guest read must wait for checkpoint ownership"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut write)
                .await
                .is_err(),
            "guest write must wait for checkpoint ownership"
        );

        hook.release();
        capture
            .await
            .expect("capture task")
            .expect("checkpoint capture");
        assert_eq!(
            exec.await.expect("exec task").expect("guest exec").stdout,
            b"printf locked"
        );
        assert_eq!(
            read.await.expect("read task").expect("guest read"),
            b"before"
        );
        write.await.expect("write task").expect("guest write");
    }

    #[tokio::test]
    async fn checkpoint_rejects_an_unfinished_lifecycle_journal() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let state = build_test_state(
            config,
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, Arc::new(MockSpawner)),
            BackendKind::Mock,
            storage,
        );
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id");
        let uuid = Uuid::parse_str(id).expect("uuid");
        let journal = {
            let mut instances = state.instances.lock().expect("instances");
            let instance = instances.get_mut(&uuid).expect("instance");
            instance.begin_operation(OperationKind::Create);
            state
                .state_store
                .persist(instance)
                .expect("persist journal");
            instance.operation.clone().expect("journal")
        };

        let error = checkpoint(&state, id)
            .await
            .expect_err("unfinished lifecycle work must fail closed");

        assert!(matches!(error, BlazeDaemonError::RecoveryRequired(_)));
        assert_eq!(
            state.instances.lock().expect("instances")[&uuid].operation,
            Some(journal)
        );
        assert_eq!(
            state
                .state_store
                .load(uuid)
                .expect("persisted instance")
                .operation,
            state.instances.lock().expect("instances")[&uuid].operation
        );
    }

    #[tokio::test]
    async fn checkpoint_rejects_a_non_running_lifecycle_state() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let state = build_test_state(
            config,
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, Arc::new(MockSpawner)),
            BackendKind::Mock,
            storage,
        );
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id");
        let uuid = Uuid::parse_str(id).expect("uuid");
        state.manager.destroy(uuid).await.expect("destroy");

        let error = checkpoint(&state, id)
            .await
            .expect_err("checkpoint must require a running instance");

        assert!(matches!(error, BlazeDaemonError::Conflict(_)));
        assert_eq!(error.status_code(), 409);
    }

    #[tokio::test]
    async fn sandbox_guest_routes_use_owned_runtime() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let state = build_test_state(
            config,
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, Arc::new(GuestMockSpawner)),
            BackendKind::Mock,
            storage,
        );
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("instance id");

        let (status, exec) = dispatched_json(
            &state,
            Method::POST,
            &format!("/v1/sandboxes/{id}/exec"),
            serde_json::to_vec(&json!({
                "cmd": "printf routed",
                "timeout": 5,
            }))
            .expect("exec request"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(exec["exit_code"], 0);
        assert_eq!(exec["stdout_b64"], BASE64.encode(b"printf routed"));

        let encoded = "AAEC/2d1ZXN0";
        let (status, written) = dispatched_json(
            &state,
            Method::POST,
            &format!("/v1/sandboxes/{id}/write"),
            serde_json::to_vec(&json!({
                "path": "/tmp/value",
                "data_b64": encoded,
            }))
            .expect("write request"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(written["bytes"], 9);

        let (status, read) = dispatched_json(
            &state,
            Method::POST,
            &format!("/v1/sandboxes/{id}/read"),
            serde_json::to_vec(&json!({"path": "/tmp/value"})).expect("read request"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(read["data_b64"], encoded);

        let invalid_timeout = dispatch(
            &Method::POST,
            &format!("/v1/sandboxes/{id}/exec"),
            "",
            serde_json::to_vec(&json!({
                "cmd": "true",
                "timeout": MAX_EXEC_TIMEOUT_SECS + 1,
            }))
            .expect("invalid request"),
            &state,
        )
        .await
        .expect_err("timeout above the API limit must fail");
        assert!(matches!(invalid_timeout, BlazeDaemonError::BadRequest(_)));

        assert_eq!(
            decode_guest_file(&BASE64.encode(b"1234"), 4).expect("boundary"),
            b"1234"
        );
        assert!(matches!(
            decode_guest_file(&BASE64.encode(b"12345"), 4),
            Err(BlazeDaemonError::Guest(
                crate::guest::GuestError::PayloadTooLarge { .. }
            ))
        ));
        assert!(matches!(
            decode_guest_file("not/base64!", 16),
            Err(BlazeDaemonError::BadRequest(_))
        ));

        let (status, destroyed) = dispatched_json(
            &state,
            Method::DELETE,
            &format!("/v1/sandboxes/{id}"),
            Vec::new(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(destroyed["destroyed"], true);
    }

    #[tokio::test]
    async fn production_mock_rejects_guest_operations() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let state = build_test_state(
            config,
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, Arc::new(MockSpawner)),
            BackendKind::Mock,
            storage,
        );
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("instance id");

        let (status, error) = handled_json(
            &state,
            Method::POST,
            &format!("/v1/sandboxes/{id}/exec"),
            serde_json::to_vec(&json!({"cmd": "true"})).expect("exec request"),
        )
        .await;

        assert_eq!(status, StatusCode::CONFLICT);
        assert!(
            error["error"]
                .as_str()
                .expect("error message")
                .contains("no guest transport")
        );
    }

    #[tokio::test]
    async fn guest_write_respects_http_and_decoded_limits() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let state = build_test_state(
            config,
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, Arc::new(GuestMockSpawner)),
            BackendKind::Mock,
            storage,
        );
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("instance id");
        let uuid = Uuid::parse_str(id).expect("uuid");
        let path = format!("/v1/sandboxes/{id}/write");

        let envelope_payload = vec![b'y'; 17 * 1024 * 1024];
        let envelope_body = serde_json::to_vec(&json!({
            "path": "/tmp/http-envelope",
            "data_b64": BASE64.encode(&envelope_payload),
        }))
        .expect("write request above the guest HTTP limit");
        assert!(envelope_body.len() > MAX_GUEST_HTTP_BODY_BYTES);
        let (status, error) = handled_json(&state, Method::POST, &path, envelope_body).await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(error["status"], 413);

        let mut payload = vec![b'z'; MAX_GUEST_FILE_BYTES];
        let body = serde_json::to_vec(&json!({
            "path": "/tmp/max-size",
            "data_b64": BASE64.encode(&payload),
        }))
        .expect("write request");
        assert!(body.len() <= MAX_GUEST_HTTP_BODY_BYTES);

        let (status, written) = handled_json(&state, Method::POST, &path, body).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(written["bytes"], MAX_GUEST_FILE_BYTES);
        let readback = state
            .manager
            .read_file(uuid, "/tmp/max-size".into())
            .await
            .expect("read maximum file");
        assert_eq!(readback, payload);
        drop(readback);

        payload.push(b'z');
        let oversized = serde_json::to_vec(&json!({
            "path": "/tmp/too-large",
            "data_b64": BASE64.encode(&payload),
        }))
        .expect("oversized write request");
        assert!(oversized.len() <= MAX_GUEST_HTTP_BODY_BYTES);
        let (status, error) = handled_json(&state, Method::POST, &path, oversized).await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(error["status"], 413);
    }

    #[tokio::test]
    async fn write_route_reports_unknown_after_delivery_failure() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let state = build_test_state(
            config,
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, Arc::new(MockSpawner)),
            BackendKind::Mock,
            storage,
        );
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("instance id");
        let uuid = Uuid::parse_str(id).expect("uuid");
        state
            .manager
            .backend_owner(uuid)
            .expect("mock owner")
            .kill()
            .await
            .expect("stop mock guest");

        let socket = temp.path().join("uncertain.uds");
        let listener = tokio::net::UnixListener::bind(&socket).expect("bind guest endpoint");
        state
            .manager
            .insert_backend_owner(
                uuid,
                Arc::new(StalledGuestOwner {
                    instance_id: uuid,
                    socket,
                    kill_count: Arc::new(AtomicUsize::new(0)),
                    killed: AtomicBool::new(false),
                }),
            )
            .expect("replace backend owner");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept guest request");
            let mut reader = tokio::io::BufReader::new(stream);
            let mut connect = String::new();
            reader.read_line(&mut connect).await.expect("read connect");
            assert_eq!(connect, "CONNECT 5000\n");
            reader
                .get_mut()
                .write_all(b"OK 5000\n")
                .await
                .expect("write handshake");
            let mut request = String::new();
            reader
                .read_line(&mut request)
                .await
                .expect("read guest request");
            let request: serde_json::Value =
                serde_json::from_str(&request).expect("parse guest request");
            assert_eq!(request["op"], "write");
        });

        let body = serde_json::to_vec(&json!({
            "path": "/tmp/value",
            "data_b64": BASE64.encode(b"value"),
        }))
        .expect("write request");
        let (status, error) = handled_json(
            &state,
            Method::POST,
            &format!("/v1/sandboxes/{id}/write"),
            body,
        )
        .await;
        assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(error["code"], "guest_outcome_unknown");
        server.await.expect("guest server");
    }

    #[tokio::test]
    async fn unknown_guest_outcome_has_stable_api_code() {
        let response = error_response(&BlazeDaemonError::Guest(
            crate::guest::GuestError::OutcomeUnknown("response lost".into()),
        ));
        assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
        let body = response
            .into_body()
            .collect()
            .await
            .expect("response body")
            .to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&body).expect("error json");
        assert_eq!(value["code"], "guest_outcome_unknown");
        assert_eq!(value["status"], 504);

        let response = error_response(&BlazeDaemonError::Guest(
            crate::guest::GuestError::ResponseTooLarge {
                actual: 5,
                limit: 4,
            },
        ));
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let body = response
            .into_body()
            .collect()
            .await
            .expect("response body")
            .to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&body).expect("error json");
        assert_eq!(value["code"], "guest_response_too_large");

        let response = error_response(&BlazeDaemonError::Guest(crate::guest::GuestError::Timeout(
            "connect stalled".into(),
        )));
        let body = response
            .into_body()
            .collect()
            .await
            .expect("response body")
            .to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&body).expect("error json");
        assert_eq!(value["code"], "guest_timeout");
    }

    #[tokio::test]
    async fn create_publishes_ownership_before_provider_acquire() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let observed = Arc::new(AtomicBool::new(false));
        let storage: Arc<dyn StorageProvider> = Arc::new(OwnershipObservingStorage {
            inner: FileStorageProvider::with_images(
                config.storage.images_dir.clone(),
                config.storage.instances_dir.clone(),
            ),
            state_dir: config.daemon.state_dir.clone(),
            observed: observed.clone(),
        });
        let state = build_test_state(
            config,
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, Arc::new(MockSpawner)),
            BackendKind::Mock,
            storage,
        );

        created_json(&state, &test_request()).await;
        assert!(observed.load(Ordering::Acquire));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn restart_reconciles_durable_starting_before_spawn() {
        let temp = tempfile::tempdir().expect("temp");
        let mut config = test_config(&temp);
        config.storage.rootfs_size = 64;
        config.storage.mem_size = 32;
        config
            .backends
            .insert(BackendKind::Bubblewrap.as_str().into(), "/bin/true".into());
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let reached = Arc::new(Notify::new());
        let state = build_test_state(
            config.clone(),
            test_policy(BackendKind::Bubblewrap),
            spawners(
                BackendKind::Bubblewrap,
                Arc::new(PreSpawnBoundarySpawner {
                    reached: reached.clone(),
                }),
            ),
            BackendKind::Bubblewrap,
            storage,
        );
        let create_state = state.clone();
        let create =
            tokio::spawn(async move { create_sandbox(&create_state, &test_request()).await });
        tokio::time::timeout(std::time::Duration::from_secs(2), reached.notified())
            .await
            .expect("create reached the pre-spawn boundary");

        let instance = state
            .manager
            .list()
            .expect("instances")
            .into_iter()
            .next()
            .expect("durable create state");
        let persisted = SandboxInstance::load(&config.daemon.state_dir, instance.id)
            .expect("load durable Starting state");
        assert_eq!(persisted.state, SandboxState::Creating);
        assert_eq!(persisted.backend_ownership, BackendOwnership::Starting);
        let pid_file = config
            .daemon
            .state_dir
            .join(instance.id.to_string())
            .join("backend.pid");
        assert_eq!(std::fs::read(&pid_file).expect("prepared PID handoff"), b"");
        assert!(
            config
                .storage
                .instances_dir
                .join(instance.id.to_string())
                .is_dir()
        );

        create.abort();
        assert!(
            create
                .await
                .expect_err("simulated daemon exit cancels create")
                .is_cancelled()
        );
        drop(state);

        let recovered_storage: Arc<dyn StorageProvider> =
            Arc::new(FileStorageProvider::with_images(
                config.storage.images_dir.clone(),
                config.storage.instances_dir.clone(),
            ));
        let recovered = build_test_state(
            config.clone(),
            test_policy(BackendKind::Bubblewrap),
            spawners(BackendKind::Bubblewrap, Arc::new(BubblewrapSpawner)),
            BackendKind::Bubblewrap,
            recovered_storage,
        );

        let report = recovered.manager.reconcile_startup().await;

        assert_eq!(report.attempted, 1);
        assert_eq!(report.completed, 1);
        assert!(report.failures.is_empty());
        assert_eq!(
            recovered
                .manager
                .get(instance.id)
                .expect("reconciled state")
                .state,
            SandboxState::Destroyed
        );
        assert!(
            !config
                .storage
                .instances_dir
                .join(instance.id.to_string())
                .exists()
        );
        assert!(
            config
                .daemon
                .state_dir
                .join(instance.id.to_string())
                .join("backend.stopped")
                .is_file()
        );
        assert!(!pid_file.exists());
        assert!(matches!(
            recovered.state_store.run_dir(instance.id),
            Err(BlazeDaemonError::NotFound(_))
        ));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn restart_retains_locked_handoff_until_retry() {
        use std::os::fd::AsRawFd;

        let temp = tempfile::tempdir().expect("temp");
        let mut config = test_config(&temp);
        config.storage.rootfs_size = 64;
        config.storage.mem_size = 32;
        let storage = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let mut instance = SandboxInstance::new(
            BackendKind::Bubblewrap,
            WorkloadClass::AgentTool,
            "sha256:locked-handoff".into(),
            "pid-handoff-test".into(),
        );
        instance
            .transition(SandboxState::Creating)
            .expect("creating");
        instance.begin_operation(OperationKind::Create);
        let run_dir = config.daemon.state_dir.join(instance.id.to_string());
        let run_dir_owner = OwnedRunDir::for_test(instance.id, run_dir.clone());
        BubblewrapSpawner
            .prepare_spawn(&run_dir_owner)
            .await
            .expect("prepare PID handoff");
        drop(run_dir_owner);
        instance.backend_ownership = BackendOwnership::Starting;
        instance
            .persist(&config.daemon.state_dir)
            .expect("persist Starting state");
        storage
            .acquire(&AcquireOpts {
                instance_id: instance.id.to_string(),
                rootfs_size: config.storage.rootfs_size,
                mem_size: config.storage.mem_size,
            })
            .await
            .expect("storage");
        let pid_file = run_dir.join("backend.pid");
        let handoff = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&pid_file)
            .expect("open PID handoff");
        assert_eq!(
            unsafe { libc::flock(handoff.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0,
            "lock PID handoff"
        );
        let state = build_test_state(
            config.clone(),
            test_policy(BackendKind::Bubblewrap),
            spawners(BackendKind::Bubblewrap, Arc::new(BubblewrapSpawner)),
            BackendKind::Bubblewrap,
            storage,
        );

        let first = state.manager.reconcile_startup().await;

        assert_eq!(first.attempted, 1);
        assert_eq!(first.completed, 0);
        assert_eq!(first.failures.len(), 1);
        assert!(first.failures[0].error.contains("still in progress"));
        assert_eq!(
            state
                .manager
                .get(instance.id)
                .expect("retained state")
                .state,
            SandboxState::RecoveryRequired
        );
        assert!(
            config
                .storage
                .instances_dir
                .join(instance.id.to_string())
                .is_dir()
        );
        assert!(!run_dir.join("backend.stopped").exists());
        assert!(state.state_store.run_dir(instance.id).is_ok());

        drop(handoff);
        let retry = state.manager.reconcile_startup().await;

        assert_eq!(retry.attempted, 1);
        assert_eq!(retry.completed, 1);
        assert!(retry.failures.is_empty());
        assert_eq!(
            state
                .manager
                .get(instance.id)
                .expect("destroyed state")
                .state,
            SandboxState::Destroyed
        );
        assert!(
            !config
                .storage
                .instances_dir
                .join(instance.id.to_string())
                .exists()
        );
        assert!(run_dir.join("backend.stopped").is_file());
        assert!(!pid_file.exists());
        assert!(matches!(
            state.state_store.run_dir(instance.id),
            Err(BlazeDaemonError::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn partial_spawn_failure_retains_owner_and_storage_for_destroy() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let instances_dir = config.storage.instances_dir.clone();
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            instances_dir.clone(),
        ));
        let state = build_test_state(
            config,
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, Arc::new(PartialSpawnSpawner)),
            BackendKind::Mock,
            storage,
        );

        let error = create_sandbox(&state, &test_request())
            .await
            .expect_err("partial spawn must require recovery");
        assert!(matches!(error, BlazeDaemonError::RecoveryRequired(_)));
        let instance = state
            .instances
            .lock()
            .expect("instances")
            .values()
            .next()
            .cloned()
            .expect("retained lifecycle");
        assert_eq!(instance.state, SandboxState::RecoveryRequired);
        assert_eq!(instance.backend_ownership, BackendOwnership::Running);
        assert_eq!(
            instance.operation.as_ref().map(|operation| operation.kind),
            Some(OperationKind::Create)
        );
        assert!(instances_dir.join(instance.id.to_string()).is_dir());
        assert!(state.manager.backend_owner(instance.id).is_some());
        assert!(state.state_store.run_dir(instance.id).is_ok());

        destroy_sandbox(&state, &instance.id.to_string())
            .await
            .expect("retry destroy");
        assert!(!instances_dir.join(instance.id.to_string()).exists());
        assert!(state.manager.backend_owner(instance.id).is_none());
        assert!(matches!(
            state.state_store.run_dir(instance.id),
            Err(BlazeDaemonError::NotFound(_))
        ));
        assert_eq!(
            state.instances.lock().expect("instances")[&instance.id].state,
            SandboxState::Destroyed
        );
    }

    #[tokio::test]
    async fn restart_destroy_uses_the_persisted_backend_spawner() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let storage = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let mut instance = SandboxInstance::new(
            BackendKind::Bubblewrap,
            WorkloadClass::AgentTool,
            "sha256:recovery".into(),
            "recovery-test".into(),
        );
        instance
            .transition(SandboxState::Creating)
            .expect("creating");
        instance.transition(SandboxState::Running).expect("running");
        instance.backend_ownership = BackendOwnership::Running;
        instance.persist(&config.daemon.state_dir).expect("persist");
        storage
            .acquire(&AcquireOpts {
                instance_id: instance.id.to_string(),
                rootfs_size: 4096,
                mem_size: 4096,
            })
            .await
            .expect("storage");

        let active_cleanups = Arc::new(AtomicUsize::new(0));
        let persisted_cleanups = Arc::new(AtomicUsize::new(0));
        let mut registry = SpawnerRegistry::new();
        registry.insert(
            BackendKind::Mock,
            Arc::new(RecordingSpawner {
                cleanup_count: active_cleanups.clone(),
            }),
        );
        registry.insert(
            BackendKind::Bubblewrap,
            Arc::new(RecordingSpawner {
                cleanup_count: persisted_cleanups.clone(),
            }),
        );
        let state = build_test_state(
            config,
            test_policy(BackendKind::Mock),
            registry,
            BackendKind::Mock,
            storage,
        );

        destroy_sandbox(&state, &instance.id.to_string())
            .await
            .expect("destroy recovered instance");
        assert_eq!(persisted_cleanups.load(Ordering::Acquire), 1);
        assert_eq!(active_cleanups.load(Ordering::Acquire), 0);
    }

    #[tokio::test]
    async fn mock_fallback_restart_destroy_uses_mock_spawner() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let instances_dir = config.storage.instances_dir.clone();
        let initial_storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            instances_dir.clone(),
        ));
        let initial_state = build_test_state(
            config.clone(),
            test_policy(BackendKind::Firecracker),
            spawners(BackendKind::Mock, Arc::new(MockSpawner)),
            BackendKind::Mock,
            initial_storage,
        );
        let created = created_json(&initial_state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id").to_string();
        assert_eq!(created["instance"]["backend"], "mock");
        drop(initial_state);

        let mock_cleanups = Arc::new(AtomicUsize::new(0));
        let policy_cleanups = Arc::new(AtomicUsize::new(0));
        let mut registry = SpawnerRegistry::new();
        registry.insert(
            BackendKind::Mock,
            Arc::new(RecordingSpawner {
                cleanup_count: mock_cleanups.clone(),
            }),
        );
        registry.insert(
            BackendKind::Firecracker,
            Arc::new(RecordingSpawner {
                cleanup_count: policy_cleanups.clone(),
            }),
        );
        let restarted_storage: Arc<dyn StorageProvider> =
            Arc::new(FileStorageProvider::with_images(
                config.storage.images_dir.clone(),
                instances_dir.clone(),
            ));
        let restarted = build_test_state(
            config,
            test_policy(BackendKind::Firecracker),
            registry,
            BackendKind::Mock,
            restarted_storage,
        );

        destroy_sandbox(&restarted, &id)
            .await
            .expect("destroy recovered mock instance");
        assert_eq!(mock_cleanups.load(Ordering::Acquire), 1);
        assert_eq!(policy_cleanups.load(Ordering::Acquire), 0);
        assert!(!instances_dir.join(id).exists());
    }

    #[tokio::test]
    async fn write_ahead_create_without_slot_is_destroyable_after_restart() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let instances_dir = config.storage.instances_dir.clone();
        let mut instance = SandboxInstance::new(
            BackendKind::Mock,
            WorkloadClass::AgentTool,
            "sha256:write-ahead".into(),
            "write-ahead-test".into(),
        );
        instance
            .transition(SandboxState::Creating)
            .expect("creating");
        instance
            .persist(&config.daemon.state_dir)
            .expect("write-ahead state");
        let id = instance.id;
        assert!(!instances_dir.join(id.to_string()).exists());

        let cleanup_count = Arc::new(AtomicUsize::new(0));
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            instances_dir.clone(),
        ));
        let restarted = build_test_state(
            config,
            test_policy(BackendKind::Mock),
            spawners(
                BackendKind::Mock,
                Arc::new(RecordingSpawner {
                    cleanup_count: cleanup_count.clone(),
                }),
            ),
            BackendKind::Mock,
            storage,
        );

        destroy_sandbox(&restarted, &id.to_string())
            .await
            .expect("destroy state without slot");
        assert_eq!(cleanup_count.load(Ordering::Acquire), 0);
        assert_eq!(
            restarted.instances.lock().expect("instances")[&id].state,
            SandboxState::Destroyed
        );
        assert!(!instances_dir.join(id.to_string()).exists());
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn guest_readiness_failure_compensates_owned_resources() {
        let request = test_request();
        let temp = tempfile::tempdir().expect("temp");
        let state = guest_mock_state(&temp);
        let hook = crate::failpoint::TestFailpoint::new(&["create-guest-ready"]);

        hook.run(create_sandbox(&state, &request))
            .await
            .expect_err("guest readiness failure");

        let instance = state
            .instances
            .lock()
            .expect("instances")
            .values()
            .next()
            .cloned()
            .expect("destroyed create");
        assert_eq!(instance.state, SandboxState::Destroyed);
        assert!(state.manager.backend_owner(instance.id).is_none());
        assert!(
            !temp
                .path()
                .join("instances")
                .join(instance.id.to_string())
                .exists()
        );
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn failure_hooks_drive_create_and_destroy_compensation() {
        let request = test_request();

        let spawn_temp = tempfile::tempdir().expect("temp");
        let spawn_state = mock_state(&spawn_temp);
        let spawn_hook = crate::failpoint::TestFailpoint::new(&["create-spawn"]);
        spawn_hook
            .run(create_sandbox(&spawn_state, &request))
            .await
            .expect_err("spawn failure");
        let spawn_instance = spawn_state
            .instances
            .lock()
            .expect("instances")
            .values()
            .next()
            .cloned()
            .expect("destroyed create");
        assert_eq!(spawn_instance.state, SandboxState::Destroyed);

        let commit_temp = tempfile::tempdir().expect("temp");
        let commit_state = mock_state(&commit_temp);
        let commit_hook = crate::failpoint::TestFailpoint::new(&["create-state-commit"]);
        commit_hook
            .run(create_sandbox(&commit_state, &request))
            .await
            .expect_err("state commit failure");
        let commit_instance = commit_state
            .instances
            .lock()
            .expect("instances")
            .values()
            .next()
            .cloned()
            .expect("destroyed create");
        assert_eq!(commit_instance.state, SandboxState::Destroyed);
        assert!(
            commit_state
                .manager
                .backend_owner(commit_instance.id)
                .is_none()
        );

        let destroy_temp = tempfile::tempdir().expect("temp");
        let destroy_state = mock_state(&destroy_temp);
        let created = created_json(&destroy_state, &request).await;
        let id = created["instance"]["id"].as_str().expect("id").to_string();
        let kill_hook = crate::failpoint::TestFailpoint::new(&["destroy-kill"]);
        kill_hook
            .run(destroy_sandbox(&destroy_state, &id))
            .await
            .expect_err("kill boundary");
        let uuid = Uuid::parse_str(&id).expect("uuid");
        let failed_destroy = destroy_state.instances.lock().expect("instances")[&uuid].clone();
        assert_eq!(failed_destroy.state, SandboxState::RecoveryRequired);
        assert_eq!(
            failed_destroy
                .operation
                .as_ref()
                .map(|operation| operation.kind),
            Some(OperationKind::Destroy)
        );
        assert!(destroy_state.manager.backend_owner(uuid).is_some());
        destroy_sandbox(&destroy_state, &id)
            .await
            .expect("destroy retry");

        let release_temp = tempfile::tempdir().expect("temp");
        let release_state = mock_state(&release_temp);
        let created = created_json(&release_state, &request).await;
        let id = created["instance"]["id"].as_str().expect("id").to_string();
        let release_hook = crate::failpoint::TestFailpoint::new(&["storage-release"]);
        release_hook
            .run(destroy_sandbox(&release_state, &id))
            .await
            .expect_err("release boundary");
        let uuid = Uuid::parse_str(&id).expect("uuid");
        assert_eq!(
            release_state.instances.lock().expect("instances")[&uuid].backend_ownership,
            BackendOwnership::Stopped
        );
        assert_eq!(
            release_state.instances.lock().expect("instances")[&uuid]
                .operation
                .as_ref()
                .map(|operation| operation.kind),
            Some(OperationKind::Destroy)
        );
        destroy_sandbox(&release_state, &id)
            .await
            .expect("release retry");
    }

    #[cfg(feature = "test-failpoints")]
    async fn assert_create_rollback_commit_failure_is_retryable(
        failpoints: &'static [&'static str],
    ) {
        let temp = tempfile::tempdir().expect("temp");
        let state = mock_state(&temp);
        let hook = crate::failpoint::TestFailpoint::new(failpoints);

        let error = hook
            .run(create_sandbox(&state, &test_request()))
            .await
            .expect_err("rollback terminal commit failure");

        assert!(matches!(error, BlazeDaemonError::RecoveryRequired(_)));
        let instance = state
            .instances
            .lock()
            .expect("instances")
            .values()
            .next()
            .cloned()
            .expect("recovery record");
        assert_eq!(instance.state, SandboxState::RecoveryRequired);
        assert_eq!(instance.backend_ownership, BackendOwnership::Stopped);
        assert_eq!(
            instance.operation.as_ref().map(|operation| operation.kind),
            Some(OperationKind::Create)
        );
        assert_eq!(
            state
                .state_store
                .load(instance.id)
                .expect("persisted recovery record")
                .state,
            SandboxState::RecoveryRequired
        );
        assert!(state.state_store.run_dir(instance.id).is_ok());
        assert!(
            !temp
                .path()
                .join("instances")
                .join(instance.id.to_string())
                .exists()
        );

        destroy_sandbox(&state, &instance.id.to_string())
            .await
            .expect("destroy retry");

        assert_eq!(
            state.instances.lock().expect("instances")[&instance.id].state,
            SandboxState::Destroyed
        );
        assert!(matches!(
            state.state_store.run_dir(instance.id),
            Err(BlazeDaemonError::NotFound(_))
        ));
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn initial_publication_failure_before_publish_touches_no_resources() {
        let temp = tempfile::tempdir().expect("temp");
        let state = mock_state(&temp);
        let hook = crate::failpoint::TestFailpoint::new(&["state-before-first-publication"]);

        hook.run(create_sandbox(&state, &test_request()))
            .await
            .expect_err("pre-publication failure");

        assert!(state.instances.lock().expect("instances").is_empty());
        assert_eq!(state.state_store.retained_run_dir_count(), 0);
        assert!(
            std::fs::read_dir(temp.path().join("state"))
                .expect("state directory")
                .next()
                .is_none()
        );
        assert!(
            std::fs::read_dir(temp.path().join("instances"))
                .expect("instance directory")
                .next()
                .is_none()
        );

        let created = created_json(&state, &test_request()).await;
        assert_eq!(created["instance"]["state"], "running");
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn initial_publication_sync_failure_is_rolled_back_terminally() {
        let temp = tempfile::tempdir().expect("temp");
        let state = mock_state(&temp);
        let hook = crate::failpoint::TestFailpoint::new(&["state-first-publication-root-sync"]);

        hook.run(create_sandbox(&state, &test_request()))
            .await
            .expect_err("initial state publication sync failure");

        let instance = state
            .instances
            .lock()
            .expect("instances")
            .values()
            .next()
            .cloned()
            .expect("terminal rollback record");
        assert_eq!(instance.state, SandboxState::Destroyed);
        assert_eq!(instance.backend_ownership, BackendOwnership::Stopped);
        assert!(instance.operation.is_none());
        assert_eq!(
            state
                .state_store
                .load(instance.id)
                .expect("persisted terminal record")
                .state,
            SandboxState::Destroyed
        );
        assert!(matches!(
            state.state_store.run_dir(instance.id),
            Err(BlazeDaemonError::NotFound(_))
        ));
        assert!(
            !temp
                .path()
                .join("instances")
                .join(instance.id.to_string())
                .exists()
        );
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn unconfirmed_initial_publication_is_retained_for_recovery() {
        let temp = tempfile::tempdir().expect("temp");
        let state = mock_state(&temp);
        let hook = crate::failpoint::TestFailpoint::new(&["state-post-publication-identity"]);

        let error = hook
            .run(create_sandbox(&state, &test_request()))
            .await
            .expect_err("unconfirmed publication");

        assert!(matches!(error, BlazeDaemonError::RecoveryRequired(_)));
        let instance = state
            .instances
            .lock()
            .expect("instances")
            .values()
            .next()
            .cloned()
            .expect("recovery record");
        assert_eq!(instance.state, SandboxState::RecoveryRequired);
        assert_eq!(instance.backend_ownership, BackendOwnership::Stopped);
        assert_eq!(
            instance.operation.as_ref().map(|operation| operation.kind),
            Some(OperationKind::Create)
        );
        assert!(
            state
                .state_store
                .has_run_dir_residual(instance.id)
                .expect("publication residual")
        );
        assert!(matches!(
            state.state_store.run_dir(instance.id),
            Err(BlazeDaemonError::RecoveryRequired(_))
        ));
        assert!(
            !temp
                .path()
                .join("instances")
                .join(instance.id.to_string())
                .exists()
        );

        destroy_sandbox(&state, &instance.id.to_string())
            .await
            .expect("destroy revalidates the publication");
        assert_eq!(
            state.instances.lock().expect("instances")[&instance.id].state,
            SandboxState::Destroyed
        );
        assert_eq!(
            state
                .state_store
                .load(instance.id)
                .expect("persisted terminal record")
                .state,
            SandboxState::Destroyed
        );
        assert!(
            !state
                .state_store
                .has_run_dir_residual(instance.id)
                .expect("released publication residual")
        );
        assert!(matches!(
            state.state_store.run_dir(instance.id),
            Err(BlazeDaemonError::NotFound(_))
        ));
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn unconfirmed_publication_rejects_a_replaced_directory_on_retry() {
        let temp = tempfile::tempdir().expect("temp");
        let state = mock_state(&temp);
        let hook = crate::failpoint::TestFailpoint::new(&["state-post-publication-identity"]);

        hook.run(create_sandbox(&state, &test_request()))
            .await
            .expect_err("unconfirmed publication");
        let instance = state
            .instances
            .lock()
            .expect("instances")
            .values()
            .next()
            .cloned()
            .expect("recovery record");
        let configured = temp.path().join("state").join(instance.id.to_string());
        let retained = temp.path().join("retained-state-directory");
        std::fs::rename(&configured, &retained).expect("move retained state directory");
        std::fs::create_dir(&configured).expect("replacement state directory");

        let error = destroy_sandbox(&state, &instance.id.to_string())
            .await
            .expect_err("replacement must keep recovery fail-closed");

        assert!(matches!(error, BlazeDaemonError::RecoveryRequired(_)));
        assert_eq!(
            state.instances.lock().expect("instances")[&instance.id].state,
            SandboxState::RecoveryRequired
        );
        assert!(
            state
                .state_store
                .has_run_dir_residual(instance.id)
                .expect("publication residual")
        );
        assert!(
            configured
                .read_dir()
                .expect("replacement directory")
                .next()
                .is_none()
        );
        assert!(retained.join("state.json").is_file());
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn spawn_failure_rollback_commit_failure_remains_retryable() {
        assert_create_rollback_commit_failure_is_retryable(&[
            "create-spawn",
            "create-rollback-final-state-commit",
        ])
        .await;
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn clean_acquire_failure_rollback_commit_failure_remains_retryable() {
        assert_create_rollback_commit_failure_is_retryable(&[
            "storage-acquire",
            "create-rollback-final-state-commit",
        ])
        .await;
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn initial_publication_and_rollback_failures_remain_retryable() {
        assert_create_rollback_commit_failure_is_retryable(&[
            "state-first-publication-root-sync",
            "create-rollback-final-state-commit",
        ])
        .await;
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn destroy_intent_failure_does_not_touch_owned_resources() {
        let temp = tempfile::tempdir().expect("temp");
        let (state, kill_count, orphan_cleanup_count, release_count) = counting_state(&temp);
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id").to_string();
        let uuid = Uuid::parse_str(&id).expect("uuid");
        let hook = crate::failpoint::TestFailpoint::new(&["destroy-intent-state-commit"]);

        let error = hook
            .run(destroy_sandbox(&state, &id))
            .await
            .expect_err("intent failure");

        assert!(matches!(error, BlazeDaemonError::RecoveryRequired(_)));
        assert_eq!(kill_count.load(Ordering::Acquire), 0);
        assert_eq!(orphan_cleanup_count.load(Ordering::Acquire), 0);
        assert_eq!(release_count.load(Ordering::Acquire), 0);
        let retained = state.instances.lock().expect("instances")[&uuid].clone();
        assert_eq!(retained.state, SandboxState::RecoveryRequired);
        assert!(retained.operation.is_none());
        let persisted = state
            .state_store
            .load(uuid)
            .expect("persisted recovery state");
        assert_eq!(persisted.state, SandboxState::RecoveryRequired);
        assert_eq!(persisted.backend_ownership, BackendOwnership::Running);
        assert!(persisted.operation.is_none());
        assert!(temp.path().join("instances").join(&id).is_dir());
        assert!(state.state_store.run_dir(uuid).is_ok());

        destroy_sandbox(&state, &id).await.expect("destroy retry");
        assert_eq!(kill_count.load(Ordering::Acquire), 1);
        assert_eq!(release_count.load(Ordering::Acquire), 1);
        assert_eq!(
            state.instances.lock().expect("instances")[&uuid].state,
            SandboxState::Destroyed
        );
        let persisted = state
            .state_store
            .load(uuid)
            .expect("persisted destroyed state");
        assert_eq!(persisted.state, SandboxState::Destroyed);
        assert!(persisted.operation.is_none());
        assert!(matches!(
            state.state_store.run_dir(uuid),
            Err(BlazeDaemonError::NotFound(_))
        ));
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn destroy_stop_commit_failure_retains_storage_for_retry() {
        let temp = tempfile::tempdir().expect("temp");
        let (state, kill_count, orphan_cleanup_count, release_count) = counting_state(&temp);
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id").to_string();
        let uuid = Uuid::parse_str(&id).expect("uuid");
        let hook = crate::failpoint::TestFailpoint::new(&["destroy-stop-state-commit"]);

        let error = hook
            .run(destroy_sandbox(&state, &id))
            .await
            .expect_err("stop commit failure");

        assert!(matches!(error, BlazeDaemonError::RecoveryRequired(_)));
        assert_eq!(kill_count.load(Ordering::Acquire), 1);
        assert_eq!(orphan_cleanup_count.load(Ordering::Acquire), 0);
        assert_eq!(release_count.load(Ordering::Acquire), 0);
        let retained = state.instances.lock().expect("instances")[&uuid].clone();
        assert_eq!(retained.state, SandboxState::RecoveryRequired);
        assert_eq!(retained.backend_ownership, BackendOwnership::Stopped);
        assert_eq!(
            retained.operation.as_ref().map(|operation| operation.kind),
            Some(OperationKind::Destroy)
        );
        let persisted = state
            .state_store
            .load(uuid)
            .expect("persisted recovery state");
        assert_eq!(persisted.state, SandboxState::RecoveryRequired);
        assert_eq!(persisted.backend_ownership, BackendOwnership::Stopped);
        assert_eq!(
            persisted.operation.as_ref().map(|operation| operation.kind),
            Some(OperationKind::Destroy)
        );
        assert!(temp.path().join("instances").join(&id).is_dir());
        assert!(state.state_store.run_dir(uuid).is_ok());

        destroy_sandbox(&state, &id).await.expect("destroy retry");
        assert_eq!(kill_count.load(Ordering::Acquire), 1);
        assert_eq!(release_count.load(Ordering::Acquire), 1);
        let persisted = state
            .state_store
            .load(uuid)
            .expect("persisted destroyed state");
        assert_eq!(persisted.state, SandboxState::Destroyed);
        assert!(persisted.operation.is_none());
        assert!(matches!(
            state.state_store.run_dir(uuid),
            Err(BlazeDaemonError::NotFound(_))
        ));
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn destroy_final_commit_failure_retains_retryable_metadata() {
        let temp = tempfile::tempdir().expect("temp");
        let (state, kill_count, orphan_cleanup_count, release_count) = counting_state(&temp);
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id").to_string();
        let uuid = Uuid::parse_str(&id).expect("uuid");
        let hook = crate::failpoint::TestFailpoint::new(&["destroy-final-state-commit"]);

        let error = hook
            .run(destroy_sandbox(&state, &id))
            .await
            .expect_err("final commit failure");

        assert!(matches!(error, BlazeDaemonError::RecoveryRequired(_)));
        assert_eq!(kill_count.load(Ordering::Acquire), 1);
        assert_eq!(orphan_cleanup_count.load(Ordering::Acquire), 0);
        assert_eq!(release_count.load(Ordering::Acquire), 1);
        assert!(!temp.path().join("instances").join(&id).exists());
        let retained = state.instances.lock().expect("instances")[&uuid].clone();
        assert_eq!(retained.state, SandboxState::RecoveryRequired);
        assert_eq!(retained.backend_ownership, BackendOwnership::Stopped);
        assert_eq!(
            retained.operation.as_ref().map(|operation| operation.kind),
            Some(OperationKind::Destroy)
        );
        let persisted = state
            .state_store
            .load(uuid)
            .expect("persisted recovery state");
        assert_eq!(persisted.state, SandboxState::RecoveryRequired);
        assert_eq!(persisted.backend_ownership, BackendOwnership::Stopped);
        assert_eq!(
            persisted.operation.as_ref().map(|operation| operation.kind),
            Some(OperationKind::Destroy)
        );
        assert!(state.state_store.run_dir(uuid).is_ok());

        destroy_sandbox(&state, &id).await.expect("destroy retry");
        assert_eq!(kill_count.load(Ordering::Acquire), 1);
        assert_eq!(release_count.load(Ordering::Acquire), 2);
        let destroyed = state.instances.lock().expect("instances")[&uuid].clone();
        assert_eq!(destroyed.state, SandboxState::Destroyed);
        assert!(destroyed.operation.is_none());
        let persisted = state
            .state_store
            .load(uuid)
            .expect("persisted destroyed state");
        assert_eq!(persisted.state, SandboxState::Destroyed);
        assert!(persisted.operation.is_none());
        assert!(matches!(
            state.state_store.run_dir(uuid),
            Err(BlazeDaemonError::NotFound(_))
        ));
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn acquire_rollback_failure_retains_a_destroyable_record() {
        let temp = tempfile::tempdir().expect("temp");
        let state = mock_state(&temp);
        let acquire_hook = crate::failpoint::TestFailpoint::new(&[
            "storage-acquire-artifacts",
            "storage-acquire-rollback",
        ]);
        let error = acquire_hook
            .run(create_sandbox(&state, &test_request()))
            .await
            .expect_err("residual slot must require recovery");
        assert!(matches!(error, BlazeDaemonError::RecoveryRequired(_)));

        let instance = state
            .instances
            .lock()
            .expect("instances")
            .values()
            .next()
            .cloned()
            .expect("recovery record");
        assert_eq!(instance.state, SandboxState::RecoveryRequired);
        assert_eq!(instance.backend_ownership, BackendOwnership::NotStarted);
        assert_eq!(
            instance.operation.as_ref().map(|operation| operation.kind),
            Some(OperationKind::Create)
        );
        assert!(
            temp.path()
                .join("instances")
                .join(instance.id.to_string())
                .is_dir()
        );
        destroy_sandbox(&state, &instance.id.to_string())
            .await
            .expect("destroy residual slot");
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn acquired_slot_is_destroyable_after_restart_before_start_commit() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let instances_dir = config.storage.instances_dir.clone();
        let initial_storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            instances_dir.clone(),
        ));
        let initial_state = build_test_state(
            config.clone(),
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, Arc::new(MockSpawner)),
            BackendKind::Mock,
            initial_storage,
        );
        let pause_hook = crate::failpoint::TestFailpoint::new(&["create-after-storage-acquire"]);
        let create_state = initial_state.clone();
        let create_hook = pause_hook.clone();
        let create = tokio::spawn(async move {
            create_hook
                .run(create_sandbox(&create_state, &test_request()))
                .await
        });
        pause_hook.wait_until_paused().await;

        let instance = initial_state
            .instances
            .lock()
            .expect("instances")
            .values()
            .next()
            .cloned()
            .expect("write-ahead instance");
        let id = instance.id;
        assert_eq!(instance.state, SandboxState::Creating);
        assert_eq!(instance.backend_ownership, BackendOwnership::NotStarted);
        assert_eq!(
            instance.operation.as_ref().map(|operation| operation.kind),
            Some(OperationKind::Create)
        );
        assert!(
            config
                .daemon
                .state_dir
                .join(id.to_string())
                .join("state.json")
                .is_file()
        );
        assert!(instances_dir.join(id.to_string()).is_dir());

        create.abort();
        assert!(
            create
                .await
                .expect_err("create task aborted")
                .is_cancelled()
        );
        drop(initial_state);

        let cleanup_count = Arc::new(AtomicUsize::new(0));
        let restarted_storage: Arc<dyn StorageProvider> =
            Arc::new(FileStorageProvider::with_images(
                config.storage.images_dir.clone(),
                instances_dir.clone(),
            ));
        let restarted = build_test_state(
            config,
            test_policy(BackendKind::Mock),
            spawners(
                BackendKind::Mock,
                Arc::new(RecordingSpawner {
                    cleanup_count: cleanup_count.clone(),
                }),
            ),
            BackendKind::Mock,
            restarted_storage,
        );
        assert!(
            restarted
                .instances
                .lock()
                .expect("instances")
                .contains_key(&id)
        );

        destroy_sandbox(&restarted, &id.to_string())
            .await
            .expect("destroy acquired slot after restart");
        assert_eq!(cleanup_count.load(Ordering::Acquire), 0);
        assert_eq!(
            restarted.instances.lock().expect("instances")[&id].state,
            SandboxState::Destroyed
        );
        assert!(!instances_dir.join(id.to_string()).exists());
    }

    #[tokio::test]
    async fn startup_reconciliation_continues_after_one_cleanup_failure() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let storage = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let failed_id = Uuid::new_v4();
        let completed_id = Uuid::new_v4();
        for id in [failed_id, completed_id] {
            let mut instance = SandboxInstance::new(
                BackendKind::Mock,
                WorkloadClass::AgentTool,
                "sha256:reconcile".into(),
                "reconcile-test".into(),
            );
            instance.id = id;
            instance
                .transition(SandboxState::Creating)
                .expect("creating");
            instance.transition(SandboxState::Running).expect("running");
            instance.backend_ownership = BackendOwnership::Running;
            instance.persist(&config.daemon.state_dir).expect("persist");
            storage
                .acquire(&AcquireOpts {
                    instance_id: id.to_string(),
                    rootfs_size: 64,
                    mem_size: 32,
                })
                .await
                .expect("storage");
        }
        let cleanup_count = Arc::new(AtomicUsize::new(0));
        let state = build_test_state(
            config.clone(),
            test_policy(BackendKind::Mock),
            spawners(
                BackendKind::Mock,
                Arc::new(SelectiveCleanupSpawner {
                    failed_id,
                    cleanup_count: cleanup_count.clone(),
                }),
            ),
            BackendKind::Mock,
            storage,
        );

        let report = state.manager.reconcile_startup().await;

        assert_eq!(report.attempted, 2);
        assert_eq!(report.completed, 1);
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].instance_id, failed_id);
        assert_eq!(cleanup_count.load(Ordering::Acquire), 2);
        assert_eq!(
            state.instances.lock().expect("instances")[&failed_id].state,
            SandboxState::RecoveryRequired
        );
        assert_eq!(
            state.instances.lock().expect("instances")[&completed_id].state,
            SandboxState::Destroyed
        );
        assert!(
            config
                .storage
                .instances_dir
                .join(failed_id.to_string())
                .is_dir()
        );
        assert!(
            !config
                .storage
                .instances_dir
                .join(completed_id.to_string())
                .exists()
        );
        assert!(state.state_store.run_dir(failed_id).is_ok());
        assert!(matches!(
            state.state_store.run_dir(completed_id),
            Err(BlazeDaemonError::NotFound(_))
        ));
        let created = created_json(&state, &test_request()).await;
        assert_eq!(created["instance"]["state"], "running");
    }

    #[tokio::test]
    async fn startup_reconciliation_destroys_legacy_reset_and_warm_records() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let release_count = Arc::new(AtomicUsize::new(0));
        let storage: Arc<dyn StorageProvider> = Arc::new(CountingStorage {
            inner: FileStorageProvider::with_images(
                config.storage.images_dir.clone(),
                config.storage.instances_dir.clone(),
            ),
            release_count: release_count.clone(),
        });
        let mut ids = Vec::new();
        for state_name in ["reset", "warm"] {
            let id = Uuid::new_v4();
            ids.push(id);
            let now = chrono::Utc::now();
            let record = json!({
                "id": id,
                "state": state_name,
                "backend": "mock",
                "workload_class": "agent-tool",
                "image_digest": "sha256:legacy",
                "start_path": "warm",
                "created_at": now,
                "updated_at": now,
                "policy_name": "legacy",
                "backend_ownership": "running"
            });
            let run_dir = config.daemon.state_dir.join(id.to_string());
            std::fs::create_dir(&run_dir).expect("legacy run directory");
            std::fs::write(
                run_dir.join("state.json"),
                serde_json::to_vec_pretty(&record).expect("legacy state JSON"),
            )
            .expect("legacy state record");
            storage
                .acquire(&AcquireOpts {
                    instance_id: id.to_string(),
                    rootfs_size: 64,
                    mem_size: 32,
                })
                .await
                .expect("legacy storage");
        }

        let kill_count = Arc::new(AtomicUsize::new(0));
        let orphan_cleanup_count = Arc::new(AtomicUsize::new(0));
        let state = build_test_state(
            config.clone(),
            test_policy(BackendKind::Mock),
            spawners(
                BackendKind::Mock,
                Arc::new(CountingSpawner {
                    kill_count: kill_count.clone(),
                    orphan_cleanup_count: orphan_cleanup_count.clone(),
                }),
            ),
            BackendKind::Mock,
            storage,
        );

        let report = state.manager.reconcile_startup().await;

        assert_eq!(report.attempted, 2);
        assert_eq!(report.completed, 2);
        assert!(report.failures.is_empty());
        assert_eq!(kill_count.load(Ordering::Acquire), 0);
        assert_eq!(orphan_cleanup_count.load(Ordering::Acquire), 2);
        assert_eq!(release_count.load(Ordering::Acquire), 2);
        for id in ids {
            assert_eq!(
                state.instances.lock().expect("instances")[&id].state,
                SandboxState::Destroyed
            );
            assert!(!config.storage.instances_dir.join(id.to_string()).exists());
            assert!(matches!(
                state.state_store.run_dir(id),
                Err(BlazeDaemonError::NotFound(_))
            ));
        }
    }

    #[tokio::test]
    async fn startup_reconciliation_skips_cleanup_for_known_stopped_states() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let release_count = Arc::new(AtomicUsize::new(0));
        let storage: Arc<dyn StorageProvider> = Arc::new(CountingStorage {
            inner: FileStorageProvider::with_images(
                config.storage.images_dir.clone(),
                config.storage.instances_dir.clone(),
            ),
            release_count: release_count.clone(),
        });
        let not_started_id = Uuid::new_v4();
        let stopped_id = Uuid::new_v4();

        let mut not_started = SandboxInstance::new(
            BackendKind::Mock,
            WorkloadClass::AgentTool,
            "sha256:not-started".into(),
            "reconcile-test".into(),
        );
        not_started.id = not_started_id;
        not_started
            .transition(SandboxState::Creating)
            .expect("creating");
        not_started
            .persist(&config.daemon.state_dir)
            .expect("persist");

        let mut stopped = SandboxInstance::new(
            BackendKind::Mock,
            WorkloadClass::AgentTool,
            "sha256:stopped".into(),
            "reconcile-test".into(),
        );
        stopped.id = stopped_id;
        stopped
            .transition(SandboxState::Creating)
            .expect("creating");
        stopped.transition(SandboxState::Running).expect("running");
        stopped.backend_ownership = BackendOwnership::Stopped;
        stopped.persist(&config.daemon.state_dir).expect("persist");

        for id in [not_started_id, stopped_id] {
            storage
                .acquire(&AcquireOpts {
                    instance_id: id.to_string(),
                    rootfs_size: 64,
                    mem_size: 32,
                })
                .await
                .expect("storage");
        }
        let kill_count = Arc::new(AtomicUsize::new(0));
        let orphan_cleanup_count = Arc::new(AtomicUsize::new(0));
        let state = build_test_state(
            config,
            test_policy(BackendKind::Mock),
            spawners(
                BackendKind::Mock,
                Arc::new(CountingSpawner {
                    kill_count: kill_count.clone(),
                    orphan_cleanup_count: orphan_cleanup_count.clone(),
                }),
            ),
            BackendKind::Mock,
            storage,
        );

        let report = state.manager.reconcile_startup().await;

        assert_eq!(report.attempted, 2);
        assert_eq!(report.completed, 2);
        assert!(report.failures.is_empty());
        assert_eq!(kill_count.load(Ordering::Acquire), 0);
        assert_eq!(orphan_cleanup_count.load(Ordering::Acquire), 0);
        assert_eq!(release_count.load(Ordering::Acquire), 2);
        assert_eq!(
            state.instances.lock().expect("instances")[&not_started_id].state,
            SandboxState::Destroyed
        );
        assert_eq!(
            state.instances.lock().expect("instances")[&stopped_id].state,
            SandboxState::Destroyed
        );
    }

    #[tokio::test]
    async fn template_routes_import_list_and_get_published_artifacts() {
        let temp = tempfile::tempdir().expect("temp");
        let import_root = temp.path().join("imports");
        let source = import_root.join("source");
        std::fs::create_dir(&import_root).expect("import root");
        std::fs::create_dir(&source).expect("source");
        std::fs::write(source.join("vmstate.snap"), b"snapshot").expect("snapshot");
        std::fs::write(source.join("mem.bin"), b"memory").expect("memory");
        std::fs::write(source.join("rootfs.ext4"), b"rootfs").expect("rootfs");

        let mut config = DaemonConfig::default();
        config.daemon.state_dir = temp.path().join("state");
        config.storage.images_dir = temp.path().join("images");
        config.storage.instances_dir = temp.path().join("instances");
        config.template.dir = temp.path().join("templates");
        config.template.import_root = Some(import_root);
        for directory in [
            &config.daemon.state_dir,
            &config.storage.images_dir,
            &config.storage.instances_dir,
            &config.template.dir,
        ] {
            std::fs::create_dir_all(directory).expect("directory");
        }
        let storage: Arc<dyn blaze_core::storage::StorageProvider> =
            Arc::new(FileStorageProvider::with_images(
                config.storage.images_dir.clone(),
                config.storage.instances_dir.clone(),
            ));
        let state = Arc::new(
            ServerState::build(
                config,
                PolicyEngine::with_policies(Vec::new()),
                HookRegistry::new(),
                spawners(BackendKind::Mock, Arc::new(MockSpawner)),
                BackendKind::Mock,
                storage,
            )
            .expect("state"),
        );

        for (method, path) in [
            (Method::GET, "/v1/runtime-templates"),
            (Method::POST, "/v1/templates/gc"),
        ] {
            let error = dispatch(&method, path, "", Vec::new(), &state)
                .await
                .expect_err("retired template route");
            assert!(matches!(error, BlazeDaemonError::NotFound(_)));
        }

        let request = serde_json::to_vec(&json!({
            "name": "runtime-base",
            "source": "source",
            "description": "reusable runtime",
        }))
        .expect("request");
        let imported = dispatch(
            &Method::POST,
            "/v1/templates/import",
            "",
            request.clone(),
            &state,
        )
        .await
        .expect("import");
        assert_eq!(imported.status(), StatusCode::CREATED);
        let imported = serde_json::from_slice::<serde_json::Value>(
            &imported
                .into_body()
                .collect()
                .await
                .expect("body")
                .to_bytes(),
        )
        .expect("json");
        assert_eq!(imported["name"], "runtime-base");
        assert_eq!(imported["description"], "reusable runtime");

        let listed = dispatch(&Method::GET, "/v1/templates", "", Vec::new(), &state)
            .await
            .expect("list");
        let concurrent_list = dispatch(&Method::GET, "/v1/templates", "", Vec::new(), &state)
            .await
            .expect_err("list response must retain the single-flight permit");
        assert!(matches!(
            concurrent_list,
            BlazeDaemonError::ServiceUnavailable(_)
        ));
        let listed = serde_json::from_slice::<serde_json::Value>(
            &listed.into_body().collect().await.expect("body").to_bytes(),
        )
        .expect("json");
        assert_eq!(listed, json!([{ "name": "runtime-base" }]));

        dispatch(&Method::GET, "/v1/templates", "", Vec::new(), &state)
            .await
            .expect("list after response body release");

        let fetched = dispatch(
            &Method::GET,
            "/v1/templates/runtime-base",
            "",
            Vec::new(),
            &state,
        )
        .await
        .expect("get");
        assert_eq!(
            fetched.headers().get(CONTENT_TYPE).expect("content type"),
            "application/json"
        );
        let concurrent_get = dispatch(
            &Method::GET,
            "/v1/templates/runtime-base",
            "",
            Vec::new(),
            &state,
        )
        .await
        .expect_err("item response must retain the single-flight permit");
        assert!(matches!(
            concurrent_get,
            BlazeDaemonError::ServiceUnavailable(_)
        ));
        let fetched = serde_json::from_slice::<serde_json::Value>(
            &fetched
                .into_body()
                .collect()
                .await
                .expect("body")
                .to_bytes(),
        )
        .expect("json");
        assert_eq!(fetched, imported);

        dispatch(
            &Method::GET,
            "/v1/templates/runtime-base",
            "",
            Vec::new(),
            &state,
        )
        .await
        .expect("get after response body release");

        let duplicate = dispatch(&Method::POST, "/v1/templates/import", "", request, &state)
            .await
            .expect_err("duplicate");
        assert!(matches!(duplicate, BlazeDaemonError::Conflict(_)));
    }

    // ---- template-backed create -------------------------------------------

    /// Write a Mock-backend template source directory with a valid manifest.
    fn write_template_source(root: &Path, expose_guest_socket: bool) {
        std::fs::create_dir_all(root).expect("template source");
        let memory = vec![0_u8; 1024 * 1024];
        std::fs::write(root.join("vmstate.snap"), b"snapshot").expect("template VM state");
        std::fs::write(root.join("mem.bin"), &memory).expect("template memory");
        std::fs::write(root.join("rootfs.ext4"), b"rootfs").expect("template rootfs");
        let digest = |bytes: &[u8]| format!("{:x}", Sha256::digest(bytes));
        let metadata = json!({
            "format_version": 1,
            "name": "source",
            "image_digest": "sha256:template-image",
            "backend": "mock",
            "backend_version": "guest-mock-v1",
            "snapshot_kind": "full",
            "expose_guest_socket": expose_guest_socket,
            "network": false,
            "rootfs_size": 6,
            "memory_size": 1048576,
            "artifacts": [
                {"name": "vmstate.snap", "size_bytes": 8, "sha256": digest(b"snapshot")},
                {"name": "mem.bin", "size_bytes": 1048576, "sha256": digest(&memory)},
                {"name": "rootfs.ext4", "size_bytes": 6, "sha256": digest(b"rootfs")}
            ]
        });
        std::fs::write(
            root.join("template.json"),
            serde_json::to_vec(&metadata).expect("template metadata"),
        )
        .expect("write template metadata");
    }

    /// Inputs a template-backed restore observed, for isolation assertions.
    struct ObservedTemplateRestore {
        instance_id: Uuid,
        preserve_network: bool,
        snapshot: Vec<u8>,
        memory: Vec<u8>,
        rootfs: Vec<u8>,
    }

    /// A spawner that refuses cold spawn and records restore inputs, then hands
    /// off to the guest-ready mock owner so create reaches its readiness gate.
    struct TemplateRestoreSpawner {
        observed: Arc<std::sync::Mutex<Option<ObservedTemplateRestore>>>,
    }

    #[async_trait]
    impl BackendSpawner for TemplateRestoreSpawner {
        async fn spawn(
            &self,
            _request: BackendSpawnRequest,
        ) -> std::result::Result<DynBackendInstance, SpawnFailure> {
            Err(SpawnFailure::clean(BlazeError::BackendError {
                msg: "template create must use restore".to_string(),
            }))
        }

        async fn restore_capability(
            &self,
            _executable: Option<&crate::spawner::PinnedExecutable>,
        ) -> blaze_core::Result<Option<blaze_core::backend::RestoreCapability>> {
            Ok(Some(blaze_core::backend::RestoreCapability {
                backend: BackendKind::Mock,
                version: Some("guest-mock-v1".to_string()),
                snapshot_kind: blaze_core::backend::SnapshotKind::Full,
            }))
        }

        async fn restore(
            &self,
            request: crate::spawner::BackendRestoreRequest,
        ) -> crate::spawner::RestoreResult {
            let observed = ObservedTemplateRestore {
                instance_id: request.instance_id,
                preserve_network: request.preserve_network,
                snapshot: tokio::fs::read(request.payload_dir.join("vmstate.snap"))
                    .await
                    .map_err(SpawnFailure::from)?,
                memory: tokio::fs::read(request.payload_dir.join("memory.snap"))
                    .await
                    .map_err(SpawnFailure::from)?,
                rootfs: tokio::fs::read(&request.storage.rootfs_path)
                    .await
                    .map_err(SpawnFailure::from)?,
            };
            *self.observed.lock().expect("template observation") = Some(observed);
            let spawn = BackendSpawnRequest::new(
                blaze_core::backend::SpawnRequest {
                    instance_id: request.instance_id,
                    binary_path: request.binary_path.clone(),
                    storage: request.storage.clone(),
                    backend: BackendConfigs::default(),
                    vm: None,
                },
                request.run_dir.clone(),
            )
            .map_err(SpawnFailure::clean)?;
            GuestMockSpawner.spawn(spawn).await
        }

        async fn probe(&self, _binary_path: &Path) -> blaze_core::Result<bool> {
            Ok(true)
        }

        async fn cleanup_orphan(
            &self,
            instance_id: Uuid,
            run_dir: &OwnedRunDir,
        ) -> blaze_core::Result<()> {
            GuestMockSpawner.cleanup_orphan(instance_id, run_dir).await
        }
    }

    /// Build a Mock-backend server state with one imported `runtime-base`
    /// template. `allowed` controls whether the policy lists it as selectable.
    async fn template_test_state(
        temp: &tempfile::TempDir,
        allowed: bool,
        expose_guest_socket: bool,
    ) -> (
        Arc<ServerState>,
        Arc<std::sync::Mutex<Option<ObservedTemplateRestore>>>,
        DaemonConfig,
    ) {
        let mut config = test_config(temp);
        // The catalog refuses symlink components in its root. Resolve the
        // temporary directory first so these tests also run where the system
        // temporary path itself is a symlink, as on macOS.
        let resolved = std::fs::canonicalize(temp.path()).expect("resolve temp root");
        config.daemon.state_dir = resolved.join("state");
        config.storage.images_dir = resolved.join("images");
        config.storage.instances_dir = resolved.join("instances");
        config.template.dir = resolved.join("templates");
        let import_root = resolved.join("imports");
        write_template_source(&import_root.join("source"), expose_guest_socket);
        config.template.import_root = Some(import_root);
        let binary = resolved.join("test-backend");
        std::fs::write(&binary, b"test backend").expect("backend fixture");
        // Preflight pins the configured executable, which requires the file to
        // actually be executable.
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755))
                .expect("backend fixture permissions");
        }
        config.backends.insert("mock".to_string(), binary);
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let observed = Arc::new(std::sync::Mutex::new(None));
        let mut policy = test_policy(BackendKind::Mock);
        if allowed {
            policy.select.templates =
                vec!["runtime-base".to_string(), "missing-template".to_string()];
        }
        let state = build_test_state(
            config.clone(),
            policy,
            spawners(
                BackendKind::Mock,
                Arc::new(TemplateRestoreSpawner {
                    observed: observed.clone(),
                }),
            ),
            BackendKind::Mock,
            storage,
        );
        state
            .manager
            .import_template(
                "runtime-base".to_string(),
                PathBuf::from("source"),
                String::new(),
            )
            .await
            .expect("import template");
        (state, observed, config)
    }

    #[tokio::test]
    async fn template_create_restores_independent_sandboxes() {
        let temp = tempfile::tempdir().expect("temp");
        let (state, observed, config) = template_test_state(&temp, true, false).await;
        let request = serde_json::to_vec(&json!({
            "workload_class": "agent-tool",
            "image_digest": "sha256:template-image",
            "template": "runtime-base"
        }))
        .expect("create request");

        let first = created_json(&state, &request).await;
        let first_id =
            Uuid::parse_str(first["instance"]["id"].as_str().expect("instance id")).expect("uuid");
        let first_restore = observed
            .lock()
            .expect("observation")
            .take()
            .expect("first restore");
        // Mutating one sandbox's private rootfs must not affect the next.
        let first_rootfs = config
            .storage
            .instances_dir
            .join(first_id.to_string())
            .join("rootfs.ext4");
        std::fs::write(&first_rootfs, b"cloned").expect("mutate first rootfs");

        let second = created_json(&state, &request).await;
        let second_id =
            Uuid::parse_str(second["instance"]["id"].as_str().expect("instance id")).expect("uuid");
        let second_restore = observed
            .lock()
            .expect("observation")
            .take()
            .expect("second restore");
        let catalog_rootfs = config.template.dir.join("runtime-base/rootfs.ext4");

        assert_ne!(first_id, second_id);
        assert_eq!(first["instance"]["template"], "runtime-base");
        assert_eq!(second["instance"]["template"], "runtime-base");
        assert_eq!(first_restore.instance_id, first_id);
        assert_eq!(second_restore.instance_id, second_id);
        // A new sandbox never inherits the source network slot.
        assert!(!first_restore.preserve_network);
        // Each restore observed the published artifacts, byte for byte.
        assert_eq!(first_restore.snapshot, b"snapshot");
        assert_eq!(second_restore.rootfs, b"rootfs");
        assert_eq!(first_restore.memory.len(), 1024 * 1024);
        // The catalog copy is untouched by a per-sandbox mutation.
        assert_eq!(
            std::fs::read(&catalog_rootfs).expect("catalog rootfs"),
            b"rootfs"
        );
        assert_eq!(
            std::fs::read(&first_rootfs).expect("first rootfs"),
            b"cloned"
        );
    }

    #[tokio::test]
    async fn template_create_is_rejected_when_policy_disallows_it() {
        let temp = tempfile::tempdir().expect("temp");
        let (state, observed, config) = template_test_state(&temp, false, false).await;
        let instances_dir = config.storage.instances_dir.clone();

        let error = create_sandbox(
            &state,
            &serde_json::to_vec(&json!({
                "workload_class": "agent-tool",
                "image_digest": "sha256:template-image",
                "template": "runtime-base"
            }))
            .expect("create request"),
        )
        .await
        .expect_err("policy must allow the template");

        assert!(matches!(error, BlazeDaemonError::Conflict(_)));
        assert!(observed.lock().expect("observation").is_none());
        assert!(state.manager.list().expect("instances").is_empty());
        assert_eq!(
            std::fs::read_dir(instances_dir).expect("instances").count(),
            0
        );
    }

    #[tokio::test]
    async fn template_create_rejects_mismatched_image_without_lifecycle_state() {
        let temp = tempfile::tempdir().expect("temp");
        let (state, observed, config) = template_test_state(&temp, true, false).await;
        let instances_dir = config.storage.instances_dir.clone();

        let error = create_sandbox(
            &state,
            &serde_json::to_vec(&json!({
                "workload_class": "agent-tool",
                "image_digest": "sha256:different-image",
                "template": "runtime-base"
            }))
            .expect("create request"),
        )
        .await
        .expect_err("image identity must match the template");

        assert!(matches!(error, BlazeDaemonError::Conflict(_)));
        assert!(observed.lock().expect("observation").is_none());
        assert!(state.manager.list().expect("instances").is_empty());
        assert_eq!(
            std::fs::read_dir(instances_dir).expect("instances").count(),
            0
        );
    }

    #[tokio::test]
    async fn template_create_rejects_unsupported_mock_guest_socket_without_lifecycle_state() {
        let temp = tempfile::tempdir().expect("temp");
        let (state, observed, config) = template_test_state(&temp, true, true).await;
        let instances_dir = config.storage.instances_dir.clone();

        let error = create_sandbox(
            &state,
            &serde_json::to_vec(&json!({
                "workload_class": "agent-tool",
                "image_digest": "sha256:template-image",
                "template": "runtime-base"
            }))
            .expect("create request"),
        )
        .await
        .expect_err("Mock cannot restore a guest transport");

        assert!(matches!(error, BlazeDaemonError::UnsupportedOperation(_)));
        assert!(observed.lock().expect("observation").is_none());
        assert!(state.manager.list().expect("instances").is_empty());
        assert_eq!(
            std::fs::read_dir(instances_dir).expect("instances").count(),
            0
        );
    }
}
