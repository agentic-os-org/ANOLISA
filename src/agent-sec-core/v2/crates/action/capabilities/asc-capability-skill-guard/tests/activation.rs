use asc_capability_skill_guard::scanner::ScannerRegistry;
use asc_capability_skill_guard::{
    DecisionAction, GuardConfig, Manifest, SkillGuardService, SkillRoot,
};
use serde_json::{Value, json};
use std::fs;
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

const SKILL: &str = "---\nname: fixture\ndescription: Local fixture\n---\nUse local files.\n";

struct Fixture {
    _temp: tempfile::TempDir,
    root: SkillRoot,
    service: Arc<SkillGuardService>,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let state = base.join("state");
        fs::create_dir(&state).unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
        let skill = base.join("skill");
        fs::create_dir(&skill).unwrap();
        fs::write(skill.join("SKILL.md"), SKILL).unwrap();
        fs::write(skill.join("main.sh"), "echo safe\n").unwrap();
        let service = Arc::new(
            SkillGuardService::new(
                GuardConfig {
                    state_dir: state.clone(),
                    managed_skill_dirs: vec![],
                },
                ScannerRegistry::default(),
            )
            .unwrap(),
        );
        service.initialize().unwrap();
        Self {
            _temp: temp,
            root: SkillRoot::direct(skill).unwrap(),
            service,
        }
    }

    fn certify(&self, scanner: &str, findings: &Value) -> Value {
        self.service
            .certify(&self.root, scanner, Some("test-1"), findings, deadline())
            .unwrap()
    }

    fn meta(&self, name: &str) -> PathBuf {
        self.root.io_dir.join(".skill-meta").join(name)
    }
    fn manifest(&self) -> Manifest {
        serde_json::from_slice(&fs::read(self.meta("latest.json")).unwrap()).unwrap()
    }
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(120)
}

fn subset(actual: &Value, expected: &Value, context: &str, failures: &mut Vec<String>) {
    if let Some(fields) = expected.as_object() {
        for (key, value) in fields {
            subset(&actual[key], value, &format!("{context}.{key}"), failures);
        }
    } else if actual != expected {
        failures.push(format!("{context}: actual={actual}, expected={expected}"));
    }
}

