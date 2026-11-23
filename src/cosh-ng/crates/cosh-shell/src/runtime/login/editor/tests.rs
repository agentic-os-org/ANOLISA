use super::*;

fn control() -> Control {
    Control {
        username: "alice".into(),
        uid: 1001,
        expected_shell: "/bin/bash".into(),
        target_shell: COSH_PATH.into(),
    }
}

#[test]
fn native_editor_replaces_only_the_exact_account_shell_and_preserves_other_bytes() {
    let original=b"root:x:0:0:Root:/root:/bin/bash\nalice:x:1001:1001:Alice:/home/alice:/bin/bash\nbob:x:1002:1002:B:/home/bob:/bin/zsh";
    let expected=b"root:x:0:0:Root:/root:/bin/bash\nalice:x:1001:1001:Alice:/home/alice:/usr/bin/cosh\nbob:x:1002:1002:B:/home/bob:/bin/zsh";
    assert_eq!(replace_shell(original, &control()).unwrap(), expected);
    let mut restore = control();
    restore.expected_shell = COSH_PATH.into();
    restore.target_shell = "/bin/bash".into();
    assert_eq!(replace_shell(expected, &restore).unwrap(), original);
}

#[test]
fn native_editor_rejects_external_shell_uid_changes_and_duplicate_identities() {
    for bytes in [
        b"alice:x:1001:1001:A:/home/alice:/bin/zsh\n".as_slice(),
        b"alice:x:1002:1001:A:/home/alice:/bin/bash\n",
        b"alice:x:1001:1001:A:/home/alice:/bin/bash\nalice:x:1001:1001:A:/home/alice:/bin/bash\n",
        b"alice:x:1001:1001:A:/home/alice:/bin/bash\nother:x:1001:1001::/:/bin/bash\n",
        b"root:x:0:0::/root:/bin/bash\n",
    ] {
        assert!(
            replace_shell(bytes, &control()).is_err(),
            "{}",
            String::from_utf8_lossy(bytes)
        );
    }
}

#[test]
fn native_editor_file_preserves_inode_and_advances_mtime_even_with_a_same_second_edit() {
    use std::io::{Read, Seek, SeekFrom, Write};
    use std::os::unix::fs::MetadataExt;
    let temporary = tempfile::NamedTempFile::new().unwrap();
    let mut file = temporary.reopen().unwrap();
    file.write_all(b"alice:x:1001:1001:A:/home/alice:/bin/bash\n")
        .unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    let before = file.metadata().unwrap();
    assert_eq!(
        edit_file(&mut file, &control(), unsafe { nix::libc::geteuid() }),
        0
    );
    let after = file.metadata().unwrap();
    assert_eq!(before.ino(), after.ino());
    assert!(after.mtime() > before.mtime());
    file.seek(SeekFrom::Start(0)).unwrap();
    let mut text = String::new();
    file.read_to_string(&mut text).unwrap();
    assert_eq!(text, "alice:x:1001:1001:A:/home/alice:/usr/bin/cosh\n");
}

#[test]
fn native_editor_control_is_read_only_bounded_and_kept_out_of_editor_argv() {
    let mut request = control();
    request.username = "alice$".into();
    let mut pipe = control_pipe(&request).unwrap();
    let metadata = pipe.metadata().unwrap();
    let flags = unsafe { nix::libc::fcntl(pipe.as_raw_fd(), nix::libc::F_GETFL) };
    let decoded =
        read_control(&mut pipe, unsafe { nix::libc::geteuid() }).unwrap_or_else(|error| {
            panic!(
                "{error}; mode={:o} uid={} owner={} flags={flags} fifo={}",
                metadata.mode(),
                metadata.uid(),
                unsafe { nix::libc::geteuid() },
                metadata.file_type().is_fifo()
            )
        });
    assert_eq!(decoded.username, "alice$");
    assert_eq!(decoded.uid, 1001);
    assert_eq!(decoded.expected_shell, "/bin/bash");
    let mut command = Command::new("/fixture/vipw");
    configure_editor(&mut command, &pipe, 12345);
    assert_eq!(command.get_args().collect::<Vec<_>>(), ["-q", "-p"]);
    let values = command
        .get_envs()
        .filter_map(|(_, value)| value)
        .collect::<Vec<_>>();
    assert_eq!(values.len(), 2);
    for value in values {
        assert_eq!(value, "/proc/12345/exe --__cosh-login-editor-v1 3");
    }
}

