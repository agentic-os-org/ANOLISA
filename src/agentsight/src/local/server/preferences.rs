//! macOS preference API over collected trajectories.
//!
//! Thin platform adapter: all shared mechanics (source selection, TTL
//! cache, LLM merging, Markdown export) live in `crate::preferences::api`
//! and match the Linux server. The eBPF genai pipeline does not exist on
//! macOS, so `source=genai` fails loudly and `source=auto` degrades
//! straight to `trajectories.db`.

use std::collections::HashSet;

use actix_web::{HttpResponse, Responder, get, web};
use agentsight_opt::preference::{LlmPreference, analyze_user_turns};
use agentsight_trajectory_collector::TrajectoryStore;

use super::optimize::OptimizeAppState;
use crate::preferences::api::{
    AutoResolution, DEFAULT_TURNS_LIMIT, MAX_TURNS_LIMIT, PreferenceSourceParam, PreferencesQuery,
    TurnsQuery, cache_get, cache_put, clamp_window_days, merge_llm_preferences, render_markdown,
    resolve_auto, window_start_ns,
};
use crate::preferences::{aggregator, analyze_rows, detector, trajectory_source};

// ─── Source loading ──────────────────────────────────────────────────────────

fn trajectory_unavailable() -> HttpResponse {
    HttpResponse::ServiceUnavailable().json(serde_json::json!({
        "error": "trajectory source unavailable",
        "message": "trajectories.db not found; run `agentsight trace` to collect trajectories first.",
    }))
}

fn load_trajectory_rows(
    store: &TrajectoryStore,
    since_ns: i64,
) -> Result<(Vec<detector::PreferenceEventRow>, PreferenceSourceParam), HttpResponse> {
    trajectory_source::load_window_rows(store, since_ns)
        .map(|rows| (rows, PreferenceSourceParam::Trajectory))
        .map_err(|e| {
            HttpResponse::InternalServerError().json(serde_json::json!({"error": e.to_string()}))
        })
}

/// Resolve the requested source and load its window. Returns the rows
/// together with the source that actually served them (never `Auto`), so
/// handlers can report real provenance in the response body.
fn load_rows(
    data: &OptimizeAppState,
    source: PreferenceSourceParam,
    window_days: u32,
) -> Result<(Vec<detector::PreferenceEventRow>, PreferenceSourceParam), HttpResponse> {
    let since_ns = window_start_ns(window_days);
    let store = data.local_state.trajectory_store();
    match source {
        PreferenceSourceParam::Genai => {
            Err(HttpResponse::ServiceUnavailable().json(serde_json::json!({
                "error": "genai source unavailable",
                "message": "The eBPF genai pipeline does not exist on macOS; \
                            use source=trajectory (or the default auto).",
            })))
        }
        PreferenceSourceParam::Trajectory => match store {
            Some(store) => load_trajectory_rows(&store, since_ns),
            None => Err(trajectory_unavailable()),
        },
        // No genai store exists to probe on macOS, so auto resolution is
        // decided by trajectory availability alone.
        PreferenceSourceParam::Auto => match resolve_auto(None, store.is_some()) {
            AutoResolution::Trajectory => match store {
                Some(store) => load_trajectory_rows(&store, since_ns),
                None => Err(trajectory_unavailable()),
            },
            _ => Err(HttpResponse::ServiceUnavailable().json(serde_json::json!({
                "error": "no preference data source available",
                "message": "trajectories.db not found; run `agentsight trace` to collect trajectories first.",
            }))),
        },
    }
}

/// Parse `?source=`, mapping bad values to a 400 with the parser's message.
fn parse_source(source: Option<&str>) -> Result<PreferenceSourceParam, HttpResponse> {
    PreferenceSourceParam::parse(source).map_err(|message| {
        HttpResponse::BadRequest().json(serde_json::json!({
            "error": "invalid source",
            "message": message,
        }))
    })
}

// ─── Handlers ────────────────────────────────────────────────────────────────

