//! Workspace snapshots attached to existing ATIF steps; file state stays in ws-ckpt.

use super::{AppState, secret, storage_data_dir};
use actix_web::http::StatusCode;
use actix_web::{HttpResponse, get, head, post, web};
use agentsight_atif::{AtifTrajectory, ObservationResult, StepSource, ToolCall, same_call_id};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use cosh_platform::checkpoint::{CkptClient, CkptRequestEffect};
use cosh_types::checkpoint::{
    CkptGuardedRollbackPreviewV2, DEFAULT_SOCKET_PATH, WorkspaceGenerationTokenV2,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

#[derive(Default, Deserialize)]
#[serde(default)]
struct Config {
    binary: Option<PathBuf>,
    socket: Option<String>,
    /// Explicit host-instance bindings, keyed by the recorded ATIF agent name.
    plugins: HashMap<String, Plugin>,
    restore_enabled: bool,
    #[serde(skip)]
    modified_at: Option<DateTime<Utc>>,
}

#[derive(Deserialize)]
struct Plugin {
    runtime: String,
    config_path: PathBuf,
    /// The actual host environment override/default, never Sight's HOME or cwd.
    workspace_override: Option<String>,
}

fn config(data: &AppState) -> anyhow::Result<Config> {
    let Some(path) = &data.config_path else {
        return Ok(Config::default());
    };
    let raw = std::fs::read_to_string(path)?;
    let value: Value = serde_json::from_str(crate::config::strip_leading_bom(&raw))?;
    let mut config: Config =
        serde_json::from_value(value.get("ws_ckpt").cloned().unwrap_or(json!({})))?;
    config.modified_at = Some(std::fs::metadata(path)?.modified()?.into());
    Ok(config)
}

impl Config {
    fn socket(&self) -> String {
        self.socket
            .clone()
            .or_else(|| std::env::var("WS_CKPT_SOCKET").ok())
            .unwrap_or_else(|| DEFAULT_SOCKET_PATH.into())
    }

    fn cli(&self, args: &[&str]) -> anyhow::Result<Value> {
        let mut cmd = Command::new(self.binary.as_deref().unwrap_or(Path::new("ws-ckpt")));
        cmd.args(args);
        cmd.env("WS_CKPT_SOCKET", self.socket());
        let output = cosh_platform::run_command_with_poll_interval(
            &mut cmd,
            Duration::from_secs(30),
            "ws-ckpt",
            Duration::from_millis(10),
        )?;
        anyhow::ensure!(
            output.status.success(),
            "ws-ckpt query failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(serde_json::from_slice(&output.stdout)?)
    }

    fn client(&self) -> CkptClient {
        CkptClient::new(&self.socket()).require_trusted_peer(nix_uid())
    }
}

fn nix_uid() -> u32 {
    // Reading euid has no memory or ownership preconditions.
    unsafe { libc::geteuid() }
}

fn succeeded(result: &ObservationResult, tool_name: &str) -> bool {
    if result.is_error() == Some(true) {
        return false;
    }
    let Some(content) = &result.content else {
        return false;
    };
    let parsed = content
        .as_str()
        .and_then(|text| serde_json::from_str::<Value>(text).ok());
    let value = parsed.as_ref().unwrap_or(content);
    if value.get("success").and_then(Value::as_bool) == Some(false) {
        return false;
    }
    if let Some(code) = value
        .get("exit_code")
        .or_else(|| value.get("exitCode"))
        .and_then(Value::as_i64)
    {
        return code == 0;
    }
    if tool_name == "run_shell_command"
        && let Some(text) = value.as_str().filter(|text| text.starts_with("Command: "))
    {
        // Qwen Code reports the actual exit status after command stdout.
        return text
            .rsplit_once("\nProcess Group PGID: ")
            .and_then(|(report, _)| report.rsplit_once("\nExit Code: "))
            .is_some_and(|(_, status)| status == "0\nSignal: (none)");
    }
    result.is_error() == Some(false)
        || value.get("success").and_then(Value::as_bool) == Some(true)
        || value.as_str().is_some_and(|text| {
            text.lines().any(|line| {
                line.trim()
                    .trim_start_matches("\x1b[32m")
                    .trim_start_matches('✓')
                    .trim_start()
                    .starts_with("Checkpoint created:")
            })
        })
}

fn explicit_workspace(call: &ToolCall) -> Option<String> {
    if call.function_name.starts_with("ws-ckpt-") {
        return call
            .arguments
            .get("workspace")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| {
                (call.arguments["action"] == "update" && call.arguments["key"] == "workspace")
                    .then(|| call.arguments["value"].as_str().map(str::to_owned))
                    .flatten()
            });
    }
    let words = literal_ws_ckpt_command(call)?;
    words.iter().enumerate().find_map(|(index, word)| {
        word.strip_prefix("--workspace=")
            .map(str::to_owned)
            .or_else(|| {
                matches!(word.as_str(), "-w" | "--workspace")
                    .then(|| words.get(index + 1).cloned())
                    .flatten()
            })
    })
}

fn literal_ws_ckpt_command(call: &ToolCall) -> Option<Vec<String>> {
    let words = literal_shell_command(call)?;
    (Path::new(words.first()?).file_name()?.to_str()? == "ws-ckpt").then_some(words)
}

fn literal_shell_command(call: &ToolCall) -> Option<Vec<String>> {
    if !matches!(
        call.function_name.to_ascii_lowercase().as_str(),
        "bash" | "shell" | "terminal" | "exec_command" | "execute" | "run_shell_command"
    ) {
        return None;
    }
    let command = call
        .arguments
        .get("command")
        .or_else(|| call.arguments.get("cmd"))?
        .as_str()?;
    // Only a single literal command is supported; never evaluate shell syntax.
    if command.contains(['$', '`', ';', '&', '|', '<', '>', '\n']) {
        return None;
    }
    shlex::split(command)
}

fn cosh_checkpoint_workspace(
    call: &ToolCall,
    result: &ObservationResult,
    cfg: &Config,
) -> Option<String> {
    if call.function_name != "shell" || result.is_error() == Some(true) {
        return None;
    }
    let words = literal_shell_command(call)?;
    if Path::new(words.first()?).file_name()?.to_str()? != "cosh-cli"
        || words.get(1)?.as_str() != "checkpoint"
        || words.get(2)?.as_str() != "create"
    {
        return None;
    }
    let socket = words
        .iter()
        .enumerate()
        .find_map(|(index, word)| {
            if word == "--socket" {
                words.get(index + 1).map(String::as_str)
            } else {
                word.strip_prefix("--socket=")
            }
        })
        .unwrap_or(DEFAULT_SOCKET_PATH);
    if socket != cfg.socket() {
        return None;
    }
    let content = result.content.as_ref()?;
    let parsed = content
        .as_str()
        .and_then(|text| serde_json::from_str(text).ok());
    let receipt = parsed.as_ref().unwrap_or(content);
    if receipt["ok"] != true
        || receipt["meta"]["subsystem"] != "checkpoint"
        || receipt["meta"]["dry_run"] != false
    {
        return None;
    }
    receipt["data"]["workspace"].as_str().map(str::to_owned)
}

fn workspace(doc: &AtifTrajectory, cfg: &Config) -> anyhow::Result<Option<String>> {
    let results: Vec<_> = doc
        .steps
        .iter()
        .filter_map(|s| s.observation.as_ref())
        .flat_map(|o| &o.results)
        .collect();
    for step in doc.steps.iter().rev() {
        for call in step.tool_calls.iter().flatten().rev() {
            let mut observations = results.iter().filter(|r| {
                r.source_call_id
                    .as_deref()
                    .is_some_and(|id| same_call_id(id, &call.tool_call_id))
            });
            if let Some(path) = observations
                .clone()
                .find_map(|r| cosh_checkpoint_workspace(call, r, cfg))
            {
                return Ok(Some(path));
            }
            if !observations.any(|r| succeeded(r, &call.function_name)) {
                continue;
            }
            if let Some(path) = explicit_workspace(call) {
                return Ok(Some(path));
            }
            if !call.function_name.starts_with("ws-ckpt-") {
                // Opaque Bash arguments retain the last known workspace for display.
                continue;
            }
            // A newer successful operation supersedes the old workspace even when unresolved.
            return plugin_workspace(cfg, &doc.agent.name, step.timestamp.as_deref(), false);
        }
    }
    // Automatic hooks need not appear as tool calls. Use the configuration
    // bound to this instance only if it was in place when the trajectory began.
    let started_at = doc.steps.first().and_then(|step| step.timestamp.as_deref());
    plugin_workspace(cfg, &doc.agent.name, started_at, true)
}

fn plugin_workspace(
    cfg: &Config,
    agent_name: &str,
    at: Option<&str>,
    auto_only: bool,
) -> anyhow::Result<Option<String>> {
    let (Some(plugin), Some(time)) = (cfg.plugins.get(agent_name), at.and_then(timestamp)) else {
        return Ok(None);
    };
    let modified: DateTime<Utc> = std::fs::metadata(&plugin.config_path)?.modified()?.into();
    if modified > time {
        return Ok(None);
    }
    if (auto_only || plugin.workspace_override.is_some())
        && cfg.modified_at.is_none_or(|modified| modified > time)
    {
        return Ok(None);
    }
    if !auto_only && plugin.workspace_override.is_some() {
        return Ok(plugin.workspace_override.clone());
    }
    let raw = std::fs::read_to_string(&plugin.config_path)?;
    let value: Value = match plugin.runtime.as_str() {
        "openclaw" => serde_json::from_str(&raw)?,
        "hermes" => serde_yaml::from_str(&raw)?,
        _ => anyhow::bail!("unsupported ws-ckpt plugin runtime"),
    };
    let value = if plugin.runtime == "hermes" {
        &value["plugins"]["ws-ckpt"]
    } else {
        &value
    };
    if auto_only && value["autoCheckpoint"] != true {
        return Ok(None);
    }
    Ok(plugin
        .workspace_override
        .clone()
        .or_else(|| value["workspace"].as_str().map(str::to_owned)))
}

fn timestamp(text: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|time| time.with_timezone(&Utc))
}

