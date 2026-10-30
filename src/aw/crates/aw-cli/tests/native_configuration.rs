//! Executable embedding consumer: original native loader, AW editor and real Host.

use aw_adapter::configuration::{NativeConfiguration, NativeLoad};
use aw_config::{
    native::{NativeSource, SourceKind},
    Validator,
};
use aw_host::{FailureAction, Host, ProcessContext, StepOutput};
use aw_package::config_edit::ConfigurationEditor;
use aw_provider::admission::AdapterCapabilities;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

struct Fixture {
    dir: tempfile::TempDir,
    aw: PathBuf,
    sources: Vec<Value>,
}

impl Fixture {
    fn new() -> Self {
        let target =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/native-configuration");
        fs::create_dir_all(&target).unwrap();
        let dir = tempfile::tempdir_in(target.canonicalize().unwrap()).unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let aw = dir.path().join("aw.yaml");
        let sources = ["user", "project", "local"].into_iter().map(|scope| {
            let path = dir.path().join(format!("{scope}.yaml"));
            fs::write(&path, format!("# native comment: {scope}\nmodel: {scope}\nfuture: {{nested: [true, 42, null]}}\ncredential: '${{NATIVE_TOKEN}}'\nhooks: [first, second]\nmcp: {{same: {{cwd: ./relative, enabled: true}}}}\nplugins: {{same: {{enabled: true}}}}\n")).unwrap();
            json!({"id":scope,"kind":"file","scope":scope,"reference":format!("{scope}.yaml"),"base_dir":dir.path()})
        }).collect();
        let fixture = Self { dir, aw, sources };
        fs::write(
            &fixture.aw,
            serde_yaml_ng::to_string(&fixture.document()).unwrap(),
        )
        .unwrap();
        fs::set_permissions(&fixture.aw, fs::Permissions::from_mode(0o600)).unwrap();
        fixture
    }

    fn document(&self) -> Value {
        let mut document: Value =
            serde_json::from_str(include_str!("../../aw-config/examples/aw.minimal.json")).unwrap();
        document["spec"]["agents"]["qoder"]["native_config"] = json!({"sources":self.sources});
        document["spec"]["providers"] = json!({"same":{
            "protocol":"aw-hook/v1alpha1", "transport":{"type":"stdio","location":"agent","argv":["/bin/sh","-c","cat >/dev/null; printf true"]},
            "timeout_ms":1000,"max_output_bytes":32,"config":{}
        }});
        document["spec"]["events"] = json!({"tool.before":{"enabled":true,"steps":[{
            "id":"same","provider":"same","effects":["block"],"on_error":"block","match":{"tools":["bash"]}
        }]}});
        document
    }

    fn snapshot(&self) -> Vec<(PathBuf, Vec<u8>, u64)> {
        ["user", "project", "local"]
            .into_iter()
            .map(|scope| {
                let path = self.dir.path().join(format!("{scope}.yaml"));
                let bytes = fs::read(&path).unwrap();
                let inode = fs::metadata(&path).unwrap().ino();
                (path, bytes, inode)
            })
            .collect()
    }

    fn host(&self, document: &Value) -> Host {
        Host::prepare(
            &serde_json::to_vec(document).unwrap(),
            "qoder",
            capabilities(),
            ProcessContext {
                cwd: self.dir.path().into(),
                environment: BTreeMap::from([("PATH".into(), "/usr/bin:/bin".into())]),
                stderr_bytes: 1024,
            },
            Instant::now() + Duration::from_secs(5),
            &AtomicBool::new(false),
        )
        .unwrap()
    }
}

// The fixture native framework owns merge order, relative path interpretation,
// credentials, Hook order and native errors. The AW layer never copies its fields.
fn native_loader(sources: &[NativeSource]) -> Result<NativeLoad<Value>, String> {
    Ok(NativeLoad {
        value: native_load(sources)?,
        sources: sources.to_vec(),
    })
}

fn native_load(sources: &[NativeSource]) -> Result<Value, String> {
    native_load_with(
        sources,
        &BTreeMap::from([("NATIVE_MODEL", "environment")]),
        &["--model", "argument"],
    )
}