/// GET /api/preferences?window_days=7&llm=true&source=auto
///
/// Rule-based preference analysis over the recent trajectory window,
/// optionally augmented with LLM findings when `llm=true` and the local
/// optimization LLM is configured. LLM problems never fail the request —
/// the response degrades to rule-only results with `llm_enabled: false`.
#[get("/api/preferences")]
pub async fn get_preferences(
    data: web::Data<OptimizeAppState>,
    query: web::Query<PreferencesQuery>,
) -> impl Responder {
    let source = match parse_source(query.source.as_deref()) {
        Ok(source) => source,
        Err(resp) => return resp,
    };
    let window_days = clamp_window_days(query.window_days);
    let llm_requested = query.llm.unwrap_or(false);
    let cache_key = (window_days, llm_requested, source);

    if let Some(body) = cache_get(cache_key) {
        return HttpResponse::Ok().json(body);
    }

    let (rows, resolved) = match load_rows(&data, source, window_days) {
        Ok(loaded) => loaded,
        Err(resp) => return resp,
    };
    let mut prefs = analyze_rows(&rows);

    let mut llm_enabled = false;
    if llm_requested {
        match llm_findings(&data, &rows).await {
            Ok(findings) => {
                llm_enabled = true;
                merge_llm_preferences(&mut prefs, findings);
                // Re-run conflict marking so exclusivity (e.g. "language")
                // holds across rule and LLM entries together.
                aggregator::mark_conflicts(&mut prefs);
            }
            Err(reason) => log::warn!("Preference API: LLM layer skipped: {reason}"),
        }
    }

    let body = serde_json::json!({
        "window_days": window_days,
        "analyzed_events": rows.len(),
        "llm_enabled": llm_enabled,
        "source": resolved.as_str(),
        "preferences": prefs,
    });
    cache_put(cache_key, body.clone());
    HttpResponse::Ok().json(body)
}

/// Run the LLM layer over the window's user turns; every failure mode is
/// reduced to one human-readable reason for the degradation log.
async fn llm_findings(
    data: &OptimizeAppState,
    rows: &[detector::PreferenceEventRow],
) -> Result<Vec<LlmPreference>, String> {
    let client = data
        .optimize
        .build_client()
        .map_err(|_| "LLM not configured".to_string())?;
    let turns: Vec<String> = rows.iter().filter_map(|r| r.user_text.clone()).collect();
    analyze_user_turns(&client, &turns)
        .await
        .map_err(|e| e.to_string())
}

/// GET /api/preferences/export?window_days=7&source=auto
///
/// Markdown digest of the confident, active rule-based preferences —
/// suitable for pasting into an agent memory/config file. Accepts the same
/// `source` parameter as the JSON endpoint.
#[get("/api/preferences/export")]
pub async fn export_preferences(
    data: web::Data<OptimizeAppState>,
    query: web::Query<PreferencesQuery>,
) -> impl Responder {
    let source = match parse_source(query.source.as_deref()) {
        Ok(source) => source,
        Err(resp) => return resp,
    };
    let window_days = clamp_window_days(query.window_days);
    let (rows, _resolved) = match load_rows(&data, source, window_days) {
        Ok(loaded) => loaded,
        Err(resp) => return resp,
    };
    let prefs = analyze_rows(&rows);
    HttpResponse::Ok()
        .content_type("text/markdown; charset=utf-8")
        .body(render_markdown(&prefs))
}

/// GET /api/preferences/turns?window_days=7&source=auto&limit=200
///
/// Returns the deduped raw user turns inside the window — the same text the
/// `llm=true` path feeds to the server-side LLM, but handed back verbatim so
/// an agent (which already has its own model) can run the enhancement itself
/// without agentsight holding any LLM credentials. Newest unique turns first;
/// `limit` bounds the count (default 200, max 1000).
#[get("/api/preferences/turns")]
pub async fn get_preference_turns(
    data: web::Data<OptimizeAppState>,
    query: web::Query<TurnsQuery>,
) -> impl Responder {
    let source = match parse_source(query.source.as_deref()) {
        Ok(source) => source,
        Err(resp) => return resp,
    };
    let window_days = clamp_window_days(query.window_days);
    let (rows, resolved) = match load_rows(&data, source, window_days) {
        Ok(loaded) => loaded,
        Err(resp) => return resp,
    };
    let limit = query
        .limit
        .unwrap_or(DEFAULT_TURNS_LIMIT)
        .clamp(1, MAX_TURNS_LIMIT);
    let mut seen: HashSet<String> = HashSet::new();
    let turns: Vec<String> = rows
        .iter()
        .filter_map(|r| r.user_text.clone())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .filter(|s| seen.insert(s.clone()))
        .take(limit)
        .collect();
    HttpResponse::Ok().json(serde_json::json!({
        "window_days": window_days,
        "source": resolved.as_str(),
        "turns_count": turns.len(),
        "turns": turns,
    }))
}
