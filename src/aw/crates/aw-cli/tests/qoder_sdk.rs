//! SDK bridge regression through the real AW service; the native suite pins the public SDK.
#![cfg(target_os = "linux")]
#[allow(dead_code)]
#[path = "launcher/support.rs"]
mod support;
use serde_json::{json, Value};
use std::{fs, path::Path, process::Command};
use support::{Fixture, Service};

#[path = "qoder_sdk/tool_scope.rs"]
mod tool_scope;

#[path = "qoder_sdk/post.rs"]
mod post;

#[path = "qoder_sdk/resident_reload.rs"]
mod resident_reload;

fn setup(f: &Fixture) -> Value {
    let node = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|p| p.join("node"))
        .find(|p| p.is_file())
        .expect("SDK fixtures require Node");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/qoder_sdk");
    let app = f.root.join("app.mjs");
    fs::copy(source.join("synthetic.mjs"), &app).unwrap();
    let sdk = f.root.join("node_modules/@qoder-ai/qoder-agent-sdk");
    fs::create_dir_all(sdk.join("dist")).unwrap();
    fs::write(sdk.join("package.json"), json!({"name":"@qoder-ai/qoder-agent-sdk","version":"1.0.56","qoderCliVersion":"1.1.78","qoderSdkBrand":"global","type":"module","main":"dist/index.js"}).to_string()).unwrap();
    fs::copy(source.join("sdk.mjs"), sdk.join("dist/index.js")).unwrap();
    let mut document = f.document();
    document["spec"]["agents"] = json!({"sdk":{"adapter":"qoder-sdk","argv":[node,app]}});
    f.provider(&mut document);
    document
}
fn launch(f: &Fixture) -> Command {
    let mut command = f.command();
    command
        .args(["run", "--config"])
        .arg(f.root.join("aw.json"))
        .args(["--agent", "sdk"]);
    command
}

#[test]
fn query_hooks_preserve_first_call_identity_and_cleanup() {
    for failure in [false, true] {
        let f = Fixture::new();
        let document = setup(&f);
        let service = Service::start(&f, &document);
        let mut command = launch(&f);
        if failure {
            command.env("QODER_FAILURE", "1");
        }
        assert_eq!(f.successful(command)["denied"], false);
        assert!(f.root.join("executed").exists());
        assert!(f.root.join("query-closed").exists());
        let requests: Vec<Value> = fs::read_dir(&f.root)
            .unwrap()
            .map(Result::unwrap)
            .filter(|e| e.file_name().to_string_lossy().starts_with("provider-"))
            .map(|e| serde_json::from_slice::<Value>(&fs::read(e.path()).unwrap()).unwrap())
            .filter(|r| r["method"] == "invoke")
            .collect();
        assert_eq!(requests.len(), 2);
        for request in &requests {
            assert_eq!(request["event"]["session_id"], "session-1");
            assert_eq!(
                request["event"]["tool"]["call_id"].as_str().unwrap().len(),
                36
            );
            assert_eq!(
                request["event"]["native"]["aw_native_tool_use_id"],
                "call-1"
            );
            assert_eq!(request["event"]["agent"]["adapter"], "qoder-sdk");
        }
        let after = requests
            .iter()
            .find(|r| r["event"]["name"] == "tool.after")
            .unwrap();
        if failure {
            assert_eq!(after["event"]["tool"]["result"]["error"], "tool failed");
        } else {
            assert_eq!(after["event"]["tool"]["result"], "native result");
        }
        service.released(&f);
    }
}
#[test]
fn denial_and_failed_initialization_never_execute_tools() {
    for mode in ["deny", "initialization"] {
        let f = Fixture::new();
        let document = setup(&f);
        let service = Service::start(&f, &document);
        let mut command = launch(&f);
        if mode == "deny" {
            command.env("QODER_SCENARIO", "block");
            assert_eq!(f.successful(command)["denied"], true);
        } else {
            command.env("QODER_INIT_FAIL", "1");
            assert!(!f.run(command).status.success());
        }
        assert!(!f.root.join("executed").exists());
        service.released(&f);
    }
}
#[test]
fn unsupported_versions_overrides_and_replacement_are_rejected() {
    for mode in [
        "version",
        "replacement",
        "override",
        "reloadable-plugins",
        "reloadable-wrappers",
    ] {
        let f = Fixture::new();
        let mut document = setup(&f);
        if mode == "version" {
            let path = f
                .root
                .join("node_modules/@qoder-ai/qoder-agent-sdk/package.json");
            fs::write(
                &path,
                fs::read_to_string(&path)
                    .unwrap()
                    .replace("1.0.56", "1.0.55"),
            )
            .unwrap();
        }
        if mode == "replacement" {
            document["spec"]["events"]["tool.after"]["steps"][0]["effects"] =
                json!(["replace_result"]);
        }
        if mode.starts_with("reloadable-") {
            document["spec"]["agents"]["sdk"]["qoder_sdk"] = if mode == "reloadable-plugins" {
                json!({"declarative_plugins":true})
            } else {
                json!({"command_wrappers":[{"path":"/absent/wrapper","sha256":"0".repeat(64)}]})
            };
        }
        let service = Service::start(&f, &document);
        let mut command = launch(&f);
        if mode == "override" {
            command.args(["--native-settings", "unsupported"]);
        }
        if mode.starts_with("reloadable-") {
            command.arg("--reloadable");
        }
        let output = f.run(command);
        assert!(!output.status.success());
        if mode.starts_with("reloadable-") {
            assert!(String::from_utf8_lossy(&output.stderr).contains("ordinary resident mode"));
            assert!(!f.root.join("query-closed").exists());
        }
        assert!(!f.root.join("executed").exists());
        service.released(&f);
    }
}