fn load(data: &AppState, kind: &str, id: &str) -> anyhow::Result<AtifTrajectory> {
    anyhow::ensure!(
        matches!(kind, "session" | "conversation" | "trajectory"),
        "invalid trajectory source"
    );
    if kind != "trajectory" {
        if let Some(store) = &data.genai_store {
            let events = if kind == "conversation" {
                store.get_events_by_conversation(id)
            } else {
                store.get_events_by_session(id)
            }
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            if !events.is_empty() {
                return (if kind == "conversation" {
                    crate::atif::convert_trace_to_atif(id, events)
                } else {
                    crate::atif::convert_session_to_atif(id, events)
                })
                .map_err(|error| anyhow::anyhow!(error.to_string()));
            }
        }
    }
    if kind != "conversation" {
        if let Some(store) = data.trajectory_store() {
            if let Some(raw) = store.get_atif_json(id)? {
                return Ok(serde_json::from_str(&raw)?);
            }
        }
    }
    anyhow::bail!("trajectory not found")
}

fn identify(cfg: &Config, selector: &str) -> anyhow::Result<Value> {
    let status = cfg.cli(&["status", "--workspace", selector, "--format", "json"])?;
    let rows = status["workspaces"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("invalid workspace status"))?;
    let row = rows
        .iter()
        .find(|row| row["path"] == selector || row["ws_id"] == selector)
        .ok_or_else(|| anyhow::anyhow!("workspace identity not found"))?;
    anyhow::ensure!(rows.len() == 1, "ambiguous workspace status");
    Ok(row.clone())
}

