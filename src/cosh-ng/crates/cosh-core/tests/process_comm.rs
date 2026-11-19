//! Kernel comm identity of the real `cosh-core` binary under both spawn paths.
//!
//! cosh-shell launches trusted assets through `/proc/self/fd/N` (TOCTOU-safe
//! fd-exec), which makes the kernel derive comm from the fd path basename
//! instead of the executable name. Both spawn paths must leave comm as
//! `cosh-core` so `ps -C` / `pgrep -x` / OOM attribution keep working.

#![cfg(target_os = "linux")]

mod common;

use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use serde_json::Value;

fn spawn_core(home: &Path, fd_exec: bool) -> Child {
    let binary = common::binary_path();
    // Keep the pinned fd open across spawn, mirroring the shell's
    // trusted-asset launch: exec the fd path while argv[0] stays the real path.
    let pinned = fd_exec.then(|| File::open(&binary).expect("open core binary for fd-exec"));
    let mut command = match &pinned {
        Some(file) => {
            let mut command = Command::new(format!("/proc/self/fd/{}", file.as_raw_fd()));
            command.arg0(&binary);
            command
        }
        None => Command::new(&binary),
    };
    command.env("HOME", home);
    common::opt_out_telemetry(&mut command, home);
    // A mock provider keeps core out of the interactive auth exchange, so the
    // initialize handshake completes without a shell answering an auth request.
    std::fs::write(
        home.join(".copilot-shell").join("config.toml"),
        "[ai]\nactive_provider = \"mock\"\n\n[ai.providers.mock]\ntype = \"mock\"\n",
    )
    .expect("write mock provider config");
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn cosh-core")
}

fn comm_after_initialize(home: &Path, fd_exec: bool) -> String {
    let mut child = spawn_core(home, fd_exec);
    writeln!(
        child.stdin.as_mut().expect("stdin"),
        r#"{{"type":"control_request","request_id":"comm-init","request":{{"subtype":"initialize"}}}}"#
    )
    .expect("write initialize");

    let stdout = child.stdout.take().expect("stdout");
    let mut initialized = false;
    for line in BufReader::new(stdout).lines() {
        let line = line.expect("read core stdout");
        if line.trim().is_empty() {
            continue;
        }
        let message: Value = serde_json::from_str(&line).expect("valid JSONL output");
        if message["type"] == "control_response" && message["response"]["request_id"] == "comm-init"
        {
            initialized = true;
            break;
        }
    }
    assert!(
        initialized,
        "core must answer initialize before comm inspection"
    );

    let comm = std::fs::read_to_string(format!("/proc/{}/comm", child.id()))
        .expect("read child comm")
        .trim()
        .to_owned();
    child.kill().expect("kill core");
    child.wait().expect("reap core");
    comm
}

#[test]
fn fd_exec_spawned_core_comm_reflects_executable() {
    let home = tempfile::tempdir().expect("temp home");
    let comm = comm_after_initialize(home.path(), true);
    assert_eq!(
        comm, "cosh-core",
        "fd-exec spawn must restore executable comm; the kernel otherwise names the process after the fd path basename"
    );
}

#[test]
fn direct_exec_spawned_core_comm_reflects_executable() {
    let home = tempfile::tempdir().expect("temp home");
    let comm = comm_after_initialize(home.path(), false);
    assert_eq!(comm, "cosh-core");
}
