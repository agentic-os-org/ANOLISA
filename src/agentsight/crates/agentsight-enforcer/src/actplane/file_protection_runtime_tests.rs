use std::cell::Cell;
use std::fs::{self, OpenOptions};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::PathBuf;

use agentsight_enforcement_protocol::{
    FILE_PROTECTION_POLICY_VERSION, FileProtectionExclusion, FileProtectionPolicy,
    FileProtectionTarget, FileProtectionTopologyMode, FileProtections,
};
use uuid::Uuid;

use super::super::file_protection_policy::normalize;
use super::super::file_protection_topology;
use super::*;

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        // Keep real-proc fixtures off container overlay filesystems.
        let root = PathBuf::from("/dev/shm").join(format!("fp-lower-{}", Uuid::new_v4().simple()));
        fs::create_dir(&root).expect("create fixture root");
        Self { root }
    }

    fn path(&self, relative: &str) -> String {
        self.root
            .join(relative)
            .to_str()
            .expect("UTF-8 fixture path")
            .into()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).expect("remove fixture root");
    }
}

fn target(
    path: String,
    kind: FileProtectionTargetKind,
    exclusions: Vec<FileProtectionExclusion>,
) -> FileProtectionTarget {
    target_with(path, kind, true, false, exclusions)
}

fn target_with(
    path: String,
    kind: FileProtectionTargetKind,
    delete: bool,
    ordinary_write: bool,
    exclusions: Vec<FileProtectionExclusion>,
) -> FileProtectionTarget {
    FileProtectionTarget {
        path,
        kind,
        protections: FileProtections {
            delete,
            ordinary_write,
        },
        exclusions,
    }
}

fn prepare(targets: Vec<FileProtectionTarget>) -> Result<PreparedFileProtection, BackendError> {
    let policy = normalize(&FileProtectionPolicy {
        version: FILE_PROTECTION_POLICY_VERSION,
        topology_mode: FileProtectionTopologyMode::StaticAtApply,
        targets,
    })
    .expect("normalize fixture policy");
    let pid = i32::try_from(std::process::id()).expect("PID fits i32");
    let start = super::super::read_process_start_time(pid)?;
    let topology = file_protection_topology::prepare(&policy, pid, start)?;
    Ok(PreparedFileProtection { policy, topology })
}

#[test]
fn exact_target_lowers_to_parent_slot_without_inode_root_membership() {
    let fixture = Fixture::new();
    let exact = fixture.path("config.json");
    fs::write(&exact, b"config").expect("write exact target");
    let prepared = match prepare(vec![target(
        exact.clone(),
        FileProtectionTargetKind::ExactFile,
        Vec::new(),
    )]) {
        Ok(prepared) => prepared,
        Err(BackendError::CompileFailure(message)) if message.contains("lacks openat2") => return,
        Err(error) => panic!("prepare exact target: {error}"),
    };

    let lowered = lower(&prepared, Uuid::nil()).expect("lower exact target");

    assert!(lowered.roots.is_empty());
    assert!(lowered.directories.is_empty());
    assert_eq!(lowered.slots.len(), 1);
    let (parent, masks) = lowered.slots.values().next().expect("direct slot");
    assert_eq!(parent.mnt_id, 0, "slot parent must match bind aliases");
    assert_eq!(masks.direct, 1);
    assert_eq!(lowered.activation.tree_mask, 0);
    assert_eq!(lowered.activation.delete_mask, 1);
    assert_eq!(lowered.activation.ordinary_write_mask, 0);
    assert!(
        lowered
            .stable_paths
            .keys()
            .all(|key| key.ino != fs::metadata(&exact).expect("exact metadata").ino()),
        "an outside hardlink must not become protected by exact-file inode identity"
    );
}

