//! Optimization analysis API — bridges captured GenAI sessions to the
//! `agentsight-opt` accuracy/perf/cost analyzers.
//!
//! LLM credentials are configured at runtime from the Dashboard settings page
//! and persisted to `optimization_config.json` next to the databases. The API
//! key is sealed with machine-bound encryption (see [`super::secret`]) before
//! it reaches disk. Analysis results are persisted per session via
//! `agentsight-opt-store`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use actix_web::{HttpResponse, Responder, get, post, web};
use serde::{Deserialize, Serialize};

use agentsight_opt::{AnalyzePipeline, AtifTrajectory, LlmClient, TrajectoryRecorder};
use agentsight_opt_store::{Dimension, OptimizationStore};
use agentsight_trajectory_collector::{TrajectoryRecord, TrajectoryStore};

use super::AppState;
use super::secret;
use crate::semantic_search;
use crate::storage::sqlite::GenAISqliteStore;

const CONFIG_FILE_NAME: &str = "optimization_config.json";
const DB_FILE_NAME: &str = "optimization.db";
const TRAJECTORIES_DIR_NAME: &str = "opt-trajectories";

/// Directory holding the config file — where the sealing salt lives too.
fn config_dir(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf()
}

// ─── LLM configuration ───────────────────────────────────────────────────────

/// Runtime LLM configuration for optimization analysis (Dashboard-managed).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OptLlmConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// LLM ranking budget for semantic session search, in seconds. Separate
    /// from the analysis path (which has no budget): a slow reasoning model
    /// should raise this without affecting long-running optimization jobs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search_timeout_secs: Option<u64>,
}

impl OptLlmConfig {
    /// Load from disk, unsealing the API key.
    ///
    /// The second value is true when the file held a legacy plaintext key
    /// that must be resealed so the plaintext leaves the disk.
    fn load(path: &Path) -> (Self, bool) {
        let mut config: Self = match std::fs::read_to_string(path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
            Err(_) => return (Self::default(), false),
        };
        let mut needs_reseal = false;
        if let Some(stored) = config.api_key.take() {
            if secret::is_sealed(&stored) {
                match secret::unseal(&stored, &config_dir(path)) {
                    Some(plain) => config.api_key = Some(plain),
                    // Salt or machine-id changed (e.g. config copied from
                    // another host) — treat as unconfigured, never as a key.
                    None => log::warn!(
                        "Cannot decrypt the stored optimization API key; \
                         re-enter it in the dashboard settings"
                    ),
                }
            } else {
                // Pre-encryption config: keep the key usable and reseal at
                // startup so the plaintext copy is overwritten.
                config.api_key = Some(stored);
                needs_reseal = true;
            }
        }
        (config, needs_reseal)
    }

    fn save(&self, path: &Path) -> std::io::Result<()> {
        // Never persist the API key as plaintext — 0o600 does not survive
        // backups, snapshots, or root compromise (see super::secret).
        let mut on_disk = self.clone();
        if let Some(ref key) = on_disk.api_key {
            on_disk.api_key = Some(secret::seal(key, &config_dir(path))?);
        }
        let json = serde_json::to_string_pretty(&on_disk)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        std::fs::write(path, json)?;
        // Defense in depth: keep the sealed config owner-only as well.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }

    fn effective_base_url(&self) -> String {
        self.base_url
            .clone()
            .filter(|s| !s.is_empty())
            .or_else(|| std::env::var("OPENAI_BASE_URL").ok())
            .unwrap_or_else(|| "https://api.openai.com/v1".into())
    }

    fn effective_api_key(&self) -> Option<String> {
        self.api_key
            .clone()
            .filter(|s| !s.is_empty())
            .or_else(|| std::env::var("OPENAI_API_KEY").ok())
    }

    fn effective_model(&self) -> String {
        self.model
            .clone()
            .filter(|s| !s.is_empty())
            .or_else(|| std::env::var("OPENAI_MODEL").ok())
            .unwrap_or_else(|| "gpt-4o".into())
    }

    fn search_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(
            self.search_timeout_secs
                .unwrap_or(semantic_search::DEFAULT_SEARCH_TIMEOUT_SECS),
        )
    }

    /// Mask the API key for display: first 6 and last 4 chars.
    fn masked_api_key(&self) -> Option<String> {
        self.effective_api_key().map(|k| {
            if k.chars().count() <= 12 {
                "••••••".to_string()
            } else {
                let head: String = k.chars().take(6).collect();
                let mut tail_chars: Vec<char> = k.chars().rev().take(4).collect();
                tail_chars.reverse();
                let tail: String = tail_chars.into_iter().collect();
                format!("{head}••••{tail}")
            }
        })
    }
}

// ─── Shared state ────────────────────────────────────────────────────────────

/// Optimization feature state shared across handlers.
pub struct OptimizeState {
    config_path: PathBuf,
    config: RwLock<OptLlmConfig>,
    store: Option<OptimizationStore>,
    /// Serializes read-modify-write on the `opt:<target>` root trajectory so
    /// parallel dimension handlers cannot lose each other's dispatch steps.
    root_lock: Mutex<()>,
}

impl OptimizeState {
    /// Initialize from the storage base directory (where the .db files live).
    pub fn init(base_dir: &Path) -> Arc<Self> {
        let config_path = base_dir.join(CONFIG_FILE_NAME);
        let (config, needs_reseal) = OptLlmConfig::load(&config_path);
        if needs_reseal {
            // One-shot migration of pre-encryption configs: rewrite the file
            // so the plaintext API key no longer exists on disk.
            match config.save(&config_path) {
                Ok(()) => log::info!("Migrated optimization API key to encrypted storage"),
                Err(e) => log::warn!("Failed to encrypt stored optimization API key: {e}"),
            }
        }
        let store = match OptimizationStore::new_with_path(&base_dir.join(DB_FILE_NAME)) {
            Ok(s) => Some(s),
            Err(e) => {
                log::warn!("Failed to open optimization store: {e}");
                None
            }
        };
        Arc::new(Self {
            config_path,
            config: RwLock::new(config),
            store,
            root_lock: Mutex::new(()),
        })
    }

    pub(crate) fn snapshot(&self) -> OptLlmConfig {
        self.config.read().map(|c| c.clone()).unwrap_or_default()
    }