fn native_load_with(
    sources: &[NativeSource],
    environment: &BTreeMap<&str, &str>,
    argv: &[&str],
) -> Result<Value, String> {
    let mut merged = json!({});
    for source in sources {
        match source.kind {
            SourceKind::File => {
                let path = Path::new(source.base_dir.as_deref().unwrap()).join(&source.reference);
                let bytes = fs::read(path).map_err(|_| "native file error".to_owned())?;
                let value: Value = serde_yaml_ng::from_slice(&bytes)
                    .map_err(|_| "native syntax error".to_owned())?;
                let fields = value.as_object().ok_or("native object required")?;
                merged.as_object_mut().unwrap().extend(fields.clone());
            }
            SourceKind::Environment => {
                if let Some(value) = environment.get(source.reference.as_str()) {
                    merged["model"] = json!(value);
                }
            }
            SourceKind::Argument => {
                if let Some(value) = argv.windows(2).find(|pair| pair[0] == source.reference) {
                    merged["model"] = json!(value[1]);
                }
            }
            SourceKind::Discovery => {
                merged["discovered"] = json!(source.reference);
            }
        }
    }
    Ok(merged)
}

fn capabilities() -> AdapterCapabilities {
    AdapterCapabilities {
        adapter: "qoder".into(),
        version: "fixture".into(),
        entrypoint: "native-loader-consumer".into(),
        events: BTreeMap::from([("tool.before".into(), vec!["block".into(), "observe".into()])]),
        failure_actions: BTreeMap::new(),
    }
}

fn event(tool: &str) -> Value {
    json!({"name":"tool.before","agent":{"adapter":"qoder","binding_id":"qoder","instance_id":null},"session_id":null,
        "tool":{"name":tool,"native_name":tool,"call_id":null,"input":{"command":"native hook final arguments"},"result":null},"native":{}})
}

#[test]
fn original_native_loader_and_host_preserve_sources_and_enforce_only_admitted_scope() {
    let fixture = Fixture::new();
    let snapshot = fixture.snapshot();
    let mut document = fixture.document();
    let sources = document["spec"]["agents"]["qoder"]["native_config"]["sources"]
        .as_array_mut()
        .unwrap();
    sources.extend([
        json!({"id":"env","kind":"environment","scope":"process","reference":"NATIVE_MODEL"}),
        json!({"id":"argv","kind":"argument","scope":"process","reference":"--model"}),
        json!({"id":"future","kind":"discovery","scope":"future","reference":"native-defaults"}),
    ]);
    let bytes = serde_json::to_vec(&document).unwrap();
    let config = Validator::new().unwrap().parse(&bytes).unwrap();
    let baseline = native_load(&config.native_sources("qoder").unwrap()).unwrap();
    let loaded = NativeConfiguration::new(&config, "qoder")
        .unwrap()
        .load(native_loader)
        .unwrap();
    assert_eq!(loaded.native(), &baseline);
    assert_eq!(loaded.native()["model"], "argument");
    assert_eq!(loaded.native()["credential"], "${NATIVE_TOKEN}");
    assert_eq!(loaded.native()["hooks"], json!(["first", "second"]));
    assert_eq!(loaded.native()["mcp"]["same"]["cwd"], "./relative");
    assert_eq!(loaded.native()["plugins"]["same"]["enabled"], true);
    assert_eq!(loaded.native()["future"]["nested"], json!([true, 42, null]));
    assert_eq!(loaded.report().aw, "not_admitted");
    let coverage = loaded.admit(&capabilities(), &BTreeMap::new()).unwrap();
    let host = fixture.host(&document);
    assert_eq!(coverage.steps(), host.steps());
    assert_eq!(coverage.target(), host.target());
    assert_eq!(
        coverage.configuration().as_value(),
        host.configuration().as_value()
    );
    assert_eq!(coverage.report().aw, "admitted_not_active");
    let report = serde_json::to_string(&coverage.report()).unwrap();
    for private in [
        "NATIVE_TOKEN",
        "native-defaults",
        "local.yaml",
        "same",
        "--model",
    ] {
        assert!(!report.contains(private));
    }
    let cancelled = AtomicBool::new(false);
    let mut handler_calls = 0;
    for tool in ["bash", "future_plugin_tool", "mcp__same__call"] {
        let invocation = host
            .event(
                event(tool),
                Instant::now() + Duration::from_secs(5),
                &cancelled,
            )
            .unwrap()
            .invoke("same")
            .unwrap();
        match invocation.result.unwrap() {
            StepOutput::Skipped => {
                assert_ne!(tool, "bash");
                // Native permission remains the final authority for uncovered tools.
                if tool != "mcp__same__call" {
                    handler_calls += 1;
                }
            }
            StepOutput::Provider(outcome) => {
                assert_eq!(tool, "bash");
                assert!(outcome.requests_block());
            }
            StepOutput::Native(_) => panic!("expected the boolean policy"),
        }
    }
    assert_eq!(handler_calls, 1);
    assert_eq!(snapshot, fixture.snapshot());
}

