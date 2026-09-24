//! HTTP API server + embedded frontend for local agent trajectory viewing.
//!
//! Simplified server: no Linux AppState or HealthChecker. Serves trajectory
//! SQLite summaries, local-session APIs, and an embedded frontend dashboard.

mod agents;
mod local_sessions;
mod optimize;
mod preferences;
mod reuse;
mod trajectories;

use actix_cors::Cors;
use actix_web::{App, HttpRequest, HttpResponse, HttpServer, Responder, get, web};
use agentsight_opt_store::{OptimizationMaintenancePolicy, OptimizationStore};
use agentsight_sqlite_lifecycle::{LifecycleError, MaintenanceJob};
use agentsight_trajectory_collector::TrajectoryStore;
use include_dir::{Dir, include_dir};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use crate::config::{OPTIMIZATION_DB_NAME, REUSE_DB_NAME, StorageConfig, TRAJECTORY_DB_NAME};
use crate::database::{
    DatabaseAccess, DatabaseCoverage, DatabaseId, DatabaseManager, DatabaseManagerError,
    DatabaseRole, DatabaseSpec,
};

/// Shared state for the macOS local server.
///
/// `trajectory_store` is wrapped in `RwLock` so handlers can lazily open
/// the DB when `trace` starts writing after `serve` has already started.
pub struct LocalState {
    pub trajectory_store: Arc<RwLock<Option<Arc<TrajectoryStore>>>>,
    pub db_path: PathBuf,
    storage_config: StorageConfig,
    database_manager: Arc<DatabaseManager>,
    /// Trajectory reuse labels (`reuse.db`).
    ///
    /// `None` when the private store could not be opened: labels are a
    /// dashboard feature, so the viewer still serves and the endpoints report
    /// why rather than the process refusing to start.
    pub reuse_store: Option<Arc<crate::reuse::ReuseStore>>,
    /// Whether a model may be asked to label trajectories the rules could not
    /// place. Off unless a configuration file says otherwise.
    pub reuse_llm_judge_enabled: bool,
}

impl LocalState {
    /// Return a trajectory store, lazily opening the DB if needed.
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

        if !self.db_path.exists() {
            return None;
        }

