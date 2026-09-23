use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};

use cosh_gateway_contracts::{
    common::{BoundedName, BoundedOpaque, BoundedText, RuntimeSelector, TargetRef},
    ids::{InstallationId, RunId, TaskId},
};
use tempfile::TempDir;

use super::*;

fn target(identifier: &str) -> TargetRef {
    TargetRef {
        kind: BoundedName::new("local").unwrap(),
        authority: BoundedName::new("cosh").unwrap(),
        identifier: BoundedOpaque::new(identifier).unwrap(),
    }
}

fn executable(directory: &Path, name: &str, marker: &Path) -> PathBuf {
    let path = directory.join(name);
    fs::write(
        &path,
        format!(
            "#!/bin/sh\nprintf '%s|%s|%s|%s|%s' \"$*\" \"$UNTRUSTED_SECRET\" \"$COSH_SYSOM_ENDPOINT\" \"$COSH_SYSOM_VPC_PROXY_HOST\" \"$COSH_SYSOM_PROBE_TIMEOUT_MS\" > \"{}\"\nwhile read line; do :; done\n",
            marker.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}

fn workspace_observer(directory: &Path, marker: &Path) -> PathBuf {
    let path = directory.join("workspace-observer.sh");
    fs::write(
        &path,
        format!(
            "#!/bin/sh\nprintf '%s' \"$PWD\" > \"{}\"\nwhile read line; do :; done\n",
            marker.display()
        ),
    )
    .unwrap();
    path
}

fn admitted(
    root: &TempDir,
    core: &Path,
    script: &Path,
) -> (InstalledBrokeredCoreRuntimePortFactory, ScheduledRun) {
    admitted_with_profile(
        root,
        core,
        script,
        GatewayCapabilityProfile::task_only_v1(),
        GATEWAY_BROKERED_CORE_RUNTIME_PROFILE,
    )
}

fn admitted_with_profile(
    root: &TempDir,
    core: &Path,
    script: &Path,
    profile: GatewayCapabilityProfile,
    runtime_profile: &str,
) -> (InstalledBrokeredCoreRuntimePortFactory, ScheduledRun) {
    let workspace = root.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    let expected_target = profile.governed_target();
    let installation = InstallationId::new();
    let actors = LocalOsActorResolver::new(installation.clone(), 1000);
    let actor = actors.actor_ref().clone();
    let workspaces = TrustedWorkspaceResolver::new(expected_target.clone(), workspace).unwrap();
    let workspace_ref = workspaces.workspace_ref().clone();
    let mut factory = InstalledBrokeredCoreRuntimePortFactory::new(
        installation,
        actors,
        workspaces,
        core,
        BTreeMap::from([
            (OsString::from("HOME"), OsString::from("/tmp/test-home")),
            (
                OsString::from("COSH_SYSOM_ENDPOINT"),
                OsString::from("https://sysom.example.com:8443"),
            ),
            (
                OsString::from("COSH_SYSOM_VPC_PROXY_HOST"),
                OsString::from("probe.example.com:443"),
            ),
            (
                OsString::from("COSH_SYSOM_PROBE_TIMEOUT_MS"),
                OsString::from("1"),
            ),
            (
                OsString::from("UNTRUSTED_SECRET"),
                OsString::from("must-not-cross"),
            ),
        ]),
    )
    .unwrap();
    factory.test_script = Some(script.to_path_buf());
    let run = ScheduledRun {
        actor,
        task_id: TaskId::new(),
        run_id: RunId::new(),
        runtime: RuntimeSelector {
            runtime: BoundedName::new("core").unwrap(),
            profile: Some(BoundedName::new(runtime_profile).unwrap()),
        },
        intent: BoundedText::new("create a checkpoint").unwrap(),
        target: expected_target,
        workspace: workspace_ref.clone(),
        capability_profile: profile.identity(),
        launch: cosh_gateway_contracts::task::TaskLaunchSpecV1::new(
            BoundedText::new("create a checkpoint").unwrap(),
            cosh_gateway_contracts::task::TaskRuntime::Core,
            workspace_ref,
            cosh_gateway_contracts::task::CheckpointPolicy::Off,
            cosh_gateway_contracts::task::ApprovalPolicy::Interactive,
        ),
        lease_generation: 9,
    };
    (factory, run)
}

#[test]
fn factory_launches_only_the_exact_profile_with_filtered_environment() {
    let root = TempDir::new().unwrap();
    let marker = root.path().join("launch.marker");
    let script = executable(root.path(), "fake-core.sh", &marker);
    let core = root.path().join("cosh-core");
    symlink("/bin/sh", &core).unwrap();
    let (mut factory, run) = admitted(&root, &core, &script);
    let debug = format!("{factory:?}");
    assert!(debug.contains("HOME"));
    assert!(!debug.contains("UNTRUSTED_SECRET"));
    assert!(!debug.contains("must-not-cross"));

    let port = factory.create(&run).unwrap();
    for _ in 0..100 {
        if marker.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(
        fs::read_to_string(marker).unwrap(),
        "--headless --execution-profile gateway-brokered-v1||https://sysom.example.com:8443||"
    );
    drop(port);
}

#[test]
fn factory_rejects_runtime_profile_and_actor_substitution_before_launch() {
    let root = TempDir::new().unwrap();
    let marker = root.path().join("launch.marker");
    let script = executable(root.path(), "fake-core.sh", &marker);
    let core = root.path().join("cosh-core");
    symlink("/bin/sh", &core).unwrap();
    let (mut factory, mut run) = admitted(&root, &core, &script);
    run.runtime.profile = Some(BoundedName::new("legacy").unwrap());
    assert_eq!(
        factory.create(&run).err().unwrap().code.as_str(),
        "runtime_profile_invalid"
    );
    run.runtime.profile = Some(BoundedName::new(GATEWAY_BROKERED_CORE_RUNTIME_PROFILE).unwrap());
    run.capability_profile.manifest_digest = Digest::parse("b".repeat(64)).unwrap();
    assert_eq!(
        factory.create(&run).err().unwrap().code.as_str(),
        "runtime_profile_invalid"
    );
    run.capability_profile = GatewayCapabilityProfile::task_only_v1().identity();
    run.actor.actor_id = cosh_gateway_contracts::ids::ActorId::new();
    assert_eq!(
        factory.create(&run).err().unwrap().code.as_str(),
        "runtime_actor_invalid"
    );
    assert!(!marker.exists());
}

#[test]
fn checkpoint_profile_requires_the_exact_runtime_selector_and_manifest() {
    let root = TempDir::new().unwrap();
    let marker = root.path().join("launch.marker");
    let script = executable(root.path(), "fake-core.sh", &marker);
    let core = root.path().join("cosh-core");
    symlink("/bin/sh", &core).unwrap();
    let (_, mut run) = admitted(&root, &core, &script);
    let checkpoint = GatewayCapabilityProfile::workspace_checkpoint_v1();
    run.target = checkpoint.governed_target();
    run.capability_profile = checkpoint.identity();
    run.runtime.profile = Some(BoundedName::new(GATEWAY_CHECKPOINT_CORE_RUNTIME_PROFILE).unwrap());

    assert_eq!(
        admitted_execution_profile(&run).unwrap(),
        GATEWAY_CHECKPOINT_CORE_RUNTIME_PROFILE
    );
    run.runtime.profile = Some(BoundedName::new("gateway-brokered-checkpoint-v1").unwrap());
    assert!(admitted_execution_profile(&run).is_err());
    run.runtime.profile = Some(BoundedName::new(GATEWAY_CHECKPOINT_CORE_RUNTIME_PROFILE).unwrap());
    run.capability_profile.manifest_digest = Digest::parse("b".repeat(64)).unwrap();
    assert!(admitted_execution_profile(&run).is_err());
}

#[test]
fn factory_maps_the_checkpoint_selector_to_the_private_core_launch_profile() {
    let root = TempDir::new().unwrap();
    let marker = root.path().join("launch.marker");
    let script = executable(root.path(), "fake-core.sh", &marker);
    let core = root.path().join("cosh-core");
    symlink("/bin/sh", &core).unwrap();
    let (mut factory, run) = admitted_with_profile(
        &root,
        &core,
        &script,
        GatewayCapabilityProfile::workspace_checkpoint_v1(),
        GATEWAY_CHECKPOINT_CORE_RUNTIME_PROFILE,
    );

    let port = factory.create(&run).unwrap();
    for _ in 0..100 {
        if marker.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(
        fs::read_to_string(marker).unwrap(),
        "--headless --execution-profile gateway-brokered-checkpoint-v1||https://sysom.example.com:8443||"
    );
    drop(port);
}

#[test]
fn workspace_write_profile_requires_exact_selector_manifest_and_private_argv() {
    let root = TempDir::new().unwrap();
    let marker = root.path().join("launch.marker");
    let script = executable(root.path(), "fake-core.sh", &marker);
    let core = root.path().join("cosh-core");
    symlink("/bin/sh", &core).unwrap();
    let (mut factory, mut run) = admitted_with_profile(
        &root,
        &core,
        &script,
        GatewayCapabilityProfile::workspace_write_v1(),
        GATEWAY_WORKSPACE_WRITE_CORE_RUNTIME_PROFILE,
    );

    assert_eq!(
        admitted_execution_profile(&run).unwrap(),
        GATEWAY_WORKSPACE_WRITE_CORE_RUNTIME_PROFILE
    );
    run.runtime.profile = Some(BoundedName::new("gateway-brokered-workspace-write-v1").unwrap());
    assert!(admitted_execution_profile(&run).is_err());
    run.runtime.profile =
        Some(BoundedName::new(GATEWAY_WORKSPACE_WRITE_CORE_RUNTIME_PROFILE).unwrap());
    run.capability_profile.manifest_digest = Digest::parse("b".repeat(64)).unwrap();
    assert!(admitted_execution_profile(&run).is_err());
    run.capability_profile = GatewayCapabilityProfile::workspace_write_v1().identity();

    let port = factory.create(&run).unwrap();
    for _ in 0..100 {
        if marker.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(
        fs::read_to_string(marker).unwrap(),
        "--headless --execution-profile gateway-brokered-workspace-write-v1||https://sysom.example.com:8443||"
    );
    drop(port);
}

#[test]
fn factory_pins_the_canonical_core_before_a_symlink_retarget() {
    let root = TempDir::new().unwrap();
    let original_marker = root.path().join("original.marker");
    let replacement_marker = root.path().join("replacement.marker");
    let script = executable(root.path(), "fake-core.sh", &original_marker);
    let original = root.path().join("original-core");
    let replacement = root.path().join("replacement-core");
    fs::copy("/bin/sh", &original).unwrap();
    fs::copy("/bin/false", &replacement).unwrap();
    let configured = root.path().join("cosh-core");
    symlink(&original, &configured).unwrap();
    let (mut factory, run) = admitted(&root, &configured, &script);

    fs::remove_file(&configured).unwrap();
    symlink(&replacement, &configured).unwrap();
    let port = factory.create(&run).unwrap();
    for _ in 0..100 {
        if original_marker.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(original_marker.exists());
    assert!(!replacement_marker.exists());
    drop(port);
}

#[test]
fn factory_keeps_the_admitted_inode_across_canonical_replacement_and_runs() {
    let root = TempDir::new().unwrap();
    let marker = root.path().join("launch.marker");
    let script = executable(root.path(), "fake-core.sh", &marker);
    let core = root.path().join("cosh-core");
    fs::copy("/bin/sh", &core).unwrap();
    let (mut factory, run) = admitted(&root, &core, &script);

    let first = factory.create(&run).unwrap();
    for _ in 0..100 {
        if marker.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(marker.exists());
    drop(first);
    fs::remove_file(&marker).unwrap();

    fs::rename(&core, root.path().join("admitted-core")).unwrap();
    fs::copy("/bin/false", &core).unwrap();
    let second = factory.create(&run).unwrap();
    for _ in 0..100 {
        if marker.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        marker.exists(),
        "replacement executable redirected the second Run"
    );
    drop(second);
}

#[test]
fn factory_uses_the_admitted_workspace_inode_after_path_replacement() {
    let root = TempDir::new().unwrap();
    let marker = root.path().join("workspace.marker");
    let script = workspace_observer(root.path(), &marker);
    let core = root.path().join("cosh-core");
    fs::copy("/bin/sh", &core).unwrap();
    let (mut factory, run) = admitted(&root, &core, &script);
    let workspace = root.path().join("workspace");
    let admitted_workspace = root.path().join("admitted-workspace");
    fs::rename(&workspace, &admitted_workspace).unwrap();
    fs::create_dir(&workspace).unwrap();

    let port = factory.create(&run).unwrap();
    for _ in 0..100 {
        if marker.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(
        fs::read_to_string(marker).unwrap(),
        admitted_workspace.display().to_string()
    );
    drop(port);
}

#[test]
fn factory_rejects_a_non_core_configured_entry() {
    let root = TempDir::new().unwrap();
    let marker = root.path().join("launch.marker");
    let core = executable(root.path(), "not-core", &marker);
    let workspace = root.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    let installation = InstallationId::new();
    let actors = LocalOsActorResolver::new(installation.clone(), 1000);
    let workspaces = TrustedWorkspaceResolver::new(target("primary"), workspace).unwrap();

    assert!(InstalledBrokeredCoreRuntimePortFactory::new(
        installation,
        actors,
        workspaces,
        core,
        BTreeMap::new(),
    )
    .is_err());
}

#[test]
fn factory_launches_in_refreshed_workspace_without_restart() {
    let root = TempDir::new().unwrap();
    let script = root.path().join("writer.sh");
    fs::write(
        &script,
        "printf '%s' \"$PWD\" > artifact; while read line; do :; done\n",
    )
    .unwrap();
    let core = root.path().join("cosh-core");
    fs::copy("/bin/sh", &core).unwrap();
    let (mut factory, run) = admitted(&root, &core, &script);
    let binding = factory.workspaces.clone();
    let workspace = root.path().join("workspace");
    for _ in 0..2 {
        let old = root.path().join("workspace.rollback-tmp");
        fs::rename(&workspace, &old).unwrap();
        fs::create_dir(&workspace).unwrap();
        fs::remove_dir_all(&old).unwrap();
        binding.refresh_after_snapshot_switch().unwrap();
        let port = factory.create(&run).unwrap();
        let artifact = workspace.join("artifact");
        for _ in 0..100 {
            if artifact.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(
            fs::read_to_string(artifact).unwrap(),
            workspace.display().to_string()
        );
        drop(port);
    }
}
