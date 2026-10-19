//! Headless launcher contracts without an installed framework or model credentials.
#![cfg(target_os = "linux")]
#[allow(dead_code)]
#[path = "launcher/support.rs"]
mod support;

use serde_json::{json, Value};
use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};
use support::{Fixture, Service};

fn setup(f: &Fixture, mode: &str) -> Value {
    let node = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|p| p.join("node"))
        .find(|p| p.is_file())
        .expect("Node.js is required");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/deepseek-harness");
    let executable = f.root.join("dsh");
    fs::write(&executable, format!("#!{}\nimport os,sys\nif sys.argv[1:] == ['--version']: print('0.2.1-rc.2'); sys.exit(0)\nos.execv({:?}, [{:?}, {:?}, *sys.argv[1:]])\n",
        f.python.display(), node, node, fixture.join("synthetic.mjs"))).unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let mut document = f.document();
    document["spec"]["agents"] = json!({"dsh":{"adapter":"deepseek-harness",
        "argv":[executable, "--profile", "headless", "task"]}});
    document["spec"]["providers"] = json!({"policy":{"protocol":"aw-provider/v1alpha1",
        "config":{}, "timeout_ms":1000, "max_output_bytes":65536,
        "transport":{"type":"stdio", "location":"agent", "argv":[f.python, fixture.join("policy.py"), f.root, mode]}}});
    document["spec"]["events"] = json!({
        "tool.before":{"enabled":true, "steps":[
            {"id":"scan", "provider":"policy", "operation":"scan", "effects":["block"], "on_error":"block"},
            {"id":"tail", "provider":"policy", "operation":"scan", "effects":["observe"], "on_error":"report"}]},
        "tool.after":{"enabled":true, "steps":[
            {"id":"compress", "provider":"policy", "operation":"record", "effects":["replace_result"], "on_error":"report"}]}
    });
    document
}

fn launch(f: &Fixture, mode: &str) -> Command {
    let mut command = f.command();
    command
        .args(["run", "--config"])
        .arg(f.root.join("aw.json"))
        .args(["--agent", "dsh"])
        .env("DSH_TEST_MODE", mode);
    command
}

#[test]
fn headless_overlay_blocks_tools_and_releases_binding() {
    for mode in ["allow", "deny", "bypass", "error"] {
        let f = Fixture::new();
        let document = setup(&f, if mode == "error" { "error" } else { "replace" });
        let service = Service::start(&f, &document);
        let output = f.run(launch(&f, mode));
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            result["executed"],
            mode == "allow",
            "{mode}: {result}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(result["result"]["isError"], mode != "allow");
        if mode == "allow" {
            assert_eq!(result["result"]["content"][0]["text"], "compact");
            assert_eq!(result["result"]["value"], "canonical");
            assert_eq!(result["result"]["meta"]["kept"], true);
        }
        service.released(&f);
    }
}

#[test]
fn unsupported_entrypoints_and_version_fail_before_service_start() {
    for args in [
        json!(["--profile", "web"]),
        json!(["headless"]),
        json!(["--profile", "headless", "--from-default-profile", "web"]),
        json!(["--profile", "headless", "--dump-config"]),
        json!(["--profile", "headless", "--patch"]),
    ] {
        let f = Fixture::new();
        let mut document = setup(&f, "replace");
        let mut argv = vec![document["spec"]["agents"]["dsh"]["argv"][0].clone()];
        argv.extend(args.as_array().unwrap().clone());
        document["spec"]["agents"]["dsh"]["argv"] = json!(argv);
        f.save(&document);
        assert!(!f.run(launch(&f, "allow")).status.success());
        assert!(!f.root.join("state").exists());
    }
    let f = Fixture::new();
    let document = setup(&f, "replace");
    let executable = f.root.join("dsh");
    fs::write(
        &executable,
        fs::read_to_string(&executable)
            .unwrap()
            .replace("0.2.1-rc.2", "0.2.2"),
    )
    .unwrap();
    f.save(&document);
    assert!(!f.run(launch(&f, "allow")).status.success());
    assert!(!f.root.join("state").exists());
}