        match self.database_manager.open_read_only(
            DatabaseId::Trajectories,
            TrajectoryStore::open_read_only_existing,
        ) {
            Ok(store) => {
                let mut guard = self
                    .trajectory_store
                    .write()
                    .unwrap_or_else(|e| e.into_inner());
                if let Some(existing) = guard.as_ref() {
                    Some(Arc::clone(existing))
                } else {
                    let arc = Arc::new(store);
                    *guard = Some(Arc::clone(&arc));
                    log::info!("Trajectory store opened lazily at {:?}", self.db_path);
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

/// Embedded frontend static files (built from dashboard/ via `npm run build:embed`)
/// Output goes to the agentsight crate root's `frontend-dist/` directory.
/// When absent (e.g. first build before running npm), include_dir! embeds an
/// empty dir and the server prints a warning.
static FRONTEND: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/frontend-dist");

// ─── Static file handler ─────────────────────────────────────────────────────

//
// The main dashboard frontend calls many Linux-only endpoints (sessions,
// auth, interruptions, etc.) that have no data on macOS. These stubs return
// correctly-shaped empty responses so the frontend doesn't crash.

/// GET /api/auth/status — macOS has no auth gate
#[get("/api/auth/status")]
async fn auth_status() -> impl Responder {
    HttpResponse::Ok().json(serde_json::json!({
        "auth_enabled": false,
        "mode": "local",
        "capabilities": ["sessions", "optimization", "reuse_labels", "atif", "settings", "agent_health"]
    }))
}

/// GET /api/auth/verify — always authenticated on macOS
#[get("/api/auth/verify")]
async fn auth_verify() -> impl Responder {
    HttpResponse::Ok().json(serde_json::json!({"authenticated": true}))
}

/// GET /api/sessions — no eBPF sessions on macOS
#[get("/api/sessions")]
async fn list_sessions() -> impl Responder {
    HttpResponse::Ok().json(Vec::<serde_json::Value>::new())
}

/// GET /api/agent-names — empty on macOS
#[get("/api/agent-names")]
async fn list_agent_names() -> impl Responder {
    HttpResponse::Ok().json(Vec::<String>::new())
}

/// GET /api/timeseries — empty on macOS
#[get("/api/timeseries")]
async fn list_timeseries() -> impl Responder {
    HttpResponse::Ok().json(Vec::<serde_json::Value>::new())
}

/// GET /api/token-savings — null on macOS
#[get("/api/token-savings")]
async fn token_savings() -> impl Responder {
    HttpResponse::Ok().json(serde_json::Value::Null)
}

/// GET /api/interruptions — empty array on macOS
#[get("/api/interruptions")]
async fn list_interruptions() -> impl Responder {
    HttpResponse::Ok().json(Vec::<serde_json::Value>::new())
}

/// GET /api/interruptions/count — empty object on macOS
#[get("/api/interruptions/count")]
async fn interruption_count() -> impl Responder {
    HttpResponse::Ok().json(serde_json::json!({}))
}

/// GET /api/interruptions/stats — empty array on macOS
#[get("/api/interruptions/stats")]
async fn interruption_stats() -> impl Responder {
    HttpResponse::Ok().json(Vec::<serde_json::Value>::new())
}

/// GET /api/interruptions/session-counts — empty array on macOS
#[get("/api/interruptions/session-counts")]
async fn interruption_session_counts() -> impl Responder {
    HttpResponse::Ok().json(Vec::<serde_json::Value>::new())
}

/// GET /api/interruptions/conversation-counts — empty array on macOS
#[get("/api/interruptions/conversation-counts")]
async fn interruption_conversation_counts() -> impl Responder {
    HttpResponse::Ok().json(Vec::<serde_json::Value>::new())
}

/// GET /api/security/status — empty on macOS
#[get("/api/security/status")]
async fn security_status() -> impl Responder {
    HttpResponse::Ok().json(serde_json::json!({}))
}

/// GET /api/security/summary — empty on macOS
#[get("/api/security/summary")]
async fn security_summary() -> impl Responder {
    HttpResponse::Ok().json(serde_json::json!({}))
}

/// GET /api/skill-metrics — empty array on macOS
#[get("/api/skill-metrics")]
async fn skill_metrics() -> impl Responder {
    HttpResponse::Ok().json(Vec::<serde_json::Value>::new())
}

#[get("/api/agent-health")]
async fn agent_health(data: web::Data<LocalState>) -> impl Responder {
    let loaded = web::block(move || match data.trajectory_store() {
        Some(store) => store
            .list_agent_activity_summaries()
            .map_err(|error| error.to_string()),
        None => Ok(Vec::new()),
    })
    .await;
    let summaries = match loaded {
        Ok(Ok(summaries)) => summaries,
        Ok(Err(error)) => {
            log::warn!("Failed to query local Agent activity: {error}");
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "Trajectory Agent activity is unavailable"
            }));
        }
        Err(error) => {
            log::warn!("Local Agent activity query worker failed: {error}");
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "Agent activity query worker failed"
            }));
        }
    };
    let agents = summaries
        .into_iter()
        .map(|summary| {
            serde_json::json!({
                "agent_name": summary.agent_name,
                "last_seen_ns": summary.last_seen_ns,
                "genai_calls": 0,
                "genai_tokens": 0,
                "trajectory_steps": summary.total_steps,
                "trajectory_tokens": summary.total_tokens,
                "source": "trajectories"
            })
        })
        .collect::<Vec<_>>();

    HttpResponse::Ok().json(serde_json::json!({ "agents": agents }))
}

