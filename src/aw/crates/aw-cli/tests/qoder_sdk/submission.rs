//! The SDK callback waits on the real service before a synthetic model dispatch.

use super::*;

#[test]
fn submit_policy_runs_before_model_without_tool_identity() {
    for (scenario, on_error, blocked) in [
        ("observe", "block", false),
        ("block", "report", true),
        ("error", "block", true),
        ("error", "report", false),
        ("timeout", "block", true),
    ] {
        let f = Fixture::new();
        let mut document = setup(&f);
        document["spec"]["events"]["input.submit"] =
            document["spec"]["events"]["tool.before"].clone();
        document["spec"]["events"]["input.submit"]["steps"][0]["on_error"] = json!(on_error);
        if scenario == "timeout" {
            document["spec"]["events"]["input.submit"]["budget_ms"] = json!(200);
        }
        let mut second = document["spec"]["events"]["input.submit"]["steps"][0].clone();
        second["id"] = json!("second-submit-check");
        document["spec"]["events"]["input.submit"]["steps"]
            .as_array_mut()
            .unwrap()
            .push(second);
        let service = Service::start(&f, &document);
        let mut command = launch(&f);
        command.env("QODER_SUBMIT", "1");
        command.env("QODER_SCENARIO", scenario);
        let run = f.run(command);
        assert!(
            run.status.success(),
            "{}",
            String::from_utf8_lossy(&run.stderr)
        );
        let output: Value = serde_json::from_slice(&run.stdout).unwrap();
        assert_eq!(
            output["denied"],
            blocked,
            "{}",
            String::from_utf8_lossy(&run.stderr)
        );
        assert_eq!(f.root.join("model-request").exists(), !blocked);
        assert_eq!(f.root.join("executed").exists(), !blocked);
        let submissions: Vec<Value> = fs::read_dir(&f.root)
            .unwrap()
            .map(Result::unwrap)
            .filter(|e| e.file_name().to_string_lossy().starts_with("provider-"))
            .map(|e| serde_json::from_slice::<Value>(&fs::read(e.path()).unwrap()).unwrap())
            .filter(|r| r["method"] == "invoke" && r["event"]["name"] == "input.submit")
            .collect();
        assert_eq!(submissions.len(), if blocked { 1 } else { 2 });
        if !blocked {
            assert_eq!(submissions[0]["event"], submissions[1]["event"]);
        }
        let event = &submissions[0]["event"];
        assert_eq!(event["input"]["text"], "fresh user input");
        assert_eq!(event["session_id"], "session-1");
        assert_eq!(event["turn_id"].as_str().unwrap().len(), 36);
        assert!(event.get("tool").is_none());
        service.released(&f);
    }
}