#[test]
fn frozen_v1_activation_workflows_match() {
    // Isolate process-wide umask changes from the parallel test harness.
    if std::env::var_os("ASC_ACTIVATION_UMASK_CHILD").is_none() {
        for mask in ["0002", "0022"] {
            let output = std::process::Command::new("sh")
                .args(["-c", "umask \"$1\"; exec \"$2\" --exact frozen_v1_activation_workflows_match --nocapture", "skillguard", mask])
                .arg(std::env::current_exe().unwrap())
                .env("ASC_ACTIVATION_UMASK_CHILD", mask)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "activation fixture under umask {mask}: {}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        return;
    }
    let fixture: Value = serde_json::from_str(include_str!("fixtures/activation.json")).unwrap();
    let mut failures = Vec::new();
    for case in fixture["cases"].as_array().unwrap() {
        let f = Fixture::new();
        for (index, step) in case["steps"].as_array().unwrap().iter().enumerate() {
            let result = match step["op"].as_str().unwrap() {
                "write" => {
                    let path = f.root.io_dir.join(step["path"].as_str().unwrap());
                    fs::create_dir_all(path.parent().unwrap()).unwrap();
                    fs::write(path, step["content"].as_str().unwrap()).unwrap();
                    continue;
                }
                "certify" => f.service.certify(
                    &f.root,
                    step["scanner"].as_str().unwrap(),
                    Some("test-1"),
                    &step["findings"],
                    deadline(),
                ),
                "check" => f.service.check(&f.root, deadline()),
                "show" => f.service.show(&f.root, deadline()),
                "activate" => f.service.activate(&f.root, deadline()),
                "clear" => f.service.clear_decision(&f.root, deadline()),
                "decide" => f.service.decide(
                    &f.root,
                    serde_json::from_value(step["action"].clone()).unwrap(),
                    step["version"].as_str(),
                    Some("fixture review"),
                    deadline(),
                ),
                "export" => {
                    let output = f.root.io_dir.with_file_name(format!("export-{index}"));
                    fs::DirBuilder::new().mode(0o700).create(&output).unwrap();
                    f.service.export(
                        &f.root,
                        step["version"].as_str().unwrap(),
                        &output,
                        fs::metadata(&output).unwrap().uid(),
                        deadline(),
                    )
                }
                operation => panic!("unexpected operation {operation}"),
            };
            let context = format!("{} step {index} {}", case["name"], step["op"]);
            if step["expected"].get("executionError").is_some() {
                if result.is_ok() {
                    failures.push(format!("{context}: expected error, received {result:?}"));
                }
            } else {
                match result {
                    Ok(value) => subset(&value, &step["expected"], &context, &mut failures),
                    Err(error) => failures.push(format!("{context}: {error}")),
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn publication_matches_real_xattr_and_hides_blocked_skills() {
    let f = Fixture::new();
    let first = f.certify(
        "fixture",
        &json!([{"rule":"risk","level":"deny","message":"review"}]),
    );
    assert_eq!(first["activation"]["exposureState"], "pending");
    assert_eq!(first["activation"]["activationPending"], false);
    let pending = asc_capability_skill_guard::activation::PENDING_TARGET;
    assert!(f.root.io_dir.join(pending).join("SKILL.md").is_file());
    let block = f
        .service
        .decide(&f.root, DecisionAction::Block, None, None, deadline())
        .unwrap();
    assert_eq!(block["activation"]["exposureState"], "hidden");
    let contract = fs::read(f.meta("activation.json")).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&contract).unwrap(),
        json!({"schemaVersion":1,"target":null})
    );
    let directory = fs::File::open(&f.root.io_dir).unwrap();
    let mut xattr = vec![0; 4096];
    let length = rustix::fs::fgetxattr(
        &directory,
        asc_capability_skill_guard::activation::ACTIVATION_XATTR,
        &mut xattr,
    )
    .unwrap();
    assert_eq!(&xattr[..length], contract);
    assert!(!f.root.io_dir.join(pending).exists());
    let before = fs::read(f.meta("latest.json")).unwrap();
    f.service.show(&f.root, deadline()).unwrap();
    assert_eq!(fs::read(f.meta("latest.json")).unwrap(), before);
    assert_eq!(fs::read(f.meta("activation.json")).unwrap(), contract);
}

#[test]
fn rollback_retains_backup_and_restores_selected_snapshot() {
    let f = Fixture::new();
    f.certify("fixture", &json!([]));
    fs::write(f.root.io_dir.join("main.sh"), "echo changed\n").unwrap();
    f.certify("fixture", &json!([]));
    let result = f
        .service
        .rollback(&f.root, Some("v000001"), Some("review"), deadline())
        .unwrap();
    assert_eq!(result["versionId"], "v000003");
    assert_eq!(result["activation"]["activeVersionId"], "v000003");
    assert_eq!(
        fs::read_to_string(f.root.io_dir.join("main.sh")).unwrap(),
        "echo safe\n"
    );
    let backup = Path::new(result["rollbackBackup"].as_str().unwrap());
    assert_eq!(
        fs::read_to_string(backup.join("main.sh")).unwrap(),
        "echo changed\n"
    );
    assert_eq!(
        f.manifest()
            .user_decision
            .unwrap()
            .target_version_id
            .as_deref(),
        Some("v000001")
    );
}

#[test]
fn failed_rollback_commit_restores_original_content() {
    let f = Fixture::new();
    f.certify("fixture", &json!([]));
    fs::write(f.root.io_dir.join("main.sh"), "echo preserve\n").unwrap();
    f.certify("fixture", &json!([]));
    let before = fs::read(f.meta("latest.json")).unwrap();
    fs::set_permissions(f.meta("versions"), fs::Permissions::from_mode(0o555)).unwrap();
    let outcome = f
        .service
        .rollback(&f.root, Some("v000001"), None, deadline());
    fs::set_permissions(f.meta("versions"), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        outcome.is_err(),
        "run without DAC override, as required by the workspace test environment"
    );
    assert_eq!(
        fs::read_to_string(f.root.io_dir.join("main.sh")).unwrap(),
        "echo preserve\n"
    );
    assert_eq!(fs::read(f.meta("latest.json")).unwrap(), before);
}

#[test]
fn reconcile_repairs_complete_split_but_never_trusts_damaged_snapshot() {
    let f = Fixture::new();
    f.certify("fixture", &json!([]));
    fs::remove_file(f.meta("latest.json")).unwrap();
    let result = f.service.reconcile(&f.root, deadline()).unwrap();
    assert_eq!(result["repairedLatest"], true);
    assert_eq!(result["activation"]["activeVersionId"], "v000001");
    fs::write(f.meta("versions/v000001.snapshot/main.sh"), "tampered").unwrap();
    fs::remove_file(f.meta("latest.json")).unwrap();
    let result = f.service.reconcile(&f.root, deadline()).unwrap();
    assert_eq!(result["repairedLatest"], false);
    assert_eq!(result["activation"]["exposureState"], "pending");
    assert!(!f.meta("latest.json").exists());
}