#[get("/api/agent-process-health")]
async fn agent_process_health() -> impl Responder {
    let result = web::block(agents::discover_agents_summary).await;
    match result {
        Ok(summary) => {
            let mut rows = Vec::new();
            for agent in summary.agents {
                for pid in agent.pids {
                    rows.push(serde_json::json!({
                        "pid": pid,
                        "agent_name": agent.name,
                        "category": agent.category,
                        "exe_path": agent.cwd,
                        "ports": [],
                        "status": "no_port",
                        "last_check_time": summary.scanned_at * 1000,
                        "latency_ms": null,
                        "error_message": "Local process discovered; HTTP health check is unavailable in local viewer mode",
                        "role": "gateway",
                        "has_crash": false
                    }));
                }
            }
            HttpResponse::Ok().json(serde_json::json!({
                "agents": rows,
                "last_scan_time": summary.scanned_at * 1000,
                "filtered_count": 0
            }))
        }
        Err(e) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": format!("Process scan failed: {e}")
        })),
    }
}

/// GET /api/export/atif/* — eBPF export is unavailable in local mode.
#[get("/api/export/atif/{tail:.*}")]
async fn export_atif_unavailable() -> impl Responder {
    HttpResponse::NotFound().json(serde_json::json!({
        "error": "not_found",
        "message": "eBPF ATIF export is unavailable in local viewer mode"
    }))
}

/// GET /api/storage/status — local trajectory and optimization databases.
#[get("/api/storage/status")]
async fn storage_status(state: web::Data<LocalState>) -> impl Responder {
    HttpResponse::Ok().json(crate::storage_status::collect_local_storage_status(
        &state.db_path,
        &state.storage_config,
        Some(&state.database_manager),
    ))
}

/// Catch-all for any other unregistered /api/* path — returns empty array
/// to avoid breaking frontend list iteration.
#[get("/api/{tail:.*}")]
async fn api_fallback() -> impl Responder {
    HttpResponse::Ok().json(Vec::<serde_json::Value>::new())
}

/// Serve embedded frontend files.
/// Any path that doesn't start with /api is treated as a static asset;
/// unknown paths fall back to index.html (SPA client-side routing).
#[get("/{tail:.*}")]
async fn serve_frontend(req: HttpRequest) -> impl Responder {
    let path = req.match_info().get("tail").unwrap_or("");

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
        "text/css"
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

pub fn local_trajectory_scan_dirs() -> Option<Vec<std::path::PathBuf>> {
    let home = dirs::home_dir()?;
    Some(vec![
        home.join(".claude/projects"),
        home.join(".qoderwork/projects"),
        home.join(".qoder/projects"),
        home.join(".codex/sessions"),
        home.join(".codex/archived_sessions"),
        home.join(".cursor/projects"),
    ])
}

fn local_database_specs(storage_config: &StorageConfig) -> Vec<DatabaseSpec> {
    let private = storage_config.base_path.join(".agentsight-private");
    let tokenless = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".tokenless")
        .join("stats.db");
    vec![
        DatabaseSpec::new(
            DatabaseId::Trajectories,
            storage_config.base_path.join(TRAJECTORY_DB_NAME),
            DatabaseAccess::ReadOnly,
            DatabaseCoverage::Partial,
        ),
        DatabaseSpec::new(
            DatabaseId::Optimization,
            storage_config.base_path.join(OPTIMIZATION_DB_NAME),
            DatabaseAccess::ReadWrite,
            DatabaseCoverage::Full,
        ),
        DatabaseSpec::new(
            DatabaseId::Reuse,
            private.join(REUSE_DB_NAME),
            DatabaseAccess::ReadWrite,
            DatabaseCoverage::Partial,
        ),
        DatabaseSpec::new(
            DatabaseId::Tokenless,
            tokenless,
            DatabaseAccess::External,
            DatabaseCoverage::External,
        ),
    ]
}

struct LocalMaintenanceStores {
    optimization: Option<Arc<OptimizationStore>>,
    reuse: Option<Arc<crate::reuse::ReuseStore>>,
}

