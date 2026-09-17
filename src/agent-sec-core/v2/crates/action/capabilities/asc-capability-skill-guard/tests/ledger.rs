use asc_capability_skill_guard::scanner::ScannerRegistry;
use asc_capability_skill_guard::{
    GuardConfig, GuardError, KeyStore, Manifest, ScanOptions, SkillGuardService, SkillIdentity,
    SkillRoot, hash_tree,
};
use serde_json::{Value, json};
use std::fs;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

const SKILL: &str = "---\nname: fixture\ndescription: Local fixture\n---\nUse local files.\n";

struct Fixture {
    _temp: tempfile::TempDir,
    state: PathBuf,
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
            state,
            root: SkillRoot::direct(skill).unwrap(),
            service,
        }
    }

    fn certify(&self, scanner: &str, findings: &Value) -> Value {
        self.service
            .certify(&self.root, scanner, Some("test-1"), findings, deadline())
            .unwrap()
    }

    fn check(&self) -> Value {
        self.service.check(&self.root, deadline()).unwrap()
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

#[test]
fn certify_accumulates_replaces_and_versions_changed_content() {
    let f = Fixture::new();
    let none = f.check();
    assert_eq!(none["status"], "none");
    assert!(none["versionId"].is_null());
    let first = f.certify("custom-one", &json!([]));
    assert_eq!(first["versionId"], "v000001");
    assert_eq!(first["scanStatus"], "pass");
    assert_eq!(first["newVersion"], true);
    f.certify(
        "custom-two",
        &json!([{"rule":"review","level":"deny","message":"review required"}]),
    );
    assert_eq!(f.check()["status"], "deny");
    let replaced = f.certify("custom-two", &json!([]));
    assert_eq!(replaced["newVersion"], false);
    assert_eq!(f.manifest().scans.len(), 2);
    assert_eq!(f.check()["status"], "pass");
    let parent_signature = f.manifest().signature.unwrap().value;
    fs::write(f.root.io_dir.join("main.sh"), "echo changed\n").unwrap();
    let drift = f.check();
    assert_eq!(drift["status"], "drifted");
    assert_eq!(drift["modified"], json!(["main.sh"]));
    assert_eq!(f.certify("custom-one", &json!([]))["versionId"], "v000002");
    let second = f.manifest();
    assert_eq!(second.previous_version_id.as_deref(), Some("v000001"));
    assert_eq!(
        second.previous_manifest_signature.as_deref(),
        Some(parent_signature.as_str())
    );
    assert_eq!(second.scans.len(), 1);
    assert_eq!(
        f.service.audit(&f.root, true, deadline()).unwrap()["valid"],
        true
    );
}

#[test]
fn scan_fills_missing_scanners_and_force_reuses_the_verified_version() {
    let f = Fixture::new();
    let code = ScanOptions {
        scanners: Some(vec!["code-scanner".into()]),
        force: false,
    };
    let first = f.service.scan(&f.root, &code, deadline()).unwrap();
    assert_eq!(first["scannersRun"], json!(["code-scanner"]));
    let fill = f
        .service
        .scan(&f.root, &ScanOptions::default(), deadline())
        .unwrap();
    assert_eq!(fill["scannersRun"], json!(["static-scanner"]));
    assert_eq!(fill["skippedScanners"], json!(["code-scanner"]));
    assert_eq!(fill["newVersion"], false);
    let before = fs::read(f.meta("latest.json")).unwrap();
    assert_eq!(
        f.service
            .scan(&f.root, &ScanOptions::default(), deadline())
            .unwrap()["status"],
        "noop"
    );
    assert_eq!(fs::read(f.meta("latest.json")).unwrap(), before);
    let forced = f
        .service
        .scan(
            &f.root,
            &ScanOptions {
                force: true,
                ..ScanOptions::default()
            },
            deadline(),
        )
        .unwrap();
    assert_eq!(forced["newVersion"], false);
    assert_eq!(
        forced["scannersRun"],
        json!(["code-scanner", "static-scanner"])
    );
    assert_eq!(
        f.service.audit(&f.root, true, deadline()).unwrap()["versions_checked"],
        1
    );
}

#[test]
fn import_only_scanner_cannot_establish_trust_through_scan() {
    let f = Fixture::new();
    assert!(
        f.service
            .scan(
                &f.root,
                &ScanOptions {
                    scanners: Some(vec!["skill-vetter".into()]),
                    force: false
                },
                deadline()
            )
            .is_err()
    );
    assert!(!f.meta("latest.json").exists());
    assert_eq!(f.certify("skill-vetter", &json!([]))["scanStatus"], "pass");
}

#[test]
fn check_withholds_tampered_metadata_and_rejects_stale_signed_latest() {
    let f = Fixture::new();
    f.certify("fixture", &json!([]));
    let old = fs::read(f.meta("latest.json")).unwrap();
    fs::write(f.root.io_dir.join("main.sh"), "echo new\n").unwrap();
    f.certify("fixture", &json!([]));
    fs::write(f.meta("latest.json"), &old).unwrap();
    let stale = f.check();
    assert_eq!(stale["status"], "tampered");
    assert!(stale["versionId"].is_null());
    let recovery = f.certify("fixture", &json!([]));
    assert_eq!(recovery["versionId"], "v000003");
    assert_eq!(recovery["auditEvents"][0]["type"], "tampered_recovered");
    let mut forged: Value =
        serde_json::from_slice(&fs::read(f.meta("latest.json")).unwrap()).unwrap();
    forged["createdAt"] = json!("attacker-selected-secret");
    fs::write(f.meta("latest.json"), serde_json::to_vec(&forged).unwrap()).unwrap();
    let result = f.check();
    assert_eq!(result["status"], "tampered");
    assert!(!result.to_string().contains("attacker-selected-secret"));
}

#[test]
fn snapshot_damage_affects_audit_and_reuse_but_not_check() {
    let f = Fixture::new();
    f.certify("fixture", &json!([]));
    fs::write(f.meta("versions/v000001.snapshot/main.sh"), "tampered").unwrap();
    assert_eq!(f.check()["status"], "pass");
    assert_eq!(
        f.service.audit(&f.root, false, deadline()).unwrap()["valid"],
        true
    );
    assert_eq!(
        f.service.audit(&f.root, true, deadline()).unwrap()["valid"],
        false
    );
    assert_eq!(f.certify("fixture", &json!([]))["versionId"], "v000002");
    assert!(f.manifest().previous_version_id.is_none());
}

#[test]
fn orphan_slots_are_reserved_without_trusting_high_version_names() {
    let f = Fixture::new();
    fs::create_dir_all(f.meta("versions/v000001.snapshot")).unwrap();
    fs::write(f.meta("versions/v999999.json"), "{}").unwrap();
    assert_eq!(f.check()["status"], "tampered");
    let recovery = f.certify("fixture", &json!([]));
    assert_eq!(recovery["versionId"], "v000002");
    assert_eq!(recovery["auditEvents"][0]["fromStatus"], "tampered");
    assert!(f.meta("versions/v000001.snapshot").is_dir());
    assert_eq!(fs::read(f.meta("versions/v999999.json")).unwrap(), b"{}");
}

#[test]
fn parallel_certifiers_and_canonical_aliases_share_one_write_boundary() {
    let f = Fixture::new();
    let barrier = Arc::new(Barrier::new(12));
    let handles: Vec<_> = (0..12)
        .map(|index| {
            let service = Arc::clone(&f.service);
            let barrier = Arc::clone(&barrier);
            let root = SkillRoot::resolved(f.root.identity.clone(), f.root.io_dir.clone()).unwrap();
            std::thread::spawn(move || {
                barrier.wait();
                service
                    .certify(
                        &root,
                        &format!("scanner-{index}"),
                        None,
                        &json!([]),
                        deadline(),
                    )
                    .unwrap()
            })
        })
        .collect();
    for handle in handles {
        assert_eq!(handle.join().unwrap()["versionId"], "v000001");
    }
    assert_eq!(f.manifest().scans.len(), 12);
    assert_eq!(
        f.service.audit(&f.root, true, deadline()).unwrap()["valid"],
        true
    );
}

#[test]
fn exact_registration_persists_without_registering_siblings() {
    let f = Fixture::new();
    f.certify("fixture", &json!([]));
    let sibling = f.root.io_dir.with_file_name("sibling");
    fs::create_dir(&sibling).unwrap();
    fs::write(sibling.join("SKILL.md"), SKILL).unwrap();
    let restarted = SkillGuardService::new(
        GuardConfig {
            state_dir: f.state.clone(),
            managed_skill_dirs: vec![],
        },
        ScannerRegistry::default(),
    )
    .unwrap();
    assert_eq!(restarted.managed_skills().unwrap(), vec![f.root.identity]);
    assert!(!sibling.join(".skill-meta").exists());
}

#[test]
fn identical_leaf_names_have_distinct_signed_identity() {
    let f = Fixture::new();
    f.certify("fixture", &json!([]));
    let alias = SkillRoot::resolved(
        SkillIdentity::new("/srv/different/skill").unwrap(),
        f.root.io_dir.clone(),
    )
    .unwrap();
    assert_eq!(
        f.service.check(&alias, deadline()).unwrap()["status"],
        "tampered"
    );
}

#[test]
fn snapshots_skip_links_but_scan_preserves_original_link_findings() {
    let f = Fixture::new();
    symlink("main.sh", f.root.io_dir.join("inside")).unwrap();
    symlink(
        "/definitely-missing-synthetic",
        f.root.io_dir.join("outside"),
    )
    .unwrap();
    fs::set_permissions(
        f.root.io_dir.join("main.sh"),
        fs::Permissions::from_mode(0o6755),
    )
    .unwrap();
    f.service
        .scan(&f.root, &ScanOptions::default(), deadline())
        .unwrap();
    let manifest = f.manifest();
    let findings: Vec<_> = manifest.scans.iter().flat_map(|s| &s.findings).collect();
    assert!(findings.iter().any(|f| f.file.as_deref() == Some("inside")));
    assert!(
        findings
            .iter()
            .any(|f| f.file.as_deref() == Some("outside"))
    );
    assert!(!manifest.file_hashes.contains_key("inside"));
    assert!(!f.meta("versions/v000001.snapshot/inside").exists());
    let mode = fs::metadata(f.meta("versions/v000001.snapshot/main.sh"))
        .unwrap()
        .mode();
    assert_eq!(mode & 0o6000, 0);
    assert_eq!(mode & 0o111, 0o111);
    assert_eq!(
        hash_tree(&f.meta("versions/v000001.snapshot"), true).unwrap(),
        manifest.file_hashes
    );
}

#[test]
fn export_authenticates_snapshot_and_only_writes_to_safe_caller_output() {
    let f = Fixture::new();
    f.certify("fixture", &json!([]));
    let output = f.state.with_file_name("export");
    fs::create_dir(&output).unwrap();
    fs::set_permissions(&output, fs::Permissions::from_mode(0o700)).unwrap();
    let uid = rustix::process::geteuid().as_raw();
    let result = f
        .service
        .export(&f.root, "latest", &output, uid, deadline())
        .unwrap();
    assert_eq!(result["versionId"], "v000001");
    assert_eq!(
        hash_tree(&output.join("snapshot"), true).unwrap(),
        f.manifest().file_hashes
    );
    let exported: Manifest =
        serde_json::from_slice(&fs::read(output.join("manifest.json")).unwrap()).unwrap();
    KeyStore::open(&f.state)
        .unwrap()
        .load()
        .unwrap()
        .verify_manifest(&exported, &f.root.identity)
        .unwrap();
    assert!(
        f.service
            .export(&f.root, "latest", &output, uid, deadline())
            .is_err()
    );
    let second = output.with_file_name("other-output");
    fs::create_dir(&second).unwrap();
    assert!(
        f.service
            .export(&f.root, "latest", &second, uid.wrapping_add(1), deadline())
            .is_err()
    );
    fs::write(f.meta("versions/v000001.snapshot/main.sh"), "changed").unwrap();
    assert!(
        f.service
            .export(&f.root, "v000001", &second, uid, deadline())
            .is_err()
    );
    assert!(fs::read_dir(second).unwrap().next().is_none());
}

#[test]
fn metadata_symlinks_hardlinks_and_expired_requests_do_not_mutate_targets() {
    let f = Fixture::new();
    let outside = f.state.with_file_name("outside.json");
    fs::write(&outside, "untouched").unwrap();
    fs::create_dir_all(f.meta("versions")).unwrap();
    symlink(&outside, f.meta("latest.json")).unwrap();
    assert_eq!(f.check()["status"], "tampered");
    f.certify("fixture", &json!([]));
    assert_eq!(fs::read(&outside).unwrap(), b"untouched");
    let before = fs::read(f.meta("latest.json")).unwrap();
    let expired = Instant::now().checked_sub(Duration::from_secs(1)).unwrap();
    assert!(matches!(
        f.service
            .certify(&f.root, "fixture", None, &json!([]), expired),
        Err(GuardError::Timeout)
    ));
    assert_eq!(fs::read(f.meta("latest.json")).unwrap(), before);
    fs::remove_file(f.meta("latest.json")).unwrap();
    fs::hard_link(&outside, f.meta("latest.json")).unwrap();
    assert_eq!(f.check()["status"], "tampered");
}

#[test]
fn obsolete_keys_are_not_used_to_authenticate_history() {
    let f = Fixture::new();
    f.certify("fixture", &json!([]));
    fs::rename(f.state.join("signing-key.pk8"), f.state.join("old-key.pk8")).unwrap();
    f.service.initialize().unwrap();
    assert_eq!(f.check()["status"], "tampered");
    let rebuilt = f.certify("fixture", &json!([]));
    assert_eq!(rebuilt["versionId"], "v000002");
    assert!(f.manifest().previous_version_id.is_none());
}

#[test]
fn missing_versions_directory_cannot_hide_existing_latest_record() {
    let f = Fixture::new();
    f.certify("fixture", &json!([]));
    fs::remove_dir_all(f.meta("versions")).unwrap();
    assert_eq!(f.check()["status"], "tampered");
}

fn write_relative(root: &Path, path: &str, content: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

#[test]
fn frozen_v1_lifecycle_contracts_match() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/ledger.json")).unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        let f = Fixture::new();
        for (index, step) in case["steps"].as_array().unwrap().iter().enumerate() {
            let result = match step["op"].as_str().unwrap() {
                "write" => {
                    write_relative(
                        &f.root.io_dir,
                        step["path"].as_str().unwrap(),
                        step["content"].as_str().unwrap(),
                    );
                    continue;
                }
                "remove" => {
                    let path = f.root.io_dir.join(step["path"].as_str().unwrap());
                    if path.is_dir() {
                        fs::remove_dir_all(path).unwrap();
                    } else {
                        fs::remove_file(path).unwrap();
                    }
                    continue;
                }
                "mkdir" => {
                    fs::create_dir_all(f.root.io_dir.join(step["path"].as_str().unwrap())).unwrap();
                    continue;
                }
                "scan" => f.service.scan(
                    &f.root,
                    &ScanOptions {
                        scanners: step
                            .get("scanners")
                            .map(|v| serde_json::from_value(v.clone()).unwrap()),
                        force: step["force"].as_bool().unwrap_or(false),
                    },
                    deadline(),
                ),
                "certify" => f.service.certify(
                    &f.root,
                    step["scanner"].as_str().unwrap(),
                    Some("test-1"),
                    &step["findings"],
                    deadline(),
                ),
                "check" => f.service.check(&f.root, deadline()),
                "audit" => f.service.audit(
                    &f.root,
                    step["snapshots"].as_bool().unwrap_or(false),
                    deadline(),
                ),
                unknown => panic!("unknown fixture operation {unknown}"),
            };
            if step["expected"].get("executionError").is_some() {
                assert!(result.is_err(), "{} step {index}", case["name"]);
                continue;
            }
            let result = result.unwrap();
            for (field, expected) in step["expected"].as_object().unwrap() {
                assert_eq!(
                    &result[field], expected,
                    "{} step {index} field {field}",
                    case["name"]
                );
            }
        }
    }
}

#[test]
fn resolved_directory_replacement_is_rejected_before_ledger_access() {
    use std::os::unix::fs::MetadataExt as _;
    let f = Fixture::new();
    f.certify("fixture", &json!([]));
    let metadata = fs::metadata(&f.root.io_dir).unwrap();
    let pinned = SkillRoot::resolved(f.root.identity.clone(), f.root.io_dir.clone())
        .unwrap()
        .with_file_identity(metadata.dev(), metadata.ino())
        .unwrap();
    assert_eq!(
        f.service.check(&pinned, deadline()).unwrap()["status"],
        "pass"
    );
    let original = f.root.io_dir.with_extension("original");
    fs::rename(&f.root.io_dir, &original).unwrap();
    fs::create_dir(&f.root.io_dir).unwrap();
    fs::write(f.root.io_dir.join("SKILL.md"), "replacement").unwrap();
    assert!(f.service.check(&pinned, deadline()).is_err());
    assert!(
        f.service
            .scan(&pinned, &ScanOptions::default(), deadline())
            .is_err()
    );
    assert!(!f.root.io_dir.join(".skill-meta").exists());
}
