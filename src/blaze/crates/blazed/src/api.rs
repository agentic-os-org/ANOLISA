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
use blaze_core::backend::{BackendKind, BackendStatus, SnapshotKind, select_backend};
use blaze_core::checkpoint::{CheckpointArtifact, CheckpointMetadata};
use blaze_core::lifecycle::{
    BackendOwnership, OperationJournal, OperationKind, OperationPhase, SandboxInstance,
    SandboxState, StartPath,
};
use blaze_core::policy::{ImageMetadata, RuntimeDecision, WorkloadClass};
use blaze_provider_api::{CapacityScope, CapacitySnapshot, DrainRequest, DrainResult};
use chrono::{DateTime, Utc};
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
        ("GET", ["v1", "pools", backend, class]) => pool_capacity(state, backend, class).await,
        ("POST", ["v1", "pools", backend, class, "drain"]) => {
            drain_pool_capacity(state, backend, class, &body).await
        }
        ("GET", ["v1", "pools"]) | ("PUT", ["v1", "pools", _, _, "sizing"]) => {
            pool_operation_unavailable()
        }
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
    instance: SandboxResp,
    decision: RuntimeDecision,
    start_path: StartPath,
    selected_backend: BackendKind,
}

/// Stable management representation of a sandbox.
///
/// The daemon can persist additional ownership and recovery records without
/// extending the management API. Only fields intentionally listed here are
/// returned to API clients.
#[derive(Debug, Serialize)]
struct SandboxResp {
    id: Uuid,
    state: SandboxState,
    backend: BackendKind,
    workload_class: WorkloadClass,
    image_digest: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    template: Option<String>,
    start_path: StartPath,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    policy_name: String,
    backend_ownership: BackendOwnership,
    operation: Option<OperationResp>,
    last_checkpoint: Option<String>,
}

/// Stable management representation of an in-progress sandbox operation.
///
/// The persisted journal may contain write-ahead recovery records. Keeping a
/// separate response type prevents those records, and future recovery-only
/// journal fields, from silently becoming part of the management API.
#[derive(Debug, Serialize)]
struct OperationResp {
    kind: OperationKind,
    started_at: DateTime<Utc>,
    checkpoint_id: Option<String>,
    phase: Option<OperationPhase>,
}

impl From<OperationJournal> for OperationResp {
    fn from(operation: OperationJournal) -> Self {
        Self {
            kind: operation.kind,
            started_at: operation.started_at,
            checkpoint_id: operation.checkpoint_id,
            phase: operation.phase,
        }
    }
}

impl From<SandboxInstance> for SandboxResp {
    fn from(instance: SandboxInstance) -> Self {
        Self {
            id: instance.id,
            state: instance.state,
            backend: instance.backend,
            workload_class: instance.workload_class,
            image_digest: instance.image_digest,
            template: instance.template,
            start_path: instance.start_path,
            created_at: instance.created_at,
            updated_at: instance.updated_at,
            policy_name: instance.policy_name,
            backend_ownership: instance.backend_ownership,
            operation: instance.operation.map(OperationResp::from),
            last_checkpoint: instance.last_checkpoint,
        }
    }
}

#[derive(Debug, Serialize)]
struct CheckpointResp {
    checkpoint_id: String,
    instance_id: Uuid,
    #[serde(flatten)]
    checkpoint: CheckpointMetadataResp,
}

/// Stable management representation of committed checkpoint metadata.
///
/// Provider ownership records remain durable state and are intentionally not
/// part of the management response.
#[derive(Debug, Serialize)]
struct CheckpointMetadataResp {
    format_version: u32,
    id: String,
    parent: Option<String>,
    sandbox_id: Uuid,
    policy_name: String,
    image_digest: String,
    backend: BackendKind,
    backend_version: Option<String>,
    created_at: DateTime<Utc>,
    snapshot_kind: SnapshotKind,
    artifacts: Vec<CheckpointArtifact>,
}

impl From<CheckpointMetadata> for CheckpointMetadataResp {
    fn from(checkpoint: CheckpointMetadata) -> Self {
        Self {
            format_version: checkpoint.format_version,
            id: checkpoint.id,
            parent: checkpoint.parent,
            sandbox_id: checkpoint.sandbox_id,
            policy_name: checkpoint.policy_name,
            image_digest: checkpoint.image_digest,
            backend: checkpoint.backend,
            backend_version: checkpoint.backend_version,
            created_at: checkpoint.created_at,
            snapshot_kind: checkpoint.snapshot_kind,
            artifacts: checkpoint.artifacts,
        }
    }
}

fn list_sandboxes(state: &Arc<ServerState>) -> Result<Response<Full<Bytes>>> {
    let sandboxes = state
        .manager
        .list()?
        .into_iter()
        .map(SandboxResp::from)
        .collect::<Vec<_>>();
    json_ok(&sandboxes)
}

fn get_sandbox(state: &Arc<ServerState>, id: &str) -> Result<Response<Full<Bytes>>> {
    json_ok(&SandboxResp::from(state.manager.get(parse_uuid(id)?)?))
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
        instance: SandboxResp::from(created.instance),
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
        checkpoint: CheckpointMetadataResp::from(checkpoint),
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
    let instance = state
        .manager
        .hibernate(uuid, HibernateSandbox { binary_path })
        .await?;
    json_ok(&SandboxResp::from(instance))
}

async fn resume(state: &Arc<ServerState>, id: &str) -> Result<Response<Full<Bytes>>> {
    let uuid = parse_uuid(id)?;
    let instance = state.manager.get(uuid)?;
    let binary_path = configured_backend_path(state, instance.backend)?;
    let instance = state
        .manager
        .resume(uuid, ResumeSandbox { binary_path })
        .await?;
    json_ok(&SandboxResp::from(instance))
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
        "pool inventory or sizing is not implemented".to_string(),
    ))
}

#[derive(Debug, Serialize, PartialEq, Eq)]
struct PoolCapacityResp {
    backend: BackendKind,
    class_sha256: String,
    root_filesystem_capacity_bytes: u64,
    guest_memory_capacity_bytes: u64,
    revision: u64,
    ready: u64,
    building: u64,
    in_use: u64,
    draining: u64,
    quarantined: u64,
    total: u64,
    accepting_allocations: bool,
}