#[test]
fn exclusions_and_nested_direct_targets_lower_as_scoped_masks() {
    let fixture = Fixture::new();
    let tree = fixture.path("tree");
    let excluded = fixture.path("tree/cache");
    let exact_excluded = fixture.path("tree/socket");
    let nested = fixture.path("tree/cache/manifest");
    fs::create_dir_all(&excluded).expect("create excluded subtree");
    fs::write(&exact_excluded, b"socket").expect("write exact exclusion");
    fs::write(&nested, b"manifest").expect("write nested direct target");
    let prepared = match prepare(vec![
        target(
            tree,
            FileProtectionTargetKind::DirectoryTree,
            vec![
                FileProtectionExclusion {
                    path: excluded.clone(),
                    kind: FileProtectionExclusionKind::DirectorySubtree,
                },
                FileProtectionExclusion {
                    path: exact_excluded.clone(),
                    kind: FileProtectionExclusionKind::ExactEntry,
                },
            ],
        ),
        target_with(
            nested.clone(),
            FileProtectionTargetKind::ExactFile,
            true,
            true,
            Vec::new(),
        ),
    ]) {
        Ok(prepared) => prepared,
        Err(BackendError::CompileFailure(message)) if message.contains("lacks openat2") => return,
        Err(error) => panic!("prepare overlap policy: {error}"),
    };

    let lowered = lower(&prepared, Uuid::nil()).expect("lower overlap policy");
    let by_name = |name: &str| {
        lowered
            .slots
            .iter()
            .find(|(key, _)| key.name == name)
            .map(|(_, (_, masks))| *masks)
            .expect("slot by basename")
    };

    assert_eq!(by_name("cache").subtree_exclude, 1);
    assert_eq!(by_name("socket").exact_exclude, 1);
    assert_eq!(by_name("manifest").direct, 2);
    assert_eq!(lowered.activation.tree_mask, 1);
    assert_eq!(lowered.activation.delete_mask, 3);
    assert_eq!(lowered.activation.ordinary_write_mask, 2);
    assert_eq!(
        lowered.targets[1].flags,
        TARGET_DELETE | TARGET_ORDINARY_WRITE
    );
    assert_eq!(lowered.targets[1].guard.rule_id, 2);
    assert!(lowered.counts().slots <= prepared.topology.capacities.slots);
}

#[test]
fn write_only_and_overlapping_targets_lower_independent_operation_masks() {
    let fixture = Fixture::new();
    let tree = fixture.path("tree");
    let exact = fixture.path("tree/config.json");
    fs::create_dir(&tree).expect("create tree target");
    fs::write(&exact, b"config").expect("write nested exact target");
    let prepared = match prepare(vec![
        target_with(
            tree,
            FileProtectionTargetKind::DirectoryTree,
            true,
            false,
            Vec::new(),
        ),
        target_with(
            exact,
            FileProtectionTargetKind::ExactFile,
            false,
            true,
            Vec::new(),
        ),
    ]) {
        Ok(prepared) => prepared,
        Err(BackendError::CompileFailure(message)) if message.contains("lacks openat2") => return,
        Err(error) => panic!("prepare write overlap policy: {error}"),
    };

    let lowered = lower(&prepared, Uuid::nil()).expect("lower write overlap policy");

    assert_eq!(lowered.activation.tree_mask, 1);
    assert_eq!(lowered.activation.delete_mask, 1);
    assert_eq!(lowered.activation.ordinary_write_mask, 2);
    assert_eq!(lowered.targets[0].flags, TARGET_DELETE | TARGET_TREE);
    assert_eq!(lowered.targets[1].flags, TARGET_ORDINARY_WRITE);
    assert!(
        lowered.slots.values().any(|(_, masks)| masks.direct == 2),
        "the nested exact target must retain its independent capability bit"
    );
}

#[test]
fn write_only_policy_lowers_without_a_delete_fallback() {
    let fixture = Fixture::new();
    let exact = fixture.path("config.json");
    fs::write(&exact, b"config").expect("write exact target");
    let prepared = match prepare(vec![target_with(
        exact,
        FileProtectionTargetKind::ExactFile,
        false,
        true,
        Vec::new(),
    )]) {
        Ok(prepared) => prepared,
        Err(BackendError::CompileFailure(message)) if message.contains("lacks openat2") => return,
        Err(error) => panic!("prepare write-only policy: {error}"),
    };

    let lowered = lower(&prepared, Uuid::nil()).expect("lower write-only policy");

    assert_eq!(lowered.activation.tree_mask, 0);
    assert_eq!(lowered.activation.delete_mask, 0);
    assert_eq!(lowered.activation.ordinary_write_mask, 1);
    assert_eq!(lowered.activation.guard.rule_id, 1);
}

