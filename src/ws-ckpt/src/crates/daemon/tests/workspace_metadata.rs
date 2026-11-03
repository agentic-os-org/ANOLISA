//! Exercise real rsync and metadata preservation without requiring a btrfs mount.

#![cfg(target_os = "linux")]

use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use ws_ckpt_common::backend::StorageBackend;
use ws_ckpt_daemon::backends::btrfs_base::{BtrfsBaseBackend, BtrfsBaseScenario};
use ws_ckpt_daemon::backends::btrfs_loop::BtrfsLoopBackend;

const CASE_ROOT: &str = "WS_CKPT_METADATA_TEST_ROOT";
const PAYLOAD: &[u8] = b"original payload\n";

fn set_xattr(path: &Path, name: &str, value: &[u8]) -> io::Result<()> {
    let path = CString::new(path.as_os_str().as_bytes()).unwrap();
    let name = CString::new(name).unwrap();
    // All buffers remain valid for the duration of the syscall.
    let result = unsafe {
        libc::setxattr(
            path.as_ptr(),
            name.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn get_xattr(path: &Path, name: &str) -> Vec<u8> {
    let path = CString::new(path.as_os_str().as_bytes()).unwrap();
    let name = CString::new(name).unwrap();
    let mut value = vec![0; 1024];
    // The output buffer is writable and both strings are NUL-terminated.
    let length = unsafe {
        libc::getxattr(
            path.as_ptr(),
            name.as_ptr(),
            value.as_mut_ptr().cast(),
            value.len(),
        )
    };
    assert!(
        length >= 0,
        "getxattr failed: {}",
        io::Error::last_os_error()
    );
    value.truncate(length as usize);
    value
}

fn access_acl(directory: bool) -> Vec<u8> {
    let mut bytes = 2u32.to_le_bytes().to_vec();
    // Use our mapped UID so this also works in single-UID user namespaces.
    let uid = unsafe { libc::getuid() };
    let execute = u16::from(directory);
    for (tag, permissions, id) in [
        (1u16, 6u16 | execute, u32::MAX),
        (2, 4 | execute, uid),
        (4, 4 | execute, u32::MAX),
        (16, 4 | execute, u32::MAX),
        (32, 0, u32::MAX),
    ] {
        bytes.extend(tag.to_le_bytes());
        bytes.extend(permissions.to_le_bytes());
        bytes.extend(id.to_le_bytes());
    }
    bytes
}

fn prepare_workspace(path: &Path) -> io::Result<()> {
    std::fs::create_dir_all(path)?;
    let file = path.join("record.bin");
    std::fs::write(&file, PAYLOAD)?;
    std::fs::hard_link(&file, path.join("alias.bin"))?;
    set_xattr(&file, "user.provenance", b"dataset-v3\0binary-value")?;
    set_xattr(path, "user.project", b"research")?;
    set_xattr(&file, "system.posix_acl_access", &access_acl(false))?;
    set_xattr(path, "system.posix_acl_access", &access_acl(true))?;
    set_xattr(path, "system.posix_acl_default", &access_acl(true))
}

fn assert_metadata(path: &Path) {
    let file = path.join("record.bin");
    let alias = path.join("alias.bin");
    assert_eq!(std::fs::read(&file).unwrap(), PAYLOAD);
    assert_eq!(
        get_xattr(&file, "user.provenance"),
        b"dataset-v3\0binary-value"
    );
    assert_eq!(get_xattr(path, "user.project"), b"research");
    assert_eq!(
        get_xattr(&file, "system.posix_acl_access"),
        access_acl(false)
    );
    assert_eq!(get_xattr(path, "system.posix_acl_access"), access_acl(true));
    assert_eq!(
        get_xattr(path, "system.posix_acl_default"),
        access_acl(true)
    );
    assert_eq!(
        std::fs::metadata(&file).unwrap().ino(),
        std::fs::metadata(&alias).unwrap().ino()
    );
    std::fs::write(&file, b"updated through original name").unwrap();
    assert_eq!(
        std::fs::read(&alias).unwrap(),
        b"updated through original name"
    );
}

fn executable(path: &Path, contents: &str) {
    std::fs::write(path, contents).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn run_cases(test_name: &str, fail_copy: bool) {
    if let Some(root) = std::env::var_os(CASE_ROOT) {
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(run_case(PathBuf::from(root), fail_copy));
        return;
    }

    if !Command::new("rsync")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
    {
        eprintln!("SKIP {test_name}: a working rsync executable is required");
        return;
    }
    let probe = tempfile::tempdir().unwrap();
    if let Err(error) = prepare_workspace(&probe.path().join("probe")) {
        if matches!(error.raw_os_error(), Some(libc::EOPNOTSUPP | libc::ENOSYS)) {
            eprintln!("SKIP {test_name}: filesystem lacks xattr/ACL support: {error}");
            return;
        }
        panic!("metadata fixture failed: {error}");
    }

    for backend in ["loop", "base"] {
        let operations: &[&str] = if fail_copy {
            &["init", "recover"]
        } else {
            &["init", "recover", "roundtrip"]
        };
        for operation in operations {
            let root = tempfile::tempdir().unwrap();
            let bin = root.path().join("bin");
            std::fs::create_dir(&bin).unwrap();
            // Only btrfs administration is simulated. Production backend code
            // still performs the copy, symlink swap, and source cleanup.
            executable(
                &bin.join("btrfs"),
                r#"#!/bin/sh
case "$1 $2" in
  'subvolume create') mkdir -p -- "$3" ;;
  'subvolume delete') rm -rf -- "$3" ;;
  'filesystem usage') printf 'Device size: 1000000000\nUsed: 1000000\nMetadata,single: Size:10000000, Used:100000\n' ;;
  'inspect-internal rootid') printf '256\n' ;;
  'filesystem sync'|'subvolume sync'|'subvolume list') ;;
  *) exit 2 ;;
esac
"#,
            );
            if fail_copy {
                executable(&bin.join("rsync"), "#!/bin/sh\nexit 23\n");
            }
            let mut paths = vec![bin];
            paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
            // Isolate PATH changes from concurrently running Rust tests.
            let output = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", test_name, "--nocapture"])
                .env(CASE_ROOT, root.path())
                .env("WS_CKPT_METADATA_TEST_BACKEND", backend)
                .env("WS_CKPT_METADATA_TEST_OPERATION", operation)
                .env("PATH", std::env::join_paths(paths).unwrap())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{backend}/{operation}:\n{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}

async fn run_case(root: PathBuf, fail_copy: bool) {
    let backend: Box<dyn StorageBackend> =
        if std::env::var("WS_CKPT_METADATA_TEST_BACKEND").unwrap() == "loop" {
            Box::new(BtrfsLoopBackend::new(
                root.join("data"),
                root.join("unused.img"),
            ))
        } else {
            Box::new(BtrfsBaseBackend::new(
                root.join("data"),
                BtrfsBaseScenario::CrossDisk,
            ))
        };
    std::fs::create_dir_all(backend.data_root()).unwrap();
    let workspace = root.join("workspace");
    let live = backend.data_root().join("ws-metadata");
    let snapshots = backend.snapshots_root().join("ws-metadata");
    let operation = std::env::var("WS_CKPT_METADATA_TEST_OPERATION").unwrap();
    if operation == "recover" {
        prepare_workspace(&live).unwrap();
        std::fs::create_dir_all(snapshots.join("retained-snapshot")).unwrap();
        std::fs::write(snapshots.join("retained-snapshot/data"), PAYLOAD).unwrap();
        std::os::unix::fs::symlink(&live, &workspace).unwrap();
    } else {
        prepare_workspace(&workspace).unwrap();
        let result = backend
            .init_workspace(workspace.to_str().unwrap(), "ws-metadata")
            .await;
        if fail_copy {
            assert!(result.is_err());
            assert_metadata(&workspace);
            assert!(!live.exists());
            return;
        }
        result.unwrap();
        assert!(!root.join("workspace.pre-init-bak").exists());
    }

    if operation != "init" {
        let result = backend
            .recover_workspace("ws-metadata", workspace.to_str().unwrap())
            .await;
        if fail_copy {
            assert!(result.is_err());
            assert_metadata(&live);
            assert_eq!(
                std::fs::read(snapshots.join("retained-snapshot/data")).unwrap(),
                PAYLOAD
            );
            return;
        }
        result.unwrap();
        assert!(!live.exists());
        assert!(!snapshots.exists());
    }
    assert_metadata(&workspace);
}

#[test]
fn rsync_workspace_copies_preserve_metadata() {
    run_cases("rsync_workspace_copies_preserve_metadata", false);
}

#[test]
fn failed_rsync_preserves_source_metadata() {
    run_cases("failed_rsync_preserves_source_metadata", true);
}
