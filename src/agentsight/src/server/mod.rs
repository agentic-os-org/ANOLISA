//! API server module
//!
//! Provides a lightweight HTTP API server using actix-web for querying
//! AgentSight storage data, and optionally serves the embedded frontend.

pub mod auth;
mod capabilities;
mod causal;
pub(crate) mod causal_store;
mod containment;
mod enforcement;
mod handlers;
pub mod optimize;
mod preferences;
mod reuse;
mod secret;
pub(crate) mod storage_status;
mod system_audit;
mod token_savings;

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use actix_cors::Cors;
use actix_web::{App, HttpRequest, HttpResponse, HttpServer, Responder, get, web};
use agentsight_audit::AuditService;
use include_dir::{Dir, include_dir};

use crate::config::{ServerAuthConfig, StorageConfig};
use crate::enforcement::{EnforcementClient, EnforcementCoordinator, EnforcementStore};
use crate::grader::EvaluationStore;
use crate::health::{HealthChecker, HealthStore};
use crate::security::{ContainmentCoordinator, SecurityCoordinator};
use crate::storage::sqlite::InterruptionStore;
use agentsight_trajectory_collector::TrajectoryStore;

use self::auth::{AuthMiddleware, DashboardAuth};

/// Embedded frontend static files (built from dashboard/ via `npm run build:embed`)
/// The directory `frontend-dist/` must exist at compile time; if it is absent
/// (e.g. first build before running npm), Rust will use an empty dir.
static FRONTEND: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/frontend-dist");

/// agent-sec security observability integration configuration.
#[derive(Clone, Debug)]
pub struct SecurityObservabilityConfig {
    /// Per-request daemon timeout.
    pub timeout_ms: u64,
}

impl Default for SecurityObservabilityConfig {
    fn default() -> Self {
        Self { timeout_ms: 5_000 }
    }
}

/// Shared application state accessible from all handlers
pub struct AppState {
    /// Path to the SQLite database file
    pub storage_path: PathBuf,
    /// Server start time (for uptime calculation)
    pub start_time: Instant,
    /// Shared health store populated by the background HealthChecker
    pub health_store: Arc<RwLock<HealthStore>>,
    /// Interruption events store
    pub interruption_store: Option<Arc<InterruptionStore>>,
    /// Grader evaluation store
    pub evaluation_store: Arc<EvaluationStore>,
    /// Desired enforcement state and privileged-service client.
    pub enforcement: Option<Arc<EnforcementCoordinator>>,
    /// Case-level durable containment orchestration.
    pub containment: Option<Arc<ContainmentCoordinator>>,
    /// AgentSight-owned system-audit application service.
    pub audit_service: Arc<AuditService>,
    /// agent-sec security observability integration configuration
    pub security_observability: SecurityObservabilityConfig,
    /// Dashboard authentication state
    pub auth: Arc<DashboardAuth>,
    /// Optimization analysis state (LLM config + result store)
    pub optimize: Option<Arc<optimize::OptimizeState>>,
    /// Trajectory reuse labels (`reuse.db`).
    ///
    /// `None` when the private store could not be opened: labels are a
    /// dashboard feature, so the rest of the server still serves and the
    /// endpoints report why rather than the process refusing to start.
    pub reuse_store: Option<Arc<crate::reuse::ReuseStore>>,
    /// Read-only store over collected trajectories (`trajectories.db`)
    ///
    /// Wrapped in `RwLock` so `trajectory_store()` can memoize lazy opens
    /// (write once when the DB first appears, read on every subsequent call).
    pub trajectory_store: Arc<RwLock<Option<Arc<TrajectoryStore>>>>,
    /// Whether a model may be asked to label trajectories the rules could not
    /// place. Off by default: every judgement is a paid request.
    pub reuse_llm_judge_enabled: bool,
    /// Durable causal attribution results (`causal.db`).
    ///
    /// `None` when the private store could not be opened: attribution still
    /// runs and still serves from its in-memory cache, it just stops surviving
    /// restarts — a degraded mode beats refusing to serve at all.
    pub causal_store: Option<Arc<causal_store::CausalCaseStore>>,
}

impl AppState {
    /// Return a trajectory store if collection has produced `trajectories.db`.
    ///
    /// `serve` is commonly started before `trace`; in that case the DB does not
    /// exist at server startup. Re-checking on demand lets the UI show newly
    /// collected log sessions without requiring a server restart.
    pub fn trajectory_store(&self) -> Option<Arc<TrajectoryStore>> {
        // Fast path: already opened
        {
            let guard = self
                .trajectory_store
                .read()
                .unwrap_or_else(|e| e.into_inner());
            if let Some(store) = guard.as_ref() {
                return Some(Arc::clone(store));
            }
        }

        // Check if DB exists
        let db_path = storage_data_dir(&self.storage_path).join("trajectories.db");
        if !db_path.exists() {
            return None;
        }

        // Try to open; upgrade to write lock to memoize
        match TrajectoryStore::open_read_only_existing(&db_path) {
            Ok(store) => {
                let mut guard = self
                    .trajectory_store
                    .write()
                    .unwrap_or_else(|e| e.into_inner());
                // Double-check: another thread may have opened it while we waited
                if let Some(existing) = guard.as_ref() {
                    Some(Arc::clone(existing))
                } else {
                    let arc = Arc::new(store);
                    *guard = Some(Arc::clone(&arc));
                    log::info!("Trajectory store opened lazily at {db_path:?}");
                    Some(arc)
                }
            }
            Err(e) => {
                log::warn!("Failed to lazily open trajectory store: {e}");
                None
            }
        }
    }
}

// ─── Static file handler ─────────────────────────────────────────────────────

/// Serve embedded frontend files.
/// Any path that doesn't start with /api or /health is treated as a static
/// asset; unknown paths fall back to index.html (SPA client-side routing).
#[get("/")]
async fn serve_frontend_root() -> impl Responder {
    serve_frontend_path("")
}

