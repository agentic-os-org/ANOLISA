use std::fs;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, symlink};
use std::path::PathBuf;

use agentsight_enforcement_protocol::FileProtectionTargetKind;
use uuid::Uuid;

use super::*;

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        // Keep real-proc fixtures off container overlay filesystems.
        let root =
            PathBuf::from("/dev/shm").join(format!("fp-resolver-{}", Uuid::new_v4().simple()));
        fs::create_dir(&root).expect("create fixture root");
        Self { root }
    }

    fn host(&self, path: &str) -> PathBuf {
        self.root.join(path.trim_start_matches('/'))
    }

    fn authored(&self, path: &str) -> String {
        self.host(path)
            .to_str()
            .expect("UTF-8 fixture path")
            .to_owned()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).expect("remove fixture");
    }
}

fn process_root() -> std::fs::File {
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_CLOEXEC | libc::O_DIRECTORY)
        .open("/proc/self/root")
        .expect("open process root")
}

fn openat2_available(root: &std::fs::File, path: &str) -> bool {
    !matches!(open_held(root, path), Err(error) if error.raw_errno() == Some(libc::ENOSYS))
}

#[test]
fn held_path_reports_namespace_identity_of_an_existing_file() {
    let fixture = Fixture::new();
    fs::write(fixture.host("/config"), b"fixture").expect("write fixture file");
    let root = process_root();
    let authored = fixture.authored("/config");
    if !openat2_available(&root, &authored) {
        eprintln!("skipping: host kernel does not provide openat2");
        return;
    }

    let held = open_held(&root, &authored).expect("hold existing file");

    assert_eq!(held.authored_path, authored);
    assert_eq!(
        held.identity.ino,
        fs::metadata(fixture.host("/config"))
            .expect("metadata")
            .ino()
    );
    assert_ne!(held.identity.mnt_id, 0);
    assert!(require_type(&held, FileProtectionTargetKind::ExactFile).is_ok());
    assert!(require_directory(&held).is_err());
}

#[test]
fn final_symlink_is_rejected_unless_the_caller_follows_directories() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.host("/real")).expect("create target directory");
    symlink("real", fixture.host("/link")).expect("create final symlink");
    let root = process_root();
    let authored = fixture.authored("/link");
    if !openat2_available(&root, &fixture.authored("/real")) {
        eprintln!("skipping: host kernel does not provide openat2");
        return;
    }

    let held = open_held(&root, &authored).expect("hold the symlink itself");
    assert!(require_directory(&held).is_err());
    assert!(require_type(&held, FileProtectionTargetKind::DirectoryTree).is_err());

    let followed = open_held_directory(&root, &authored).expect("follow intermediate alias");
    assert_eq!(
        followed.identity.ino,
        fs::metadata(fixture.host("/real")).expect("metadata").ino()
    );
    assert!(require_directory(&followed).is_ok());
}

#[test]
fn absent_components_report_enoent_while_other_failures_stay_distinct() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.host("/tree")).expect("create parent directory");
    fs::write(fixture.host("/tree/config"), b"fixture").expect("write regular file");
    let root = process_root();
    if !openat2_available(&root, &fixture.authored("/tree")) {
        eprintln!("skipping: host kernel does not provide openat2");
        return;
    }

    // An absent final component and an absent parent are both ENOENT; callers
    // needing that distinction must resolve the parent separately.
    for path in ["/tree/future", "/absent/future"] {
        let error = open_held(&root, &fixture.authored(path)).expect_err("absent path must fail");
        assert!(error.is_missing_final(), "{path}");
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    }

    // A regular file used as an intermediate component is a different failure.
    let not_a_directory = open_held(&root, &fixture.authored("/tree/config/child"))
        .expect_err("file used as a directory must fail");
    assert!(!not_a_directory.is_missing_final());
    assert_eq!(not_a_directory.raw_errno(), Some(libc::ENOTDIR));
}

#[test]
fn resolution_is_confined_to_the_process_root_and_rejects_nul() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.host("/tree")).expect("create fixture directory");
    let root = process_root();
    let authored = fixture.authored("/tree");
    if !openat2_available(&root, &authored) {
        eprintln!("skipping: host kernel does not provide openat2");
        return;
    }

    // RESOLVE_IN_ROOT clamps a leading escape attempt at the root instead of
    // resolving outside it, so both spellings reach the same object.
    let direct = open_held_directory(&root, &authored).expect("hold fixture directory");
    let clamped = open_held_directory(&root, &format!("/../..{authored}"))
        .expect("escape attempt is clamped to the root");
    assert_eq!(direct.identity, clamped.identity);

    // An interior NUL cannot reach the kernel at all.
    let interior_nul = open_held(&root, "/tmp/\0/config").expect_err("NUL must be rejected");
    assert!(interior_nul.raw_errno().is_none());
    assert_eq!(interior_nul.kind(), std::io::ErrorKind::InvalidInput);
}
