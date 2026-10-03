//! Signal-shutdown teardown of a foreground mount whose mountpoint contains
//! bytes that are not valid UTF-8.
//!
//! The foreground `skillfs mount` handles Ctrl+C / SIGTERM by invoking
//! `fusermount3 -u` on the mountpoint before returning success
//! (`trigger_unmount` in `src/main.rs`). That invocation must address the
//! raw mountpoint bytes: a `to_string_lossy()` view replaces an invalid
//! byte with U+FFFD, so `fusermount3` unmounts a nonexistent path, the
//! handler still returns success, and the mount leaks while the process
//! exits. This regression pins both halves of the contract: the process
//! must exit AND the mount must be gone.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn bin_path() -> &'static str {
    env!("CARGO_BIN_EXE_skillfs")
}

/// Byte-exact mount probe, same matcher the production teardown uses.
fn is_mounted(path: &Path) -> bool {
    let Ok(mounts) = std::fs::read("/proc/mounts") else {
        return false;
    };
    skillfs_fuse::proc_mounts::mounts_contain_target(&mounts, path.as_os_str().as_bytes())
}

/// Best-effort FUSE availability check for gating vs. failing.
fn fuse_available() -> bool {
    Path::new("/dev/fuse").exists()
        && Command::new("fusermount3")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
}

/// Correct-byte best-effort cleanup so a failing (red) run does not leak
/// the mount into the developer environment: invoke fusermount3 with the
/// raw OS string the production code should have used.
fn cleanup_mount(path: &Path) {
    for _ in 0..50 {
        if !is_mounted(path) {
            return;
        }
        let _ = Command::new("fusermount3")
            .arg("-u")
            .arg(path.as_os_str())
            .output();
        let _ = Command::new("fusermount3")
            .arg("-u")
            .arg("-z")
            .arg(path.as_os_str())
            .output();
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn wait_until_mounted(path: &Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if is_mounted(path) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

fn wait_for_exit(child: &mut Child, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

#[test]
fn sigterm_exits_and_unmounts_a_non_utf8_foreground_mount() {
    if !fuse_available() {
        eprintln!("SKIP {}: FUSE not available", std::file!());
        return;
    }

    let base = tempfile::tempdir().expect("base tempdir");
    // Mountpoint with a byte that is not valid UTF-8, the same shape the
    // review reproduced: `mount-\xff`.
    let mountpoint: PathBuf = base.path().join(OsStr::from_bytes(b"mount-\xff"));
    std::fs::create_dir(&mountpoint).expect("create non-UTF-8 mountpoint");

    let source = tempfile::tempdir().expect("source tempdir");
    let skill_dir = source.path().join("alpha");
    std::fs::create_dir(&skill_dir).expect("create skill dir");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: alpha\ndescription: demo skill\n---\n\nBody.\n",
    )
    .expect("write SKILL.md");

    let mut child = Command::new(bin_path())
        .arg("mount")
        .arg("--foreground")
        .arg(source.path().as_os_str())
        .arg(mountpoint.as_os_str())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn skillfs mount --foreground");

    // The foreground mount mounts asynchronously; a mount that never
    // appears means this environment cannot mount FUSE, so skip rather
    // than fail for an unrelated reason.
    if !wait_until_mounted(&mountpoint, Duration::from_secs(10)) {
        eprintln!(
            "SKIP {}: FUSE session did not mount in this environment",
            std::file!()
        );
        let _ = child.kill();
        let _ = child.wait();
        return;
    }

    // SIGTERM: the exact teardown an operator's `kill -TERM` hits.
    let pid = child.id().to_string();
    let _ = Command::new("kill").args(["-TERM", &pid]).status();

    let exited = wait_for_exit(&mut child, Duration::from_secs(20));
    let unmounted = !is_mounted(&mountpoint);
    if !unmounted {
        // Red-run hygiene: remove the leaked mount with the correct bytes.
        cleanup_mount(&mountpoint);
    }
    let _ = child.kill();
    let _ = child.wait();
    assert!(
        exited,
        "foreground mount must exit after SIGTERM instead of hanging on the FUSE task"
    );
    assert!(
        unmounted,
        "SIGTERM teardown must remove the non-UTF-8 mountpoint from /proc/mounts; \
         fusermount3 received a lossy (U+FFFD-mangled) path instead of the raw \
         mountpoint bytes"
    );
}
