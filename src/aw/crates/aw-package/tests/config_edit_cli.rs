//! User-facing offline editing and failure-preserving CLI contracts.

use aw_package::{Policy, Settings};
use serde_json::{json, Value};
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Output},
};

struct Fixture {
    directory: tempfile::TempDir,
    config: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/config-edit-cli");
        fs::create_dir_all(&target).unwrap();
        let directory = tempfile::tempdir_in(target.canonicalize().unwrap()).unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let config = directory.path().join("aw.yaml");
        let document = aw_package::document(&Settings {
            prefix: &directory.path().join("prefix"),
            config: &config,
            state: &directory.path().join("state"),
            policy: Policy::None,
            qoder: Some(Path::new("/unexecuted/qoder")),
            node: None,
            openclaw: None,
        })
        .unwrap();
        assert_eq!(document["spec"]["providers"], json!({}));
        assert_eq!(document["spec"]["events"], json!({}));
        fs::write(&config, serde_yaml_ng::to_string(&document).unwrap()).unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
        Self { directory, config }
    }

    fn run(&self, action: &str, arguments: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_aw-package"))
            .current_dir(self.directory.path())
            .args(["config", action, "--config"])
            .arg(&self.config)
            .args(arguments)
            .output()
            .unwrap()
    }

    fn success(&self, action: &str, arguments: &[&str]) -> String {
        let output = self.run(action, arguments);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
        String::from_utf8(output.stdout).unwrap()
    }

    fn failure(&self, action: &str, arguments: &[&str]) -> String {
        let before = fs::read(&self.config).unwrap();
        let inode = fs::metadata(&self.config).unwrap().ino();
        let output = self.run(action, arguments);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let diagnostic = String::from_utf8(output.stderr).unwrap();
        assert!(diagnostic.contains("aw-package:"));
        assert!(!diagnostic.contains("private-value"));
        assert_eq!(fs::read(&self.config).unwrap(), before);
        assert_eq!(fs::metadata(&self.config).unwrap().ino(), inode);
        diagnostic
    }

    fn definition(&self, name: &str, value: &Value) {
        fs::write(
            self.directory.path().join(name),
            serde_yaml_ng::to_string(value).unwrap(),
        )
        .unwrap();
    }

    fn no_op(&self, action: &str, arguments: &[&str]) {
        let bytes = fs::read(&self.config).unwrap();
        let inode = fs::metadata(&self.config).unwrap().ino();
        assert_eq!(
            self.success(action, arguments).trim(),
            "Configuration unchanged"
        );
        assert_eq!(fs::read(&self.config).unwrap(), bytes);
        assert_eq!(fs::metadata(&self.config).unwrap().ino(), inode);
    }
}

fn provider(native: bool) -> Value {
    json!({
        "protocol": if native {"native-hook/v1alpha1"} else {"aw-provider/v1alpha1"},
        "transport":{"type":"stdio","location":"agent","argv":["/unexecuted/provider"]},
        "timeout_ms":1000,"max_output_bytes":4096,
        "config": if native {json!({})} else {json!({"password":"private-value","未知":{"ratio":0.125}})}
    })
}

fn hook(native: bool) -> Value {
    if native {
        json!({"id":"first","provider":"native","native":{},"on_error":"report"})
    } else {
        json!({"id":"first","provider":"policy","operation":"observe","effects":["observe"],"on_error":"report"})
    }
}

