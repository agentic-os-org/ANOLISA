// SPDX-License-Identifier: Apache-2.0
//! A consumer that stops reading early, or a stdout that cannot take the
//! bytes, must reach the README's exit codes and stderr JSON instead of a Rust
//! panic or a silent success.
#![cfg(target_os = "linux")]

use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::process::ExitStatusExt;
use std::process::{Command, Stdio};

/// The JSON `ktuner check` prints on this host; `None` when it has less than
/// one page to write (an error body goes to stderr, so this also covers the
/// error exit).
fn check_json_len() -> Option<usize> {
    let out = Command::new(env!("CARGO_BIN_EXE_ktuner"))
        .arg("check")
        .output()
        .expect("run ktuner check");
    (out.stdout.len() > 4096).then_some(out.stdout.len())
}

/// A stdout that fails every write: `/dev/full` answers ENOSPC.
fn full_stdout() -> Option<std::fs::File> {
    std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/full")
        .ok()
}

#[test]
fn a_closed_stdout_does_not_panic_the_cli() {
    // Precondition, not the assertion: the one-page pipe can only produce the
    // condition when the command has more than one page to write. A host with
    // nothing to recommend cannot exercise it (and does not need the fix).
    let Some(json_len) = check_json_len() else {
        eprintln!("skipping: `ktuner check` has no multi-page JSON report on this host");
        return;
    };

    let mut child = Command::new(env!("CARGO_BIN_EXE_ktuner"))
        .arg("check")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn ktuner check");
    let mut stdout = child.stdout.take().expect("piped stdout");

    // Shrink the pipe to one page: `pipe(7)` documents that capacity for a
    // user at the pipe-page soft limit, and F_SETPIPE_SZ rounds up to the
    // system page size, so read back what the kernel granted and fail loudly
    // if it is not smaller than the report this host produces.
    let granted = unsafe { libc::fcntl(stdout.as_raw_fd(), libc::F_SETPIPE_SZ, 4096) };
    assert!(granted >= 4096, "F_SETPIPE_SZ failed");
    if (granted as usize) >= json_len {
        eprintln!(
            "skipping: the granted pipe ({} bytes) holds the whole report",
            granted
        );
        let _ = child.kill();
        let _ = child.wait();
        return;
    }
    let actual = unsafe { libc::fcntl(stdout.as_raw_fd(), libc::F_GETPIPE_SZ) };
    assert!(actual >= 4096, "F_GETPIPE_SZ failed");

    // The consumer reads a little and goes away.
    let mut head = [0u8; 16];
    let _ = stdout.read(&mut head);
    drop(stdout);

    let output = child.wait_with_output().expect("wait for ktuner check");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("panicked"),
        "a closed stdout must not panic the CLI; stderr was:\n{stderr}"
    );
    assert_ne!(
        output.status.code(),
        Some(101),
        "the panic exit code replaced the command's own status; stderr was:\n{stderr}"
    );
    assert!(
        output.status.signal().is_none(),
        "the command was killed by a signal instead of finishing"
    );
}

#[test]
fn a_failed_help_write_is_an_error_not_a_success() {
    let Some(full) = full_stdout() else {
        eprintln!("skipping: /dev/full is not available on this host");
        return;
    };

    // Control: the same help text through a working stdout is still a success.
    let control = Command::new(env!("CARGO_BIN_EXE_ktuner"))
        .arg("--help")
        .output()
        .expect("run ktuner --help");
    assert_eq!(control.status.code(), Some(0), "help text is not an error");
    assert!(!control.stdout.is_empty(), "clap rendered no help text");

    // A write error that is not a closed pipe (a full device) is a real error:
    // the README documents the details as stderr JSON with exit 2, and text
    // that never reached the consumer must not keep the success code.
    let failed = Command::new(env!("CARGO_BIN_EXE_ktuner"))
        .arg("--help")
        .stdout(Stdio::from(full))
        .output()
        .expect("run ktuner --help with a full stdout");
    assert_eq!(
        failed.status.code(),
        Some(2),
        "a failed help write must not exit 0"
    );
    let stderr = String::from_utf8_lossy(&failed.stderr);
    assert!(
        stderr.contains("\"error\""),
        "the write error must reach stderr as JSON; stderr was:\n{stderr}"
    );
    assert!(
        stderr.contains("stdout"),
        "the body must name the stream that failed (AGENTS.md 3.4); stderr was:\n{stderr}"
    );
}