#[get("/{tail:.*}")]
async fn serve_frontend(req: HttpRequest) -> impl Responder {
    let path = req.match_info().get("tail").unwrap_or("");
    serve_frontend_path(path)
}

fn serve_frontend_path(path: &str) -> HttpResponse {
    // Try exact match first
    let file = if path.is_empty() {
        FRONTEND.get_file("index.html")
    } else {
        FRONTEND.get_file(path)
    };

    match file {
        Some(f) => {
            let mime = if path.is_empty() {
                "text/html; charset=utf-8"
            } else {
                mime_for_path(path)
            };
            HttpResponse::Ok().content_type(mime).body(f.contents())
        }
        None => {
            // SPA fallback: return index.html for unmatched paths
            match FRONTEND.get_file("index.html") {
                Some(index) => HttpResponse::Ok()
                    .content_type("text/html; charset=utf-8")
                    .body(index.contents()),
                None => HttpResponse::NotFound()
                    .body("Frontend not embedded. Run `npm run build:embed` first."),
            }
        }
    }
}

fn mime_for_path(path: &str) -> &'static str {
    if path.ends_with(".html") {
        "text/html; charset=utf-8"
    } else if path.ends_with(".js") {
        "application/javascript; charset=utf-8"
    } else if path.ends_with(".css") {
        "text/css; charset=utf-8"
    } else if path.ends_with(".json") {
        "application/json"
    } else if path.ends_with(".svg") {
        "image/svg+xml"
    } else if path.ends_with(".png") {
        "image/png"
    } else if path.ends_with(".ico") {
        "image/x-icon"
    } else if path.ends_with(".woff2") {
        "font/woff2"
    } else {
        "application/octet-stream"
    }
}

fn configure_routes(cfg: &mut web::ServiceConfig) {
    cfg
        // Top-level health & metrics (not under /api)
        .service(handlers::health)
        .service(handlers::metrics)
        // Auth endpoints (exempt from middleware)
        .service(
            web::scope("/api/auth")
                .service(handlers::auth_status)
                .service(handlers::auth_verify)
                .service(web::resource("/login").route(web::post().to(handlers::auth_login))),
        )
        // All API routes under /api scope
        .service(
            web::scope("/api")
                .service(handlers::list_sessions)
                .service(handlers::list_traces_by_session)
                .service(handlers::get_session_resources)
                .service(handlers::get_trace_detail)
                .service(handlers::get_conversation_events)
                .service(handlers::evaluate_grader)
                .service(handlers::latest_grader)
                .service(handlers::list_agent_names)
                .service(handlers::get_timeseries)
                .service(handlers::get_latency_metrics)
                .service(handlers::export_atif_trace)
                .service(handlers::export_atif_session)
                .service(handlers::export_atif_conversation)
                .service(handlers::get_agent_health)
                .service(handlers::get_agent_process_health)
                .service(
                    web::resource("/agent-health/{pid}")
                        .route(web::delete().to(handlers::delete_agent_health)),
                )
                .service(handlers::restart_agent_health)
                // Interruption API routes
                .service(handlers::list_interruptions)
                .service(handlers::interruption_count)
                .service(handlers::interruption_stats)
                .service(handlers::interruption_session_counts)
                .service(handlers::interruption_conversation_counts)
                .service(handlers::list_session_interruptions)
                .service(handlers::list_conversation_interruptions)
                .service(
                    web::resource("/interruptions/{interruption_id}/resolve")
                        .route(web::post().to(handlers::resolve_interruption)),
                )
                .service(handlers::get_interruption)
                .service(token_savings::get_token_savings)
                .service(token_savings::get_session_savings)
                // AgentSight local security and system-audit API routes
                .service(handlers::security_status)
                .service(handlers::security_summary)
                .service(handlers::security_events_count_by)
                .service(handlers::security_events_list)
                .service(handlers::security_event_detail)
                .service(handlers::security_observability_sessions)
                .service(handlers::security_observability_runs)
                .service(handlers::security_observability_timeline)
                .service(system_audit::summary)
                .service(system_audit::sessions)
                .service(system_audit::events)
                .service(system_audit::cases)
                .service(system_audit::case_detail)
                .service(system_audit::review_case)
                .service(containment::containment_plan)
                .service(containment::contain_case)
                // AgentSight-owned enforcement API routes
                .service(enforcement::health)
                .service(enforcement::apply_binding)
                .service(enforcement::apply_file_binding)
                .service(enforcement::apply_credential_binding)
                .service(enforcement::preview_agent_protection)
                .service(enforcement::list_bindings)
                .service(enforcement::detach_binding)
                .service(enforcement::list_violations)
                // Skill Metrics API routes
                .service(handlers::skill_metrics_all)
                .service(handlers::skill_metrics_downloads)
                .service(handlers::skill_metrics_loads)
                .service(handlers::skill_metrics_usage_ratio)
                .service(handlers::skill_metrics_distribution)
                .service(handlers::skill_metrics_hotness)
                // Optimization analysis API routes
                .service(optimize::run_optimization)
                .service(optimize::get_optimization_results)
                .service(optimize::list_optimization_history)
                .service(optimize::get_optimize_config)
                .service(optimize::update_optimize_config)
                .service(optimize::semantic_search_sessions)
                // User preference analysis API routes (export before the shorter path)
                .service(reuse::run_judgements)
                .service(reuse::apply_label)
                .service(reuse::confirm_labels)
                .service(reuse::label_stats)
                .service(reuse::list_sessions)
                .service(reuse::run_triage)
                .service(preferences::export_preferences)
                .service(preferences::get_preferences)
                .service(preferences::get_preference_turns)
                // Causal attribution API routes
                .service(causal::run_causal_attribution)
                // Trajectory collection API routes (static paths before the dynamic segment)
                .service(handlers::list_trajectories)
                .service(handlers::trajectory_filters)
                .service(handlers::list_trajectory_steps)
                .service(handlers::get_trajectory_detail)
                // Storage lifecycle status
                .service(storage_status::get_storage_status)
                // API self-documentation
                .service(web::resource("/docs").route(web::get().to(api_docs)))
                .default_service(web::route().to(api_not_found)),
        )
        // Health scope with not-found fallback
        .service(web::scope("/health").default_service(web::route().to(api_not_found)))
        // Frontend static files (catch-all, must be last)
        .service(serve_frontend_root)
        .service(serve_frontend);
}