#[test]
fn configure_empty_events_support_explicit_structured_and_native_hook_lifecycles() {
    let fixture = Fixture::new();
    let original: Value = serde_json::from_str(&fixture.success("show", &[])).unwrap();
    for (name, native) in [("policy", false), ("native", true)] {
        fixture.definition("provider.yaml", &provider(native));
        assert_eq!(
            fixture
                .success(
                    "add-provider",
                    &["--name", name, "--definition", "provider.yaml"]
                )
                .trim(),
            "Updated configuration"
        );
        fixture.no_op(
            "add-provider",
            &["--name", name, "--definition", "provider.yaml"],
        );
    }
    fixture.definition("hook.yaml", &hook(false));
    fixture.failure(
        "add-hook",
        &["--event", "tool.before", "--definition", "hook.yaml"],
    );
    fixture.definition(
        "event.yaml",
        &json!({"enabled":false,"budget_ms":250,"match":{"tools":["bash"]},"steps":[]}),
    );
    fixture.success(
        "add-event",
        &["--event", "tool.before", "--definition", "event.yaml"],
    );
    fixture.no_op(
        "add-event",
        &["--event", "tool.before", "--definition", "event.yaml"],
    );
    fixture.success(
        "add-hook",
        &["--event", "tool.before", "--definition", "hook.yaml"],
    );
    fixture.no_op(
        "add-hook",
        &["--event", "tool.before", "--definition", "hook.yaml"],
    );
    fixture.definition("event.yaml", &json!({"enabled":true,"steps":[]}));
    fixture.success(
        "add-event",
        &["--event", "tool.after", "--definition", "event.yaml"],
    );
    // The same ID may be used in a different event, with another Provider protocol.
    fixture.definition("hook.yaml", &hook(true));
    fixture.success(
        "add-hook",
        &["--event", "tool.after", "--definition", "hook.yaml"],
    );
    assert_eq!(
        fixture.success("validate", &[]).trim(),
        "Valid configuration"
    );
    let saved: Value = serde_json::from_str(&fixture.success("show", &[])).unwrap();
    for field in ["daemon", "execution", "audit", "agents"] {
        assert_eq!(saved["spec"][field], original["spec"][field]);
    }
    assert_eq!(saved["spec"]["events"]["tool.before"]["enabled"], false);
    assert_eq!(saved["spec"]["providers"]["policy"], provider(false));
    assert_eq!(
        saved["spec"]["events"]["tool.after"]["steps"][0],
        hook(true)
    );
    for (event, name) in [("tool.before", "policy"), ("tool.after", "native")] {
        fixture.failure("remove-provider", &["--name", name]);
        fixture.success("remove-hook", &["--event", event, "--id", "first"]);
        fixture.no_op("remove-hook", &["--event", event, "--id", "first"]);
        fixture.success("remove-provider", &["--name", name]);
        fixture.no_op("remove-provider", &["--name", name]);
    }
    fixture.no_op("remove-hook", &["--event", "session.end", "--id", "absent"]);
    let saved: Value = serde_json::from_str(&fixture.success("show", &[])).unwrap();
    assert_eq!(saved["spec"]["events"]["tool.before"]["steps"], json!([]));
    assert_eq!(saved["spec"]["events"]["tool.before"]["budget_ms"], 250);
    assert_eq!(fs::metadata(&fixture.config).unwrap().mode() & 0o777, 0o600);
}

#[test]
fn show_and_validate_are_readonly_and_show_explicitly_returns_private_values() {
    let fixture = Fixture::new();
    fixture.definition("provider.yaml", &provider(false));
    fixture.success(
        "add-provider",
        &["--name", "policy", "--definition", "provider.yaml"],
    );
    fixture.definition(
        "event.yaml",
        &json!({"enabled":true,"required":true,"steps":[hook(false)]}),
    );
    fixture.success(
        "add-event",
        &["--event", "tool.before", "--definition", "event.yaml"],
    );
    let bytes = fs::read(&fixture.config).unwrap();
    let inode = fs::metadata(&fixture.config).unwrap().ino();
    let full = fixture.success("show", &[]);
    assert!(full.contains("\n  \"apiVersion\""));
    assert!(full.contains("private-value"));
    let selected: Value =
        serde_json::from_str(&fixture.success("show", &["--provider", "policy"])).unwrap();
    assert_eq!(selected, provider(false));
    let event: Value =
        serde_json::from_str(&fixture.success("show", &["--event", "tool.before"])).unwrap();
    assert_eq!(event["steps"], json!([hook(false)]));
    let step: Value = serde_json::from_str(
        &fixture.success("show", &["--event", "tool.before", "--id", "first"]),
    )
    .unwrap();
    assert_eq!(step, hook(false));
    assert_eq!(
        fixture.success("validate", &[]).trim(),
        "Valid configuration"
    );
    for arguments in [
        vec!["--provider", "missing"],
        vec!["--event", "session.end"],
        vec!["--event", "tool.before", "--id", "missing"],
    ] {
        fixture.failure("show", &arguments);
    }
    assert_eq!(fs::read(&fixture.config).unwrap(), bytes);
    assert_eq!(fs::metadata(&fixture.config).unwrap().ino(), inode);
}