#[test]
fn native_editor_rejects_untrusted_control_and_linked_temporary_files() {
    let owner = unsafe { nix::libc::geteuid() };
    let mut pipe = control_pipe(&control()).unwrap();
    assert!(read_control(&mut pipe, owner.wrapping_add(1)).is_err());
    let temporary = tempfile::NamedTempFile::new().unwrap();
    let link = temporary.path().with_extension("linked-editor-test");
    std::fs::hard_link(temporary.path(), &link).unwrap();
    let mut file = temporary.reopen().unwrap();
    assert_eq!(edit_file(&mut file, &control(), owner), 97);
    std::fs::remove_file(link).unwrap();
    let mut readonly = File::open(temporary.path()).unwrap();
    assert!(read_control(&mut readonly, owner).is_err());
}

#[test]
fn store_lease_survives_parent_release_until_the_editor_child_exits() {
    use super::super::{record::Phase, record::Record, transact, Action, Store};
    use crate::process::child::{CleanupScope, ReapOwner, COMMAND_POLL_INTERVAL};
    use crate::process::job::{configure_isolated_worker, JobDeadline};
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, Instant};

    let directory = tempfile::tempdir().unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let store = Store::fixture(directory.path());
    let account = account::Account {
        name: "alice".into(),
        uid: 1001,
        gid: 1001,
        home: "/home/alice".into(),
        shell: "/bin/bash".into(),
    };
    store
        .write(&Record {
            schema: 1,
            username: account.name.clone(),
            uid: account.uid,
            original_shell: account.shell.clone(),
            cosh_shell: COSH_PATH.into(),
            original_sha256: "a".repeat(64),
            phase: Phase::Prepared,
        })
        .unwrap();
    let lock = store.lock().unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "runtime::login::editor::tests::store_lease_child",
        ])
        .env("COSH_LOGIN_STORE_LEASE_FIXTURE", directory.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null());
    let inherited = inherit_store_lease(&mut command, &lock).unwrap();
    assert!(inherited.as_raw_fd() >= 4);
    assert_ne!(
        unsafe { nix::libc::fcntl(inherited.as_raw_fd(), nix::libc::F_GETFD) }
            & nix::libc::FD_CLOEXEC,
        0
    );
    command.env(
        "COSH_LOGIN_STORE_LEASE_FD",
        inherited.as_raw_fd().to_string(),
    );
    unsafe {
        command.pre_exec(|| {
            if nix::libc::dup2(0, CONTROL_FD) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let permit = ReapOwner::global().unwrap().reserve().unwrap();
    let deadline = JobDeadline::after(Duration::from_secs(3)).unwrap();
    let limit = deadline.instant().unwrap();
    configure_isolated_worker(&mut command, deadline);
    let child = command.spawn().unwrap();
    let mut child = permit.register(child, CleanupScope::OwnedGroup, limit);
    while !directory.path().join("ready").exists() {
        assert!(Instant::now() < limit && !child.has_exited().unwrap());
        std::thread::sleep(COMMAND_POLL_INTERVAL);
    }
    drop(inherited);
    drop(lock);
    assert!(!child.has_exited().unwrap());
    let restored = transact(
        Action::Restore,
        &account,
        &"a".repeat(64),
        &store,
        || Ok(()),
        || Ok(account.clone()),
        |_, _| panic!("pending editor must block another restore"),
    );
    assert!(
        restored.is_err(),
        "restore finalized before the editor child committed: {restored:?}"
    );
    assert_eq!(
        store.read(account.uid).unwrap().unwrap().phase,
        Phase::Prepared
    );
    std::fs::write(directory.path().join("release"), b"1").unwrap();
    while !child.has_exited().unwrap() {
        assert!(Instant::now() < limit);
        std::thread::sleep(COMMAND_POLL_INTERVAL);
    }
    assert!(child.cleanup().unwrap().unwrap().success());
    assert!(store.lock().is_ok());
}

#[test]
fn store_lease_child() {
    let Some(root) = std::env::var_os("COSH_LOGIN_STORE_LEASE_FIXTURE") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let fd: i32 = std::env::var("COSH_LOGIN_STORE_LEASE_FD")
        .unwrap()
        .parse()
        .unwrap();
    let flags = unsafe { nix::libc::fcntl(fd, nix::libc::F_GETFD) };
    assert!(fd >= 4 && flags >= 0 && flags & nix::libc::FD_CLOEXEC == 0);
    std::fs::write(root.join("ready"), b"1").unwrap();
    let limit = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while !root.join("release").exists() {
        assert!(std::time::Instant::now() < limit);
        std::thread::sleep(crate::process::child::COMMAND_POLL_INTERVAL);
    }
}