impl PoolCapacityResp {
    fn from_snapshot(snapshot: CapacitySnapshot) -> Result<Self> {
        Ok(Self {
            backend: snapshot.scope.backend,
            class_sha256: encode_capacity_class_digest(snapshot.scope.class_digest),
            root_filesystem_capacity_bytes: snapshot.class.root_filesystem_capacity_bytes,
            guest_memory_capacity_bytes: snapshot.class.guest_memory_capacity_bytes,
            revision: snapshot.revision,
            ready: snapshot.ready,
            building: snapshot.building,
            in_use: snapshot.in_use,
            draining: snapshot.draining,
            quarantined: snapshot.quarantined,
            total: snapshot.checked_total().ok_or_else(|| {
                BlazeDaemonError::DataPlane(blaze_provider_api::ProviderError::InvalidResponse)
            })?,
            accepting_allocations: snapshot.accepting_allocations,
        })
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct DrainPoolCapacityReq {
    #[serde(default)]
    operation_id: Option<Uuid>,
}

#[derive(Debug, Serialize)]
struct DrainPoolCapacityResp {
    operation_id: Uuid,
    removed_ready: u64,
    deferred_in_use: u64,
    capacity: PoolCapacityResp,
}

fn parse_capacity_scope(state: &ServerState, backend: &str, class: &str) -> Result<CapacityScope> {
    let backend = backend
        .parse::<BackendKind>()
        .map_err(|_| BlazeDaemonError::BadRequest(format!("unknown pool backend {backend}")))?;
    if backend != state.active_backend {
        return Err(BlazeDaemonError::NotFound(format!(
            "pool backend {backend}"
        )));
    }
    Ok(CapacityScope {
        backend,
        class_digest: decode_capacity_class_digest(class)?,
    })
}

fn decode_capacity_class_digest(value: &str) -> Result<[u8; 32]> {
    let bytes = value.as_bytes();
    if bytes.len() != 64 {
        return Err(BlazeDaemonError::BadRequest(
            "pool class must be a 64-character lowercase SHA-256 digest".to_string(),
        ));
    }
    let mut digest = [0_u8; 32];
    for (index, output) in digest.iter_mut().enumerate() {
        let high = decode_lower_hex(bytes[index * 2]).ok_or_else(|| {
            BlazeDaemonError::BadRequest(
                "pool class must be a 64-character lowercase SHA-256 digest".to_string(),
            )
        })?;
        let low = decode_lower_hex(bytes[index * 2 + 1]).ok_or_else(|| {
            BlazeDaemonError::BadRequest(
                "pool class must be a 64-character lowercase SHA-256 digest".to_string(),
            )
        })?;
        *output = (high << 4) | low;
    }
    if digest == [0; 32] {
        return Err(BlazeDaemonError::BadRequest(
            "pool class must not be the all-zero digest".to_string(),
        ));
    }
    Ok(digest)
}

const fn decode_lower_hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

fn encode_capacity_class_digest(digest: [u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

async fn pool_capacity(
    state: &Arc<ServerState>,
    backend: &str,
    class: &str,
) -> Result<Response<Full<Bytes>>> {
    let scope = parse_capacity_scope(state, backend, class)?;
    let snapshot = state.manager.provider_capacity(scope).await?;
    json_ok(&PoolCapacityResp::from_snapshot(snapshot)?)
}

async fn drain_pool_capacity(
    state: &Arc<ServerState>,
    backend: &str,
    class: &str,
    body: &[u8],
) -> Result<Response<Full<Bytes>>> {
    let scope = parse_capacity_scope(state, backend, class)?;
    let request = if body.is_empty() {
        DrainPoolCapacityReq::default()
    } else {
        serde_json::from_slice(body).map_err(|error| {
            BlazeDaemonError::BadRequest(format!("invalid pool drain body: {error}"))
        })?
    };
    let result = state
        .manager
        .drain_provider_capacity(DrainRequest {
            scope,
            operation_id: request.operation_id.unwrap_or_else(Uuid::new_v4),
        })
        .await?;
    drain_pool_capacity_response(result)
}

fn drain_pool_capacity_response(result: DrainResult) -> Result<Response<Full<Bytes>>> {
    json_ok(&DrainPoolCapacityResp {
        operation_id: result.operation_id,
        removed_ready: result.removed_ready,
        deferred_in_use: result.deferred_in_use,
        capacity: PoolCapacityResp::from_snapshot(result.snapshot)?,
    })
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
    mod provider_contract;
    mod unsafe_prepare;
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
    use blaze_core::data_plane::{
        BackendProcessIdentity, BackendRuntimeRecord, DataPlaneLeaseState,
        DataPlaneRequestContextRecord, PendingProviderOperationKind,
        PendingProviderOperationRecord,
    };
    use blaze_core::kernel::HookRegistry;
    #[cfg(feature = "test-failpoints")]
    use blaze_core::lifecycle::OperationPhase;
    use blaze_core::lifecycle::{BackendOwnership, OperationKind, SandboxState};
    use blaze_core::policy::{
        BackendConfigs, FallbackOnMissingHook, PolicyEngine, PolicyFile, PolicyHooks, PolicyMatch,
        PolicySelect, WorkloadClass,
    };
    use blaze_core::storage::{
        AcquireOpts, OwnedStorageSlot, PoolStatus, StorageAcquireError, StorageOwnershipClaim,
        StorageOwnershipKey, StorageOwnershipRequest, StorageProvider, StorageSlot,
    };
    use blaze_provider_api::{
        AbortRequest, AbortResult, BeginInventoryRequest, CapacityClass, CapacityRequest,
        CapacitySnapshot, CheckpointSubmission, CommitRequest, CommittedLease, DataPlaneCapacity,
        DataPlaneCheckpoint, DataPlaneInventory, DataPlaneProvider, DataPlaneSuspend, DrainRequest,
        DrainResult, FinalizeRequest, FinalizedLease, InspectRequest, InventoryLease,
        InventoryPage, InventoryPageRequest, InventorySnapshot, LeaseBinding, LeaseState,
        ObservedLease, PrepareRequest, PrepareSource, PreparedLease, PreparedResources,
        ProviderCapabilities, ProviderCheckpointRef, ProviderCheckpointRequest, ProviderDescriptor,
        ProviderError, ProviderSuspensionRef, PublicTransitionRef, ReconcileAction,
        ReconcileRequest, ReconcileResult, ReleaseRequest, ReleaseResult, RequestContext,
        RestoreCheckpointRequest, ResumeRequest as ProviderResumeRequest, RetireCheckpointRequest,
        RetireCheckpointResult, RetireSuspensionRequest, RetireSuspensionResult, StopRequest,
        StoppedLease, SuspendRequest, SuspensionSubmission,
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

    fn assert_sandbox_management_shape(value: &serde_json::Value) {
        let mut actual = value
            .as_object()
            .expect("sandbox response object")
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let mut expected = vec![
            "backend",
            "backend_ownership",
            "created_at",
            "id",
            "image_digest",
            "last_checkpoint",
            "operation",
            "policy_name",
            "start_path",
            "state",
            "updated_at",
            "workload_class",
        ];
        actual.sort_unstable();
        expected.sort_unstable();
        assert_eq!(actual, expected, "management API sandbox fields changed");
    }

    fn assert_operation_management_shape(value: &serde_json::Value) {
        let mut actual = value
            .as_object()
            .expect("operation response object")
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let mut expected = vec!["checkpoint_id", "kind", "phase", "started_at"];
        actual.sort_unstable();
        expected.sort_unstable();
        assert_eq!(actual, expected, "management API operation fields changed");
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

    fn uuid_directory_count(root: &Path) -> usize {
        std::fs::read_dir(root)
            .expect("directory")
            .filter_map(std::result::Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| Uuid::parse_str(name).is_ok())
            })
            .count()
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

    fn build_test_state_with_provider(
        config: DaemonConfig,
        policy: PolicyFile,
        registry: SpawnerRegistry,
        active_backend: BackendKind,
        storage: Arc<dyn StorageProvider>,
        data_plane: Arc<dyn DataPlaneProvider>,
    ) -> Arc<ServerState> {
        Arc::new(
            ServerState::build_with_provider(
                config,
                PolicyEngine::with_policies(vec![policy]),
                HookRegistry::new(),
                registry,
                active_backend,
                storage,
                data_plane,
            )
            .expect("state"),
        )
    }

    /// Materialize the same durable file-provider ownership that a successful
    /// create transaction would publish before a restart test takes over the
    /// lifecycle record. Tests that call `StorageProvider::acquire` directly
    /// bypass the ownership ledger and therefore no longer describe a resource
    /// that startup is allowed to delete.
    async fn attach_finalized_file_lease(
        storage: Arc<dyn StorageProvider>,
        state_dir: &Path,
        instance: &mut SandboxInstance,
        root_filesystem_bytes: u64,
        guest_memory_bytes: u64,
    ) {
        let provider = crate::data_plane::FileDataPlaneProvider::new(storage);
        let context = RequestContext {
            instance_id: instance.id,
            request_id: Uuid::new_v4(),
            operation_id: Uuid::new_v4(),
            lease_id: Uuid::new_v4(),
            generation: 1,
        };
        let prepared = provider
            .prepare(PrepareRequest {
                context,
                source: PrepareSource::Image {
                    image_digest: instance.image_digest.clone(),
                },
                root_filesystem_bytes,
                guest_memory_bytes,
            })
            .await
            .expect("prepare provider-owned test storage");
        let committed = provider
            .commit(CommitRequest {
                binding: prepared.binding,
            })
            .await
            .expect("commit provider-owned test storage");
        instance.data_plane_lease = Some(
            committed
                .binding
                .to_record(root_filesystem_bytes, guest_memory_bytes),
        );
        instance
            .persist(state_dir)
            .expect("persist committed public owner");
        let finalized = provider
            .finalize(FinalizeRequest {
                binding: committed.binding,
                public_transition: PublicTransitionRef {
                    instance_id: instance.id,
                    operation_id: context.operation_id,
                },
            })
            .await
            .expect("finalize provider-owned test storage");
        instance.data_plane_lease = Some(
            finalized
                .binding
                .to_record(root_filesystem_bytes, guest_memory_bytes),
        );
        instance
            .persist(state_dir)
            .expect("persist finalized public owner");
    }

    struct ManagedStorageToggleProvider {
        inner: crate::data_plane::FileDataPlaneProvider,
        images: AtomicBool,
        daemon_managed_storage: AtomicBool,
        opened_template_restore_resources: AtomicBool,
        prepare_calls: AtomicUsize,
    }

    impl ManagedStorageToggleProvider {
        fn new(storage: Arc<dyn StorageProvider>) -> Self {
            Self {
                inner: crate::data_plane::FileDataPlaneProvider::new(storage),
                images: AtomicBool::new(true),
                daemon_managed_storage: AtomicBool::new(true),
                opened_template_restore_resources: AtomicBool::new(false),
                prepare_calls: AtomicUsize::new(0),
            }
        }

        fn set_daemon_managed_storage(&self, enabled: bool) {
            self.daemon_managed_storage
                .store(enabled, Ordering::Release);
        }

        fn set_images(&self, enabled: bool) {
            self.images.store(enabled, Ordering::Release);
        }

        fn set_opened_restore_resources(&self, enabled: bool) {
            self.opened_template_restore_resources
                .store(enabled, Ordering::Release);
        }

        fn prepare_calls(&self) -> usize {
            self.prepare_calls.load(Ordering::Acquire)
        }
    }

    #[async_trait]
    impl DataPlaneProvider for ManagedStorageToggleProvider {
        fn descriptor(&self) -> ProviderDescriptor {
            self.inner.descriptor()
        }

        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                images: self.images.load(Ordering::Acquire),
                daemon_managed_storage: self.daemon_managed_storage.load(Ordering::Acquire),
                opened_template_restore_resources: self
                    .opened_template_restore_resources
                    .load(Ordering::Acquire),
                ..self.inner.capabilities()
            }
        }

        async fn probe(&self) -> std::result::Result<(), ProviderError> {
            self.inner.probe().await
        }

        async fn prepare(
            &self,
            request: PrepareRequest,
        ) -> std::result::Result<PreparedLease, ProviderError> {
            self.prepare_calls.fetch_add(1, Ordering::AcqRel);
            self.inner.prepare(request).await
        }

        async fn inspect(
            &self,
            request: InspectRequest,
        ) -> std::result::Result<ObservedLease, ProviderError> {
            self.inner.inspect(request).await
        }

        async fn commit(
            &self,
            request: CommitRequest,
        ) -> std::result::Result<CommittedLease, ProviderError> {
            self.inner.commit(request).await
        }

        async fn finalize(
            &self,
            request: FinalizeRequest,
        ) -> std::result::Result<FinalizedLease, ProviderError> {
            self.inner.finalize(request).await
        }

        async fn abort(
            &self,
            request: AbortRequest,
        ) -> std::result::Result<AbortResult, ProviderError> {
            self.inner.abort(request).await
        }

        async fn stop(
            &self,
            request: StopRequest,
        ) -> std::result::Result<StoppedLease, ProviderError> {
            self.inner.stop(request).await
        }

        async fn release(
            &self,
            request: ReleaseRequest,
        ) -> std::result::Result<ReleaseResult, ProviderError> {
            self.inner.release(request).await
        }
    }

    struct CapacityTestProvider {
        inner: crate::data_plane::FileDataPlaneProvider,
        capacity: std::sync::Mutex<CapacitySnapshot>,
        drains: std::sync::Mutex<HashMap<Uuid, DrainResult>>,
        response_error: std::sync::Mutex<Option<ProviderError>>,
        drain_response: std::sync::Mutex<Option<DrainResult>>,
        drain_calls: AtomicUsize,
    }

    impl CapacityTestProvider {
        fn new(storage: Arc<dyn StorageProvider>) -> Self {
            let inner = crate::data_plane::FileDataPlaneProvider::new(storage);
            let provider_instance_id = inner.descriptor().provider_instance_id;
            let class = CapacityClass {
                root_filesystem_capacity_bytes: 4 * 1024 * 1024 * 1024,
                guest_memory_capacity_bytes: 512 * 1024 * 1024,
            };
            Self {
                inner,
                capacity: std::sync::Mutex::new(CapacitySnapshot {
                    provider_instance_id,
                    scope: CapacityScope {
                        backend: BackendKind::Mock,
                        class_digest: class.digest(),
                    },
                    class,
                    revision: 1,
                    ready: 3,
                    building: 1,
                    in_use: 2,
                    draining: 0,
                    quarantined: 1,
                    accepting_allocations: true,
                }),
                drains: std::sync::Mutex::new(HashMap::new()),
                response_error: std::sync::Mutex::new(None),
                drain_response: std::sync::Mutex::new(None),
                drain_calls: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl DataPlaneProvider for CapacityTestProvider {
        fn descriptor(&self) -> ProviderDescriptor {
            self.inner.descriptor()
        }

        fn capabilities(&self) -> ProviderCapabilities {
            self.inner.capabilities()
        }

        fn capacity_control(&self) -> Option<&dyn DataPlaneCapacity> {
            Some(self)
        }

        async fn probe(&self) -> std::result::Result<(), ProviderError> {
            self.inner.probe().await
        }

        async fn prepare(
            &self,
            request: PrepareRequest,
        ) -> std::result::Result<PreparedLease, ProviderError> {
            self.inner.prepare(request).await
        }

        async fn inspect(
            &self,
            request: InspectRequest,
        ) -> std::result::Result<ObservedLease, ProviderError> {
            self.inner.inspect(request).await
        }

        async fn commit(
            &self,
            request: CommitRequest,
        ) -> std::result::Result<CommittedLease, ProviderError> {
            self.inner.commit(request).await
        }

        async fn finalize(
            &self,
            request: FinalizeRequest,
        ) -> std::result::Result<FinalizedLease, ProviderError> {
            self.inner.finalize(request).await
        }

        async fn abort(
            &self,
            request: AbortRequest,
        ) -> std::result::Result<AbortResult, ProviderError> {
            self.inner.abort(request).await
        }

        async fn stop(
            &self,
            request: StopRequest,
        ) -> std::result::Result<StoppedLease, ProviderError> {
            self.inner.stop(request).await
        }

        async fn release(
            &self,
            request: ReleaseRequest,
        ) -> std::result::Result<ReleaseResult, ProviderError> {
            self.inner.release(request).await
        }
    }

    #[async_trait]
    impl DataPlaneCapacity for CapacityTestProvider {
        async fn capacity(
            &self,
            request: CapacityRequest,
        ) -> std::result::Result<CapacitySnapshot, ProviderError> {
            if let Some(error) = *self.response_error.lock().expect("response error") {
                return Err(error);
            }
            let snapshot = *self.capacity.lock().expect("provider capacity");
            if snapshot.scope != request.scope {
                return Err(ProviderError::NotFound);
            }
            Ok(snapshot)
        }

        async fn drain(
            &self,
            request: DrainRequest,
        ) -> std::result::Result<DrainResult, ProviderError> {
            self.drain_calls.fetch_add(1, Ordering::AcqRel);
            if let Some(error) = *self.response_error.lock().expect("response error") {
                return Err(error);
            }
            if let Some(response) = *self.drain_response.lock().expect("drain response") {
                return Ok(response);
            }
            if request.operation_id.is_nil() {
                return Err(ProviderError::Conflict);
            }
            if let Some(result) = self
                .drains
                .lock()
                .expect("provider drain results")
                .get(&request.operation_id)
                .copied()
            {
                return Ok(result);
            }

            let mut snapshot = self.capacity.lock().expect("provider capacity");
            if snapshot.scope != request.scope {
                return Err(ProviderError::NotFound);
            }
            let removed_ready = snapshot.ready;
            let deferred_in_use = snapshot.in_use;
            snapshot.revision = snapshot
                .revision
                .checked_add(1)
                .ok_or(ProviderError::InvalidResponse)?;
            snapshot.draining = snapshot
                .draining
                .checked_add(snapshot.building)
                .and_then(|count| count.checked_add(snapshot.in_use))
                .ok_or(ProviderError::InvalidResponse)?;
            snapshot.ready = 0;
            snapshot.building = 0;
            snapshot.in_use = 0;
            snapshot.accepting_allocations = false;
            let result = DrainResult {
                operation_id: request.operation_id,
                removed_ready,
                deferred_in_use,
                snapshot: *snapshot,
            };
            self.drains
                .lock()
                .expect("provider drain results")
                .insert(request.operation_id, result);
            Ok(result)
        }
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
            .expect("write rootfs fixture");
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
                        provider_checkpoint: None,
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

        async fn reserve_ownership(
            &self,
            request: StorageOwnershipRequest,
        ) -> blaze_core::Result<StorageOwnershipClaim> {
            self.inner.reserve_ownership(request).await
        }

        async fn publish_ownership(
            &self,
            slot: &StorageSlot,
            request: StorageOwnershipRequest,
        ) -> blaze_core::Result<StorageOwnershipClaim> {
            self.inner.publish_ownership(slot, request).await
        }

        async fn reconstruct_owned(
            &self,
            key: StorageOwnershipKey,
        ) -> blaze_core::Result<Option<OwnedStorageSlot>> {
            self.inner.reconstruct_owned(key).await
        }

        async fn advance_ownership(
            &self,
            key: StorageOwnershipKey,
            expected_state: DataPlaneLeaseState,
            expected_generation: u64,
            next_state: DataPlaneLeaseState,
            next_generation: u64,
        ) -> blaze_core::Result<StorageOwnershipClaim> {
            self.inner
                .advance_ownership(
                    key,
                    expected_state,
                    expected_generation,
                    next_state,
                    next_generation,
                )
                .await
        }

        async fn release_owned(
            &self,
            key: StorageOwnershipKey,
            expected_state: DataPlaneLeaseState,
            expected_generation: u64,
        ) -> blaze_core::Result<bool> {
            self.inner
                .release_owned(key, expected_state, expected_generation)
                .await
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

        async fn reserve_ownership(
            &self,
            request: StorageOwnershipRequest,
        ) -> blaze_core::Result<StorageOwnershipClaim> {
            self.inner.reserve_ownership(request).await
        }

        async fn publish_ownership(
            &self,
            slot: &StorageSlot,
            request: StorageOwnershipRequest,
        ) -> blaze_core::Result<StorageOwnershipClaim> {
            self.inner.publish_ownership(slot, request).await
        }

        async fn reconstruct_owned(
            &self,
            key: StorageOwnershipKey,
        ) -> blaze_core::Result<Option<OwnedStorageSlot>> {
            self.inner.reconstruct_owned(key).await
        }

        async fn advance_ownership(
            &self,
            key: StorageOwnershipKey,
            expected_state: DataPlaneLeaseState,
            expected_generation: u64,
            next_state: DataPlaneLeaseState,
            next_generation: u64,
        ) -> blaze_core::Result<StorageOwnershipClaim> {
            self.inner
                .advance_ownership(
                    key,
                    expected_state,
                    expected_generation,
                    next_state,
                    next_generation,
                )
                .await
        }

        async fn release_owned(
            &self,
            key: StorageOwnershipKey,
            expected_state: DataPlaneLeaseState,
            expected_generation: u64,
        ) -> blaze_core::Result<bool> {
            self.inner
                .release_owned(key, expected_state, expected_generation)
                .await
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

    struct AdoptableOwner {
        instance_id: Uuid,
        process: BackendProcessIdentity,
        running: AtomicBool,
    }

    #[async_trait]
    impl BackendInstance for AdoptableOwner {
        fn instance_id(&self) -> Uuid {
            self.instance_id
        }

        fn backend(&self) -> BackendKind {
            BackendKind::Mock
        }

        fn version(&self) -> Option<&str> {
            Some("adoptable-mock-v1")
        }

        fn runtime_record(&self) -> BackendRuntimeRecord {
            BackendRuntimeRecord {
                process: Some(self.process),
                version: self.version().map(str::to_owned),
                guest_transport: false,
                network_slot: false,
                console_log: false,
            }
        }

        async fn try_wait(&self) -> blaze_core::Result<Option<SpawnResult>> {
            Ok(
                (!self.running.load(Ordering::Acquire)).then_some(SpawnResult {
                    instance_id: self.instance_id,
                    exit_code: Some(0),
                    signal: None,
                }),
            )
        }

        async fn kill(&self) -> blaze_core::Result<()> {
            self.running.store(false, Ordering::Release);
            Ok(())
        }
    }

    #[derive(Default)]
    struct AdoptableSpawner {
        owners: std::sync::Mutex<HashMap<Uuid, DynBackendInstance>>,
    }

    #[async_trait]
    impl BackendSpawner for AdoptableSpawner {
        async fn spawn(
            &self,
            request: BackendSpawnRequest,
        ) -> std::result::Result<DynBackendInstance, SpawnFailure> {
            let pid = request.instance_id.as_u128() as u32 | 1;
            let owner: DynBackendInstance = Arc::new(AdoptableOwner {
                instance_id: request.instance_id,
                process: BackendProcessIdentity {
                    pid,
                    start_time_ticks: 17,
                },
                running: AtomicBool::new(true),
            });
            self.owners
                .lock()
                .expect("adoptable owners")
                .insert(request.instance_id, owner.clone());
            Ok(owner)
        }

        async fn adopt(
            &self,
            instance_id: Uuid,
            runtime: &BackendRuntimeRecord,
            _run_dir: OwnedRunDir,
            _guest_memory_bytes: u64,
        ) -> blaze_core::Result<Option<DynBackendInstance>> {
            let owner = self
                .owners
                .lock()
                .expect("adoptable owners")
                .get(&instance_id)
                .cloned();
            if owner
                .as_ref()
                .is_some_and(|owner| owner.runtime_record() != *runtime)
            {
                return Err(BlazeError::BackendError {
                    msg: "durable mock identity changed".to_string(),
                });
            }
            Ok(owner)
        }

        async fn probe(&self, _binary_path: &Path) -> blaze_core::Result<bool> {
            Ok(true)
        }

        async fn cleanup_orphan(
            &self,
            instance_id: Uuid,
            _run_dir: &OwnedRunDir,
        ) -> blaze_core::Result<()> {
            let owner = self
                .owners
                .lock()
                .expect("adoptable owners")
                .remove(&instance_id);
            if let Some(owner) = owner {
                owner.kill().await?;
            }
            Ok(())
        }
    }

    struct InventoryTestProvider {
        inner: crate::data_plane::FileDataPlaneProvider,
        snapshot_id: Uuid,
        inventory_enabled: AtomicBool,
        prepared_response_fault: std::sync::Mutex<Option<unsafe_prepare::PreparedResponseFault>>,
        absent_prepare_context: std::sync::Mutex<Option<RequestContext>>,
        unsafe_inspect_calls: AtomicUsize,
        abort_calls: AtomicUsize,
        bindings: std::sync::Mutex<HashMap<Uuid, LeaseBinding>>,
        checkpoints: std::sync::Mutex<HashMap<Uuid, ProviderCheckpointRef>>,
        suspensions: std::sync::Mutex<HashMap<Uuid, ProviderSuspensionRef>>,
        extents: std::sync::Mutex<HashMap<Uuid, (u64, u64)>>,
        opened_checkpoint_restore_resources: AtomicBool,
        opened_suspension_restore_resources: AtomicBool,
        restore_checkpoint_calls: AtomicUsize,
        resume_calls: AtomicUsize,
        reject_suspension_retirement: AtomicBool,
        suspension_retirement_calls: AtomicUsize,
        suspension_public_owner_path: std::sync::Mutex<Option<PathBuf>>,
        retired_suspension_while_public_owner_existed: AtomicBool,
        capture_fault: std::sync::Mutex<Option<provider_contract::CaptureFault>>,
        checkpoint_retirement_fault: std::sync::Mutex<Option<provider_contract::ReceiptFault>>,
        suspension_retirement_fault: std::sync::Mutex<Option<provider_contract::ReceiptFault>>,
        opened_file_identities: std::sync::Mutex<HashMap<Uuid, Vec<(u64, u64)>>>,
    }

    impl InventoryTestProvider {
        fn new(storage: Arc<dyn StorageProvider>) -> Self {
            Self {
                inner: crate::data_plane::FileDataPlaneProvider::new(storage),
                snapshot_id: Uuid::new_v4(),
                inventory_enabled: AtomicBool::new(true),
                prepared_response_fault: std::sync::Mutex::new(None),
                absent_prepare_context: std::sync::Mutex::new(None),
                unsafe_inspect_calls: AtomicUsize::new(0),
                abort_calls: AtomicUsize::new(0),
                bindings: std::sync::Mutex::new(HashMap::new()),
                checkpoints: std::sync::Mutex::new(HashMap::new()),
                suspensions: std::sync::Mutex::new(HashMap::new()),
                extents: std::sync::Mutex::new(HashMap::new()),
                opened_checkpoint_restore_resources: AtomicBool::new(false),
                opened_suspension_restore_resources: AtomicBool::new(false),
                restore_checkpoint_calls: AtomicUsize::new(0),
                resume_calls: AtomicUsize::new(0),
                reject_suspension_retirement: AtomicBool::new(false),
                suspension_retirement_calls: AtomicUsize::new(0),
                suspension_public_owner_path: std::sync::Mutex::new(None),
                retired_suspension_while_public_owner_existed: AtomicBool::new(false),
                capture_fault: std::sync::Mutex::new(None),
                checkpoint_retirement_fault: std::sync::Mutex::new(None),
                suspension_retirement_fault: std::sync::Mutex::new(None),
                opened_file_identities: std::sync::Mutex::new(HashMap::new()),
            }
        }

        fn record(&self, binding: LeaseBinding) {
            self.bindings
                .lock()
                .expect("inventory bindings")
                .insert(binding.context.lease_id, binding);
        }

        fn binding(&self, lease_id: Uuid) -> Option<LeaseBinding> {
            self.bindings
                .lock()
                .expect("inventory bindings")
                .get(&lease_id)
                .copied()
        }

        fn checkpoint_count(&self) -> usize {
            self.checkpoints.lock().expect("provider checkpoints").len()
        }

        fn suspension_count(&self) -> usize {
            self.suspensions.lock().expect("provider suspensions").len()
        }

        fn advertise_opened_checkpoint_restore_resources(&self) {
            self.opened_checkpoint_restore_resources
                .store(true, Ordering::Release);
        }

        fn advertise_opened_suspension_restore_resources(&self) {
            self.opened_suspension_restore_resources
                .store(true, Ordering::Release);
        }

        fn restore_checkpoint_calls(&self) -> usize {
            self.restore_checkpoint_calls.load(Ordering::Acquire)
        }

        fn resume_calls(&self) -> usize {
            self.resume_calls.load(Ordering::Acquire)
        }

        fn observe_suspension_public_owner(&self, path: PathBuf) {
            *self
                .suspension_public_owner_path
                .lock()
                .expect("suspension public owner path") = Some(path);
        }

        fn reject_suspension_retirement(&self, reject: bool) {
            self.reject_suspension_retirement
                .store(reject, Ordering::Release);
        }

        fn transition(
            &self,
            binding: LeaseBinding,
            state: LeaseState,
            remove: bool,
        ) -> std::result::Result<LeaseBinding, ProviderError> {
            let mut bindings = self.bindings.lock().expect("inventory bindings");
            if bindings.get(&binding.context.lease_id) != Some(&binding) {
                return Err(ProviderError::Conflict);
            }
            let next = LeaseBinding {
                generation: binding.generation + 1,
                state,
                ..binding
            };
            if remove {
                bindings.remove(&binding.context.lease_id);
            } else {
                bindings.insert(binding.context.lease_id, next);
            }
            Ok(next)
        }
    }

    #[async_trait]
    impl DataPlaneProvider for InventoryTestProvider {
        fn descriptor(&self) -> ProviderDescriptor {
            self.inner.descriptor()
        }

        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                opened_checkpoint_restore_resources: self
                    .opened_checkpoint_restore_resources
                    .load(Ordering::Acquire),
                opened_suspension_restore_resources: self
                    .opened_suspension_restore_resources
                    .load(Ordering::Acquire),
                ..self.inner.capabilities()
            }
        }

        fn inventory(&self) -> Option<&dyn DataPlaneInventory> {
            self.inventory_enabled
                .load(Ordering::Acquire)
                .then_some(self)
        }

        fn checkpoints(&self) -> Option<&dyn DataPlaneCheckpoint> {
            Some(self)
        }

        fn suspension(&self) -> Option<&dyn DataPlaneSuspend> {
            Some(self)
        }

        async fn probe(&self) -> std::result::Result<(), ProviderError> {
            self.inner.probe().await
        }

        async fn prepare(
            &self,
            request: PrepareRequest,
        ) -> std::result::Result<PreparedLease, ProviderError> {
            let lease_id = request.context.lease_id;
            let extents = (request.root_filesystem_bytes, request.guest_memory_bytes);
            let prepared = self.inner.prepare(request).await?;
            self.record(prepared.binding);
            self.extents
                .lock()
                .expect("provider extents")
                .insert(lease_id, extents);
            Ok(self.inject_prepared_response_fault(prepared))
        }

        async fn inspect(
            &self,
            request: InspectRequest,
        ) -> std::result::Result<ObservedLease, ProviderError> {
            if *self.absent_prepare_context.lock().expect("absent context") == Some(request.context)
            {
                self.unsafe_inspect_calls.fetch_add(1, Ordering::AcqRel);
                return Err(ProviderError::NotFound);
            }
            let Some(binding) = self.binding(request.context.lease_id) else {
                return Err(ProviderError::NotFound);
            };
            if binding.context != request.context {
                return Err(ProviderError::Conflict);
            }
            Ok(ObservedLease { binding })
        }

        async fn commit(
            &self,
            request: CommitRequest,
        ) -> std::result::Result<CommittedLease, ProviderError> {
            Ok(CommittedLease {
                binding: self.transition(request.binding, LeaseState::Committed, false)?,
            })
        }

        async fn finalize(
            &self,
            request: FinalizeRequest,
        ) -> std::result::Result<FinalizedLease, ProviderError> {
            Ok(FinalizedLease {
                binding: self.transition(request.binding, LeaseState::Finalized, false)?,
            })
        }

        async fn abort(
            &self,
            request: AbortRequest,
        ) -> std::result::Result<AbortResult, ProviderError> {
            self.abort_calls.fetch_add(1, Ordering::AcqRel);
            let binding = self.transition(request.binding, LeaseState::Released, true)?;
            self.extents
                .lock()
                .expect("provider extents")
                .remove(&binding.context.lease_id);
            Ok(AbortResult { binding })
        }

        async fn stop(
            &self,
            request: StopRequest,
        ) -> std::result::Result<StoppedLease, ProviderError> {
            Ok(StoppedLease {
                binding: self.transition(request.binding, LeaseState::Stopped, false)?,
            })
        }

        async fn release(
            &self,
            request: ReleaseRequest,
        ) -> std::result::Result<ReleaseResult, ProviderError> {
            let binding = self.transition(request.binding, LeaseState::Released, true)?;
            self.extents
                .lock()
                .expect("provider extents")
                .remove(&binding.context.lease_id);
            Ok(ReleaseResult { binding })
        }
    }

    #[async_trait]
    impl DataPlaneInventory for InventoryTestProvider {
        async fn begin_inventory(
            &self,
            _request: BeginInventoryRequest,
        ) -> std::result::Result<InventorySnapshot, ProviderError> {
            Ok(InventorySnapshot {
                provider_instance_id: self.descriptor().provider_instance_id,
                snapshot_id: self.snapshot_id,
            })
        }

        async fn inventory_page(
            &self,
            request: InventoryPageRequest,
        ) -> std::result::Result<InventoryPage, ProviderError> {
            if request.snapshot_id != self.snapshot_id || request.cursor.is_some() {
                return Err(ProviderError::Conflict);
            }
            let leases = self
                .bindings
                .lock()
                .expect("inventory bindings")
                .values()
                .copied()
                .map(|binding| InventoryLease { binding })
                .collect();
            Ok(InventoryPage {
                leases,
                next_cursor: None,
            })
        }

        async fn reconcile(
            &self,
            request: ReconcileRequest,
        ) -> std::result::Result<ReconcileResult, ProviderError> {
            let mut bindings = self.bindings.lock().expect("inventory bindings");
            if bindings.get(&request.observed.context.lease_id) != Some(&request.observed) {
                return Err(ProviderError::Conflict);
            }
            if matches!(request.action, ReconcileAction::Adopt { .. })
                && request.expected != Some(request.observed)
            {
                return Err(ProviderError::Conflict);
            }
            let state = match request.action {
                ReconcileAction::Adopt { .. } => LeaseState::Finalized,
                ReconcileAction::Quarantine => LeaseState::Quarantined,
            };
            let binding = LeaseBinding {
                generation: request.observed.generation + 1,
                state,
                ..request.observed
            };
            if state == LeaseState::Released {
                bindings.remove(&binding.context.lease_id);
            } else {
                bindings.insert(binding.context.lease_id, binding);
            }
            Ok(ReconcileResult { binding })
        }
    }

    #[async_trait]
    impl DataPlaneCheckpoint for InventoryTestProvider {
        async fn checkpoint(
            &self,
            request: ProviderCheckpointRequest,
        ) -> std::result::Result<CheckpointSubmission, ProviderError> {
            if request.binding.state != LeaseState::Finalized
                || self.binding(request.binding.context.lease_id) != Some(request.binding)
                || request.parent.as_ref().is_some_and(|parent| {
                    parent.provider_instance_id != request.binding.provider_instance_id
                })
            {
                return Err(ProviderError::Conflict);
            }
            let binding = self.transition(request.binding, LeaseState::Finalized, false)?;
            let checkpoint = ProviderCheckpointRef {
                provider_instance_id: binding.provider_instance_id,
                public_checkpoint_id: request.checkpoint_id,
                reference_id: Uuid::new_v4(),
                content_digest: format!(
                    "sha256:{:x}",
                    Sha256::digest(request.checkpoint_id.as_bytes())
                ),
                parent_reference_id: request.parent.map(|parent| parent.reference_id),
                source_lease_id: binding.context.lease_id,
                source_generation: binding.generation,
            };
            self.checkpoints
                .lock()
                .expect("provider checkpoints")
                .insert(checkpoint.public_checkpoint_id, checkpoint.clone());
            let mut checkpoint = checkpoint;
            if let Some(fault) = *self.capture_fault.lock().expect("capture fault") {
                fault.apply(&mut checkpoint);
            }
            Ok(CheckpointSubmission {
                binding,
                checkpoint,
            })
        }

        async fn restore_checkpoint(
            &self,
            request: RestoreCheckpointRequest,
        ) -> std::result::Result<PreparedLease, ProviderError> {
            self.restore_checkpoint_calls.fetch_add(1, Ordering::AcqRel);
            if self
                .checkpoints
                .lock()
                .expect("provider checkpoints")
                .get(&request.checkpoint.public_checkpoint_id)
                != Some(&request.checkpoint)
            {
                return Err(ProviderError::Conflict);
            }
            let binding = LeaseBinding {
                provider_instance_id: self.descriptor().provider_instance_id,
                context: request.context,
                generation: request.context.generation,
                state: LeaseState::Prepared,
            };
            self.record(binding);
            self.extents.lock().expect("provider extents").insert(
                binding.context.lease_id,
                (request.root_filesystem_bytes, request.guest_memory_bytes),
            );
            let id = request.context.instance_id.to_string();
            Ok(self.inject_prepared_response_fault(PreparedLease {
                binding,
                resources: if self
                    .opened_checkpoint_restore_resources
                    .load(Ordering::Acquire)
                {
                    PreparedResources::CheckpointRestore {
                        storage: None,
                        attachments: self.opened_resources(
                            binding,
                            request.root_filesystem_bytes,
                            request.guest_memory_bytes,
                        ),
                    }
                } else {
                    PreparedResources::CheckpointRestore {
                        storage: Some(StorageSlot {
                            id,
                            rootfs_path: PathBuf::new(),
                            mem_path: PathBuf::new(),
                            mem_diff_path: PathBuf::new(),
                            rootfs_diff_path: PathBuf::new(),
                            instance_dir: PathBuf::new(),
                        }),
                        attachments: Vec::new(),
                    }
                },
            }))
        }

        async fn retire_checkpoint(
            &self,
            request: RetireCheckpointRequest,
        ) -> std::result::Result<RetireCheckpointResult, ProviderError> {
            if let Some(fault) = *self
                .checkpoint_retirement_fault
                .lock()
                .expect("retirement fault")
            {
                let (public_checkpoint_id, reference_id) =
                    fault.identities(request.public_checkpoint_id, request.reference_id);
                return Ok(RetireCheckpointResult {
                    public_checkpoint_id,
                    reference_id,
                    retired: true,
                });
            }
            if request.provider_instance_id != self.descriptor().provider_instance_id
                || request.public_checkpoint_id.is_nil()
                || request
                    .reference_id
                    .is_some_and(|reference_id| reference_id.is_nil())
                || request.operation_id.is_nil()
            {
                return Err(ProviderError::Conflict);
            }
            let mut checkpoints = self.checkpoints.lock().expect("provider checkpoints");
            if let (Some(expected), Some(actual)) = (
                request.reference_id,
                checkpoints.get(&request.public_checkpoint_id),
            ) && expected != actual.reference_id
            {
                return Err(ProviderError::Conflict);
            }
            let retired = checkpoints.remove(&request.public_checkpoint_id);
            Ok(RetireCheckpointResult {
                public_checkpoint_id: request.public_checkpoint_id,
                reference_id: request.reference_id,
                retired: retired.is_some(),
            })
        }
    }

    #[async_trait]
    impl DataPlaneSuspend for InventoryTestProvider {
        async fn suspend(
            &self,
            request: SuspendRequest,
        ) -> std::result::Result<SuspensionSubmission, ProviderError> {
            if request.binding.state != LeaseState::Finalized
                || self.binding(request.binding.context.lease_id) != Some(request.binding)
                || request.suspension_id.is_nil()
                || self
                    .extents
                    .lock()
                    .expect("provider extents")
                    .get(&request.binding.context.lease_id)
                    != Some(&(request.root_filesystem_bytes, request.guest_memory_bytes))
            {
                return Err(ProviderError::Conflict);
            }
            let binding = self.transition(request.binding, LeaseState::Finalized, false)?;
            let suspension = ProviderSuspensionRef {
                provider_instance_id: binding.provider_instance_id,
                suspension_id: request.suspension_id,
                reference_id: Uuid::new_v4(),
                content_digest: format!(
                    "sha256:{:x}",
                    Sha256::digest(request.suspension_id.as_bytes())
                ),
                source_lease_id: binding.context.lease_id,
                source_generation: binding.generation,
                root_filesystem_bytes: request.root_filesystem_bytes,
                guest_memory_bytes: request.guest_memory_bytes,
            };
            self.suspensions
                .lock()
                .expect("provider suspensions")
                .insert(request.suspension_id, suspension.clone());
            Ok(SuspensionSubmission {
                binding,
                suspension,
            })
        }

        async fn resume(
            &self,
            request: ProviderResumeRequest,
        ) -> std::result::Result<PreparedLease, ProviderError> {
            self.resume_calls.fetch_add(1, Ordering::AcqRel);
            if self
                .suspensions
                .lock()
                .expect("provider suspensions")
                .get(&request.suspension.suspension_id)
                != Some(&request.suspension)
                || request.root_filesystem_bytes != request.suspension.root_filesystem_bytes
                || request.guest_memory_bytes != request.suspension.guest_memory_bytes
            {
                return Err(ProviderError::Conflict);
            }
            let binding = LeaseBinding {
                provider_instance_id: self.descriptor().provider_instance_id,
                context: request.context,
                generation: request.context.generation,
                state: LeaseState::Prepared,
            };
            self.record(binding);
            self.extents.lock().expect("provider extents").insert(
                binding.context.lease_id,
                (request.root_filesystem_bytes, request.guest_memory_bytes),
            );
            Ok(self.inject_prepared_response_fault(PreparedLease {
                binding,
                resources: if self
                    .opened_suspension_restore_resources
                    .load(Ordering::Acquire)
                {
                    PreparedResources::SuspensionRestore {
                        storage: None,
                        attachments: self.opened_resources(
                            binding,
                            request.root_filesystem_bytes,
                            request.guest_memory_bytes,
                        ),
                    }
                } else {
                    PreparedResources::SuspensionRestore {
                        storage: Some(StorageSlot {
                            id: request.context.instance_id.to_string(),
                            rootfs_path: PathBuf::new(),
                            mem_path: PathBuf::new(),
                            mem_diff_path: PathBuf::new(),
                            rootfs_diff_path: PathBuf::new(),
                            instance_dir: PathBuf::new(),
                        }),
                        attachments: Vec::new(),
                    }
                },
            }))
        }

        async fn retire_suspension(
            &self,
            request: RetireSuspensionRequest,
        ) -> std::result::Result<RetireSuspensionResult, ProviderError> {
            if let Some(fault) = *self
                .suspension_retirement_fault
                .lock()
                .expect("retirement fault")
            {
                let (suspension_id, reference_id) =
                    fault.identities(request.suspension_id, request.reference_id);
                return Ok(RetireSuspensionResult {
                    suspension_id,
                    reference_id,
                    retired: true,
                });
            }
            self.suspension_retirement_calls
                .fetch_add(1, Ordering::AcqRel);
            if self
                .suspension_public_owner_path
                .lock()
                .map_err(|_| ProviderError::OutcomeUnknown)?
                .as_ref()
                .is_some_and(|path| path.exists())
            {
                self.retired_suspension_while_public_owner_existed
                    .store(true, Ordering::Release);
            }
            if self.reject_suspension_retirement.load(Ordering::Acquire) {
                return Err(ProviderError::OutcomeUnknown);
            }
            if request.provider_instance_id != self.descriptor().provider_instance_id
                || request.suspension_id.is_nil()
                || request
                    .reference_id
                    .is_some_and(|reference_id| reference_id.is_nil())
                || request.operation_id.is_nil()
            {
                return Err(ProviderError::Conflict);
            }
            let mut suspensions = self.suspensions.lock().expect("provider suspensions");
            if let (Some(expected), Some(actual)) = (
                request.reference_id,
                suspensions.get(&request.suspension_id),
            ) && expected != actual.reference_id
            {
                return Err(ProviderError::Conflict);
            }
            let retired = suspensions.remove(&request.suspension_id);
            Ok(RetireSuspensionResult {
                suspension_id: request.suspension_id,
                reference_id: request.reference_id,
                retired: retired.is_some(),
            })
        }
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
                consumes_typed_opened_attachments: false,
            }))
        }

        async fn restore(
            &self,
            request: crate::spawner::BackendRestoreRequest,
        ) -> crate::spawner::RestoreResult {
            if request.provider_attachments.is_some() {
                return Err(SpawnFailure::clean(BlazeError::BackendError {
                    msg: "transport-dropping restore does not consume typed opened attachments"
                        .to_string(),
                }));
            }
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

    struct KillGateOwner {
        inner: DynBackendInstance,
        kill_allowed: Arc<AtomicBool>,
        kill_attempts: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl BackendInstance for KillGateOwner {
        fn instance_id(&self) -> Uuid {
            self.inner.instance_id()
        }

        fn backend(&self) -> BackendKind {
            self.inner.backend()
        }

        fn version(&self) -> Option<&str> {
            self.inner.version()
        }

        fn supports_checkpoint_capture(&self) -> bool {
            self.inner.supports_checkpoint_capture()
        }

        fn guest_socket_path(&self) -> &Path {
            self.inner.guest_socket_path()
        }

        fn holds_network_slot(&self) -> bool {
            self.inner.holds_network_slot()
        }

        fn records_console_log(&self) -> bool {
            self.inner.records_console_log()
        }

        fn runtime_record(&self) -> BackendRuntimeRecord {
            self.inner.runtime_record()
        }

        async fn try_wait(&self) -> blaze_core::Result<Option<SpawnResult>> {
            self.inner.try_wait().await
        }

        async fn pause(&self) -> blaze_core::Result<()> {
            self.inner.pause().await
        }

        async fn resume(&self) -> blaze_core::Result<()> {
            self.inner.resume().await
        }

        async fn quiesce_for_capture(&self) -> blaze_core::Result<()> {
            self.inner.quiesce_for_capture().await
        }

        async fn unquiesce_after_capture(&self) -> blaze_core::Result<()> {
            self.inner.unquiesce_after_capture().await
        }

        async fn snapshot(
            &self,
            request: blaze_core::backend::SnapshotRequest,
        ) -> blaze_core::Result<()> {
            self.inner.snapshot(request).await
        }

        async fn kill(&self) -> blaze_core::Result<()> {
            self.kill_attempts.fetch_add(1, Ordering::AcqRel);
            if !self.kill_allowed.load(Ordering::Acquire) {
                return Err(BlazeError::BackendError {
                    msg: format!("instance {} termination blocked", self.instance_id()),
                });
            }
            self.inner.kill().await
        }
    }

    struct KillGateSpawner {
        current_kill_allowed: Arc<AtomicBool>,
        current_kill_attempts: Arc<AtomicUsize>,
        replacement_kill_allowed: Arc<AtomicBool>,
        replacement_kill_attempts: Arc<AtomicUsize>,
    }

    impl KillGateSpawner {
        fn gate(
            owner: DynBackendInstance,
            kill_allowed: Arc<AtomicBool>,
            kill_attempts: Arc<AtomicUsize>,
        ) -> DynBackendInstance {
            Arc::new(KillGateOwner {
                inner: owner,
                kill_allowed,
                kill_attempts,
            })
        }
    }

    #[async_trait]
    impl BackendSpawner for KillGateSpawner {
        async fn spawn(
            &self,
            request: BackendSpawnRequest,
        ) -> std::result::Result<DynBackendInstance, SpawnFailure> {
            let owner = GuestMockSpawner.spawn(request).await?;
            Ok(Self::gate(
                owner,
                self.current_kill_allowed.clone(),
                self.current_kill_attempts.clone(),
            ))
        }

        async fn restore_capability(
            &self,
            executable: Option<&crate::spawner::PinnedExecutable>,
        ) -> blaze_core::Result<Option<blaze_core::backend::RestoreCapability>> {
            GuestMockSpawner.restore_capability(executable).await
        }

        async fn restore(
            &self,
            request: crate::spawner::BackendRestoreRequest,
        ) -> crate::spawner::RestoreResult {
            let owner = GuestMockSpawner.restore(request).await?;
            Ok(Self::gate(
                owner,
                self.replacement_kill_allowed.clone(),
                self.replacement_kill_attempts.clone(),
            ))
        }

        async fn probe(&self, binary_path: &Path) -> blaze_core::Result<bool> {
            GuestMockSpawner.probe(binary_path).await
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

        async fn reserve_ownership(
            &self,
            request: StorageOwnershipRequest,
        ) -> blaze_core::Result<StorageOwnershipClaim> {
            self.inner.reserve_ownership(request).await
        }

        async fn publish_ownership(
            &self,
            slot: &StorageSlot,
            request: StorageOwnershipRequest,
        ) -> blaze_core::Result<StorageOwnershipClaim> {
            self.inner.publish_ownership(slot, request).await
        }

        async fn reconstruct_owned(
            &self,
            key: StorageOwnershipKey,
        ) -> blaze_core::Result<Option<OwnedStorageSlot>> {
            self.inner.reconstruct_owned(key).await
        }

        async fn advance_ownership(
            &self,
            key: StorageOwnershipKey,
            expected_state: DataPlaneLeaseState,
            expected_generation: u64,
            next_state: DataPlaneLeaseState,
            next_generation: u64,
        ) -> blaze_core::Result<StorageOwnershipClaim> {
            self.inner
                .advance_ownership(
                    key,
                    expected_state,
                    expected_generation,
                    next_state,
                    next_generation,
                )
                .await
        }

        async fn release_owned(
            &self,
            key: StorageOwnershipKey,
            expected_state: DataPlaneLeaseState,
            expected_generation: u64,
        ) -> blaze_core::Result<bool> {
            self.release_count.fetch_add(1, Ordering::AcqRel);
            self.inner
                .release_owned(key, expected_state, expected_generation)
                .await
        }

        async fn sync_artifacts(&self, slot: &StorageSlot) -> blaze_core::Result<()> {
            self.inner.sync_artifacts(slot).await
        }

        fn pool_status(&self) -> PoolStatus {
            self.inner.pool_status()
        }
    }

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

    fn replace_durable_provider_identity(state: &Arc<ServerState>, id: Uuid) -> serde_json::Value {
        let mut instance = state.manager.get(id).expect("sandbox lifecycle");
        let lease = instance
            .data_plane_lease
            .as_mut()
            .expect("durable data-plane lease");
        let original_provider = lease.provider_instance_id;
        lease.provider_instance_id = Uuid::new_v4();
        assert_ne!(lease.provider_instance_id, original_provider);
        state
            .state_store
            .persist(&instance)
            .expect("persist foreign provider fixture");
        state
            .instances
            .lock()
            .expect("instances")
            .insert(id, instance.clone());
        serde_json::to_value(instance).expect("serialize lifecycle fixture")
    }

    #[test]
    fn sandbox_management_response_does_not_expose_provider_journal() {
        let mut instance = SandboxInstance::new(
            BackendKind::Mock,
            WorkloadClass::AgentTool,
            "sha256:management-shape".to_string(),
            "management-shape".to_string(),
        );
        instance.begin_operation(OperationKind::Create);
        instance
            .begin_provider_operation(PendingProviderOperationRecord {
                provider_instance_id: Uuid::new_v4(),
                context: DataPlaneRequestContextRecord {
                    instance_id: instance.id,
                    request_id: Uuid::new_v4(),
                    operation_id: Uuid::new_v4(),
                    lease_id: Uuid::new_v4(),
                    generation: 1,
                },
                generation_before_call: 0,
                root_filesystem_bytes: 4096,
                guest_memory_bytes: 8192,
                kind: PendingProviderOperationKind::PrepareLease,
            })
            .expect("provider write-ahead record");

        let persisted = serde_json::to_value(&instance).expect("persisted sandbox shape");
        assert!(
            persisted["operation"]["provider_operation"].is_object(),
            "the fixture must include a persisted provider write-ahead record"
        );

        let response =
            serde_json::to_value(SandboxResp::from(instance)).expect("management sandbox shape");
        assert_sandbox_management_shape(&response);
        assert_operation_management_shape(&response["operation"]);
        assert!(
            response["operation"].get("provider_operation").is_none(),
            "provider write-ahead records must not enter the management API"
        );
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
        assert_sandbox_management_shape(&created["instance"]);
        assert!(created["decision"].is_object());
        assert_eq!(created["start_path"], "cold");
        assert_eq!(created["selected_backend"], "mock");
        let id = created["instance"]["id"]
            .as_str()
            .expect("sandbox id")
            .to_string();
        let durable = state
            .manager
            .get(Uuid::parse_str(&id).expect("sandbox UUID"))
            .expect("durable sandbox");
        assert!(
            durable.data_plane_lease.is_some(),
            "the response test must cover a durable provider lease"
        );
        assert!(
            durable.backend_runtime.is_some(),
            "the response test must cover a durable backend identity"
        );
        let item = format!("/v1/sandboxes/{id}");

        let (status, sandboxes) =
            dispatched_json(&state, Method::GET, "/v1/sandboxes", Vec::new()).await;
        assert_eq!(status, StatusCode::OK);
        let listed = sandboxes
            .as_array()
            .expect("sandbox list")
            .iter()
            .find(|candidate| candidate["id"] == id)
            .expect("created sandbox in list");
        assert_sandbox_management_shape(listed);

        let (status, fetched) = dispatched_json(&state, Method::GET, &item, Vec::new()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(fetched["id"], id);
        assert_sandbox_management_shape(&fetched);

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

        let class = "2a".repeat(32);
        for (method, path) in [
            (Method::GET, "/v1/pools".to_string()),
            (Method::GET, format!("/v1/pools/mock/{class}")),
            (Method::POST, format!("/v1/pools/mock/{class}/drain")),
            (Method::PUT, format!("/v1/pools/mock/{class}/sizing")),
        ] {
            let (status, body) = handled_json(&state, method, &path, Vec::new()).await;
            assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{path}");
            assert_eq!(body["status"], 501, "{path}");
        }

        let (status, body) = handled_json(&state, Method::GET, "/v1/pools/mock", Vec::new()).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["status"], 404);
    }

    #[tokio::test]
    async fn provider_capacity_routes_report_and_idempotently_drain_one_partition() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let provider = Arc::new(CapacityTestProvider::new(storage.clone()));
        let state = build_test_state_with_provider(
            config,
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, Arc::new(MockSpawner)),
            BackendKind::Mock,
            storage,
            provider,
        );

        let capacity_class = CapacityClass {
            root_filesystem_capacity_bytes: 4 * 1024 * 1024 * 1024,
            guest_memory_capacity_bytes: 512 * 1024 * 1024,
        };
        let class = encode_capacity_class_digest(capacity_class.digest());
        let item = format!("/v1/pools/mock/{class}");
        let (status, capacity) = handled_json(&state, Method::GET, &item, Vec::new()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(capacity["backend"], "mock");
        assert_eq!(capacity["class_sha256"], class);
        assert_eq!(
            capacity["root_filesystem_capacity_bytes"],
            4_294_967_296_u64
        );
        assert_eq!(capacity["guest_memory_capacity_bytes"], 536_870_912_u64);
        assert_eq!(capacity["revision"], 1);
        assert_eq!(capacity["ready"], 3);
        assert_eq!(capacity["building"], 1);
        assert_eq!(capacity["in_use"], 2);
        assert_eq!(capacity["draining"], 0);
        assert_eq!(capacity["quarantined"], 1);
        assert_eq!(capacity["total"], 7);
        assert_eq!(capacity["accepting_allocations"], true);
        assert!(capacity.get("provider_instance_id").is_none());

        let operation_id = Uuid::new_v4();
        let body =
            serde_json::to_vec(&json!({"operation_id": operation_id})).expect("drain request");
        for _ in 0..2 {
            let (status, drained) =
                handled_json(&state, Method::POST, &format!("{item}/drain"), body.clone()).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(drained["operation_id"], operation_id.to_string());
            assert_eq!(drained["removed_ready"], 3);
            assert_eq!(drained["deferred_in_use"], 2);
            assert_eq!(drained["capacity"]["revision"], 2);
            assert_eq!(drained["capacity"]["ready"], 0);
            assert_eq!(drained["capacity"]["building"], 0);
            assert_eq!(drained["capacity"]["in_use"], 0);
            assert_eq!(drained["capacity"]["draining"], 3);
            assert_eq!(drained["capacity"]["quarantined"], 1);
            assert_eq!(drained["capacity"]["total"], 4);
            assert_eq!(drained["capacity"]["accepting_allocations"], false);
        }

        let (status, body) = handled_json(
            &state,
            Method::GET,
            &format!("/v1/pools/firecracker/{class}"),
            Vec::new(),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["status"], 404);

        let (status, body) = handled_json(
            &state,
            Method::GET,
            &format!("/v1/pools/not-a-backend/{class}"),
            Vec::new(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["status"], 400);

        let (status, body) = handled_json(
            &state,
            Method::GET,
            "/v1/pools/mock/not-a-digest",
            Vec::new(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["status"], 400);

        let unknown = "5a".repeat(32);
        let (status, body) = handled_json(
            &state,
            Method::GET,
            &format!("/v1/pools/mock/{unknown}"),
            Vec::new(),
        )
        .await;
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
    async fn checkpoint_rejects_unmanaged_storage_without_mutation() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let provider = Arc::new(ManagedStorageToggleProvider::new(storage.clone()));
        let state = build_test_state_with_provider(
            config,
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, Arc::new(MockSpawner)),
            BackendKind::Mock,
            storage,
            provider.clone(),
        );
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id");
        let uuid = Uuid::parse_str(id).expect("uuid");
        write_checkpoint_fixture(&state, id).await;
        let state_path = configured_state_dir(&state).join(id).join("state.json");
        let persisted_before = std::fs::read(&state_path).expect("persisted state");
        provider.set_daemon_managed_storage(false);

        let error = state
            .manager
            .checkpoint(uuid)
            .await
            .expect_err("unmanaged storage must not use the standard checkpoint path");

        assert!(matches!(error, BlazeDaemonError::UnsupportedOperation(_)));
        assert!(
            error
                .to_string()
                .contains("does not use daemon-managed storage for checkpoint capture")
        );
        let lifecycle = state.manager.get(uuid).expect("lifecycle");
        assert_eq!(lifecycle.state, SandboxState::Running);
        assert!(lifecycle.operation.is_none());
        assert_eq!(
            std::fs::read(state_path).expect("persisted state"),
            persisted_before
        );
        assert!(state.manager.backend_owner(uuid).is_some());
    }

    #[tokio::test]
    async fn checkpoint_restore_rejects_unmanaged_storage_without_mutation() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let provider = Arc::new(ManagedStorageToggleProvider::new(storage.clone()));
        let state = build_test_state_with_provider(
            config,
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, Arc::new(MockSpawner)),
            BackendKind::Mock,
            storage,
            provider.clone(),
        );
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id");
        let uuid = Uuid::parse_str(id).expect("uuid");
        write_checkpoint_fixture(&state, id).await;
        let checkpoint = state
            .manager
            .checkpoint(uuid)
            .await
            .expect("standard checkpoint");
        let state_path = configured_state_dir(&state).join(id).join("state.json");
        let persisted_before = std::fs::read(&state_path).expect("persisted state");
        let owner = state.manager.backend_owner(uuid).expect("backend owner");
        provider.set_daemon_managed_storage(false);

        let error = state
            .manager
            .restore(
                uuid,
                RestoreSandbox {
                    checkpoint_id: checkpoint.id.clone(),
                    binary_path: PathBuf::new(),
                },
            )
            .await
            .expect_err("unmanaged storage must not use the standard restore path");

        assert!(matches!(error, BlazeDaemonError::UnsupportedOperation(_)));
        assert!(
            error
                .to_string()
                .contains("does not use daemon-managed storage for checkpoint restore")
        );
        let lifecycle = state.manager.get(uuid).expect("lifecycle");
        assert_eq!(lifecycle.state, SandboxState::Running);
        assert!(lifecycle.operation.is_none());
        assert_eq!(
            lifecycle.last_checkpoint.as_deref(),
            Some(checkpoint.id.as_str())
        );
        assert_eq!(
            std::fs::read(state_path).expect("persisted state"),
            persisted_before
        );
        let retained = state.manager.backend_owner(uuid).expect("retained backend");
        assert!(Arc::ptr_eq(&owner, &retained));
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
    async fn provider_checkpoint_pairs_backend_artifacts_with_an_opaque_reference() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let provider = Arc::new(InventoryTestProvider::new(storage.clone()));
        let state = build_test_state_with_provider(
            config.clone(),
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, Arc::new(MockSpawner)),
            BackendKind::Mock,
            storage,
            provider.clone(),
        );
        let created = created_json(&state, &test_request()).await;
        let id = Uuid::parse_str(created["instance"]["id"].as_str().expect("sandbox id"))
            .expect("sandbox UUID");

        let checkpoint = state
            .manager
            .checkpoint(id)
            .await
            .expect("provider checkpoint");

        let provider_record = checkpoint
            .provider_checkpoint
            .as_ref()
            .expect("provider checkpoint record");
        assert_eq!(
            format!("ckpt-{}", provider_record.public_checkpoint_id),
            checkpoint.id
        );
        assert_eq!(provider.checkpoint_count(), 1);
        assert!(
            checkpoint
                .artifacts
                .iter()
                .all(|artifact| artifact.name.starts_with("backend/"))
        );
        assert!(
            !configured_state_dir(&state)
                .join("checkpoints")
                .join(id.to_string())
                .join(&checkpoint.id)
                .join("storage/rootfs.snap")
                .exists()
        );
        let lifecycle = state.manager.get(id).expect("lifecycle");
        assert!(lifecycle.pending_provider_retirements.is_empty());
        assert_eq!(
            lifecycle.data_plane_lease.map(|lease| lease.generation),
            Some(provider_record.source_generation)
        );
    }

    #[tokio::test]
    async fn provider_checkpoint_response_omits_provider_ownership_record() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let provider = Arc::new(InventoryTestProvider::new(storage.clone()));
        let state = build_test_state_with_provider(
            config,
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, Arc::new(MockSpawner)),
            BackendKind::Mock,
            storage,
            provider.clone(),
        );
        let (status, created) =
            dispatched_json(&state, Method::POST, "/v1/sandboxes", test_request()).await;
        assert_eq!(status, StatusCode::CREATED);
        let id = created["instance"]["id"].as_str().expect("sandbox id");

        let (status, checkpoint) = dispatched_json(
            &state,
            Method::POST,
            &format!("/v1/sandboxes/{id}/checkpoint"),
            Vec::new(),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert!(
            checkpoint.get("provider_checkpoint").is_none(),
            "provider ownership must not extend the checkpoint response"
        );
        assert_eq!(
            provider.checkpoint_count(),
            1,
            "the test must exercise a provider-owned checkpoint"
        );
    }

    #[tokio::test]
    async fn provider_checkpoint_restore_replaces_the_lease_after_backend_readiness() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let provider = Arc::new(InventoryTestProvider::new(storage.clone()));
        let state = build_test_state_with_provider(
            config,
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, Arc::new(MockSpawner)),
            BackendKind::Mock,
            storage,
            provider.clone(),
        );
        let created = created_json(&state, &test_request()).await;
        let id = Uuid::parse_str(created["instance"]["id"].as_str().expect("sandbox id"))
            .expect("sandbox UUID");
        let checkpoint = state
            .manager
            .checkpoint(id)
            .await
            .expect("provider checkpoint");
        let old_lease = state
            .manager
            .get(id)
            .expect("before restore")
            .data_plane_lease
            .expect("old lease")
            .lease_id;

        let restored = state
            .manager
            .restore(
                id,
                RestoreSandbox {
                    checkpoint_id: checkpoint.id.clone(),
                    binary_path: PathBuf::new(),
                },
            )
            .await
            .expect("provider restore");

        let replacement = restored
            .instance
            .data_plane_lease
            .expect("replacement lease");
        assert_ne!(replacement.lease_id, old_lease);
        assert_eq!(
            replacement.state,
            blaze_core::data_plane::DataPlaneLeaseState::Finalized
        );
        assert_eq!(replacement.generation, 3);
        assert!(restored.instance.replacement_data_plane_lease.is_none());
        assert_eq!(restored.instance.state, SandboxState::Running);
        assert!(state.manager.backend_owner(id).is_some());
        assert!(provider.binding(old_lease).is_none());
        assert_eq!(
            provider
                .binding(replacement.lease_id)
                .map(|binding| binding.state),
            Some(LeaseState::Finalized)
        );
    }

    #[tokio::test]
    async fn provider_checkpoint_restore_rejects_unconsumable_opened_resources_before_prepare() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let provider = Arc::new(InventoryTestProvider::new(storage.clone()));
        let state = build_test_state_with_provider(
            config,
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, Arc::new(MockSpawner)),
            BackendKind::Mock,
            storage,
            provider.clone(),
        );
        let created = created_json(&state, &test_request()).await;
        let id = Uuid::parse_str(created["instance"]["id"].as_str().expect("sandbox id"))
            .expect("sandbox UUID");
        let checkpoint = state
            .manager
            .checkpoint(id)
            .await
            .expect("provider checkpoint");
        let before = state.manager.get(id).expect("running lifecycle");
        let state_path = configured_state_dir(&state)
            .join(id.to_string())
            .join("state.json");
        let persisted_before = std::fs::read(&state_path).expect("persisted lifecycle");
        let owner = state.manager.backend_owner(id).expect("running owner");
        provider.advertise_opened_checkpoint_restore_resources();

