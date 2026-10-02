//! Configuration patterns and daemon request scope must agree without trusting registration.

use asc_action_runtime::{CapabilityExecutor as _, ExecutionControl};
use asc_action_types::{SkillSecCommand, SkillSecRequest};
use asc_capability_skill_sec::executor::{SkillEnvironment, SkillSecExecutor};
use asc_capability_skill_sec::scanner::ScannerRegistry;
use asc_capability_skill_sec::{
    ManagedSkillDir, SkillIdentity, SkillRoot, SkillSecConfig, SkillSecError, SkillSecService,
};
use serde_json::{Value, json};
use std::fs;
use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

fn identity(path: impl AsRef<Path>) -> SkillIdentity {
    SkillIdentity::new(path).unwrap()
}
fn skill(path: impl AsRef<Path>) -> PathBuf {
    fs::create_dir_all(&path).unwrap();
    fs::write(path.as_ref().join("SKILL.md"), "Safe fixture").unwrap();
    path.as_ref().into()
}
fn service(base: &Path, patterns: &[PathBuf]) -> Arc<SkillSecService> {
    let state = base.join("state");
    fs::create_dir_all(&state).unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
    Arc::new(
        SkillSecService::new(
            SkillSecConfig {
                state_dir: state,
                managed_skill_dirs: patterns
                    .iter()
                    .map(|path| ManagedSkillDir::new(path).unwrap())
                    .collect(),
            },
            ScannerRegistry::default(),
        )
        .unwrap(),
    )
}
fn run(executor: &SkillSecExecutor, command: Value) -> asc_action_types::ActionOutcome {
    executor.execute(
        &ExecutionControl {
            deadline: Instant::now() + Duration::from_secs(10),
            cancelled: false,
        },
        &SkillSecRequest {
            command: serde_json::from_value(command).unwrap(),
            caller_uid: 1001,
        },
    )
}

#[test]
fn patterns_round_trip_and_match_components_without_home_or_general_globs() {
    for (pattern, accepted, rejected) in [
        (
            "/skills/a",
            vec!["/skills/a"],
            vec!["/skills", "/skills/a/child", "/skills/ab"],
        ),
        (
            "/skills/*",
            vec!["/skills/a"],
            vec![
                "/skills",
                "/skills/a/child",
                "/skills/.hidden",
                "/skills-other/a",
            ],
        ),
        (
            "/skills/**",
            vec!["/skills", "/skills/a", "/skills/a/child"],
            vec![
                "/skills/.hidden/a",
                "/skills/a/.skill-meta/v1",
                "/skills-other/a",
            ],
        ),
    ] {
        let parsed: ManagedSkillDir = serde_json::from_value(json!(pattern)).unwrap();
        assert_eq!(serde_json::to_value(&parsed).unwrap(), pattern);
        for path in accepted {
            assert!(parsed.contains(&identity(path)), "{pattern}: {path}");
        }
        for path in rejected {
            assert!(!parsed.contains(&identity(path)), "{pattern}: {path}");
        }
    }
    for invalid in [
        "~/skills/*",
        "relative/**",
        "/skills/../*",
        "/skills//**",
        "/skills/*/child",
        "/skills/a?",
        "/skills/[ab]",
    ] {
        assert!(ManagedSkillDir::new(invalid).is_err(), "{invalid}");
    }
}

#[test]
fn discovery_preserves_v1_pattern_depth_hidden_filter_dedup_and_dynamic_children() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let parent = skill(base.join("skills"));
    let child = skill(parent.join("one"));
    let nested = skill(child.join("nested"));
    skill(parent.join(".hidden"));
    skill(child.join(".skill-meta/versions/v1"));
    let outside = skill(base.join("outside"));
    symlink(&outside, parent.join("link")).unwrap();
    for (patterns, expected) in [
        (vec![parent.clone()], vec![parent.clone()]),
        (vec![parent.join("*"), child.clone()], vec![child.clone()]),
        (
            vec![parent.join("**")],
            vec![parent.clone(), child.clone(), nested],
        ),
    ] {
        let executor = SkillSecExecutor::new(service(&base, &patterns));
        // Startup and RPC share discovery, including configured but unregistered Skills.
        let discovered = executor
            .discover(Instant::now() + Duration::from_secs(10))
            .unwrap();
        assert_eq!(
            discovered
                .iter()
                .map(|id| id.path().to_owned())
                .collect::<Vec<_>>(),
            expected
        );
        let outcome = run(&executor, json!({"command":"status", "verbose":true}));
        assert!(outcome.success, "{outcome:?}");
        let actual: Vec<_> = outcome.data["output"]["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| PathBuf::from(value["canonicalSkillDir"].as_str().unwrap()))
            .collect();
        assert_eq!(actual, expected);
        assert_eq!(
            outcome.data["output"]["config"]["managedSkillDirPatterns"],
            patterns.len()
        );
    }
    let executor = SkillSecExecutor::new(service(&base, &[parent.join("*")]));
    skill(parent.join("two"));
    let outcome = run(&executor, json!({"command":"check", "all":true}));
    assert!(outcome.success, "{outcome:?}");
    assert_eq!(
        outcome.data["output"]["results"].as_array().unwrap().len(),
        2
    );
    assert!(!base.join("state/signing-key.pk8").exists());
}

