//! Local optimization analysis API.
//!
//! Mirrors the Linux `/api/optimize/*` contract, but loads trajectories only
//! from the local `trajectories.db` written by the local collector.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use actix_web::{HttpResponse, Responder, get, post, web};
use agentsight_opt::{AnalyzePipeline, AtifTrajectory, LlmClient};
use agentsight_opt_store::{Dimension, OptimizationStore};
use agentsight_trajectory_collector::TrajectoryStore;
use serde::{Deserialize, Serialize};

use crate::semantic_search;

const CONFIG_FILE_NAME: &str = "optimization_config.json";

/// Runtime LLM configuration for optimization analysis.
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
    fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    fn save(&self, path: &Path) -> std::io::Result<()> {
        preserve_unparseable_config(path)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json =
            serde_json::to_string_pretty(self).map_err(|e| std::io::Error::other(e.to_string()))?;
        std::fs::write(path, json)?;
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
            .unwrap_or_else(|| "https://api.openai.com/v1".to_string())
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
            .unwrap_or_else(|| "gpt-4o".to_string())
    }

    fn search_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(
            self.search_timeout_secs
                .unwrap_or(semantic_search::DEFAULT_SEARCH_TIMEOUT_SECS),
        )
    }

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

/// Back up a config file this process could not parse before `save` replaces it.
///
/// [`OptLlmConfig::load`] treats a file that does not deserialize into the
/// typed config — truncated JSON *or* valid JSON with a wrong field type — as
/// an empty configuration, so its settings — including the stored API key —
/// never enter memory: the next save would overwrite them without a trace.
/// Keep a copy first, mirroring the Linux server's settings path
/// (`server::optimize::preserve_unparseable_config`, which follows the same
/// backup discipline as `config.rs::ensure_default_agents_config`, issue
/// #1502).
fn preserve_unparseable_config(path: &Path) -> std::io::Result<()> {
    let Ok(content) = std::fs::read_to_string(path) else {
        // Absent or unreadable: there is nothing this process is about to lose.
        return Ok(());
    };
    // Validating against the typed struct, not just `serde_json::Value`, is
    // what makes a file like `{"search_timeout_secs": "60"}` unparseable here
    // too: `load` drops it to the default config, so its contents are just as
    // lost as truncated JSON if `save` overwrites it unpreserved.
    if serde_json::from_str::<OptLlmConfig>(&content).is_ok() {
        return Ok(());
    }
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let backup = path.with_extension(format!("json.bak.{ts}"));
    std::fs::copy(path, &backup)?;
    log::warn!(
        "Kept the unparseable optimization config at {backup:?} before overwriting {path:?}"
    );
    Ok(())
}

pub struct OptimizeState {
    config_path: PathBuf,
    config: RwLock<OptLlmConfig>,
    store: Option<Arc<OptimizationStore>>,
}

impl OptimizeState {
    pub fn init(base_dir: &Path, store: Option<Arc<OptimizationStore>>) -> Arc<Self> {
        let config_path = base_dir.join(CONFIG_FILE_NAME);
        let config = OptLlmConfig::load(&config_path);
        Arc::new(Self {
            config_path,
            config: RwLock::new(config),
            store,
        })
    }

    pub(super) fn snapshot(&self) -> OptLlmConfig {
        self.config.read().map(|c| c.clone()).unwrap_or_default()
    }