#[test]
fn conflicts_dangling_references_and_invalid_edits_preserve_original_file() {
    let fixture = Fixture::new();
    fixture.definition("provider.yaml", &provider(false));
    fixture.success(
        "add-provider",
        &["--name", "policy", "--definition", "provider.yaml"],
    );
    fixture.definition("event.yaml", &json!({"enabled":true,"steps":[hook(false)]}));
    fixture.success(
        "add-event",
        &["--event", "tool.before", "--definition", "event.yaml"],
    );
    let mut conflict = provider(false);
    conflict["timeout_ms"] = json!(99);
    fixture.definition("provider.yaml", &conflict);
    fixture.failure(
        "add-provider",
        &["--name", "policy", "--definition", "provider.yaml"],
    );
    fixture.failure(
        "add-provider",
        &["--name", "bad/name", "--definition", "provider.yaml"],
    );
    fixture.definition("event.yaml", &json!({"enabled":false,"steps":[]}));
    fixture.failure(
        "add-event",
        &["--event", "tool.before", "--definition", "event.yaml"],
    );
    fixture.failure(
        "add-event",
        &["--event", "unknown.event", "--definition", "event.yaml"],
    );
    let mut conflict = hook(false);
    conflict["operation"] = json!("other");
    fixture.definition("hook.yaml", &conflict);
    fixture.failure(
        "add-hook",
        &["--event", "tool.before", "--definition", "hook.yaml"],
    );
    conflict["id"] = json!("other");
    conflict["provider"] = json!("unknown");
    fixture.definition("hook.yaml", &conflict);
    fixture.failure(
        "add-hook",
        &["--event", "tool.before", "--definition", "hook.yaml"],
    );
    fixture.failure("remove-provider", &["--name", "policy"]);
}

#[test]
fn json_definitions_are_supported_and_definition_files_are_never_executed() {
    let fixture = Fixture::new();
    let mut value = provider(true);
    let marker = fixture.directory.path().join("executed");
    value["transport"]["argv"] = json!(["/usr/bin/touch", marker]);
    fs::write(
        fixture.directory.path().join("provider.json"),
        serde_json::to_vec(&value).unwrap(),
    )
    .unwrap();
    let definition = fixture.directory.path().join("provider.json");
    fixture.success(
        "add-provider",
        &[
            "--name",
            "native",
            "--definition",
            definition.to_str().unwrap(),
        ],
    );
    fixture.definition("event.yaml", &json!({"enabled":true,"steps":[hook(true)]}));
    fixture.success(
        "add-event",
        &["--event", "tool.before", "--definition", "event.yaml"],
    );
    fixture.success("show", &[]);
    fixture.success("validate", &[]);
    assert!(!marker.exists());
}