#[test]
fn existing_submount_lowers_namespace_and_underlying_root_tokens() {
    let fixture = Fixture::new();
    let tree = fixture.path("tree");
    fs::create_dir(&tree).expect("create tree target");
    let mut prepared = match prepare(vec![target(
        tree,
        FileProtectionTargetKind::DirectoryTree,
        Vec::new(),
    )]) {
        Ok(prepared) => prepared,
        Err(BackendError::CompileFailure(message)) if message.contains("lacks openat2") => return,
        Err(error) => panic!("prepare tree target: {error}"),
    };
    let source = fixture.path("outside-source");
    fs::create_dir(&source).expect("create submount source");
    let proc_root = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_CLOEXEC | libc::O_DIRECTORY)
        .open("/proc/self/root")
        .expect("open process root");
    let underlying = super::super::file_protection_resolver::open_held(&proc_root, &source)
        .expect("hold underlying source");
    let mut namespace_path = underlying.clone();
    namespace_path.authored_path = "/tree/mounted".into();
    namespace_path.identity.mnt_id = namespace_path.identity.mnt_id.wrapping_add(100);
    assert_ne!(namespace_path.identity.mnt_id, 0);
    let namespace_root = TopologyPathRecord {
        target_id: 1,
        path: namespace_path,
    };
    let underlying_root = TopologyPathRecord {
        target_id: 1,
        path: underlying,
    };
    prepared.topology.existing_submounts = vec![namespace_root.clone()];
    prepared.topology.underlying_submounts = vec![underlying_root.clone()];

    let lowered = lower(&prepared, Uuid::nil()).expect("lower submount topology");

    assert_eq!(
        lowered
            .roots
            .get(&PathKey::from_identity(namespace_root.path.identity)),
        Some(&1)
    );
    assert_eq!(
        lowered
            .roots
            .get(&PathKey::from_identity(underlying_root.path.identity)),
        Some(&1)
    );
    assert_eq!(
        lowered
            .directories
            .get(&PathKey::underlying(underlying_root.path.identity)),
        Some(&1)
    );
}

#[test]
fn generation_is_stable_nonzero_and_revision_sensitive() {
    let binding = Uuid::parse_str("00000000-0000-4000-8000-000000000041").expect("fixture UUID");
    let first = generation(binding, "7").expect("derive generation");
    let repeat = generation(binding, "7").expect("repeat generation");
    let next = generation(binding, "8").expect("next generation");

    assert_eq!(first, repeat);
    assert_ne!(first.get(), 0);
    assert_ne!(first, next);
}

#[test]
fn exchanged_directory_aborts_activation_and_drops_inactive_owner() {
    struct InactiveOwner<'a>(&'a Cell<bool>);

    impl Drop for InactiveOwner<'_> {
        fn drop(&mut self) {
            self.0.set(true);
        }
    }

    let fixture = Fixture::new();
    let tree = fixture.path("tree");
    let other = fixture.path("other");
    let swap = fixture.path("swap");
    fs::create_dir(&tree).expect("create tree target");
    fs::create_dir(&other).expect("create exchange partner");
    let prepared = match prepare(vec![target(
        tree.clone(),
        FileProtectionTargetKind::DirectoryTree,
        Vec::new(),
    )]) {
        Ok(prepared) => prepared,
        Err(BackendError::CompileFailure(message)) if message.contains("lacks openat2") => return,
        Err(error) => panic!("prepare activation policy: {error}"),
    };
    fs::rename(&tree, &swap).expect("move held directory aside");
    fs::rename(&other, &tree).expect("install exchange partner");
    fs::rename(&swap, &other).expect("complete directory exchange");
    let activated = Cell::new(false);
    let cleaned = Cell::new(false);
    let owner = InactiveOwner(&cleaned);

    let error = activate_validated(&prepared.topology, || {
        activated.set(true);
        drop(owner);
        Ok(())
    })
    .expect_err("replaced pathname must abort before activation");

    assert!(error.to_string().contains("identity changed"));
    assert!(!activated.get(), "activation callback must never run");
    assert!(cleaned.get(), "inactive staging owner must be dropped");
}

#[test]
fn stable_path_reaches_activation_and_propagates_map_error() {
    let fixture = Fixture::new();
    let exact = fixture.path("config.json");
    fs::write(&exact, b"config").expect("write exact target");
    let prepared = match prepare(vec![target(
        exact,
        FileProtectionTargetKind::ExactFile,
        Vec::new(),
    )]) {
        Ok(prepared) => prepared,
        Err(BackendError::CompileFailure(message)) if message.contains("lacks openat2") => return,
        Err(error) => panic!("prepare activation policy: {error}"),
    };
    let calls = Cell::new(0);
    let active = activate_validated(&prepared.topology, || {
        calls.set(calls.get() + 1);
        Ok(42)
    })
    .expect("activate stable topology");
    assert_eq!(active, 42);
    assert_eq!(calls.get(), 1);

    let error = activate_validated(&prepared.topology, || {
        calls.set(calls.get() + 1);
        Err::<(), _>(io::Error::other("activation map failed"))
    })
    .expect_err("map failure must propagate");
    assert!(error.to_string().contains("activation map failed"));
    assert_eq!(calls.get(), 2);
}
