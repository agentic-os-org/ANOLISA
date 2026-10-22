//! SDK bridge regression through the real AW service; the native suite pins the public SDK.
#![cfg(target_os = "linux")]
#[allow(dead_code)]
#[path = "launcher/support.rs"]
mod support;
use serde_json::{json, Value};
use std::{fs, path::Path, process::Command};
use support::{Fixture, Service};

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
    fs::write(sdk.join("package.json"), json!({"name":"@qoder-ai/qoder-agent-sdk","version":"1.0.54","qoderCliVersion":"1.1.74","qoderSdkBrand":"global","type":"module","main":"dist/index.js"}).to_string()).unwrap();
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
            assert_eq!(request["event"]["tool"]["call_id"], "call-1");
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
    for mode in ["version", "replacement", "reload"] {
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
                    .replace("1.0.54", "1.0.55"),
            )
            .unwrap();
        }
        if mode == "replacement" {
            document["spec"]["events"]["tool.after"]["steps"][0]["effects"] =
                json!(["replace_result"]);
        }
        let service = Service::start(&f, &document);
        let mut command = launch(&f);
        if mode == "reload" {
            command.arg("--reloadable");
        }
        assert!(!f.run(command).status.success());
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
    assert!(String::from_utf8_lossy(&output.stdout).contains("# pass 11"));
}