/// Route inventory served by `GET /api/docs`.
///
/// Keep in sync with `configure_routes` above — the doc test in this module
/// spot-checks a few entries but cannot detect every drift.
const API_ROUTES: &[(&str, &str, &str)] = &[
    ("GET", "/health", "Liveness probe (localhost only)"),
    ("GET", "/metrics", "Prometheus metrics (localhost only)"),
    (
        "GET",
        "/api/auth/status",
        "Whether dashboard auth is enabled",
    ),
    ("GET", "/api/auth/verify", "Verify a dashboard token"),
    (
        "POST",
        "/api/auth/login",
        "Exchange token for a session cookie",
    ),
    ("GET", "/api/docs", "This route list"),
    ("GET", "/api/sessions", "List observed agent sessions"),
    (
        "POST",
        "/api/sessions/search",
        "Semantic session search via the configured LLM",
    ),
    (
        "GET",
        "/api/sessions/{session_id}/traces",
        "Traces of a session",
    ),
    (
        "GET",
        "/api/sessions/{session_id}/resources",
        "Process resource timeline of a session",
    ),
    ("GET", "/api/traces/{trace_id}", "Trace detail"),
    (
        "GET",
        "/api/conversations/{conversation_id}",
        "Conversation events",
    ),
    ("GET", "/api/agent-names", "Distinct agent names"),
    ("GET", "/api/timeseries", "Token/call time series"),
    ("GET", "/api/metrics/latency", "LLM latency metrics"),
    ("POST", "/api/grader/evaluate", "Run grader on a session"),
    ("GET", "/api/grader/latest", "Latest grader result"),
    (
        "GET",
        "/api/export/atif/trace/{trace_id}",
        "Export trace as ATIF",
    ),
    (
        "GET",
        "/api/export/atif/session/{session_id}",
        "Export session as ATIF",
    ),
    (
        "GET",
        "/api/export/atif/conversation/{conversation_id}",
        "Export conversation as ATIF",
    ),
    (
        "GET",
        "/api/agent-health",
        "Historical Agent activity from SQLite",
    ),
    (
        "GET",
        "/api/agent-process-health",
        "Live Agent process health (see filtered_count)",
    ),
    (
        "DELETE",
        "/api/agent-health/{pid}",
        "Acknowledge an offline agent",
    ),
    (
        "POST",
        "/api/agent-health/{pid}/restart",
        "Restart a hung agent",
    ),
    ("GET", "/api/interruptions", "List interruption events"),
    ("GET", "/api/interruptions/count", "Interruption count"),
    ("GET", "/api/interruptions/stats", "Interruption statistics"),
    (
        "GET",
        "/api/interruptions/session-counts",
        "Counts per session",
    ),
    (
        "GET",
        "/api/interruptions/conversation-counts",
        "Counts per conversation",
    ),
    (
        "GET",
        "/api/interruptions/{interruption_id}",
        "Interruption detail",
    ),
    (
        "POST",
        "/api/interruptions/{interruption_id}/resolve",
        "Mark an interruption resolved",
    ),
    (
        "GET",
        "/api/sessions/{session_id}/interruptions",
        "Interruptions of a session",
    ),
    (
        "GET",
        "/api/conversations/{conversation_id}/interruptions",
        "Interruptions of a conversation",
    ),
    ("GET", "/api/token-savings", "Token savings summary"),
    (
        "GET",
        "/api/token-savings/session/{session_id}",
        "Token savings of a session",
    ),
    ("GET", "/api/security/status", "Security module status"),
    ("GET", "/api/security/summary", "Security event summary"),
    (
        "GET",
        "/api/security/events/count-by",
        "Security event counts",
    ),
    ("GET", "/api/security/events", "Security event list"),
    (
        "GET",
        "/api/security/events/{event_id}",
        "Security event detail",
    ),
    (
        "GET",
        "/api/security/observability/sessions",
        "Security sessions",
    ),
    (
        "GET",
        "/api/security/observability/sessions/{session_id}/runs",
        "Security session runs",
    ),
    (
        "GET",
        "/api/security/observability/timeline",
        "Security timeline",
    ),
    ("GET", "/api/audit/summary", "System audit summary"),
    ("GET", "/api/audit/events", "System audit events"),
    ("GET", "/api/audit/sessions", "System audit sessions"),
    ("GET", "/api/audit/cases", "System audit cases"),
    ("GET", "/api/audit/cases/{case_id}", "Audit case detail"),
    (
        "POST",
        "/api/audit/cases/{case_id}/review",
        "Review an audit case",
    ),
    (
        "GET",
        "/api/audit/cases/{case_id}/containment-plan",
        "Containment plan for a case",
    ),
    (
        "POST",
        "/api/audit/cases/{case_id}/contain",
        "Contain an audit case",
    ),
    ("GET", "/api/enforcement/health", "Enforcer health"),
    (
        "POST",
        "/api/enforcement/bindings",
        "Apply enforcement binding (token required)",
    ),
    (
        "POST",
        "/api/enforcement/file-bindings",
        "Apply file binding (token required)",
    ),
    (
        "POST",
        "/api/enforcement/credential-bindings",
        "Apply credential binding (token required)",
    ),
    (
        "GET",
        "/api/enforcement/bindings",
        "List enforcement bindings",
    ),
    (
        "DELETE",
        "/api/enforcement/bindings/{binding_id}",
        "Detach a binding (token required)",
    ),
    (
        "GET",
        "/api/enforcement/violations",
        "List enforcement violations",
    ),
    ("GET", "/api/skill-metrics", "All skill metrics"),
    (
        "GET",
        "/api/skill-metrics/downloads",
        "Skill download counts",
    ),
    ("GET", "/api/skill-metrics/loads", "Skill load counts"),
    ("GET", "/api/skill-metrics/usage-ratio", "Skill usage ratio"),
    (
        "GET",
        "/api/skill-metrics/distribution",
        "Skill usage distribution",
    ),
    ("GET", "/api/skill-metrics/hotness", "Skill hotness ranking"),
    (
        "POST",
        "/api/optimize/sessions/{session_id}/{dimension}",
        "Run optimization analysis",
    ),
    (
        "GET",
        "/api/optimize/sessions/{session_id}/results",
        "Optimization results of a session",
    ),
    (
        "GET",
        "/api/optimize/results",
        "Latest optimization results",
    ),
    ("GET", "/api/optimize/config", "Optimization config"),
    ("POST", "/api/optimize/config", "Update optimization config"),
    (
        "POST",
        "/api/reuse/triage",
        "Label collected trajectories with deterministic rules",
    ),
    ("GET", "/api/reuse/sessions", "List trajectory reuse labels"),
    (
        "POST",
        "/api/reuse/sessions/{session_id}/label",
        "Confirm or override one trajectory label",
    ),
    (
        "POST",
        "/api/reuse/sessions/labels:batch-confirm",
        "Confirm automatic labels in batch",
    ),
    (
        "GET",
        "/api/reuse/label-stats",
        "Acceptance and override statistics by rule",
    ),
    (
        "POST",
        "/api/reuse/judge",
        "Use the configured LLM to label unresolved trajectories (feature-gated)",
    ),
    (
        "GET",
        "/api/preferences",
        "User preference analysis (rule + optional LLM)",
    ),
    (
        "GET",
        "/api/preferences/export",
        "User preferences as Markdown",
    ),
    (
        "GET",
        "/api/preferences/turns",
        "Raw user turns for agent-side LLM reasoning",
    ),
    ("POST", "/api/causal-attribution", "Run causal attribution"),
    ("GET", "/api/trajectories", "List collected trajectories"),
    (
        "GET",
        "/api/trajectories/filters",
        "Trajectory filter values",
    ),
    (
        "GET",
        "/api/trajectories/steps",
        "Steps by derived category, with surrounding context",
    ),
    ("GET", "/api/trajectories/{session_id}", "Trajectory detail"),
    ("GET", "/api/storage/status", "SQLite storage status"),
];