fn hermes_checkpoint_steps(
    doc: &AtifTrajectory,
    path: &str,
    workspace_id: &str,
) -> Vec<(usize, String, DateTime<Utc>)> {
    if doc.agent.name != "hermes" {
        return Vec::new();
    }
    let mut checkpoints = Vec::new();
    // Hermes may timestamp the entire turn after its tools ran.
    for step in doc
        .steps
        .iter()
        .filter(|step| step.source == StepSource::Agent)
    {
        for call in step.tool_calls.iter().flatten() {
            let is_checkpoint = call.function_name == "ws-ckpt-checkpoint"
                || literal_ws_ckpt_command(call)
                    .is_some_and(|words| words.get(1).is_some_and(|word| word == "checkpoint"));
            if !is_checkpoint
                || !matches!(explicit_workspace(call).as_deref(), Some(ws) if ws == path || ws == workspace_id)
            {
                continue;
            }
            let Some(result_at) = call
                .extra
                .as_ref()
                .and_then(|extra| extra.get("result_timestamp"))
                .and_then(Value::as_str)
                .and_then(timestamp)
            else {
                continue;
            };
            for result in step
                .observation
                .iter()
                .flat_map(|observation| &observation.results)
            {
                if !result
                    .source_call_id
                    .as_deref()
                    .is_some_and(|id| same_call_id(id, &call.tool_call_id))
                    || !succeeded(result, &call.function_name)
                {
                    continue;
                }
                let Some(content) = &result.content else {
                    continue;
                };
                let parsed = content
                    .as_str()
                    .and_then(|text| serde_json::from_str::<Value>(text).ok());
                let value = parsed.as_ref().unwrap_or(content);
                let output = value.get("output").unwrap_or(value).as_str().unwrap_or("");
                if let Some(id) = output.lines().find_map(|line| {
                    line.trim()
                        .trim_start_matches("\x1b[32m")
                        .trim_start_matches('✓')
                        .trim_start()
                        .strip_prefix("Checkpoint created: ")
                        .map(|id| id.trim_end_matches("\x1b[0m").trim())
                }) {
                    checkpoints.push((step.step_id, id.to_owned(), result_at));
                }
            }
        }
    }
    checkpoints
}