#[test]
fn native_errors_and_covered_provider_failures_never_become_uncovered_success() {
    let fixture = Fixture::new();
    let mut document = fixture.document();
    document["spec"]["providers"]["same"]["transport"]["argv"] =
        json!(["/bin/sh", "-c", "cat >/dev/null; exit 23"]);
    let config = Validator::new()
        .unwrap()
        .parse(&serde_json::to_vec(&document).unwrap())
        .unwrap();
    let loaded = NativeConfiguration::new(&config, "qoder")
        .unwrap()
        .load(native_loader)
        .unwrap();
    let coverage = loaded.admit(&capabilities(), &BTreeMap::new()).unwrap();
    let host = fixture.host(&document);
    let cancelled = AtomicBool::new(false);
    let invocation = host
        .event(
            event("bash"),
            Instant::now() + Duration::from_secs(5),
            &cancelled,
        )
        .unwrap()
        .invoke("same")
        .unwrap();
    assert!(invocation.result.is_err());
    assert_eq!(invocation.failure_action, Some(FailureAction::Block));
    assert!(coverage.steps()[0].matches_tool(&event("bash")));
    assert_eq!(coverage.report().admitted_steps["tool.before"], 1);
    let mut unsupported = capabilities();
    unsupported.events.clear();
    assert!(loaded.admit(&unsupported, &BTreeMap::new()).is_err());
    assert_eq!(loaded.native()["model"], "local");
    fs::write(fixture.dir.path().join("local.yaml"), "[invalid:").unwrap();
    let baseline = native_load(&config.native_sources("qoder").unwrap()).unwrap_err();
    let error = NativeConfiguration::new(&config, "qoder")
        .unwrap()
        .load(native_loader)
        .err()
        .unwrap();
    assert_eq!(baseline, error);
    assert_eq!(error, "native syntax error");
}

#[test]
fn native_live_handle_retains_updates_without_mutating_admitted_policy() {
    let fixture = Fixture::new();
    let config = Validator::new()
        .unwrap()
        .parse(&fs::read(&fixture.aw).unwrap())
        .unwrap();
    // This loader returns a live reader, as a watcher-backed native loader would.
    let loaded = NativeConfiguration::new(&config, "qoder")
        .unwrap()
        .load(|sources| {
            let retained = sources.to_vec();
            Ok::<_, String>(NativeLoad {
                sources: retained.clone(),
                value: move || native_load(&retained),
            })
        })
        .unwrap();
    let coverage = loaded.admit(&capabilities(), &BTreeMap::new()).unwrap();
    assert_eq!((loaded.native())().unwrap()["model"], "local");
    fs::write(
        fixture.dir.path().join("local.yaml"),
        "model: changed\nnew_tool: enabled\n",
    )
    .unwrap();
    assert_eq!((loaded.native())().unwrap()["model"], "changed");
    assert!(coverage.steps()[0].matches_tool(&event("bash")));
    assert!(!coverage.steps()[0].matches_tool(&event("new_tool")));
    assert_eq!(coverage.report().admitted_steps["tool.before"], 1);
}

#[test]
fn editor_conflicts_invalid_candidates_and_stale_saves_preserve_native_files() {
    let fixture = Fixture::new();
    let snapshot = fixture.snapshot();
    let original = fs::read(&fixture.aw).unwrap();
    let mut editor = ConfigurationEditor::open(&fixture.aw).unwrap();
    let mut stale = ConfigurationEditor::open(&fixture.aw).unwrap();
    assert!(!editor
        .add_native_source("qoder", fixture.sources[0].clone())
        .unwrap());
    assert!(!editor.save().unwrap());
    assert_eq!(fs::read(&fixture.aw).unwrap(), original);
    let mut conflict = fixture.sources[0].clone();
    conflict["reference"] = json!("different.yaml");
    assert!(editor.add_native_source("qoder", conflict).is_err());
    assert!(editor.add_native_source("qoder",json!({"id":"bad","kind":"file","scope":"user","reference":"native","unknown":{"secret":"private"}})).is_err());
    assert!(editor.remove_provider("same").is_err());
    assert!(!editor.save().unwrap());
    editor.remove_native_source("qoder", "project").unwrap();
    editor.save().unwrap();
    stale.remove_native_source("qoder", "local").unwrap();
    assert!(stale.save().is_err());
    let current = Validator::new()
        .unwrap()
        .parse(&fs::read(&fixture.aw).unwrap())
        .unwrap();
    let mut candidate = fixture.document();
    candidate["spec"]["agents"]["qoder"]["native_config"]["sources"] = json!([]);
    let next = Validator::new()
        .unwrap()
        .parse(&serde_json::to_vec(&candidate).unwrap())
        .unwrap();
    assert!(current.validate_policy_update(&next).is_err());
    assert_eq!(snapshot, fixture.snapshot());
}

