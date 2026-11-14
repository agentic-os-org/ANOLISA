//! Process handles retain identity and observe exit/access changes without cached verdicts.

use super::*;
use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, Command, Stdio},
    thread,
};

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn registration_caller_rejects_a_reused_peer_identity() {
    let owner = Process::read(std::process::id()).unwrap();
    let mut peer = owner.clone();
    owner.caller(&peer).unwrap();
    peer.start.push('0');
    assert!(owner.caller(&peer).is_err());
}

#[test]
fn start_time_and_namespace_changes_invalidate_process_association() {
    for watched in [false, true] {
        let mut process = Process::read(std::process::id()).unwrap();
        if watched {
            process = process.watch().unwrap();
            assert!(process.exit_notification().is_some());
        }
        process.check().unwrap();
        process.corroborate(std::process::id()).unwrap();
        let start = process.start.clone();
        process.start.push('0');
        assert!(process.check().is_err());
        process.start = start;
        let namespace = process.namespace.clone();
        process.namespace.push_str("-changed");
        assert!(process.check().is_err());
        process.namespace = namespace;
        process.uid = process.uid.wrapping_add(1);
        assert!(process.check().is_err());
    }
    assert!(Process::read(0).is_err());
}

#[test]
fn cloned_watches_check_concurrently_and_fallback_observes_exit() {
    let mut child = ChildGuard(Command::new("sleep").arg("20").spawn().unwrap());
    let fallback = Process::read(child.0.id()).unwrap();
    let watched = fallback.clone().watch().unwrap();
    assert!(watched.exit_notification().is_some());
    thread::scope(|scope| {
        for _ in 0..4 {
            let process = watched.clone();
            scope.spawn(move || {
                for _ in 0..50 {
                    process.check().unwrap();
                }
            });
        }
    });
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    assert!(watched.check().is_err());
    assert!(fallback.check().is_err());
    use std::os::fd::AsRawFd;
    let exit = watched.exit_notification().unwrap();
    let mut poll = libc::pollfd {
        fd: exit.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: the retained pidfd and pollfd are live for this call.
    assert_eq!(unsafe { libc::poll(&mut poll, 1, 0) }, 1);
    assert_ne!(poll.revents & libc::POLLIN, 0);
}

#[test]
fn retained_descriptors_do_not_hide_procfs_access_revocation() {
    let script =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/process-watch.py");
    let mut child = ChildGuard(
        Command::new("python3")
            .arg("-I")
            .arg(script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut output = BufReader::new(child.0.stdout.take().unwrap());
    let mut line = String::new();
    output.read_line(&mut line).unwrap();
    assert_eq!(line, "ready\n");
    let fallback = Process::read(child.0.id()).unwrap();
    let watched = fallback.clone().watch().unwrap();
    assert!(watched.exit_notification().is_some());
    child
        .0
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"change\n")
        .unwrap();
    line.clear();
    output.read_line(&mut line).unwrap();
    assert_eq!(line, "changed\n");
    assert!(fallback.check().is_err());
    assert!(watched.check().is_err());
}
