//! Hook shortcuts preserve offline editing, literal argv and existing policy semantics.

use super::support::{success, Fixture};
use serde_json::{json, Value};
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    process::Command,
};

fn read(f: &Fixture) -> Value {
    aw_config::Validator::new()
        .unwrap()
        .parse(&fs::read(&f.config).unwrap())
        .unwrap()
        .as_value()
        .clone()
}

fn init(f: &Fixture, binary: &str) -> Command {
    let mut cmd = Command::new(binary);
    cmd.current_dir(&f.root);
    if binary == env!("CARGO_BIN_EXE_aw") {
        cmd.args(["config", "init"]);
    } else {
        cmd.arg("configure");
    }
    cmd.arg("--prefix")
        .arg(&f.prefix)
        .arg("--config")
        .arg(&f.config)
        .arg("--state-dir")
        .arg(&f.state)
        .args(["--qwenpaw", "/bin/true"]);
    cmd
}

#[test]
fn qwenpaw_only_initialization_keeps_native_app_and_legacy_command_templates() {
    for binary in [env!("CARGO_BIN_EXE_aw"), env!("CARGO_BIN_EXE_aw-package")] {
        let f = Fixture::new();
        success(f.install().output().unwrap());
        success(init(&f, binary).output().unwrap());
        let value = read(&f);
        assert_eq!(
            value["spec"]["agents"],
            json!({"qwenpaw":{"adapter":"qwenpaw","argv":["/bin/true","app"]}})
        );
        assert_eq!(value["spec"]["providers"], json!({}));
        assert_eq!(value["spec"]["events"], json!({}));
        assert!(!f.state.exists());
        assert_eq!(fs::metadata(&f.config).unwrap().mode() & 0o777, 0o600);
        let bytes = fs::read(&f.config).unwrap();
        assert!(!init(&f, binary).output().unwrap().status.success());
        assert_eq!(fs::read(&f.config).unwrap(), bytes);
        fs::remove_file(&f.config).unwrap();
        success(
            init(&f, binary)
                .args([
                    "--provider",
                    "command",
                    "--check",
                    "/bin/true",
                    "--effect",
                    "block",
                    "--reason-code",
                    "rule",
                    "--",
                    "--json",
                    "literal ; $HOME",
                ])
                .output()
                .unwrap(),
        );
        let value = read(&f);
        assert_eq!(
            value["spec"]["providers"]["command"]["transport"]["argv"],
            json!(["/bin/true", "--json", "literal ; $HOME"])
        );
        assert_eq!(
            value["spec"]["events"]["tool.before"]["steps"][0]["on_error"],
            "block"
        );
        assert!(!f.state.exists());
    }
}

