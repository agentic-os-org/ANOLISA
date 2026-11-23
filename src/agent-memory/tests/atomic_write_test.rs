use agent_memory::safe_fs;
use agent_memory::{config::AppConfig, service::MemoryService};
use std::os::fd::AsFd;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

fn setup() -> (tempfile::TempDir, MemoryService) {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = AppConfig::default();
    cfg.memory.paths.base_dir = tmp.path().join("data").to_string_lossy().into_owned();
    cfg.memory.session.base_dir = tmp.path().join("sessions").to_string_lossy().into_owned();
    cfg.memory.mount.strategy = agent_memory::mount::MountStrategyKind::Userland;
    cfg.memory.index.enabled = false;
    cfg.memory.git.enabled = false;
    (tmp, MemoryService::new(cfg).unwrap())
}

#[test]
fn failed_edit_preserves_original_content() {
    // Resource limits and signal dispositions affect the process, so isolate
    // this probe in a dedicated child, never the whole test runner.
    if std::env::var_os("ANOLISA_AUDIT_EDIT_CHILD").is_none() {
        for operation in ["edit", "overwrite"] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "failed_edit_preserves_original_content",
                    "--nocapture",
                ])
                .env("ANOLISA_AUDIT_EDIT_CHILD", operation)
                .output()
                .unwrap();
            print!("{}", String::from_utf8_lossy(&output.stdout));
            eprint!("{}", String::from_utf8_lossy(&output.stderr));
            assert!(
                String::from_utf8_lossy(&output.stdout).contains("atomic-write-child-completed")
            );
            assert!(
                output.status.success(),
                "failed edit destroyed the original contents"
            );
        }
        return;
    }
    let (_tmp, svc) = setup();
    let original = "original important memory";
    svc.write("notes/important.md", original, false).unwrap();
    let replacement = "N".repeat(128);
    let mut old_limit = nix::libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: valid local pointers; this child owns the temporary soft limit
    // and restores both process settings before any assertion.
    let old_signal = unsafe {
        assert_eq!(
            nix::libc::getrlimit(nix::libc::RLIMIT_FSIZE, &mut old_limit),
            0
        );
        let signal = nix::libc::signal(nix::libc::SIGXFSZ, nix::libc::SIG_IGN);
        let limit = nix::libc::rlimit {
            rlim_cur: 32.min(old_limit.rlim_max),
            rlim_max: old_limit.rlim_max,
        };
        assert_eq!(nix::libc::setrlimit(nix::libc::RLIMIT_FSIZE, &limit), 0);
        signal
    };
    let result = if std::env::var("ANOLISA_AUDIT_EDIT_CHILD").unwrap() == "edit" {
        svc.edit("notes/important.md", original, &replacement)
    } else {
        svc.write("notes/important.md", &replacement, true)
            .map(|_| ())
    };
    unsafe {
        assert_eq!(nix::libc::setrlimit(nix::libc::RLIMIT_FSIZE, &old_limit), 0);
        nix::libc::signal(nix::libc::SIGXFSZ, old_signal);
    }
    let actual = svc.read("notes/important.md").unwrap();
    assert_eq!(
        std::fs::read_dir(svc.mount.root.join("notes"))
            .unwrap()
            .count(),
        1,
        "failed preparation left a temporary file"
    );
    println!(
        "edit result: {result:?}; remaining {} bytes: {actual:?}",
        actual.len()
    );
    assert!(result.is_err(), "the kernel must reject this write");
    assert_eq!(
        actual, original,
        "a failed edit must retain the previous memory"
    );
    assert!(!std::fs::read_dir(&svc.mount.meta_dir).unwrap().any(|e| {
        e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".memory-write-")
    }));
    println!("atomic-write-child-completed");
}

#[test]
fn edit_without_io_failure_succeeds() {
    let (_tmp, svc) = setup();
    svc.write("notes/control.md", "original", false).unwrap();
    svc.edit("notes/control.md", "original", &"N".repeat(128))
        .unwrap();
    assert_eq!(svc.read("notes/control.md").unwrap(), "N".repeat(128));
}

