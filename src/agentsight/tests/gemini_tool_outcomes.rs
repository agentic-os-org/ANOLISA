use agentsight_opt::{atif::AtifTrajectory, cost::compute_cost, trace::collect_tool_calls};
use agentsight_trajectory_collector::gemini::convert_gemini_events;
use serde_json::{Value, json};

fn converted(status: Option<&str>, output: &str) -> AtifTrajectory {
    let mut call = json!({"id":"call-fixture", "name":"read_file", "args":{"path":"errors.log"},
        "result":[{"functionResponse":{"name":"read_file","response":{"output":output}}}]});
    if let Some(status) = status {
        call["status"] = json!(status);
    }
    let trajectory = convert_gemini_events(
        &[
            json!({"sessionId":"fixture", "projectHash":"project"}),
            json!({"id":"u", "type":"user", "content":"inspect the error log"}),
            json!({"id":"a", "type":"gemini", "content":"", "toolCalls":[call]}),
        ],
        "gemini-cli",
    )
    .unwrap();
    AtifTrajectory::from_json(&trajectory.to_json_value().unwrap().to_string()).unwrap()
}

#[test]
fn successful_log_reads_keep_their_declared_outcome() {
    for text in [
        "Error: recorded in yesterday's log",
        "ERROR expected by the test",
        "Permission denied example",
    ] {
        let trajectory = converted(Some("success"), text);
        let extra = trajectory.steps[1].results()[0]
            .extra
            .as_ref()
            .unwrap_or(&Value::Null);
        assert_eq!(
            extra["is_error"], false,
            "the producer's success flag must survive"
        );
        assert!(!collect_tool_calls(&trajectory)[0].err);
        assert!(!compute_cost(&trajectory).unwrap().calls[0].removable_turn);
    }
}

#[test]
fn declared_errors_do_not_depend_on_output_wording() {
    let trajectory = converted(Some("error"), "The requested resource is unavailable");
    assert!(collect_tool_calls(&trajectory)[0].err);
    assert!(compute_cost(&trajectory).unwrap().calls[0].removable_turn);
}

#[test]
fn missing_or_unfinished_status_keeps_legacy_text_detection() {
    for status in [
        None,
        Some("scheduled"),
        Some("executing"),
        Some("future-status"),
    ] {
        let trajectory = converted(status, "Error: unavailable");
        assert!(trajectory.steps[1].results()[0].extra.is_none());
        assert!(collect_tool_calls(&trajectory)[0].err);
    }
}
