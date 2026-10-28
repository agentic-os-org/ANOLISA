use std::fs::{self, OpenOptions};
use std::io;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, symlink};
use std::sync::Arc;

use agentsight_enforcement_protocol::{
    FILE_PROTECTION_POLICY_VERSION, FileProtectionExclusion, FileProtectionPolicy,
    FileProtectionTarget, FileProtectionTopologyMode, FileProtections,
};

use super::super::file_protection_manifest::tests::{FakeSource, Fixture, mount, snapshot};
use super::super::file_protection_policy::normalize;
use super::super::read_process_start_time;
use super::*;

fn target(path: &str, kind: FileProtectionTargetKind) -> FileProtectionTarget {
    FileProtectionTarget {
        path: path.into(),
        kind,
        protections: FileProtections {
            delete: true,
            ordinary_write: false,
        },
        exclusions: Vec::new(),
    }
}

fn normalized(targets: Vec<FileProtectionTarget>) -> NormalizedFileProtectionPolicy {
    normalize(&FileProtectionPolicy {
        version: FILE_PROTECTION_POLICY_VERSION,
        topology_mode: FileProtectionTopologyMode::StaticAtApply,
        targets,
    })
    .expect("normalize fixture policy")
}

fn prepare_fixture(
    fixture: &Fixture,
    policy: &NormalizedFileProtectionPolicy,
    mounts: Vec<MountInfo>,
) -> Result<PreparedFileProtectionTopology, BackendError> {
    let stable = snapshot(77, 9, mounts);
    prepare_from(
        policy,
        123,
        77,
        Arc::new(FakeSource::new(&fixture.root, vec![stable.clone(), stable])),
    )
}

#[test]
fn current_process_prepare_holds_real_proc_topology_and_future_exclusion() {
    let fixture = Fixture::new("fp-topology");
    let proc_root = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_CLOEXEC | libc::O_DIRECTORY)
        .open("/proc/self/root")
        .expect("open current process root");
    match super::super::file_protection_resolver::open_held(
        &proc_root,
        fixture.root.to_str().expect("UTF-8 fixture path"),
    ) {
        Err(error) if error.raw_errno() == Some(libc::ENOSYS) => {
            eprintln!("skipping: host kernel does not provide openat2");
            return;
        }
        Err(error) => panic!("openat2 preflight failed: {error}"),
        Ok(_) => {}
    }

    let target_path = fixture.root.to_str().expect("UTF-8 fixture path");
    let future_path = fixture.root.join("future");
    let mut tree = target(target_path, FileProtectionTargetKind::DirectoryTree);
    tree.exclusions.push(FileProtectionExclusion {
        path: future_path.to_str().expect("UTF-8 exclusion path").into(),
        kind: FileProtectionExclusionKind::ExactEntry,
    });
    let policy = normalized(vec![tree]);
    let pid = i32::try_from(std::process::id()).expect("current PID fits i32");
    let start_time = read_process_start_time(pid).expect("read current process start time");

    let prepared = prepare(&policy, pid, start_time).expect("prepare current process topology");

    assert_eq!(prepared.direct_targets.len(), 1);
    let root = &prepared.direct_targets[0];
    assert_eq!(
        root.path.identity.ino,
        fs::metadata(&fixture.root).unwrap().ino()
    );
    assert_ne!(root.path.identity.mnt_id, 0);
    assert_eq!(root.target_id, 1);

    assert_eq!(prepared.direct_slots.len(), 1);
    assert_eq!(prepared.exclusions.len(), 1);
    let exclusion = &prepared.exclusions[0];
    assert_eq!(exclusion.slot.target_id, root.target_id);
    assert_eq!(exclusion.kind, FileProtectionExclusionKind::ExactEntry);
    assert_eq!(exclusion.slot.parent.authored_path, target_path);
    assert_eq!(exclusion.slot.parent.identity, root.path.identity);
    assert_eq!(exclusion.slot.name, "future");
    assert!(exclusion.existing.is_none());

    assert_eq!(
        prepared.capacities.roots,
        1 + prepared.existing_submounts.len()
            + prepared.underlying_submounts.len()
            + prepared.known_bind_aliases.len()
    );
    assert_eq!(prepared.capacities.directories, prepared.capacities.roots);
    assert_eq!(
        prepared.capacities.stable_paths,
        prepared.ancestor_slots.len()
    );
    assert_eq!(prepared.capacities.slots, 2);
    assert_eq!(prepared.capacities.targets, 1);
    assert_eq!(prepared.capacities.domains, 1);
}

#[test]
fn exact_file_and_directory_tree_require_the_authored_type() {
    let fixture = Fixture::new("fp-topology");
    fixture.write("/config");
    fixture.mkdir("/tree");
    let mounts = vec![mount(1, 0, "/", "/", "ext4")];
    let prepared = prepare_fixture(
        &fixture,
        &normalized(vec![
            target("/config", FileProtectionTargetKind::ExactFile),
            target("/tree", FileProtectionTargetKind::DirectoryTree),
        ]),
        mounts.clone(),
    )
    .expect("prepare existing targets");

    assert_eq!(prepared.direct_targets.len(), 2);
    assert_eq!(prepared.direct_slots.len(), 2);
    assert_eq!(prepared.capacities.targets, 2);
    assert_eq!(prepared.capacities.domains, 1);
    assert!(
        prepare_fixture(
            &fixture,
            &normalized(vec![target("/tree", FileProtectionTargetKind::ExactFile)]),
            mounts,
        )
        .is_err()
    );
}