    pub(super) fn build_client(&self) -> Result<LlmClient, HttpResponse> {
        let config = self.snapshot();
        let Some(api_key) = config.effective_api_key() else {
            return Err(HttpResponse::BadRequest().json(serde_json::json!({
                "error": "llm_not_configured",
                "message": "LLM API key not configured. Set it in the optimization settings.",
            })));
        };
        let mut client = LlmClient::with_config(
            config.effective_base_url(),
            api_key,
            config.effective_model(),
        );
        client.set_temperature(0.0);
        Ok(client)
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn optimize_state(data: &AppState) -> Result<&Arc<OptimizeState>, HttpResponse> {
    data.optimize.as_ref().ok_or_else(|| {
        HttpResponse::ServiceUnavailable()
            .json(serde_json::json!({"error": "optimization feature unavailable"}))
    })
}

// ─── Semantic session search ────────────────────────────────────────────────

/// POST /api/sessions/search — delegates ranking to the shared module.
#[post("/sessions/search")]
pub async fn semantic_search_sessions(
    data: web::Data<AppState>,
    body: web::Json<semantic_search::SemanticSearchRequest>,
) -> impl Responder {
    let state = match optimize_state(&data) {
        Ok(s) => s,
        Err(_) => {
            return HttpResponse::Ok()
                .json(semantic_search::SemanticSearchResponse { results: vec![] });
        }
    };
    let client = match state.build_client() {
        Ok(c) => c,
        Err(_) => {
            // Search degrades to empty instead of failing, but an unconfigured
            // LLM is the most common cause of an empty page — make it
            // attributable rather than indistinguishable from "no matches".
            log::warn!(
                "semantic_search: LLM not configured (missing API key), returning empty results"
            );
            return HttpResponse::Ok()
                .json(semantic_search::SemanticSearchResponse { results: vec![] });
        }
    };
    let timeout = state.snapshot().search_timeout();
    let request = body.into_inner();

    // Sessions labelled `useless` are out of retrieval scope by design — the
    // whole trajectory was judged to have nothing worth putting in front of a
    // future search. Filtering here covers every caller of the endpoint, and
    // before the minimum-count check so a fully-excluded request reads as
    // empty rather than shrinking past the LLM threshold.
    let request = match data.reuse_store.as_deref() {
        Some(store) => match store.excluded_sessions() {
            Ok(excluded) => semantic_search::filter_excluded(
                request,
                &excluded
                    .into_iter()
                    .collect::<std::collections::HashSet<_>>(),
            ),
            Err(error) => {
                // A store failure must not take search down: the labels are a
                // filter, not a dependency. Search everything rather than nothing.
                log::warn!("reuse: reading excluded sessions failed, ranking unfiltered: {error}");
                request
            }
        },
        None => request,
    };
    semantic_search::handle_semantic_search(&client, &request, timeout).await
}

/// Load a session's captured events and build the ATIF trajectory that the
/// analyzers consume. The boundary format is standard ATIF JSON: the export
/// document is serialized and re-parsed into the opt crate's ATIF model.
///
/// When the session has no eBPF-captured events, falls back to the
/// log-collected trajectory store (trajectories.db), whose rows already hold
/// ready-made ATIF v1.7 JSON.
fn load_trajectory(
    db_path: &Path,
    trajectory_store: Option<Arc<TrajectoryStore>>,
    session_id: &str,
) -> Result<AtifTrajectory, HttpResponse> {
    let store = GenAISqliteStore::new_with_path(db_path).map_err(|e| {
        HttpResponse::InternalServerError().json(serde_json::json!({"error": e.to_string()}))
    })?;
    let events = store.get_events_by_session(session_id).map_err(|e| {
        HttpResponse::InternalServerError().json(serde_json::json!({"error": e.to_string()}))
    })?;
    if events.is_empty() {
        return load_collected_trajectory(trajectory_store, session_id);
    }
    let doc = crate::atif::convert_session_to_atif(session_id, events).map_err(|e| {
        HttpResponse::UnprocessableEntity().json(serde_json::json!({
            "error": "atif_conversion_failed",
            "message": e.to_string(),
        }))
    })?;
    let json = serde_json::to_string(&doc).map_err(|e| {
        HttpResponse::InternalServerError().json(serde_json::json!({"error": e.to_string()}))
    })?;
    AtifTrajectory::from_json(&json).map_err(|e| {
        HttpResponse::InternalServerError().json(serde_json::json!({
            "error": "atif_parse_failed",
            "message": e.to_string(),
        }))
    })
}

/// Fallback for sessions absent from the eBPF capture: load the ATIF JSON
/// persisted by the trajectory collector (log-collected Qoder/QoderWork
/// sessions).
fn load_collected_trajectory(
    trajectory_store: Option<Arc<TrajectoryStore>>,
    session_id: &str,
) -> Result<AtifTrajectory, HttpResponse> {
    let Some(tstore) = trajectory_store else {
        return Err(HttpResponse::NotFound()
            .json(serde_json::json!({"error": "session not found or pruned"})));
    };
    match tstore.get_atif_json(session_id) {
        Ok(Some(atif_json)) => AtifTrajectory::from_json(&atif_json).map_err(|e| {
            HttpResponse::UnprocessableEntity().json(serde_json::json!({
                "error": "atif_parse_failed",
                "message": e.to_string(),
            }))
        }),
        Ok(None) => Err(HttpResponse::NotFound()
            .json(serde_json::json!({"error": "session not found or pruned"}))),
        Err(e) => {
            Err(HttpResponse::InternalServerError()
                .json(serde_json::json!({"error": e.to_string()})))
        }
    }
}

/// The dimension a dispatch step was generated for, from its `ToolCall(Agent)`.
fn step_dimension(step: &agentsight_atif::Step) -> Option<&str> {
    step.tool_calls
        .as_ref()?
        .first()?
        .arguments
        .get("subagent_type")?
        .as_str()
}

/// Create or update the per-target optimization run root trajectory
/// (`opt:<target>`). Each dimension analysis contributes exactly one dispatch
/// step with a `ToolCall(Agent)` + `subagent_trajectory_ref` pointing at the
/// dimension record (`opt:<target>:subagent:<dim>`), so the existing subagent
/// injection and session-list folding group all analyses under one row.
///
/// Re-running a dimension replaces its step (and its stored subagent row)
/// rather than adding another, keeping the subagent graph at one node per
/// dimension however often the analysis is re-triggered.
fn upsert_opt_run_root(
    store: &TrajectoryStore,
    target_session_id: &str,
    dimension_raw: &str,
    dim_doc: &agentsight_atif::AtifTrajectory,
) -> anyhow::Result<()> {
    use agentsight_atif as schema;

    let root_id = format!("opt:{target_session_id}");
    let mut root: schema::AtifTrajectory = match store.get_atif_json(&root_id)? {
        Some(json) => serde_json::from_str(&json)?,
        None => schema::AtifTrajectory {
            schema_version: schema::ATIF_SCHEMA_VERSION.to_string(),
            agent: schema::Agent {
                name: "agentsight-opt".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                model_name: None,
                tool_definitions: None,
                extra: None,
            },
            steps: Vec::new(),
            session_id: Some(root_id.clone()),
            trajectory_id: None,
            notes: Some(format!("优化分析运行 · 目标会话 {target_session_id}")),
            final_metrics: None,
            continued_trajectory_ref: None,
            subagent_trajectories: None,
            extra: None,
        },
    };
    if root.agent.model_name.is_none() {
        root.agent.model_name = dim_doc.agent.model_name.clone();
    }

    let (dim_prompt, dim_completion) = dim_doc
        .final_metrics
        .as_ref()
        .map(|m| {
            (
                m.total_prompt_tokens.unwrap_or(0),
                m.total_completion_tokens.unwrap_or(0),
            )
        })
        .unwrap_or((0, 0));

    // Build the dispatch step for this dimension run.
    let sub_traj_id = dim_doc.trajectory_id.clone();
    let call_id = format!(
        "dispatch-{}",
        sub_traj_id.as_deref().unwrap_or(dimension_raw)
    );
    let ts = dim_doc
        .steps
        .last()
        .and_then(|s| s.timestamp.clone())
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
    let dispatch = schema::Step {
        step_id: 0, // renumbered below, after the per-dimension collapse
        source: schema::StepSource::Agent,
        message: String::new(),
        timestamp: Some(ts),
        model_name: dim_doc.agent.model_name.clone(),
        reasoning_effort: None,
        reasoning_content: None,
        tool_calls: Some(vec![schema::ToolCall {
            tool_call_id: call_id.clone(),
            function_name: "Agent".to_string(),
            arguments: serde_json::json!({
                "subagent_type": dimension_raw,
                "description": format!("{dimension_raw} 维度优化分析"),
            }),
            extra: None,
        }]),
        observation: Some(schema::Observation {
            results: vec![schema::ObservationResult {
                source_call_id: Some(call_id),
                content: None,
                subagent_trajectory_ref: Some(vec![schema::SubagentTrajectoryRef {
                    trajectory_id: sub_traj_id,
                    trajectory_path: None,
                    session_id: dim_doc.session_id.clone(),
                    extra: None,
                }]),
                extra: None,
            }],
        }),
        metrics: Some(schema::Metrics {
            prompt_tokens: Some(dim_prompt),
            completion_tokens: Some(dim_completion),
            cached_tokens: None,
            cost_usd: None,
            logprobs: None,
            completion_token_ids: None,
            prompt_token_ids: None,
            extra: None,
        }),
        extra: None,
        llm_call_count: None,
        is_copied_context: None,
    };

    // Collapse to one dispatch step per dimension, newest run winning. This
    // both folds in the step just built and heals roots written before the
    // subagent ids were canonical (which held one step per *run*).
    let mut steps: Vec<schema::Step> = Vec::new();
    for step in root.steps.drain(..).chain(std::iter::once(dispatch)) {
        let existing = step_dimension(&step)
            .and_then(|dim| steps.iter().position(|s| step_dimension(s) == Some(dim)));
        match existing {
            Some(i) => steps[i] = step,
            None => steps.push(step),
        }
    }
    for (i, step) in steps.iter_mut().enumerate() {
        step.step_id = i + 1;
    }
    root.steps = steps;

    // Totals are recomputed from the per-dimension step metrics, so a re-run
    // overwrites its dimension's contribution instead of double-counting it.
    let (total_prompt, total_completion) =
        root.steps
            .iter()
            .fold((0u64, 0u64), |(prompt, completion), step| {
                let metrics = step.metrics.as_ref();
                (
                    prompt + metrics.and_then(|m| m.prompt_tokens).unwrap_or(0),
                    completion + metrics.and_then(|m| m.completion_tokens).unwrap_or(0),
                )
            });
    root.final_metrics = Some(schema::FinalMetrics {
        total_prompt_tokens: Some(total_prompt),
        total_completion_tokens: Some(total_completion),
        total_cached_tokens: None,
        total_cost_usd: None,
        total_steps: Some(root.steps.len()),
        extra: None,
    });

    // Subagent rows the collapsed root still points at; everything else under
    // `opt:<target>:subagent:%` is a superseded run and gets pruned below.
    let mut keep = Vec::new();
    for step in &root.steps {
        for result in step.observation.iter().flat_map(|o| &o.results) {
            for sub_ref in result.subagent_trajectory_ref.iter().flatten() {
                if let Some(id) = &sub_ref.session_id {
                    keep.push(id.clone());
                }
            }
        }
    }
    // Canonical per-dimension ids are always kept: a sibling dimension may
    // have persisted its row without having folded its dispatch into this
    // root yet (its own root update runs later or failed). Pruning must only
    // ever target legacy `<dim>-<uuid>` rows from superseded runs.
    for dim in ALL_DIMENSIONS {
        keep.push(format!("opt:{target_session_id}:subagent:{dim}"));
    }

    // Steps sit in first-seen dimension order, not chronological order, so the
    // run window comes from the timestamp extremes rather than first/last step.
    let timestamps: Vec<&String> = root
        .steps
        .iter()
        .filter_map(|s| s.timestamp.as_ref())
        .collect();

    let atif_json = serde_json::to_string(&root)?;
    let record = TrajectoryRecord {
        session_id: root_id.clone(),
        schema_version: root.schema_version.clone(),
        agent_name: "agentsight-opt".to_string(),
        model_name: root.agent.model_name.clone(),
        num_steps: root.steps.len() as i64,
        total_prompt_tokens: root
            .final_metrics
            .as_ref()
            .and_then(|m| m.total_prompt_tokens)
            .map(|v| v as i64),
        total_completion_tokens: root
            .final_metrics
            .as_ref()
            .and_then(|m| m.total_completion_tokens)
            .map(|v| v as i64),
        start_time: timestamps.iter().min().map(|s| s.to_string()),
        end_time: timestamps.iter().max().map(|s| s.to_string()),
        first_user_message: Some(format!("优化分析运行 · 目标会话 {target_session_id}")),
        last_user_message: Some(format!("最近维度: {dimension_raw}")),
        atif_json,
        project: target_session_id.to_string(),
        source: "agentsight-opt".to_string(),
        is_subagent: false,
        file_path: String::new(),
        file_size: 0,
        file_mtime_ns: 0,
    };
    store.upsert_trajectory(&record)?;

    // An empty keep-list would delete every child, so treat it as "nothing
    // resolvable to prune against" rather than as a licence to wipe the run.
    if !keep.is_empty() {
        let pruned = store.retain_subagents(&root_id, &keep)?;
        if pruned > 0 {
            log::info!("Pruned {pruned} superseded opt subagent trajectories under {root_id}");
        }
    }
    Ok(())
}

/// Serialize an analysis result, persist it, and build the HTTP response.
fn persist_and_respond<T: Serialize>(
    state: &OptimizeState,
    session_id: &str,
    dimension: Dimension,
    result: &T,
) -> HttpResponse {
    let json = match serde_json::to_string(result) {
        Ok(j) => j,
        Err(e) => {
            return HttpResponse::InternalServerError()
                .json(serde_json::json!({"error": e.to_string()}));
        }
    };
    if let Some(ref store) = state.store {
        if let Err(e) = store.save_dimension(session_id, dimension, &json) {
            log::warn!("Failed to persist optimization result for {session_id}: {e}");
        }
    }
    HttpResponse::Ok()
        .content_type("application/json")
        .body(json)
}

fn parse_dimension(raw: &str) -> Option<Dimension> {
    match raw {
        "perf" => Some(Dimension::Perf),
        "perf-issues" => Some(Dimension::PerfIssues),
        "cost" => Some(Dimension::Cost),
        "cost-waste" => Some(Dimension::CostWaste),
        "accuracy" => Some(Dimension::Accuracy),
        "summary" => Some(Dimension::Summary),
        _ => None,
    }
}

/// Every dimension id `parse_dimension` accepts (keep the two in sync). Used
/// to shield canonical subagent rows from the superseded-run prune pass.
const ALL_DIMENSIONS: &[&str] = &[
    "perf",
    "perf-issues",
    "cost",
    "cost-waste",
    "accuracy",
    "summary",
];

// ─── Handlers ────────────────────────────────────────────────────────────────

/// POST /api/optimize/sessions/{session_id}/{dimension}
///
/// Runs one analysis dimension for a captured session. `perf` and `cost` are
/// pure computation; `perf-issues`, `cost-waste` and `accuracy` call the
/// configured LLM and can take 10–60 s.
#[post("/optimize/sessions/{session_id}/{dimension}")]
pub async fn run_optimization(
    data: web::Data<AppState>,
    path: web::Path<(String, String)>,
) -> impl Responder {
    let (session_id, dimension_raw) = path.into_inner();
    let state = match optimize_state(&data) {
        Ok(s) => Arc::clone(s),
        Err(resp) => return resp,
    };
    let Some(dimension) = parse_dimension(&dimension_raw) else {
        return HttpResponse::BadRequest().json(serde_json::json!({
            "error": "unknown dimension",
            "message": "expected one of: perf, perf-issues, cost, cost-waste, accuracy",
        }));
    };

    let trajectory = match load_trajectory(&data.storage_path, data.trajectory_store(), &session_id)
    {
        Ok(t) => t,
        Err(resp) => return resp,
    };

    match dimension {
        Dimension::Perf => match AnalyzePipeline::run_perf(&trajectory) {
            Ok(stats) => persist_and_respond(&state, &session_id, dimension, &stats),
            Err(e) => HttpResponse::InternalServerError()
                .json(serde_json::json!({"error": e.to_string()})),
        },
        Dimension::Cost => match AnalyzePipeline::run_cost(&trajectory) {
            Ok(stats) => persist_and_respond(&state, &session_id, dimension, &stats),
            Err(e) => HttpResponse::InternalServerError()
                .json(serde_json::json!({"error": e.to_string()})),
        },
        Dimension::PerfIssues | Dimension::CostWaste | Dimension::Accuracy | Dimension::Summary => {
            let mut client = match state.build_client() {
                Ok(c) => c,
                Err(resp) => return resp,
            };

            // Attach trajectory recorder to capture LLM calls.
            let recorder = std::sync::Arc::new(TrajectoryRecorder::new(
                client.model().to_string(),
                session_id.clone(),
            ));
            client.set_recorder(std::sync::Arc::clone(&recorder));

            let pipeline = AnalyzePipeline::new(&client);
            let result: Result<String, anyhow::Error> = match dimension {
                Dimension::PerfIssues => pipeline
                    .run_perf_issues(&trajectory)
                    .await
                    .and_then(|r| serde_json::to_string(&r).map_err(|e| anyhow::anyhow!(e))),
                Dimension::CostWaste => pipeline
                    .run_cost_waste(&trajectory)
                    .await
                    .and_then(|r| serde_json::to_string(&r).map_err(|e| anyhow::anyhow!(e))),
                Dimension::Accuracy => pipeline
                    .run_accuracy(&trajectory, None)
                    .await
                    .and_then(|r| serde_json::to_string(&r).map_err(|e| anyhow::anyhow!(e))),
                Dimension::Summary => pipeline
                    .run_summary(&trajectory)
                    .await
                    .and_then(|r| serde_json::to_string(&r).map_err(|e| anyhow::anyhow!(e))),
                // Pure-compute dimensions handled in the outer match.
                Dimension::Perf | Dimension::Cost => unreachable!(),
            };

            // Save LLM trajectory as ATIF file (best-effort, non-blocking).
            if !recorder.is_empty() {
                let traj_dir = state
                    .config_path
                    .parent()
                    .unwrap_or(std::path::Path::new("/var/log/sysak/.agentsight"))
                    .join(TRAJECTORIES_DIR_NAME)
                    .join(&dimension_raw);
                if let Err(e) = recorder.save_to_dir(&traj_dir) {
                    log::warn!("Failed to save opt LLM trajectory: {e}");
                }

                // Persist to trajectories.db for Dashboard query. The record
                // becomes a subagent of the per-target run root (`opt:<target>`)
                // so all dimension analyses of one session group under one row.
                if let Some(traj_store) = data.trajectory_store() {
                    let mut doc = recorder.to_atif();
                    // Id is keyed on the dimension alone: re-running an analysis
                    // overwrites its row instead of adding a new subagent node.
                    let record_session_id = format!("opt:{session_id}:subagent:{dimension_raw}");
                    doc.session_id = Some(record_session_id.clone());
                    doc.trajectory_id = Some(dimension_raw.clone());
                    // The graph labels nodes from `agent.name` minus the
                    // `agentsight-opt:` prefix, so name it after the dimension.
                    doc.agent.name = format!("agentsight-opt:{dimension_raw}");
                    let atif_json = serde_json::to_string(&doc).unwrap_or_default();
                    let (first_user_message, last_user_message) =
                        agentsight_trajectory_collector::store::extract_user_message_previews(
                            &atif_json,
                        );
                    let record = TrajectoryRecord {
                        session_id: record_session_id,
                        schema_version: doc.schema_version.clone(),
                        agent_name: "agentsight-opt".to_string(),
                        model_name: doc.agent.model_name.clone(),
                        num_steps: doc.steps.len() as i64,
                        total_prompt_tokens: doc
                            .final_metrics
                            .as_ref()
                            .and_then(|m| m.total_prompt_tokens)
                            .map(|v| v as i64),
                        total_completion_tokens: doc
                            .final_metrics
                            .as_ref()
                            .and_then(|m| m.total_completion_tokens)
                            .map(|v| v as i64),
                        start_time: doc.steps.first().and_then(|s| s.timestamp.clone()),
                        end_time: doc.steps.last().and_then(|s| s.timestamp.clone()),
                        first_user_message,
                        last_user_message,
                        atif_json,
                        project: session_id.clone(),
                        source: "agentsight-opt".to_string(),
                        is_subagent: true,
                        file_path: String::new(),
                        file_size: 0,
                        file_mtime_ns: 0,
                    };
                    // Hold the lock across "write subagent row → update root →
                    // prune": a parallel dimension's prune pass must never see
                    // a sibling row its root does not reference yet, or it
                    // would delete it and leave the root with a dangling ref.
                    let _guard = state.root_lock.lock().unwrap_or_else(|e| e.into_inner());
                    if let Err(e) = traj_store.upsert_trajectory(&record) {
                        log::warn!("Failed to persist opt trajectory to SQLite: {e}");
                    } else if let Err(e) =
                        upsert_opt_run_root(traj_store.as_ref(), &session_id, &dimension_raw, &doc)
                    {
                        log::warn!("Failed to update opt run root trajectory: {e}");
                    }
                }
            }

            match result {
                Ok(json) => {
                    if let Some(ref store) = state.store {
                        if let Err(e) = store.save_dimension(&session_id, dimension, &json) {
                            log::warn!(
                                "Failed to persist optimization result for {session_id}: {e}"
                            );
                        }
                    }
                    HttpResponse::Ok()
                        .content_type("application/json")
                        .body(json)
                }
                Err(e) => HttpResponse::InternalServerError()
                    .json(serde_json::json!({"error": e.to_string()})),
            }
        }
    }
}

/// GET /api/optimize/sessions/{session_id}/results
///
/// Returns previously persisted analysis results for a session (dimension
/// payloads parsed back into JSON, null when never analyzed).
#[get("/optimize/sessions/{session_id}/results")]
pub async fn get_optimization_results(
    data: web::Data<AppState>,
    path: web::Path<String>,
) -> impl Responder {
    let session_id = path.into_inner();
    let state = match optimize_state(&data) {
        Ok(s) => s,
        Err(resp) => return resp,
    };
    let Some(ref store) = state.store else {
        return HttpResponse::ServiceUnavailable()
            .json(serde_json::json!({"error": "optimization store unavailable"}));
    };

    match store.get(&session_id) {
        Ok(Some(record)) => {
            let parse = |s: &Option<String>| -> serde_json::Value {
                s.as_deref()
                    .and_then(|v| serde_json::from_str(v).ok())
                    .unwrap_or(serde_json::Value::Null)
            };
            HttpResponse::Ok().json(serde_json::json!({
                "session_id": record.session_id,
                "perf": parse(&record.perf),
                "perf_issues": parse(&record.perf_issues),
                "cost": parse(&record.cost),
                "cost_waste": parse(&record.cost_waste),
                "accuracy": parse(&record.accuracy),
                "summary": parse(&record.summary),
                "created_at_ns": record.created_at_ns,
                "updated_at_ns": record.updated_at_ns,
            }))
        }
        Ok(None) => HttpResponse::Ok().json(serde_json::json!({
            "session_id": session_id,
            "perf": null, "perf_issues": null, "cost": null,
            "cost_waste": null, "accuracy": null, "summary": null,
        })),
        Err(e) => {
            HttpResponse::InternalServerError().json(serde_json::json!({"error": e.to_string()}))
        }
    }
}

/// Query parameters for the analysis-history listing.
#[derive(Debug, Deserialize)]
pub struct HistoryQuery {
    pub start_ns: Option<i64>,
    pub end_ns: Option<i64>,
    pub limit: Option<usize>,
}

/// Current UNIX time in nanoseconds; 0 if the clock is before the epoch.
fn now_ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

/// Max rows a single history listing may return.
const HISTORY_MAX_LIMIT: usize = 200;
/// Default listing window when the caller passes no range: 30 days.
const HISTORY_DEFAULT_WINDOW_NS: i64 = 30 * 86_400_000_000_000;

/// GET /api/optimize/results?start_ns=&end_ns=&limit=
///
/// Lists previously analyzed sessions, newest first. Only per-dimension
/// presence flags are returned — never the payloads, which are megabytes of
/// JSON and are fetched per session via `/optimize/sessions/{id}/results`.
#[get("/optimize/results")]
pub async fn list_optimization_history(
    data: web::Data<AppState>,
    query: web::Query<HistoryQuery>,
) -> impl Responder {
    let state = match optimize_state(&data) {
        Ok(s) => s,
        Err(resp) => return resp,
    };
    // Store unavailable → empty list rather than an error: the history table is
    // a secondary view and should degrade like the trajectory endpoints do.
    let Some(ref store) = state.store else {
        return HttpResponse::Ok().json(Vec::<serde_json::Value>::new());
    };

    let end_ns = query.end_ns.unwrap_or_else(now_ns);
    let start_ns = query
        .start_ns
        .unwrap_or_else(|| end_ns.saturating_sub(HISTORY_DEFAULT_WINDOW_NS));
    let limit = query.limit.unwrap_or(100).clamp(1, HISTORY_MAX_LIMIT);

    match store.list(start_ns, end_ns, limit) {
        Ok(records) => {
            let items: Vec<serde_json::Value> = records
                .iter()
                .map(|r| {
                    let mut dimensions = Vec::new();
                    for (name, value) in [
                        ("perf", &r.perf),
                        ("perf_issues", &r.perf_issues),
                        ("cost", &r.cost),
                        ("cost_waste", &r.cost_waste),
                        ("accuracy", &r.accuracy),
                        ("summary", &r.summary),
                    ] {
                        if value.is_some() {
                            dimensions.push(name);
                        }
                    }
                    serde_json::json!({
                        "session_id": r.session_id,
                        "dimensions": dimensions,
                        "created_at_ns": r.created_at_ns,
                        "updated_at_ns": r.updated_at_ns,
                    })
                })
                .collect();
            HttpResponse::Ok().json(items)
        }
        Err(e) => {
            HttpResponse::InternalServerError().json(serde_json::json!({"error": e.to_string()}))
        }
    }
}

/// GET /api/optimize/config — current LLM config with masked API key.
#[get("/optimize/config")]
pub async fn get_optimize_config(data: web::Data<AppState>) -> impl Responder {
    let state = match optimize_state(&data) {
        Ok(s) => s,
        Err(resp) => return resp,
    };
    let config = state.snapshot();
    HttpResponse::Ok().json(serde_json::json!({
        "api_key": config.masked_api_key(),
        "base_url": config.effective_base_url(),
        "model": config.effective_model(),
        "configured": config.effective_api_key().is_some(),
    }))
}

/// Body for POST /api/optimize/config. Omitted fields keep their prior value.
#[derive(Debug, Deserialize)]
pub struct UpdateOptConfig {
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub model: Option<String>,
}

/// POST /api/optimize/config — update LLM config (persisted to disk).
#[post("/optimize/config")]
pub async fn update_optimize_config(
    data: web::Data<AppState>,
    body: web::Json<UpdateOptConfig>,
) -> impl Responder {
    let state = match optimize_state(&data) {
        Ok(s) => s,
        Err(resp) => return resp,
    };

    let updated = {
        let mut config = match state.config.write() {
            Ok(c) => c,
            Err(_) => {
                return HttpResponse::InternalServerError()
                    .json(serde_json::json!({"error": "config lock poisoned"}));
            }
        };
        if let Some(ref key) = body.api_key {
            if !key.is_empty() && !key.contains('•') {
                config.api_key = Some(key.clone());
            }
        }
        if let Some(ref url) = body.base_url {
            if !url.is_empty() {
                config.base_url = Some(url.clone());
            }
        }
        if let Some(ref model) = body.model {
            if !model.is_empty() {
                config.model = Some(model.clone());
            }
        }
        config.clone()
    };

    if let Err(e) = updated.save(&state.config_path) {
        return HttpResponse::InternalServerError().json(serde_json::json!({
            "error": format!("failed to persist config: {e}"),
        }));
    }

    HttpResponse::Ok().json(serde_json::json!({
        "api_key": updated.masked_api_key(),
        "base_url": updated.effective_base_url(),
        "model": updated.effective_model(),
        "configured": updated.effective_api_key().is_some(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The eBPF export feeds the optimizer through JSON, so the shared-schema
    /// document must survive the analyzer's parser with tokens and per-step
    /// timing intact — the same contract the collected v1.7 trajectories rely on.
    #[test]
    fn converted_export_parses_into_analyzer_trajectory() {
        let events = crate::atif::converter::tests::two_call_chain();
        let doc = crate::atif::convert_session_to_atif("session-1", events).unwrap();
        let json = serde_json::to_string(&doc).unwrap();

        let traj = AtifTrajectory::from_json(&json).expect("analyzer must accept the export");
        assert_eq!(traj.session_id, "session-1");
        assert_eq!(traj.model_name(), "claude-opus-5");

        let agent_steps: Vec<_> = traj.steps.iter().filter(|s| s.is_agent()).collect();
        assert_eq!(agent_steps.len(), 2);
        // start_ts comes from extra.start_timestamp, end_ts from timestamp;
        // losing either one silently zeroes out the perf dimension.
        assert!(agent_steps[0].start_ts().is_some());
        assert!(agent_steps[0].end_ts() > agent_steps[0].start_ts());
        assert_eq!(agent_steps[0].results().len(), 1);
        assert_eq!(
            traj.final_metrics
                .as_ref()
                .and_then(|m| m.total_prompt_tokens),
            Some(200)
        );
    }

    #[test]
    fn config_prefers_explicit_values_and_masks_key() {
        let config = OptLlmConfig {
            api_key: Some("sk-1234567890abcd".into()),
            base_url: Some("http://localhost/v1".into()),
            model: Some("test-model".into()),
            search_timeout_secs: None,
        };

        assert_eq!(
            config.effective_api_key().as_deref(),
            Some("sk-1234567890abcd")
        );
        assert_eq!(config.effective_base_url(), "http://localhost/v1");
        assert_eq!(config.effective_model(), "test-model");
        assert_eq!(config.masked_api_key().as_deref(), Some("sk-123••••abcd"));
    }

    #[test]
    fn config_ignores_empty_values_and_masks_short_key() {
        let config = OptLlmConfig {
            api_key: Some("short".into()),
            base_url: Some(String::new()),
            model: Some(String::new()),
            search_timeout_secs: None,
        };

        assert_eq!(config.effective_api_key().as_deref(), Some("short"));
        assert_eq!(config.masked_api_key().as_deref(), Some("••••••"));
    }

    /// The API key must reach disk sealed (issue: plaintext key protected
    /// only by 0o600 leaks via backups/snapshots/root compromise).
    #[test]
    fn save_seals_api_key_and_load_restores_it() {
        let dir = tmp_dir("sealed-config");
        let path = dir.join(CONFIG_FILE_NAME);
        let config = OptLlmConfig {
            api_key: Some("sk-super-secret-key".into()),
            base_url: Some("http://localhost/v1".into()),
            model: Some("test-model".into()),
            search_timeout_secs: None,
        };
        config.save(&path).unwrap();

        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("sk-super-secret-key"), "plaintext on disk");
        assert!(raw.contains("enc:v1:"), "api_key must be sealed");
        // Non-sensitive fields stay readable for debugging.
        assert!(raw.contains("http://localhost/v1"));

        let (loaded, needs_reseal) = OptLlmConfig::load(&path);
        assert!(!needs_reseal);
        assert_eq!(loaded.api_key.as_deref(), Some("sk-super-secret-key"));
        assert_eq!(loaded.model.as_deref(), Some("test-model"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Legacy plaintext configs stay usable and are flagged for resealing.
    #[test]
    fn load_flags_legacy_plaintext_key_for_reseal() {
        let dir = tmp_dir("legacy-config");
        let path = dir.join(CONFIG_FILE_NAME);
        std::fs::write(&path, r#"{"api_key":"sk-legacy-plain","model":"m"}"#).unwrap();

        let (loaded, needs_reseal) = OptLlmConfig::load(&path);
        assert!(needs_reseal);
        assert_eq!(loaded.api_key.as_deref(), Some("sk-legacy-plain"));

        // The migration path: saving removes the plaintext copy.
        loaded.save(&path).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("sk-legacy-plain"));
        let (reloaded, needs_reseal) = OptLlmConfig::load(&path);
        assert!(!needs_reseal);
        assert_eq!(reloaded.api_key.as_deref(), Some("sk-legacy-plain"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An undecryptable envelope (foreign host / lost salt) must degrade to
    /// "not configured" instead of surfacing ciphertext as an API key.
    #[test]
    fn load_drops_undecryptable_key() {
        let dir = tmp_dir("undecryptable-config");
        let path = dir.join(CONFIG_FILE_NAME);
        std::fs::write(
            &path,
            r#"{"api_key":"enc:v1:AAAAAAAAAAAAAAAA:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAA","model":"m"}"#,
        )
        .unwrap();

        let (loaded, needs_reseal) = OptLlmConfig::load(&path);
        assert!(!needs_reseal);
        assert_eq!(loaded.api_key, None);
        assert_eq!(loaded.model.as_deref(), Some("m"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parses_known_optimization_dimensions() {
        assert_eq!(parse_dimension("perf"), Some(Dimension::Perf));
        assert_eq!(parse_dimension("perf-issues"), Some(Dimension::PerfIssues));
        assert_eq!(parse_dimension("cost"), Some(Dimension::Cost));
        assert_eq!(parse_dimension("cost-waste"), Some(Dimension::CostWaste));
        assert_eq!(parse_dimension("accuracy"), Some(Dimension::Accuracy));
        assert_eq!(parse_dimension("unknown"), None);
    }

    fn tmp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("opt-traj-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn collected_record(session_id: &str, atif_json: &str) -> TrajectoryRecord {
        TrajectoryRecord {
            session_id: session_id.to_string(),
            schema_version: "ATIF-v1.7".to_string(),
            agent_name: "qoder".to_string(),
            model_name: None,
            num_steps: 0,
            total_prompt_tokens: None,
            total_completion_tokens: None,
            start_time: None,
            end_time: None,
            first_user_message: None,
            last_user_message: None,
            atif_json: atif_json.to_string(),
            project: "proj".to_string(),
            source: "qoder".to_string(),
            is_subagent: false,
            file_path: String::new(),
            file_size: 0,
            file_mtime_ns: 0,
        }
    }

    #[test]
    fn load_trajectory_falls_back_to_collected_store() {
        let dir = tmp_dir("fallback");
        let db_path = dir.join("genai.db");
        let tstore = TrajectoryStore::new_with_path(&dir.join("trajectories.db")).unwrap();
        let atif = r#"{"schema_version":"ATIF-v1.7","session_id":"log-1","steps":[]}"#;
        tstore
            .upsert_trajectory(&collected_record("log-1", atif))
            .unwrap();

        let trajectory = load_trajectory(&db_path, Some(Arc::new(tstore)), "log-1").unwrap();
        assert_eq!(trajectory.session_id, "log-1");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_trajectory_returns_not_found_when_both_sources_miss() {
        let dir = tmp_dir("miss");
        let db_path = dir.join("genai.db");
        let tstore = TrajectoryStore::new_with_path(&dir.join("trajectories.db")).unwrap();

        // Store present but session absent → 404.
        let resp = load_trajectory(&db_path, Some(Arc::new(tstore)), "nope").unwrap_err();
        assert_eq!(resp.status(), actix_web::http::StatusCode::NOT_FOUND);

        // No store at all → 404 as well.
        let resp = load_trajectory(&db_path, None, "nope").unwrap_err();
        assert_eq!(resp.status(), actix_web::http::StatusCode::NOT_FOUND);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_trajectory_rejects_corrupt_collected_atif() {
        let dir = tmp_dir("corrupt");
        let db_path = dir.join("genai.db");
        let tstore = TrajectoryStore::new_with_path(&dir.join("trajectories.db")).unwrap();
        tstore
            .upsert_trajectory(&collected_record("bad-1", "not json"))
            .unwrap();

        let resp = load_trajectory(&db_path, Some(Arc::new(tstore)), "bad-1").unwrap_err();
        assert_eq!(
            resp.status(),
            actix_web::http::StatusCode::UNPROCESSABLE_ENTITY
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn dimension_doc(
        traj_id: &str,
        session_id: &str,
        prompt: u64,
    ) -> agentsight_atif::AtifTrajectory {
        agentsight_atif::AtifTrajectory {
            schema_version: agentsight_atif::ATIF_SCHEMA_VERSION.to_string(),
            agent: agentsight_atif::Agent {
                name: "agentsight-opt".into(),
                version: "test".into(),
                model_name: Some("gpt-4o".into()),
                tool_definitions: None,
                extra: None,
            },
            steps: vec![],
            session_id: Some(session_id.to_string()),
            trajectory_id: Some(traj_id.to_string()),
            notes: None,
            final_metrics: Some(agentsight_atif::FinalMetrics {
                total_prompt_tokens: Some(prompt),
                total_completion_tokens: Some(10),
                total_cached_tokens: None,
                total_cost_usd: None,
                total_steps: None,
                extra: None,
            }),
            continued_trajectory_ref: None,
            subagent_trajectories: None,
            extra: None,
        }
    }

    /// The record a dimension run persists before `upsert_opt_run_root` folds
    /// it into the root, mirroring the shape written by `run_llm_dimension`.
    fn subagent_record(session_id: &str) -> TrajectoryRecord {
        let mut record = collected_record(session_id, "{}");
        record.is_subagent = true;
        record
    }

    #[test]
    fn opt_run_root_collects_one_dispatch_per_dimension() {
        let dir = tmp_dir("runroot");
        let tstore = TrajectoryStore::new_with_path(&dir.join("trajectories.db")).unwrap();

        upsert_opt_run_root(
            &tstore,
            "target-1",
            "perf-issues",
            &dimension_doc("perf-issues", "opt:target-1:subagent:perf-issues", 100),
        )
        .unwrap();
        upsert_opt_run_root(
            &tstore,
            "target-1",
            "cost-waste",
            &dimension_doc("cost-waste", "opt:target-1:subagent:cost-waste", 200),
        )
        .unwrap();

        let json = tstore.get_atif_json("opt:target-1").unwrap().unwrap();
        let root: agentsight_atif::AtifTrajectory = serde_json::from_str(&json).unwrap();

        // One dispatch step per dimension; totals sum the per-step metrics.
        assert_eq!(root.steps.len(), 2);
        assert_eq!(root.session_id.as_deref(), Some("opt:target-1"));
        let metrics = root.final_metrics.unwrap();
        assert_eq!(metrics.total_prompt_tokens, Some(300));
        assert_eq!(metrics.total_completion_tokens, Some(20));

        // Dispatch step carries ToolCall(Agent) + subagent ref.
        let tc = &root.steps[1].tool_calls.as_ref().unwrap()[0];
        assert_eq!(tc.function_name, "Agent");
        assert_eq!(tc.arguments["subagent_type"], "cost-waste");
        let refs = root.steps[1].observation.as_ref().unwrap().results[0]
            .subagent_trajectory_ref
            .as_ref()
            .unwrap();
        assert_eq!(refs[0].trajectory_id.as_deref(), Some("cost-waste"));
        assert_eq!(
            refs[0].session_id.as_deref(),
            Some("opt:target-1:subagent:cost-waste")
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rerunning_a_dimension_replaces_its_dispatch_instead_of_appending() {
        let dir = tmp_dir("rerun");
        let tstore = TrajectoryStore::new_with_path(&dir.join("trajectories.db")).unwrap();

        for prompt in [100, 500] {
            tstore
                .upsert_trajectory(&subagent_record("opt:target-1:subagent:accuracy"))
                .unwrap();
            upsert_opt_run_root(
                &tstore,
                "target-1",
                "accuracy",
                &dimension_doc("accuracy", "opt:target-1:subagent:accuracy", prompt),
            )
            .unwrap();
        }

        let json = tstore.get_atif_json("opt:target-1").unwrap().unwrap();
        let root: agentsight_atif::AtifTrajectory = serde_json::from_str(&json).unwrap();

        // Second run overwrites the first: still one step, totals not doubled.
        assert_eq!(root.steps.len(), 1);
        assert_eq!(root.steps[0].step_id, 1);
        let metrics = root.final_metrics.unwrap();
        assert_eq!(metrics.total_prompt_tokens, Some(500));
        assert_eq!(metrics.total_completion_tokens, Some(10));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn opt_run_root_prunes_subagent_rows_from_superseded_runs() {
        let dir = tmp_dir("prune");
        let tstore = TrajectoryStore::new_with_path(&dir.join("trajectories.db")).unwrap();

        // Rows written by the pre-dedupe id scheme (`<dim>-<uuid>`), plus one
        // belonging to a dimension that is not being re-run.
        for (dim, legacy_id) in [
            ("accuracy", "opt:target-1:subagent:accuracy-aaaaaaaa"),
            ("accuracy", "opt:target-1:subagent:accuracy-bbbbbbbb"),
            ("summary", "opt:target-1:subagent:summary-cccccccc"),
        ] {
            tstore
                .upsert_trajectory(&subagent_record(legacy_id))
                .unwrap();
            upsert_opt_run_root(&tstore, "target-1", dim, &dimension_doc(dim, legacy_id, 10))
                .unwrap();
        }

        tstore
            .upsert_trajectory(&subagent_record("opt:target-1:subagent:accuracy"))
            .unwrap();
        upsert_opt_run_root(
            &tstore,
            "target-1",
            "accuracy",
            &dimension_doc("accuracy", "opt:target-1:subagent:accuracy", 100),
        )
        .unwrap();

        // Both stale accuracy rows are gone; the untouched summary row stays.
        let json = tstore.get_atif_json("opt:target-1").unwrap().unwrap();
        let root: agentsight_atif::AtifTrajectory = serde_json::from_str(&json).unwrap();
        assert_eq!(
            root.steps.len(),
            2,
            "one step per dimension: accuracy, summary"
        );
        assert_eq!(
            tstore
                .get_subagent_atif_jsons("opt:target-1")
                .unwrap()
                .len(),
            2,
            "stale accuracy runs pruned, summary row retained"
        );
        assert!(
            tstore
                .get("opt:target-1:subagent:accuracy-aaaaaaaa")
                .unwrap()
                .is_none()
        );
        assert!(
            tstore
                .get("opt:target-1:subagent:summary-cccccccc")
                .unwrap()
                .is_some()
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn root_upsert_keeps_unlinked_sibling_dimension_rows() {
        let dir = tmp_dir("sibling");
        let tstore = TrajectoryStore::new_with_path(&dir.join("trajectories.db")).unwrap();

        // Dimension B persisted its subagent row but has not folded its
        // dispatch into the root yet — the interleaving the handler's
        // root_lock serializes against, plus the error path where B's own
        // root update failed after the row write.
        tstore
            .upsert_trajectory(&subagent_record("opt:target-1:subagent:cost-waste"))
            .unwrap();

        // Dimension A runs a full root upsert, including the prune pass.
        tstore
            .upsert_trajectory(&subagent_record("opt:target-1:subagent:accuracy"))
            .unwrap();
        upsert_opt_run_root(
            &tstore,
            "target-1",
            "accuracy",
            &dimension_doc("accuracy", "opt:target-1:subagent:accuracy", 100),
        )
        .unwrap();

        // B's row survives: prune only targets legacy `<dim>-<uuid>` rows.
        assert!(
            tstore
                .get("opt:target-1:subagent:cost-waste")
                .unwrap()
                .is_some(),
            "sibling row written before its root update must not be pruned"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn search_timeout_defaults_to_shared_default() {
        assert_eq!(
            OptLlmConfig::default().search_timeout(),
            std::time::Duration::from_secs(semantic_search::DEFAULT_SEARCH_TIMEOUT_SECS)
        );
    }

    #[test]
    fn search_timeout_secs_round_trips_and_stays_absent_when_unset() {
        let config: OptLlmConfig =
            serde_json::from_str(r#"{"search_timeout_secs": 30}"#).expect("deserialize");
        assert_eq!(config.search_timeout(), std::time::Duration::from_secs(30));
        // Dashboard-managed files must not grow keys the user never set.
        let serialized = serde_json::to_string(&OptLlmConfig::default()).expect("serialize");
        assert!(!serialized.contains("search_timeout_secs"), "{serialized}");
    }
}
