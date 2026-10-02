//! Native Linux regression: run the ignored case with root and `CAP_SYS_ADMIN`.
//! Mounts exist only in a child mount namespace; the daemon itself needs no mount capability.

use asc_action_runtime::{CapabilityExecutor as _, ExecutionControl};
use asc_action_types::SkillSecRequest;
use asc_capability_skill_sec::{
    ManagedSkillDir, SkillSecConfig, SkillSecService, executor::SkillSecExecutor,
    scanner::ScannerRegistry,
};
use serde_json::{Value, json};
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn checked(command: &mut Command) {
    let result = command.output().unwrap();
    assert!(
        result.status.success(),
        "{command:?}: {}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

fn run(executor: &SkillSecExecutor, command: Value) -> asc_action_types::ActionOutcome {
    executor.execute(
        &ExecutionControl {
            deadline: Instant::now() + Duration::from_secs(30),
            cancelled: false,
        },
        &SkillSecRequest {
            command: serde_json::from_value(command).unwrap(),
            caller_uid: 1001,
        },
    )
}

fn bind_readonly(source: &Path, target: &Path) {
    checked(Command::new("mount").arg("--bind").arg(source).arg(target));
    checked(
        Command::new("mount")
            .args(["-o", "remount,bind,ro"])
            .arg(target),
    );
}

fn service(state: &Path, pattern: &Path) -> Arc<SkillSecService> {
    Arc::new(
        SkillSecService::new(
            SkillSecConfig {
                state_dir: state.to_owned(),
                managed_skill_dirs: vec![ManagedSkillDir::new(pattern).unwrap()],
            },
            ScannerRegistry::default(),
        )
        .unwrap(),
    )
}

#[test]
#[ignore = "requires native Linux root, mount and unshare; never changes host mounts"]
fn readonly_system_batches_skip_without_weakening_explicit_writes() {
    let namespace = fs::read_link("/proc/self/ns/mnt").unwrap();
    if let Some(parent_namespace) = std::env::var_os("ASC_TEST_READONLY_NAMESPACE") {
        assert_ne!(namespace.as_os_str(), parent_namespace.as_os_str());
    } else {
        checked(
            Command::new("unshare")
                .args(["--mount", "--propagation", "private"])
                .arg(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "readonly_system_batches_skip_without_weakening_explicit_writes",
                    "--ignored",
                    "--nocapture",
                ])
                .env("ASC_TEST_READONLY_NAMESPACE", namespace),
        );
        return;
    }
    assert_eq!(rustix::process::geteuid().as_raw(), 0);
    let temp = tempfile::tempdir().unwrap();
    let share = temp.path().join("share");
    fs::create_dir(&share).unwrap();
    checked(
        Command::new("mount")
            .arg("--bind")
            .arg(&share)
            .arg("/usr/share"),
    );
    let parent = Path::new("/usr/share/anolisa/skills");
    let readonly = parent.join("readonly");
    let writable = parent.join("writable");
    for root in [&readonly, &writable] {
        fs::create_dir_all(root).unwrap();
        fs::write(
            root.join("SKILL.md"),
            "---\nname: fixture\ndescription: fixture\n---\nSafe\n",
        )
        .unwrap();
    }
    // Mode bits alone must not skip a root-writable Skill.
    fs::set_permissions(&writable, fs::Permissions::from_mode(0o555)).unwrap();
    bind_readonly(&readonly, &readonly);
    assert_eq!(
        fs::write(readonly.join("probe"), "")
            .unwrap_err()
            .raw_os_error(),
        Some(rustix::io::Errno::ROFS.raw_os_error())
    );
    let state = temp.path().join("state");
    fs::create_dir(&state).unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
    let service = service(&state, &parent.join("*"));
    let executor = SkillSecExecutor::new(service.clone());
    for command in [
        json!({"command":"scan", "all":true}),
        json!({"command":"init", "baseline":true}),
    ] {
        let result = run(&executor, command);
        assert!(result.success, "{result:?}");
        assert_eq!(result.exit_code, 0);
        let results = result.data["output"]["results"].as_array().unwrap();
        let skipped = results
            .iter()
            .find(|value| value["skillName"] == "readonly")
            .unwrap();
        assert_eq!(skipped["status"], "skipped");
        assert_eq!(skipped["reasonCode"], "readonly_system_skill");
        assert_eq!(skipped["persisted"], false);
        assert!(!readonly.join(".skill-meta").exists());
        assert!(writable.join(".skill-meta/latest.json").is_file());
    }
    assert_eq!(service.managed_skills().unwrap().len(), 1);
    assert!(!run(&executor, json!({"command":"scan", "skillDir":readonly})).success);
    // The same EROFS storage outside the default location remains a batch error.
    let ordinary = share.join("anolisa/skills/readonly");
    bind_readonly(&readonly, &ordinary);
    let strict = SkillSecExecutor::new(self::service(&state, &ordinary));
    assert!(!run(&strict, json!({"command":"scan", "all":true})).success);
    assert_eq!(
        run(&strict, json!({"command":"scan", "skillDir":readonly})).error_type,
        "PermissionDenied"
    );
    checked(Command::new("umount").arg(&ordinary));
    checked(Command::new("umount").arg(&readonly));
    checked(Command::new("umount").arg("/usr/share"));
}
