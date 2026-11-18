//! Workspace snapshots attached to existing ATIF steps; file state stays in ws-ckpt.

use super::{AppState, secret, storage_data_dir};
use actix_web::http::StatusCode;
use actix_web::{HttpResponse, get, head, post, web};
use agentsight_atif::{AtifTrajectory, ObservationResult, StepSource, ToolCall, same_call_id};
use anyhow::Context;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};
mod command;

const DEFAULT_SOCKET_PATH: &str = "/run/ws-ckpt/ws-ckpt.sock";
// CLI JSON contract: src/ws-ckpt/docs/design/guarded-checkpoint-v2.md.
const ROLLBACK_SCHEMA: &str = "ws-ckpt.rollback.v1";

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

    fn run(&self, args: &[&str], input: Option<&[u8]>) -> anyhow::Result<std::process::Output> {
        let binary = self.binary.as_deref().unwrap_or(Path::new("ws-ckpt"));
        command::run(binary, &self.socket(), args, input)
    }

    fn query(&self, args: &[&str]) -> anyhow::Result<Value> {
        let output = self.run(args, None)?;
        anyhow::ensure!(
            output.status.success(),
            "ws-ckpt query failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(serde_json::from_slice(&output.stdout)?)
    }

    fn guarded_rollback(&self, args: &[&str], input: Option<&[u8]>) -> anyhow::Result<Value> {
        let mut command = vec!["rollback", "--guarded", "--json"];
        command.extend_from_slice(args);
        self.guarded_json(&command, input)
    }

    fn guarded_json(&self, args: &[&str], input: Option<&[u8]>) -> anyhow::Result<Value> {
        let output = self.run(args, input)?;
        let response: Value = serde_json::from_slice(&output.stdout).with_context(|| {
            format!(
                "ws-ckpt guarded JSON is unavailable ({}): {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            )
        })?;
        anyhow::ensure!(
            response["schema"] == ROLLBACK_SCHEMA,
            "incompatible ws-ckpt rollback schema: expected {ROLLBACK_SCHEMA}, got {}",
            response["schema"]
        );
        if output.status.success()
            || matches!(
                response["status"].as_str(),
                Some("known_no_effect" | "unknown")
            )
        {
            Ok(response)
        } else {
            anyhow::bail!(
                "ws-ckpt guarded operation failed: {}",
                response["message"]
                    .as_str()
                    .unwrap_or("unexpected CLI response")
            );
        }
    }
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
    if !matches!(kind, "session" | "conversation" | "trajectory") {
        return Err(Rejected {
            status: StatusCode::BAD_REQUEST,
            message: "invalid trajectory source",
        }
        .into());
    }
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
    Err(Rejected {
        status: StatusCode::NOT_FOUND,
        message: "trajectory not found",
    }
    .into())
}

#[derive(Deserialize, Serialize)]
struct WorkspaceInfo {
    ws_id: String,
    path: String,
    snapshot_count: u32,
}

fn identify(cfg: &Config, selector: &str) -> anyhow::Result<WorkspaceInfo> {
    let mut status = cfg.query(&["status", "--workspace", selector, "--format", "json"])?;
    let workspaces: Vec<WorkspaceInfo> = serde_json::from_value(status["workspaces"].take())?;
    let mut rows = workspaces.into_iter();
    let row = rows
        .next()
        .filter(|row| row.path == selector || row.ws_id == selector)
        .ok_or_else(|| anyhow::anyhow!("workspace identity not found"))?;
    anyhow::ensure!(rows.next().is_none(), "ambiguous workspace status");
    Ok(row)
}

fn compatible_snapshot_scope(cfg: &Config, selector: &str) -> Option<WorkspaceInfo> {
    let check = || {
        let scope = identify(cfg, selector).context("query ws-ckpt workspace identity")?;
        let response = cfg
            .guarded_rollback(&["--check", "--workspace", &scope.path], None)
            .context("check ws-ckpt guarded rollback JSON support")?;
        anyhow::ensure!(
            response["status"] == "compatible"
                && response["ws_id"] == scope.ws_id
                && response["registered_path"] == scope.path,
            "ws-ckpt guarded rollback check did not confirm the registered workspace"
        );
        anyhow::ensure!(
            response["diff_json"] == true,
            "ws-ckpt diff JSON support could not be confirmed"
        );
        Ok(scope)
    };
    match check() {
        Ok(scope) => Some(scope),
        Err(error) => {
            log::warn!("workspace snapshots unavailable: {error:#}");
            None
        }
    }
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
            let Some(selector) = workspace(&doc, &cfg)? else {
                return Ok(false);
            };
            Ok::<_, anyhow::Error>(compatible_snapshot_scope(&cfg, &selector).is_some())
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
        let Some(scope) = compatible_snapshot_scope(&cfg, &selector) else {
            return Ok(json!({
                "trajectory": doc,
                "scope": null,
                "reason": "ws_ckpt_incompatible",
                "step_snapshots": {},
                "restore_enabled": false
            }));
        };
        let ws = &scope.ws_id;
        let boundaries: Vec<_> = doc.steps.iter().map(|step| (step.step_id, &step.source, &step.timestamp)).collect();
        let checkpoints = hermes_checkpoint_steps(&doc, &scope.path, ws);
        let revision = format!("{:x}", Sha256::digest(serde_json::to_vec(&(kind, id, boundaries, ws, &scope.path, cfg.socket(), &checkpoints))?));
        let cursor: Option<Cursor> = cursor
            .map(|raw| -> anyhow::Result<_> {
                let bytes = URL_SAFE_NO_PAD.decode(raw).map_err(|_| Rejected {
                    status: StatusCode::BAD_REQUEST,
                    message: "invalid cursor",
                })?;
                Ok(serde_json::from_slice(&bytes).map_err(|_| Rejected {
                    status: StatusCode::BAD_REQUEST,
                    message: "invalid cursor",
                })?)
            })
            .transpose()?;
        if let Some(cursor) = &cursor {
            anyhow::ensure!(&cursor.workspace == ws && cursor.revision == revision && cursor.as_of <= now, Rejected { status: StatusCode::CONFLICT, message: "snapshot page changed; reload from the first page" });
        }
        let as_of = cursor.as_ref().map(|c| c.as_of).unwrap_or(now);
        let mut args = vec!["list", "--workspace", ws, "--format", "json", "--limit", "100"];
        if let Some(cursor) = &cursor { args.extend(["--cursor", &cursor.next]); }
        let mut page = cfg.query(&args)?;
        let rows = serde_json::from_value(page["snapshots"].take())?;
        let next = page["next_cursor"].as_str().map(|next| -> anyhow::Result<_> {
            Ok(URL_SAFE_NO_PAD.encode(serde_json::to_vec(&Cursor {workspace: ws.clone(), revision: revision.clone(), as_of, next: next.into()})?))
        }).transpose()?;
        let mut response = associate(&doc, rows, as_of, &checkpoints);
        response["trajectory"] = serde_json::to_value(doc)?;
        response["scope"] = serde_json::to_value(scope)?;
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

fn snapshot_changes(
    cfg: &Config,
    scope: &WorkspaceInfo,
    snapshot_id: &str,
) -> anyhow::Result<Vec<Value>> {
    let mut cursor: Option<String> = None;
    loop {
        let mut args = vec![
            "list",
            "--workspace",
            &scope.ws_id,
            "--format",
            "json",
            "--limit",
            "100",
        ];
        if let Some(cursor) = &cursor {
            args.extend(["--cursor", cursor]);
        }
        let page = cfg.query(&args)?;
        let snapshots = page["snapshots"]
            .as_array()
            .context("invalid snapshot list")?;
        if snapshots.iter().any(|row| {
            row["id"] == snapshot_id
                && row["workspace"] == scope.path
                && row["meta"]["missing"] == false
        }) {
            break;
        }
        cursor = page["next_cursor"].as_str().map(str::to_owned);
        anyhow::ensure!(cursor.is_some(), "exact snapshot is unavailable");
    }
    let from = format!("--from={snapshot_id}");
    let mut result = cfg.query(&[
        "diff",
        "--workspace",
        &scope.ws_id,
        &from,
        "--format",
        "json",
    ])?;
    Ok(serde_json::from_value(result["changes"].take())?)
}

fn guarded_preview(
    cfg: &Config,
    scope: &WorkspaceInfo,
    target: &Target,
) -> anyhow::Result<(PreviewFingerprint, Vec<Value>)> {
    let snapshot_arg = format!("--snapshot={}", target.snapshot_id);
    let mut response = cfg.guarded_rollback(
        &["--preview", "--workspace", &scope.path, &snapshot_arg],
        None,
    )?;
    anyhow::ensure!(
        response["status"] == "preview"
            && response["registered_path"] == scope.path
            && response["ws_id"] == scope.ws_id
            && response["snapshot_id"] == target.snapshot_id,
        "ws-ckpt preview does not match the requested rollback target"
    );
    let changes = serde_json::from_value(response["changes"].take())?;
    Ok((serde_json::from_value(response)?, changes))
}

#[derive(Deserialize, Serialize, PartialEq, Eq)]
struct PreviewFingerprint {
    generation: [u8; 32],
    diff_digest: [u8; 32],
}

/// Compare one exact snapshot with the current workspace; never evaluate recorded commands.
#[post("/ws-ckpt/diff")]
pub(super) async fn diff(data: web::Data<AppState>, body: web::Json<Target>) -> HttpResponse {
    let target = body.into_inner();
    blocking(move || {
        let cfg = config(&data)?;
        let scope = identify(&cfg, &target.workspace)?;
        let mut output = if cfg.restore_enabled && data.auth.enabled {
            let (fingerprint, changes) = guarded_preview(&cfg, &scope, &target)?;
            json!({"changes": changes, "restore_preview": fingerprint})
        } else {
            let changes = snapshot_changes(&cfg, &scope, &target.snapshot_id)?;
            json!({"changes": changes})
        };
        output["scope"] = serde_json::to_value(scope)?;
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
        let (fingerprint, _) = guarded_preview(&cfg, &scope, &request.target)?;
        anyhow::ensure!(
            fingerprint == request.expected_preview,
            Rejected { status: StatusCode::CONFLICT, message: "workspace or file changes differ from the displayed preview; stop other writes, refresh, and confirm again" }
        );
        let preview = PreparedPreview {
            registered_path: scope.path,
            ws_id: scope.ws_id,
            target_snapshot_id: request.target.snapshot_id,
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

fn rollback_input(prepared: &Prepared) -> anyhow::Result<Vec<u8>> {
    Ok(serde_json::to_vec(&json!({
        "ws_id": prepared.preview.ws_id,
        "generation": prepared.preview.fingerprint.generation,
        "diff_digest": prepared.preview.fingerprint.diff_digest,
        "operation_id": prepared.operation_id,
        "operation_digest": prepared.operation_digest,
    }))?)
}

fn matches_evidence(prepared: &Prepared, evidence: &Value) -> bool {
    let preview = &prepared.preview;
    evidence["ws_id"] == preview.ws_id
        && evidence["registered_path"] == preview.registered_path
        && evidence["expected_generation"] == json!(preview.fingerprint.generation)
        && evidence["target_snapshot_id"] == preview.target_snapshot_id
        && evidence["expected_diff_digest"] == json!(preview.fingerprint.diff_digest)
        && evidence["operation_id"] == prepared.operation_id
        && evidence["operation_digest"] == json!(prepared.operation_digest)
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
        let input = rollback_input(&prepared)?;
        let snapshot_arg = format!("--snapshot={}", p.target_snapshot_id);
        match cfg.guarded_rollback(&["--workspace", &p.registered_path, &snapshot_arg], Some(&input)) {
            Ok(response) if response["status"] == "succeeded"
                && matches_evidence(&prepared, &response["evidence"])
                && response["evidence"]["outcome"]["Succeeded"].is_object() =>
            {
                return Ok(json!({"status": "succeeded", "evidence": response["evidence"]}));
            }
            Ok(response) if response["status"] == "known_no_effect" => {
                let code = response["code"].as_str().unwrap_or("rejected");
                let message = response["message"].as_str().unwrap_or("ws-ckpt refused rollback");
                return Err(RestoreNoEffect(format!("{code}: {message}")).into());
            }
            Err(error) if error.is::<command::BeforeExecution>() => {
                return Err(RestoreNoEffect(error.to_string()).into());
            }
            Err(error) => log::warn!("ws-ckpt rollback {} result is unknown: {error:#}", prepared.operation_id),
            Ok(response) => log::warn!(
                "ws-ckpt rollback {} result is unknown: status={}, code={}, message={}",
                prepared.operation_id, response["status"], response["code"], response["message"]
            ),
        }
        Ok(json!({"status": "unknown", "operation_id": prepared.operation_id, "confirmation_token": token}))
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
        let response = cfg.guarded_rollback(
            &["--result", "--workspace", &p.preview.ws_id],
            Some(&rollback_input(&p)?),
        )?;
        let evidence = response
            .get("evidence")
            .filter(|_| response["status"] == "result")
            .ok_or_else(|| anyhow::anyhow!("unexpected ws-ckpt rollback evidence response"))?;
        anyhow::ensure!(
            evidence.is_null() || matches_evidence(&p, evidence),
            "ws-ckpt evidence does not match the requested rollback operation"
        );
        Ok(json!({"evidence": evidence, "operation_id": p.operation_id}))
    })
    .await
}

#[cfg(test)]
#[path = "ws_ckpt_tests.rs"]
mod tests;
