//! Kernel-visible process name after the adapter's trusted fd-exec launch.

#![cfg(target_os = "linux")]

use std::fs::{self, File};
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const EXPECTED_NAME: &str = "cosh-core";

/// Spawns the binary through `/proc/self/fd/<n>` the way cosh-shell does.
///
/// `extra_descriptors` shifts the descriptor number so the caller can prove the
/// reported name does not follow the shell's open file count. The child keeps
/// its stdin pipe open, so `--session-control` blocks until the caller kills it.
fn spawn_through_descriptor(extra_descriptors: usize) -> (Child, i32) {
    let program = env!("CARGO_BIN_EXE_cosh-core");
    let padding: Vec<File> = (0..extra_descriptors)
        .map(|_| File::open(program).expect("open padding descriptor"))
        .collect();
    let executable = File::open(program).expect("open cosh-core");
    let descriptor = executable.as_raw_fd();
    let mut command = Command::new(format!("/proc/self/fd/{descriptor}"));
    command.arg0(program);
    command.arg("--session-control");
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = command
        .spawn()
        .expect("spawn cosh-core through its descriptor");
    drop((padding, executable));
    (child, descriptor)
}

fn wait_for_name(child: &mut Child, descriptor: i32, extra_descriptors: usize) {
    let pid = child.id();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait().expect("poll cosh-core") {
            let mut stderr = String::new();
            if let Some(mut pipe) = child.stderr.take() {
                let _ = pipe.read_to_string(&mut stderr);
            }
            panic!(
                "descriptor {descriptor} exited before reporting a name: {status}, stderr {stderr:?}"
            );
        }
        let comm = fs::read_to_string(format!("/proc/{pid}/comm"))
            .ok()
            .map(|comm| comm.trim().to_owned());
        if comm.as_deref() == Some(EXPECTED_NAME) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "descriptor {descriptor} with {extra_descriptors} extra descriptors: comm settled at {comm:?}"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn fd_exec_spawn_names_process_cosh_core() {
    let mut descriptors = Vec::new();
    for extra_descriptors in [0, 4, 12] {
        let (mut child, descriptor) = spawn_through_descriptor(extra_descriptors);
        descriptors.push(descriptor);
        wait_for_name(&mut child, descriptor, extra_descriptors);
        let _ = child.kill();
        let _ = child.wait();
    }

    descriptors.sort_unstable();
    descriptors.dedup();
    assert!(
        descriptors.len() > 1,
        "the cases did not use different descriptors: {descriptors:?}"
    );
}
