use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use agentsight_enforcement_protocol::{
    FILE_PROTECTION_POLICY_VERSION, FileProtectionExclusion, FileProtectionPolicy,
    FileProtectionTarget, FileProtectionTopologyMode, FileProtections,
};
use uuid::Uuid;

use super::super::file_protection_mountinfo::{
    MAX_MOUNTINFO_LINE_BYTES, parse_mountinfo, visible_paths_for_mount_root,
};
use super::super::file_protection_policy::normalize;
use super::super::read_process_start_time;
use super::*;

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = PathBuf::from("/tmp").join(format!("fp-topology-{}", Uuid::new_v4().simple()));
        fs::create_dir(&root).expect("create fixture root");
        Self { root }
    }

    fn mkdir(&self, path: &str) {
        fs::create_dir_all(self.host(path)).expect("create fixture directory");
    }

    fn write(&self, path: &str) {
        if let Some(parent) = self.host(path).parent() {
            fs::create_dir_all(parent).expect("create fixture parent");
        }
        fs::write(self.host(path), b"fixture").expect("write fixture file");
    }

    fn host(&self, path: &str) -> PathBuf {
        self.root.join(path.trim_start_matches('/'))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).expect("remove fixture");
    }
}

type OpenAction = Box<dyn FnOnce() + Send>;

struct FakeState {
    snapshots: VecDeque<TopologySnapshot>,
    last: Option<TopologySnapshot>,
    open_count: usize,
    snapshot_count: usize,
    open_action: Option<(usize, OpenAction)>,
    snapshot_action: Option<(usize, OpenAction)>,
}

struct FakeSource {
    root: File,
    state: Mutex<FakeState>,
}

impl FakeSource {
    fn new(root: &Path, snapshots: Vec<TopologySnapshot>) -> Self {
        let root = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_CLOEXEC | libc::O_DIRECTORY)
            .open(root)
            .expect("open fixture root");
        Self {
            root,
            state: Mutex::new(FakeState {
                snapshots: snapshots.into(),
                last: None,
                open_count: 0,
                snapshot_count: 0,
                open_action: None,
                snapshot_action: None,
            }),
        }
    }

    fn act_after_opens(&self, additional: usize, action: impl FnOnce() + Send + 'static) {
        let mut state = self.state.lock().expect("lock fake topology source");
        let open_count = state.open_count;
        state.open_action = Some((open_count + additional, Box::new(action)));
    }

    fn act_on_next_open(&self, action: impl FnOnce() + Send + 'static) {
        self.act_after_opens(1, action);
    }

    fn act_after_snapshots(&self, additional: usize, action: impl FnOnce() + Send + 'static) {
        let mut state = self.state.lock().expect("lock fake topology source");
        let snapshot_count = state.snapshot_count;
        state.snapshot_action = Some((snapshot_count + additional, Box::new(action)));
    }
}

impl TopologySource for FakeSource {
    fn open_root(&self) -> io::Result<File> {
        let action = {
            let mut state = self.state.lock().expect("lock fake topology source");
            state.open_count += 1;
            if state
                .open_action
                .as_ref()
                .is_some_and(|(at, _)| *at == state.open_count)
            {
                state.open_action.take().map(|(_, action)| action)
            } else {
                None
            }
        };
        if let Some(action) = action {
            action();
        }
        self.root.try_clone()
    }

    fn snapshot(&self) -> Result<TopologySnapshot, BackendError> {
        let (snapshot, action) = {
            let mut state = self.state.lock().expect("lock fake topology source");
            state.snapshot_count += 1;
            let action = if state
                .snapshot_action
                .as_ref()
                .is_some_and(|(at, _)| *at == state.snapshot_count)
            {
                state.snapshot_action.take().map(|(_, action)| action)
            } else {
                None
            };
            let snapshot = if let Some(snapshot) = state.snapshots.pop_front() {
                state.last = Some(snapshot.clone());
                snapshot
            } else {
                state
                    .last
                    .clone()
                    .ok_or_else(|| invalid("fixture has no snapshot"))?
            };
            (snapshot, action)
        };
        if let Some(action) = action {
            action();
        }
        Ok(snapshot)
    }
}

fn mount(id: u32, parent_id: u32, root: &str, mountpoint: &str, fs_type: &str) -> MountInfo {
    MountInfo {
        id,
        parent_id,
        device: (8, 1),
        root: root.into(),
        mountpoint: mountpoint.into(),
        fs_type: fs_type.into(),
    }
}

