//! `trajectories.db` provider: flattens stored ATIF trajectories into the
//! unified [`PreferenceEventRow`] shape consumed by the rule engine.
//!
//! This is the only preference data source available on macOS, where the
//! eBPF genai pipeline does not exist and turns are collected by the
//! trajectory collector instead.

use agentsight_atif::{AtifTrajectory, StepSource};
use agentsight_trajectory_collector::{TrajectoryStore, strip_system_context};

use super::detector::PreferenceEventRow;

/// Hard cap on trajectories loaded per analysis window. Unlike the genai
/// cap (`PREFERENCE_WINDOW_MAX_ROWS`), which counts individual event rows,
/// this counts whole ATIF documents: one document can reach several MB of
/// JSON and flatten into many turns, so the budget is deliberately far
/// smaller.
pub const PREFERENCE_TRAJECTORY_MAX_ROWS: usize = 50;

/// Load one analysis window from `trajectories.db`: the most recent
/// main-agent trajectories collected at or after `since_ns`, flattened to
/// unified rows. Trajectories whose ATIF JSON fails to parse are skipped
/// with a debug log — a single corrupted row must not fail the request.
///
/// Rows are returned newest-first: the store yields documents newest-first
/// and [`rows_from_atif_json`] emits each document's turns newest-first, so
/// the concatenation is globally ordered — the order the shared turn
/// selection contract in [`crate::preferences::api`] requires.
///
/// # Errors
/// Returns an error when the store query itself fails.
pub fn load_window_rows(
    store: &TrajectoryStore,
    since_ns: i64,
) -> anyhow::Result<Vec<PreferenceEventRow>> {
    let documents =
        store.list_recent_atif_jsons(since_ns, PREFERENCE_TRAJECTORY_MAX_ROWS as i64)?;
    let mut rows = Vec::new();
    // Ids only serve as signal provenance; a per-window sequence keeps them
    // unique across trajectories without inventing fake storage ids.
    let mut next_id: i64 = 1;
    for (session_id, atif_json) in &documents {
        rows.extend(rows_from_atif_json(session_id, atif_json, &mut next_id));
    }
    Ok(rows)
}

/// Flatten one ATIF document into unified rows.
///
/// Each user step opens a turn; tool-call names from the agent steps that
/// follow attach to that turn, mirroring the genai shape where one row
/// carries both the user query and the tools the response invoked. Rows are
/// emitted newest-first — the shared selection contract in
/// [`crate::preferences::api`] — while tool attachment itself walks the
/// steps in their stored order. Turns stay adjacent in time after the
/// reversal, so the rapid-followup rule works exactly as on genai rows when
/// steps carry parseable timestamps — and is skipped for turns that do not.
/// Returns an empty vec on malformed JSON.
pub fn rows_from_atif_json(
    session_id: &str,
    atif_json: &str,
    next_id: &mut i64,
) -> Vec<PreferenceEventRow> {
    let trajectory: AtifTrajectory = match serde_json::from_str(atif_json) {
        Ok(t) => t,
        Err(e) => {
            log::debug!(
                "Preference trajectory source: unparseable ATIF JSON (session={session_id}): {e}"
            );
            return Vec::new();
        }
    };
    let mut rows: Vec<PreferenceEventRow> = Vec::new();
    for step in &trajectory.steps {
        match step.source {
            StepSource::User => {
                let cleaned = strip_system_context(&step.message);
                let id = *next_id;
                *next_id += 1;
                rows.push(PreferenceEventRow {
                    id,
                    session_id: Some(session_id.to_string()),
                    // One trajectory is one conversation, so the session id
                    // doubles as the cross-turn grouping key.
                    conversation_id: Some(session_id.to_string()),
                    timestamp_ns: step.timestamp.as_deref().and_then(parse_step_timestamp_ns),
                    user_text: (!cleaned.is_empty()).then_some(cleaned),
                    tool_names: Vec::new(),
                });
            }
            StepSource::Agent => {
                // Tool calls belong to the preceding user turn; agent steps
                // before any user turn have no turn to attach to (a shape
                // real trajectories do not produce) and are dropped.
                if let (Some(turn), Some(calls)) = (rows.last_mut(), step.tool_calls.as_ref()) {
                    turn.tool_names.extend(
                        calls
                            .iter()
                            .map(|call| call.function_name.clone())
                            .filter(|name| !name.is_empty()),
                    );
                }
            }
            StepSource::System => {}
        }
    }
    // ATIF steps arrive oldest-first; the consumers are documented to require
    // newest-first input, and an unreversed document made the window
    // concatenation sorted only at document granularity — the turns endpoint
    // returned the OLDEST turns and the LLM prompt ran in reverse order.
    // Reversing after the loop keeps tool attachment untouched.
    rows.reverse();
    rows
}

