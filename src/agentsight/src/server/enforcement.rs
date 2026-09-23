//! HTTP boundary for local enforcement control and evidence queries.

#[cfg(unix)]
use std::ffi::OsStr;
use std::fs;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use actix_web::{HttpResponse, delete, get, post, web};
use agentsight_enforcement_protocol::{
    ApplyCredentialPolicy, ApplyPolicy, Binding, BindingState, CredentialExfiltrationPolicy,
    DestinationScope, HealthStatus, PolicyMode,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use super::AppState;
use crate::enforcement::{
    EnforcementCoordinatorError, canonical_policy_file, read_process_start_time,
    resolve_and_read_target, resolve_to_host_pid,
};

/// Bounded evidence list query.
#[derive(Debug, Deserialize)]
pub(super) struct ViolationQuery {
    /// Maximum returned events, clamped to `1..=1000`.
    limit: Option<usize>,
}

const FILE_POLICY_REVISION: &str = "agentsight-file-open-v1";
const CREDENTIAL_POLICY_ID: &str = "agentsight-credential-exfiltration";

/// Product-level fields for binding a sensitive file to an agent process.
#[derive(Debug, Deserialize)]
pub(super) struct FileBindingRequest {
    agent_id: String,
    session_id: Option<String>,
    root_pid: i32,
    path: PathBuf,
}

/// Product fields for a taint-aware credential exfiltration binding.
#[derive(Debug, Deserialize)]
pub(super) struct CredentialBindingRequest {
    agent_id: String,
    session_id: Option<String>,
    root_pid: i32,
    source_path: Option<PathBuf>,
    #[serde(default)]
    source_paths: Vec<PathBuf>,
    trusted_endpoint: Option<String>,
    #[serde(default)]
    trusted_endpoints: Vec<String>,
    revision: u64,
    mode: PolicyMode,
    taint_ttl_secs: Option<u64>,
    destination_scope: DestinationScope,
}

#[derive(Debug, Deserialize)]
pub(super) struct ProtectionPreviewQuery {
    directory: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
struct ProtectionPreview {
    agent_id: String,
    root_pid: u32,
    workspace_path: String,
    source_paths: Vec<String>,
    trusted_endpoints: Vec<String>,
    mode: PolicyMode,
    max_trusted_endpoints: usize,
}

/// Returns safe, content-free defaults for the Agent-card protection action.
#[get("/enforcement/agent-protection/{pid}")]
pub(super) async fn preview_agent_protection(
    data: web::Data<AppState>,
    pid: web::Path<u32>,
    query: web::Query<ProtectionPreviewQuery>,
) -> HttpResponse {
    let pid = pid.into_inner();
    let agent = data.health_store.read().ok().and_then(|store| {
        store
            .all_agents()
            .into_iter()
            .find(|agent| agent.pid == pid)
    });
    let Some(agent) = agent else {
        return error_response(
            actix_web::http::StatusCode::NOT_FOUND,
            "agent_not_found",
            "agent process was not found",
            false,
        );
    };
    let using_default_directory = query.directory.is_none();
    if using_default_directory {
        if let Some(coordinator) = data.enforcement.clone() {
            let active_policy = web::block(move || {
                let binding_id = coordinator
                    .bindings()?
                    .into_iter()
                    .filter(|binding| is_active_credential_binding(binding, pid))
                    .max_by_key(|binding| {
                        binding
                            .request
                            .policy_revision
                            .parse::<u64>()
                            .unwrap_or_default()
                    })
                    .map(|binding| binding.request.binding_id);
                match binding_id {
                    Some(binding_id) => coordinator.credential_policy_snapshot(binding_id),
                    None => Ok(None),
                }
            })
            .await;
            match active_policy {
                Ok(Ok(Some(snapshot))) => match snapshot.policy() {
                    Ok(policy) => {
                        if let Some(preview) = protection_preview_from_policy(
                            agent.agent_name.clone(),
                            pid,
                            agent.workspace_path.as_deref().map(Path::new),
                            policy,
                        ) {
                            return HttpResponse::Ok().json(preview);
                        }
                    }
                    Err(error) => {
                        log::warn!("ignoring invalid credential policy preview: {error}");
                    }
                },
                Ok(Ok(None)) => {}
                Ok(Err(error)) => {
                    log::warn!("could not load active Agent protection policy: {error}");
                }
                Err(error) => {
                    log::warn!("could not join Agent protection policy lookup: {error}");
                }
            }
        }
    }
    let Some(workspace) = agent.workspace_path.as_deref().map(PathBuf::from) else {
        return error_response(
            actix_web::http::StatusCode::CONFLICT,
            "workspace_unavailable",
            "agent workspace is unavailable",
            true,
        );
    };
    let root = query.directory.as_deref().unwrap_or(&workspace);
    let root = match root.canonicalize() {
        Ok(root) if root.is_dir() && root != Path::new("/") => root,
        Ok(_) => {
            return error_response(
                actix_web::http::StatusCode::BAD_REQUEST,
                "unsafe_protection_directory",
                "protection directory cannot be the filesystem root",
                false,
            );
        }
        _ => {
            return error_response(
                actix_web::http::StatusCode::BAD_REQUEST,
                "invalid_protection_directory",
                "protection directory must be an existing directory",
                false,
            );
        }
    };
    // Run the recursive filesystem scan on the blocking pool so a large or slow
    // directory never ties up an async Actix worker. HOME is only a fallback for
    // an empty default-workspace scan; an explicit directory remains authoritative.
    let workspace_path = root.to_string_lossy().into_owned();
    let fallback_pid = using_default_directory.then_some(pid);
    let scan_root = root.clone();
    let source_paths = web::block(move || {
        let fallback_home =
            fallback_pid.and_then(|pid| validated_process_home(Path::new("/proc"), pid));
        discover_sensitive_files_with_fallback(&scan_root, fallback_home.as_deref(), 3, 64)
    })
    .await
    .unwrap_or_else(|error| {
        log::warn!("sensitive-file scan worker failed: {error}");
        Vec::new()
    });
    HttpResponse::Ok().json(ProtectionPreview {
        agent_id: agent.agent_name,
        root_pid: pid,
        workspace_path,
        source_paths,
        trusted_endpoints: Vec::new(),
        mode: PolicyMode::Audit,
        max_trusted_endpoints: 1,
    })
}

fn is_active_credential_binding(binding: &Binding, pid: u32) -> bool {
    binding.request.root_pid == pid as i32
        && binding.request.policy_id == CREDENTIAL_POLICY_ID
        && matches!(
            binding.state,
            BindingState::Pending | BindingState::Enforced | BindingState::Degraded
        )
}

fn protection_preview_from_policy(
    agent_id: String,
    root_pid: u32,
    workspace: Option<&Path>,
    policy: &CredentialExfiltrationPolicy,
) -> Option<ProtectionPreview> {
    let workspace = workspace
        .filter(|path| path.is_absolute() && *path != Path::new("/"))
        .map(Path::to_path_buf);
    let source_paths = workspace
        .as_ref()
        .map(|root| {
            policy
                .source_patterns
                .iter()
                .filter(|path| Path::new(path).starts_with(root))
                .cloned()
                .collect::<Vec<_>>()
        })
        .filter(|paths| !paths.is_empty())
        .unwrap_or_else(|| policy.source_patterns.clone());
    let workspace_path = workspace
        .filter(|root| {
            source_paths
                .iter()
                .all(|path| Path::new(path).starts_with(root))
        })
        .or_else(|| common_source_directory(&source_paths))
        .filter(|path| path.is_absolute() && path != Path::new("/"))?;

    Some(ProtectionPreview {
        agent_id,
        root_pid,
        workspace_path: workspace_path.to_string_lossy().into_owned(),
        source_paths,
        trusted_endpoints: policy.trusted_endpoints.clone(),
        mode: policy.mode,
        max_trusted_endpoints: 1,
    })
}

fn common_source_directory(source_paths: &[String]) -> Option<PathBuf> {
    let mut common = Path::new(source_paths.first()?).parent()?.to_path_buf();
    while !source_paths
        .iter()
        .all(|path| Path::new(path).starts_with(&common))
    {
        if !common.pop() {
            return Some(PathBuf::from("/"));
        }
    }
    Some(common)
}

#[cfg(unix)]
fn parse_process_uid(status: &str) -> Option<u32> {
    status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

#[cfg(unix)]
fn parse_home_from_environ(environ: &[u8]) -> Option<PathBuf> {
    let mut homes = environ
        .split(|byte| *byte == 0)
        .filter_map(|entry| entry.strip_prefix(b"HOME="))
        .filter(|value| !value.is_empty());
    let home = homes.next()?;
    if homes.next().is_some() {
        return None;
    }
    Some(PathBuf::from(OsStr::from_bytes(home)))
}

#[cfg(unix)]
fn validated_process_home(proc_root: &Path, pid: u32) -> Option<PathBuf> {
    let process_dir = proc_root.join(pid.to_string());
    let process_uid = parse_process_uid(&fs::read_to_string(process_dir.join("status")).ok()?)?;
    let home = parse_home_from_environ(&fs::read(process_dir.join("environ")).ok()?)?;
    if !home.is_absolute() || home == Path::new("/") {
        return None;
    }
    let home = home.canonicalize().ok()?;
    if home == Path::new("/") {
        return None;
    }
    let metadata = fs::metadata(&home).ok()?;
    if !metadata.is_dir() || metadata.uid() != process_uid {
        return None;
    }
    Some(home)
}

#[cfg(not(unix))]
fn validated_process_home(_proc_root: &Path, _pid: u32) -> Option<PathBuf> {
    None
}

fn discover_sensitive_files_with_fallback(
    root: &Path,
    fallback: Option<&Path>,
    max_depth: usize,
    limit: usize,
) -> Vec<String> {
    let mut files = discover_sensitive_files(root, max_depth, limit);
    if files.is_empty()
        && let Some(fallback) = fallback.filter(|fallback| *fallback != root)
    {
        files.extend(discover_sensitive_files(fallback, max_depth, limit));
        files.sort();
        files.dedup();
        files.truncate(limit);
    }
    files
}

/// Returns true when `name` looks like a real credential/secret file rather than
/// source code that merely mentions credentials in its filename.
///
/// The previous heuristic used `name.contains("credential")` which matched
/// `credential_pool.py`, `test_credential_*.py`, and `.pyc` bytecode across an
/// agent's source tree, drowning out actual `.env` files.
fn is_sensitive_file(name: &str) -> bool {
    // Reject source / bytecode / cache files regardless of stem.
    const CODE_EXTS: &[&str] = &[
        "py", "pyc", "pyi", "pyo", "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "go", "java", "c",
        "cc", "cpp", "h", "hpp", "rb", "php", "sh", "bash", "zsh", "toml", "md", "html", "css",
        "lock", "map",
    ];
    if let Some((_, ext)) = name.rsplit_once('.') {
        if CODE_EXTS.iter().any(|e| e.eq_ignore_ascii_case(ext)) {
            return false;
        }
    }
    if name.starts_with("test_") || name.ends_with("_test") {
        return false;
    }

    // Positive matches: real dotenv / credential / private-key style filenames.
    name == ".env"
        || name.starts_with(".env.")
        || name == ".npmrc"
        || name == ".pypirc"
        || name == ".netrc"
        || name == "credentials"
        || name == "credentials.json"
        || name == "credentials.yaml"
        || name == "credentials.yml"
        || name == "auth.json"
        || name.starts_with("id_rsa")
        || name.starts_with("id_ed25519")
        || name.starts_with("id_ecdsa")
        || name.starts_with("id_dsa")
        || name.ends_with(".pem")
        || name.ends_with(".key")
        || name.ends_with(".p12")
        || name.ends_with(".pfx")
        // JSON/YAML config files whose filename contains a credential-shaped
        // word — e.g. `service-credential.json`, `gcp-secret.yaml`. Source
        // extensions have already returned false above, so this cannot re-admit
        // `.py`/`.rs`/... names that merely mention credentials.
        || ((name.ends_with(".json")
            || name.ends_with(".yaml")
            || name.ends_with(".yml"))
            && (name.contains("credential") || name.contains("secret")))
}

fn discover_sensitive_files(root: &Path, max_depth: usize, limit: usize) -> Vec<String> {
    fn visit(
        root: &Path,
        depth: usize,
        max_depth: usize,
        limit: usize,
        visited: &mut usize,
        out: &mut Vec<String>,
    ) {
        // Bound the total directory entries inspected so a huge or slow tree
        // with no matches still returns promptly instead of tying up the
        // scanning worker indefinitely.
        const MAX_ENTRIES_VISITED: usize = 20_000;
        if depth > max_depth || out.len() >= limit || *visited >= MAX_ENTRIES_VISITED {
            return;
        }
        let Ok(entries) = fs::read_dir(root) else {
            return;
        };
        for entry in entries.flatten() {
            if out.len() >= limit || *visited >= MAX_ENTRIES_VISITED {
                break;
            }
            *visited += 1;
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if path.is_dir() {
                if !matches!(
                    name.as_ref(),
                    ".git"
                        | "node_modules"
                        | "target"
                        | "dist"
                        | "build"
                        | ".cache"
                        | "__pycache__"
                        | ".venv"
                        | "venv"
                        | "site-packages"
                        | "tests"
                        | "test"
                        | "__tests__"
                ) {
                    visit(&path, depth + 1, max_depth, limit, visited, out);
                }
            } else if path.is_file() && is_sensitive_file(name.as_ref()) {
                out.push(path.to_string_lossy().into_owned());
            }
        }
    }
    let mut files = Vec::new();
    let mut visited = 0usize;
    visit(root, 0, max_depth, limit, &mut visited, &mut files);
    files.sort();
    files.dedup();
    files
}

/// Returns privileged backend readiness.
#[get("/enforcement/health")]
pub(super) async fn health(data: web::Data<AppState>) -> HttpResponse {
    let Some(coordinator) = data.enforcement.clone() else {
        return unavailable();
    };
    match web::block(move || coordinator.health()).await {
        Ok(Ok(health)) => HttpResponse::Ok().json(public_health(health)),
        Ok(Err(error)) => coordinator_error(error),
        Err(error) => blocking_error(error),
    }
}

/// Validates, persists, and applies one desired policy binding.
#[post("/enforcement/bindings")]
pub(super) async fn apply_binding(
    data: web::Data<AppState>,
    body: web::Json<ApplyPolicy>,
) -> HttpResponse {
    let Some(coordinator) = data.enforcement.clone() else {
        return unavailable();
    };
    if let Err(message) = validate_target_identity(body.root_pid, body.process_start_time) {
        log::warn!("rejecting enforcement target identity: {message}");
        return error_response(
            actix_web::http::StatusCode::BAD_REQUEST,
            "invalid_target",
            "target process identity is invalid",
            false,
        );
    }
    let mut request = body.into_inner();
    // Replace namespace-local PID with host PID so the enforcer seeds cap_task
    // with the PID that BPF handle_fork will actually use.
    let host_pid = resolve_to_host_pid(request.root_pid, request.process_start_time);
    if host_pid != request.root_pid {
        log::info!(
            "resolved namespace PID {} to host PID {}",
            request.root_pid,
            host_pid
        );
        request.root_pid = host_pid;
    }
    run_binding(move || coordinator.apply(request)).await
}

/// Builds and applies an AgentSight-owned file-open policy.
#[post("/enforcement/file-bindings")]
pub(super) async fn apply_file_binding(
    data: web::Data<AppState>,
    body: web::Json<FileBindingRequest>,
) -> HttpResponse {
    let Some(coordinator) = data.enforcement.clone() else {
        return unavailable();
    };
    let request = match build_file_binding(body.into_inner()) {
        Ok(request) => request,
        Err(message) => {
            log::warn!("rejecting file enforcement binding: {message}");
            return error_response(
                actix_web::http::StatusCode::BAD_REQUEST,
                "invalid_file_binding",
                "file enforcement binding is invalid",
                false,
            );
        }
    };
    run_binding(move || coordinator.apply(request)).await
}

/// Builds a product-level taint policy and delegates DSL compilation to the enforcer.
#[post("/enforcement/credential-bindings")]
pub(super) async fn apply_credential_binding(
    data: web::Data<AppState>,
    body: web::Json<CredentialBindingRequest>,
) -> HttpResponse {
    let Some(coordinator) = data.enforcement.clone() else {
        return unavailable();
    };
    let request = match build_credential_binding(body.into_inner()) {
        Ok(request) => request,
        Err(message) => {
            log::warn!("rejecting credential enforcement binding: {message}");
            return error_response(
                actix_web::http::StatusCode::BAD_REQUEST,
                "invalid_credential_binding",
                "credential enforcement binding is invalid",
                false,
            );
        }
    };
    let audit_service = Arc::clone(&data.audit_service);
    let policy = request.policy.clone();
    let registered =
        web::block(move || audit_service.register_policy_revision(&policy, unix_epoch_ns())).await;
    match registered {
        Ok(Ok(())) => {}
        Ok(Err(agentsight_audit::AuditError::PolicyRevisionConflict { .. })) => {
            return error_response(
                actix_web::http::StatusCode::CONFLICT,
                "policy_revision_conflict",
                "policy revision already exists with different contents",
                false,
            );
        }
        Ok(Err(error)) => {
            log::error!("credential policy revision persistence failed: {error}");
            return error_response(
                actix_web::http::StatusCode::SERVICE_UNAVAILABLE,
                "policy_revision_store_unavailable",
                "policy revision could not be persisted",
                true,
            );
        }
        Err(error) => return blocking_error(error),
    }
    run_binding(move || coordinator.apply_credential_policy(request)).await
}

fn unix_epoch_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .min(u128::from(u64::MAX)) as u64
}

/// Lists AgentSight's persisted desired binding states.
///
/// Detached bindings acknowledge successful deletion and are hidden so the
/// list reflects the bindings an operator can still act on. Each item keeps
/// its `state` field so failed or degraded bindings remain distinguishable.
#[get("/enforcement/bindings")]
pub(super) async fn list_bindings(data: web::Data<AppState>) -> HttpResponse {
    let Some(coordinator) = data.enforcement.clone() else {
        return unavailable();
    };
    match web::block(move || coordinator.active_bindings()).await {
        Ok(Ok(bindings)) => HttpResponse::Ok().json(json!({
            "bindings": bindings.into_iter().map(public_binding).collect::<Vec<_>>()
        })),
        Ok(Err(error)) => coordinator_error(error),
        Err(error) => blocking_error(error),
    }
}

/// Detaches one binding after a privileged-service acknowledgement.
#[delete("/enforcement/bindings/{binding_id}")]
pub(super) async fn detach_binding(
    data: web::Data<AppState>,
    binding_id: web::Path<Uuid>,
) -> HttpResponse {
    let binding_id = binding_id.into_inner();
    if let Some(containment) = data.containment.clone() {
        match web::block(move || containment.remove_binding(binding_id)).await {
            Ok(Ok(true)) => return HttpResponse::NoContent().finish(),
            Ok(Ok(false)) => {}
            Ok(Err(error)) => {
                log::error!("containment binding restoration failed: {error}");
                return error_response(
                    actix_web::http::StatusCode::SERVICE_UNAVAILABLE,
                    "containment_restore_failed",
                    "containment policy restoration failed",
                    true,
                );
            }
            Err(error) => return blocking_error(error),
        }
    }
    let Some(coordinator) = data.enforcement.clone() else {
        return unavailable();
    };
    match web::block(move || coordinator.detach(binding_id)).await {
        Ok(Ok(())) => HttpResponse::NoContent().finish(),
        Ok(Err(error)) => coordinator_error(error),
        Err(error) => blocking_error(error),
    }
}

/// Lists newest normalized violation facts.
#[get("/enforcement/violations")]
pub(super) async fn list_violations(
    data: web::Data<AppState>,
    query: web::Query<ViolationQuery>,
) -> HttpResponse {
    let Some(coordinator) = data.enforcement.clone() else {
        return unavailable();
    };
    let audit_service = Arc::clone(&data.audit_service);
    let limit = query.limit.unwrap_or(100).clamp(1, 1000);
    match web::block(move || coordinator.violations(limit)).await {
        Ok(Ok(violations)) => {
            // Use the precise risk_evidence_links table to map each violation's
            // event_id to the exact case that generated it, instead of the coarse
            // (agent, policy, revision) index which picks the latest case and
            // mis-links violations from earlier bursts.
            let event_ids: Vec<uuid::Uuid> = violations.iter().map(|v| v.event_id).collect();
            let case_map = audit_service
                .case_ids_for_events(&event_ids)
                .unwrap_or_default();
            let enriched: Vec<serde_json::Value> = violations
                .iter()
                .map(|v| {
                    let mut obj = serde_json::to_value(v).unwrap_or_default();
                    if let Some(case_id) = case_map.get(&v.event_id) {
                        obj["case_id"] = serde_json::json!(case_id.to_string());
                    }
                    obj
                })
                .collect();
            HttpResponse::Ok().json(json!({ "violations": enriched }))
        }
        Ok(Err(error)) => coordinator_error(error),
        Err(error) => blocking_error(error),
    }
}

async fn run_binding<F>(operation: F) -> HttpResponse
where
    F: FnOnce() -> Result<Binding, EnforcementCoordinatorError> + Send + 'static,
{
    match web::block(operation).await {
        Ok(Ok(binding)) => HttpResponse::Ok().json(public_binding(binding)),
        Ok(Err(error)) => coordinator_error(error),
        Err(error) => blocking_error(error),
    }
}

fn public_binding(mut binding: Binding) -> Binding {
    if let Some(message) = binding.message.take() {
        log::warn!(
            "redacting enforcement binding {} status detail: {message}",
            binding.request.binding_id
        );
        binding.message = Some(
            match binding.state {
                BindingState::Pending => "enforcement binding is pending",
                BindingState::Enforced => "enforcement binding is active",
                BindingState::Failed => "enforcement binding failed",
                BindingState::Degraded => "enforcement binding is degraded",
                BindingState::Detaching => "enforcement binding is detaching",
                BindingState::Detached => "enforcement binding is detached",
            }
            .into(),
        );
    }
    binding
}

fn public_health(mut status: HealthStatus) -> HealthStatus {
    if let Some(message) = status.message.take() {
        log::warn!(
            "redacting {} enforcement health detail: {message}",
            status.backend
        );
        if !status.ready {
            status.message = Some("enforcement backend is not ready".into());
        }
    }
    status
}

fn validate_target_identity(root_pid: i32, expected_start_time: u64) -> Result<(), String> {
    // resolve_and_read_target handles namespace PID translation + start-time check.
    let (_host_pid, actual_start_time) =
        resolve_and_read_target(root_pid, expected_start_time).map_err(|e| e.to_string())?;
    if actual_start_time != expected_start_time {
        return Err(format!(
            "PID {root_pid} start time changed: expected {expected_start_time}, found {actual_start_time}"
        ));
    }
    Ok(())
}

fn build_file_binding(request: FileBindingRequest) -> Result<ApplyPolicy, String> {
    let agent_id = request.agent_id.trim();
    if agent_id.is_empty() || agent_id.len() > 128 {
        return Err("agent_id must contain 1 to 128 characters".into());
    }
    let path = canonical_policy_file(&request.path).map_err(|error| error.to_string())?;
    let process_start_time =
        read_process_start_time(request.root_pid).map_err(|error| error.to_string())?;
    let binding_id = Uuid::new_v4();
    let path = path
        .to_str()
        .ok_or_else(|| "path must be valid UTF-8".to_string())?;
    let policy_dsl = format!(
        "source AGENT = exec \"**\"\n\
         rule agentsight-file-open:\n\
           block open file \"{path}\" if AGENT\n\
           because \"AgentSight sensitive file policy\"\n"
    );
    Ok(ApplyPolicy {
        binding_id,
        agent_id: agent_id.into(),
        session_id: request
            .session_id
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()),
        root_pid: request.root_pid,
        process_start_time,
        policy_id: format!("agentsight-file-open:{binding_id}"),
        policy_revision: FILE_POLICY_REVISION.into(),
        policy_dsl,
        policy_mode: Some(PolicyMode::Enforce),
    })
}

fn build_credential_binding(
    request: CredentialBindingRequest,
) -> Result<ApplyCredentialPolicy, String> {
    let agent_id = request.agent_id.trim();
    if agent_id.is_empty() || agent_id.len() > 128 {
        return Err("agent_id must contain 1 to 128 characters".into());
    }
    let mut requested_sources = request.source_paths;
    if let Some(source_path) = request.source_path {
        requested_sources.push(source_path);
    }
    if requested_sources.is_empty() {
        return Err("at least one source path is required".into());
    }
    let mut source_patterns = requested_sources
        .iter()
        .map(|path| canonical_policy_file(path).map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|path| {
            path.to_str()
                .map(str::to_owned)
                .ok_or_else(|| "source paths must be valid UTF-8".to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    source_patterns.sort();
    source_patterns.dedup();
    let process_start_time =
        read_process_start_time(request.root_pid).map_err(|error| error.to_string())?;
    let mut trusted_endpoints = request.trusted_endpoints;
    if let Some(endpoint) = request.trusted_endpoint {
        trusted_endpoints.push(endpoint);
    }
    trusted_endpoints = trusted_endpoints
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect();
    trusted_endpoints.sort();
    trusted_endpoints.dedup();
    if trusted_endpoints.len() > 1 {
        return Err("the current runtime supports at most one trusted network target".into());
    }
    let policy = CredentialExfiltrationPolicy {
        policy_id: "agentsight-credential-exfiltration".into(),
        revision: request.revision,
        source_patterns,
        trusted_endpoints,
        taint_label: "CREDENTIAL".into(),
        taint_ttl_secs: request.taint_ttl_secs.unwrap_or(900),
        destination_scope: request.destination_scope,
        mode: request.mode,
    };
    policy.validate().map_err(|error| error.to_string())?;
    Ok(ApplyCredentialPolicy {
        binding_id: Uuid::new_v4(),
        agent_id: agent_id.into(),
        session_id: request
            .session_id
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()),
        root_pid: request.root_pid,
        process_start_time,
        policy,
    })
}

fn coordinator_error(error: EnforcementCoordinatorError) -> HttpResponse {
    log::error!("enforcement coordinator request failed: {error}");
    let (error, persisted) = match error {
        EnforcementCoordinatorError::PersistedUnacknowledged {
            binding_id,
            state,
            source,
        } => (*source, Some((binding_id, state))),
        error => (error, None),
    };
    let (status, code, message, retryable) = match &error {
        EnforcementCoordinatorError::IngestionUnavailable => (
            actix_web::http::StatusCode::SERVICE_UNAVAILABLE,
            "enforcement_ingestion_unavailable",
            "enforcement evidence ingestion is unavailable",
            true,
        ),
        EnforcementCoordinatorError::EnforcementUnavailable(_) => (
            actix_web::http::StatusCode::SERVICE_UNAVAILABLE,
            "enforcement_unavailable",
            "enforcement service is unavailable",
            true,
        ),
        EnforcementCoordinatorError::Store(
            crate::enforcement::EnforcementStoreError::MissingBinding(_),
        ) => (
            actix_web::http::StatusCode::NOT_FOUND,
            "binding_not_found",
            "enforcement binding was not found",
            false,
        ),
        EnforcementCoordinatorError::Client(crate::enforcement::EnforcementError::Remote {
            code,
            ..
        }) if matches!(code.as_str(), "compile_failure" | "stale_process") => (
            actix_web::http::StatusCode::UNPROCESSABLE_ENTITY,
            code.as_str(),
            "enforcement policy was rejected",
            false,
        ),
        EnforcementCoordinatorError::Client(crate::enforcement::EnforcementError::Remote {
            code,
            ..
        }) if code == "binding_conflict" => (
            actix_web::http::StatusCode::CONFLICT,
            "binding_conflict",
            "enforcement binding conflicts with the current state",
            false,
        ),
        EnforcementCoordinatorError::Client(crate::enforcement::EnforcementError::Remote {
            code,
            ..
        }) if code == "missing_binding" => (
            actix_web::http::StatusCode::NOT_FOUND,
            "binding_not_found",
            "enforcement binding was not found",
            false,
        ),
        EnforcementCoordinatorError::Client(_) => (
            actix_web::http::StatusCode::SERVICE_UNAVAILABLE,
            "enforcer_unavailable",
            "enforcement service is unavailable",
            true,
        ),
        _ => (
            actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
            "enforcement_error",
            "enforcement operation failed",
            false,
        ),
    };
    if let Some((binding_id, state)) = persisted
        && status == actix_web::http::StatusCode::SERVICE_UNAVAILABLE
    {
        return persisted_error_response(status, code, message, retryable, binding_id, state);
    }
    error_response(status, code, message, retryable)
}

fn unavailable() -> HttpResponse {
    error_response(
        actix_web::http::StatusCode::SERVICE_UNAVAILABLE,
        "enforcement_disabled",
        "enforcement coordinator is not configured",
        true,
    )
}

fn blocking_error(error: actix_web::error::BlockingError) -> HttpResponse {
    log::error!("enforcement blocking worker failed: {error}");
    error_response(
        actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
        "blocking_worker_failed",
        "enforcement worker failed",
        true,
    )
}

fn error_response(
    status: actix_web::http::StatusCode,
    code: &str,
    message: &str,
    retryable: bool,
) -> HttpResponse {
    HttpResponse::build(status).json(json!({
        "error": { "code": code, "message": message, "retryable": retryable }
    }))
}

/// Marks a 503 as "the desired state was persisted anyway".
///
/// Callers receiving this error can look up `binding_id` in the bindings list
/// instead of assuming the failed request left no trace.
fn persisted_error_response(
    status: actix_web::http::StatusCode,
    code: &str,
    message: &str,
    retryable: bool,
    binding_id: Uuid,
    state: BindingState,
) -> HttpResponse {
    HttpResponse::build(status).json(json!({
        "error": {
            "code": code,
            "message": message,
            "retryable": retryable,
            "persisted": true,
            "binding_id": binding_id,
            "state": state,
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_source_directory_finds_shared_parent() {
        let paths = vec![
            "/home/agent/.ssh/id_rsa".to_string(),
            "/home/agent/.ssh/id_ed25519".to_string(),
        ];
        assert_eq!(
            common_source_directory(&paths),
            Some(PathBuf::from("/home/agent/.ssh"))
        );
    }

    #[test]
    fn common_source_directory_falls_back_to_root_when_divergent() {
        let paths = vec![
            "/home/agent/.env".to_string(),
            "/etc/credentials.txt".to_string(),
        ];
        assert_eq!(common_source_directory(&paths), Some(PathBuf::from("/")));
    }

    #[test]
    fn common_source_directory_is_none_for_empty_input() {
        assert_eq!(common_source_directory(&[]), None);
    }

    #[test]
    fn discover_sensitive_files_selects_secrets_and_respects_limits() {
        let root = std::env::temp_dir().join(format!("agentsight-scan-{}", Uuid::new_v4()));
        fs::create_dir_all(root.join("node_modules")).expect("fixture dir should exist");
        fs::create_dir_all(root.join("nested")).expect("nested dir should exist");
        fs::write(root.join(".env"), b"SECRET=1").expect("env fixture");
        fs::write(root.join("id_rsa"), b"key").expect("key fixture");
        fs::write(root.join("README.md"), b"docs").expect("plain fixture");
        fs::write(root.join("node_modules/.env"), b"ignored").expect("ignored fixture");
        fs::write(root.join("nested/credentials.json"), b"c").expect("nested secret");

        let found = discover_sensitive_files(&root, 3, 64);
        assert!(found.iter().any(|p| p.ends_with(".env")));
        assert!(found.iter().any(|p| p.ends_with("id_rsa")));
        assert!(found.iter().any(|p| p.ends_with("credentials.json")));
        assert!(!found.iter().any(|p| p.ends_with("README.md")));
        // node_modules must be skipped entirely.
        assert!(!found.iter().any(|p| p.contains("node_modules")));

        // max_depth=0 must not descend into nested directories.
        let shallow = discover_sensitive_files(&root, 0, 64);
        assert!(!shallow.iter().any(|p| p.contains("nested")));

        // limit caps the number of returned entries.
        let limited = discover_sensitive_files(&root, 3, 1);
        assert_eq!(limited.len(), 1);

        // A non-existent root exercises the read_dir error branch.
        let missing = discover_sensitive_files(&root.join("does-not-exist"), 3, 64);
        assert!(missing.is_empty());

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn is_sensitive_file_matches_real_secrets_not_source() {
        // Real credential-shaped filenames must match.
        assert!(is_sensitive_file(".env"));
        assert!(is_sensitive_file(".env.local"));
        assert!(is_sensitive_file(".env.production"));
        assert!(is_sensitive_file(".netrc"));
        assert!(is_sensitive_file(".pypirc"));
        assert!(is_sensitive_file("credentials"));
        assert!(is_sensitive_file("credentials.json"));
        assert!(is_sensitive_file("auth.json"));
        // Real-world variants: JSON/YAML config whose stem carries the word
        // "credential" or "secret" — must still be picked up so we do not
        // regress the upstream expectation on files like `service-credential.json`.
        assert!(is_sensitive_file("service-credential.json"));
        assert!(is_sensitive_file("gcp-secret.yaml"));
        assert!(is_sensitive_file("id_rsa"));
        assert!(is_sensitive_file("id_ed25519.pub"));
        assert!(is_sensitive_file("server.pem"));
        assert!(is_sensitive_file("client.key"));

        // Source & bytecode named after credentials must NOT match because an
        // installed Agent source tree can otherwise produce dozens of false positives.
        assert!(!is_sensitive_file("credential_pool.py"));
        assert!(!is_sensitive_file("credential_pool.cpython-311.pyc"));
        assert!(!is_sensitive_file("credential_sources.py"));
        assert!(!is_sensitive_file("test_credential_pool.py"));
        assert!(!is_sensitive_file("test_credential_lifecycle.py"));
        assert!(!is_sensitive_file("credential_pool_admin.py"));
    }

    #[test]
    fn discover_sensitive_files_skips_source_trees_and_finds_env() {
        // A real `.env` sits next to a source subtree whose filenames merely
        // mention "credential". The scan must find the `.env` and skip every source
        // file, `__pycache__` entry, and anything under `tests/`.
        let root = std::env::temp_dir().join(format!("agentsight-scan-{}", Uuid::new_v4()));
        fs::create_dir_all(root.join("hermes-agent/agent/__pycache__")).expect("cache dir fixture");
        fs::create_dir_all(root.join("hermes-agent/tests/agent")).expect("tests dir fixture");
        fs::write(root.join(".env"), b"MAAS_API_KEY=x").expect("env fixture");
        fs::write(
            root.join("hermes-agent/agent/credential_pool.py"),
            b"# source",
        )
        .expect("source fixture");
        fs::write(
            root.join("hermes-agent/agent/__pycache__/credential_pool.cpython-311.pyc"),
            b"pyc",
        )
        .expect("pyc fixture");
        fs::write(
            root.join("hermes-agent/tests/agent/test_credential_pool.py"),
            b"# tests",
        )
        .expect("test fixture");

        let found = discover_sensitive_files(&root, 5, 64);

        assert!(
            found.iter().any(|p| p.ends_with("/.env")),
            "real .env should be picked up, got {found:?}"
        );
        assert!(
            !found.iter().any(|p| p.ends_with(".py")),
            "source files must not match, got {found:?}"
        );
        assert!(
            !found.iter().any(|p| p.ends_with(".pyc")),
            "bytecode must not match, got {found:?}"
        );
        assert!(
            !found.iter().any(|p| p.contains("__pycache__")),
            "__pycache__ subtree must be skipped, got {found:?}"
        );
        assert!(
            !found.iter().any(|p| p.contains("/tests/")),
            "tests subtree must be skipped, got {found:?}"
        );

        fs::remove_dir_all(&root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn validates_process_home_from_proc_metadata() {
        let root = std::env::temp_dir().join(format!("agentsight-proc-{}", Uuid::new_v4()));
        let proc_root = root.join("proc");
        let process_dir = proc_root.join("4242");
        let home = root.join("home");
        fs::create_dir_all(&process_dir).expect("process fixture dir");
        fs::create_dir_all(&home).expect("home fixture dir");
        let owner_uid = fs::metadata(&home).expect("home metadata").uid();
        fs::write(
            process_dir.join("status"),
            format!("Name:\tfixture\nUid:\t{owner_uid}\t{owner_uid}\t{owner_uid}\t{owner_uid}\n"),
        )
        .expect("status fixture");
        fs::write(
            process_dir.join("environ"),
            format!("PATH=/usr/bin\0HOME={}\0", home.display()).as_bytes(),
        )
        .expect("environ fixture");

        assert_eq!(
            validated_process_home(&proc_root, 4242),
            Some(home.canonicalize().expect("canonical home"))
        );

        // Relative, root, duplicate, nonexistent, and differently-owned HOME
        // values all fail closed instead of broadening a privileged scan.
        for value in ["relative/home", "/", "/does/not/exist"] {
            fs::write(
                process_dir.join("environ"),
                format!("HOME={value}\0").as_bytes(),
            )
            .expect("invalid environ fixture");
            assert_eq!(validated_process_home(&proc_root, 4242), None);
        }
        let root_link = root.join("root-link");
        std::os::unix::fs::symlink("/", &root_link).expect("root symlink fixture");
        fs::write(
            process_dir.join("environ"),
            format!("HOME={}\0", root_link.display()).as_bytes(),
        )
        .expect("symlinked root HOME fixture");
        assert_eq!(validated_process_home(&proc_root, 4242), None);

        fs::write(
            process_dir.join("environ"),
            format!("HOME={}\0HOME={}\0", home.display(), home.display()).as_bytes(),
        )
        .expect("duplicate HOME fixture");
        assert_eq!(validated_process_home(&proc_root, 4242), None);

        fs::write(
            process_dir.join("environ"),
            format!("HOME={}\0", home.display()).as_bytes(),
        )
        .expect("restored environ fixture");
        let other_uid = owner_uid.wrapping_add(1);
        fs::write(
            process_dir.join("status"),
            format!("Uid:\t{other_uid}\t{other_uid}\t{other_uid}\t{other_uid}\n"),
        )
        .expect("mismatched status fixture");
        assert_eq!(validated_process_home(&proc_root, 4242), None);

        fs::write(
            process_dir.join("status"),
            format!("Uid:\t{owner_uid}\t{owner_uid}\t{owner_uid}\t{owner_uid}\n"),
        )
        .expect("restored status fixture");
        fs::remove_file(process_dir.join("environ")).expect("environ fixture should be removed");
        assert_eq!(validated_process_home(&proc_root, 4242), None);

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn sensitive_file_scan_uses_home_only_as_empty_default_fallback() {
        let root = std::env::temp_dir().join(format!("agentsight-fallback-{}", Uuid::new_v4()));
        let workspace = root.join("workspace");
        let home = root.join("home");
        fs::create_dir_all(&workspace).expect("workspace fixture");
        fs::create_dir_all(&home).expect("home fixture");
        fs::write(home.join(".env"), b"SECRET=1").expect("home secret fixture");

        let fallback = discover_sensitive_files_with_fallback(&workspace, Some(&home), 3, 64);
        assert_eq!(fallback.len(), 1);
        assert!(fallback[0].ends_with("/home/.env"));

        // Supplying an explicit directory passes no fallback candidate; the
        // same empty directory therefore stays empty.
        let explicit = discover_sensitive_files_with_fallback(&workspace, None, 3, 64);
        assert!(explicit.is_empty());

        // A successful cwd scan remains authoritative and does not merge HOME.
        fs::write(workspace.join("auth.json"), b"{}").expect("workspace secret fixture");
        let primary = discover_sensitive_files_with_fallback(&workspace, Some(&home), 3, 64);
        assert_eq!(primary.len(), 1);
        assert!(primary[0].ends_with("/workspace/auth.json"));

        fs::remove_dir_all(&root).ok();
    }

    #[cfg(target_os = "linux")]
    #[actix_web::test]
    async fn preview_agent_protection_scans_workspace_and_reports_secrets() {
        use actix_web::http::StatusCode;
        use actix_web::{App, test as awtest, web};
        use std::sync::RwLock;
        use std::time::Instant;

        use super::super::{SecurityObservabilityConfig, configure_routes};
        use crate::config::ServerAuthConfig;
        use crate::grader::EvaluationStore;
        use crate::health::store::AgentRole;
        use crate::health::{AgentHealthState, AgentHealthStatus, HealthStore};
        use crate::server::auth::DashboardAuth;

        // A real workspace directory with a secret so discovery returns a hit.
        let workspace = std::env::temp_dir().join(format!("agentsight-preview-{}", Uuid::new_v4()));
        fs::create_dir_all(&workspace).expect("workspace should exist");
        fs::write(workspace.join(".env"), b"SECRET=1").expect("secret fixture");
        let canonical = workspace
            .canonicalize()
            .expect("workspace should canonicalize");

        // A live process whose cwd has no secrets but whose HOME does. This
        // exercises the real `/proc/<pid>/{environ,status}` fallback path.
        let fallback_root =
            std::env::temp_dir().join(format!("agentsight-home-preview-{}", Uuid::new_v4()));
        let empty_workspace = fallback_root.join("workspace");
        let process_home = fallback_root.join("home");
        fs::create_dir_all(&empty_workspace).expect("empty workspace should exist");
        fs::create_dir_all(&process_home).expect("process home should exist");
        fs::write(process_home.join(".env"), b"HOME_SECRET=1").expect("home secret fixture");
        let canonical_empty_workspace = empty_workspace
            .canonicalize()
            .expect("empty workspace should canonicalize");
        let canonical_process_home = process_home
            .canonicalize()
            .expect("process home should canonicalize");
        let mut fallback_process = std::process::Command::new("sleep")
            .arg("30")
            .current_dir(&canonical_empty_workspace)
            .env("HOME", &canonical_process_home)
            .spawn()
            .expect("fallback process should start");
        let fallback_pid = fallback_process.id();

        let pid = std::process::id();
        let agents_health = Arc::new(RwLock::new(HealthStore::new()));
        agents_health.write().unwrap().update(
            pid,
            AgentHealthStatus {
                pid,
                agent_name: "Scanner".into(),
                category: "test".into(),
                exe_path: "/usr/bin/sleep".into(),
                workspace_path: Some(canonical.to_string_lossy().into_owned()),
                ports: Vec::new(),
                status: AgentHealthState::Healthy,
                last_check_time: 1,
                latency_ms: None,
                error_message: None,
                restart_cmd: None,
                offline_since: None,
                role: AgentRole::Client,
                parent_pid: None,
                has_crash: false,
            },
        );
        agents_health.write().unwrap().update(
            fallback_pid,
            AgentHealthStatus {
                pid: fallback_pid,
                agent_name: "HomeScanner".into(),
                category: "test".into(),
                exe_path: "/usr/bin/sleep".into(),
                workspace_path: Some(canonical_empty_workspace.to_string_lossy().into_owned()),
                ports: Vec::new(),
                status: AgentHealthState::Healthy,
                last_check_time: 1,
                latency_ms: None,
                error_message: None,
                restart_cmd: None,
                offline_since: None,
                role: AgentRole::Client,
                parent_pid: None,
                has_crash: false,
            },
        );

        let auth_dir =
            std::env::temp_dir().join(format!("agentsight-preview-auth-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&auth_dir).expect("auth dir should exist");
        let state = web::Data::new(AppState {
            reuse_store: None,
            reuse_llm_judge_enabled: false,
            causal_store: None,
            storage_path: PathBuf::from(":memory:"),
            start_time: Instant::now(),
            health_store: Arc::clone(&agents_health),
            interruption_store: None,
            evaluation_store: Arc::new(
                EvaluationStore::new_with_path(Path::new(":memory:"))
                    .expect("evaluation fixture should open"),
            ),
            enforcement: None,
            containment: None,
            audit_service: Arc::new(agentsight_audit::AuditService::new(Arc::new(
                agentsight_audit::AuditStore::open_in_memory().expect("audit store should open"),
            ))),
            security_observability: SecurityObservabilityConfig::default(),
            auth: Arc::new(DashboardAuth::init(
                &ServerAuthConfig { enabled: false },
                &auth_dir,
            )),
            optimize: None,
            trajectory_store: Arc::new(RwLock::new(None)),
        });

        let app =
            awtest::init_service(App::new().app_data(state).configure(configure_routes)).await;

        // Unknown pid resolves to a 404 agent_not_found.
        let missing = awtest::call_service(
            &app,
            awtest::TestRequest::get()
                .uri("/api/enforcement/agent-protection/4294967295")
                .to_request(),
        )
        .await;
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);

        // Known pid scans its workspace and reports the discovered secret.
        let ok = awtest::call_service(
            &app,
            awtest::TestRequest::get()
                .uri(&format!("/api/enforcement/agent-protection/{pid}"))
                .to_request(),
        )
        .await;
        assert_eq!(ok.status(), StatusCode::OK);
        let body: serde_json::Value = awtest::read_body_json(ok).await;
        assert_eq!(body["root_pid"], pid);
        assert_eq!(body["mode"], "audit");
        assert!(
            body["source_paths"]
                .as_array()
                .expect("source_paths should be an array")
                .iter()
                .any(|p| p.as_str().is_some_and(|s| s.ends_with(".env")))
        );

        // With no explicit directory, an empty cwd scan falls back to the
        // process-owned HOME while preserving cwd as the displayed workspace.
        let fallback = awtest::call_service(
            &app,
            awtest::TestRequest::get()
                .uri(&format!("/api/enforcement/agent-protection/{fallback_pid}"))
                .to_request(),
        )
        .await;
        assert_eq!(fallback.status(), StatusCode::OK);
        let fallback_body: serde_json::Value = awtest::read_body_json(fallback).await;
        assert_eq!(
            fallback_body["workspace_path"],
            canonical_empty_workspace.to_string_lossy().as_ref()
        );
        assert_eq!(
            fallback_body["source_paths"],
            serde_json::json!([canonical_process_home.join(".env")])
        );

        // An explicit directory is authoritative and never expands to HOME.
        let explicit = awtest::call_service(
            &app,
            awtest::TestRequest::get()
                .uri(&format!(
                    "/api/enforcement/agent-protection/{fallback_pid}?directory={}",
                    canonical_empty_workspace.display()
                ))
                .to_request(),
        )
        .await;
        assert_eq!(explicit.status(), StatusCode::OK);
        let explicit_body: serde_json::Value = awtest::read_body_json(explicit).await;
        assert_eq!(explicit_body["source_paths"], serde_json::json!([]));

        fallback_process
            .kill()
            .expect("fallback process should stop");
        fallback_process
            .wait()
            .expect("fallback process should exit");
        fs::remove_dir_all(&fallback_root).ok();
        fs::remove_dir_all(&workspace).ok();
        fs::remove_dir_all(&auth_dir).ok();
    }

    #[test]
    fn builds_file_binding_from_product_fields() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("fixture process should start");
        let path = std::env::temp_dir().join(format!("agentsight-secret-{}", Uuid::new_v4()));
        fs::write(&path, b"fixture").expect("fixture file should exist");

        let binding = build_file_binding(FileBindingRequest {
            agent_id: " qoder ".into(),
            session_id: Some(" session-1 ".into()),
            root_pid: child.id() as i32,
            path: path.clone(),
        })
        .expect("valid request should build");

        assert_eq!(binding.agent_id, "qoder");
        assert_eq!(binding.session_id.as_deref(), Some("session-1"));
        assert_eq!(binding.root_pid, child.id() as i32);
        assert!(binding.process_start_time > 0);
        assert_eq!(binding.policy_revision, "agentsight-file-open-v1");
        assert!(binding.policy_id.starts_with("agentsight-file-open:"));
        assert!(binding.policy_dsl.contains("source AGENT = exec \"**\""));
        assert!(binding.policy_dsl.contains("block open file"));
        assert!(
            binding
                .policy_dsl
                .contains(path.canonicalize().unwrap().to_str().unwrap())
        );

        child.kill().expect("fixture process should stop");
        child.wait().expect("fixture process should exit");
        fs::remove_file(path).expect("fixture file should be removed");
    }

    #[test]
    fn builds_taint_aware_credential_binding() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("fixture process should start");
        let path = std::env::temp_dir().join(format!("agentsight-credential-{}", Uuid::new_v4()));
        fs::write(&path, b"fixture").expect("fixture file should exist");

        let binding = build_credential_binding(CredentialBindingRequest {
            agent_id: " qoder ".into(),
            session_id: Some(" session-1 ".into()),
            root_pid: child.id() as i32,
            source_path: Some(path.clone()),
            source_paths: Vec::new(),
            trusted_endpoint: Some(" 10.0.0.8 ".into()),
            trusted_endpoints: Vec::new(),
            revision: 3,
            mode: PolicyMode::Audit,
            taint_ttl_secs: None,
            destination_scope: DestinationScope::PublicIpv4,
        })
        .expect("valid credential policy should build");

        assert_eq!(binding.agent_id, "qoder");
        assert_eq!(binding.session_id.as_deref(), Some("session-1"));
        assert_eq!(binding.policy.mode, PolicyMode::Audit);
        assert_eq!(binding.policy.revision, 3);
        assert_eq!(binding.policy.taint_label, "CREDENTIAL");
        assert_eq!(binding.policy.taint_ttl_secs, 900);
        assert_eq!(
            binding.policy.destination_scope,
            DestinationScope::PublicIpv4
        );
        assert_eq!(binding.policy.trusted_endpoints, ["10.0.0.8"]);
        assert_eq!(
            binding.policy.source_patterns,
            [path.canonicalize().unwrap().to_str().unwrap()]
        );

        child.kill().expect("fixture process should stop");
        child.wait().expect("fixture process should exit");
        fs::remove_file(path).expect("fixture file should be removed");
    }

    #[test]
    fn discovers_workspace_sensitive_files_without_dependency_noise() {
        let directory =
            std::env::temp_dir().join(format!("agentsight-protection-{}", Uuid::new_v4()));
        let nested = directory.join("config");
        let dependency = directory.join("node_modules/package");
        fs::create_dir_all(&nested).expect("fixture directory should exist");
        fs::create_dir_all(&dependency).expect("dependency directory should exist");
        fs::write(directory.join(".env"), b"fixture").expect("env fixture should exist");
        fs::write(nested.join("service-credential.json"), b"fixture")
            .expect("credential fixture should exist");
        fs::write(dependency.join("credential.json"), b"fixture")
            .expect("dependency fixture should exist");
        fs::write(directory.join("README.md"), b"fixture").expect("ordinary fixture should exist");

        let discovered = discover_sensitive_files(&directory, 3, 64);

        assert_eq!(discovered.len(), 2);
        assert!(discovered.iter().any(|path| path.ends_with("/.env")));
        assert!(
            discovered
                .iter()
                .any(|path| path.ends_with("/config/service-credential.json"))
        );
        assert!(discovered.iter().all(|path| !path.contains("node_modules")));
        fs::remove_dir_all(directory).expect("fixture directory should be removed");
    }

    #[test]
    fn restores_agent_protection_preview_from_active_policy() {
        let policy = CredentialExfiltrationPolicy {
            policy_id: "agentsight-credential-exfiltration".into(),
            revision: 7,
            source_patterns: vec![
                "/root/agentsight-demo/.env".into(),
                "/root/agentsight-demo/config/credential.json".into(),
            ],
            trusted_endpoints: vec!["10.0.0.8:443".into()],
            taint_label: "CREDENTIAL".into(),
            taint_ttl_secs: 900,
            destination_scope: DestinationScope::PublicIpv4,
            mode: PolicyMode::Audit,
        };

        let preview = protection_preview_from_policy(
            "Hermes".into(),
            9073,
            Some(Path::new("/root/agentsight-demo")),
            &policy,
        )
        .expect("preview should be present");

        assert_eq!(preview.agent_id, "Hermes");
        assert_eq!(preview.root_pid, 9073);
        assert_eq!(preview.workspace_path, "/root/agentsight-demo");
        assert_eq!(preview.source_paths, policy.source_patterns);
        assert_eq!(preview.trusted_endpoints, ["10.0.0.8:443"]);
        assert_eq!(preview.mode, PolicyMode::Audit);
    }

    #[test]
    fn only_current_credential_bindings_restore_agent_protection() {
        let request = ApplyPolicy {
            binding_id: Uuid::new_v4(),
            agent_id: "Hermes".into(),
            session_id: None,
            root_pid: 9073,
            process_start_time: 1,
            policy_id: "agentsight-credential-exfiltration".into(),
            policy_revision: "7".into(),
            policy_dsl: "fixture".into(),
            policy_mode: Some(PolicyMode::Audit),
        };
        let active = Binding {
            request: request.clone(),
            state: BindingState::Enforced,
            message: None,
            domain_id: Some(1),
        };
        let detached = Binding {
            request,
            state: BindingState::Detached,
            message: None,
            domain_id: None,
        };

        assert!(is_active_credential_binding(&active, 9073));
        assert!(!is_active_credential_binding(&active, 9074));
        assert!(!is_active_credential_binding(&detached, 9073));
    }

    #[test]
    fn rejects_more_trusted_targets_than_the_runtime_supports() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("fixture process should start");
        let path = std::env::temp_dir().join(format!("agentsight-credential-{}", Uuid::new_v4()));
        fs::write(&path, b"fixture").expect("fixture file should exist");

        let error = build_credential_binding(CredentialBindingRequest {
            agent_id: "qoder".into(),
            session_id: None,
            root_pid: child.id() as i32,
            source_path: None,
            source_paths: vec![path.clone()],
            trusted_endpoint: None,
            trusted_endpoints: vec!["10.0.0.8:443".into(), "10.0.0.9:443".into()],
            revision: 1,
            mode: PolicyMode::Audit,
            taint_ttl_secs: None,
            destination_scope: DestinationScope::PublicIpv4,
        })
        .expect_err("multiple trusted targets must be rejected explicitly");

        assert!(error.contains("at most one trusted network target"));
        child.kill().expect("fixture process should stop");
        child.wait().expect("fixture process should exit");
        fs::remove_file(path).expect("fixture file should be removed");
    }

    #[test]
    fn rejects_unsafe_file_binding_inputs() {
        let directory = std::env::temp_dir();
        assert!(
            build_file_binding(FileBindingRequest {
                agent_id: "".into(),
                session_id: None,
                root_pid: 1,
                path: directory,
            })
            .is_err()
        );

        assert!(canonical_policy_file(std::path::Path::new("relative/secret")).is_err());
        assert!(canonical_policy_file(std::path::Path::new("/tmp/quote\"secret")).is_err());
    }

    #[test]
    fn rejects_self_file_binding_target() {
        let path = std::env::temp_dir().join(format!("agentsight-secret-{}", Uuid::new_v4()));
        fs::write(&path, b"fixture").expect("fixture file should exist");

        assert!(
            build_file_binding(FileBindingRequest {
                agent_id: "qoder".into(),
                session_id: None,
                root_pid: std::process::id() as i32,
                path: path.clone(),
            })
            .is_err()
        );

        fs::remove_file(path).expect("fixture file should be removed");
    }

    #[test]
    fn rejects_protected_service_file_binding_targets() {
        let directory =
            std::env::temp_dir().join(format!("agentsight-protected-{}", Uuid::new_v4()));
        fs::create_dir(&directory).expect("fixture directory should exist");
        let path = directory.join("secret");
        fs::write(&path, b"fixture").expect("fixture file should exist");

        for process_name in ["agentsight", "agentsight-enforcer"] {
            let executable = directory.join(process_name);
            std::os::unix::fs::symlink("/bin/sleep", &executable)
                .expect("protected-service fixture should exist");
            let mut child = std::process::Command::new(&executable)
                .arg("30")
                .spawn()
                .expect("fixture process should start");
            let expected_process_name: String = process_name.chars().take(15).collect();
            let mut actual_process_name = String::new();
            for _ in 0..10 {
                let stat = fs::read_to_string(format!("/proc/{}/stat", child.id()))
                    .expect("fixture proc stat should exist");
                let open = stat
                    .find('(')
                    .expect("fixture proc stat should contain open");
                let close = stat
                    .rfind(')')
                    .expect("fixture proc stat should contain close");
                actual_process_name = stat[open + 1..close].into();
                if actual_process_name == expected_process_name {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            assert_eq!(actual_process_name, expected_process_name);

            assert!(
                build_file_binding(FileBindingRequest {
                    agent_id: "qoder".into(),
                    session_id: None,
                    root_pid: child.id() as i32,
                    path: path.clone(),
                })
                .is_err()
            );

            child.kill().expect("fixture process should stop");
            child.wait().expect("fixture process should exit");
            fs::remove_file(executable).expect("protected-service fixture should be removed");
        }

        fs::remove_file(path).expect("fixture file should be removed");
        fs::remove_dir(directory).expect("fixture directory should be removed");
    }

    #[test]
    fn rejects_init_and_self_targets_before_uds_calls() {
        assert!(validate_target_identity(1, 0).is_err());
        assert!(validate_target_identity(std::process::id() as i32, 0).is_err());
    }

    #[test]
    fn accepts_live_target_and_rejects_pid_reuse_marker() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = child.id() as i32;
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
        let close = stat.rfind(')').unwrap();
        let start_time = stat[close + 1..]
            .split_whitespace()
            .nth(19)
            .unwrap()
            .parse::<u64>()
            .unwrap();

        assert!(validate_target_identity(pid, start_time).is_ok());
        assert!(validate_target_identity(pid, start_time + 1).is_err());
        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[test]
    fn maps_policy_rejections_to_actionable_http_statuses() {
        for (code, expected) in [
            (
                "compile_failure",
                actix_web::http::StatusCode::UNPROCESSABLE_ENTITY,
            ),
            (
                "stale_process",
                actix_web::http::StatusCode::UNPROCESSABLE_ENTITY,
            ),
            ("binding_conflict", actix_web::http::StatusCode::CONFLICT),
            ("missing_binding", actix_web::http::StatusCode::NOT_FOUND),
        ] {
            let response = coordinator_error(EnforcementCoordinatorError::Client(
                crate::enforcement::EnforcementError::Remote {
                    code: code.into(),
                    message: "fixture rejection".into(),
                },
            ));
            assert_eq!(response.status(), expected);
        }
    }

    #[actix_web::test]
    async fn maps_ingestion_unavailable_to_retryable_service_unavailable() {
        let response = coordinator_error(EnforcementCoordinatorError::IngestionUnavailable);
        assert_eq!(
            response.status(),
            actix_web::http::StatusCode::SERVICE_UNAVAILABLE
        );
        let body = actix_web::body::to_bytes(response.into_body())
            .await
            .expect("error response body should load");
        let value: serde_json::Value =
            serde_json::from_slice(&body).expect("error response should be JSON");
        assert_eq!(
            value["error"]["code"],
            serde_json::Value::String("enforcement_ingestion_unavailable".into())
        );
        assert_eq!(value["error"]["retryable"], serde_json::Value::Bool(true));
    }

    #[actix_web::test]
    async fn maps_combined_health_failure_to_retryable_service_unavailable() {
        let response = coordinator_error(EnforcementCoordinatorError::EnforcementUnavailable(
            "violation event delivery loss".into(),
        ));
        assert_eq!(
            response.status(),
            actix_web::http::StatusCode::SERVICE_UNAVAILABLE
        );
        let body = actix_web::body::to_bytes(response.into_body())
            .await
            .expect("error response body should load");
        let value: serde_json::Value =
            serde_json::from_slice(&body).expect("error response should be JSON");
        assert_eq!(
            value["error"]["code"],
            serde_json::Value::String("enforcement_unavailable".into())
        );
        assert_eq!(value["error"]["retryable"], serde_json::Value::Bool(true));
        // Health failures happen before persistence and must not claim otherwise.
        assert!(value["error"].get("persisted").is_none());
    }

    #[actix_web::test]
    async fn coordinator_errors_do_not_expose_backend_details() {
        let response = coordinator_error(EnforcementCoordinatorError::Client(
            crate::enforcement::EnforcementError::Remote {
                code: "compile_failure".into(),
                message: "socket /run/agentsight/private.sock rejected /root/credential".into(),
            },
        ));

        assert_eq!(
            response.status(),
            actix_web::http::StatusCode::UNPROCESSABLE_ENTITY
        );
        let body = actix_web::body::to_bytes(response.into_body())
            .await
            .expect("error response body should load");
        let text = String::from_utf8_lossy(&body);
        assert!(!text.contains("/run/agentsight/private.sock"));
        assert!(!text.contains("/root/credential"));
        let value: serde_json::Value =
            serde_json::from_slice(&body).expect("error response should be JSON");
        assert_eq!(value["error"]["code"], "compile_failure");
        assert_eq!(value["error"]["retryable"], false);
    }

    #[test]
    fn successful_binding_and_health_payloads_redact_internal_details() {
        let request = agentsight_enforcement_protocol::ApplyPolicy {
            binding_id: Uuid::new_v4(),
            agent_id: "agent-1".into(),
            session_id: None,
            root_pid: 42,
            process_start_time: 7,
            policy_id: "policy-1".into(),
            policy_revision: "revision-1".into(),
            policy_dsl: "label AGENT".into(),
            policy_mode: None,
        };
        let binding = public_binding(Binding {
            request,
            state: BindingState::Degraded,
            message: Some("socket /run/agentsight/private.sock failed".into()),
            domain_id: None,
        });
        let status = public_health(HealthStatus {
            ready: false,
            backend: "actplane".into(),
            capabilities: agentsight_enforcement_protocol::EnforcementCapabilities::actplane(),
            message: Some("database /root/private/enforcement.db failed".into()),
        });

        assert_eq!(
            binding.message.as_deref(),
            Some("enforcement binding is degraded")
        );
        assert_eq!(
            status.message.as_deref(),
            Some("enforcement backend is not ready")
        );
    }

    #[actix_web::test]
    async fn persisted_apply_failures_expose_persisted_state_on_503() {
        let binding_id = Uuid::new_v4();
        let response = coordinator_error(EnforcementCoordinatorError::PersistedUnacknowledged {
            binding_id,
            state: BindingState::Failed,
            source: Box::new(EnforcementCoordinatorError::Client(
                crate::enforcement::EnforcementError::Remote {
                    code: "internal_error".into(),
                    message: "fixture apply rejection".into(),
                },
            )),
        });

        assert_eq!(
            response.status(),
            actix_web::http::StatusCode::SERVICE_UNAVAILABLE
        );
        let body = actix_web::body::to_bytes(response.into_body())
            .await
            .expect("error response body should load");
        let value: serde_json::Value =
            serde_json::from_slice(&body).expect("error response should be JSON");
        assert_eq!(value["error"]["code"], "enforcer_unavailable");
        assert_eq!(value["error"]["persisted"], serde_json::Value::Bool(true));
        assert_eq!(value["error"]["state"], "failed");
        assert_eq!(
            value["error"]["binding_id"],
            serde_json::Value::String(binding_id.to_string())
        );
    }

    #[actix_web::test]
    async fn persisted_degraded_apply_failures_expose_state_on_503() {
        let response = coordinator_error(EnforcementCoordinatorError::PersistedUnacknowledged {
            binding_id: Uuid::new_v4(),
            state: BindingState::Degraded,
            source: Box::new(EnforcementCoordinatorError::EnforcementUnavailable(
                "fixture backend lost".into(),
            )),
        });

        assert_eq!(
            response.status(),
            actix_web::http::StatusCode::SERVICE_UNAVAILABLE
        );
        let body = actix_web::body::to_bytes(response.into_body())
            .await
            .expect("error response body should load");
        let value: serde_json::Value =
            serde_json::from_slice(&body).expect("error response should be JSON");
        assert_eq!(value["error"]["code"], "enforcement_unavailable");
        assert_eq!(value["error"]["persisted"], serde_json::Value::Bool(true));
        assert_eq!(value["error"]["state"], "degraded");
    }

    #[actix_web::test]
    async fn persisted_policy_rejections_keep_actionable_status_without_state_detail() {
        let response = coordinator_error(EnforcementCoordinatorError::PersistedUnacknowledged {
            binding_id: Uuid::new_v4(),
            state: BindingState::Failed,
            source: Box::new(EnforcementCoordinatorError::Client(
                crate::enforcement::EnforcementError::Remote {
                    code: "compile_failure".into(),
                    message: "fixture compile rejection".into(),
                },
            )),
        });

        assert_eq!(
            response.status(),
            actix_web::http::StatusCode::UNPROCESSABLE_ENTITY
        );
        let body = actix_web::body::to_bytes(response.into_body())
            .await
            .expect("error response body should load");
        let value: serde_json::Value =
            serde_json::from_slice(&body).expect("error response should be JSON");
        assert_eq!(value["error"]["code"], "compile_failure");
        assert!(value["error"].get("persisted").is_none());
    }
}
