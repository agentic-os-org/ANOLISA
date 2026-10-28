use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use uuid::Uuid;

use super::super::file_protection_mountinfo::MountInfo;
use super::super::file_protection_resolver::open_held;
use super::*;

pub(crate) struct Fixture {
    pub(crate) root: PathBuf,
}

impl Fixture {
    pub(crate) fn new(prefix: &str) -> Self {
        // Keep real-proc fixtures off container overlay filesystems.
        let root = PathBuf::from("/dev/shm").join(format!("{prefix}-{}", Uuid::new_v4().simple()));
        fs::create_dir(&root).expect("create fixture root");
        Self { root }
    }

    pub(crate) fn mkdir(&self, path: &str) {
        fs::create_dir_all(self.host(path)).expect("create fixture directory");
    }

    pub(crate) fn write(&self, path: &str) {
        if let Some(parent) = self.host(path).parent() {
            fs::create_dir_all(parent).expect("create fixture parent");
        }
        fs::write(self.host(path), b"fixture").expect("write fixture file");
    }

    pub(crate) fn host(&self, path: &str) -> PathBuf {
        self.root.join(path.trim_start_matches('/'))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).expect("remove fixture");
    }
}

type Action = Box<dyn FnOnce() + Send>;

struct FakeState {
    snapshots: VecDeque<TopologySnapshot>,
    last: Option<TopologySnapshot>,
    open_count: usize,
    open_action: Option<(usize, Action)>,
}

/// Topology source backed by a fixture directory, so manifest validation can be
/// driven deterministically without a second process.
pub(crate) struct FakeSource {
    root: File,
    state: Mutex<FakeState>,
}

impl FakeSource {
    pub(crate) fn new(root: &Path, snapshots: Vec<TopologySnapshot>) -> Self {
        Self {
            root: fixture_root(root),
            state: Mutex::new(FakeState {
                snapshots: snapshots.into(),
                last: None,
                open_count: 0,
                open_action: None,
            }),
        }
    }

    pub(crate) fn act_on_next_open(&self, action: impl FnOnce() + Send + 'static) {
        let mut state = self.state.lock().expect("lock fake topology source");
        let open_count = state.open_count;
        state.open_action = Some((open_count + 1, Box::new(action)));
    }
}

impl TopologySource for FakeSource {
    fn open_root(&self) -> io::Result<File> {
        let action = {
            let mut state = self.state.lock().expect("lock fake topology source");
            state.open_count += 1;
            state
                .open_action
                .as_ref()
                .is_some_and(|(at, _)| *at == state.open_count)
                .then(|| state.open_action.take().map(|(_, action)| action))
                .flatten()
        };
        if let Some(action) = action {
            action();
        }
        self.root.try_clone()
    }

    fn snapshot(&self) -> Result<TopologySnapshot, BackendError> {
        let mut state = self.state.lock().expect("lock fake topology source");
        if let Some(snapshot) = state.snapshots.pop_front() {
            state.last = Some(snapshot.clone());
            Ok(snapshot)
        } else {
            state
                .last
                .clone()
                .ok_or_else(|| BackendError::CompileFailure("fixture has no snapshot".into()))
        }
    }
}

pub(crate) fn fixture_root(root: &Path) -> File {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_CLOEXEC | libc::O_DIRECTORY)
        .open(root)
        .expect("open fixture root")
}

pub(crate) fn mount(
    id: u32,
    parent_id: u32,
    root: &str,
    mountpoint: &str,
    fs_type: &str,
) -> MountInfo {
    MountInfo {
        id,
        parent_id,
        device: (8, 1),
        root: root.into(),
        mountpoint: mountpoint.into(),
        fs_type: fs_type.into(),
    }
}

pub(crate) fn snapshot(start_time: u64, digest: u64, mounts: Vec<MountInfo>) -> TopologySnapshot {
    TopologySnapshot {
        namespace: (1, 2),
        start_time,
        mountinfo_digest: digest,
        mounts,
    }
}

fn openat2_available(root: &File) -> bool {
    !matches!(open_held(root, "/"), Err(error) if error.raw_errno() == Some(libc::ENOSYS))
}

#[test]
fn snapshot_compares_process_identity_and_mount_topology() {
    let base = snapshot(77, 9, vec![mount(1, 0, "/", "/", "ext4")]);
    assert!(base.validate_process(123, 77).is_ok());
    assert!(matches!(
        base.validate_process(123, 78),
        Err(BackendError::StaleProcess { pid: 123 })
    ));

    assert!(base.same_topology(&base.clone()));
    let mut other_digest = base.clone();
    other_digest.mountinfo_digest = 10;
    assert!(!base.same_topology(&other_digest));
    assert!(base.change_message(&other_digest).contains("digest 9->10"));

    let mut other_namespace = base.clone();
    other_namespace.namespace = (1, 3);
    assert!(!base.same_topology(&other_namespace));

    let mut other_mounts = base.clone();
    other_mounts.mounts.push(mount(2, 1, "/x", "/mnt", "ext4"));
    assert!(!base.same_topology(&other_mounts));
}

#[test]
fn process_topology_source_reads_the_live_process() {
    let source = ProcTopologySource::new(i32::try_from(std::process::id()).expect("PID fits i32"));
    let snapshot = source.snapshot().expect("snapshot own topology");

    assert!(snapshot.mounts.iter().any(|mount| mount.mountpoint == "/"));
    assert_ne!(snapshot.start_time, 0);
    assert!(source.open_root().is_ok());

    let absent = ProcTopologySource::new(-1);
    assert!(absent.snapshot().is_err());
    assert!(absent.open_root().is_err());
}