/// GET /api/docs — machine-readable route inventory for integrators, so
/// endpoints are discoverable without reverse-engineering the frontend bundle.
async fn api_docs() -> impl Responder {
    let routes: Vec<serde_json::Value> = API_ROUTES
        .iter()
        .map(|(method, path, description)| {
            serde_json::json!({
                "method": method,
                "path": path,
                "description": description,
            })
        })
        .collect();
    HttpResponse::Ok().json(serde_json::json!({
        "service": "agentsight",
        "routes": routes,
    }))
}

async fn api_not_found() -> impl Responder {
    HttpResponse::NotFound().json(serde_json::json!({
        "error": "not_found",
        "message": "No matching API endpoint; see GET /api/docs for the route list"
    }))
}

/// Builds the JSON body extractor config registered on the server `App`.
///
/// Actix's default rejection is a `text/plain` raw serde message; API
/// consumers expect the structured `{"error":{...}}` envelope, so every
/// `web::Json` failure is rewritten here. Kept as a shared constructor so
/// tests exercise the exact handler used in production.
fn json_extractor_config() -> web::JsonConfig {
    // Generic prefix keeps Rust type names out of the leading text while
    // preserving the serde detail (field/variant names are public API states).
    web::JsonConfig::default()
        .error_handler(|error, _req| extractor_error(format!("invalid request body: {error}")))
}

/// Builds the typed path extractor config registered on the server `App`.
///
/// Also turns typed-path failures (e.g. a non-UUID `{binding_id}`) into 400
/// instead of actix's default 404, matching handlers that parse ids manually.
fn path_extractor_config() -> web::PathConfig {
    web::PathConfig::default()
        .error_handler(|error, _req| extractor_error(format!("invalid path parameter: {error}")))
}

/// Wraps an extractor failure into a 400 response with the shared envelope.
fn extractor_error(message: String) -> actix_web::Error {
    let response = system_audit::error_response(
        actix_web::http::StatusCode::BAD_REQUEST,
        "bad_request",
        &message,
        false,
    );
    actix_web::error::InternalError::from_response(message, response).into()
}

// ─── Server entry point ───────────────────────────────────────────────────────

fn private_state_dir(storage_path: &Path) -> PathBuf {
    storage_data_dir(storage_path).join(".agentsight-private")
}

/// Directory holding the sibling databases of `storage_path`.
///
/// `serve --db` points at one database file; every other store must follow it
/// into the same directory, otherwise browsing an archived copy mixes its
/// sessions with the live host's interruptions. A bare relative `--db name.db`
/// has an empty parent, which means the current directory (`.`) — resolving it
/// to the system default would reintroduce the mixed-data behaviour.
fn storage_data_dir(storage_path: &Path) -> &Path {
    match storage_path.parent() {
        Some(parent) if parent.as_os_str().is_empty() => Path::new("."),
        Some(parent) => parent,
        None => Path::new("/var/log/sysak/.agentsight"),
    }
}