struct NeverResolve(AtomicUsize);
impl SkillEnvironment for NeverResolve {
    fn resolve(&self, _: &SkillIdentity, _: Instant) -> Result<SkillRoot, SkillSecError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(SkillSecError::Invalid(
            "must reject before resolution".into(),
        ))
    }
}

#[test]
fn all_explicit_business_commands_reject_outside_scope_before_resolution_or_side_effects() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let allowed = skill(base.join("allowed"));
    let outside = skill(base.join("outside"));
    let output = base.join("export");
    let service = service(&base, std::slice::from_ref(&allowed));
    let environment = Arc::new(NeverResolve(AtomicUsize::new(0)));
    let executor = SkillSecExecutor::new(service).with_environment(environment.clone());
    for command in [
        json!({"command":"scan","skillDir":outside}),
        json!({"command":"certify","skillDir":outside,"scanner":"fixture","findings":[]}),
        json!({"command":"analyze","skillDir":outside}),
        json!({"command":"check","skillDir":outside}),
        json!({"command":"audit","skillDir":outside}),
        json!({"command":"show","skillDir":outside}),
        json!({"command":"decide","skillDir":outside,"action":"allow"}),
        json!({"command":"decide","skillDir":outside,"action":"rollback","version":"v000001"}),
        json!({"command":"decide","skillDir":outside,"clear":true}),
        json!({"command":"activate","skillDir":outside}),
        json!({"command":"export","skillDir":outside,"version":"v000001","output":output}),
        json!({"command":"scan","all":true,"skillDirs":[allowed, outside]}),
        json!({"command":"check","all":true,"skillDirs":[outside]}),
        json!({"command":"init","baseline":true,"skillDirs":[outside]}),
    ] {
        let outcome = run(&executor, command.clone());
        assert_eq!(
            outcome.error_type, "PermissionDenied",
            "{command}: {outcome:?}"
        );
        assert_eq!(outcome.exit_code, 1);
    }
    assert_eq!(environment.0.load(Ordering::SeqCst), 0);
    assert!(!base.join("state/signing-key.pk8").exists());
    assert!(!base.join("state/managed-skills.json").exists());
    assert!(!outside.join(".skill-meta").exists());
    assert!(!allowed.join(".skill-meta").exists());
    assert!(!output.exists());
}

#[test]
fn stale_registration_does_not_authorize_requests_or_recovery_after_restart() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let old = skill(base.join("old"));
    let new = skill(base.join("new"));
    let first = SkillSecExecutor::new(service(&base, std::slice::from_ref(&old)));
    assert!(
        run(
            &first,
            json!({"command":"certify","skillDir":old,"scanner":"fixture","findings":[]})
        )
        .success
    );
    let before = fs::read(old.join(".skill-meta/activation.json")).unwrap();
    let restarted = service(&base, &[new]);
    assert_eq!(restarted.managed_skills().unwrap(), vec![identity(&old)]);
    let executor = SkillSecExecutor::new(restarted);
    assert_eq!(
        run(&executor, json!({"command":"show","skillDir":old})).error_type,
        "PermissionDenied"
    );
    let recovered = executor.execute(
        &ExecutionControl {
            deadline: Instant::now() + Duration::from_secs(10),
            cancelled: false,
        },
        &SkillSecRequest {
            command: SkillSecCommand::Reconcile {
                skill_dir: identity(&old),
            },
            caller_uid: 0,
        },
    );
    assert_eq!(recovered.error_type, "PermissionDenied");
    let status = run(&executor, json!({"command":"status","verbose":true}));
    assert!(status.success, "{status:?}");
    assert_eq!(status.data["output"]["skills"]["discovered"], 1);
    assert_eq!(
        fs::read(old.join(".skill-meta/activation.json")).unwrap(),
        before
    );
}