fn local_maintenance_jobs(
    manager: &DatabaseManager,
    storage_config: &StorageConfig,
    stores: LocalMaintenanceStores,
) -> Result<Vec<Box<dyn MaintenanceJob>>, DatabaseManagerError> {
    let mut jobs = Vec::new();
    for (id, policy) in [
        (DatabaseId::Optimization, storage_config.optimization),
        (DatabaseId::Reuse, storage_config.reuse),
    ] {
        if policy.check_interval_secs == 0 {
            continue;
        }
        let interval = Duration::from_secs(policy.check_interval_secs);
        let job = match id {
            DatabaseId::Optimization => stores.optimization.as_ref().map(|store| {
                let store = Arc::clone(store);
                manager.maintenance_job(id, interval, move || {
                    store
                        .maintain(OptimizationMaintenancePolicy {
                            retention_days: policy.retention_days,
                            max_db_size_mb: policy.max_db_size_mb,
                        })
                        .map(|_| ())
                        .map_err(|error| LifecycleError::MaintenanceJobFailed(error.to_string()))
                })
            }),
            DatabaseId::Reuse => stores.reuse.as_ref().map(|store| {
                let store = Arc::clone(store);
                manager.maintenance_job(id, interval, move || {
                    store
                        .maintain(policy.retention_days, policy.max_db_size_mb)
                        .map(|_| ())
                        .map_err(|error| LifecycleError::MaintenanceJobFailed(error.to_string()))
                })
            }),
            DatabaseId::Primary
            | DatabaseId::GenAi
            | DatabaseId::Interruptions
            | DatabaseId::Trajectories
            | DatabaseId::SecurityAudit
            | DatabaseId::Enforcement
            | DatabaseId::Causal
            | DatabaseId::Tokenless => None,
        };
        if let Some(job) = job {
            jobs.push(job?);
        }
    }
    Ok(jobs)
}

// ─── Server entry point ───────────────────────────────────────────────────────