#[test]
fn cli_records_and_removes_only_descriptors_and_reports_no_runtime_claims() {
    let fixture = Fixture::new();
    let snapshot = fixture.snapshot();
    let definition = fixture.dir.path().join("source.json");
    fs::write(&definition,r#"{"id":"private-id","kind":"discovery","scope":"private-scope","reference":"private-reference"}"#).unwrap();
    let run = |action: &str, args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_aw"))
            .args(["config", action, "--config"])
            .arg(&fixture.aw)
            .args(["--agent", "qoder"])
            .args(args)
            .output()
            .unwrap()
    };
    let result = run(
        "add-native-source",
        &["--definition", definition.to_str().unwrap(), "--json"],
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let report = run("native-sources", &[]);
    assert!(report.status.success());
    let report = String::from_utf8(report.stdout).unwrap();
    assert!(!report.contains("private"));
    let report: Value = serde_json::from_str(&report).unwrap();
    assert_eq!(report["native"], "declared_not_loaded");
    assert_eq!(report["aw"], "not_admitted");
    assert_eq!(report["sources"].as_array().unwrap().len(), 4);
    let result = run("remove-native-source", &["--id", "private-id"]);
    assert!(result.status.success());
    for id in ["user", "project", "local"] {
        assert!(run("remove-native-source", &["--id", id]).status.success());
    }
    let config = Validator::new()
        .unwrap()
        .parse(&fs::read(&fixture.aw).unwrap())
        .unwrap();
    assert!(config.as_value()["spec"]["agents"]["qoder"]
        .get("native_config")
        .is_none());
    assert_eq!(snapshot, fixture.snapshot());
}

#[test]
fn native_loader_receives_original_context_and_runs_once_with_or_without_descriptors() {
    let fixture = Fixture::new();
    let mut document = fixture.document();
    let references = document["spec"]["agents"]["qoder"]["native_config"]["sources"]
        .as_array_mut()
        .unwrap();
    references.extend([
        json!({"id":"env","kind":"environment","scope":"process","reference":"NATIVE_MODEL"}),
        json!({"id":"argv","kind":"argument","scope":"process","reference":"--model"}),
    ]);
    let config = Validator::new()
        .unwrap()
        .parse(&serde_json::to_vec(&document).unwrap())
        .unwrap();
    let environment = BTreeMap::from([("NATIVE_MODEL", "private-environment-value")]);
    for argv in [vec![], vec!["--model", "private-argument-value"]] {
        let expected = native_load_with(
            &config.native_sources("qoder").unwrap(),
            &environment,
            &argv,
        )
        .unwrap();
        let calls = std::cell::Cell::new(0);
        let loaded = NativeConfiguration::new(&config, "qoder")
            .unwrap()
            .load(|sources| {
                calls.set(calls.get() + 1);
                Ok::<_, String>(NativeLoad {
                    value: native_load_with(sources, &environment, &argv)?,
                    sources: sources
                        .iter()
                        .filter(|source| source.kind != SourceKind::Argument || !argv.is_empty())
                        .cloned()
                        .collect(),
                })
            })
            .unwrap();
        assert_eq!(calls.get(), 1);
        assert_eq!(
            loaded.report().sources.len(),
            if argv.is_empty() { 4 } else { 5 }
        );
        assert_eq!(loaded.native(), &expected);
        assert_eq!(
            expected["model"],
            if argv.is_empty() {
                "private-environment-value"
            } else {
                "private-argument-value"
            }
        );
        assert!(!serde_json::to_string(&loaded.report())
            .unwrap()
            .contains("private"));
    }
    document["spec"]["agents"]["qoder"]
        .as_object_mut()
        .unwrap()
        .remove("native_config");
    let config = Validator::new()
        .unwrap()
        .parse(&serde_json::to_vec(&document).unwrap())
        .unwrap();
    let loaded = NativeConfiguration::new(&config, "qoder")
        .unwrap()
        .load(|sources| {
            assert!(sources.is_empty());
            // Native default discovery remains available to the embedding loader.
            let defaults = fixture
                .sources
                .iter()
                .cloned()
                .map(serde_json::from_value)
                .collect::<Result<Vec<NativeSource>, _>>()
                .unwrap();
            native_loader(&defaults)
        })
        .unwrap();
    assert_eq!(loaded.report().sources.len(), 3);
    assert_eq!(loaded.into_native()["model"], "local");
}
