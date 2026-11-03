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
    service_with_defaults(base, patterns, &[])
}
fn service_with_defaults(
    base: &Path,
    patterns: &[PathBuf],
    defaults: &[PathBuf],
) -> Arc<SkillSecService> {
    let state = base.join("state");
    fs::create_dir_all(&state).unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
    Arc::new(
        SkillSecService::new(
            SkillSecConfig {
                default_skill_dirs: defaults
                    .iter()
                    .map(|p| ManagedSkillDir::new(p).unwrap())
                    .collect(),
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
    run_as(executor, command, 1001)
}
fn run_as(
    executor: &SkillSecExecutor,
    command: Value,
    caller_uid: u32,
) -> asc_action_types::ActionOutcome {
    executor.execute(
        &ExecutionControl {
            deadline: Instant::now() + Duration::from_secs(10),
            cancelled: false,
        },
        &SkillSecRequest {
            command: serde_json::from_value(command).unwrap(),
            caller_uid,
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
                .skills
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
fn removed_configuration_does_not_authorize_requests_or_recovery_after_restart() {
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
    assert!(!base.join("state/managed-skills.json").exists());
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
    assert!(!base.join("state/managed-skills.json").exists());
    assert!(!root.join(".skill-meta").exists());
}

#[test]
fn default_scope_authorizes_new_children_but_never_outside_or_symlinked_roots() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let defaults = base.join("home/.codex/skills");
    let executor = SkillSecExecutor::new(service_with_defaults(&base, &[], &[defaults.join("*")]));
    assert!(
        executor
            .discover(Instant::now() + Duration::from_secs(10))
            .unwrap()
            .skills
            .is_empty()
    );
    let child = skill(defaults.join("new"));
    assert!(run(&executor, json!({"command":"check","skillDir":child})).success);
    assert!(run(&executor, json!({"command":"scan","skillDir":child})).success);
    assert!(run(&executor, json!({"command":"show","skillDir":child})).success);
    let outside = skill(base.join("outside"));
    assert_eq!(
        run(&executor, json!({"command":"scan","skillDir":outside})).error_type,
        "PermissionDenied"
    );
    let link = defaults.join("link");
    symlink(&outside, &link).unwrap();
    assert!(!run(&executor, json!({"command":"scan","skillDir":link})).success);
    assert!(!outside.join(".skill-meta").exists());
    let disabled = SkillSecExecutor::new(service(&base, &[]));
    assert_eq!(
        run(&disabled, json!({"command":"show","skillDir":child})).error_type,
        "PermissionDenied"
    );
}

#[test]
fn partial_discovery_keeps_results_reports_failure_and_never_audits_pass() {
    use asc_action_runtime::AuditProjector as _;
    use asc_action_types::AuditProjection;
    use asc_capability_skill_sec::executor::SkillSecAuditProjector;
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let good = skill(base.join("good"));
    let blocked = base.join("PRIVATE_BROKEN_ROOT");
    fs::write(&blocked, "not a directory").unwrap();
    for mixed in [true, false] {
        let patterns = if mixed {
            vec![blocked.clone(), good.clone()]
        } else {
            vec![blocked.clone()]
        };
        let executor = SkillSecExecutor::new(service_with_defaults(&base, &[], &patterns));
        for command in [
            json!({"command":"check","all":true}),
            json!({"command":"status","verbose":true}),
            json!({"command":"scan","all":true}),
            json!({"command":"init","baseline":true}),
        ] {
            let outcome = run(&executor, command.clone());
            assert!(!outcome.success, "{command}: {outcome:?}");
            assert_eq!(outcome.error_type, "SkillLedgerError");
            assert_eq!(
                outcome.data["output"]["discoveryErrors"]
                    .as_array()
                    .unwrap()
                    .len(),
                1
            );
            assert_eq!(
                outcome.data["output"]["results"].as_array().unwrap().len(),
                usize::from(mixed)
            );
            if command["command"] == "status" {
                assert_eq!(outcome.data["output"]["skills"]["health"], "critical");
                assert!(outcome.data["output"]["keys"].is_object());
            }
            let AuditProjection::Completed {
                result, failure, ..
            } = SkillSecAuditProjector.project(
                &SkillSecRequest {
                    command: serde_json::from_value(command.clone()).unwrap(),
                    caller_uid: 1001,
                },
                &outcome,
            )
            else {
                panic!("expected completed projection")
            };
            assert_eq!(result["discoveryErrorCount"], 1);
            if command["command"] != "status" {
                assert_eq!(result["verdict"], "error");
            }
            assert!(failure.is_some());
            assert!(
                !serde_json::to_string(&result)
                    .unwrap()
                    .contains("PRIVATE_BROKEN_ROOT")
            );
        }
    }
    // Unsupported replacement is rejected before partial baseline discovery.
    let service = service_with_defaults(&base, &[], &[good, blocked]);
    let deadline = Instant::now() + Duration::from_secs(10);
    let before = service.key_status(deadline).unwrap();
    let executor = SkillSecExecutor::new(service.clone());
    let outcome = executor.execute(
        &ExecutionControl {
            deadline,
            cancelled: false,
        },
        &SkillSecRequest {
            command: serde_json::from_value(
                json!({"command":"init","baseline":true,"forceKeys":true}),
            )
            .unwrap(),
            caller_uid: 0,
        },
    );
    assert!(!outcome.success);
    assert_eq!(service.key_status(deadline).unwrap(), before);
}

#[test]
fn entirely_failed_discovery_does_not_create_keys() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let blocked = base.join("blocked");
    fs::write(&blocked, "file").unwrap();
    let executor = SkillSecExecutor::new(service_with_defaults(&base, &[], &[blocked]));
    for command in [
        json!({"command":"scan","all":true}),
        json!({"command":"init","baseline":true}),
    ] {
        let outcome = run(&executor, command);
        assert_eq!(outcome.error_type, "SkillLedgerError");
        assert!(!base.join("state/signing-key.pk8").exists());
    }
}

#[test]
fn default_budget_does_not_truncate_explicit_or_remembered_skills() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let parent = base.join("skills");
    let skills: Vec<_> = (0..1025)
        .map(|n| skill(parent.join(format!("skill-{n:04}"))))
        .collect();
    let pattern = parent.join("*");
    let discover = |service| {
        SkillSecExecutor::new(service)
            .discover(Instant::now() + Duration::from_secs(30))
            .unwrap()
    };
    let limited = discover(service_with_defaults(
        &base,
        &[],
        std::slice::from_ref(&pattern),
    ));
    assert_eq!(limited.skills.len(), 1024);
    assert_eq!(limited.errors.len(), 1);
    let explicit = discover(service_with_defaults(
        &base,
        std::slice::from_ref(&pattern),
        std::slice::from_ref(&pattern),
    ));
    assert_eq!(explicit.skills.len(), 1025);
    assert!(explicit.errors.is_empty());
    let remembered = discover(service_with_defaults(&base, &skills, &[pattern]));
    assert_eq!(remembered.skills.len(), 1025);
    assert!(remembered.errors.is_empty());
}

#[test]
fn completed_explicit_discovery_skips_covered_default_entry_budget() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let parent = base.join("home/.hermes");
    let root = skill(parent.join("skills/nested"));
    for n in 0..10_001 {
        fs::write(root.join(format!("entry-{n}")), "").unwrap();
    }
    let pattern = parent.join("skills/**");
    let discover = |explicit: &[PathBuf]| {
        SkillSecExecutor::new(service_with_defaults(
            &base,
            explicit,
            std::slice::from_ref(&pattern),
        ))
        .discover(Instant::now() + Duration::from_secs(30))
        .unwrap()
    };
    let limited = discover(&[]);
    assert_eq!(limited.errors.len(), 1);
    assert!(limited.errors[0].error.contains("directory limits"));
    let covered = discover(&[parent.join("**")]);
    assert_eq!(covered.skills, vec![identity(&root)]);
    assert!(covered.errors.is_empty(), "{:?}", covered.errors);
}

#[test]
fn narrower_or_hidden_explicit_scope_does_not_suppress_default_discovery() {
    for (explicit, default) in [
        ("home/.hermes", "home/.hermes/skills/**"),
        ("home/.hermes/*", "home/.hermes/skills/**"),
        ("home/**", "home/.hermes/skills/**"),
        ("home/.hermes/skills/*", "home/.hermes/skills/**"),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let root = skill(base.join("home/.hermes/skills/group/nested"));
        let discovery = SkillSecExecutor::new(service_with_defaults(
            &base,
            &[base.join(explicit)],
            &[base.join(default)],
        ))
        .discover(Instant::now() + Duration::from_secs(10))
        .unwrap();
        assert_eq!(discovery.skills, vec![identity(&root)], "{explicit}");
        assert!(discovery.errors.is_empty(), "{explicit}");
    }
}

#[test]
fn covered_default_roots_preserve_discovery_validation() {
    for kind in ["symlink-root", "symlink-parent", "file", "missing"] {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let parent = base.join("home/.hermes");
        let root = parent.join("skills/nested");
        fs::create_dir_all(&parent).unwrap();
        let outside = skill(base.join("outside"));
        if kind == "symlink-parent" {
            symlink(&outside, parent.join("skills")).unwrap();
        } else {
            fs::create_dir_all(parent.join("skills")).unwrap();
            match kind {
                "symlink-root" => symlink(&outside, &root).unwrap(),
                "file" => fs::write(&root, "file").unwrap(),
                _ => {}
            }
        }
        let discovery = SkillSecExecutor::new(service_with_defaults(
            &base,
            &[parent.join("**")],
            &[root.join("**")],
        ))
        .discover(Instant::now() + Duration::from_secs(10))
        .unwrap();
        assert!(discovery.skills.is_empty(), "{kind}");
        if kind == "missing" {
            assert!(discovery.errors.is_empty());
        } else {
            assert_eq!(discovery.errors.len(), 1, "{kind}");
            assert_eq!(discovery.errors[0].root, identity(&root), "{kind}");
        }
    }
}

#[test]
fn failed_explicit_discovery_preserves_default_discoveries_and_the_error() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt as _;

    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let parent = base.join("home/.hermes");
    let root = skill(parent.join("skills/nested"));
    fs::write(parent.join(OsString::from_vec(vec![0xff])), "").unwrap();
    let discovery = SkillSecExecutor::new(service_with_defaults(
        &base,
        &[parent.join("**")],
        &[parent.join("skills/**")],
    ))
    .discover(Instant::now() + Duration::from_secs(10))
    .unwrap();
    assert_eq!(discovery.skills, vec![identity(&root)]);
    assert_eq!(discovery.errors.len(), 1);
    assert_eq!(discovery.errors[0].root, identity(&parent));
    assert!(discovery.errors[0].error.contains("non-UTF-8"));
}

#[test]
fn startup_scan_is_internal_and_preserves_deny_verdict() {
    use asc_action_runtime::AuditProjector as _;
    use asc_action_types::AuditProjection;
    use asc_capability_skill_sec::executor::SkillSecAuditProjector;
    assert!(
        serde_json::from_value::<SkillSecCommand>(
            json!({"command":"startup-scan","skillDir":"/fixture"})
        )
        .is_err()
    );
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let root = skill(base.join("skill"));
    fs::write(root.join("id_rsa"), "private fixture").unwrap();
    let executor = SkillSecExecutor::new(service_with_defaults(
        &base,
        &[],
        std::slice::from_ref(&root),
    ));
    let request = SkillSecRequest {
        command: SkillSecCommand::StartupScan {
            skill_dir: identity(&root),
        },
        caller_uid: 0,
    };
    let outcome = executor.execute(
        &ExecutionControl {
            deadline: Instant::now() + Duration::from_secs(10),
            cancelled: false,
        },
        &request,
    );
    assert!(outcome.success, "{outcome:?}");
    assert_eq!(outcome.data["output"]["results"][0]["scanStatus"], "deny");
    let AuditProjection::Completed { result, .. } =
        SkillSecAuditProjector.project(&request, &outcome)
    else {
        panic!("expected completed projection")
    };
    assert_eq!(result["verdict"], "deny");
}

#[test]
fn rotation_requests_are_rejected_before_resolution_or_writes() {
    for initialized in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let root = skill(base.join("skill"));
        let service = service(&base, std::slice::from_ref(&root));
        if initialized {
            service.initialize().unwrap();
        }
        let key = fs::read(base.join("state/signing-key.pk8")).ok();
        let environment = Arc::new(NeverResolve(AtomicUsize::new(0)));
        let executor = SkillSecExecutor::new(service).with_environment(environment.clone());
        for uid in [0, 1001] {
            for command in [
                json!({"command":"rotate-keys"}),
                json!({"command":"init","forceKeys":true,"baseline":false}),
                json!({"command":"init","forceKeys":true,"baseline":true,"skillDirs":[root]}),
            ] {
                let outcome = run_as(&executor, command, uid);
                assert_eq!(outcome.error_type, "NotImplementedError", "{outcome:?}");
                assert_eq!(outcome.exit_code, 1);
                assert_eq!(fs::read(base.join("state/signing-key.pk8")).ok(), key);
            }
        }
        assert_eq!(environment.0.load(Ordering::SeqCst), 0);
        assert!(!root.join(".skill-meta").exists());
        assert!(!base.join("state/key-rotation.json").exists());
        assert!(!base.join("state/managed-skills.json").exists());
    }
}