#[test]
fn manifest_rejects_conflicting_expectations_for_one_resolution_mode() {
    let fixture = Fixture::new("fp-manifest");
    fixture.mkdir("/tree");
    fixture.write("/config");
    let root = fixture_root(&fixture.root);
    if !openat2_available(&root) {
        eprintln!("skipping: host kernel does not provide openat2");
        return;
    }

    let tree = open_held(&root, "/tree").expect("hold tree");
    let config = open_held(&root, "/config").expect("hold config");
    let mut manifest = IdentityManifest::default();
    manifest.add_existing(&config).expect("record exact file");
    manifest.add_directory(&tree).expect("record directory");
    // Re-recording the same expectation is idempotent.
    manifest.add_existing(&config).expect("repeat exact file");

    let mut replaced = config.clone();
    replaced.identity.ino += 1;
    let error = manifest
        .add_existing(&replaced)
        .expect_err("a different identity for one path must conflict");
    assert!(error.to_string().contains("conflicting topology identity"));

    // The same path may appear under both resolution modes.
    manifest
        .add_directory(&tree)
        .expect("directory mode stays recorded");
    manifest.validate(&root).expect("fixture is unchanged");
}

#[test]
fn manifest_validation_detects_replaced_paths_and_materialized_exclusions() {
    let fixture = Fixture::new("fp-manifest");
    fixture.mkdir("/tree");
    fixture.write("/config");
    let root = fixture_root(&fixture.root);
    if !openat2_available(&root) {
        eprintln!("skipping: host kernel does not provide openat2");
        return;
    }
    let tree = open_held(&root, "/tree").expect("hold tree");
    let config = open_held(&root, "/config").expect("hold config");

    let mut future = IdentityManifest::default();
    future
        .add_missing_final("/tree/future", &tree)
        .expect("record future exclusion");
    future
        .validate(&root)
        .expect("future entry is still absent");
    fixture.write("/tree/future");
    let error = future
        .validate(&root)
        .expect_err("a materialized exclusion must be rejected");
    assert!(error.to_string().contains("now exists"));

    let mut existing = IdentityManifest::default();
    existing.add_existing(&config).expect("record exact file");
    fs::rename(fixture.host("/config"), fixture.host("/retired")).expect("retire inode");
    fixture.write("/config");
    let error = existing
        .validate(&root)
        .expect_err("a replaced path must be rejected");
    assert!(error.to_string().contains("identity changed"));
}

#[test]
fn topology_validation_brackets_manifest_checks_with_snapshots() {
    let fixture = Fixture::new("fp-manifest");
    fixture.write("/config");
    let root = fixture_root(&fixture.root);
    if !openat2_available(&root) {
        eprintln!("skipping: host kernel does not provide openat2");
        return;
    }
    let mut manifest = IdentityManifest::default();
    manifest
        .add_existing(&open_held(&root, "/config").expect("hold config"))
        .expect("record exact file");
    let stable = snapshot(77, 1, vec![mount(1, 0, "/", "/", "ext4")]);

    let source = std::sync::Arc::new(FakeSource::new(&fixture.root, vec![stable.clone()]));
    let validation =
        TopologyValidation::new(123, 77, stable.clone(), manifest.clone(), source.clone());
    validation.validate().expect("stable topology and identity");

    // A path replaced between snapshots is caught by the fresh-root re-resolution.
    let path = fixture.host("/config");
    let retired = fixture.host("/retired-config");
    source.act_on_next_open(move || {
        fs::rename(&path, &retired).expect("retire inode");
        fs::write(&path, b"replacement").expect("replace authored path");
    });
    let error = validation
        .validate()
        .expect_err("replacement must fail validation");
    assert!(error.to_string().contains("identity changed"));

    // A process restart is reported before any path is re-resolved.
    let restarted = std::sync::Arc::new(FakeSource::new(
        &fixture.root,
        vec![snapshot(78, 1, vec![mount(1, 0, "/", "/", "ext4")])],
    ));
    let error = TopologyValidation::new(123, 77, stable.clone(), manifest.clone(), restarted)
        .validate()
        .expect_err("stale process must fail");
    assert!(matches!(error, BackendError::StaleProcess { pid: 123 }));

    // Mount topology drift is reported even when every identity still matches.
    let drifted = std::sync::Arc::new(FakeSource::new(
        &fixture.root,
        vec![snapshot(77, 2, vec![mount(1, 0, "/", "/", "ext4")])],
    ));
    let error = TopologyValidation::new(123, 77, stable, manifest, drifted)
        .validate()
        .expect_err("topology drift must fail");
    assert!(error.to_string().contains("topology changed"));
}

#[test]
fn opening_a_fresh_root_reports_the_requested_process() {
    let fixture = Fixture::new("fp-manifest");
    let source = FakeSource::new(&fixture.root, Vec::new());
    let root = open_fresh_root(&source, 123).expect("fixture root");
    assert_eq!(
        root.metadata().expect("root metadata").ino(),
        fs::metadata(&fixture.root).expect("fixture metadata").ino()
    );

    let error = open_fresh_root(&ProcTopologySource::new(-1), -1)
        .expect_err("an absent process has no root");
    assert!(error.to_string().contains("/proc/-1/root"));
}