#[test]
fn command_hooks_append_atomically_preserve_policy_and_keep_literal_arguments() {
    let f = Fixture::new();
    success(f.install().output().unwrap());
    success(f.init().output().unwrap());
    let mut value = read(&f);
    value["spec"]["providers"]["security"] = json!({"transport":{"type":"stdio","location":"agent","argv":["/unexecuted/security"]},"timeout_ms":2000,"max_output_bytes":32});
    let security =
        json!({"id":"security","provider":"security","effects":["block"],"on_error":"block"});
    value["spec"]["events"]["tool.before"] = json!({"enabled":true,"required":true,"budget_ms":4000,"match":{"tools":["bash"]},"steps":[security]});
    fs::write(&f.config, serde_yaml_ng::to_string(&value).unwrap()).unwrap();
    let before = [
        "--event",
        "tool.before",
        "--id",
        "tenant",
        "--check",
        "/unexecuted/check",
        "--effect",
        "block",
        "--on-error",
        "block",
        "--reason-code",
        "tenant_policy",
        "--",
        "--json",
        "two words",
        "; $(touch NEVER)",
    ];
    success(f.edit("add-hook", &before).output().unwrap());
    let after = [
        "--event",
        "tool.after",
        "--id",
        "record",
        "--check",
        "/unexecuted/record",
        "--effect",
        "observe",
        "--on-error",
        "report",
    ];
    let output = f.edit("add-hook", &after).arg("--json").output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "succeeded");
    assert_eq!(report["result"]["changed"], true);
    let saved = read(&f);
    for key in ["daemon", "agents", "audit", "execution"] {
        assert_eq!(saved["spec"][key], value["spec"][key]);
    }
    assert_eq!(
        saved["spec"]["providers"]["security"],
        value["spec"]["providers"]["security"]
    );
    let event = &saved["spec"]["events"]["tool.before"];
    for key in ["enabled", "required", "budget_ms", "match"] {
        assert_eq!(event[key], value["spec"]["events"]["tool.before"][key]);
    }
    assert_eq!(event["steps"][0], security);
    assert_eq!(event["steps"][1]["id"], "tenant");
    assert_eq!(
        saved["spec"]["providers"]["tenant"]["transport"]["argv"],
        json!([
            "/unexecuted/check",
            "--json",
            "two words",
            "; $(touch NEVER)"
        ])
    );
    assert_eq!(
        saved["spec"]["events"]["tool.after"]["steps"][0]["effects"],
        json!(["observe"])
    );
    let bytes = fs::read(&f.config).unwrap();
    let inode = fs::metadata(&f.config).unwrap().ino();
    assert_eq!(
        success(f.edit("add-hook", &before).output().unwrap()).trim(),
        "Configuration unchanged"
    );
    assert_eq!(fs::read(&f.config).unwrap(), bytes);
    assert_eq!(fs::metadata(&f.config).unwrap().ino(), inode);
    success(
        f.edit("remove-hook", &["--event", "tool.before", "--id", "tenant"])
            .output()
            .unwrap(),
    );
    assert_eq!(
        read(&f)["spec"]["events"]["tool.before"]["steps"],
        json!([security])
    );
    assert!(read(&f)["spec"]["providers"].get("tenant").is_some());
    assert!(!f.state.exists());
    assert!(!f.root.join("NEVER").exists());
}

#[test]
fn command_hook_invalid_or_conflicting_edits_leave_bytes_and_inode_unchanged() {
    for binary in [env!("CARGO_BIN_EXE_aw"), env!("CARGO_BIN_EXE_aw-package")] {
        let f = Fixture::new();
        success(f.install().output().unwrap());
        success(f.init().output().unwrap());
        let base = [
            "--event",
            "tool.before",
            "--id",
            "rule",
            "--check",
            "/unexecuted/check",
            "--effect",
            "observe",
            "--on-error",
            "report",
        ];
        let run = |args: &[&str]| {
            Command::new(binary)
                .current_dir(&f.root)
                .args(["config", "add-hook", "--config"])
                .arg(&f.config)
                .args(args)
                .output()
                .unwrap()
        };
        assert!(run(&base).status.success());
        let mut disabled = read(&f);
        disabled["spec"]["events"]["tool.after"] = json!({"enabled":false,"steps":[]});
        fs::write(&f.config, serde_yaml_ng::to_string(&disabled).unwrap()).unwrap();
        let bytes = fs::read(&f.config).unwrap();
        let inode = fs::metadata(&f.config).unwrap().ino();
        for replacement in [
            (1, "tool.after"), // Disabled event cannot be silently enabled.
            (1, "unknown.event"),
            (5, "relative-check"),
            (5, "/different/check"), // Same Provider name cannot be overwritten.
            (7, "replace_result"),
            (9, "invalid"),
        ] {
            let mut args = base.to_vec();
            args[replacement.0] = replacement.1;
            assert!(!run(&args).status.success(), "{args:?}");
            assert_eq!(fs::read(&f.config).unwrap(), bytes);
            assert_eq!(fs::metadata(&f.config).unwrap().ino(), inode);
        }
        for extra in [
            vec!["--definition", "missing.yaml"],
            vec![
                "--provider",
                "new-provider",
                "--reason-code",
                "different_reason",
            ],
            vec!["--reason-code", "bad code"],
        ] {
            let mut args = base.to_vec();
            args.extend(extra);
            assert!(!run(&args).status.success());
            assert_eq!(fs::read(&f.config).unwrap(), bytes);
            assert_eq!(fs::metadata(&f.config).unwrap().ino(), inode);
        }
        assert!(!f.state.exists());
    }
}