#[test]
fn atomic_write_preserves_metadata_and_replaces_only_named_link() {
    let (_tmp, svc) = setup();
    svc.write("note.md", "old", false).unwrap();
    let path = svc.mount.root.join("note.md");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
    std::fs::hard_link(&path, svc.mount.root.join("alias.md")).unwrap();
    let before = std::fs::metadata(&path).unwrap();
    let name = std::ffi::CString::new("user.memory-test").unwrap();
    let cpath = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: the CString arguments and literal value are valid buffers.
    let xattrs = xattr_supported(unsafe {
        nix::libc::setxattr(cpath.as_ptr(), name.as_ptr(), b"tag".as_ptr().cast(), 3, 0)
    });
    svc.write("note.md", "new", true).unwrap();
    let after = std::fs::metadata(&path).unwrap();
    assert_eq!(
        (after.uid(), after.gid(), after.mode()),
        (before.uid(), before.gid(), before.mode())
    );
    assert_ne!(after.ino(), before.ino());
    assert_eq!(svc.read("note.md").unwrap(), "new");
    assert_eq!(svc.read("alias.md").unwrap(), "old");
    if !xattrs {
        return;
    }
    let mut value = [0u8; 3];
    // SAFETY: value is writable for its supplied length.
    assert_eq!(
        unsafe {
            nix::libc::getxattr(
                cpath.as_ptr(),
                name.as_ptr(),
                value.as_mut_ptr().cast(),
                value.len(),
            )
        },
        3
    );
    assert_eq!(&value, b"tag");
}

#[test]
fn atomic_write_rejects_symlinks_and_directories() {
    let tmp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("file"), "untouched").unwrap();
    std::os::unix::fs::symlink(outside.path().join("file"), tmp.path().join("link")).unwrap();
    std::os::unix::fs::symlink(outside.path(), tmp.path().join("parent")).unwrap();
    std::fs::create_dir(tmp.path().join("dir")).unwrap();
    std::fs::create_dir(tmp.path().join(".anolisa")).unwrap();
    let root = safe_fs::open_root(tmp.path()).unwrap();
    for path in ["link", "parent/file", "dir", "../escape"] {
        assert!(
            safe_fs::write_atomic(root.as_fd(), Path::new(".anolisa"), Path::new(path), b"new")
                .is_err(),
            "{path}"
        );
    }
    assert_eq!(
        std::fs::read_to_string(outside.path().join("file")).unwrap(),
        "untouched"
    );
}

#[test]
fn atomic_write_rejects_readonly_file_without_replacing_it() {
    if nix::unistd::geteuid().is_root() {
        return;
    }
    let (_tmp, svc) = setup();
    svc.write("note.md", "old", false).unwrap();
    let path = svc.mount.root.join("note.md");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
    assert!(svc.write("note.md", "new", true).is_err());
    assert_eq!(svc.read("note.md").unwrap(), "old");
}

#[test]
fn atomic_write_requires_existing_file_and_accepts_empty_replacement() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join(".anolisa")).unwrap();
    let root = safe_fs::open_root(tmp.path()).unwrap();
    assert!(matches!(
        safe_fs::write_atomic(
            root.as_fd(),
            Path::new(".anolisa"),
            Path::new("new.md"),
            b"hello"
        ),
        Err(agent_memory::error::MemoryError::NotFound(_))
    ));
    std::fs::write(tmp.path().join("new.md"), "old").unwrap();
    assert_eq!(
        safe_fs::write_atomic(
            root.as_fd(),
            Path::new(".anolisa"),
            Path::new("new.md"),
            b""
        )
        .unwrap(),
        0
    );
    assert_eq!(std::fs::read(tmp.path().join("new.md")).unwrap(), b"");
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 2);
}

#[test]
fn atomic_write_preserves_posix_acl() {
    let (_tmp, svc) = setup();
    svc.write("acl.md", "old", false).unwrap();
    let path = std::ffi::CString::new(svc.mount.root.join("acl.md").as_os_str().as_encoded_bytes())
        .unwrap();
    let name = std::ffi::CString::new("system.posix_acl_access").unwrap();
    // Linux posix_acl_xattr_header / posix_acl_xattr_entry, version 2:
    // owner rw, named uid 65534 r, owning group none, mask r, other none.
    let mut acl = 2u32.to_le_bytes().to_vec();
    for (tag, permission, id) in [
        (1u16, 6u16, u32::MAX),
        (2, 4, 65534),
        (4, 0, u32::MAX),
        (16, 4, u32::MAX),
        (32, 0, u32::MAX),
    ] {
        acl.extend_from_slice(&tag.to_le_bytes());
        acl.extend_from_slice(&permission.to_le_bytes());
        acl.extend_from_slice(&id.to_le_bytes());
    }
    // SAFETY: path/name are terminated; acl is readable for its length.
    if !xattr_supported(unsafe {
        nix::libc::setxattr(
            path.as_ptr(),
            name.as_ptr(),
            acl.as_ptr().cast(),
            acl.len(),
            0,
        )
    }) {
        return;
    }
    svc.edit("acl.md", "old", "new").unwrap();
    let mut actual = vec![0u8; acl.len()];
    // SAFETY: actual is writable for its supplied length.
    let read = unsafe {
        nix::libc::getxattr(
            path.as_ptr(),
            name.as_ptr(),
            actual.as_mut_ptr().cast(),
            actual.len(),
        )
    };
    assert_eq!(read, acl.len() as isize);
    assert_eq!(actual, acl);
}