/// Start the API server.
///
/// Binds to the given host:port and serves local-session API endpoints + the
/// embedded frontend. Blocks until the server is shut down.
pub async fn run_server(
    host: &str,
    port: u16,
    storage_config: StorageConfig,
    reuse_llm_judge_enabled: bool,
) -> std::io::Result<()> {
    let has_frontend = FRONTEND.get_file("index.html").is_some();
    log::info!(
        "agentsight local server listening on http://{}:{}",
        host,
        port
    );
    eprintln!(
        "agentsight local server listening on http://{}:{}",
        host, port
    );
    if has_frontend {
        eprintln!("Dashboard UI: http://{}:{}/", host, port);
    } else {
        eprintln!(
            "[WARN] Frontend not embedded. Run `npm run build:embed` in dashboard/ then recompile."
        );
    }

    let database_manager = Arc::new(
        DatabaseManager::new(
            DatabaseRole::LocalServer,
            local_database_specs(&storage_config),
        )
        .map_err(|error| std::io::Error::other(error.to_string()))?,
    );
    let db_path = storage_config.trajectory_path();

    // Collection belongs to `agentsight trace`; serve opens only an existing
    // read-only store and retries lazily if trace creates the database later.
    let initial_store: Option<Arc<TrajectoryStore>> = if db_path.exists() {
        match database_manager.open_read_only(
            DatabaseId::Trajectories,
            TrajectoryStore::open_read_only_existing,
        ) {
            Ok(store) => {
                log::info!("Trajectory store initialized at {db_path:?}");
                Some(Arc::new(store))
            }
            Err(error) => {
                log::warn!("Failed to open trajectory store: {error}");
                None
            }
        }
    } else {
        log::debug!("Trajectory DB not found at {db_path:?}; run `agentsight trace` to collect");
        None
    };

    // Opening the private reuse store creates the shared base directory as a
    // parent, while keeping owner-only permissions scoped to its own child.
    let reuse_store = match database_manager.open_read_write(DatabaseId::Reuse, |path| {
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        crate::reuse::ReuseStore::open_private(parent)
    }) {
        Ok(store) => Some(Arc::new(store)),
        Err(error) => {
            log::warn!("Reuse label store unavailable, labels disabled: {error}");
            None
        }
    };
    let optimization_store = match database_manager
        .open_read_write(DatabaseId::Optimization, OptimizationStore::new_with_path)
    {
        Ok(store) => Some(Arc::new(store)),
        Err(error) => {
            log::warn!("Failed to open local optimization store: {error}");
            None
        }
    };

    let local_state = web::Data::new(LocalState {
        trajectory_store: Arc::new(RwLock::new(initial_store)),
        db_path,
        storage_config: storage_config.clone(),
        database_manager: Arc::clone(&database_manager),
        reuse_store: reuse_store.as_ref().map(Arc::clone),
        reuse_llm_judge_enabled,
    });
    let optimize_state = optimize::OptimizeState::init(
        &storage_config.base_path,
        optimization_store.as_ref().map(Arc::clone),
    );
    let optimize_data = web::Data::new(optimize::OptimizeAppState {
        optimize: optimize_state,
        local_state: local_state.clone(),
    });

    let server = HttpServer::new(move || {
        let cors = Cors::default()
            .allow_any_origin()
            .allowed_methods(vec!["GET", "DELETE", "POST", "OPTIONS"])
            .allowed_headers(vec!["Content-Type"])
            .max_age(3600);

        App::new()
            .wrap(cors)
            .app_data(local_state.clone())
            .app_data(optimize_data.clone())
            // Trajectory collection API (static paths before the dynamic segment)
            .service(trajectories::list_trajectories)
            .service(trajectories::trajectory_filters)
            .service(trajectories::list_trajectory_steps)
            .service(trajectories::get_trajectory_detail)
            // Local session discovery + ATIF conversion API
            .service(local_sessions::list_local_sessions)
            .service(local_sessions::convert_local_to_atif)
            .service(local_sessions::read_local_session_file)
            // Agent process discovery API
            .service(agents::list_agents)
            // macOS stubs (no eBPF data available)
            .service(auth_status)
            .service(auth_verify)
            .service(list_sessions)
            .service(list_agent_names)
            .service(list_timeseries)
            .service(token_savings)
            .service(list_interruptions)
            .service(interruption_count)
            .service(interruption_stats)
            .service(interruption_session_counts)
            .service(interruption_conversation_counts)
            .service(security_status)
            .service(security_summary)
            .service(skill_metrics)
            .service(agent_health)
            .service(agent_process_health)
            // Local optimization analysis API
            .service(optimize::run_optimization)
            .service(optimize::get_optimization_results)
            .service(optimize::list_optimization_history)
            .service(optimize::get_optimize_config)
            .service(optimize::update_optimize_config)
            .service(optimize::semantic_search_sessions)
            // User preference analysis API (registered before api_fallback)
            .service(reuse::run_judgements)
            .service(reuse::apply_label)
            .service(reuse::confirm_labels)
            .service(reuse::label_stats)
            .service(reuse::list_sessions)
            .service(reuse::run_triage)
            .service(preferences::export_preferences)
            .service(preferences::get_preferences)
            .service(preferences::get_preference_turns)
            .service(export_atif_unavailable)
            .service(storage_status)
            // Catch-all for unregistered API endpoints (returns empty array)
            .service(api_fallback)
            // Frontend static files (catch-all, must be last)
            .service(serve_frontend)
    })
    .bind((host, port))?;

    let jobs = local_maintenance_jobs(
        &database_manager,
        &storage_config,
        LocalMaintenanceStores {
            optimization: optimization_store,
            reuse: reuse_store,
        },
    )
    .map_err(|error| std::io::Error::other(error.to_string()))?;
    database_manager
        .start_maintenance(jobs)
        .map_err(|error| std::io::Error::other(error.to_string()))?;

    let server_result = server.run().await;
    let maintenance_result = database_manager
        .stop_maintenance()
        .map_err(|error| std::io::Error::other(error.to_string()));
    server_result.and(maintenance_result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mime_for_path_all_types() {
        assert_eq!(mime_for_path("index.html"), "text/html; charset=utf-8");
        assert_eq!(
            mime_for_path("app.js"),
            "application/javascript; charset=utf-8"
        );
        assert_eq!(mime_for_path("style.css"), "text/css");
        assert_eq!(mime_for_path("data.json"), "application/json");
        assert_eq!(mime_for_path("icon.svg"), "image/svg+xml");
        assert_eq!(mime_for_path("logo.png"), "image/png");
        assert_eq!(mime_for_path("favicon.ico"), "image/x-icon");
        assert_eq!(mime_for_path("font.woff2"), "font/woff2");
        assert_eq!(mime_for_path("file.xyz"), "application/octet-stream");
        assert_eq!(mime_for_path("noext"), "application/octet-stream");
    }

    #[test]
    fn test_local_trajectory_scan_dirs_returns_some() {
        let dirs = local_trajectory_scan_dirs();
        assert!(dirs.is_some());
        let dirs = dirs.unwrap();
        assert_eq!(dirs.len(), 6);
    }

    #[test]
    fn local_maintenance_schedules_only_writable_owned_databases() {
        let base = std::env::temp_dir().join(format!(
            "agentsight-local-maintenance-{}",
            std::process::id()
        ));
        let mut config = StorageConfig::default();
        config.base_path = base.clone();
        std::fs::create_dir_all(&base).unwrap();
        let manager =
            DatabaseManager::new(DatabaseRole::LocalServer, local_database_specs(&config)).unwrap();
        assert_eq!(
            manager.spec(DatabaseId::Trajectories).unwrap().access,
            DatabaseAccess::ReadOnly
        );
        assert_eq!(
            manager.spec(DatabaseId::Tokenless).unwrap().access,
            DatabaseAccess::External
        );
        let optimization = Arc::new(
            manager
                .open_read_write(DatabaseId::Optimization, OptimizationStore::new_with_path)
                .unwrap(),
        );
        let reuse = Arc::new(
            manager
                .open_read_write(DatabaseId::Reuse, |path| {
                    crate::reuse::ReuseStore::open_private(path.parent().unwrap())
                })
                .unwrap(),
        );

        let jobs = local_maintenance_jobs(
            &manager,
            &config,
            LocalMaintenanceStores {
                optimization: Some(Arc::clone(&optimization)),
                reuse: Some(Arc::clone(&reuse)),
            },
        )
        .unwrap();
        let ids = jobs.iter().map(|job| job.id()).collect::<Vec<_>>();
        assert_eq!(ids, ["optimization", "reuse"]);
        assert!(!ids.contains(&DatabaseId::Trajectories.as_str()));
        assert!(!ids.contains(&DatabaseId::Tokenless.as_str()));

        drop(jobs);
        drop(optimization);
        drop(reuse);
        drop(manager);
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn local_maintenance_skips_zero_intervals() {
        let base = std::env::temp_dir().join(format!(
            "agentsight-local-maintenance-zero-{}",
            std::process::id()
        ));
        let mut config = StorageConfig::default();
        config.base_path = base.clone();
        config.optimization.check_interval_secs = 0;
        config.reuse.check_interval_secs = 0;
        let manager =
            DatabaseManager::new(DatabaseRole::LocalServer, local_database_specs(&config)).unwrap();

        let jobs = local_maintenance_jobs(
            &manager,
            &config,
            LocalMaintenanceStores {
                optimization: None,
                reuse: None,
            },
        )
        .unwrap();

        assert!(jobs.is_empty());
        let _ = std::fs::remove_dir_all(base);
    }

    fn build_stub_app() -> App<
        impl actix_web::dev::ServiceFactory<
            actix_web::dev::ServiceRequest,
            Config = (),
            Response = actix_web::dev::ServiceResponse,
            Error = actix_web::Error,
            InitError = (),
        >,
    > {
        let db_path = std::env::temp_dir().join(format!(
            "agentsight-missing-trajectory-{}-{}.db",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
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
            .unwrap(),
        );
        let local_state = web::Data::new(LocalState {
            trajectory_store: Arc::new(RwLock::new(None)),
            db_path,
            storage_config: StorageConfig::default(),
            database_manager,
            reuse_store: None,
            reuse_llm_judge_enabled: false,
        });
        App::new()
            .app_data(local_state)
            .service(auth_status)
            .service(auth_verify)
            .service(list_sessions)
            .service(list_agent_names)
            .service(list_timeseries)
            .service(token_savings)
            .service(list_interruptions)
            .service(interruption_count)
            .service(interruption_stats)
            .service(interruption_session_counts)
            .service(interruption_conversation_counts)
            .service(security_status)
            .service(security_summary)
            .service(skill_metrics)
            .service(agent_health)
            .service(agent_process_health)
            .service(export_atif_unavailable)
            .service(api_fallback)
            .service(serve_frontend)
    }

    #[actix_web::test]
    async fn test_stub_endpoints_return_correct_shapes() {
        let app = actix_web::test::init_service(build_stub_app()).await;

        // Auth stubs
        let req = actix_web::test::TestRequest::get()
            .uri("/api/auth/status")
            .to_request();
        let resp = actix_web::test::call_service(&app, req).await;
        assert!(resp.status().is_success());

        let req = actix_web::test::TestRequest::get()
            .uri("/api/auth/verify")
            .to_request();
        let resp = actix_web::test::call_service(&app, req).await;
        assert!(resp.status().is_success());

        // Empty-array stubs
        for path in &[
            "/api/sessions",
            "/api/agent-names",
            "/api/timeseries",
            "/api/interruptions",
            "/api/interruptions/stats",
            "/api/interruptions/session-counts",
            "/api/interruptions/conversation-counts",
            "/api/skill-metrics",
        ] {
            let req = actix_web::test::TestRequest::get().uri(path).to_request();
            let resp = actix_web::test::call_service(&app, req).await;
            assert!(resp.status().is_success(), "failed: {path}");
        }

        // Empty-object stubs
        for path in &[
            "/api/interruptions/count",
            "/api/security/status",
            "/api/security/summary",
        ] {
            let req = actix_web::test::TestRequest::get().uri(path).to_request();
            let resp = actix_web::test::call_service(&app, req).await;
            assert!(resp.status().is_success(), "failed: {path}");
        }

        // Null stub
        let req = actix_web::test::TestRequest::get()
            .uri("/api/token-savings")
            .to_request();
        let resp = actix_web::test::call_service(&app, req).await;
        assert!(resp.status().is_success());

        let req = actix_web::test::TestRequest::get()
            .uri("/api/agent-health")
            .to_request();
        let resp = actix_web::test::call_service(&app, req).await;
        assert!(resp.status().is_success());
        let body: serde_json::Value = actix_web::test::read_body_json(resp).await;
        assert_eq!(body["agents"], serde_json::json!([]));

        let req = actix_web::test::TestRequest::get()
            .uri("/api/agent-process-health")
            .to_request();
        let resp = actix_web::test::call_service(&app, req).await;
        assert!(resp.status().is_success());

        // Export atif unavailable
        let req = actix_web::test::TestRequest::get()
            .uri("/api/export/atif/anything")
            .to_request();
        let resp = actix_web::test::call_service(&app, req).await;
        assert_eq!(resp.status(), actix_web::http::StatusCode::NOT_FOUND);

        // API fallback
        let req = actix_web::test::TestRequest::get()
            .uri("/api/unknown-endpoint")
            .to_request();
        let resp = actix_web::test::call_service(&app, req).await;
        assert!(resp.status().is_success());

        // Frontend SPA fallback
        let req = actix_web::test::TestRequest::get()
            .uri("/some/route")
            .to_request();
        let resp = actix_web::test::call_service(&app, req).await;
        // Either serves index.html or 404 if frontend not embedded
        assert!(resp.status().is_success() || resp.status().is_client_error());
    }
}