fn snapshot(start_time: u64, digest: u64, mounts: Vec<MountInfo>) -> TopologySnapshot {
    TopologySnapshot {
        namespace: (1, 2),
        start_time,
        mountinfo_digest: digest,
        mounts,
    }
}

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
    let fixture = Fixture::new();
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
    let fixture = Fixture::new();
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
    let fixture = Fixture::new();
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
    let fixture = Fixture::new();
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
    let fixture = Fixture::new();
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
fn mountinfo_decodes_escapes_and_rejects_malformed_duplicate_and_bounds() {
    let parsed = parse_mountinfo(
        b"1 0 8:1 / / rw - ext4 /dev/sda rw\n2 1 8:1 /a\\040b /mnt\\134x rw - ext4 /dev/sda rw\n",
    )
    .expect("parse escaped mount paths");
    assert_eq!(parsed[1].root, "/a b");
    assert_eq!(parsed[1].mountpoint, "/mnt\\x");
    let root_mount = mount(1, 0, "/", "/", "ext4");
    let bind_mount = mount(2, 1, "/outside/source", "/tree/mounted", "ext4");
    assert_eq!(
        underlying_mount_path(
            &[root_mount.clone(), mount(3, 1, "/base", "/mnt", "ext4")],
            "/mnt/tree",
        )
        .expect("derive filesystem-relative target"),
        "/base/tree"
    );
    assert_eq!(
        visible_paths_for_mount_root(&[root_mount, bind_mount.clone()], &bind_mount),
        vec!["/outside/source"]
    );

    for malformed in [
        "1 0 8:1 / / rw ext4 /dev/sda rw\n",
        "1 0 8:1 / / rw - ext4 /dev/sda rw\n1 0 8:1 / /x rw - ext4 /dev/sda rw\n",
        "1 0 8:1 / / rw - ext4 /dev/sda rw\n2 99 8:1 / /x rw - ext4 /dev/sda rw\n",
        "1 0 8:1 /bad\\999 / rw - ext4 /dev/sda rw\n",
    ] {
        assert!(
            parse_mountinfo(malformed.as_bytes()).is_err(),
            "{malformed:?}"
        );
    }
    let oversized = format!(
        "1 0 8:1 / /{} rw - ext4 /dev/sda rw\n",
        "x".repeat(MAX_MOUNTINFO_LINE_BYTES)
    );
    assert!(parse_mountinfo(oversized.as_bytes()).is_err());
}

#[test]
fn overlay_rejection_is_scoped_to_targets_exclusions_and_submounts() {
    let fixture = Fixture::new();
    fixture.mkdir("/tree/sub");
    let tree = normalized(vec![target(
        "/tree",
        FileProtectionTargetKind::DirectoryTree,
    )]);
    fixture.mkdir("/unrelated");
    let base = mount(1, 0, "/", "/", "ext4");
    assert!(
        reject_relevant_overlay(
            &[
                mount(1, 0, "/", "/", "overlay"),
                mount(2, 1, "/", "/tmp", "tmpfs"),
            ],
            "/tmp/tree",
            true,
            2,
        )
        .is_ok(),
        "a non-overlay mount shadows its overlay parent"
    );
    assert!(
        prepare_fixture(
            &fixture,
            &tree,
            vec![base.clone(), mount(2, 1, "/x", "/unrelated", "overlay")],
        )
        .is_ok()
    );
    assert!(
        prepare_fixture(
            &fixture,
            &tree,
            vec![base, mount(2, 1, "/x", "/tree/sub", "overlayfs")],
        )
        .is_err()
    );
}

#[test]
fn submounts_are_assigned_to_their_component_boundary_root() {
    let fixture = Fixture::new();
    fixture.mkdir("/tree/sub");
    fixture.mkdir("/treeish/sub");
    let prepared = prepare_fixture(
        &fixture,
        &normalized(vec![target(
            "/tree",
            FileProtectionTargetKind::DirectoryTree,
        )]),
        vec![
            mount(1, 0, "/", "/", "ext4"),
            mount(2, 1, "/sub", "/tree/sub", "ext4"),
            mount(3, 1, "/other", "/treeish/sub", "ext4"),
        ],
    )
    .expect("prepare submount topology");

    assert_eq!(prepared.existing_submounts.len(), 1);
    assert_eq!(
        prepared.existing_submounts[0].path.authored_path,
        "/tree/sub"
    );
    assert_eq!(prepared.existing_submounts[0].target_id, 1);
}