#[test]
fn atomic_write_clears_set_id_bits() {
    let (_tmp, svc) = setup();
    svc.write("script.sh", "old", false).unwrap();
    let path = svc.mount.root.join("script.sh");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o6755)).unwrap();
    svc.edit("script.sh", "old", "new").unwrap();
    assert_eq!(
        std::fs::metadata(path).unwrap().permissions().mode() & 0o7777,
        0o755
    );
}

fn xattr_supported(result: i32) -> bool {
    if result == 0 {
        return true;
    }
    let error = std::io::Error::last_os_error();
    if matches!(
        error.raw_os_error(),
        Some(nix::libc::ENOTSUP | nix::libc::ENOSYS)
    ) {
        eprintln!("attribute-specific assertions unavailable: {error}");
        return false;
    }
    panic!("cannot prepare xattr fixture: {error}");
}

#[test]
fn metadata_staging_is_absent_from_list_grep_git_and_snapshots() {
    use agent_memory::tools::{grep::GrepOptions, list::ListOptions};
    let (_tmp, svc) = setup();
    svc.write("note.md", "old", false).unwrap();
    agent_memory::git_repo::init(&svc.mount.root).unwrap();
    // Simulate a process killed with a staged replacement still on disk.
    let residue = svc.mount.meta_dir.join(".memory-write-crashed.tmp");
    std::fs::write(&residue, "ONLY_TEMP_SENTINEL").unwrap();
    assert!(
        !svc.list(
            "",
            ListOptions {
                recursive: true,
                ..Default::default()
            }
        )
        .unwrap()
        .iter()
        .any(|entry| entry.path.contains(".memory-write-"))
    );
    assert!(
        svc.grep("ONLY_TEMP_SENTINEL", GrepOptions::default())
            .unwrap()
            .is_empty()
    );
    let repo = git2::Repository::open(&svc.mount.root).unwrap();
    let mut index = repo.index().unwrap();
    index
        .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    assert!(
        !index
            .iter()
            .any(|entry| entry.path.starts_with(b".anolisa/"))
    );
    let snap = svc.mem_snapshot(None).unwrap();
    let file = std::fs::File::open(
        svc.mount
            .meta_dir
            .join("snapshots")
            .join(format!("{}.tar.gz", snap.id)),
    )
    .unwrap();
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(file));
    assert!(!archive.entries().unwrap().any(|entry| {
        entry
            .unwrap()
            .path()
            .unwrap()
            .to_string_lossy()
            .contains(".memory-write-")
    }));
    svc.edit("note.md", "old", "new").unwrap();
    assert_eq!(svc.read("note.md").unwrap(), "new");
    assert!(
        residue.exists(),
        "unowned residue is never swept during an edit"
    );
}

#[test]
fn new_overwrite_keeps_destination_parent_group() {
    let (_tmp, svc) = setup();
    svc.mkdir("shared").unwrap();
    let parent = svc.mount.root.join("shared");
    // A supplementary group plus setgid exercises inheritance independently
    // from the metadata directory's group, without another user's privileges.
    if let Some(gid) = nix::unistd::getgroups()
        .unwrap()
        .into_iter()
        .find(|g| *g != nix::unistd::getegid())
    {
        nix::unistd::chown(&parent, None, Some(gid)).unwrap();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o2770)).unwrap();
        svc.write("shared/new.md", "new", true).unwrap();
        assert_eq!(
            std::fs::metadata(parent.join("new.md")).unwrap().gid(),
            gid.as_raw()
        );
    }
}

#[test]
fn actual_replacement_creates_its_staging_file_in_metadata_only() {
    use notify::{RecursiveMode, Watcher};
    let (_tmp, svc) = setup();
    svc.write("note.md", "old", false).unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    let mut watcher = notify::recommended_watcher(send).unwrap();
    watcher
        .watch(&svc.mount.root, RecursiveMode::NonRecursive)
        .unwrap();
    watcher
        .watch(&svc.mount.meta_dir, RecursiveMode::NonRecursive)
        .unwrap();
    svc.edit("note.md", "old", "new").unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let event = receive
            .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
            .expect("kernel did not report staged file creation")
            .unwrap();
        if !matches!(event.kind, notify::EventKind::Create(_)) {
            continue;
        }
        for path in event.paths {
            if path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".memory-write-")
            {
                assert_eq!(path.parent(), Some(svc.mount.meta_dir.as_path()));
                return;
            }
        }
    }
}