#[test]
fn strict_definition_parsing_rejects_ambiguous_unbounded_or_nonobject_documents() {
    let fixture = Fixture::new();
    let mut inputs = vec![
        b"protocol: aw-provider/v1alpha1\nprotocol: native-hook/v1alpha1\n".to_vec(),
        br#"{"config":{"password":"private-value","password":"other"}}"#.to_vec(),
        b"1: private-value\n".to_vec(),
        b"x: .nan\n".to_vec(),
        b"x: !private private-value\n".to_vec(),
        b"<<: {x: private-value}\n".to_vec(),
        b"---\nx: private-value\n---\nx: other\n".to_vec(),
        b"private-value\n".to_vec(),
        b"[]\n".to_vec(),
        vec![],
        vec![0xff],
        vec![b' '; aw_config::MAX_DOCUMENT_BYTES + 1],
    ];
    inputs.push(
        format!(
            "{{\"nested\":{}null{}}}",
            "[".repeat(aw_config::MAX_DEPTH + 1),
            "]".repeat(aw_config::MAX_DEPTH + 1)
        )
        .into_bytes(),
    );
    for bytes in inputs {
        fs::write(fixture.directory.path().join("invalid.yaml"), bytes).unwrap();
        fixture.failure(
            "add-provider",
            &["--name", "policy", "--definition", "invalid.yaml"],
        );
    }
    fixture.failure(
        "add-provider",
        &["--name", "policy", "--definition", "missing.yaml"],
    );
    fixture.failure("add-provider", &["--name", "policy", "--definition", "."]);
    let fifo = fixture.directory.path().join("definition.fifo");
    let name = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
    // SAFETY: name is a live NUL-terminated path inside this owned fixture.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    fixture.failure(
        "add-provider",
        &["--name", "policy", "--definition", "definition.fifo"],
    );
}

#[test]
fn invalid_configuration_validation_masks_values_and_preserves_source_bytes() {
    let fixture = Fixture::new();
    let original = fs::read_to_string(&fixture.config).unwrap();
    fs::write(
        &fixture.config,
        original.replace("kind: AWConfiguration", "kind: private-value"),
    )
    .unwrap();
    fixture.failure("validate", &[]);
    fixture.failure("show", &[]);
    fixture.definition("provider.yaml", &provider(false));
    fixture.failure(
        "add-provider",
        &["--name", "policy", "--definition", "provider.yaml"],
    );
}

#[test]
fn malformed_options_fail_before_accessing_the_configuration() {
    let fixture = Fixture::new();
    let absent = fixture.directory.path().join("absent.yaml");
    let cases: Vec<Vec<&str>> = vec![
        vec!["unknown"],
        vec!["show", "--unknown", "value"],
        vec!["show", "--config", "unused"],
        vec!["show", "--provider", "policy", "--provider", "other"],
        vec!["show", "--provider", "policy", "--event", "tool.before"],
        vec!["show", "--id", "first"],
        vec!["show", "--event"],
        vec!["show", "--event", "--id", "first"],
        vec!["validate", "--event", "tool.before"],
        vec!["add-provider", "--definition", "missing.yaml"],
        vec!["add-provider", "--name", "policy"],
        vec!["remove-provider", "--id", "first"],
        vec!["remove-provider"],
        vec!["add-event", "--definition", "missing.yaml"],
        vec!["add-hook", "--event", "tool.before"],
        vec!["remove-hook", "--event", "tool.before"],
        vec!["remove-hook", "--id", "first"],
    ];
    for arguments in cases {
        let output = Command::new(env!("CARGO_BIN_EXE_aw-package"))
            .current_dir(fixture.directory.path())
            .arg("config")
            .args(arguments)
            .arg("--config")
            .arg(&absent)
            .output()
            .unwrap();
        assert!(!output.status.success());
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains("aw-package:"));
        assert!(
            !stderr.contains("No such file"),
            "accessed config before validating options: {stderr}"
        );
        assert!(!absent.exists());
    }
    let output = Command::new(env!("CARGO_BIN_EXE_aw-package"))
        .args(["config", "validate"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr)
        .unwrap()
        .contains("--config"));
}

#[test]
fn help_exposes_config_actions_and_the_private_value_display_boundary() {
    for arguments in [vec!["--help"], vec!["config", "--help"]] {
        let output = Command::new(env!("CARGO_BIN_EXE_aw-package"))
            .args(arguments)
            .output()
            .unwrap();
        assert!(output.status.success());
        let text = String::from_utf8(output.stdout).unwrap();
        for action in [
            "show",
            "validate",
            "add-provider",
            "remove-provider",
            "add-event",
            "add-hook",
            "remove-hook",
        ] {
            assert!(
                text.contains(&format!("config {action}")),
                "missing config {action}"
            );
        }
        assert!(text.contains("private"));
    }
}