/// Start the API server
///
/// Binds to the given host:port and serves API endpoints + embedded frontend.
/// This function blocks until the server is shut down.
pub async fn run_server(
    host: &str,
    port: u16,
    storage_path: PathBuf,
    auth_config: ServerAuthConfig,
    storage_config: StorageConfig,
    reuse_llm_judge_enabled: bool,
) -> std::io::Result<()> {
    let security_observability = SecurityObservabilityConfig::default();

    let state_dir = private_state_dir(&storage_path);
    let security_store = Arc::new(
        crate::security::open_private_store(&state_dir)
            .map_err(|error| std::io::Error::other(error.to_string()))?,
    );
    let audit_service = Arc::new(AuditService::new(security_store.audit_store()));

    let evaluation_store = Arc::new(
        EvaluationStore::new_with_policy(&storage_path, storage_config.genai)
            .map_err(|error| std::io::Error::other(error.to_string()))?,
    );

    // Labels sit beside the other private databases: opening tightens the
    // directory to 0700, which is why it must not be the shared data directory.
    let reuse_store = match crate::reuse::ReuseStore::open_private(&state_dir) {
        Ok(store) => Some(Arc::new(store)),
        Err(error) => {
            log::warn!("Reuse label store unavailable, labels disabled: {error}");
            None
        }
    };

    // Attribution cases are paid pipeline runs; losing them on restart means
    // paying again for the same answer. Same degrade-don't-die rule as labels.
    let causal_store = match causal_store::CausalCaseStore::open_private(&state_dir) {
        Ok(store) => Some(Arc::new(store)),
        Err(error) => {
            log::warn!("Causal case store unavailable, results will not persist: {error}");
            None
        }
    };

    let enforcement_client = EnforcementClient::new(capabilities::enforcer_socket_path());
    let enforcement = Arc::new(EnforcementCoordinator::new(
        enforcement_client.clone(),
        EnforcementStore::open_private(&state_dir)
            .map_err(|error| std::io::Error::other(error.to_string()))?,
    ));
    let enforcement_ingestion = enforcement
        .start_ingestion()
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    let security_coordinator =
        SecurityCoordinator::with_service(enforcement_client, Arc::clone(&audit_service));
    let security_ingestion = match security_coordinator.start() {
        Ok(ingestion) => ingestion,
        Err(error) => {
            stop_enforcement_ingestion(&enforcement, enforcement_ingestion);
            return Err(std::io::Error::other(error.to_string()));
        }
    };
    let containment = Arc::new(ContainmentCoordinator::new(
        Arc::clone(&security_store),
        enforcement.clone(),
    ));
    let containment_reconciler = match containment::start_reconciler(&containment) {
        Ok(worker) => worker,
        Err(error) => {
            stop_security_ingestion(&security_coordinator, security_ingestion);
            stop_enforcement_ingestion(&enforcement, enforcement_ingestion);
            return Err(std::io::Error::other(error.to_string()));
        }
    };

    // Initialize dashboard authentication
    let storage_base = storage_path
        .parent()
        .unwrap_or(std::path::Path::new("/var/log/sysak/.agentsight"));
    let dashboard_auth = Arc::new(DashboardAuth::init(&auth_config, storage_base));
    if dashboard_auth.enabled {
        if let Some(token) = dashboard_auth.read_token_from_file() {
            let masked = if token.len() > 8 {
                format!("{}****", &token[..8])
            } else {
                "****".to_string()
            };
            eprintln!(
                "Dashboard auth enabled. Token: {masked}  (use `agentsight dashboard` to view)"
            );
        }
    }

    // Initialize GenAI SQLite store (needed for HealthChecker to query pending calls).
    // Open the same database `--db` selected so the checker reads pending calls and
    // writes agent_crash events into one consistent dataset.
    let genai_store: Option<Arc<crate::storage::sqlite::GenAISqliteStore>> =
        match crate::storage::sqlite::GenAISqliteStore::new_with_path(
            &storage_path,
            storage_config.genai,
        ) {
            Ok(store) => {
                log::info!("GenAI SQLite store initialized for HealthChecker at {storage_path:?}");
                Some(Arc::new(store))
            }
            Err(e) => {
                log::warn!("Failed to initialize GenAI store for HealthChecker: {e}");
                None
            }
        };

    // Initialize interruption store
    let interruption_store: Option<Arc<InterruptionStore>> = {
        let db_path = storage_data_dir(&storage_path).join("interruption_events.db");
        match InterruptionStore::new_with_path(&db_path) {
            Ok(store) => {
                log::info!("Interruption store initialized at {db_path:?}");
                Some(Arc::new(store))
            }
            Err(e) => {
                log::warn!("Failed to open interruption store: {e}");
                None
            }
        }
    };

    // Spin up the background health checker
    let health_store = Arc::new(RwLock::new(HealthStore::new()));
    let mut checker = HealthChecker::new(Arc::clone(&health_store), Duration::from_secs(30));
    if let Some(ref istore) = interruption_store {
        checker = checker.with_interruption_store(Arc::clone(istore));
    }
    if let Some(ref gstore) = genai_store {
        checker = checker.with_genai_store(Arc::clone(gstore));
    }
    checker.start();

    // Initialize read-only trajectory store (collector writes it in `trace` mode;
    // serve only consumes). Path follows `--db` through storage_data_dir so
    // reader and writer always resolve the same file. A missing DB simply
    // yields an empty table → empty API results (graceful degradation).
    // Only open when the file already exists to avoid creating an empty DB as a
    // persistent side-effect in serve mode when collection was never enabled.
    let trajectory_store: Option<Arc<TrajectoryStore>> = {
        let db_path = storage_data_dir(&storage_path).join("trajectories.db");
        if !db_path.exists() {
            log::debug!("Trajectory store not found at {db_path:?}; endpoints degrade to empty");
            None
        } else {
            match TrajectoryStore::open_read_only_existing(&db_path) {
                Ok(store) => {
                    log::info!("Trajectory store initialized at {db_path:?}");
                    Some(Arc::new(store))
                }
                Err(e) => {
                    log::warn!("Failed to open trajectory store: {e}");
                    None
                }
            }
        }
    };

    let optimize_state = optimize::OptimizeState::init(storage_base);
    let optimization_maintenance =
        start_optimization_maintenance(Arc::clone(&optimize_state), storage_config.optimization);

    let data = web::Data::new(AppState {
        storage_path,
        start_time: Instant::now(),
        health_store,
        interruption_store,
        evaluation_store,
        enforcement: Some(Arc::clone(&enforcement)),
        containment: Some(Arc::clone(&containment)),
        audit_service,
        security_observability,
        auth: dashboard_auth.clone(),
        optimize: Some(optimize_state),
        reuse_store,
        reuse_llm_judge_enabled,
        causal_store,
        trajectory_store: Arc::new(RwLock::new(trajectory_store)),
    });
    let audit_retention = start_audit_retention(
        Arc::clone(&data.audit_service),
        storage_config.security_audit,
    );
    let storage_status_config = web::Data::new(storage_config);

    let has_frontend = FRONTEND.get_file("index.html").is_some();
    log::info!("AgentSight API server listening on http://{host}:{port}");
    eprintln!("AgentSight API server listening on http://{host}:{port}");
    if has_frontend {
        eprintln!("Dashboard UI: http://{host}:{port}/");
    } else {
        eprintln!(
            "[WARN] Frontend not embedded. Run `npm run build:embed` in dashboard/ then recompile."
        );
    }

    let server = match HttpServer::new(move || {
        let cors = Cors::default()
            .allow_any_origin()
            .allowed_methods(vec!["GET", "DELETE", "POST", "OPTIONS"])
            .allowed_headers(vec!["Content-Type", "Authorization"])
            .max_age(3600);

        App::new()
            .wrap(cors)
            .wrap(AuthMiddleware::new(dashboard_auth.clone()))
            .app_data(data.clone())
            .app_data(storage_status_config.clone())
            .app_data(json_extractor_config())
            .app_data(path_extractor_config())
            .configure(configure_routes)
    })
    .bind((host, port))
    {
        Ok(server) => server,
        Err(error) => {
            if let Some(worker) = audit_retention {
                worker.abort();
            }
            if let Some(worker) = optimization_maintenance {
                worker.abort();
            }
            containment::stop_reconciler(&containment, containment_reconciler);
            stop_security_ingestion(&security_coordinator, security_ingestion);
            stop_enforcement_ingestion(&enforcement, enforcement_ingestion);
            return Err(error);
        }
    };

    // Guide users toward the `dashboard` subcommand when listening on all interfaces
    if host == "0.0.0.0" || host == "::" {
        eprintln!();
        eprintln!("提示：远程访问需要安全组放行 TCP {port}。");
        eprintln!("运行 'agentsight dashboard' 可自动检测并生成配置命令。");
        eprintln!();
    }

    let server_result = server.run().await;

    if let Some(worker) = audit_retention {
        worker.abort();
    }
    if let Some(worker) = optimization_maintenance {
        worker.abort();
    }
    containment::stop_reconciler(&containment, containment_reconciler);
    stop_security_ingestion(&security_coordinator, security_ingestion);
    stop_enforcement_ingestion(&enforcement, enforcement_ingestion);
    server_result
}

