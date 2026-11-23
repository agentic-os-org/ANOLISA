use super::*;
#[cfg(target_os = "linux")]
use nix::libc;
use std::sync::OnceLock;
use std::thread;

fn wait(worker: &mut BoundedWorker) -> io::Result<Vec<u8>> {
    loop {
        if let Some(result) = worker.poll_ready() {
            return result;
        }
        assert!(Instant::now() < worker.deadline);
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn complete_reply_requires_confirmed_group_cleanup() {
    static OWNER: OnceLock<ReapOwner> = OnceLock::new();
    let owner = OWNER.get_or_init(ReapOwner::new);
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut command = Command::new("/bin/sh");
    command
        .args(["-c", "printf 'receipt\\n'; sleep 1"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    configure_isolated_worker(
        &mut command,
        JobDeadline::at_instant(deadline, Duration::from_secs(2)).unwrap(),
    );
    let mut child = command.spawn().unwrap();
    let stdout = child.stdout.take().unwrap();
    set_nonblocking(stdout.as_raw_fd()).unwrap();
    let guard = owner
        .reserve()
        .unwrap()
        .register(child, CleanupScope::OwnedGroup, Instant::now());
    let mut worker = BoundedWorker {
        child: Some(guard),
        stdout: Some(stdout),
        bytes: Vec::new(),
        deadline,
        limit: MAX_FRAME,
        sealed: false,
    };
    let result = wait(&mut worker);
    drop(worker);
    let end = Instant::now() + Duration::from_secs(1);
    while owner.reap_pending() {
        assert!(Instant::now() < end, "test child remained unreaped");
        thread::sleep(Duration::from_millis(1));
    }
    assert!(
        result.is_err(),
        "an unconfirmed cleanup cannot publish a receipt"
    );
}

#[test]
fn bounded_worker_returns_one_complete_reply_without_waiting_for_eof() {
    let _guard = crate::diagnostics::test_env::env_guard();
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "cat; sleep 2"]);
    let mut worker = BoundedWorker::spawn(
        command,
        b"receipt\n",
        Instant::now() + Duration::from_secs(1),
        MAX_FRAME,
    )
    .unwrap();
    assert_eq!(wait(&mut worker).unwrap(), b"receipt");
    assert!(worker.poll_ready().is_none());
}

#[test]
fn oversized_request_is_rejected_before_the_child_starts() {
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("started");
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "touch \"$1\"", "sh", marker.to_str().unwrap()]);
    assert!(BoundedWorker::spawn(
        command,
        &[b'a'; MAX_FRAME + 1],
        Instant::now() + Duration::from_secs(1),
        MAX_FRAME
    )
    .is_err());
    assert!(!marker.exists());
}

#[test]
fn oversized_reply_is_rejected_while_the_writer_is_still_running() {
    let _guard = crate::diagnostics::test_env::env_guard();
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "while :; do printf 0123456789; done"]);
    let mut worker =
        BoundedWorker::spawn(command, b"", Instant::now() + Duration::from_secs(1), 32).unwrap();
    assert_eq!(
        wait(&mut worker).unwrap_err().raw_os_error(),
        Some(nix::libc::EFBIG)
    );
}

#[cfg(target_os = "linux")]
fn reject_close_range(command: &mut Command, errno: i32) {
    use std::os::unix::process::CommandExt;
    unsafe {
        command.pre_exec(move || {
            let filter = [
                libc::sock_filter {
                    code: 0x20,
                    jt: 0,
                    jf: 0,
                    k: 0,
                },
                libc::sock_filter {
                    code: 0x15,
                    jt: 0,
                    jf: 1,
                    k: libc::SYS_close_range as u32,
                },
                libc::sock_filter {
                    code: 0x06,
                    jt: 0,
                    jf: 0,
                    k: libc::SECCOMP_RET_ERRNO | errno as u32,
                },
                libc::sock_filter {
                    code: 0x06,
                    jt: 0,
                    jf: 0,
                    k: libc::SECCOMP_RET_ALLOW,
                },
            ];
            let program = libc::sock_fprog {
                len: filter.len() as u16,
                filter: filter.as_ptr() as *mut _,
            };
            if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) < 0
                || libc::prctl(libc::PR_SET_SECCOMP, libc::SECCOMP_MODE_FILTER, &program) < 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(target_os = "linux")]
#[test]
fn cloexec_fallback_covers_descriptors_above_a_lowered_soft_limit() {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::process::CommandExt;
    let file = std::fs::File::open("/dev/null").unwrap();
    let high = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_DUPFD, 256) };
    assert!(high >= 256);
    let high = unsafe { OwnedFd::from_raw_fd(high) };
    for errno in [libc::EINVAL, libc::ENOSYS] {
        let mut command = Command::new("/bin/sh");
        command.args([
            "-c",
            &format!("test ! -e /proc/self/fd/{}", high.as_raw_fd()),
        ]);
        reject_close_range(&mut command, errno);
        unsafe {
            command.pre_exec(|| {
                let mut limit: libc::rlimit = std::mem::zeroed();
                if libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) < 0 {
                    return Err(io::Error::last_os_error());
                }
                limit.rlim_cur = limit.rlim_cur.min(64);
                if libc::setrlimit(libc::RLIMIT_NOFILE, &limit) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        configure_isolated_worker(
            &mut command,
            JobDeadline::after(Duration::from_secs(1)).unwrap(),
        );
        configure_cloexec(&mut command, 3).unwrap();
        assert!(command.status().unwrap().success());
    }
    assert!(unsafe { libc::fcntl(high.as_raw_fd(), libc::F_GETFD) } >= 0);
}

#[cfg(target_os = "linux")]
#[test]
fn cloexec_fallback_preserves_spawn_errors_and_security_rejections() {
    for (errno, executable, expected) in [
        (libc::ENOSYS, "/cosh-nonexistent-worker", libc::ENOENT),
        (libc::EPERM, "/bin/true", libc::EPERM),
    ] {
        let mut command = Command::new(executable);
        reject_close_range(&mut command, errno);
        configure_isolated_worker(
            &mut command,
            JobDeadline::after(Duration::from_secs(1)).unwrap(),
        );
        configure_cloexec(&mut command, 3).unwrap();
        assert_eq!(command.spawn().unwrap_err().raw_os_error(), Some(expected));
    }
}