    // `pub(super)` so the sibling preferences module can reuse the same
    // configured client for its optional LLM layer.
    pub(super) fn build_client(&self) -> Result<LlmClient, HttpResponse> {
        let config = self.snapshot();
        let Some(api_key) = config.effective_api_key() else {
            return Err(HttpResponse::BadRequest().json(serde_json::json!({
                "error": "llm_not_configured",
                "message": "LLM API key not configured. Set it in the optimization settings."
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

pub struct OptimizeAppState {
    pub optimize: Arc<OptimizeState>,
    pub local_state: web::Data<super::LocalState>,
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

fn load_collected_trajectory(
    trajectory_store: Option<&TrajectoryStore>,
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
                "message": e.to_string()
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

fn persist_and_respond<T: Serialize>(
    state: &OptimizeState,
    session_id: &str,
    dimension: Dimension,
    result: &T,
) -> HttpResponse {
    let json = match serde_json::to_string(result) {
        Ok(json) => json,
        Err(e) => {
            return HttpResponse::InternalServerError()
                .json(serde_json::json!({"error": e.to_string()}));
        }
    };
    if let Some(ref store) = state.store
        && let Err(e) = store.save_dimension(session_id, dimension, &json)
    {
        log::warn!("Failed to persist local optimization result for {session_id}: {e}");
    }
    HttpResponse::Ok()
        .content_type("application/json")
        .body(json)
}

/// POST /api/optimize/sessions/{session_id}/{dimension}
#[post("/api/optimize/sessions/{session_id}/{dimension}")]
pub async fn run_optimization(
    data: web::Data<OptimizeAppState>,
    path: web::Path<(String, String)>,
) -> impl Responder {
    let (session_id, dimension_raw) = path.into_inner();
    let Some(dimension) = parse_dimension(&dimension_raw) else {
        return HttpResponse::BadRequest().json(serde_json::json!({
            "error": "unknown dimension",
            "message": "expected one of: perf, perf-issues, cost, cost-waste, accuracy, summary"
        }));
    };

    let tstore = data.local_state.trajectory_store();
    let trajectory = match load_collected_trajectory(tstore.as_deref(), &session_id) {
        Ok(trajectory) => trajectory,
        Err(response) => return response,
    };

    match dimension {
        Dimension::Perf => match AnalyzePipeline::run_perf(&trajectory) {
            Ok(stats) => persist_and_respond(&data.optimize, &session_id, dimension, &stats),
            Err(e) => HttpResponse::InternalServerError()
                .json(serde_json::json!({"error": e.to_string()})),
        },
        Dimension::Cost => match AnalyzePipeline::run_cost(&trajectory) {
            Ok(stats) => persist_and_respond(&data.optimize, &session_id, dimension, &stats),
            Err(e) => HttpResponse::InternalServerError()
                .json(serde_json::json!({"error": e.to_string()})),
        },
        Dimension::PerfIssues | Dimension::CostWaste | Dimension::Accuracy | Dimension::Summary => {
            let client = match data.optimize.build_client() {
                Ok(client) => client,
                Err(response) => return response,
            };
            let pipeline = AnalyzePipeline::new(&client);
            let result: Result<String, anyhow::Error> = match dimension {
                Dimension::PerfIssues => pipeline
                    .run_perf_issues(&trajectory)
                    .await
                    .and_then(|r| serde_json::to_string(&r).map_err(anyhow::Error::from)),
                Dimension::CostWaste => pipeline
                    .run_cost_waste(&trajectory)
                    .await
                    .and_then(|r| serde_json::to_string(&r).map_err(anyhow::Error::from)),
                Dimension::Accuracy => pipeline
                    .run_accuracy(&trajectory, None)
                    .await
                    .and_then(|r| serde_json::to_string(&r).map_err(anyhow::Error::from)),
                Dimension::Summary => pipeline
                    .run_summary(&trajectory)
                    .await
                    .and_then(|r| serde_json::to_string(&r).map_err(anyhow::Error::from)),
                Dimension::Perf | Dimension::Cost => unreachable!(),
            };

            match result {
                Ok(json) => {
                    if let Some(ref store) = data.optimize.store
                        && let Err(e) = store.save_dimension(&session_id, dimension, &json)
                    {
                        log::warn!(
                            "Failed to persist local optimization result for {session_id}: {e}"
                        );
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
#[get("/api/optimize/sessions/{session_id}/results")]
pub async fn get_optimization_results(
    data: web::Data<OptimizeAppState>,
    path: web::Path<String>,
) -> impl Responder {
    let session_id = path.into_inner();
    let Some(ref store) = data.optimize.store else {
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
            "cost_waste": null, "accuracy": null, "summary": null
        })),
        Err(e) => {
            HttpResponse::InternalServerError().json(serde_json::json!({"error": e.to_string()}))
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct HistoryQuery {
    pub start_ns: Option<i64>,
    pub end_ns: Option<i64>,
    pub limit: Option<usize>,
}

fn now_ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

const HISTORY_MAX_LIMIT: usize = 200;
const HISTORY_DEFAULT_WINDOW_NS: i64 = 30 * 86_400_000_000_000;

/// Reject an explicitly inverted time window (`start_ns > end_ns`).
///
/// Mirrors the Linux server's `reject_inverted_window` family: the store's
/// `updated_at_ns >= start AND <= end` predicate matches nothing for an
/// inverted window, so the endpoint used to answer an empty 200 that a
/// caller cannot tell apart from "no results in this range". The guard runs
/// before the store is consulted, so an unconfigured instance answers the
/// family's 400 too, like its Linux sibling.
fn reject_inverted_window(start_ns: Option<i64>, end_ns: Option<i64>) -> Option<HttpResponse> {
    if matches!((start_ns, end_ns), (Some(start), Some(end)) if start > end) {
        return Some(
            HttpResponse::BadRequest()
                .json(serde_json::json!({"error": "start_ns must not exceed end_ns"})),
        );
    }
    None
}

/// GET /api/optimize/results
#[get("/api/optimize/results")]
pub async fn list_optimization_history(
    data: web::Data<OptimizeAppState>,
    query: web::Query<HistoryQuery>,
) -> impl Responder {
    if let Some(response) = reject_inverted_window(query.start_ns, query.end_ns) {
        return response;
    }
    let Some(ref store) = data.optimize.store else {
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
                .map(|record| {
                    let mut dimensions = Vec::new();
                    for (name, value) in [
                        ("perf", &record.perf),
                        ("perf_issues", &record.perf_issues),
                        ("cost", &record.cost),
                        ("cost_waste", &record.cost_waste),
                        ("accuracy", &record.accuracy),
                        ("summary", &record.summary),
                    ] {
                        if value.is_some() {
                            dimensions.push(name);
                        }
                    }
                    serde_json::json!({
                        "session_id": record.session_id,
                        "dimensions": dimensions,
                        "created_at_ns": record.created_at_ns,
                        "updated_at_ns": record.updated_at_ns,
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

/// GET /api/optimize/config
#[get("/api/optimize/config")]
pub async fn get_optimize_config(data: web::Data<OptimizeAppState>) -> impl Responder {
    let config = data.optimize.snapshot();
    HttpResponse::Ok().json(config_response(&config))
}

#[derive(Debug, Deserialize)]
pub struct UpdateOptConfig {
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub search_timeout_secs: Option<u64>,
}

/// Host portion of an absolute http(s) `base_url`, or `None` when the URL
/// does not have that shape. A trailing `:port` (only digits) is stripped;
/// bracketed IPv6 literals keep their brackets.
fn base_url_host(url: &str) -> Option<&str> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = match authority.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => host,
        _ => authority,
    };
    (!host.is_empty()).then_some(host)
}

/// The stored `base_url` becomes the target of every optimization LLM
/// request, so it must be an absolute http(s) URL naming a host — the
/// sink only speaks http(s), and embedded credentials are never intended.
fn validate_base_url(url: &str) -> Result<(), String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .ok_or_else(|| "base_url must be an absolute http(s) URL".to_string())?;
    if base_url_host(url).is_none() {
        return Err("base_url must name a host".to_string());
    }
    if rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .contains('@')
    {
        return Err("base_url must not embed credentials".to_string());
    }
    Ok(())
}

/// The complete externally visible origin of an http(s) `base_url`:
/// scheme, normalized host, and the effective port (a port-less authority
/// takes its scheme's default). `None` when the URL does not have that
/// shape; a bracketed IPv6 literal keeps its brackets.
fn base_url_origin(url: &str) -> Option<(&'static str, String, u16)> {
    let (scheme, rest) = if let Some(rest) = url.strip_prefix("https://") {
        ("https", rest)
    } else if let Some(rest) = url.strip_prefix("http://") {
        ("http", rest)
    } else {
        return None;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port))
            if (host.starts_with('[') || !host.contains(':'))
                && !port.is_empty()
                && port.chars().all(|c| c.is_ascii_digit()) =>
        {
            (host, Some(port.parse::<u16>().ok()?))
        }
        _ => (authority, None),
    };
    if host.is_empty() {
        return None;
    }
    let effective_port = port.unwrap_or(if scheme == "https" { 443 } else { 80 });
    Some((scheme, host.to_ascii_lowercase(), effective_port))
}

/// Whether two base URLs name different origins (scheme, normalized host or
/// effective port). Origin-preserving path adjustments are not changes.
/// Malformed shapes count as changes so they fail closed.
fn origin_changes(current: &str, next: &str) -> bool {
    match (base_url_origin(current), base_url_origin(next)) {
        (Some(a), Some(b)) => a != b,
        _ => true,
    }
}

fn apply_config_update(config: &mut OptLlmConfig, update: &UpdateOptConfig) -> Result<(), String> {
    // Validate the whole update against the stored config before touching
    // anything: a rejected update must not leave a partially-applied state
    // such as a new provider's key paired with the previous endpoint.
    if let Some(ref url) = update.base_url
        && !url.is_empty()
    {
        validate_base_url(url)?;
        // Retargeting the endpoint origin (scheme, host or effective port)
        // silently forwards the stored API key and the analyzed conversation
        // content to a different origin on the next LLM call. The change
        // therefore requires the caller to prove knowledge of the stored key
        // by re-entering it verbatim in the same update; origin-preserving
        // path adjustments stay free. Proving knowledge means matching the
        // stored key: this server is reachable cross-origin from visited web
        // pages, so treating any replacement value as proof would let such a
        // page retarget the endpoint with an attacker-chosen key and
        // exfiltrate the conversation payload to it.
        if origin_changes(&config.effective_base_url(), url) {
            let proves_knowledge = match (
                update.api_key.as_deref(),
                config.effective_api_key().as_deref(),
            ) {
                (Some(fresh), Some(stored)) => {
                    !fresh.is_empty() && !fresh.contains('•') && fresh == stored
                }
                // First-time setup has no stored key to prove knowledge of.
                (Some(fresh), None) => !fresh.is_empty() && !fresh.contains('•'),
                _ => false,
            };
            if !proves_knowledge {
                return Err(
                    "changing the LLM endpoint origin (scheme, host or port) requires \
                     re-entering the current API key in the same update"
                        .to_string(),
                );
            }
        }
    }
    // Apply only after every field validated.
    if let Some(ref key) = update.api_key
        && !key.is_empty()
        && !key.contains('•')
    {
        config.api_key = Some(key.clone());
    }
    if let Some(ref url) = update.base_url
        && !url.is_empty()
    {
        config.base_url = Some(url.clone());
    }
    if let Some(ref model) = update.model
        && !model.is_empty()
    {
        config.model = Some(model.clone());
    }
    if let Some(timeout_secs) = update.search_timeout_secs
        && timeout_secs > 0
    {
        config.search_timeout_secs = Some(timeout_secs);
    }
    Ok(())
}

fn config_response(config: &OptLlmConfig) -> serde_json::Value {
    serde_json::json!({
        "api_key": config.masked_api_key(),
        "base_url": config.effective_base_url(),
        "model": config.effective_model(),
        "search_timeout_secs": config
            .search_timeout_secs
            .unwrap_or(semantic_search::DEFAULT_SEARCH_TIMEOUT_SECS),
        "configured": config.effective_api_key().is_some(),
    })
}

/// POST /api/optimize/config
#[post("/api/optimize/config")]
pub async fn update_optimize_config(
    data: web::Data<OptimizeAppState>,
    body: web::Json<UpdateOptConfig>,
) -> impl Responder {
    let updated = match persist_config_update(&data.optimize, &body) {
        Ok(updated) => updated,
        Err(ConfigUpdateError::Rejected(message)) => {
            return HttpResponse::BadRequest().json(serde_json::json!({
                "error": message
            }));
        }
        Err(ConfigUpdateError::Io(e)) => {
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": format!("failed to persist config: {e}")
            }));
        }
    };

    HttpResponse::Ok().json(config_response(&updated))
}

enum ConfigUpdateError {
    /// The update was refused (invalid endpoint shape, or a host change
    /// without the API key re-entered); nothing was persisted or published.
    Rejected(String),
    Io(std::io::Error),
}

fn persist_config_update(
    state: &OptimizeState,
    update: &UpdateOptConfig,
) -> Result<OptLlmConfig, ConfigUpdateError> {
    let mut config = state
        .config
        .write()
        .map_err(|_| ConfigUpdateError::Io(std::io::Error::other("config lock poisoned")))?;
    // Publish memory only after persistence succeeds, keeping the lock
    // through both so another update cannot save an older snapshot last.
    let mut updated = config.clone();
    apply_config_update(&mut updated, update).map_err(ConfigUpdateError::Rejected)?;
    updated
        .save(&state.config_path)
        .map_err(ConfigUpdateError::Io)?;
    *config = updated.clone();
    Ok(updated)
}

// ─── Semantic session search ────────────────────────────────────────────────
//
// Delegates to the shared ranking module so the dashboard's semantic search
// also works on macOS, where ServeCommand delegates to this local server.

/// POST /api/sessions/search
#[post("/api/sessions/search")]
pub async fn semantic_search_sessions(
    data: web::Data<OptimizeAppState>,
    body: web::Json<semantic_search::SemanticSearchRequest>,
) -> impl Responder {
    let client = match data.optimize.build_client() {
        Ok(c) => c,
        Err(_) => {
            // Same contract as the Linux endpoint: degrade to empty, but make
            // the most common cause (unconfigured LLM) attributable.
            log::warn!(
                "semantic_search: LLM not configured (missing API key), returning empty results"
            );
            return HttpResponse::Ok()
                .json(semantic_search::SemanticSearchResponse { results: vec![] });
        }
    };
    let timeout = data.optimize.snapshot().search_timeout();
    let request = body.into_inner();

    // Sessions labelled `useless` are out of retrieval scope; same filter as
    // the Linux endpoint, applied here for the same reason — one policy, every
    // caller. A store failure degrades to unfiltered rather than failing search.
    let request = match data.local_state.reuse_store.as_deref() {
        Some(store) => match store.excluded_sessions() {
            Ok(excluded) => semantic_search::filter_excluded(
                request,
                &excluded
                    .into_iter()
                    .collect::<std::collections::HashSet<_>>(),
            ),
            Err(error) => {
                log::warn!("reuse: reading excluded sessions failed, ranking unfiltered: {error}");
                request
            }
        },
        None => request,
    };
    semantic_search::handle_semantic_search(&client, &request, timeout).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_config_update_keeps_memory_and_allows_retry() {
        let tmp = std::env::temp_dir().join(format!(
            "agentsight_local_failed_update_{}",
            std::process::id()
        ));
        std::fs::create_dir_all(tmp.join(CONFIG_FILE_NAME)).unwrap();
        let state = OptimizeState::init(&tmp, None);
        let update = UpdateOptConfig {
            model: Some("synthetic-model".into()),
            search_timeout_secs: Some(17),
            api_key: None,
            base_url: None,
        };
        assert!(persist_config_update(&state, &update).is_err());
        assert!(state.snapshot().model.is_none());
        assert!(state.snapshot().search_timeout_secs.is_none());
        std::fs::remove_dir(tmp.join(CONFIG_FILE_NAME)).unwrap();
        persist_config_update(&state, &update).unwrap();
        assert_eq!(state.snapshot().model.as_deref(), Some("synthetic-model"));
        assert_eq!(
            OptLlmConfig::load(&tmp.join(CONFIG_FILE_NAME)).search_timeout_secs,
            Some(17)
        );
        std::fs::remove_dir_all(tmp).unwrap();
    }

    #[test]
    fn concurrent_config_updates_preserve_both_fields_on_restart() {
        let tmp = std::env::temp_dir().join(format!(
            "agentsight_local_concurrent_update_{}",
            std::process::id()
        ));
        let state = OptimizeState::init(&tmp, None);
        let start = Arc::new(std::sync::Barrier::new(3));
        let workers: Vec<_> = (0..2)
            .map(|index| {
                let state = Arc::clone(&state);
                let start = Arc::clone(&start);
                std::thread::spawn(move || {
                    start.wait();
                    for _ in 0..50 {
                        persist_config_update(
                            &state,
                            &UpdateOptConfig {
                                model: (index == 0).then(|| "synthetic-model".into()),
                                search_timeout_secs: (index == 1).then_some(17),
                                api_key: None,
                                base_url: None,
                            },
                        )
                        .unwrap();
                    }
                })
            })
            .collect();
        start.wait();
        for worker in workers {
            worker.join().unwrap();
        }
        let restarted = OptimizeState::init(&tmp, None).snapshot();
        assert_eq!(restarted.model.as_deref(), Some("synthetic-model"));
        assert_eq!(restarted.search_timeout_secs, Some(17));
        assert_eq!(
            state.snapshot().search_timeout_secs,
            restarted.search_timeout_secs
        );
        std::fs::remove_dir_all(tmp).unwrap();
    }

    #[test]
    fn test_opt_llm_config_default() {
        let config = OptLlmConfig::default();
        assert!(config.api_key.is_none());
        assert!(config.base_url.is_none());
        assert!(config.model.is_none());
    }

    #[test]
    fn test_opt_llm_config_effective_base_url_from_config() {
        let config = OptLlmConfig {
            base_url: Some("https://custom.api.com/v1".to_string()),
            ..Default::default()
        };
        assert_eq!(config.effective_base_url(), "https://custom.api.com/v1");
    }

    #[test]
    fn test_opt_llm_config_effective_base_url_default() {
        let config = OptLlmConfig::default();
        assert_eq!(config.effective_base_url(), "https://api.openai.com/v1");
    }

    #[test]
    fn test_opt_llm_config_effective_model_from_config() {
        let config = OptLlmConfig {
            model: Some("gpt-4-turbo".to_string()),
            ..Default::default()
        };
        assert_eq!(config.effective_model(), "gpt-4-turbo");
    }

    #[test]
    fn test_opt_llm_config_effective_model_default() {
        let config = OptLlmConfig::default();
        assert_eq!(config.effective_model(), "gpt-4o");
    }

    #[test]
    fn test_opt_llm_config_effective_api_key_from_config() {
        let config = OptLlmConfig {
            api_key: Some("sk-test123".to_string()),
            ..Default::default()
        };
        assert_eq!(config.effective_api_key(), Some("sk-test123".to_string()));
    }

    #[test]
    fn test_opt_llm_config_effective_api_key_empty() {
        let config = OptLlmConfig {
            api_key: Some("".to_string()),
            ..Default::default()
        };
        assert_eq!(config.effective_api_key(), None);
    }

    #[test]
    fn test_opt_llm_config_masked_api_key_long() {
        let config = OptLlmConfig {
            api_key: Some("sk-1234567890abcdef".to_string()),
            ..Default::default()
        };
        let masked = config.masked_api_key().unwrap();
        assert!(masked.starts_with("sk-123"));
        assert!(masked.contains("••••"));
        assert!(masked.ends_with("cdef"));
    }

    #[test]
    fn test_opt_llm_config_masked_api_key_short() {
        let config = OptLlmConfig {
            api_key: Some("short".to_string()),
            ..Default::default()
        };
        let masked = config.masked_api_key().unwrap();
        assert_eq!(masked, "••••••");
    }

    #[test]
    fn test_opt_llm_config_masked_api_key_none() {
        let config = OptLlmConfig::default();
        assert!(config.masked_api_key().is_none());
    }

    #[test]
    fn test_opt_llm_config_save_and_load() {
        let tmp = std::env::temp_dir().join("agentsight_opt_config_test.json");
        let config = OptLlmConfig {
            api_key: Some("sk-testkey".to_string()),
            base_url: Some("https://test.api.com".to_string()),
            model: Some("test-model".to_string()),
            search_timeout_secs: None,
        };
        config.save(&tmp).unwrap();

        let loaded = OptLlmConfig::load(&tmp);
        assert_eq!(loaded.api_key.as_deref(), Some("sk-testkey"));
        assert_eq!(loaded.base_url.as_deref(), Some("https://test.api.com"));
        assert_eq!(loaded.model.as_deref(), Some("test-model"));
        let _ = std::fs::remove_file(&tmp);
    }

    fn config_backups(dir: &Path) -> Vec<std::path::PathBuf> {
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|p| {
                p.file_name()
                    .is_some_and(|n| n.to_string_lossy().contains(".bak."))
            })
            .collect()
    }

    #[test]
    fn test_opt_llm_config_save_keeps_a_typed_invalid_config() {
        // Valid JSON that does not deserialize into `OptLlmConfig` (here
        // `search_timeout_secs` is a string) is folded into the default config
        // by `load` exactly like truncated JSON, dropping the stored API key
        // and every other setting. `save` must keep a copy before overwriting
        // it — the same guard the Linux server's settings path applies.
        let dir = std::env::temp_dir().join(format!(
            "agentsight_local_opt_typed_invalid_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(CONFIG_FILE_NAME);
        let typed_invalid = r#"{"api_key":"sk-live","model":"qwen","search_timeout_secs":"60"}"#;
        std::fs::write(&path, typed_invalid).unwrap();

        let config = OptLlmConfig {
            model: Some("gpt-4o".to_string()),
            ..OptLlmConfig::default()
        };
        config.save(&path).unwrap();

        let backups = config_backups(&dir);
        assert_eq!(
            backups.len(),
            1,
            "a config that fails OptLlmConfig deserialization must be kept"
        );
        assert_eq!(
            std::fs::read_to_string(&backups[0]).unwrap(),
            typed_invalid,
            "the backup must hold the file exactly as it was"
        );
        let stored = std::fs::read_to_string(&path).unwrap();
        assert!(
            stored.contains("gpt-4o"),
            "the new config is written: {stored}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_opt_llm_config_save_keeps_a_truncated_config() {
        // Truncated JSON is the other shape `load` folds into the default:
        // the file holds a sealed-looking key and no closing brace. Losing it
        // to an overwrite would destroy the only copy of the key.
        let dir = std::env::temp_dir().join(format!(
            "agentsight_local_opt_truncated_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(CONFIG_FILE_NAME);
        let truncated = r#"{"api_key":"sk-live","model":"qwen""#;
        std::fs::write(&path, truncated).unwrap();

        OptLlmConfig::default().save(&path).unwrap();

        let backups = config_backups(&dir);
        assert_eq!(backups.len(), 1, "a truncated config must be kept");
        assert_eq!(std::fs::read_to_string(&backups[0]).unwrap(), truncated);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_opt_llm_config_save_keeps_no_backup_when_parseable() {
        // Control: a config that parses round-trips through `load`, so its
        // values are not about to be lost — `save` must not litter the
        // settings directory with copies.
        let dir = std::env::temp_dir().join(format!(
            "agentsight_local_opt_parseable_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(CONFIG_FILE_NAME);

        OptLlmConfig {
            api_key: Some("sk-first".to_string()),
            ..OptLlmConfig::default()
        }
        .save(&path)
        .unwrap();
        OptLlmConfig {
            model: Some("gpt-4o".to_string()),
            ..OptLlmConfig::default()
        }
        .save(&path)
        .unwrap();

        assert!(
            config_backups(&dir).is_empty(),
            "a parseable config needs no backup"
        );
        let stored = std::fs::read_to_string(&path).unwrap();
        assert!(
            stored.contains("gpt-4o"),
            "the latest config is written: {stored}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_opt_llm_config_load_missing_file() {
        let config = OptLlmConfig::load(std::path::Path::new("/nonexistent/path/config.json"));
        assert!(config.api_key.is_none());
        assert!(config.base_url.is_none());
        assert!(config.model.is_none());
    }

    #[test]
    fn test_parse_dimension_valid() {
        assert!(parse_dimension("perf").is_some());
        assert!(parse_dimension("perf-issues").is_some());
        assert!(parse_dimension("cost").is_some());
        assert!(parse_dimension("cost-waste").is_some());
        assert!(parse_dimension("accuracy").is_some());
        assert!(parse_dimension("summary").is_some());
    }

    #[test]
    fn test_parse_dimension_invalid() {
        assert!(parse_dimension("unknown").is_none());
        assert!(parse_dimension("").is_none());
        assert!(parse_dimension("invalid").is_none());
    }

    #[test]
    fn test_optimize_state_uses_opened_store() {
        let tmp = std::env::temp_dir().join("agentsight_opt_state_test");
        std::fs::create_dir_all(&tmp).unwrap();
        let store = Arc::new(
            OptimizationStore::new_with_path(&tmp.join(crate::config::OPTIMIZATION_DB_NAME))
                .unwrap(),
        );
        let state = OptimizeState::init(&tmp, Some(Arc::clone(&store)));
        assert!(state.store.is_some());
        assert!(Arc::ptr_eq(state.store.as_ref().unwrap(), &store));
        drop(state);
        drop(store);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn config_update_persists_and_returns_search_timeout() {
        let tmp = std::env::temp_dir().join(format!(
            "agentsight_local_opt_config_timeout_{}.json",
            std::process::id()
        ));
        let mut config = OptLlmConfig::default();
        let update = UpdateOptConfig {
            api_key: None,
            base_url: None,
            model: None,
            search_timeout_secs: Some(30),
        };

        apply_config_update(&mut config, &update).unwrap();
        config.save(&tmp).unwrap();
        let loaded = OptLlmConfig::load(&tmp);
        assert_eq!(loaded.search_timeout_secs, Some(30));
        assert_eq!(config_response(&loaded)["search_timeout_secs"], 30);

        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn base_url_origin_parses_scheme_host_and_effective_port() {
        assert_eq!(
            base_url_origin("https://api.example.com/v1"),
            Some(("https", "api.example.com".into(), 443))
        );
        assert_eq!(
            base_url_origin("http://API.example.com:8080/x"),
            Some(("http", "api.example.com".into(), 8080))
        );
        assert_eq!(
            base_url_origin("http://[::1]:8443"),
            Some(("http", "[::1]".into(), 8443))
        );
        assert_eq!(
            base_url_origin("http://localhost"),
            Some(("http", "localhost".into(), 80))
        );
        assert_eq!(base_url_origin("ftp://example.com"), None);
        assert_eq!(base_url_origin("example.com"), None);
    }

    #[test]
    fn origin_gates_cover_scheme_and_port_changes() {
        let mut config = OptLlmConfig {
            api_key: Some("sk-1234567890abcd".into()),
            base_url: Some("https://api.openai.com/v1".into()),
            model: None,
            search_timeout_secs: None,
        };
        // A scheme or port change forwards the stored Bearer key to a
        // different origin and requires the same key proof as a host change.
        assert!(
            apply_config_update(
                &mut config,
                &UpdateOptConfig {
                    api_key: None,
                    base_url: Some("http://api.openai.com/v1".into()),
                    model: None,
                    search_timeout_secs: None,
                },
            )
            .is_err()
        );
        assert!(
            apply_config_update(
                &mut config,
                &UpdateOptConfig {
                    api_key: None,
                    base_url: Some("https://api.openai.com:8443/v1".into()),
                    model: None,
                    search_timeout_secs: None,
                },
            )
            .is_err()
        );
        // The default port may be spelled out; the origin is unchanged.
        assert!(
            apply_config_update(
                &mut config,
                &UpdateOptConfig {
                    api_key: None,
                    base_url: Some("https://api.openai.com:443/v1beta".into()),
                    model: None,
                    search_timeout_secs: None,
                },
            )
            .is_ok()
        );
        assert_eq!(
            config.base_url.as_deref(),
            Some("https://api.openai.com:443/v1beta")
        );
    }

    #[test]
    fn a_wrong_key_does_not_prove_knowledge_and_leaves_nothing_applied() {
        let mut config = OptLlmConfig {
            api_key: Some("sk-1234567890abcd".into()),
            base_url: Some("https://api.openai.com/v1".into()),
            model: Some("gpt-test".into()),
            search_timeout_secs: None,
        };
        // Only the stored key proves knowledge: an attacker-chosen key with
        // an attacker-controlled endpoint is refused, and because validation
        // precedes application the stored config is untouched.
        let refused = apply_config_update(
            &mut config,
            &UpdateOptConfig {
                api_key: Some("attacker-chosen-key".into()),
                base_url: Some("https://attacker.example/v1".into()),
                model: Some("attacker-model".into()),
                search_timeout_secs: Some(999),
            },
        );
        assert!(refused.is_err());
        assert_eq!(config.api_key.as_deref(), Some("sk-1234567890abcd"));
        assert_eq!(config.base_url.as_deref(), Some("https://api.openai.com/v1"));
        assert_eq!(config.model.as_deref(), Some("gpt-test"));
        assert_eq!(config.search_timeout_secs, None);
    }

    #[test]
    fn local_base_url_updates_are_shape_checked_and_host_changes_need_the_key() {
        let mut config = OptLlmConfig {
            api_key: Some("sk-1234567890abcd".into()),
            base_url: Some("https://api.openai.com/v1".into()),
            model: None,
            search_timeout_secs: None,
        };
        // Retargeting the host without proving knowledge of the key is the
        // exfiltration path: the stored key would be sent to the new host.
        let refused = apply_config_update(
            &mut config,
            &UpdateOptConfig {
                api_key: None,
                base_url: Some("https://attacker.example/v1".into()),
                model: None,
                search_timeout_secs: None,
            },
        );
        assert!(refused.is_err());
        assert_eq!(
            config.base_url.as_deref(),
            Some("https://api.openai.com/v1")
        );
        // Same-host path adjustments stay free.
        assert!(
            apply_config_update(
                &mut config,
                &UpdateOptConfig {
                    api_key: None,
                    base_url: Some("https://api.openai.com/v1beta".into()),
                    model: None,
                    search_timeout_secs: None,
                },
            )
            .is_ok()
        );
        // Non-http(s) shapes are refused outright.
        for bad in [
            "",
            "api.example.com/v1",
            "ftp://api.example.com",
            "https://user:pass@e.com",
        ] {
            assert!(
                apply_config_update(
                    &mut config,
                    &UpdateOptConfig {
                        api_key: None,
                        base_url: Some(bad.into()),
                        model: None,
                        search_timeout_secs: None,
                    },
                )
                .is_err(),
                "{bad}"
            );
        }
        // A host change with the key re-entered in the same update applies.
        assert!(
            apply_config_update(
                &mut config,
                &UpdateOptConfig {
                    api_key: Some("sk-1234567890abcd".into()),
                    base_url: Some("https://dashscope.example.com/compatible-mode/v1".into()),
                    model: None,
                    search_timeout_secs: None,
                },
            )
            .is_ok()
        );
        assert_eq!(
            config.base_url.as_deref(),
            Some("https://dashscope.example.com/compatible-mode/v1")
        );
    }

    #[test]
    fn config_response_uses_default_search_timeout() {
        assert_eq!(
            config_response(&OptLlmConfig::default())["search_timeout_secs"],
            semantic_search::DEFAULT_SEARCH_TIMEOUT_SECS
        );
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
        let serialized = serde_json::to_string(&OptLlmConfig::default()).expect("serialize");
        assert!(!serialized.contains("search_timeout_secs"), "{serialized}");
    }

    #[actix_web::test]
    async fn optimization_history_rejects_an_inverted_window() {
        // The Linux sibling rejects `start_ns > end_ns` with the window-guard
        // family's 400 before consulting the optimizer's state; this endpoint
        // handed the inverted range to the store, whose
        // `updated_at_ns >= start AND <= end` predicate matches nothing, and
        // answered an empty 200 that reads as "no results in this range".
        use crate::config::StorageConfig;
        use crate::database::{
            DatabaseAccess, DatabaseCoverage, DatabaseId, DatabaseManager, DatabaseRole,
            DatabaseSpec,
        };
        use actix_web::{App, test as awtest};

        let dir = std::env::temp_dir().join(format!(
            "agentsight-local-opt-inverted-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");

        let db_path = dir.join("trajectories.db");
        let database_manager = Arc::new(
            DatabaseManager::new(
                DatabaseRole::LocalServer,
                [DatabaseSpec::new(
                    DatabaseId::Trajectories,
                    &db_path,
                    DatabaseAccess::ReadOnly,
                    DatabaseCoverage::Partial,
                )],
            )
            .expect("build the local database manager"),
        );
        let storage_config = StorageConfig::default();
        let local_state = web::Data::new(super::super::LocalState {
            trajectory_store: Arc::new(RwLock::new(None)),
            db_path,
            storage_budget: Arc::new(crate::storage_budget::StorageBudget::new(
                None,
                &storage_config,
            )),
            storage_config,
            config_path: None,
            database_manager,
            reuse_store: None,
            reuse_llm_judge_enabled: false,
        });
        // `store: None` is the unconfigured instance: the guard must answer
        // before that state is consulted, like the Linux sibling.
        let state = web::Data::new(OptimizeAppState {
            optimize: OptimizeState::init(&dir, None),
            local_state,
        });

        let app = awtest::init_service(
            App::new()
                .app_data(state)
                .service(list_optimization_history),
        )
        .await;

        let request = awtest::TestRequest::get()
            .uri("/api/optimize/results?start_ns=2000&end_ns=1000")
            .to_request();
        let response = awtest::call_service(&app, request).await;
        assert_eq!(
            response.status(),
            actix_web::http::StatusCode::BAD_REQUEST,
            "an inverted window must be rejected, not answered with an empty 200"
        );
        let body: serde_json::Value = awtest::read_body_json(response).await;
        assert_eq!(body["error"], "start_ns must not exceed end_ns");

        // Controls: an ascending window and one-sided windows stay 200.
        for uri in [
            "/api/optimize/results?start_ns=1000&end_ns=2000",
            "/api/optimize/results?start_ns=2000",
            "/api/optimize/results?end_ns=1000",
            "/api/optimize/results",
        ] {
            let request = awtest::TestRequest::get().uri(uri).to_request();
            let response = awtest::call_service(&app, request).await;
            assert_eq!(
                response.status(),
                actix_web::http::StatusCode::OK,
                "a well-formed window must pass: {uri}"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