fn start_audit_retention(
    audit_service: Arc<AuditService>,
    policy: crate::config::PeriodicStoragePolicy,
) -> Option<actix_web::rt::task::JoinHandle<()>> {
    (policy.check_interval_secs > 0).then(|| {
        actix_web::rt::spawn(async move {
            loop {
                match audit_service
                    .store()
                    .maintain(agentsight_audit::AuditMaintenancePolicy {
                        retention_days: policy.retention_days,
                        max_db_size_mb: policy.max_db_size_mb,
                    }) {
                    Ok(report)
                        if report.expired_rows > 0 || report.size.deleted_rows > 0 =>
                    {
                        log::info!(
                            "system-audit maintenance deleted {} expired rows and {} size-policy rows",
                            report.expired_rows,
                            report.size.deleted_rows
                        );
                    }
                    Ok(_) => {}
                    Err(error) => log::warn!("system-audit maintenance failed: {error}"),
                }
                actix_web::rt::time::sleep(Duration::from_secs(policy.check_interval_secs)).await;
            }
        })
    })
}

fn start_optimization_maintenance(
    optimize_state: Arc<optimize::OptimizeState>,
    policy: crate::config::PeriodicStoragePolicy,
) -> Option<actix_web::rt::task::JoinHandle<()>> {
    (policy.check_interval_secs > 0).then(|| {
        actix_web::rt::spawn(async move {
            loop {
                match optimize_state.maintain_storage(policy) {
                    Ok(Some(report))
                        if report.expired_results > 0 || report.size.deleted_rows > 0 =>
                    {
                        log::info!(
                            "optimization maintenance deleted {} expired rows and {} size-policy rows",
                            report.expired_results,
                            report.size.deleted_rows
                        );
                    }
                    Ok(_) => {}
                    Err(error) => log::warn!("optimization maintenance failed: {error}"),
                }
                actix_web::rt::time::sleep(Duration::from_secs(policy.check_interval_secs)).await;
            }
        })
    })
}

fn stop_security_ingestion(
    coordinator: &SecurityCoordinator,
    ingestion: std::thread::JoinHandle<()>,
) {
    coordinator.stop();
    if ingestion.join().is_err() {
        log::error!("AgentSight security ingestion worker panicked during shutdown");
    }
}

