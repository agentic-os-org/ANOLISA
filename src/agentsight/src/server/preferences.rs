//! User preference API: on-demand analysis over recent agent activity.
//!
//! No persistence — every request reads one analysis window from the
//! selected source (`genai_events.db` or `trajectories.db`), runs the rule
//! pipeline (`crate::preferences`) and optionally merges LLM findings from
//! `agentsight-opt`. All source-agnostic mechanics (source selection, TTL
//! cache, LLM merging, Markdown export) live in `crate::preferences::api`
//! and are shared with the macOS local server.

use std::collections::HashSet;

use actix_web::{HttpResponse, Responder, get, web};
use agentsight_opt::preference::{LlmPreference, analyze_user_turns};

use super::AppState;
use crate::preferences::api::{
    AutoResolution, DEFAULT_TURNS_LIMIT, MAX_TURNS_LIMIT, PreferenceSourceParam, PreferencesQuery,
    TurnsQuery, cache_get, cache_put, clamp_window_days, merge_llm_preferences, render_markdown,
    resolve_auto, window_start_ns,
};
use crate::preferences::{aggregator, analyze_rows, detector, genai_source, trajectory_source};
use crate::storage::sqlite::GenAISqliteStore;

// ─── Source loading ──────────────────────────────────────────────────────────

/// Fetch one genai window; the error string doubles as the "unavailable"
/// reason for both the explicit-source error response and the auto-fallback
/// debug log.
fn load_genai_rows(
    data: &AppState,
    since_ns: i64,
) -> Result<Vec<detector::PreferenceEventRow>, String> {
    if !data.storage_path.exists() {
        return Err(
            "SQLite storage is not enabled or no events have been captured yet.".to_string(),
        );
    }
    let store = GenAISqliteStore::new_with_path(
        &data.storage_path,
        crate::config::InsertStoragePolicy::default(),
    )
    .map_err(|e| e.to_string())?;
    let raw = store
        .get_preference_window_events(since_ns)
        .map_err(|e| e.to_string())?;
    // Interpret raw columns here rather than in the store: the mapping strips
    // agent template noise and mines tool names, which is analysis, not storage.
    Ok(raw
        .iter()
        .map(|row| {
            genai_source::row_from_event(
                row.id,
                row.session_id.clone(),
                row.conversation_id.clone(),
                row.start_timestamp_ns,
                row.user_query.as_deref(),
                row.output_messages.as_deref(),
            )
        })
        .collect())
}

/// Fetch one trajectory window from `trajectories.db` (lazily opened by the
/// shared app state, same as the trajectory browsing endpoints).
fn load_trajectory_rows(
    data: &AppState,
    since_ns: i64,
) -> Result<Vec<detector::PreferenceEventRow>, String> {
    let store = data.trajectory_store().ok_or_else(|| {
        "trajectories.db not found; run `agentsight trace` with trajectory collection enabled."
            .to_string()
    })?;
    trajectory_source::load_window_rows(&store, since_ns).map_err(|e| e.to_string())
}

fn source_unavailable(source: PreferenceSourceParam, reason: &str) -> HttpResponse {
    HttpResponse::ServiceUnavailable().json(serde_json::json!({
        "error": format!("{} source unavailable", source.as_str()),
        "message": reason,
    }))
}

/// Resolve the requested source and load its window. Returns the rows
/// together with the source that actually served them (never `Auto`), so
/// handlers can report real provenance in the response body.
fn load_rows(
    data: &AppState,
    source: PreferenceSourceParam,
    window_days: u32,
) -> Result<(Vec<detector::PreferenceEventRow>, PreferenceSourceParam), HttpResponse> {
    let since_ns = window_start_ns(window_days);
    match source {
        PreferenceSourceParam::Genai => load_genai_rows(data, since_ns)
            .map(|rows| (rows, PreferenceSourceParam::Genai))
            .map_err(|reason| source_unavailable(source, &reason)),
        PreferenceSourceParam::Trajectory => load_trajectory_rows(data, since_ns)
            .map(|rows| (rows, PreferenceSourceParam::Trajectory))
            .map_err(|reason| source_unavailable(source, &reason)),
        PreferenceSourceParam::Auto => {
            let genai_rows = match load_genai_rows(data, since_ns) {
                Ok(rows) => Some(rows),
                Err(reason) => {
                    log::debug!("Preference API: auto source skips genai: {reason}");
                    None
                }
            };
            // Trajectories are only probed when genai cannot serve the
            // window — parsing stored ATIF JSON is the expensive path.
            let trajectory_rows = match &genai_rows {
                Some(rows) if !rows.is_empty() => None,
                _ => match load_trajectory_rows(data, since_ns) {
                    Ok(rows) => Some(rows),
                    Err(reason) => {
                        log::debug!("Preference API: auto source skips trajectory: {reason}");
                        None
                    }
                },
            };
            match resolve_auto(genai_rows.as_ref().map(Vec::len), trajectory_rows.is_some()) {
                AutoResolution::Genai => {
                    Ok((genai_rows.unwrap_or_default(), PreferenceSourceParam::Genai))
                }
                AutoResolution::Trajectory => Ok((
                    trajectory_rows.unwrap_or_default(),
                    PreferenceSourceParam::Trajectory,
                )),
                AutoResolution::Unavailable => {
                    Err(HttpResponse::ServiceUnavailable().json(serde_json::json!({
                        "error": "no preference data source available",
                        "message": "Neither genai_events.db nor trajectories.db can be read; \
                                    capture some agent activity first.",
                    })))
                }
            }
        }
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
/// Rule-based preference analysis over the recent activity window,
/// optionally augmented with LLM findings when `llm=true` and the
/// optimization LLM is configured. LLM problems never fail the request —
/// the response degrades to rule-only results with `llm_enabled: false`.
/// `source` picks the data source (`auto` falls back from genai events to
/// collected trajectories); the response's `source` field reports which
/// one actually served the request.
#[get("/preferences")]
pub async fn get_preferences(
    data: web::Data<AppState>,
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
    data: &AppState,
    rows: &[detector::PreferenceEventRow],
) -> Result<Vec<LlmPreference>, String> {
    let state = data
        .optimize
        .as_ref()
        .ok_or_else(|| "optimization feature unavailable".to_string())?;
    let client = state
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
#[get("/preferences/export")]
pub async fn export_preferences(
    data: web::Data<AppState>,
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
#[get("/preferences/turns")]
pub async fn get_preference_turns(
    data: web::Data<AppState>,
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