#[test]
fn topology_change_retries_but_persistent_change_fails() {
    let fixture = Fixture::new();
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
    let fixture = Fixture::new();
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
fn intermediate_symlink_target_discovers_canonical_submount() {
    let fixture = Fixture::new();
    fixture.mkdir("/real/tree/mounted");
    symlink("real", fixture.host("/alias")).expect("intermediate directory symlink");
    let prepared = prepare_fixture(
        &fixture,
        &normalized(vec![target(
            "/alias/tree",
            FileProtectionTargetKind::DirectoryTree,
        )]),
        vec![
            mount(1, 0, "/", "/", "ext4"),
            mount(2, 1, "/outside/source", "/real/tree/mounted", "ext4"),
        ],
    )
    .expect("prepare canonical submount through authored alias");
    assert!(
        prepared
            .existing_submounts
            .iter()
            .any(|record| record.path.authored_path == "/real/tree/mounted")
    );
    assert!(
        prepared
            .known_bind_aliases
            .iter()
            .any(|record| record.path.authored_path == "/real/tree")
    );
    assert_eq!(prepared.direct_targets[0].path.authored_path, "/alias/tree");
    prepared.validate().expect("stable intermediate alias");
}

#[test]
fn authored_path_aba_during_prepare_is_rejected() {
    let fixture = Fixture::new();
    fixture.mkdir("/config");
    fixture.mkdir("/other");
    let expected_ino = fs::metadata(fixture.host("/config"))
        .expect("original directory")
        .ino();
    let path = fixture.host("/config");
    let other = fixture.host("/other");
    let temporary = fixture.host("/swap");
    let stable = snapshot(77, 1, vec![mount(1, 0, "/", "/", "ext4")]);
    let source = Arc::new(FakeSource::new(&fixture.root, vec![stable]));
    let swap_path = path.clone();
    let swap_other = other.clone();
    let swap_temporary = temporary.clone();
    source.act_on_next_open(move || {
        fs::rename(&swap_path, &swap_temporary).expect("move authored inode aside");
        fs::rename(&swap_other, &swap_path).expect("install transient inode");
        fs::rename(&swap_temporary, &swap_other).expect("complete first exchange");
    });
    source.act_after_snapshots(2, move || {
        fs::rename(&path, &temporary).expect("move transient inode aside");
        fs::rename(&other, &path).expect("restore authored inode");
        fs::rename(&temporary, &other).expect("complete ABA exchange");
    });

    let error = prepare_from(
        &normalized(vec![target(
            "/config",
            FileProtectionTargetKind::DirectoryTree,
        )]),
        123,
        77,
        source,
    )
    .expect_err("fresh-root re-resolution must detect a restored authored path");

    assert!(error.to_string().contains("identity changed"));
    assert_eq!(
        fs::metadata(fixture.host("/config"))
            .expect("restored directory")
            .ino(),
        expected_ino
    );
}

#[test]
fn authored_path_aba_before_activation_is_rejected() {
    let fixture = Fixture::new();
    fixture.write("/config");
    let stable = snapshot(77, 1, vec![mount(1, 0, "/", "/", "ext4")]);
    let source = Arc::new(FakeSource::new(&fixture.root, vec![stable]));
    let prepared = prepare_from(
        &normalized(vec![target("/config", FileProtectionTargetKind::ExactFile)]),
        123,
        77,
        source.clone(),
    )
    .expect("prepare stable identity manifest");
    let path = fixture.host("/config");
    let retired = fixture.host("/retired-config");
    source.act_on_next_open(move || {
        fs::rename(&path, &retired).expect("retire prepared inode");
        fs::write(&path, b"replacement").expect("replace authored path");
    });

    let error = prepared
        .validate()
        .expect_err("activation validation must detect path replacement");

    assert!(error.to_string().contains("identity changed"));
}

#[test]
fn future_exclusion_must_still_have_a_missing_final_component() {
    let fixture = Fixture::new();
    fixture.mkdir("/tree");
    let mut tree = target("/tree", FileProtectionTargetKind::DirectoryTree);
    tree.exclusions.push(FileProtectionExclusion {
        path: "/tree/future".into(),
        kind: FileProtectionExclusionKind::ExactEntry,
    });
    let stable = snapshot(77, 1, vec![mount(1, 0, "/", "/", "ext4")]);
    let source = Arc::new(FakeSource::new(&fixture.root, vec![stable]));
    let prepared = prepare_from(&normalized(vec![tree]), 123, 77, source.clone())
        .expect("prepare future exclusion");
    let future = fixture.host("/tree/future");
    source.act_on_next_open(move || {
        fs::write(future, b"now present").expect("materialize future exclusion");
    });

    let error = prepared
        .validate()
        .expect_err("activation validation must reject a materialized exclusion");

    assert!(error.to_string().contains("now exists"));
}

#[test]
fn unrelated_unresolvable_mounts_do_not_abort_tree_preparation() {
    let fixture = Fixture::new();
    fixture.mkdir("/tree");
    symlink("loop", fixture.host("/loop")).expect("unrelated looping symlink");
    let policy = normalized(vec![target(
        "/tree",
        FileProtectionTargetKind::DirectoryTree,
    )]);
    let prepared = prepare_fixture(
        &fixture,
        &policy,
        vec![
            mount(1, 0, "/", "/", "ext4"),
            mount(2, 1, "/missing", "/hidden/missing", "ext4"),
            mount(3, 1, "/loop", "/loop/mounted", "ext4"),
        ],
    )
    .expect("unrelated inaccessible mountpoints must not reject policy");
    prepared.validate().expect("stable relevant identities");
    assert!(prepared.existing_submounts.is_empty());

    assert!(
        prepare_fixture(
            &fixture,
            &policy,
            vec![
                mount(1, 0, "/", "/", "ext4"),
                mount(2, 1, "/missing", "/tree/missing", "ext4"),
            ],
        )
        .is_err(),
        "an unresolvable protected submount must still fail closed"
    );
}

#[test]
fn symlink_entry_exclusion_and_followed_parent_keep_separate_identities() {
    let fixture = Fixture::new();
    fixture.mkdir("/tree");
    fixture.write("/outside/file");
    symlink("../outside", fixture.host("/tree/link")).expect("excluded symlink entry");
    let mut tree = target("/tree", FileProtectionTargetKind::DirectoryTree);
    tree.exclusions.push(FileProtectionExclusion {
        path: "/tree/link".into(),
        kind: FileProtectionExclusionKind::ExactEntry,
    });
    let prepared = prepare_fixture(
        &fixture,
        &normalized(vec![
            tree,
            target("/tree/link/file", FileProtectionTargetKind::ExactFile),
        ]),
        vec![mount(1, 0, "/", "/", "ext4")],
    )
    .expect("the excluded symlink and followed directory are distinct expectations");
    prepared.validate().expect("validate both resolution modes");
    fs::rename(fixture.host("/outside"), fixture.host("/retired-outside"))
        .expect("retire followed parent");
    fixture.write("/outside/file");
    assert!(
        prepared.validate().is_err(),
        "a stable symlink inode must not hide a replaced parent"
    );
}

#[test]
fn submount_discovery_matches_underlying_identity_across_mount_ids() {
    let fixture = Fixture::new();
    fixture.mkdir("/tree/mounted");
    symlink("tree", fixture.host("/view")).expect("simulate alias spelling");
    let source = FakeSource::new(&fixture.root, Vec::new());
    let root = source.open_root().expect("open fixture root");
    let mut identity = resolve_held(&root, "/tree")
        .expect("target identity")
        .identity;
    identity.mnt_id = identity.mnt_id.wrapping_add(1);
    let discovery = discover_submounts(
        &root,
        &[
            mount(1, 0, "/", "/", "ext4"),
            mount(2, 1, "/source", "/view/mounted", "ext4"),
        ],
        "/tree",
        identity,
    )
    .expect("discover alias submount with a distinct namespace mount id");
    assert_eq!(discovery.indices, vec![1]);
    assert!(discovery.target_spellings.contains("/view"));
}

#[test]
fn capacity_count_overflow_is_reported() {
    let error = checked_capacity_counts(0, usize::MAX, 0, 1, 0)
        .expect_err("capacity arithmetic must be checked");
    assert!(error.to_string().contains("overflow"));
}
