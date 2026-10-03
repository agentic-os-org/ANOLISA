//! Live-mount coverage for mountpoints whose paths contain characters the
//! kernel octal-escapes in `/proc/mounts` (space, tab, newline, backslash).
//!
//! The kernel writes field 2 of `/proc/mounts` escaped (`\040` for space,
//! `\011` tab, `\012` newline, `\134` backslash — `mangle()` in
//! `fs/proc_namespace.c`), so a probe that compares the raw mountpoint
//! string against the raw field is blind to exactly those mounts. These
//! tests reproduce both failure shapes from the audit with real FUSE mounts:
//!
//! * a **managed** mount at a spaced path must reach readiness (and its
//!   `stop` must tear it down);
//! * `stop` against a **live (non-managed) mount** at a spaced path must
//!   unmount it, not report "already stopped" while the mount persists.
//!
//! Both tests need `/dev/fuse` + `fusermount3`; they skip (with a message)
//! when FUSE is unavailable, mirroring the gate in `cli_startup_tests.rs`.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output};
use std::time::{Duration, Instant};

/// The kernel's octal escapes for `/proc/mounts` field characters, applied to
/// build the expected escaped field. Independent oracle: this is how the
/// kernel serializes the mount line, not how the CLI decodes it.
fn escaped_mount_field(path: &str) -> String {
    path.replace('\\', "\\134")
        .replace(' ', "\\040")
        .replace('\t', "\\011")
        .replace('\n', "\\012")
}

/// True when `path` is listed in `/proc/mounts`, comparing against the
/// kernel-escaped form of field 2.
fn mount_listed(path: &Path) -> bool {
    let expected = escaped_mount_field(&path.to_string_lossy());
    std::fs::read_to_string("/proc/mounts")
        .map(|mounts| {
            mounts
                .lines()
                .any(|line| line.split_whitespace().nth(1) == Some(expected.as_str()))
        })
        .unwrap_or(false)
}

/// Best-effort FUSE availability gate (mirrors `cli_startup_tests.rs`).
fn fuse_available() -> bool {
    Path::new("/dev/fuse").exists()
        && Command::new("fusermount3")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
}

fn combined(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    )
}

/// Bounded, best-effort unmount so a failing assertion never leaks a mount.
fn force_unmount(path: &Path) {
    for _ in 0..50 {
        if !mount_listed(path) {
            return;
        }
        let mp = path.to_string_lossy();
        let _ = Command::new("fusermount3").args(["-u", &mp]).output();
        let _ = Command::new("fusermount3").args(["-u", "-z", &mp]).output();
        let _ = Command::new("umount").args(["-l", &mp]).output();
        std::thread::sleep(Duration::from_millis(100));
    }
    if mount_listed(path) {
        eprintln!("WARN: leaked SkillFS FUSE mount at {}", path.display());
    }
}

/// SIGTERM the child, wait for exit, then force-unmount as a fallback.
fn stop_child(child: &mut Child, mountpoint: &Path) {
    let pid = child.id().to_string();
    let _ = Command::new("kill").args(["-TERM", &pid]).status();
    for _ in 0..50 {
        if matches!(child.try_wait(), Ok(Some(_))) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    force_unmount(mountpoint);
    let _ = child.kill();
    let _ = child.wait();
}

/// A source tree with one valid skill plus a mountpoint whose path contains
/// a space (the character the audit reproduced the blind probe with).
fn spaced_source_and_mountpoint(tag: &str) -> (tempfile::TempDir, PathBuf) {
    let source = tempfile::tempdir().expect("source tempdir");
    let skill = source.path().join("demo-skill");
    std::fs::create_dir_all(&skill).expect("create skill dir");
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: demo-skill\ndescription: fixture\n---\nbody\n",
    )
    .expect("write SKILL.md");

    let mount_parent = tempfile::tempdir().expect("mount tempdir");
    // The literal directory name embeds a space, so the mountpoint path does
    // too and `/proc/mounts` shows it escaped.
    let mountpoint = mount_parent.path().join(format!("{tag} dir/mp"));
    std::fs::create_dir_all(&mountpoint).expect("create spaced mountpoint");
    (source, mountpoint)
}