#[test]
fn callback_composition_preserves_vetoes_isolation_and_cancellation() {
    let f = Fixture::new();
    let mut command = Command::new("node");
    command
        .current_dir(&f.root)
        .args(["--test", "--test-reporter=tap"])
        .arg(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/qoder_sdk/bridge.test.mjs"),
        );
    let output = f.run(command);
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("# pass 119"));
}

#[test]
fn existing_queries_update_only_after_native_cleanup() {
    reload_queries(false);
}

#[test]
fn single_configuration_keeps_sdk_updates_busy_checks_and_service_identity() {
    reload_queries(true);
}

fn reload_queries(single_configuration: bool) {
    for failure in [false, true] {
        let f = Fixture::new();
        let mut candidate = setup(&f);
        let policy = f.root.join("retry-policy.py");
        fs::write(&policy, fs::read_to_string(f.action()).unwrap().replace(
            "\"reason_code\": \"fixture_policy\"",
            "\"reason_code\": \"fixture_policy\", \"text\": \"Retry with arguments: rtk candidate\"",
        )).unwrap();
        candidate["spec"]["providers"]["policy"]["transport"]["argv"][1] = json!(policy);
        let mut initial = candidate.clone();
        initial["spec"]["providers"] = json!({});
        initial["spec"]["events"] = json!({});
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/qoder_sdk");
        fs::copy(source.join("reload.mjs"), f.root.join("app.mjs")).unwrap();
        fs::copy(
            source.join("reload-sdk.mjs"),
            f.root
                .join("node_modules/@qoder-ai/qoder-agent-sdk/dist/index.js"),
        )
        .unwrap();
        fs::write(f.root.join("candidate.json"), candidate.to_string()).unwrap();
        let mut native = initial.clone();
        f.native(&mut native, "failure", "observe");
        native["spec"]["providers"]["failure"]["transport"]["argv"] =
            json!([f.python, "-c", "import sys; sys.exit(3)"]);
        fs::write(f.root.join("native-failure.json"), native.to_string()).unwrap();
        native["spec"]["events"]["tool.before"]["steps"][0]["on_error"] = json!("report");
        fs::write(f.root.join("native-report.json"), native.to_string()).unwrap();
        let service = Service::start(&f, &initial);
        let mut command = launch(&f);
        command.arg("--reloadable");
        if single_configuration {
            command.env("QODER_SINGLE_CONFIG", "1");
        }
        if failure {
            command.env("QODER_RELOAD_FAILURE", "1");
        }
        let output = f.run(command);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{stderr}");
        assert!(!stderr.contains("aw hook failed"), "{stderr}");
        assert_eq!(
            serde_json::from_slice::<Value>(&output.stdout).unwrap()["passed"],
            true
        );
        service.released(&f);
    }
}

#[test]
fn installed_bridge_fences_queries_after_private_cleanup_failure() {
    let fixture = Fixture::new();
    let document = setup(&fixture);
    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/qoder_sdk/private-packaged.mjs");
    fs::copy(source, fixture.root.join("app.mjs")).unwrap();
    let service = Service::start(&fixture, &document);
    assert_eq!(fixture.successful(launch(&fixture))["fenced"], true);
    assert!(!fixture.root.join("executed").exists());
    service.released(&fixture);
}