        let error = state
            .manager
            .restore(
                id,
                RestoreSandbox {
                    checkpoint_id: checkpoint.id,
                    binary_path: PathBuf::new(),
                },
            )
            .await
            .expect_err("the backend cannot consume typed opened attachments");

        assert!(matches!(error, BlazeDaemonError::UnsupportedOperation(_)));
        assert_eq!(provider.restore_checkpoint_calls(), 0);
        let after = state.manager.get(id).expect("unchanged lifecycle");
        assert_eq!(after.state, SandboxState::Running);
        assert_eq!(after.operation, before.operation);
        assert_eq!(after.data_plane_lease, before.data_plane_lease);
        assert!(after.replacement_data_plane_lease.is_none());
        assert_eq!(
            std::fs::read(state_path).expect("persisted lifecycle"),
            persisted_before
        );
        let retained_owner = state.manager.backend_owner(id).expect("running owner");
        assert!(Arc::ptr_eq(&owner, &retained_owner));
    }

    #[tokio::test]
    async fn provider_restore_stop_failure_retains_current_owner_and_replacement_lease() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let provider = Arc::new(InventoryTestProvider::new(storage.clone()));
        let current_kill_allowed = Arc::new(AtomicBool::new(false));
        let current_kill_attempts = Arc::new(AtomicUsize::new(0));
        let replacement_kill_allowed = Arc::new(AtomicBool::new(true));
        let state = build_test_state_with_provider(
            config,
            test_policy(BackendKind::Mock),
            spawners(
                BackendKind::Mock,
                Arc::new(KillGateSpawner {
                    current_kill_allowed: current_kill_allowed.clone(),
                    current_kill_attempts: current_kill_attempts.clone(),
                    replacement_kill_allowed,
                    replacement_kill_attempts: Arc::new(AtomicUsize::new(0)),
                }),
            ),
            BackendKind::Mock,
            storage,
            provider.clone(),
        );
        let created = created_json(&state, &test_request()).await;
        let id = Uuid::parse_str(created["instance"]["id"].as_str().expect("sandbox id"))
            .expect("sandbox UUID");
        let checkpoint = state
            .manager
            .checkpoint(id)
            .await
            .expect("provider checkpoint");
        let current_owner = state.manager.backend_owner(id).expect("current owner");

        let error = state
            .manager
            .restore(
                id,
                RestoreSandbox {
                    checkpoint_id: checkpoint.id,
                    binary_path: PathBuf::new(),
                },
            )
            .await
            .expect_err("uncertain backend stop must fail closed");

        assert!(matches!(error, BlazeDaemonError::RecoveryRequired(_)));
        assert!(current_kill_attempts.load(Ordering::Acquire) >= 1);
        let retained_owner = state
            .manager
            .backend_owner(id)
            .expect("retained current owner");
        assert!(Arc::ptr_eq(&current_owner, &retained_owner));
        let lifecycle = state.manager.get(id).expect("retained lifecycle");
        assert_eq!(lifecycle.state, SandboxState::RecoveryRequired);
        assert_eq!(lifecycle.backend_ownership, BackendOwnership::Unknown);
        assert!(lifecycle.backend_runtime.is_some());
        let replacement = lifecycle
            .replacement_data_plane_lease
            .expect("retained replacement lease");
        assert!(provider.binding(replacement.lease_id).is_some());

        current_kill_allowed.store(true, Ordering::Release);
        assert!(state.manager.destroy(id).await.expect("destroy"));
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn provider_restore_cleanup_failure_retains_replacement_owner_and_lease() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let provider = Arc::new(InventoryTestProvider::new(storage.clone()));
        let replacement_kill_allowed = Arc::new(AtomicBool::new(false));
        let replacement_kill_attempts = Arc::new(AtomicUsize::new(0));
        let state = build_test_state_with_provider(
            config,
            test_policy(BackendKind::Mock),
            spawners(
                BackendKind::Mock,
                Arc::new(KillGateSpawner {
                    current_kill_allowed: Arc::new(AtomicBool::new(true)),
                    current_kill_attempts: Arc::new(AtomicUsize::new(0)),
                    replacement_kill_allowed: replacement_kill_allowed.clone(),
                    replacement_kill_attempts: replacement_kill_attempts.clone(),
                }),
            ),
            BackendKind::Mock,
            storage,
            provider.clone(),
        );
        let created = created_json(&state, &test_request()).await;
        let id = Uuid::parse_str(created["instance"]["id"].as_str().expect("sandbox id"))
            .expect("sandbox UUID");
        let checkpoint = state
            .manager
            .checkpoint(id)
            .await
            .expect("provider checkpoint");
        let hook = crate::failpoint::TestFailpoint::new(&["restore-guest-ready"]);

        let error = hook
            .run(state.manager.restore(
                id,
                RestoreSandbox {
                    checkpoint_id: checkpoint.id,
                    binary_path: PathBuf::new(),
                },
            ))
            .await
            .expect_err("uncertain replacement cleanup must fail closed");

        assert!(matches!(error, BlazeDaemonError::RecoveryRequired(_)));
        assert!(replacement_kill_attempts.load(Ordering::Acquire) >= 1);
        assert!(state.manager.backend_owner(id).is_some());
        let lifecycle = state.manager.get(id).expect("retained lifecycle");
        assert_eq!(lifecycle.state, SandboxState::RecoveryRequired);
        assert_eq!(lifecycle.backend_ownership, BackendOwnership::Unknown);
        assert!(lifecycle.backend_runtime.is_some());
        let replacement = lifecycle
            .replacement_data_plane_lease
            .expect("retained replacement lease");
        assert!(provider.binding(replacement.lease_id).is_some());

        replacement_kill_allowed.store(true, Ordering::Release);
        assert!(state.manager.destroy(id).await.expect("destroy"));
    }

    #[tokio::test]
    async fn provider_checkpoint_prune_retires_content_after_catalog_removal() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let provider = Arc::new(InventoryTestProvider::new(storage.clone()));
        let state = build_test_state_with_provider(
            config,
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, Arc::new(MockSpawner)),
            BackendKind::Mock,
            storage,
            provider.clone(),
        );
        let created = created_json(&state, &test_request()).await;
        let id = Uuid::parse_str(created["instance"]["id"].as_str().expect("sandbox id"))
            .expect("sandbox UUID");
        let root = state.manager.checkpoint(id).await.expect("root checkpoint");
        let unreachable = state
            .manager
            .checkpoint(id)
            .await
            .expect("child checkpoint");
        state
            .manager
            .restore(
                id,
                RestoreSandbox {
                    checkpoint_id: root.id.clone(),
                    binary_path: PathBuf::new(),
                },
            )
            .await
            .expect("restore root");

        let removed = state
            .manager
            .prune_checkpoints(id)
            .await
            .expect("provider prune");

        assert_eq!(removed, vec![unreachable.id]);
        assert_eq!(provider.checkpoint_count(), 1);
        assert!(
            state
                .manager
                .get(id)
                .expect("lifecycle")
                .pending_provider_retirements
                .is_empty()
        );
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn provider_checkpoint_publication_failure_retires_unpublished_content() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let provider = Arc::new(InventoryTestProvider::new(storage.clone()));
        let state = build_test_state_with_provider(
            config,
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, Arc::new(MockSpawner)),
            BackendKind::Mock,
            storage,
            provider.clone(),
        );
        let created = created_json(&state, &test_request()).await;
        let id = Uuid::parse_str(created["instance"]["id"].as_str().expect("sandbox id"))
            .expect("sandbox UUID");
        let hook = crate::failpoint::TestFailpoint::new(&["checkpoint-publish"]);

        let error = hook
            .run(state.manager.checkpoint(id))
            .await
            .expect_err("publication failure");

        assert!(!matches!(error, BlazeDaemonError::RecoveryRequired(_)));
        assert_eq!(provider.checkpoint_count(), 0);
        let lifecycle = state.manager.get(id).expect("compensated lifecycle");
        assert_eq!(lifecycle.state, SandboxState::Running);
        assert!(lifecycle.operation.is_none());
        assert!(lifecycle.pending_provider_retirements.is_empty());
        assert!(state.manager.backend_owner(id).is_some());
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
    async fn hibernate_rejects_unmanaged_storage_without_mutation() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let provider = Arc::new(ManagedStorageToggleProvider::new(storage.clone()));
        let state = build_test_state_with_provider(
            config,
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, Arc::new(GuestMockSpawner)),
            BackendKind::Mock,
            storage,
            provider.clone(),
        );
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id");
        let uuid = Uuid::parse_str(id).expect("uuid");
        let state_path = configured_state_dir(&state).join(id).join("state.json");
        let persisted_before = std::fs::read(&state_path).expect("persisted state");
        let owner = state.manager.backend_owner(uuid).expect("backend owner");
        provider.set_daemon_managed_storage(false);

        let error = state
            .manager
            .hibernate(
                uuid,
                HibernateSandbox {
                    binary_path: PathBuf::new(),
                },
            )
            .await
            .expect_err("unmanaged storage must not use the standard hibernation path");

        assert!(matches!(error, BlazeDaemonError::UnsupportedOperation(_)));
        assert!(
            error
                .to_string()
                .contains("does not use daemon-managed storage for hibernation")
        );
        let lifecycle = state.manager.get(uuid).expect("lifecycle");
        assert_eq!(lifecycle.state, SandboxState::Running);
        assert!(lifecycle.operation.is_none());
        assert_eq!(
            std::fs::read(state_path).expect("persisted state"),
            persisted_before
        );
        let retained = state.manager.backend_owner(uuid).expect("retained backend");
        assert!(Arc::ptr_eq(&owner, &retained));
    }

    #[tokio::test]
    async fn resume_rejects_unmanaged_storage_without_mutation() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let provider = Arc::new(ManagedStorageToggleProvider::new(storage.clone()));
        let state = build_test_state_with_provider(
            config,
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, Arc::new(GuestMockSpawner)),
            BackendKind::Mock,
            storage,
            provider.clone(),
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
            .expect("standard hibernation");
        let state_path = configured_state_dir(&state).join(id).join("state.json");
        let persisted_before = std::fs::read(&state_path).expect("persisted state");
        provider.set_daemon_managed_storage(false);

        let error = state
            .manager
            .resume(
                uuid,
                ResumeSandbox {
                    binary_path: PathBuf::new(),
                },
            )
            .await
            .expect_err("unmanaged storage must not use the standard resume path");

        assert!(matches!(error, BlazeDaemonError::UnsupportedOperation(_)));
        assert!(
            error
                .to_string()
                .contains("does not use daemon-managed storage for resume")
        );
        let lifecycle = state.manager.get(uuid).expect("lifecycle");
        assert_eq!(lifecycle.state, SandboxState::Hibernated);
        assert!(lifecycle.operation.is_none());
        assert_eq!(
            std::fs::read(state_path).expect("persisted state"),
            persisted_before
        );
        assert!(state.manager.backend_owner(uuid).is_none());
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
        let report = state
            .manager
            .reconcile_startup()
            .await
            .expect("startup reconciliation");
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
        let report = restarted
            .manager
            .reconcile_startup()
            .await
            .expect("startup reconciliation");
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
    async fn provider_hibernation_releases_active_lease_and_resumes_with_a_fresh_lease() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let provider = Arc::new(InventoryTestProvider::new(storage.clone()));
        let state = build_test_state_with_provider(
            config.clone(),
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, Arc::new(GuestMockSpawner)),
            BackendKind::Mock,
            storage,
            provider.clone(),
        );
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id");
        let uuid = Uuid::parse_str(id).expect("uuid");
        let original_lease = state
            .manager
            .get(uuid)
            .expect("created lifecycle")
            .data_plane_lease
            .expect("created provider lease")
            .lease_id;

        let hibernated = state
            .manager
            .hibernate(
                uuid,
                HibernateSandbox {
                    binary_path: PathBuf::new(),
                },
            )
            .await
            .expect("provider hibernate");
        assert_eq!(hibernated.state, SandboxState::Hibernated);
        assert_eq!(hibernated.backend_ownership, BackendOwnership::Stopped);
        assert!(hibernated.data_plane_lease.is_none());
        let suspension = hibernated
            .provider_suspension
            .clone()
            .expect("durable provider suspension");
        assert_eq!(provider.suspension_count(), 1);
        assert!(provider.binding(original_lease).is_none());
        drop(state);

        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let restarted = build_test_state_with_provider(
            config,
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, Arc::new(GuestMockSpawner)),
            BackendKind::Mock,
            storage,
            provider.clone(),
        );
        let report = restarted
            .manager
            .reconcile_startup()
            .await
            .expect("restart reconciliation");
        assert_eq!(report.attempted, 0);
        assert!(report.failures.is_empty());

        let resumed = restarted
            .manager
            .resume(
                uuid,
                ResumeSandbox {
                    binary_path: PathBuf::new(),
                },
            )
            .await
            .expect("provider resume");
        assert_eq!(resumed.state, SandboxState::Running);
        assert_eq!(resumed.provider_suspension, Some(suspension));
        let resumed_lease = resumed.data_plane_lease.expect("fresh provider lease");
        assert_ne!(resumed_lease.lease_id, original_lease);
        assert_eq!(
            resumed_lease.state,
            blaze_core::data_plane::DataPlaneLeaseState::Finalized
        );
        assert_eq!(provider.suspension_count(), 1);

        assert!(restarted.manager.destroy(uuid).await.expect("destroy"));
        assert_eq!(provider.suspension_count(), 0);
        let destroyed = restarted.manager.get(uuid).expect("destroyed lifecycle");
        assert!(destroyed.data_plane_lease.is_none());
        assert!(destroyed.provider_suspension.is_none());
    }

    #[tokio::test]
    async fn provider_resume_rejects_unconsumable_opened_resources_before_prepare() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let provider = Arc::new(InventoryTestProvider::new(storage.clone()));
        let state = build_test_state_with_provider(
            config,
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, Arc::new(GuestMockSpawner)),
            BackendKind::Mock,
            storage,
            provider.clone(),
        );
        let created = created_json(&state, &test_request()).await;
        let id = Uuid::parse_str(created["instance"]["id"].as_str().expect("sandbox id"))
            .expect("sandbox UUID");
        state
            .manager
            .hibernate(
                id,
                HibernateSandbox {
                    binary_path: PathBuf::new(),
                },
            )
            .await
            .expect("provider hibernate");
        let before = state.manager.get(id).expect("hibernated lifecycle");
        let state_path = configured_state_dir(&state)
            .join(id.to_string())
            .join("state.json");
        let persisted_before = std::fs::read(&state_path).expect("persisted lifecycle");
        let suspension = before
            .provider_suspension
            .clone()
            .expect("provider suspension");
        assert!(state.manager.backend_owner(id).is_none());
        provider.advertise_opened_suspension_restore_resources();

        let error = state
            .manager
            .resume(
                id,
                ResumeSandbox {
                    binary_path: PathBuf::new(),
                },
            )
            .await
            .expect_err("the backend cannot consume typed opened attachments");

        assert!(matches!(error, BlazeDaemonError::UnsupportedOperation(_)));
        assert_eq!(provider.resume_calls(), 0);
        let after = state.manager.get(id).expect("unchanged lifecycle");
        assert_eq!(after.state, SandboxState::Hibernated);
        assert!(after.operation.is_none());
        assert_eq!(after.provider_suspension, Some(suspension));
        assert!(after.data_plane_lease.is_none());
        assert!(after.replacement_data_plane_lease.is_none());
        assert_eq!(
            std::fs::read(state_path).expect("persisted lifecycle"),
            persisted_before
        );
        assert!(state.manager.backend_owner(id).is_none());
        assert_eq!(provider.suspension_count(), 1);
    }

    #[tokio::test]
    async fn destroy_removes_the_hibernation_owner_before_retiring_provider_content() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let provider = Arc::new(InventoryTestProvider::new(storage.clone()));
        let state = build_test_state_with_provider(
            config.clone(),
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, Arc::new(GuestMockSpawner)),
            BackendKind::Mock,
            storage,
            provider.clone(),
        );
        let created = created_json(&state, &test_request()).await;
        let id = Uuid::parse_str(created["instance"]["id"].as_str().expect("sandbox id"))
            .expect("sandbox UUID");
        let hibernated = state
            .manager
            .hibernate(
                id,
                HibernateSandbox {
                    binary_path: PathBuf::new(),
                },
            )
            .await
            .expect("provider hibernate");
        let suspension = hibernated
            .provider_suspension
            .clone()
            .expect("provider suspension");
        let hibernate_dir = config
            .daemon
            .state_dir
            .join(id.to_string())
            .join("hibernate");
        assert!(hibernate_dir.join("manifest.json").is_file());
        provider.observe_suspension_public_owner(hibernate_dir.clone());
        provider.reject_suspension_retirement(true);

        let error = state
            .manager
            .destroy(id)
            .await
            .expect_err("retirement failure must retain a retry ledger");

        assert!(matches!(error, BlazeDaemonError::RecoveryRequired(_)));
        assert!(
            !hibernate_dir.exists(),
            "hibernation owner still exists after destroy failed: {error}; lifecycle: {:?}",
            state.manager.get(id).expect("retained lifecycle")
        );
        assert!(provider.suspension_retirement_calls.load(Ordering::Acquire) >= 1);
        assert!(
            !provider
                .retired_suspension_while_public_owner_existed
                .load(Ordering::Acquire)
        );
        assert_eq!(provider.suspension_count(), 1);
        let retained = state.manager.get(id).expect("retained retirement ledger");
        assert!(retained.provider_suspension.is_none());
        assert!(
            retained
                .pending_provider_suspension_retirements
                .contains(&suspension)
        );

        provider.reject_suspension_retirement(false);
        assert!(state.manager.destroy(id).await.expect("retry destroy"));
        assert_eq!(provider.suspension_count(), 0);
    }

    #[tokio::test]
    async fn startup_reads_the_hibernation_manifest_before_retrying_retirement() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let provider = Arc::new(InventoryTestProvider::new(storage.clone()));
        let state = build_test_state_with_provider(
            config.clone(),
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, Arc::new(GuestMockSpawner)),
            BackendKind::Mock,
            storage,
            provider.clone(),
        );
        let created = created_json(&state, &test_request()).await;
        let id = Uuid::parse_str(created["instance"]["id"].as_str().expect("sandbox id"))
            .expect("sandbox UUID");
        state
            .manager
            .hibernate(
                id,
                HibernateSandbox {
                    binary_path: PathBuf::new(),
                },
            )
            .await
            .expect("provider hibernate");
        let hibernate_dir = config
            .daemon
            .state_dir
            .join(id.to_string())
            .join("hibernate");
        let mut interrupted = state.manager.get(id).expect("hibernated lifecycle");
        let suspension = interrupted
            .provider_suspension
            .take()
            .expect("provider suspension");
        interrupted
            .pending_provider_suspension_retirements
            .push(suspension);
        interrupted
            .persist(&config.daemon.state_dir)
            .expect("persist crash boundary after lifecycle owner removal");
        drop(state);

        provider.observe_suspension_public_owner(hibernate_dir.clone());
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let restarted = build_test_state_with_provider(
            config,
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, Arc::new(GuestMockSpawner)),
            BackendKind::Mock,
            storage,
            provider.clone(),
        );

        restarted
            .manager
            .reconcile_startup()
            .await
            .expect("startup cleanup");

        assert!(!hibernate_dir.exists());
        assert!(provider.suspension_retirement_calls.load(Ordering::Acquire) >= 1);
        assert!(
            !provider
                .retired_suspension_while_public_owner_existed
                .load(Ordering::Acquire)
        );
        assert_eq!(provider.suspension_count(), 0);
        assert_eq!(
            restarted.manager.get(id).expect("terminal lifecycle").state,
            SandboxState::Destroyed
        );
    }

    #[tokio::test]
    async fn provider_suspension_stays_out_of_hibernate_and_resume_responses() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let provider = Arc::new(InventoryTestProvider::new(storage.clone()));
        let state = build_test_state_with_provider(
            config,
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, Arc::new(GuestMockSpawner)),
            BackendKind::Mock,
            storage,
            provider.clone(),
        );
        let (status, created) =
            dispatched_json(&state, Method::POST, "/v1/sandboxes", test_request()).await;
        assert_eq!(status, StatusCode::CREATED);
        let id = created["instance"]["id"].as_str().expect("sandbox id");
        let uuid = Uuid::parse_str(id).expect("sandbox UUID");
        let item = format!("/v1/sandboxes/{id}");

        let (status, hibernated) = dispatched_json(
            &state,
            Method::POST,
            &format!("{item}/hibernate"),
            Vec::new(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(hibernated["state"], "hibernated");
        assert_sandbox_management_shape(&hibernated);
        assert!(
            state
                .manager
                .get(uuid)
                .expect("durable hibernated sandbox")
                .provider_suspension
                .is_some(),
            "the test must cover a durable provider suspension"
        );
        assert_eq!(provider.suspension_count(), 1);

        let (status, resumed) =
            dispatched_json(&state, Method::POST, &format!("{item}/resume"), Vec::new()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(resumed["state"], "running");
        assert_sandbox_management_shape(&resumed);

        state.manager.destroy(uuid).await.expect("destroy sandbox");
        assert_eq!(provider.suspension_count(), 0);
    }

    #[tokio::test]
    async fn provider_hibernation_requires_guest_hooks_before_provider_mutation() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let provider = Arc::new(InventoryTestProvider::new(storage.clone()));
        let state = build_test_state_with_provider(
            config,
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, Arc::new(MockSpawner)),
            BackendKind::Mock,
            storage,
            provider.clone(),
        );
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id");
        let uuid = Uuid::parse_str(id).expect("uuid");
        let before = state.manager.get(uuid).expect("created lifecycle");

        let error = state
            .manager
            .hibernate(
                uuid,
                HibernateSandbox {
                    binary_path: PathBuf::new(),
                },
            )
            .await
            .expect_err("guest transport is mandatory");

        assert!(matches!(error, BlazeDaemonError::UnsupportedOperation(_)));
        let after = state.manager.get(uuid).expect("retained lifecycle");
        assert_eq!(after.state, SandboxState::Running);
        assert_eq!(after.operation, None);
        assert_eq!(after.data_plane_lease, before.data_plane_lease);
        assert_eq!(provider.suspension_count(), 0);
        assert!(state.manager.backend_owner(uuid).is_some());
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn provider_resume_start_failure_preserves_immutable_content_for_retry() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let provider = Arc::new(InventoryTestProvider::new(storage.clone()));
        let state = build_test_state_with_provider(
            config,
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, Arc::new(GuestMockSpawner)),
            BackendKind::Mock,
            storage,
            provider.clone(),
        );
        let created = created_json(&state, &test_request()).await;
        let id = created["instance"]["id"].as_str().expect("id");
        let uuid = Uuid::parse_str(id).expect("uuid");
        let hibernated = state
            .manager
            .hibernate(
                uuid,
                HibernateSandbox {
                    binary_path: PathBuf::new(),
                },
            )
            .await
            .expect("hibernate");
        let suspension = hibernated
            .provider_suspension
            .clone()
            .expect("provider suspension");
        let hook = crate::failpoint::TestFailpoint::new(&["resume-backend-start"]);

        hook.run(state.manager.resume(
            uuid,
            ResumeSandbox {
                binary_path: PathBuf::new(),
            },
        ))
        .await
        .expect_err("resume start failure");

        let retained = state.manager.get(uuid).expect("retained hibernation");
        assert_eq!(retained.state, SandboxState::Hibernated);
        assert_eq!(retained.provider_suspension, Some(suspension));
        assert!(retained.data_plane_lease.is_none());
        assert!(retained.replacement_data_plane_lease.is_none());
        assert_eq!(provider.suspension_count(), 1);
        assert!(state.manager.backend_owner(uuid).is_none());

        let resumed = state
            .manager
            .resume(
                uuid,
                ResumeSandbox {
                    binary_path: PathBuf::new(),
                },
            )
            .await
            .expect("retry resume");
        assert_eq!(resumed.state, SandboxState::Running);
    }

    #[cfg(feature = "test-failpoints")]
    #[tokio::test]
    async fn provider_resume_cleanup_failure_retains_replacement_owner_and_lease() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let provider = Arc::new(InventoryTestProvider::new(storage.clone()));
        let replacement_kill_allowed = Arc::new(AtomicBool::new(false));
        let replacement_kill_attempts = Arc::new(AtomicUsize::new(0));
        let state = build_test_state_with_provider(
            config,
            test_policy(BackendKind::Mock),
            spawners(
                BackendKind::Mock,
                Arc::new(KillGateSpawner {
                    current_kill_allowed: Arc::new(AtomicBool::new(true)),
                    current_kill_attempts: Arc::new(AtomicUsize::new(0)),
                    replacement_kill_allowed: replacement_kill_allowed.clone(),
                    replacement_kill_attempts: replacement_kill_attempts.clone(),
                }),
            ),
            BackendKind::Mock,
            storage,
            provider.clone(),
        );
        let created = created_json(&state, &test_request()).await;
        let id = Uuid::parse_str(created["instance"]["id"].as_str().expect("sandbox id"))
            .expect("sandbox UUID");
        state
            .manager
            .hibernate(
                id,
                HibernateSandbox {
                    binary_path: PathBuf::new(),
                },
            )
            .await
            .expect("provider hibernate");
        let hook = crate::failpoint::TestFailpoint::new(&["resume-guest-ready"]);

        let error = hook
            .run(state.manager.resume(
                id,
                ResumeSandbox {
                    binary_path: PathBuf::new(),
                },
            ))
            .await
            .expect_err("uncertain replacement cleanup must fail closed");

        assert!(matches!(error, BlazeDaemonError::RecoveryRequired(_)));
        assert!(replacement_kill_attempts.load(Ordering::Acquire) >= 1);
        assert!(state.manager.backend_owner(id).is_some());
        let lifecycle = state.manager.get(id).expect("retained lifecycle");
        assert_eq!(lifecycle.state, SandboxState::RecoveryRequired);
        assert_eq!(lifecycle.backend_ownership, BackendOwnership::Unknown);
        assert!(lifecycle.backend_runtime.is_some());
        let replacement = lifecycle
            .replacement_data_plane_lease
            .expect("retained replacement lease");
        assert!(provider.binding(replacement.lease_id).is_some());

        replacement_kill_allowed.store(true, Ordering::Release);
        assert!(state.manager.destroy(id).await.expect("destroy"));
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
        attach_finalized_file_lease(
            storage.clone(),
            &config.daemon.state_dir,
            &mut instance,
            4096,
            4096,
        )
        .await;
        instance
            .begin_hibernate_operation()
            .expect("begin hibernation");
        instance
            .transition(SandboxState::Hibernating)
            .expect("hibernating");
        instance.persist(&config.daemon.state_dir).expect("persist");
        let id = instance.id;
        let state = build_test_state(
            config,
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, Arc::new(MockSpawner)),
            BackendKind::Mock,
            storage,
        );

        let report = state
            .manager
            .reconcile_startup()
            .await
            .expect("startup reconciliation");
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
            let mut config = test_config(&restart_temp);
            config.storage.rootfs_size = 64;
            config.storage.mem_size = 32;
            let restart_state = mock_state_from_config(config.clone());
            let created = created_json(&restart_state, &test_request()).await;
            let restart_id = created["instance"]["id"]
                .as_str()
                .expect("restart id")
                .to_string();
            let restart_uuid = Uuid::parse_str(&restart_id).expect("restart uuid");
            let slot = write_checkpoint_fixture(&restart_state, &restart_id).await;
            persist_crashed_checkpoint_phase(&restart_state, restart_uuid, phase).await;
            let rootfs = std::fs::OpenOptions::new()
                .write(true)
                .open(&slot.rootfs_path)
                .expect("open restart rootfs fixture");
            rootfs
                .set_len(config.storage.rootfs_size)
                .expect("restore restart rootfs extent");
            rootfs.sync_all().expect("sync restart rootfs extent");
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

            let report = restarted
                .manager
                .reconcile_startup()
                .await
                .expect("startup reconciliation");
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

    #[tokio::test]
    async fn restart_adopts_matching_provider_and_backend_ownership() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let provider = Arc::new(InventoryTestProvider::new(storage.clone()));
        let spawner = Arc::new(AdoptableSpawner::default());
        let state = build_test_state_with_provider(
            config.clone(),
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, spawner.clone()),
            BackendKind::Mock,
            storage.clone(),
            provider.clone(),
        );

        let created = created_json(&state, &test_request()).await;
        let id = Uuid::parse_str(created["instance"]["id"].as_str().expect("sandbox id"))
            .expect("sandbox UUID");
        let before = SandboxInstance::load(&config.daemon.state_dir, id).expect("durable state");
        assert_eq!(
            before.data_plane_lease.map(|lease| lease.state),
            Some(blaze_core::data_plane::DataPlaneLeaseState::Finalized)
        );
        assert!(
            before
                .backend_runtime
                .and_then(|runtime| runtime.process)
                .is_some()
        );
        drop(state);

        let recovered = build_test_state_with_provider(
            config,
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, spawner),
            BackendKind::Mock,
            storage,
            provider,
        );
        let report = recovered
            .manager
            .reconcile_startup()
            .await
            .expect("restart reconciliation");

        assert_eq!(report.attempted, 1);
        assert_eq!(report.completed, 1);
        assert!(report.failures.is_empty());
        let adopted = recovered.manager.get(id).expect("adopted sandbox");
        assert_eq!(adopted.state, SandboxState::Running);
        assert_eq!(
            adopted.data_plane_lease.map(|lease| lease.generation),
            Some(4)
        );
        assert!(recovered.manager.backend_owner(id).is_some());
    }

    #[tokio::test]
    async fn restart_completes_adoption_from_a_committed_lease() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let provider = Arc::new(InventoryTestProvider::new(storage.clone()));
        let spawner = Arc::new(AdoptableSpawner::default());
        let state = build_test_state_with_provider(
            config.clone(),
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, spawner.clone()),
            BackendKind::Mock,
            storage.clone(),
            provider.clone(),
        );

        let created = created_json(&state, &test_request()).await;
        let id = Uuid::parse_str(created["instance"]["id"].as_str().expect("sandbox id"))
            .expect("sandbox UUID");
        let mut interrupted =
            SandboxInstance::load(&config.daemon.state_dir, id).expect("durable finalized state");
        let mut committed = interrupted.data_plane_lease.expect("durable lease");
        committed.state = blaze_core::data_plane::DataPlaneLeaseState::Committed;
        committed.generation -= 1;
        interrupted.data_plane_lease = Some(committed);
        interrupted
            .persist(&config.daemon.state_dir)
            .expect("persist committed crash boundary");
        provider.record(LeaseBinding::from_record(id, committed));
        drop(state);

        let recovered = build_test_state_with_provider(
            config,
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, spawner),
            BackendKind::Mock,
            storage,
            provider,
        );
        let report = recovered
            .manager
            .reconcile_startup()
            .await
            .expect("restart reconciliation");

        assert_eq!(report.attempted, 1);
        assert_eq!(report.completed, 1);
        assert!(report.failures.is_empty());
        let adopted = recovered.manager.get(id).expect("adopted sandbox");
        assert_eq!(
            adopted.data_plane_lease.map(|lease| lease.state),
            Some(blaze_core::data_plane::DataPlaneLeaseState::Finalized)
        );
        assert_eq!(
            adopted.data_plane_lease.map(|lease| lease.generation),
            Some(committed.generation + 1)
        );
        assert!(recovered.manager.backend_owner(id).is_some());
    }

    #[tokio::test]
    async fn restart_quarantines_provider_identity_drift() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let provider = Arc::new(InventoryTestProvider::new(storage.clone()));
        let spawner = Arc::new(AdoptableSpawner::default());
        let state = build_test_state_with_provider(
            config.clone(),
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, spawner.clone()),
            BackendKind::Mock,
            storage.clone(),
            provider.clone(),
        );

        let created = created_json(&state, &test_request()).await;
        let id = Uuid::parse_str(created["instance"]["id"].as_str().expect("sandbox id"))
            .expect("sandbox UUID");
        let mut corrupted =
            SandboxInstance::load(&config.daemon.state_dir, id).expect("durable finalized state");
        let mut durable = corrupted.data_plane_lease.expect("durable lease");
        let lease_id = durable.lease_id;
        durable.request_id = Uuid::new_v4();
        corrupted.data_plane_lease = Some(durable);
        corrupted
            .persist(&config.daemon.state_dir)
            .expect("persist identity drift");
        drop(state);

        let recovered = build_test_state_with_provider(
            config,
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, spawner),
            BackendKind::Mock,
            storage,
            provider.clone(),
        );
        let report = recovered
            .manager
            .reconcile_startup()
            .await
            .expect("restart reconciliation");

        assert_eq!(report.attempted, 1);
        assert_eq!(report.completed, 0);
        assert_eq!(report.failures.len(), 1);
        let quarantined = recovered.manager.get(id).expect("quarantined sandbox");
        assert_eq!(quarantined.state, SandboxState::RecoveryRequired);
        assert_eq!(
            quarantined.data_plane_lease.map(|lease| lease.state),
            Some(blaze_core::data_plane::DataPlaneLeaseState::Quarantined)
        );
        assert_eq!(
            provider.binding(lease_id).map(|binding| binding.state),
            Some(LeaseState::Quarantined)
        );
        assert!(recovered.manager.backend_owner(id).is_none());
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

        let report = recovered
            .manager
            .reconcile_startup()
            .await
            .expect("startup reconciliation");

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

        let first = state
            .manager
            .reconcile_startup()
            .await
            .expect("startup reconciliation");

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
        let retry = state
            .manager
            .reconcile_startup()
            .await
            .expect("startup reconciliation");

        assert_eq!(retry.attempted, 1);
        assert_eq!(retry.completed, 1, "retry report: {retry:?}");
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
        let provider = crate::data_plane::FileDataPlaneProvider::new(storage.clone());
        let context = RequestContext {
            instance_id: instance.id,
            request_id: Uuid::new_v4(),
            operation_id: Uuid::new_v4(),
            lease_id: Uuid::new_v4(),
            generation: 1,
        };
        let prepared = provider
            .prepare(PrepareRequest {
                context,
                source: PrepareSource::Image {
                    image_digest: instance.image_digest.clone(),
                },
                root_filesystem_bytes: 4096,
                guest_memory_bytes: 4096,
            })
            .await
            .expect("prepare provider-owned storage");
        let committed = provider
            .commit(CommitRequest {
                binding: prepared.binding,
            })
            .await
            .expect("commit provider-owned storage");
        instance.data_plane_lease = Some(committed.binding.to_record(4096, 4096));
        instance
            .persist(&config.daemon.state_dir)
            .expect("persist public transition");
        let finalized = provider
            .finalize(FinalizeRequest {
                binding: committed.binding,
                public_transition: PublicTransitionRef {
                    instance_id: instance.id,
                    operation_id: context.operation_id,
                },
            })
            .await
            .expect("finalize provider-owned storage");
        instance.data_plane_lease = Some(finalized.binding.to_record(4096, 4096));
        instance
            .persist(&config.daemon.state_dir)
            .expect("persist finalized provider ownership");
        drop(provider);

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
        assert_eq!(uuid_directory_count(&temp.path().join("instances")), 0);

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

    #[tokio::test]
    async fn direct_destroy_rejects_a_foreign_provider_lease_without_cleanup() {
        let temp = tempfile::tempdir().expect("temp");
        let (state, kill_count, orphan_cleanup_count, release_count) = counting_state(&temp);
        let created = created_json(&state, &test_request()).await;
        let id = Uuid::parse_str(created["instance"]["id"].as_str().expect("sandbox id"))
            .expect("sandbox UUID");
        let before = replace_durable_provider_identity(&state, id);

        let error = state
            .manager
            .destroy(id)
            .await
            .expect_err("foreign provider ownership must stop destroy");

        assert!(matches!(
            error,
            BlazeDaemonError::RecoveryRequired(message)
                if message.contains("another provider")
                    && message.contains("records were retained")
        ));
        assert_eq!(kill_count.load(Ordering::Acquire), 0);
        assert_eq!(orphan_cleanup_count.load(Ordering::Acquire), 0);
        assert_eq!(release_count.load(Ordering::Acquire), 0);
        assert_eq!(
            serde_json::to_value(state.manager.get(id).expect("retained lifecycle"))
                .expect("serialize retained lifecycle"),
            before
        );
        assert!(temp.path().join("instances").join(id.to_string()).is_dir());
    }

    #[tokio::test]
    async fn startup_rejects_a_foreign_provider_lease_without_cleanup() {
        let temp = tempfile::tempdir().expect("temp");
        let (state, kill_count, orphan_cleanup_count, release_count) = counting_state(&temp);
        let created = created_json(&state, &test_request()).await;
        let id = Uuid::parse_str(created["instance"]["id"].as_str().expect("sandbox id"))
            .expect("sandbox UUID");
        let before = replace_durable_provider_identity(&state, id);

        let error = state
            .manager
            .reconcile_startup()
            .await
            .expect_err("foreign provider ownership must stop startup");

        assert!(matches!(
            error,
            BlazeDaemonError::RecoveryRequired(message)
                if message.contains("another provider")
                    && message.contains("records were retained")
        ));
        assert_eq!(kill_count.load(Ordering::Acquire), 0);
        assert_eq!(orphan_cleanup_count.load(Ordering::Acquire), 0);
        assert_eq!(release_count.load(Ordering::Acquire), 0);
        assert_eq!(
            serde_json::to_value(state.manager.get(id).expect("retained lifecycle"))
                .expect("serialize retained lifecycle"),
            before
        );
        assert!(temp.path().join("instances").join(id.to_string()).is_dir());
    }

    #[tokio::test]
    async fn startup_recognizes_a_standard_file_lease_after_restart() {
        let temp = tempfile::tempdir().expect("temp");
        let (first, _, _, _) = counting_state(&temp);
        let created = created_json(&first, &test_request()).await;
        let id = Uuid::parse_str(created["instance"]["id"].as_str().expect("sandbox id"))
            .expect("sandbox UUID");
        let original_owner = first
            .manager
            .get(id)
            .expect("created lifecycle")
            .data_plane_lease
            .expect("durable data-plane lease")
            .provider_instance_id;
        drop(first);

        let (restarted, _, _, _) = counting_state(&temp);
        let loaded_owner = restarted
            .manager
            .get(id)
            .expect("reloaded lifecycle")
            .data_plane_lease
            .expect("reloaded data-plane lease")
            .provider_instance_id;
        assert_eq!(loaded_owner, original_owner);

        let report = restarted
            .manager
            .reconcile_startup()
            .await
            .expect("the standard provider must retain its identity across restart");
        assert_eq!(report.attempted, 1);
        assert!(
            report
                .failures
                .iter()
                .all(|failure| !failure.error.contains("another provider"))
        );
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
    async fn acquire_failure_is_released_through_its_durable_owner() {
        let temp = tempfile::tempdir().expect("temp");
        let state = mock_state(&temp);
        let acquire_hook = crate::failpoint::TestFailpoint::new(&[
            "storage-acquire-artifacts",
            "storage-acquire-rollback",
        ]);
        let error = acquire_hook
            .run(create_sandbox(&state, &test_request()))
            .await
            .expect_err("failed acquire must report provider unavailability");
        assert!(
            matches!(
                error,
                BlazeDaemonError::DataPlane(ProviderError::Unavailable)
            ),
            "unexpected create failure: {error:?}"
        );

        let instance = state
            .instances
            .lock()
            .expect("instances")
            .values()
            .next()
            .cloned()
            .expect("terminal create record");
        assert_eq!(instance.state, SandboxState::Destroyed);
        assert_eq!(instance.backend_ownership, BackendOwnership::Stopped);
        assert!(instance.operation.is_none());
        assert!(instance.data_plane_lease.is_none());
        assert!(instance.backend_runtime.is_none());
        assert!(
            !temp
                .path()
                .join("instances")
                .join(instance.id.to_string())
                .exists()
        );
        assert!(
            !temp
                .path()
                .join("instances/.blaze-storage-ownership")
                .join(format!("{}.json", instance.id))
                .exists()
        );
        assert!(matches!(
            state.state_store.run_dir(instance.id),
            Err(BlazeDaemonError::NotFound(_))
        ));
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
            attach_finalized_file_lease(
                storage.clone(),
                &config.daemon.state_dir,
                &mut instance,
                64,
                32,
            )
            .await;
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

        let report = state
            .manager
            .reconcile_startup()
            .await
            .expect("startup reconciliation");

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
            let mut owner = SandboxInstance::new(
                BackendKind::Mock,
                WorkloadClass::AgentTool,
                "sha256:legacy".into(),
                "legacy".into(),
            );
            owner.id = id;
            owner.transition(SandboxState::Creating).expect("creating");
            owner.transition(SandboxState::Running).expect("running");
            owner.backend_ownership = BackendOwnership::Running;
            attach_finalized_file_lease(
                storage.clone(),
                &config.daemon.state_dir,
                &mut owner,
                64,
                32,
            )
            .await;
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
                "backend_ownership": "running",
                "data_plane_lease": owner.data_plane_lease
            });
            let run_dir = config.daemon.state_dir.join(id.to_string());
            std::fs::write(
                run_dir.join("state.json"),
                serde_json::to_vec_pretty(&record).expect("legacy state JSON"),
            )
            .expect("legacy state record");
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

        let report = state
            .manager
            .reconcile_startup()
            .await
            .expect("startup reconciliation");

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
        attach_finalized_file_lease(
            storage.clone(),
            &config.daemon.state_dir,
            &mut not_started,
            64,
            32,
        )
        .await;
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
        attach_finalized_file_lease(
            storage.clone(),
            &config.daemon.state_dir,
            &mut stopped,
            64,
            32,
        )
        .await;
        stopped.persist(&config.daemon.state_dir).expect("persist");
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

        let report = state
            .manager
            .reconcile_startup()
            .await
            .expect("startup reconciliation");

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
                consumes_typed_opened_attachments: false,
            }))
        }

        async fn restore(
            &self,
            request: crate::spawner::BackendRestoreRequest,
        ) -> crate::spawner::RestoreResult {
            if request.provider_attachments.is_some() {
                return Err(SpawnFailure::clean(BlazeError::BackendError {
                    msg: "template test restore does not consume typed opened attachments"
                        .to_string(),
                }));
            }
            let storage = request.storage.clone().ok_or_else(|| {
                SpawnFailure::clean(BlazeError::BackendError {
                    msg: "template observation requires path-backed storage".to_string(),
                })
            })?;
            let observed = ObservedTemplateRestore {
                instance_id: request.instance_id,
                preserve_network: request.preserve_network,
                snapshot: tokio::fs::read(request.payload_dir.join("vmstate.snap"))
                    .await
                    .map_err(SpawnFailure::from)?,
                memory: tokio::fs::read(request.payload_dir.join("memory.snap"))
                    .await
                    .map_err(SpawnFailure::from)?,
                rootfs: tokio::fs::read(&storage.rootfs_path)
                    .await
                    .map_err(SpawnFailure::from)?,
            };
            *self.observed.lock().expect("template observation") = Some(observed);
            let spawn = BackendSpawnRequest::new(
                blaze_core::backend::SpawnRequest {
                    instance_id: request.instance_id,
                    binary_path: request.binary_path.clone(),
                    storage: Some(storage),
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
        opened_restore_resources: bool,
    ) -> (
        Arc<ServerState>,
        Arc<std::sync::Mutex<Option<ObservedTemplateRestore>>>,
        DaemonConfig,
        Arc<ManagedStorageToggleProvider>,
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
        let data_plane = Arc::new(ManagedStorageToggleProvider::new(storage.clone()));
        data_plane.set_opened_restore_resources(opened_restore_resources);
        let observed = Arc::new(std::sync::Mutex::new(None));
        let mut policy = test_policy(BackendKind::Mock);
        if allowed {
            policy.select.templates =
                vec!["runtime-base".to_string(), "missing-template".to_string()];
        }
        let state = build_test_state_with_provider(
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
            data_plane.clone(),
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
        (state, observed, config, data_plane)
    }

    #[tokio::test]
    async fn template_create_restores_independent_sandboxes() {
        let temp = tempfile::tempdir().expect("temp");
        let (state, observed, config, _data_plane) =
            template_test_state(&temp, true, false, false).await;
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
        let (state, observed, config, _data_plane) =
            template_test_state(&temp, false, false, false).await;
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
        assert_eq!(uuid_directory_count(&instances_dir), 0);
    }

    #[tokio::test]
    async fn ordinary_create_rejects_undeclared_image_support_before_lifecycle_state() {
        let temp = tempfile::tempdir().expect("temp");
        let config = test_config(&temp);
        let storage_instances_dir = config.storage.instances_dir.clone();
        let storage: Arc<dyn StorageProvider> = Arc::new(FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        ));
        let provider = Arc::new(ManagedStorageToggleProvider::new(storage.clone()));
        provider.set_images(false);
        let state = build_test_state_with_provider(
            config,
            test_policy(BackendKind::Mock),
            spawners(BackendKind::Mock, Arc::new(MockSpawner)),
            BackendKind::Mock,
            storage,
            provider.clone(),
        );

        let error = create_sandbox(&state, &test_request())
            .await
            .expect_err("ordinary images require an explicit provider capability");

        assert!(matches!(error, BlazeDaemonError::UnsupportedOperation(_)));
        assert!(
            error
                .to_string()
                .contains("does not support ordinary images")
        );
        assert_eq!(provider.prepare_calls(), 0);
        assert!(state.manager.list().expect("instances").is_empty());
        assert_eq!(uuid_directory_count(&storage_instances_dir), 0);
        assert_eq!(uuid_directory_count(&configured_state_dir(&state)), 0);
    }

    #[tokio::test]
    async fn template_create_rejects_unconsumable_opened_resources_before_provider_prepare() {
        let temp = tempfile::tempdir().expect("temp");
        let (state, observed, config, data_plane) =
            template_test_state(&temp, true, false, true).await;
        let instances_dir = config.storage.instances_dir.clone();
        assert_eq!(data_plane.prepare_calls(), 0);

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
        .expect_err("the selected backend cannot consume opened resources");

        assert!(matches!(error, BlazeDaemonError::UnsupportedOperation(_)));
        assert_eq!(
            data_plane.prepare_calls(),
            0,
            "provider preparation must not run before backend compatibility is proven"
        );
        assert!(observed.lock().expect("observation").is_none());
        assert!(state.manager.list().expect("instances").is_empty());
        assert_eq!(uuid_directory_count(&instances_dir), 0);
    }

    #[tokio::test]
    async fn template_create_rejects_mismatched_image_without_lifecycle_state() {
        let temp = tempfile::tempdir().expect("temp");
        let (state, observed, config, _data_plane) =
            template_test_state(&temp, true, false, false).await;
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
        assert_eq!(uuid_directory_count(&instances_dir), 0);
    }

    #[tokio::test]
    async fn template_create_rejects_unsupported_mock_guest_socket_without_lifecycle_state() {
        let temp = tempfile::tempdir().expect("temp");
        let (state, observed, config, _data_plane) =
            template_test_state(&temp, true, true, false).await;
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
        assert_eq!(uuid_directory_count(&instances_dir), 0);
    }
}