/// RFC 3339 step timestamp → epoch nanoseconds. `None` for missing or
/// unparseable values so time-gap rules skip the turn instead of running on
/// a fabricated timestamp.
fn parse_step_timestamp_ns(raw: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(raw)
        .ok()?
        .timestamp_nanos_opt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flatten(atif_json: &str) -> Vec<PreferenceEventRow> {
        let mut next_id = 1;
        rows_from_atif_json("traj-1", atif_json, &mut next_id)
    }

    #[test]
    fn extracts_user_turns_with_tool_names_and_timestamps() {
        let atif = r#"{
            "agent": {"name": "qoder"},
            "steps": [
                {"step_id": 1, "source": "user", "message": "先出方案再写代码",
                 "timestamp": "2026-07-25T10:00:00Z"},
                {"step_id": 2, "source": "agent", "message": "ok",
                 "tool_calls": [
                    {"tool_call_id": "t1", "function_name": "bash", "arguments": {}},
                    {"tool_call_id": "t2", "function_name": "", "arguments": {}}
                 ]},
                {"step_id": 3, "source": "agent", "message": "more",
                 "tool_calls": [
                    {"tool_call_id": "t3", "function_name": "grep", "arguments": {}}
                 ]},
                {"step_id": 4, "source": "user", "message": "改完之后跑测试",
                 "timestamp": "2026-07-25T10:00:30Z"}
            ]
        }"#;
        let rows = flatten(atif);
        assert_eq!(rows.len(), 2);

        // Rows are emitted newest-first (the shared selection contract).
        let newest = &rows[0];
        assert_eq!(newest.id, 2);
        assert_eq!(newest.user_text.as_deref(), Some("改完之后跑测试"));
        assert!(newest.tool_names.is_empty());
        // 30 seconds after the first turn — inside the correction window.
        assert_eq!(newest.timestamp_ns, Some(1_784_973_630_000_000_000));

        let oldest = &rows[1];
        assert_eq!(oldest.id, 1);
        assert_eq!(oldest.session_id.as_deref(), Some("traj-1"));
        assert_eq!(oldest.conversation_id.as_deref(), Some("traj-1"));
        assert_eq!(oldest.user_text.as_deref(), Some("先出方案再写代码"));
        // Both agent steps of the turn contribute; empty names are dropped.
        // Tool attachment runs in steps order, before the newest-first flip.
        assert_eq!(oldest.tool_names, vec!["bash", "grep"]);
        assert_eq!(oldest.timestamp_ns, Some(1_784_973_600_000_000_000));
    }

    #[test]
    fn strips_system_context_and_skips_unreliable_timestamps() {
        let atif = r#"{
            "agent": {"name": "qoder"},
            "steps": [
                {"step_id": 1, "source": "user",
                 "message": "<system-reminder>memo</system-reminder>跑测试"},
                {"step_id": 2, "source": "user",
                 "message": "<command-message>clear</command-message>",
                 "timestamp": "not-a-timestamp"}
            ]
        }"#;
        let rows = flatten(atif);
        assert_eq!(rows.len(), 2);
        // Newest-first: the tag-only turn (step 2) heads the window.
        // Only injected tags left → no usable user text.
        assert!(rows[0].user_text.is_none());
        // Unparseable timestamp → None as well.
        assert_eq!(rows[0].timestamp_ns, None);
        assert_eq!(rows[1].user_text.as_deref(), Some("跑测试"));
        // No timestamp on the step → no fabricated value.
        assert_eq!(rows[1].timestamp_ns, None);
    }

    #[test]
    fn malformed_json_and_empty_steps_yield_no_rows() {
        assert!(flatten("not json at all").is_empty());
        assert!(flatten(r#"{"agent": {"name": "qoder"}, "steps": []}"#).is_empty());
        // `steps` missing entirely: the ATIF schema defaults it to empty.
        assert!(flatten(r#"{"agent": {"name": "qoder"}}"#).is_empty());
    }

    #[test]
    fn ids_stay_unique_across_trajectories() {
        let atif = r#"{
            "agent": {"name": "qoder"},
            "steps": [{"step_id": 1, "source": "user", "message": "hello there my friend"}]
        }"#;
        let mut next_id = 1;
        let a = rows_from_atif_json("t-1", atif, &mut next_id);
        let b = rows_from_atif_json("t-2", atif, &mut next_id);
        assert_eq!(a[0].id, 1);
        assert_eq!(b[0].id, 2);
    }

    #[test]
    fn load_window_rows_emits_turns_newest_first() {
        use agentsight_trajectory_collector::TrajectoryRecord;

        // The shared selection contract (preferences::api) requires
        // newest-first input. ATIF steps are oldest-first, so without a
        // per-document reversal the concatenation over DESC documents is
        // only sorted at document granularity: GET /turns?limit=1 answered
        // the oldest turn of the newest document and the LLM prompt was
        // fed in the wrong order.
        let dir = std::env::temp_dir().join(format!(
            "agentsight-traj-source-order-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let store = TrajectoryStore::new_with_path(&dir.join("trajectories.db"))
            .expect("open trajectory store");

        let atif = r#"{
            "agent": {"name": "qoder"},
            "steps": [
                {"step_id": 1, "source": "user", "message": "older turn",
                 "timestamp": "2026-07-25T10:00:00Z"},
                {"step_id": 2, "source": "user", "message": "newer turn",
                 "timestamp": "2026-07-25T10:00:30Z"}
            ]
        }"#;
        store
            .upsert_trajectory(&TrajectoryRecord {
                session_id: "traj-order".to_string(),
                schema_version: "ATIF-v1.7".to_string(),
                agent_name: "qoder".to_string(),
                model_name: None,
                num_steps: 2,
                total_prompt_tokens: None,
                total_completion_tokens: None,
                start_time: None,
                end_time: None,
                first_user_message: None,
                last_user_message: None,
                atif_json: atif.to_string(),
                project: String::new(),
                source: "qoder".to_string(),
                is_subagent: false,
                file_path: String::new(),
                file_size: 0,
                file_mtime_ns: 0,
            })
            .expect("upsert trajectory");

        let rows = load_window_rows(&store, 0).expect("load window");
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0].user_text.as_deref(),
            Some("newer turn"),
            "the newest turn must come first: {rows:?}"
        );
        assert_eq!(rows[1].user_text.as_deref(), Some("older turn"));

        // Both consumers documented to require newest-first input now answer
        // the newest turn for a limit of one.
        let turns = crate::preferences::api::select_unique_turns(rows.iter(), Some(1));
        assert_eq!(turns, vec!["newer turn"]);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