fn stop_enforcement_ingestion(
    coordinator: &EnforcementCoordinator,
    ingestion: std::thread::JoinHandle<()>,
) {
    coordinator.stop_ingestion();
    if ingestion.join().is_err() {
        log::error!("AgentSight enforcement ingestion worker panicked during shutdown");
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::{Arc, RwLock};
    use std::time::Instant;

    use actix_web::http::StatusCode;
    use actix_web::test as awtest;
    use actix_web::{App, web};

    use crate::grader::EvaluationStore;
    use crate::health::HealthStore;

    #[test]
    fn storage_data_dir_default_matches_legacy_layout() {
        // Without `--db`, serve opens the default GenAI DB; every sibling store
        // must resolve to the historical directory so this change is a no-op
        // for existing deployments.
        let default_db = crate::storage::sqlite::GenAISqliteStore::default_path();
        assert_eq!(
            super::storage_data_dir(&default_db),
            std::path::Path::new("/var/log/sysak/.agentsight"),
        );
    }

    #[test]
    fn storage_data_dir_follows_absolute_db() {
        let db = PathBuf::from("/backup/2026/genai_events.db");
        assert_eq!(
            super::storage_data_dir(&db),
            std::path::Path::new("/backup/2026"),
        );
    }

    #[test]
    fn storage_data_dir_keeps_relative_db_in_cwd() {
        // A bare relative `--db archived.db` must keep its siblings next to it
        // (current directory), not fall back to the system directory.
        let db = PathBuf::from("archived.db");
        assert_eq!(super::storage_data_dir(&db), std::path::Path::new("."));
    }

    use super::auth::DashboardAuth;
    use super::{
        AppState, SecurityObservabilityConfig, TrajectoryStore, configure_routes,
        json_extractor_config, path_extractor_config, private_state_dir, serve_frontend,
        serve_frontend_root,
    };
    use crate::config::ServerAuthConfig;

    #[test]
    fn security_observability_config_defaults_to_five_seconds() {
        let config = SecurityObservabilityConfig::default();

        assert_eq!(config.timeout_ms, 5_000);
    }

    #[test]
    fn private_state_uses_a_dedicated_sibling_directory() {
        assert_eq!(
            private_state_dir(std::path::Path::new("/tmp/agentsight.db")),
            std::path::Path::new("/tmp/.agentsight-private")
        );
    }

    #[test]
    fn trajectory_store_returns_some_when_already_set() {
        let store = TrajectoryStore::new_with_path(std::path::Path::new(":memory:")).unwrap();
        let state = test_app_state_with_trajectory_store(store);

        assert!(state.trajectory_store().is_some());
    }

    #[test]
    fn trajectory_store_returns_none_when_not_set_and_db_missing() {
        let state = test_app_state(0);
        // The default db path (/var/log/sysak/.agentsight/trajectories.db)
        // should not exist in CI, so lazy loading returns None.
        if crate::config::default_base_path()
            .join("trajectories.db")
            .exists()
        {
            return; // db exists, can't test the "missing" path
        }

        assert!(state.trajectory_store().is_none());
    }

    #[actix_web::test]
    async fn configure_routes_registers_security_routes_before_static_fallback() {
        let app = awtest::init_service(
            App::new()
                .app_data(test_app_state(0))
                .configure(configure_routes),
        )
        .await;
        let request = awtest::TestRequest::get()
            .uri("/api/security/summary?limit=bad")
            .to_request();

        let response = awtest::call_service(&app, request).await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn api_docs_lists_routes_and_not_found_points_to_it() {
        let app = awtest::init_service(
            App::new()
                .app_data(test_app_state(0))
                .configure(configure_routes),
        )
        .await;

        let response = awtest::call_service(
            &app,
            awtest::TestRequest::get().uri("/api/docs").to_request(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body: serde_json::Value =
            serde_json::from_slice(&awtest::read_body(response).await).unwrap();
        let routes = body["routes"].as_array().unwrap();
        assert!(routes.len() >= 50, "route inventory should be complete");
        let paths: Vec<&str> = routes.iter().filter_map(|r| r["path"].as_str()).collect();
        for expected in [
            "/api/sessions",
            "/api/token-savings",
            "/api/agent-health",
            "/api/security/summary",
            "/api/storage/status",
            "/api/reuse/triage",
            "/api/reuse/sessions",
            "/api/reuse/sessions/{session_id}/label",
            "/api/reuse/sessions/labels:batch-confirm",
            "/api/reuse/label-stats",
            "/api/reuse/judge",
            "/api/docs",
        ] {
            assert!(paths.contains(&expected), "missing {expected} in /api/docs");
        }

        // The /api fallback must point integrators to the route list.
        let response = awtest::call_service(
            &app,
            awtest::TestRequest::get()
                .uri("/api/v1/metrics")
                .to_request(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body: serde_json::Value =
            serde_json::from_slice(&awtest::read_body(response).await).unwrap();
        assert!(
            body["message"].as_str().unwrap().contains("/api/docs"),
            "404 fallback should reference /api/docs"
        );
    }

    #[actix_web::test]
    async fn configure_routes_registers_enforcement_routes() {
        let app = awtest::init_service(
            App::new()
                .app_data(test_app_state(0))
                .configure(configure_routes),
        )
        .await;
        let request = awtest::TestRequest::get()
            .uri("/api/enforcement/health")
            .to_request();

        let response = awtest::call_service(&app, request).await;

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

        let request = awtest::TestRequest::post()
            .uri("/api/enforcement/file-bindings")
            .insert_header(("content-type", "application/json"))
            .set_payload("{")
            .to_request();

        let response = awtest::call_service(&app, request).await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    /// Asserts the response is a 400 carrying the structured error envelope
    /// produced by the extractor error handlers (issues #2372/#2392).
    async fn assert_bad_request_envelope<B>(
        response: actix_web::dev::ServiceResponse<B>,
        message_fragment: &str,
    ) where
        B: actix_web::body::MessageBody,
    {
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            response
                .headers()
                .get(actix_web::http::header::CONTENT_TYPE)
                .expect("content-type header"),
            "application/json"
        );
        let body: serde_json::Value = awtest::read_body_json(response).await;
        assert_eq!(body["error"]["code"], "bad_request");
        assert_eq!(body["error"]["retryable"], false);
        let message = body["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.contains(message_fragment),
            "message {message:?} does not contain {message_fragment:?}"
        );
    }

    #[actix_web::test]
    async fn review_json_extractor_errors_return_error_envelope() {
        let app = awtest::init_service(
            App::new()
                .app_data(test_app_state(0))
                .app_data(json_extractor_config())
                .app_data(path_extractor_config())
                .configure(configure_routes),
        )
        .await;
        let uri = "/api/audit/cases/00000000-0000-0000-0000-000000000000/review";

        let missing_field = awtest::TestRequest::post()
            .uri(uri)
            .insert_header(("content-type", "application/json"))
            .set_payload("{}")
            .to_request();
        let response = awtest::call_service(&app, missing_field).await;
        assert_bad_request_envelope(response, "missing field").await;

        let invalid_variant = awtest::TestRequest::post()
            .uri(uri)
            .insert_header(("content-type", "application/json"))
            .set_payload(r#"{"status":"bogus"}"#)
            .to_request();
        let response = awtest::call_service(&app, invalid_variant).await;
        assert_bad_request_envelope(response, "unknown variant").await;
    }

    #[actix_web::test]
    async fn enforcement_json_extractor_error_returns_error_envelope() {
        let app = awtest::init_service(
            App::new()
                .app_data(test_app_state(0))
                .app_data(json_extractor_config())
                .app_data(path_extractor_config())
                .configure(configure_routes),
        )
        .await;
        let request = awtest::TestRequest::post()
            .uri("/api/enforcement/bindings")
            .insert_header(("content-type", "application/json"))
            .set_payload("{}")
            .to_request();

        let response = awtest::call_service(&app, request).await;

        assert_bad_request_envelope(response, "invalid request body").await;
    }

    #[actix_web::test]
    async fn enforcement_path_extractor_error_returns_error_envelope() {
        let app = awtest::init_service(
            App::new()
                .app_data(test_app_state(0))
                .app_data(json_extractor_config())
                .app_data(path_extractor_config())
                .configure(configure_routes),
        )
        .await;
        // Without the PathConfig handler actix answers 404 here; 400 matches
        // handlers that parse path ids manually (e.g. audit case_detail).
        let request = awtest::TestRequest::delete()
            .uri("/api/enforcement/bindings/not-a-uuid")
            .to_request();

        let response = awtest::call_service(&app, request).await;

        assert_bad_request_envelope(response, "invalid path parameter").await;
    }

    #[actix_web::test]
    async fn frontend_routes_handle_root_and_tail_paths() {
        let app = awtest::init_service(
            App::new()
                .service(serve_frontend_root)
                .service(serve_frontend),
        )
        .await;

        let root =
            awtest::call_service(&app, awtest::TestRequest::get().uri("/").to_request()).await;
        let tail = awtest::call_service(
            &app,
            awtest::TestRequest::get().uri("/missing").to_request(),
        )
        .await;

        assert!(root.status().is_success() || root.status() == StatusCode::NOT_FOUND);
        assert!(tail.status().is_success() || tail.status() == StatusCode::NOT_FOUND);
    }

    fn test_app_state(timeout_ms: u64) -> web::Data<AppState> {
        let auth_config = ServerAuthConfig { enabled: false };
        let auth = Arc::new(DashboardAuth::init(
            &auth_config,
            std::path::Path::new("/tmp"),
        ));
        web::Data::new(AppState {
            storage_path: PathBuf::from(":memory:"),
            start_time: Instant::now(),
            health_store: Arc::new(RwLock::new(HealthStore::new())),
            interruption_store: None,
            evaluation_store: Arc::new(
                EvaluationStore::new_with_path(std::path::Path::new(":memory:")).unwrap(),
            ),
            enforcement: None,
            containment: None,
            audit_service: Arc::new(agentsight_audit::AuditService::new(
                crate::security::SecurityStore::open_in_memory()
                    .unwrap()
                    .audit_store(),
            )),
            security_observability: SecurityObservabilityConfig { timeout_ms },
            auth,
            optimize: None,
            reuse_store: None,
            reuse_llm_judge_enabled: false,
            causal_store: None,
            trajectory_store: Arc::new(RwLock::new(None)),
        })
    }

    fn test_app_state_with_trajectory_store(store: TrajectoryStore) -> web::Data<AppState> {
        let auth_config = ServerAuthConfig { enabled: false };
        let auth = Arc::new(DashboardAuth::init(
            &auth_config,
            std::path::Path::new("/tmp"),
        ));
        web::Data::new(AppState {
            storage_path: PathBuf::from(":memory:"),
            start_time: Instant::now(),
            health_store: Arc::new(RwLock::new(HealthStore::new())),
            interruption_store: None,
            evaluation_store: Arc::new(
                EvaluationStore::new_with_path(std::path::Path::new(":memory:")).unwrap(),
            ),
            enforcement: None,
            containment: None,
            audit_service: Arc::new(agentsight_audit::AuditService::new(
                crate::security::SecurityStore::open_in_memory()
                    .unwrap()
                    .audit_store(),
            )),
            security_observability: SecurityObservabilityConfig { timeout_ms: 0 },
            auth,
            optimize: None,
            reuse_store: None,
            reuse_llm_judge_enabled: false,
            causal_store: None,
            trajectory_store: Arc::new(RwLock::new(Some(Arc::new(store)))),
        })
    }
}