fn associate(
    doc: &AtifTrajectory,
    snapshots: Vec<Value>,
    as_of: DateTime<Utc>,
    checkpoints: &[(usize, String, DateTime<Utc>)],
) -> Value {
    let steps: Vec<_> = doc
        .steps
        .iter()
        .filter(|s| s.source == StepSource::Agent)
        .collect();
    let times: Vec<_> = steps
        .iter()
        .map(|s| s.timestamp.as_deref().and_then(timestamp))
        .collect();
    let mut assigned = HashMap::<usize, Vec<Value>>::new();
    let mut unassigned = Vec::new();
    let clock_invalid = times.iter().flatten().any(|time| *time > as_of);
    for snapshot in snapshots {
        let time = snapshot["meta"]["created_at"].as_str().and_then(timestamp);
        let owner = time
            .filter(|time| !clock_invalid && *time < as_of)
            .and_then(|time| {
                // Result timestamps are recorded at millisecond precision.
                if let Some((step, _, _)) = checkpoints.iter().find(|(_, id, result_at)| {
                    snapshot["id"].as_str() == Some(id)
                        && *result_at <= as_of
                        && time.timestamp_millis() <= result_at.timestamp_millis()
                }) {
                    return Some(*step);
                }
                times.iter().enumerate().find_map(|(i, start)| {
                    let start = (*start)?;
                    let end = if i + 1 < times.len() {
                        times[i + 1]?
                    } else {
                        as_of
                    };
                    let ordered = i == 0 || times[i - 1].is_none_or(|previous| previous <= start);
                    (ordered && start <= time && time < end).then_some(steps[i].step_id)
                })
            });
        if let Some(step) = owner {
            assigned.entry(step).or_default().push(snapshot);
        } else {
            unassigned.push(json!({"snapshot": snapshot, "reason": if clock_invalid { "step_clock_after_query" } else if time.is_none() { "invalid_snapshot_time" } else { "outside_valid_step_interval" }}));
        }
    }
    json!({"step_snapshots": assigned, "unassigned": unassigned})
}

