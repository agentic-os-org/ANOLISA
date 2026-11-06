//! Ordinary resident policy changes share one root transaction and fresh Query snapshots.

use super::*;
use support::{Process, TIMEOUT};

#[test]
fn first_query_policy_update_preserves_launch_receipt() {
    for (initial_hooks, updated_hooks) in [(false, Some(true)), (true, Some(false)), (true, None)] {
        let f = Fixture::new();
        let mut initial = setup(&f);
        initial["spec"]["events"] = json!({});
        initial["spec"]["providers"] = json!({});
        let empty = initial.clone();
        if initial_hooks {
            f.native(&mut initial, "scan", "block");
        }
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/qoder_sdk");
        fs::copy(
            source.join("first-query-reload.mjs"),
            f.root.join("app.mjs"),
        )
        .unwrap();
        let service = Service::start(&f, &initial);
        let mut process = Process::spawn(&f, launch(&f));
        process
            .wait_until("first Query gate and worker association", TIMEOUT, |_| {
                let state = service.status();
                let marker = f.root.join("first-query-gated.json");
                if !marker.exists() {
                    return Ok((false, state.to_string()));
                }
                let worker: Value = serde_json::from_slice(&fs::read(marker).unwrap()).unwrap();
                let associated = state["instances"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|instance| {
                        instance["instance_id"] == worker["instance_id"]
                            && instance["process"]["pid"] == worker["pid"]
                            && instance["ready"] == true
                    });
                Ok((associated, state.to_string()))
            })
            .unwrap();
        let status = service.status();
        assert_eq!(status["instances"][0]["ready"], true);
        assert_eq!(status["instances"].as_array().unwrap().len(), 1);
        assert_eq!(status["events"], 0);
        assert_eq!(service.status()["instances"][0]["requests"], 0);
        assert!(!f.root.join("executed").exists());
        if let Some(hooks) = updated_hooks {
            let mut candidate = empty.clone();
            if hooks {
                f.native(&mut candidate, "scan", "block");
            }
            fs::write(f.root.join("candidate.json"), candidate.to_string()).unwrap();
            let mut update = f.command();
            update
                .args(["update", "--socket"])
                .arg(f.root.join("state/aw.sock"))
                .args(["--agent", "sdk", "--policy"])
                .arg(f.root.join("candidate.json"));
            let output = f.run(update);
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        fs::write(f.root.join("release-first-query"), "go").unwrap();
        let output = process.finish();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        let expected_denial = updated_hooks.unwrap_or(initial_hooks);
        assert_eq!(result["denied"], expected_denial);
        assert_eq!(result["receipt_hooks"], 2);
        assert_eq!(f.root.join("executed").exists(), !expected_denial);
        assert!(f.root.join("query-closed").exists());
        assert!(f.root.join("daemon-stopped").exists());
        service.released(&f);
    }
}

#[test]
fn ordinary_queries_update_hooks_post_and_mcp_after_cleanup() {
    let f = Fixture::new();
    let mut initial = setup(&f);
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/qoder_sdk");
    fs::copy(source.join("resident-reload.mjs"), f.root.join("app.mjs")).unwrap();
    let sdk = f.root.join("node_modules/@qoder-ai/qoder-agent-sdk/dist");
    fs::copy(source.join("sdk.mjs"), sdk.join("fixture-sdk.mjs")).unwrap();
    fs::copy(source.join("resident-reload-sdk.mjs"), sdk.join("index.js")).unwrap();
    initial["spec"]["events"] = json!({});
    initial["spec"]["providers"] = json!({});
    let save = |name: &str, value: &Value| {
        fs::write(f.root.join(name), serde_json::to_vec(value).unwrap()).unwrap();
    };
    save("empty.json", &initial);
    let mut deny = initial.clone();
    f.native(&mut deny, "new-scan", "block");
    deny["spec"]["events"]["tool.before"]["budget_ms"] = json!(1200);
    save("deny.json", &deny);
    let mut strict = initial.clone();
    f.provider(&mut strict);
    strict["spec"]["providers"]["policy"]["transport"]["argv"] =
        json!([f.python, source.join("post-policy.py")]);
    strict["spec"]["events"]["tool.before"]["steps"][0]["effects"] = json!(["observe"]);
    strict["spec"]["events"]["tool.after"]["steps"][0]["operation"] = json!("check");
    strict["spec"]["events"]["tool.after"]["steps"][0]["on_error"] = json!("withhold_result");
    save("strict.json", &strict);
    strict["spec"]["events"]["tool.after"]["steps"][0]["on_error"] = json!("report");
    save("report.json", &strict);
    let mut mcp = deny.clone();
    mcp["spec"]["events"]["tool.before"]["steps"][0]["match"] =
        json!({"tools":["native:qoder-sdk:mcp__files__read"]});
    save("mcp-deny.json", &mcp);
    let service = Service::start(&f, &initial);
    let mut command = launch(&f);
    command
        .env("QODER_TEST_ROOT", &f.root)
        .env("QODER_POST_CASE", "failure");
    assert_eq!(f.successful(command)["passed"], true);
    service.released(&f);
}

#[test]
fn empty_policies_capture_tool_facts_without_sight_in_both_launch_modes() {
    for reloadable in [false, true] {
        for failure in [false, true] {
            let fixture = Fixture::new();
            let mut document = setup(&fixture);
            document["spec"]["events"] = json!({});
            document["spec"]["providers"] = json!({});
            let service = Service::start(&fixture, &document);
            let mut command = launch(&fixture);
            if reloadable {
                let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/qoder_sdk");
                fs::copy(
                    source.join("reload-facts.mjs"),
                    fixture.root.join("app.mjs"),
                )
                .unwrap();
                fs::copy(
                    source.join("reload-sdk.mjs"),
                    fixture
                        .root
                        .join("node_modules/@qoder-ai/qoder-agent-sdk/dist/index.js"),
                )
                .unwrap();
                command.arg("--reloadable");
            }
            if failure {
                command.env("QODER_FAILURE", "1");
            }
            assert_eq!(fixture.successful(command)["denied"], false);
            if !reloadable {
                assert!(fixture.root.join("executed").exists());
            }
            assert!(fixture.root.join("query-closed").exists());
            let mut events = Vec::new();
            for entry in fs::read_dir(fixture.root.join("state/journal")).unwrap() {
                let path = entry.unwrap().path();
                if path
                    .extension()
                    .is_none_or(|extension| extension != "jsonl")
                {
                    continue;
                }
                let text = fs::read_to_string(path).unwrap();
                let initial: Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
                let event = &initial["record"]["plan"]["executionEvent"];
                if matches!(
                    event["data"]["name"].as_str(),
                    Some("tool.before" | "tool.after")
                ) {
                    events.push(event.clone());
                }
            }
            events.sort_by_key(|event| event["data"]["name"].as_str().unwrap().to_owned());
            assert_eq!(
                events.len(),
                2,
                "reloadable={reloadable}, failure={failure}: {events:?}"
            );
            assert_eq!(events[0]["data"]["name"], "tool.after");
            assert_eq!(events[1]["data"]["name"], "tool.before");
            assert_eq!(
                events[0]["data"]["details"]["resultStatus"],
                if failure { "error" } else { "success" }
            );
            assert_eq!(events[0]["data"]["subject"], events[1]["data"]["subject"]);
            assert!(events
                .iter()
                .all(|event| event["data"]["association"]["status"] == "verified"));
            service.released(&fixture);
        }
    }
}
