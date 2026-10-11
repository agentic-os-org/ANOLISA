//! The structured tool-failure flag (`observation.results[].extra.is_error`,
//! `EXTRA_IS_ERROR` in the shared `agentsight-atif` schema, written by both
//! in-repo producers) must reach `PerfStats.tool_calls[].err`. Documents
//! without the flag keep the text-heuristic fallback.

use agentsight_opt::atif::AtifTrajectory;
use agentsight_opt::perf::compute_stats;

fn traj_with_observation(content: &str, extra: Option<&str>) -> String {
    let extra = match extra {
        Some(e) => format!(r#","extra": {e}"#),
        None => String::new(),
    };
    format!(
        r#"{{
      "schema_version": "ATIF-v1.7",
      "session_id": "s1",
      "agent": {{"name": "a", "version": "1"}},
      "steps": [
        {{"step_id": 1, "source": "agent", "timestamp": "2026-01-01T00:00:00Z",
         "tool_calls": [{{"tool_call_id": "c1", "function_name": "Bash",
                         "arguments": {{"command": "make test"}}}}],
         "observation": {{"results": [{{
            "source_call_id": "c1",
            "content": "{content}"{extra}
         }}]}}}},
        {{"step_id": 2, "source": "agent", "timestamp": "2026-01-01T00:00:30Z",
         "message": "done"}}
      ]
    }}"#
    )
}

fn err_of(json: &str) -> bool {
    let traj = AtifTrajectory::from_json(json).unwrap();
    let stats = compute_stats(&traj).unwrap();
    assert_eq!(stats.tool_calls.len(), 1);
    stats.tool_calls[0].err
}

#[test]
fn provider_flagged_failure_is_reported_as_err() {
    // Clean-text failure: no heuristic marker matches, only the flag knows.
    let json = traj_with_observation(
        "make: *** Tool execution aborted, exit status 1",
        Some(r#"{"is_error": true}"#),
    );
    assert!(
        err_of(&json),
        "producer set observation extra.is_error=true but analyze reports err=false"
    );
}

#[test]
fn explicit_success_flag_beats_error_text() {
    // Successful call whose output merely mentions "Error:" — the exact
    // misfire the shared schema warns about for text re-derivation.
    let json = traj_with_observation(
        "Error: connection reset (recovered, retry succeeded)",
        Some(r#"{"is_error": false}"#),
    );
    assert!(
        !err_of(&json),
        "producer set observation extra.is_error=false but text heuristic overrode it"
    );
}

#[test]
fn flagless_failure_keeps_text_heuristic() {
    let json = traj_with_observation("Error: request timed out", None);
    assert!(
        err_of(&json),
        "flag-less failure text must still be flagged"
    );
}