#[derive(Default, Deserialize)]
pub(super) struct ListQuery {
    cursor: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Cursor {
    workspace: String,
    revision: String,
    as_of: DateTime<Utc>,
    next: String,
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
struct Rejected {
    status: StatusCode,
    message: &'static str,
}

#[derive(Debug, thiserror::Error)]
#[error("restore refused; refresh changes and confirm again: {0}")]
struct RestoreNoEffect(String);

async fn blocking(job: impl FnOnce() -> anyhow::Result<Value> + Send + 'static) -> HttpResponse {
    match web::block(job).await {
        Ok(Ok(value)) => HttpResponse::Ok().json(value),
        Ok(Err(error)) if error.is::<RestoreNoEffect>() => HttpResponse::BadGateway()
            .json(json!({"error": error.to_string(), "effect": "known_no_effect"})),
        Ok(Err(error)) => HttpResponse::build(
            error
                .downcast_ref::<Rejected>()
                .map(|e| e.status)
                .unwrap_or(StatusCode::BAD_GATEWAY),
        )
        .json(json!({"error": error.to_string()})),
        Err(_) => {
            HttpResponse::InternalServerError().json(json!({"error": "snapshot worker failed"}))
        }
    }
}

/// Indicate whether signing in would expose a registered workspace for this trajectory.
#[head("/ws-ckpt/{kind}/{id}/snapshots")]
pub(super) async fn snapshot_availability(
    data: web::Data<AppState>,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let (kind, id) = path.into_inner();
    let available = data.auth.enabled
        && web::block(move || {
            let cfg = config(&data)?;
            let doc = load(&data, &kind, &id)?;
            Ok::<_, anyhow::Error>(
                workspace(&doc, &cfg)?.is_some_and(|selector| identify(&cfg, &selector).is_ok()),
            )
        })
        .await
        .is_ok_and(|result| result.unwrap_or(false));
    HttpResponse::build(if available {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::NOT_FOUND
    })
    .insert_header(("Cache-Control", "no-store"))
    .finish()
}

/// Return the exact ATIF document and one fixed-boundary page of associated snapshots.
#[get("/ws-ckpt/{kind}/{id}/snapshots")]
pub(super) async fn get_snapshots(
    data: web::Data<AppState>,
    path: web::Path<(String, String)>,
    query: web::Query<ListQuery>,
) -> HttpResponse {
    let (kind, id) = path.into_inner();
    let cursor = query.into_inner().cursor;
    blocking(move || {
        let now = Utc::now();
        let cfg = config(&data)?;
        let doc = load(&data, &kind, &id)?;
        let Some(selector) = workspace(&doc, &cfg)? else {
            return Ok(json!({"trajectory": doc, "scope": null, "reason": "workspace_unresolved", "step_snapshots": {}}));
        };
        let scope = identify(&cfg, &selector)?;
        let ws = scope["ws_id"].as_str().ok_or_else(|| anyhow::anyhow!("missing ws_id"))?;
        let boundaries: Vec<_> = doc.steps.iter().map(|step| (step.step_id, &step.source, &step.timestamp)).collect();
        let checkpoints = hermes_checkpoint_steps(&doc, scope["path"].as_str().unwrap_or_default(), ws);
        let revision = format!("{:x}", Sha256::digest(serde_json::to_vec(&(kind, id, boundaries, ws, &scope["path"], cfg.socket(), &checkpoints))?));
        let cursor: Option<Cursor> = cursor.map(|raw| -> anyhow::Result<_> { Ok(serde_json::from_slice(&URL_SAFE_NO_PAD.decode(raw)?)?) }).transpose()?;
        if let Some(cursor) = &cursor {
            anyhow::ensure!(cursor.workspace == ws && cursor.revision == revision && cursor.as_of <= now, Rejected { status: StatusCode::CONFLICT, message: "snapshot page changed; reload from the first page" });
        }
        let as_of = cursor.as_ref().map(|c| c.as_of).unwrap_or(now);
        let mut args = vec!["list", "--workspace", ws, "--format", "json", "--limit", "100"];
        if let Some(cursor) = &cursor { args.extend(["--cursor", &cursor.next]); }
        let page = cfg.cli(&args)?;
        let rows: Vec<Value> = serde_json::from_value(page["snapshots"].clone())?;
        let next = page["next_cursor"].as_str().map(|next| -> anyhow::Result<_> {
            Ok(URL_SAFE_NO_PAD.encode(serde_json::to_vec(&Cursor {workspace: ws.into(), revision: revision.clone(), as_of, next: next.into()})?))
        }).transpose()?;
        let mut response = associate(&doc, rows, as_of, &checkpoints);
        response["trajectory"] = serde_json::to_value(doc)?;
        response["scope"] = scope;
        let method = if checkpoints.is_empty() { "timestamp_interval" } else { "checkpoint_result_then_timestamp_interval" };
        response["association"] = json!({"method": method, "revision": revision, "as_of": as_of});
        response["completeness"] = json!({"complete": next.is_none(), "next_cursor": next});
        response["restore_enabled"] = json!(cfg.restore_enabled && data.auth.enabled);
        Ok(response)
    }).await
}

#[derive(Deserialize)]
pub(super) struct Target {
    workspace: String,
    snapshot_id: String,
}

fn guarded_preview(
    cfg: &Config,
    scope: &Value,
    path: &str,
    target: &Target,
) -> anyhow::Result<CkptGuardedRollbackPreviewV2> {
    let client = cfg.client();
    let identity = client.workspace_identity_v2(path)?;
    anyhow::ensure!(
        identity.registered_path == path && scope["ws_id"] == identity.ws_id,
        "workspace identity changed; refresh before restore"
    );
    Ok(client.guarded_rollback_preview_v2(
        path,
        &identity.ws_id,
        identity.generation,
        &target.snapshot_id,
    )?)
}

#[derive(Deserialize, Serialize, PartialEq, Eq)]
struct PreviewFingerprint {
    generation: WorkspaceGenerationTokenV2,
    diff_digest: [u8; 32],
}

impl From<&CkptGuardedRollbackPreviewV2> for PreviewFingerprint {
    fn from(preview: &CkptGuardedRollbackPreviewV2) -> Self {
        Self {
            generation: preview.generation,
            diff_digest: preview.diff_digest,
        }
    }
}

/// Compare one exact snapshot with the current workspace; never evaluate recorded commands.
#[post("/ws-ckpt/diff")]
pub(super) async fn diff(data: web::Data<AppState>, body: web::Json<Target>) -> HttpResponse {
    let target = body.into_inner();
    blocking(move || {
        let cfg = config(&data)?;
        let scope = identify(&cfg, &target.workspace)?;
        let path = scope["path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing registered path"))?;
        let mut output = if cfg.restore_enabled && data.auth.enabled {
            let preview = guarded_preview(&cfg, &scope, path, &target)?;
            let fingerprint = PreviewFingerprint::from(&preview);
            json!({ "changes": preview.changes, "restore_preview": fingerprint })
        } else {
            // CLI diff accepts prefixes, so verify the full ID in read-only mode.
            let snapshot = cfg.client().find_snapshot(path, &target.snapshot_id)?;
            anyhow::ensure!(
                snapshot.is_some_and(|entry| !entry.missing && entry.workspace == path),
                "exact snapshot is unavailable"
            );
            cfg.cli(&[
                "diff",
                "--workspace",
                path,
                "--from",
                &target.snapshot_id,
                "--format",
                "json",
            ])?
        };
        output["scope"] = scope;
        output["snapshot_id"] = json!(target.snapshot_id);
        output["compared_at"] = json!(Utc::now());
        Ok(output)
    })
    .await
}

#[derive(Serialize, Deserialize)]
struct PreparedPreview {
    registered_path: String,
    ws_id: String,
    target_snapshot_id: String,
    #[serde(flatten)]
    fingerprint: PreviewFingerprint,
}

#[derive(Serialize, Deserialize)]
struct Prepared {
    preview: PreparedPreview,
    operation_id: String,
    operation_digest: [u8; 32],
    socket: String,
}

#[derive(Deserialize)]
pub(super) struct Prepare {
    target: Target,
    writers_stopped: bool,
    expected_preview: PreviewFingerprint,
}

fn restore_config(data: &AppState) -> anyhow::Result<Config> {
    let cfg = config(data)?;
    anyhow::ensure!(
        cfg.restore_enabled && data.auth.enabled,
        Rejected {
            status: StatusCode::FORBIDDEN,
            message: "snapshot restore requires enabled dashboard authentication and ws_ckpt.restore_enabled"
        }
    );
    Ok(cfg)
}

/// Bind a same-dialog confirmation to the current diff.
#[post("/ws-ckpt/restore/prepare")]
pub(super) async fn prepare(data: web::Data<AppState>, body: web::Json<Prepare>) -> HttpResponse {
    let request = body.into_inner();
    blocking(move || {
        anyhow::ensure!(request.writers_stopped, Rejected { status: StatusCode::CONFLICT, message: "confirm there will be no other writes to this workspace during rollback" });
        let cfg = restore_config(&data)?;
        let scope = identify(&cfg, &request.target.workspace)?;
        let path = scope["path"].as_str().ok_or_else(|| anyhow::anyhow!("missing registered path"))?;
        let preview = guarded_preview(&cfg, &scope, path, &request.target)?;
        let fingerprint = PreviewFingerprint::from(&preview);
        anyhow::ensure!(
            fingerprint == request.expected_preview,
            Rejected { status: StatusCode::CONFLICT, message: "workspace or file changes differ from the displayed preview; stop other writes, refresh, and confirm again" }
        );
        let preview = PreparedPreview {
            registered_path: preview.registered_path,
            ws_id: preview.ws_id,
            target_snapshot_id: preview.target_snapshot_id,
            fingerprint,
        };
        let operation_id = uuid::Uuid::new_v4().to_string();
        let digest: [u8; 32] = Sha256::digest(serde_json::to_vec(&json!(["sight-restore", operation_id, preview]))?).into();
        let prepared = Prepared { preview, operation_id, operation_digest: digest, socket: cfg.socket() };
        let token = secret::seal(&serde_json::to_string(&prepared)?, storage_data_dir(&data.storage_path))?;
        Ok(json!({"confirmation_token": token, "operation_id": prepared.operation_id}))
    }).await
}

#[derive(Deserialize)]
pub(super) struct Confirmation {
    confirmation_token: String,
}

fn open_confirmation(data: &AppState, token: &str) -> anyhow::Result<Prepared> {
    let raw = secret::unseal(token, storage_data_dir(&data.storage_path))
        .ok_or_else(|| anyhow::anyhow!("invalid restore confirmation; open the snapshot again"))?;
    let prepared: Prepared = serde_json::from_str(&raw)?;
    anyhow::ensure!(
        prepared.socket == config(data)?.socket(),
        "ws-ckpt instance changed; prepare restore again"
    );
    Ok(prepared)
}

/// Execute only the sealed, exact preview; uncertain outcomes are queried, never retried here.
#[post("/ws-ckpt/restore/execute")]
pub(super) async fn execute(
    data: web::Data<AppState>,
    body: web::Json<Confirmation>,
) -> HttpResponse {
    let token = body.into_inner().confirmation_token;
    blocking(move || {
        let cfg = restore_config(&data)?;
        let prepared = open_confirmation(&data, &token)?;
        let p = &prepared.preview;
        match cfg.client().guarded_rollback_v2(&p.registered_path, &p.ws_id, p.fingerprint.generation, &p.target_snapshot_id, p.fingerprint.diff_digest, &prepared.operation_id, prepared.operation_digest) {
            Ok(evidence) => Ok(json!({"status": "succeeded", "evidence": evidence})),
            Err(failure) if failure.effect == CkptRequestEffect::KnownNoEffect => Err(RestoreNoEffect(failure.error.to_string()).into()),
            Err(_) => Ok(json!({"status": "unknown", "operation_id": prepared.operation_id, "confirmation_token": token})),
        }
    }).await
}

/// Reconcile a possibly applied operation using durable ws-ckpt evidence.
#[post("/ws-ckpt/restore/result")]
pub(super) async fn restore_result(
    data: web::Data<AppState>,
    body: web::Json<Confirmation>,
) -> HttpResponse {
    let token = body.into_inner().confirmation_token;
    blocking(move || {
        let cfg = config(&data)?;
        let p = open_confirmation(&data, &token)?;
        let evidence = cfg.client().guarded_rollback_evidence_v2(
            &p.preview.ws_id,
            &p.operation_id,
            p.operation_digest,
        )?;
        Ok(json!({"evidence": evidence, "operation_id": p.operation_id}))
    })
    .await
}

#[cfg(test)]
#[path = "ws_ckpt_tests.rs"]
mod tests;