#[test]
fn wildcard_never_follows_a_symlink_to_an_unapproved_directory() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let parent = base.join("skills");
    fs::create_dir(&parent).unwrap();
    let outside = skill(base.join("outside"));
    symlink(&outside, parent.join("link")).unwrap();
    let executor = SkillSecExecutor::new(service(&base, &[parent.join("**")]));
    let outcome = run(
        &executor,
        json!({"command":"analyze","skillDir":parent.join("link")}),
    );
    assert!(!outcome.success, "{outcome:?}");
    assert!(!base.join("state/signing-key.pk8").exists());
    assert!(!outside.join(".skill-meta").exists());
}

#[test]
fn interrupted_rotation_exception_cannot_authorize_a_new_baseline() {
    for resume in [
        json!({"command":"rotate-keys"}),
        json!({"command":"init","forceKeys":true,"baseline":false}),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let old = skill(base.join("old"));
        let first = SkillSecExecutor::new(service(&base, std::slice::from_ref(&old)));
        assert!(
            run(
                &first,
                json!({"command":"certify","skillDir":old,"scanner":"fixture","findings":[]})
            )
            .success
        );
        let fingerprint =
            run(&first, json!({"command":"status"})).data["output"]["keys"]["fingerprint"].clone();
        let intent_path = base.join("state/key-rotation.json");
        let intent =
            serde_json::to_vec(&json!({"previous_fingerprint":fingerprint, "skills":[old]}))
                .unwrap();
        fs::write(&intent_path, &intent).unwrap();
        fs::set_permissions(&intent_path, fs::Permissions::from_mode(0o600)).unwrap();
        let key_path = base.join("state/signing-key.pk8");
        let key = fs::read(&key_path).unwrap();
        let activation_path = old.join(".skill-meta/activation.json");
        let activation = fs::read(&activation_path).unwrap();
        let narrowed = service(&base, &[base.join("new")]);
        let environment = Arc::new(NeverResolve(AtomicUsize::new(0)));
        let executor =
            SkillSecExecutor::new(narrowed.clone()).with_environment(environment.clone());
        for (uid, command) in [
            (1001, json!({"command":"rotate-keys"})),
            (
                1001,
                json!({"command":"init","forceKeys":true,"baseline":false}),
            ),
            (
                0,
                json!({"command":"init","forceKeys":true,"baseline":true}),
            ),
        ] {
            let outcome = executor.execute(
                &ExecutionControl {
                    deadline: Instant::now() + Duration::from_secs(10),
                    cancelled: false,
                },
                &SkillSecRequest {
                    command: serde_json::from_value(command).unwrap(),
                    caller_uid: uid,
                },
            );
            assert_eq!(outcome.error_type, "PermissionDenied", "{outcome:?}");
        }
        assert_eq!(environment.0.load(Ordering::SeqCst), 0);
        assert_eq!(fs::read(&intent_path).unwrap(), intent);
        assert_eq!(fs::read(&key_path).unwrap(), key);
        assert_eq!(fs::read(&activation_path).unwrap(), activation);
        let outcome = SkillSecExecutor::new(narrowed).execute(
            &ExecutionControl {
                deadline: Instant::now() + Duration::from_secs(10),
                cancelled: false,
            },
            &SkillSecRequest {
                command: serde_json::from_value(resume).unwrap(),
                caller_uid: 0,
            },
        );
        assert!(outcome.success, "{outcome:?}");
        assert!(!intent_path.exists());
        assert_ne!(fs::read(&key_path).unwrap(), key);
    }
}

#[test]
fn configured_unregistered_skill_is_shown_as_managed_without_registration() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let parent = base.join("skills");
    let root = skill(parent.join("new"));
    let service = service(&base, &[parent.join("*")]);
    service.initialize().unwrap();
    let executor = SkillSecExecutor::new(service.clone());
    let outcome = run(&executor, json!({"command":"show", "skillDir":root}));
    assert!(outcome.success, "{outcome:?}");
    assert_eq!(outcome.data["output"]["managed"], true);
    assert_eq!(outcome.data["output"]["latestStatus"], "none");
    assert!(service.managed_skills().unwrap().is_empty());
    assert!(!root.join(".skill-meta").exists());
}
