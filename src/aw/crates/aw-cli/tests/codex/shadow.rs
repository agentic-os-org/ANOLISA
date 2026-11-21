//! Preserve native callbacks while evaluating shadow Provider policy.

use super::*;

#[test]
fn slow_shadow_leaves_enforcing_tool_outcome_unchanged() {
    let mut outputs = Vec::new();
    for mode in ["off", "shadow"] {
        let f = Fixture::new();
        let mut document = setup(&f);
        let before = &mut document["spec"]["events"]["tool.before"];
        before["budget_ms"] = json!(2200);
        before["steps"].as_array_mut().unwrap().insert(
            0,
            json!({
                "id":"slow-shadow","provider":"policy","operation":"slow",
                "mode":mode,"effects":["observe","block"],"on_error":"block"
            }),
        );
        document["spec"]["providers"]["policy"]["timeout_ms"] = json!(5000);
        f.save(&document);
        let service = Service::start(&f, &document);
        install(&f);
        let mut launch = command(&f, "run");
        if mode == "shadow" {
            launch.env("FAKE_CODEX_EXPECT_SHADOW", "1");
        }
        launch.args(["--", "--", "allow"]);
        let output = f.run(launch);
        assert!(
            output.status.success(),
            "{mode}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(f.root.join("codex-tool-executed").exists(), "{mode}");
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        outputs.push(result);
        let calls = fs::read_to_string(f.root.join("policy-calls")).unwrap();
        assert!(
            calls.starts_with("scan_prompt\nscan\nretry\n"),
            "{mode}: {calls}"
        );
        service.released(&f);
    }
    assert_eq!(outputs[0], outputs[1]);
}

#[test]
fn off_and_shadow_replacement_preserve_native_post_tool_hooks() {
    for mode in ["off", "shadow"] {
        let f = Fixture::new();
        let mut document = setup(&f);
        let step = &mut document["spec"]["events"]["tool.after"]["steps"][0];
        step["mode"] = json!(mode);
        step["effects"] = json!(["replace_result"]);
        step["on_error"] = json!("withhold_result");
        f.save(&document);
        let service = Service::start(&f, &document);
        install(&f);
        let hooks = f.root.join("home/hooks.json");
        let mut inventory: Value = serde_json::from_slice(&fs::read(&hooks).unwrap()).unwrap();
        let native_hook = json!({"matcher":"Bash","hooks":[{"type":"command","command":"printf native-post","timeout":3}]});
        if inventory["hooks"]["PostToolUse"].is_null() {
            inventory["hooks"]["PostToolUse"] = json!([]);
        }
        let post = inventory["hooks"]["PostToolUse"].as_array_mut().unwrap();
        if mode == "off" {
            post.clear();
        }
        post.insert(0, native_hook);
        fs::write(&hooks, inventory.to_string()).unwrap();
        let mut launch = command(&f, "run");
        launch.env("FAKE_CODEX_FEATURES", r#"{"hooks":true,"code_mode":true}"#);
        launch.args(["--", "--", "replace"]);
        let output = f.run(launch);
        assert!(
            output.status.success(),
            "{mode}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["response"], "original output");
        assert!(result["results"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["stdout"] == "native-post"));
        assert!(!output
            .stdout
            .windows(b"AW_REDACTED_RESULT".len())
            .any(|part| part == b"AW_REDACTED_RESULT"));
        service.released(&f);
    }
}