#[test]
fn missing_target_and_direct_symlink_are_rejected() {
    let fixture = Fixture::new("fp-topology");
    fixture.write("/real");
    symlink("real", fixture.host("/link")).expect("create direct symlink");
    let mounts = vec![mount(1, 0, "/", "/", "ext4")];

    for path in ["/missing", "/link"] {
        let error = prepare_fixture(
            &fixture,
            &normalized(vec![target(path, FileProtectionTargetKind::ExactFile)]),
            mounts.clone(),
        )
        .expect_err("unsafe direct target must fail");
        assert!(matches!(error, BackendError::CompileFailure(_)));
    }
}

#[test]
fn missing_exclusion_final_is_held_by_parent_but_missing_intermediate_fails() {
    let fixture = Fixture::new("fp-topology");
    fixture.mkdir("/tree/existing");
    let root = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_CLOEXEC | libc::O_DIRECTORY)
        .open(&fixture.root)
        .expect("open fixture root");
    let missing = super::super::file_protection_resolver::open_held(&root, "/tree/existing/future")
        .expect_err("future exclusion does not exist yet");
    assert_eq!(missing.kind(), io::ErrorKind::NotFound);
    assert_eq!(missing.raw_errno(), Some(libc::ENOENT));

    let mut tree = target("/tree", FileProtectionTargetKind::DirectoryTree);
    tree.exclusions.push(FileProtectionExclusion {
        path: "/tree/existing/future".into(),
        kind: FileProtectionExclusionKind::ExactEntry,
    });
    let mounts = vec![mount(1, 0, "/", "/", "ext4")];
    let prepared = prepare_fixture(&fixture, &normalized(vec![tree]), mounts.clone())
        .expect("missing final component is a supported future slot");

    assert!(prepared.exclusions[0].existing.is_none());
    assert_eq!(prepared.exclusions[0].slot.name, "future");
    assert_eq!(
        prepared.exclusions[0].slot.parent.authored_path,
        "/tree/existing"
    );

    let mut invalid_tree = target("/tree", FileProtectionTargetKind::DirectoryTree);
    invalid_tree.exclusions.push(FileProtectionExclusion {
        path: "/tree/missing/future".into(),
        kind: FileProtectionExclusionKind::DirectorySubtree,
    });
    assert!(prepare_fixture(&fixture, &normalized(vec![invalid_tree]), mounts).is_err());
}

#[test]
fn exact_entry_exclusion_may_name_a_symlink_without_following_it() {
    let fixture = Fixture::new("fp-topology");
    fixture.mkdir("/tree");
    symlink("/outside", fixture.host("/tree/link")).expect("create exclusion symlink");
    let mut tree = target("/tree", FileProtectionTargetKind::DirectoryTree);
    tree.exclusions.push(FileProtectionExclusion {
        path: "/tree/link".into(),
        kind: FileProtectionExclusionKind::ExactEntry,
    });

    let prepared = prepare_fixture(
        &fixture,
        &normalized(vec![tree]),
        vec![mount(1, 0, "/", "/", "ext4")],
    )
    .expect("exact entry records the symlink itself");
    let entry = prepared.exclusions[0]
        .existing
        .as_ref()
        .expect("symlink entry exists");
    assert_eq!(
        entry.identity.mode & libc::S_IFMT as u16,
        libc::S_IFLNK as u16
    );
}

#[test]
fn topology_change_retries_but_persistent_change_fails() {
    let fixture = Fixture::new("fp-topology");
    fixture.write("/config");
    let policy = normalized(vec![target("/config", FileProtectionTargetKind::ExactFile)]);
    let a = snapshot(77, 1, vec![mount(1, 0, "/", "/", "ext4")]);
    let b = snapshot(77, 2, vec![mount(1, 0, "/", "/", "ext4")]);
    let retry = Arc::new(FakeSource::new(
        &fixture.root,
        vec![a.clone(), b.clone(), b.clone(), b.clone()],
    ));
    assert!(prepare_from(&policy, 123, 77, retry).is_ok());

    let changing = Arc::new(FakeSource::new(
        &fixture.root,
        vec![a.clone(), b.clone(), a, b],
    ));
    let error =
        prepare_from(&policy, 123, 77, changing).expect_err("two unstable snapshots must fail");
    assert!(error.to_string().contains("topology changed"));
}

#[test]
fn process_identity_and_namespace_changes_fail_closed() {
    let fixture = Fixture::new("fp-topology");
    fixture.write("/config");
    let policy = normalized(vec![target("/config", FileProtectionTargetKind::ExactFile)]);
    let stale = snapshot(78, 1, vec![mount(1, 0, "/", "/", "ext4")]);
    let source = Arc::new(FakeSource::new(&fixture.root, vec![stale]));
    assert!(matches!(
        prepare_from(&policy, 123, 77, source),
        Err(BackendError::StaleProcess { pid: 123 })
    ));

    let before = snapshot(77, 1, vec![mount(1, 0, "/", "/", "ext4")]);
    let mut after = before.clone();
    after.namespace = (1, 3);
    let source = Arc::new(FakeSource::new(
        &fixture.root,
        vec![before.clone(), after.clone(), before, after],
    ));
    assert!(prepare_from(&policy, 123, 77, source).is_err());
}

#[test]
fn capacity_count_overflow_is_reported() {
    let error = checked_capacity_counts(0, usize::MAX, 0, 1, 0)
        .expect_err("capacity arithmetic must be checked");
    assert!(error.to_string().contains("overflow"));
}