#[test]
fn qwenpaw_initialization_validates_entrypoints_and_node_pair_before_publication() {
    let f = Fixture::new();
    success(f.install().output().unwrap());
    let script = f.root.join("unexecuted");
    fs::write(&script, "touch NEVER").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o600)).unwrap();
    let mut invalid = init(&f, env!("CARGO_BIN_EXE_aw"));
    // Select a different target input without relying on duplicate-option behavior.
    let args: Vec<_> = invalid.get_args().map(|s| s.to_owned()).collect();
    invalid = f.command();
    invalid.args(&args[..args.len() - 1]).arg(&script);
    assert!(!invalid.output().unwrap().status.success());
    assert!(!f.config.exists());
    assert!(!init(&f, env!("CARGO_BIN_EXE_aw"))
        .args(["--node", "/bin/true"])
        .output()
        .unwrap()
        .status
        .success());
    assert!(!f.config.exists());
    assert!(!f.root.join("NEVER").exists());
}

#[test]
fn generated_before_and_after_hooks_execute_through_the_existing_boolean_host() {
    use aw_host::{Host, ProcessContext, StepOutput};
    use aw_provider::admission::AdapterCapabilities;
    use std::{
        collections::BTreeMap,
        sync::atomic::AtomicBool,
        time::{Duration, Instant},
    };
    let f = Fixture::new();
    success(f.install().output().unwrap());
    success(f.init().output().unwrap());
    let script = f.root.join("check.py");
    fs::write(&script, "import json,sys\nfrom pathlib import Path\nr=json.load(sys.stdin)\nPath(r['name']+'.json').write_text(json.dumps({'event':r,'argv':sys.argv[1:]}))\nprint('true')\n").unwrap();
    let literal = "two words ; $(touch NEVER) $HOME";
    for (event, id, effect, on_error) in [
        ("tool.before", "check", "block", "block"),
        ("tool.after", "record", "observe", "report"),
    ] {
        success(
            f.edit(
                "add-hook",
                &[
                    "--event",
                    event,
                    "--id",
                    id,
                    "--check",
                    "/usr/bin/python3",
                    "--effect",
                    effect,
                    "--on-error",
                    on_error,
                    "--",
                    script.to_str().unwrap(),
                    literal,
                ],
            )
            .output()
            .unwrap(),
        );
    }
    let stop = AtomicBool::new(false);
    let deadline = Instant::now() + Duration::from_secs(10);
    let host = Host::prepare(
        &fs::read(&f.config).unwrap(),
        "qoder",
        AdapterCapabilities {
            adapter: "qoder".into(),
            version: "fixture".into(),
            entrypoint: "fixture".into(),
            events: BTreeMap::from([
                ("tool.before".into(), vec!["observe".into(), "block".into()]),
                ("tool.after".into(), vec!["observe".into()]),
            ]),
        },
        ProcessContext {
            cwd: f.root.clone(),
            environment: BTreeMap::new(),
            stderr_bytes: 4096,
        },
        deadline,
        &stop,
    )
    .unwrap();
    assert!(host.preparation().is_empty());
    assert!(!f.root.join("tool.before.json").exists());
    for (name, id, blocked) in [
        ("tool.before", "check", true),
        ("tool.after", "record", false),
    ] {
        let input = json!({"name":name,"agent":{"adapter":"qoder","binding_id":"qoder","instance_id":null},"session_id":null,"tool":{"name":"Bash","native_name":"Bash","call_id":null,"input":{"command":"echo fixture"},"result":null},"native":{}});
        let event = host.event(input.clone(), deadline, &stop).unwrap();
        let StepOutput::Provider(result) = event.invoke(id).unwrap().result.unwrap() else {
            panic!("expected boolean Hook projection")
        };
        assert_eq!(result.requests_block(), blocked);
        let observed: Value =
            serde_json::from_slice(&fs::read(f.root.join(format!("{name}.json"))).unwrap())
                .unwrap();
        assert_eq!(observed["event"], input);
        assert_eq!(observed["argv"], json!([literal]));
    }
    assert!(!f.root.join("NEVER").exists());
    assert!(!f.state.exists());
}
