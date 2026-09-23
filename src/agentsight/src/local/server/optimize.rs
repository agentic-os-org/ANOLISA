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
const DB_FILE_NAME: &str = "optimization.db";

/// Runtime LLM configuration for optimization analysis.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OptLlmConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Deadline for semantic-search ranking requests in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_search_timeout_secs: Option<u64>,
}

impl OptLlmConfig {
    fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    fn save(&self, path: &Path) -> std::io::Result<()> {
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

    fn effective_semantic_search_timeout_secs(&self) -> u64 {
        self.semantic_search_timeout_secs
            .filter(|timeout_secs| *timeout_secs > 0)
            .unwrap_or(semantic_search::DEFAULT_SEMANTIC_SEARCH_TIMEOUT_SECS)
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

pub struct OptimizeState {
    config_path: PathBuf,
    config: RwLock<OptLlmConfig>,
    store: Option<OptimizationStore>,
}

impl OptimizeState {
    pub fn init(base_dir: &Path) -> Arc<Self> {
        let config_path = base_dir.join(CONFIG_FILE_NAME);
        let config = OptLlmConfig::load(&config_path);
        let store = match OptimizationStore::new_with_path(&base_dir.join(DB_FILE_NAME)) {
            Ok(store) => Some(store),
            Err(e) => {
                log::warn!("Failed to open local optimization store: {e}");
                None
            }
        };
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

    fn semantic_search_timeout_secs(&self) -> u64 {
        self.snapshot().effective_semantic_search_timeout_secs()
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

/// GET /api/optimize/results
#[get("/api/optimize/results")]
pub async fn list_optimization_history(
    data: web::Data<OptimizeAppState>,
    query: web::Query<HistoryQuery>,
) -> impl Responder {
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
    HttpResponse::Ok().json(serde_json::json!({
        "api_key": config.masked_api_key(),
        "base_url": config.effective_base_url(),
        "model": config.effective_model(),
        "semantic_search_timeout_secs": config.effective_semantic_search_timeout_secs(),
        "configured": config.effective_api_key().is_some(),
    }))
}

#[derive(Debug, Deserialize)]
pub struct UpdateOptConfig {
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub semantic_search_timeout_secs: Option<u64>,
}

/// POST /api/optimize/config
#[post("/api/optimize/config")]
pub async fn update_optimize_config(
    data: web::Data<OptimizeAppState>,
    body: web::Json<UpdateOptConfig>,
) -> impl Responder {
    let updated = {
        let mut config = match data.optimize.config.write() {
            Ok(config) => config,
            Err(_) => {
                return HttpResponse::InternalServerError()
                    .json(serde_json::json!({"error": "config lock poisoned"}));
            }
        };
        if let Some(ref key) = body.api_key
            && !key.is_empty()
            && !key.contains('•')
        {
            config.api_key = Some(key.clone());
        }
        if let Some(ref url) = body.base_url
            && !url.is_empty()
        {
            config.base_url = Some(url.clone());
        }
        if let Some(ref model) = body.model
            && !model.is_empty()
        {
            config.model = Some(model.clone());
        }
        if let Some(timeout_secs) = body.semantic_search_timeout_secs
            && timeout_secs > 0
        {
            config.semantic_search_timeout_secs = Some(timeout_secs);
        }
        config.clone()
    };

    if let Err(e) = updated.save(&data.optimize.config_path) {
        return HttpResponse::InternalServerError().json(serde_json::json!({
            "error": format!("failed to persist config: {e}")
        }));
    }

    HttpResponse::Ok().json(serde_json::json!({
        "api_key": updated.masked_api_key(),
        "base_url": updated.effective_base_url(),
        "model": updated.effective_model(),
        "semantic_search_timeout_secs": updated.effective_semantic_search_timeout_secs(),
        "configured": updated.effective_api_key().is_some(),
    }))
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
            return HttpResponse::Ok()
                .json(semantic_search::SemanticSearchResponse { results: vec![] });
        }
    };
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
    semantic_search::handle_semantic_search(
        &client,
        &request,
        data.optimize.semantic_search_timeout_secs(),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn test_opt_llm_config_semantic_search_timeout_defaults_and_overrides() {
        assert_eq!(
            OptLlmConfig::default().effective_semantic_search_timeout_secs(),
            semantic_search::DEFAULT_SEMANTIC_SEARCH_TIMEOUT_SECS
        );
        let config = OptLlmConfig {
            semantic_search_timeout_secs: Some(12),
            ..Default::default()
        };
        assert_eq!(config.effective_semantic_search_timeout_secs(), 12);
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
            semantic_search_timeout_secs: Some(12),
        };
        config.save(&tmp).unwrap();

        let loaded = OptLlmConfig::load(&tmp);
        assert_eq!(loaded.api_key.as_deref(), Some("sk-testkey"));
        assert_eq!(loaded.base_url.as_deref(), Some("https://test.api.com"));
        assert_eq!(loaded.model.as_deref(), Some("test-model"));
        assert_eq!(loaded.semantic_search_timeout_secs, Some(12));
        let _ = std::fs::remove_file(&tmp);
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
    fn test_optimize_state_init() {
        let tmp = std::env::temp_dir().join("agentsight_opt_state_test");
        std::fs::create_dir_all(&tmp).unwrap();
        let state = OptimizeState::init(&tmp);
        assert!(state.store.is_some());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