fn wait_until_listed(mountpoint: &Path, budget: Duration) -> bool {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if mount_listed(mountpoint) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

fn wait_until_gone(mountpoint: &Path, budget: Duration) -> bool {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if !mount_listed(mountpoint) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

#[test]
fn managed_mount_at_spaced_path_becomes_ready_and_stops() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE unavailable; cannot mount at a spaced path");
        return;
    }
    let (source, mountpoint) = spaced_source_and_mountpoint("audit");

    // Before the octal decode fix this never saw the mount: the client spun
    // until READY_TIMEOUT_MS and failed with "did not become ready".
    let out = Command::new(env!("CARGO_BIN_EXE_skillfs"))
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mountpoint.to_str().unwrap(),
            "--managed",
        ])
        .output()
        .expect("invoke skillfs mount --managed");

    assert!(
        out.status.success(),
        "managed mount at a spaced path must become ready, stdout/stderr={}",
        combined(&out)
    );
    assert!(
        combined(&out).contains("managed mount ready"),
        "expected readiness message, got: {}",
        combined(&out)
    );
    assert!(
        mount_listed(&mountpoint),
        "mount at {} must be listed in /proc/mounts",
        mountpoint.display()
    );

    // Managed stop must tear the spaced-path instance down too.
    let stop = Command::new(env!("CARGO_BIN_EXE_skillfs"))
        .args(["stop", mountpoint.to_str().unwrap()])
        .output()
        .expect("invoke skillfs stop");
    assert!(
        stop.status.success(),
        "stop must succeed, stdout/stderr={}",
        combined(&stop)
    );
    assert!(
        combined(&stop).contains("stopped managed mount"),
        "stop must report the managed teardown, got: {}",
        combined(&stop)
    );
    assert!(
        wait_until_gone(&mountpoint, Duration::from_secs(10)),
        "mount at {} must be gone after stop",
        mountpoint.display()
    );
    force_unmount(&mountpoint);
}

#[test]
fn stop_reports_live_spaced_mount_and_unmounts_it() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE unavailable; cannot mount at a spaced path");
        return;
    }
    let (source, mountpoint) = spaced_source_and_mountpoint("live");

    let mut child = Command::new(env!("CARGO_BIN_EXE_skillfs"))
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mountpoint.to_str().unwrap(),
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn foreground skillfs mount");

    // Bring up a real (non-managed) FUSE mount at the spaced path. Before the
    // fix this is the trap: the mount IS listed (escaped), yet a raw-field
    // probe claims it is not.
    let listed = wait_until_listed(&mountpoint, Duration::from_secs(15));
    if !listed {
        stop_child(&mut child, &mountpoint);
        panic!(
            "FUSE is available but the mount at {} never appeared in /proc/mounts",
            mountpoint.display()
        );
    }

    let stop = Command::new(env!("CARGO_BIN_EXE_skillfs"))
        .args(["stop", mountpoint.to_str().unwrap()])
        .output()
        .expect("invoke skillfs stop");

    // Capture the outcome before cleanup so a failing assertion cannot leak
    // the mount.
    let stop_report = combined(&stop);
    let gone = wait_until_gone(&mountpoint, Duration::from_secs(10));
    stop_child(&mut child, &mountpoint);

    assert!(
        stop.status.success(),
        "stop must succeed, stdout/stderr={stop_report}"
    );
    assert!(
        !stop_report.contains("already stopped"),
        "stop must not claim a live spaced-path mount is already stopped, got: {stop_report}"
    );
    assert!(
        stop_report.contains("unmounted"),
        "stop must report the unmount of the live mount, got: {stop_report}"
    );
    assert!(
        gone,
        "the live mount at {} must actually be gone after stop",
        mountpoint.display()
    );
}
