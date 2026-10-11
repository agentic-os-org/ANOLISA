//! CLI startup-gate coverage for `--decision-command`.
//!
//! These tests run the actual `skillfs mount` binary so the early
//! validation gates can be observed end-to-end:
//!
//! * `--security` without `--decision-command` must reject the
//!   startup before any FUSE side effect runs.
//! * An empty / whitespace-only `--decision-command` must reject
//!   startup with a clear error.
//!
//! Running the real binary is the cheapest available signal: the CLI
//! parser plus the up-front error path is exactly what an operator
//! sees, and a unit test would only exercise the parser shape, not the
//! wiring in `cmd_mount`.

use std::path::Path;
use std::process::{Child, Command};
use std::time::Duration;

fn bin_path() -> &'static str {
    env!("CARGO_BIN_EXE_skillfs")
}

/// True when `path` is a live mountpoint per `/proc/mounts`. Authoritative
/// even for a dead FUSE endpoint (where `metadata()` would misbehave).
/// Delegates to the production byte-exact, escape-decoding matcher.
fn is_mounted(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(mounts) = std::fs::read("/proc/mounts") else {
        return false;
    };
    skillfs_fuse::proc_mounts::mounts_contain_target(&mounts, path.as_os_str().as_bytes())
}

/// Bounded, best-effort force unmount: `fusermount3 -u`, then lazy `-z`, then
/// `umount -l`, retried until the path leaves `/proc/mounts` or the budget is
/// exhausted. Never panics.
fn force_unmount(path: &Path) {
    for _ in 0..50 {
        if !is_mounted(path) {
            return;
        }
        let mp = path.to_string_lossy();
        let _ = Command::new("fusermount3").args(["-u", &mp]).output();
        let _ = Command::new("fusermount3").args(["-u", "-z", &mp]).output();
        let _ = Command::new("umount").args(["-l", &mp]).output();
        std::thread::sleep(Duration::from_millis(100));
    }
    if is_mounted(path) {
        eprintln!("WARN: leaked SkillFS FUSE mount at {}", path.display());
    }
}

/// Stop a spawned `skillfs mount` child without leaking its FUSE mount.
///
/// `child.kill()` sends SIGKILL, which the binary cannot catch, so the FUSE
/// endpoint would survive under the (possibly workspace-rooted) mountpoint.
/// Instead send SIGTERM — the mount command unmounts cleanly on SIGTERM —
/// wait a bounded time for graceful exit, then force-unmount as a fallback
/// and SIGKILL to guarantee the child is reap-able by the caller.
fn stop_mount_child(child: &mut Child, mountpoint: &Path) {
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
}

fn empty_source() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("source tempdir");
    // Make the source path a real directory so the "Source directory
    // does not exist" gate is not the one that fires first.
    assert!(Path::new(dir.path()).is_dir());
    dir
}

#[test]
fn relative_skill_discover_root_fails_before_mount() {
    let source = empty_source();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--skill-discover-root",
            "relative/skills",
        ])
        .output()
        .expect("invoke skillfs");

    assert!(!out.status.success(), "relative root must be rejected");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("--skill-discover-root must be absolute"),
        "expected absolute-path error, got: {combined}"
    );
    assert!(!is_mounted(mount.path()), "validation must run before FUSE");
}

/// Connect to a control socket, send `ping`, and return the one-line
/// response. Proves the server is alive; the response is either a `pong`
/// or a `permission_denied` (the test binary may fail peer verification),
/// both of which carry the `schemaVersion` envelope.
fn probe_control_socket(path: &Path) -> Option<String> {
    request_control_socket(path, r#"{"schemaVersion":"1","method":"ping"}"#)
}

fn request_control_socket(path: &Path, request: &str) -> Option<String> {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    let mut stream = UnixStream::connect(path).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(3))).ok()?;
    writeln!(stream, "{request}").ok()?;
    stream.flush().ok()?;
    let mut reader = BufReader::new(&stream);
    let mut response = String::new();
    match reader.read_line(&mut response) {
        Ok(n) if n > 0 => Some(response),
        _ => None,
    }
}

/// True when a control-socket response is an authenticated `pong` — proves
/// both that the server is alive AND that the connecting peer passed
/// verification. Requires the trusted peer to be the test binary itself.
fn response_is_authenticated_pong(resp: &str) -> bool {
    resp.contains("\"schemaVersion\"") && resp.contains("\"pong\":true")
}

/// Best-effort FUSE availability check for gating vs. failing. Mirrors the
/// process-level gate in scripts/test.sh.
fn fuse_available() -> bool {
    Path::new("/dev/fuse").exists()
        && Command::new("fusermount3")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
}

/// Path of the running test binary — used as the trusted peer so the
/// test's own probe connection authenticates and receives a real `pong`.
fn test_exe() -> String {
    std::env::current_exe()
        .expect("current_exe")
        .to_string_lossy()
        .into_owned()
}

/// Serializes tests that bind the OS-global default endpoint
/// `/run/user/<uid>/skillfs/control.sock`, which only one instance can hold
/// at a time. cargo runs tests in parallel threads, so without this two
/// default-endpoint tests would race for the same path.
static DEFAULT_ENDPOINT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn security_without_decision_command_fails_startup() {
    let source = empty_source();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
        ])
        .output()
        .expect("invoke skillfs");
    assert!(
        !out.status.success(),
        "expected non-zero exit, stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("--security requires --decision-command")
            || combined.contains("--security requires"),
        "expected startup error message, got: {combined}"
    );
}

#[test]
fn empty_decision_command_fails_startup() {
    let source = empty_source();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--decision-command",
            "   ",
        ])
        .output()
        .expect("invoke skillfs");
    assert!(!out.status.success(), "expected non-zero exit");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("invalid --decision-command"),
        "expected decision-command parse error, got: {combined}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Activation mode startup gates
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn activation_mode_file_without_security_fails_startup() {
    let source = empty_source();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--activation-mode",
            "file",
        ])
        .output()
        .expect("invoke skillfs");
    assert!(!out.status.success(), "expected non-zero exit");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("--activation-mode file requires --security"),
        "expected activation-requires-security error, got: {combined}"
    );
}

#[test]
fn activation_mode_file_with_decision_command_fails_startup() {
    let source = empty_source();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--activation-mode",
            "file",
            "--decision-command",
            "agent-sec-cli skill-ledger",
        ])
        .output()
        .expect("invoke skillfs");
    assert!(!out.status.success(), "expected non-zero exit");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("mutually exclusive"),
        "expected dual-source conflict error, got: {combined}"
    );
}

#[test]
fn activation_mode_file_with_events_log_fails_startup() {
    let source = empty_source();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let events_log = tempfile::NamedTempFile::new().expect("events log");
    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--activation-mode",
            "file",
            "--events-log",
            events_log.path().to_str().unwrap(),
        ])
        .output()
        .expect("invoke skillfs");
    assert!(!out.status.success(), "expected non-zero exit");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("not supported with --activation-mode file"),
        "expected events-log-not-supported error, got: {combined}"
    );
}

#[test]
fn invalid_activation_mode_value_fails_startup() {
    let source = empty_source();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--activation-mode",
            "auto",
        ])
        .output()
        .expect("invoke skillfs");
    assert!(!out.status.success(), "expected non-zero exit");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("invalid --activation-mode"),
        "expected invalid mode error, got: {combined}"
    );
}

#[test]
fn config_activation_file_overridden_by_cli_off() {
    let source = empty_source();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let config_dir = tempfile::tempdir().expect("config dir");
    let config_path = config_dir.path().join("security.toml");
    std::fs::write(&config_path, "[activation]\nmode = \"file\"\n").unwrap();

    // CLI --activation-mode off should override config file's "file".
    // Without --decision-command or --activation-mode file, --security
    // should fail asking for a source, NOT try to load activation files.
    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--activation-mode",
            "off",
            "--config",
            config_path.to_str().unwrap(),
        ])
        .output()
        .expect("invoke skillfs");
    assert!(!out.status.success(), "expected non-zero exit");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    // When activation is off and no decision-command, should get the
    // "requires --decision-command or --activation-mode file" error,
    // proving the CLI off overrode the config's file.
    assert!(
        combined.contains("--security requires"),
        "expected security-requires error (proving CLI off overrode config file), got: {combined}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Activation events log startup gates (N3)
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn activation_events_log_without_security_fails_startup() {
    let source = empty_source();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let log = tempfile::NamedTempFile::new().expect("log file");
    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--activation-events-log",
            log.path().to_str().unwrap(),
        ])
        .output()
        .expect("invoke skillfs");
    assert!(!out.status.success(), "expected non-zero exit");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("--activation-events-log") && combined.contains("requires --security"),
        "expected requires-security error, got: {combined}"
    );
}

#[test]
fn activation_events_log_without_activation_mode_file_fails_startup() {
    let source = empty_source();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let log = tempfile::NamedTempFile::new().expect("log file");
    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--decision-command",
            "echo",
            "--activation-events-log",
            log.path().to_str().unwrap(),
        ])
        .output()
        .expect("invoke skillfs");
    assert!(!out.status.success(), "expected non-zero exit");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("--activation-events-log")
            && combined.contains("requires --activation-mode file"),
        "expected requires-activation-mode-file error, got: {combined}"
    );
}

#[test]
fn activation_events_log_with_decision_command_fails_startup() {
    let source = empty_source();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let log = tempfile::NamedTempFile::new().expect("log file");
    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--activation-mode",
            "file",
            "--decision-command",
            "echo",
            "--activation-events-log",
            log.path().to_str().unwrap(),
        ])
        .output()
        .expect("invoke skillfs");
    assert!(!out.status.success(), "expected non-zero exit");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("mutually exclusive"),
        "expected mutually-exclusive error, got: {combined}"
    );
}

#[test]
fn activation_events_log_inside_source_fails_startup() {
    let source = empty_source();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let log_path = source.path().join("events.jsonl");
    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--activation-mode",
            "file",
            "--activation-events-log",
            log_path.to_str().unwrap(),
        ])
        .output()
        .expect("invoke skillfs");
    assert!(!out.status.success(), "expected non-zero exit");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("lies inside the SkillFS source root")
            || combined.contains("--activation-events-log"),
        "expected inside-source rejection, got: {combined}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Activation reload mode startup gates (A3)
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn reload_poll_without_notify_source_fails_startup() {
    let source = empty_source();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--activation-mode",
            "file",
            "--activation-reload-mode",
            "poll",
        ])
        .output()
        .expect("invoke skillfs");
    assert!(!out.status.success(), "expected non-zero exit");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains(
            "--activation-reload-mode poll requires --notify-socket or --activation-events-log"
        ),
        "expected reload-requires-trigger error, got: {combined}"
    );
}

#[test]
fn config_reload_poll_without_notify_source_fails_startup() {
    let source = empty_source();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let config_dir = tempfile::tempdir().expect("config dir");
    let config_path = config_dir.path().join("security.toml");
    std::fs::write(
        &config_path,
        "[activation]\nmode = \"file\"\nreload = \"poll\"\n",
    )
    .unwrap();

    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--config",
            config_path.to_str().unwrap(),
        ])
        .output()
        .expect("invoke skillfs");
    assert!(!out.status.success(), "expected non-zero exit");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains(
            "--activation-reload-mode poll requires --notify-socket or --activation-events-log"
        ),
        "expected reload-requires-trigger error from config, got: {combined}"
    );
}

#[test]
fn reload_poll_without_security_fails_startup() {
    let source = empty_source();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--activation-mode",
            "file",
            "--activation-reload-mode",
            "poll",
        ])
        .output()
        .expect("invoke skillfs");
    assert!(!out.status.success(), "expected non-zero exit");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("requires --security"),
        "expected requires-security error, got: {combined}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// A6/B1: Ledger backing root startup gates
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn ledger_backing_root_without_security_fails_startup() {
    // Don't pass --activation-mode file either (its own gate fires first).
    // Just pass --ledger-backing-root without --security; activation mode
    // stays off so the backing root's "requires --security" gate fires.
    let source = empty_source();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let backing = tempfile::tempdir().expect("backing tempdir");
    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--ledger-backing-root",
            backing.path().to_str().unwrap(),
        ])
        .output()
        .expect("invoke skillfs");
    assert!(!out.status.success(), "expected non-zero exit");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("--ledger-backing-root") && combined.contains("requires --security"),
        "expected requires-security error, got: {combined}"
    );
}

#[test]
fn ledger_backing_root_without_activation_mode_file_fails_startup() {
    // Pass --security --decision-command echo to satisfy the "security
    // requires a source" gate, then the backing root check fires for
    // activation_mode != File.
    let source = empty_source();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let backing = tempfile::tempdir().expect("backing tempdir");
    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--decision-command",
            "echo",
            "--ledger-backing-root",
            backing.path().to_str().unwrap(),
        ])
        .output()
        .expect("invoke skillfs");
    assert!(!out.status.success(), "expected non-zero exit");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    // The backing root check fires: activation_mode != File.
    // But note: --decision-command is also present, so "mutually exclusive"
    // may fire instead. Both are acceptable — the point is startup is rejected.
    assert!(
        (combined.contains("--ledger-backing-root")
            && combined.contains("requires --activation-mode file"))
            || combined.contains("mutually exclusive"),
        "expected requires-activation-mode-file or mutually-exclusive error, got: {combined}"
    );
}

#[test]
fn ledger_backing_root_with_decision_command_fails_startup() {
    let source = empty_source();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let backing = tempfile::tempdir().expect("backing tempdir");
    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--activation-mode",
            "file",
            "--decision-command",
            "echo",
            "--ledger-backing-root",
            backing.path().to_str().unwrap(),
        ])
        .output()
        .expect("invoke skillfs");
    assert!(!out.status.success(), "expected non-zero exit");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("mutually exclusive"),
        "expected mutually-exclusive error, got: {combined}"
    );
}

#[test]
fn ledger_backing_root_config_overridden_by_cli() {
    // Config has [ledger].backing_root = "/config/path".
    // CLI provides --ledger-backing-root /cli/path.
    // The CLI value should take precedence. We verify this by checking
    // that the CLI path appears in the startup error (not the config path).
    // The CLI path is inside source (which is a tempdir), so it should
    // be rejected with an inside-source/mount error.
    let source = empty_source();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let config_dir = tempfile::tempdir().expect("config dir");
    let config_path = config_dir.path().join("security.toml");
    std::fs::write(
        &config_path,
        "[activation]\nmode = \"file\"\n[ledger]\nbacking_root = \"/nonexistent/config/path\"\n",
    )
    .unwrap();

    // CLI provides a path inside source — should be rejected.
    let cli_backing = source.path().join("backing_root");
    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--activation-mode",
            "file",
            "--config",
            config_path.to_str().unwrap(),
            "--ledger-backing-root",
            cli_backing.to_str().unwrap(),
        ])
        .output()
        .expect("invoke skillfs");
    assert!(!out.status.success(), "expected non-zero exit");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    // The CLI path should be the one that's rejected, not the config path.
    assert!(
        combined.contains("backing root") && combined.contains("backing_root"),
        "expected CLI backing root path in error, got: {combined}"
    );
    assert!(
        !combined.contains("/nonexistent/config/path"),
        "config path should not appear when CLI overrides it, got: {combined}"
    );
}

#[test]
fn ledger_backing_root_inside_source_fails_before_mount() {
    // In non-in-place mode, backing root inside source is rejected.
    // (In non-in-place, source != mount, so inside-source is not the same
    // as inside-mount. But the backing root inside the source tree is
    // still a bad idea for in-place; here we test non-in-place where
    // backing root == source is allowed.)
    //
    // Instead, test that a backing root inside the mount path is rejected.
    let source = empty_source();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let backing_inside_mount = mount.path().join("backing_root");

    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--activation-mode",
            "file",
            "--ledger-backing-root",
            backing_inside_mount.to_str().unwrap(),
        ])
        .output()
        .expect("invoke skillfs");
    assert!(!out.status.success(), "expected non-zero exit");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("backing root") && combined.contains("mount path"),
        "expected inside-mount-path rejection, got: {combined}"
    );
    // The mount point should NOT have a backing_root directory created.
    assert!(
        !backing_inside_mount.exists(),
        "backing root dir should not be created when path check fails"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// #1262: PrivateTmp daemon-facing backing root / source gates
// ─────────────────────────────────────────────────────────────────────────────
//
// agent-sec-core.service runs with PrivateTmp=true, so a daemon-facing
// source or backing root under /tmp or /var/tmp is invisible to the
// daemon. SkillFS must fail fast at startup (before any mount / bind
// mount) rather than letting notify be rejected and activation time out.

/// A directory that is NOT under /tmp (tempfile defaults to /tmp). Rooted
/// at the crate's target dir so daemon-facing paths can be exercised with
/// a genuinely non-tmp canonical location.
fn non_tmp_dir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("skillfs-nontmp-")
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .expect("non-tmp tempdir")
}

/// A non-tmp directory short enough to hold a bindable control socket.
///
/// `sun_path` caps a socket address at 108 bytes on Linux. `non_tmp_dir()`
/// lives under `CARGO_TARGET_TMPDIR`, whose length follows the checkout path,
/// so a checkout nested a few directories deep overruns the cap and the child
/// exits with "path must be shorter than SUN_LEN". `/dev/shm` is outside the
/// /tmp and /var/tmp roots the PrivateTmp gate rejects and keeps the socket
/// address near 45 bytes wherever the repository is checked out. When
/// `/dev/shm` is missing or not writable it falls back to `non_tmp_dir()`.
fn non_tmp_socket_dir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("skillfs-nontmp-")
        .tempdir_in("/dev/shm")
        .unwrap_or_else(|_| non_tmp_dir())
}

/// A collision-resistant leaf name (pid + nanosecond timestamp) so tests
/// that use a fixed parent directory (e.g. /tmp) never trip over a stale
/// path left behind by a previous manual run.
fn unique_leaf(prefix: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{prefix}-{}-{}", std::process::id(), nanos)
}

/// Spawn the binary, let startup run briefly, then stop it cleanly and return
/// the combined stdout+stderr. Used for configs that pass the new gate and
/// would otherwise block on the FUSE mount. `mountpoint` must match the mount
/// path passed in `args` so the FUSE mount is always torn down (never leaked
/// under the workspace).
fn run_briefly(mountpoint: &Path, args: &[&str]) -> String {
    let mut child = Command::new(bin_path())
        .args(args)
        .stderr(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn skillfs");
    std::thread::sleep(Duration::from_secs(1));
    stop_mount_child(&mut child, mountpoint);
    let out = child.wait_with_output().expect("wait for child");
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    )
}

#[test]
fn backing_root_under_tmp_fails_before_setup() {
    // --ledger-backing-root under /tmp with daemon-driven activation
    // (--notify-socket) must be rejected before any mount / bind mount.
    let source = empty_source();
    let mount = tempfile::tempdir().expect("mount tempdir");
    // Leaf need not exist; parent (/tmp) canonicalizes. Nothing is created.
    // Use a unique leaf so a stale dir from a prior run cannot skew the
    // "dir not created" assertion below.
    let backing = format!("/tmp/{}", unique_leaf("skillfs-privtmp-test-backing"));
    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--activation-mode",
            "file",
            "--notify-socket",
            "/run/skillfs-privtmp-test.sock",
            "--ledger-backing-root",
            &backing,
            "--trusted-peer-exe",
            bin_path(),
        ])
        .output()
        .expect("invoke skillfs");
    assert!(!out.status.success(), "expected non-zero exit");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("--ledger-backing-root")
            && combined.contains("/tmp or /var/tmp")
            && combined.contains("PrivateTmp=true")
            && combined.contains("/run/user/$UID/")
            && combined.contains("/run/"),
        "expected PrivateTmp backing-root rejection, got: {combined}"
    );
    assert!(
        !Path::new(&backing).exists(),
        "backing root dir must not be created when the gate rejects it"
    );
}

#[test]
fn backing_root_under_var_tmp_fails_before_setup() {
    let source = empty_source();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let backing = format!("/var/tmp/{}", unique_leaf("skillfs-privtmp-test-backing"));
    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--activation-mode",
            "file",
            "--activation-events-log",
            "/run/skillfs-privtmp-test-events.jsonl",
            "--ledger-backing-root",
            &backing,
        ])
        .output()
        .expect("invoke skillfs");
    assert!(!out.status.success(), "expected non-zero exit");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("--ledger-backing-root")
            && combined.contains("/tmp or /var/tmp")
            && combined.contains("PrivateTmp=true"),
        "expected PrivateTmp backing-root rejection for /var/tmp, got: {combined}"
    );
}

#[test]
#[cfg(unix)]
fn backing_root_symlink_to_tmp_fails_before_setup() {
    // A backing root whose path shape is non-tmp but which symlinks to a
    // real /tmp directory must still be rejected. Without canonicalizing
    // the backing root itself, the parent-canonicalize + leaf shape check
    // would pass while LedgerBackingRoot::setup resolves the symlink and
    // hands the daemon a /tmp path it cannot see.
    let source = empty_source();
    let mount = tempfile::tempdir().expect("mount tempdir");
    // Real target directory under /tmp (must exist for canonicalize to
    // resolve the symlink through to it).
    let tmp_target = tempfile::tempdir().expect("tmp target");
    // Symlink lives under a non-tmp parent so the shape check alone passes.
    let parent = non_tmp_dir();
    let link = parent.path().join("backing-link");
    std::os::unix::fs::symlink(tmp_target.path(), &link).expect("create symlink");

    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--activation-mode",
            "file",
            "--notify-socket",
            "/run/skillfs-privtmp-test.sock",
            "--ledger-backing-root",
            link.to_str().unwrap(),
            "--trusted-peer-exe",
            bin_path(),
        ])
        .output()
        .expect("invoke skillfs");
    assert!(!out.status.success(), "expected non-zero exit");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("--ledger-backing-root")
            && combined.contains("/tmp or /var/tmp")
            && combined.contains("PrivateTmp=true"),
        "expected PrivateTmp rejection for symlinked backing root, got: {combined}"
    );
}

#[test]
fn fallback_source_under_tmp_fails_before_mount() {
    // Non-in-place mount, no backing root: the daemon-facing root falls
    // back to the source. A source under /tmp with a notify trigger must
    // be rejected with a message that points at --ledger-backing-root.
    let source = empty_source(); // tempfile => under /tmp
    let mount = tempfile::tempdir().expect("mount tempdir");
    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--activation-mode",
            "file",
            "--notify-socket",
            "/run/skillfs-privtmp-test.sock",
        ])
        .output()
        .expect("invoke skillfs");
    assert!(!out.status.success(), "expected non-zero exit");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("source")
            && combined.contains("/tmp or /var/tmp")
            && combined.contains("PrivateTmp=true")
            && combined.contains("--ledger-backing-root")
            && combined.contains("/run/"),
        "expected PrivateTmp source-fallback rejection, got: {combined}"
    );
}

#[test]
fn mountpoint_under_tmp_not_rejected_when_source_is_non_tmp() {
    // Only the daemon-facing path (source/backing root) is guarded. A
    // plain agent-visible mountpoint under /tmp must NOT trip the new
    // PrivateTmp gate when the source is non-tmp. The mount may still fail
    // later for ordinary environment reasons, but the PrivateTmp error
    // must not fire.
    let source = non_tmp_dir();
    let mount = tempfile::tempdir().expect("mount tempdir"); // under /tmp
    let combined = run_briefly(
        mount.path(),
        &[
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--activation-mode",
            "file",
            "--notify-socket",
            "/run/skillfs-privtmp-test.sock",
        ],
    );
    assert!(
        !combined.contains("PrivateTmp=true"),
        "PrivateTmp gate must not fire for an agent-visible /tmp mountpoint \
         with a non-tmp source, got: {combined}"
    );
}

#[test]
fn non_tmp_daemon_facing_root_passes_gate() {
    // A non-tmp daemon-facing backing root keeps existing behavior: the
    // new gate does not fire. Using backing_root == source (a non-in-place
    // convenience that needs no bind mount) exercises the backing-root
    // branch without requiring CAP_SYS_ADMIN.
    let source = non_tmp_dir();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let combined = run_briefly(
        mount.path(),
        &[
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--activation-mode",
            "file",
            "--notify-socket",
            "/run/skillfs-privtmp-test.sock",
            "--ledger-backing-root",
            source.path().to_str().unwrap(),
            "--trusted-peer-exe",
            bin_path(),
        ],
    );
    assert!(
        !combined.contains("PrivateTmp=true")
            && !combined.contains("resolves under /tmp")
            && !combined.contains("authenticated live-source resolver"),
        "PrivateTmp gate must not fire for a non-tmp backing root, got: {combined}"
    );
}

#[test]
fn activation_events_log_under_tmp_fails_before_mount() {
    // The daemon tails the activation events log, so it is a daemon-facing
    // transport path. A non-tmp source/mount with the events log under /tmp
    // must still be rejected, naming --activation-events-log.
    let source = non_tmp_dir();
    let mount = non_tmp_dir();
    let events_log = format!("/tmp/{}", unique_leaf("skillfs-privtmp-test-events.jsonl"));
    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--activation-mode",
            "file",
            "--activation-events-log",
            &events_log,
        ])
        .output()
        .expect("invoke skillfs");
    assert!(!out.status.success(), "expected non-zero exit");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("--activation-events-log")
            && combined.contains("/tmp or /var/tmp")
            && combined.contains("agent-sec-core.service")
            && combined.contains("PrivateTmp=true")
            && combined.contains("/run/"),
        "expected PrivateTmp events-log rejection, got: {combined}"
    );
}

#[test]
fn activation_events_log_under_var_tmp_fails_before_mount() {
    let source = non_tmp_dir();
    let mount = non_tmp_dir();
    let events_log = format!(
        "/var/tmp/{}",
        unique_leaf("skillfs-privtmp-test-events.jsonl")
    );
    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--activation-mode",
            "file",
            "--activation-events-log",
            &events_log,
        ])
        .output()
        .expect("invoke skillfs");
    assert!(!out.status.success(), "expected non-zero exit");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("--activation-events-log")
            && combined.contains("/tmp or /var/tmp")
            && combined.contains("PrivateTmp=true"),
        "expected PrivateTmp events-log rejection for /var/tmp, got: {combined}"
    );
}

#[test]
#[cfg(unix)]
fn activation_events_log_symlink_to_tmp_fails_before_mount() {
    // A non-tmp events-log path that symlinks to a real /tmp file resolves
    // (via canonicalize) to a PrivateTmp-invisible path and must be
    // rejected, mirroring the backing-root symlink handling.
    let source = non_tmp_dir();
    let mount = non_tmp_dir();
    let tmp_target = tempfile::NamedTempFile::new().expect("tmp target file"); // /tmp
    let parent = non_tmp_dir();
    let link = parent.path().join("events-link.jsonl");
    std::os::unix::fs::symlink(tmp_target.path(), &link).expect("create symlink");

    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--activation-mode",
            "file",
            "--activation-events-log",
            link.to_str().unwrap(),
        ])
        .output()
        .expect("invoke skillfs");
    assert!(!out.status.success(), "expected non-zero exit");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("--activation-events-log")
            && combined.contains("/tmp or /var/tmp")
            && combined.contains("PrivateTmp=true"),
        "expected PrivateTmp rejection for symlinked events log, got: {combined}"
    );
}

#[test]
fn notify_socket_under_tmp_fails_before_mount() {
    // The daemon owns the notify socket, so it is daemon-facing. A non-tmp
    // source/mount with the notify socket under /tmp must be rejected,
    // naming --notify-socket.
    let source = non_tmp_dir();
    let mount = non_tmp_dir();
    let socket = format!("/tmp/{}", unique_leaf("skillfs-privtmp-test.sock"));
    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--activation-mode",
            "file",
            "--notify-socket",
            &socket,
        ])
        .output()
        .expect("invoke skillfs");
    assert!(!out.status.success(), "expected non-zero exit");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("--notify-socket")
            && combined.contains("/tmp or /var/tmp")
            && combined.contains("agent-sec-core.service")
            && combined.contains("PrivateTmp=true")
            && combined.contains("/run/"),
        "expected PrivateTmp notify-socket rejection, got: {combined}"
    );
}

#[test]
fn non_tmp_transport_paths_pass_gate() {
    // Non-tmp events log AND non-tmp notify socket keep existing behavior:
    // the PrivateTmp gate does not fire.
    let source = non_tmp_dir();
    let mount = non_tmp_dir();
    let events_dir = non_tmp_dir();
    let events_log = events_dir.path().join("events.jsonl");
    let combined = run_briefly(
        mount.path(),
        &[
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--activation-mode",
            "file",
            "--activation-events-log",
            events_log.to_str().unwrap(),
            "--notify-socket",
            "/run/skillfs-privtmp-test.sock",
        ],
    );
    assert!(
        !combined.contains("PrivateTmp=true") && !combined.contains("resolves under /tmp"),
        "PrivateTmp gate must not fire for non-tmp transport paths, got: {combined}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// #1262: PrivateTmp daemon-facing control-plane gates
// ─────────────────────────────────────────────────────────────────────────────
//
// The resolver control plane is daemon-facing too: the daemon connects to
// the control socket and the resolver opens the physical live source. With
// PrivateTmp=true the daemon cannot see /tmp or /var/tmp, so a control-plane
// source fallback, backing root, or explicit socket under those roots must
// fail fast — even when notify / events-log are not configured.

#[test]
fn control_plane_only_source_under_tmp_fails_before_mount() {
    // Control plane enabled via a trusted peer (default endpoint, no
    // explicit socket) and no backing root: the resolver's live root falls
    // back to the source. A source under /tmp must be rejected, pointing at
    // --ledger-backing-root, even though notify/events-log are absent.
    let source = empty_source(); // under /tmp
    let mount = tempfile::tempdir().expect("mount tempdir");
    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--activation-mode",
            "file",
            "--trusted-peer-exe",
            &test_exe(),
        ])
        .output()
        .expect("invoke skillfs");
    assert!(!out.status.success(), "expected non-zero exit");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("source")
            && combined.contains("/tmp or /var/tmp")
            && combined.contains("PrivateTmp=true")
            && combined.contains("--ledger-backing-root"),
        "expected PrivateTmp source-fallback rejection for control plane, got: {combined}"
    );
}

#[test]
fn control_plane_backing_root_under_tmp_fails_before_setup() {
    // Control plane enabled with a backing root under /tmp must be rejected
    // before any bind mount, naming --ledger-backing-root.
    let source = non_tmp_dir();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let backing = format!("/tmp/{}", unique_leaf("skillfs-privtmp-cp-backing"));
    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--activation-mode",
            "file",
            "--trusted-peer-exe",
            &test_exe(),
            "--ledger-backing-root",
            &backing,
        ])
        .output()
        .expect("invoke skillfs");
    assert!(!out.status.success(), "expected non-zero exit");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("--ledger-backing-root")
            && combined.contains("/tmp or /var/tmp")
            && combined.contains("PrivateTmp=true"),
        "expected PrivateTmp backing-root rejection for control plane, got: {combined}"
    );
    assert!(
        !Path::new(&backing).exists(),
        "backing root dir must not be created when the gate rejects it"
    );
}

#[test]
fn control_socket_under_tmp_fails_before_mount() {
    // An explicit --control-socket under /tmp is daemon-facing: the daemon
    // connects to it with PrivateTmp=true and could not see it. The source
    // is non-tmp so the socket path is the one that trips the gate.
    let source = non_tmp_dir();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let socket = format!("/tmp/{}", unique_leaf("skillfs-privtmp-control.sock"));
    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--activation-mode",
            "file",
            "--control-socket",
            &socket,
            "--trusted-peer-exe",
            &test_exe(),
        ])
        .output()
        .expect("invoke skillfs");
    assert!(!out.status.success(), "expected non-zero exit");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("--control-socket")
            && combined.contains("/tmp or /var/tmp")
            && combined.contains("PrivateTmp=true")
            && combined.contains("/run/"),
        "expected PrivateTmp control-socket rejection, got: {combined}"
    );
    assert!(
        !Path::new(&socket).exists(),
        "control socket must not be bound on a rejected startup"
    );
}

#[test]
#[cfg(unix)]
fn control_socket_symlink_to_tmp_fails_before_mount() {
    // A control socket whose parent directory symlinks to a real /tmp
    // directory canonicalizes to a PrivateTmp-invisible path and must be
    // rejected, mirroring the backing-root / events-log symlink handling.
    let source = non_tmp_dir();
    let mount = tempfile::tempdir().expect("mount tempdir");
    // Real /tmp directory the symlink resolves through to.
    let tmp_target = tempfile::tempdir().expect("tmp target");
    // Symlink lives under a non-tmp parent so only canonicalization reveals
    // the /tmp destination.
    let parent = non_tmp_dir();
    let link = parent.path().join("control-link");
    std::os::unix::fs::symlink(tmp_target.path(), &link).expect("create symlink");
    let socket = link.join("control.sock");

    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--activation-mode",
            "file",
            "--control-socket",
            socket.to_str().unwrap(),
            "--trusted-peer-exe",
            &test_exe(),
        ])
        .output()
        .expect("invoke skillfs");
    assert!(!out.status.success(), "expected non-zero exit");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("--control-socket")
            && combined.contains("/tmp or /var/tmp")
            && combined.contains("PrivateTmp=true"),
        "expected PrivateTmp rejection for symlinked control socket, got: {combined}"
    );
}

#[test]
#[cfg(unix)]
fn control_socket_ancestor_symlink_missing_parent_to_tmp_fails() {
    // The socket's direct parent (`missing-parent`) does not exist yet, and
    // an ancestor (`link`) is a symlink into /tmp. A parent-only
    // canonicalize would fall back to the raw lexical path (no /tmp prefix)
    // and wrongly pass; resolution must climb to the deepest existing
    // ancestor (`link` → /tmp) and still reject before any socket is bound.
    let source = non_tmp_dir();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let tmp_target = tempfile::tempdir().expect("tmp target");
    let parent = non_tmp_dir();
    let link = parent.path().join("cp-ancestor-link");
    std::os::unix::fs::symlink(tmp_target.path(), &link).expect("create symlink");
    // `missing-parent` does not exist under the symlink target.
    let socket = link.join("missing-parent").join("control.sock");

    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--activation-mode",
            "file",
            "--control-socket",
            socket.to_str().unwrap(),
            "--trusted-peer-exe",
            &test_exe(),
        ])
        .output()
        .expect("invoke skillfs");
    assert!(!out.status.success(), "expected non-zero exit");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("--control-socket")
            && combined.contains("/tmp or /var/tmp")
            && combined.contains("PrivateTmp=true"),
        "expected PrivateTmp rejection for ancestor-symlink socket, got: {combined}"
    );
    // The socket must not have been bound under the real /tmp target.
    assert!(
        !tmp_target
            .path()
            .join("missing-parent")
            .join("control.sock")
            .exists(),
        "socket must not be bound under the /tmp symlink target on a rejected startup"
    );
}

#[test]
fn control_plane_cli_socket_with_config_trusted_peer_passes_gate() {
    // Mixed configuration: --control-socket on the CLI, trusted peer from
    // the config file. Both are daemon-visible (non-tmp), so the control
    // plane comes up and answers an authenticated pong.
    if !fuse_available() {
        eprintln!("SKIP: FUSE unavailable; cannot bring up the control socket");
        return;
    }
    let source = non_tmp_dir();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let sock_dir = non_tmp_socket_dir();
    let sock_path = sock_dir.path().join("skillfs.sock");
    let config_dir = tempfile::tempdir().expect("config dir");
    let config_path = config_dir.path().join("security.toml");
    // Only the trusted peer comes from config; the socket path is CLI-only.
    std::fs::write(
        &config_path,
        format!(
            r#"
[activation]
mode = "file"

[control_socket]
trusted_peer_exe = "{}"
"#,
            test_exe()
        ),
    )
    .unwrap();

    let mut child = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--config",
            config_path.to_str().unwrap(),
            "--control-socket",
            sock_path.to_str().unwrap(),
        ])
        .stderr(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn skillfs");

    std::thread::sleep(std::time::Duration::from_secs(2));
    let child_alive = matches!(child.try_wait(), Ok(None));
    let socket_exists = sock_path.exists();
    let probe = if socket_exists {
        probe_control_socket(&sock_path)
    } else {
        None
    };
    stop_mount_child(&mut child, mount.path());
    let output = child.wait_with_output();

    assert!(
        child_alive,
        "child exited before binding the mixed CLI-socket/config-peer control socket: {:?}",
        output.map(|o| String::from_utf8_lossy(&o.stderr).into_owned())
    );
    assert!(
        socket_exists,
        "FUSE is available but the mixed-config control socket {} was not created",
        sock_path.display()
    );
    let resp = probe.expect("mixed-config control socket must respond");
    assert!(
        response_is_authenticated_pong(&resp),
        "mixed-config control socket did not return an authenticated pong: {resp}"
    );
}

#[test]
fn control_plane_config_socket_with_cli_trusted_peer_passes_gate() {
    // Reverse mixed configuration: socket path from the config file, trusted
    // peer from the CLI. The config loader must NOT reject the path-only
    // config, and the post-merge gate must accept the CLI-supplied peer, so
    // the control plane comes up and answers an authenticated pong.
    if !fuse_available() {
        eprintln!("SKIP: FUSE unavailable; cannot bring up the control socket");
        return;
    }
    let source = non_tmp_dir();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let sock_dir = non_tmp_socket_dir();
    let sock_path = sock_dir.path().join("skillfs.sock");
    let config_dir = tempfile::tempdir().expect("config dir");
    let config_path = config_dir.path().join("security.toml");
    // Only the socket path comes from config; the trusted peer is CLI-only.
    std::fs::write(
        &config_path,
        format!(
            r#"
[activation]
mode = "file"

[control_socket]
path = "{}"
"#,
            sock_path.display()
        ),
    )
    .unwrap();

    let mut child = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--config",
            config_path.to_str().unwrap(),
            "--trusted-peer-exe",
            &test_exe(),
        ])
        .stderr(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn skillfs");

    std::thread::sleep(std::time::Duration::from_secs(2));
    let child_alive = matches!(child.try_wait(), Ok(None));
    let socket_exists = sock_path.exists();
    let probe = if socket_exists {
        probe_control_socket(&sock_path)
    } else {
        None
    };
    stop_mount_child(&mut child, mount.path());
    let output = child.wait_with_output();

    assert!(
        child_alive,
        "child exited before binding the config-socket/CLI-peer control socket: {:?}",
        output.map(|o| String::from_utf8_lossy(&o.stderr).into_owned())
    );
    assert!(
        socket_exists,
        "FUSE is available but the config-socket/CLI-peer control socket {} was not created",
        sock_path.display()
    );
    let resp = probe.expect("config-socket/CLI-peer control socket must respond");
    assert!(
        response_is_authenticated_pong(&resp),
        "config-socket/CLI-peer control socket did not return an authenticated pong: {resp}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// PID file cleanup tests
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn pid_file_not_left_behind_on_startup_failure() {
    let source = empty_source();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let pid_dir = tempfile::tempdir().expect("pid dir");
    let pid_path = pid_dir.path().join("skillfs.pid");

    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--pid-file",
            pid_path.to_str().unwrap(),
        ])
        .output()
        .expect("invoke skillfs");
    assert!(!out.status.success());
    assert!(
        !pid_path.exists(),
        "pid file must not be left behind after startup failure"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Trusted writer exe startup gates
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn trusted_writer_exe_nonexistent_fails_startup() {
    let source = empty_source();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--trusted-writer-exe",
            "/nonexistent/binary/path",
        ])
        .output()
        .expect("invoke skillfs");
    assert!(!out.status.success(), "expected non-zero exit");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("--trusted-writer-exe"),
        "expected trusted-writer-exe error, got: {combined}"
    );
}

#[test]
fn trusted_writer_exe_directory_fails_startup() {
    let source = empty_source();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let dir = tempfile::tempdir().expect("directory for test");
    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--trusted-writer-exe",
            dir.path().to_str().unwrap(),
        ])
        .output()
        .expect("invoke skillfs");
    assert!(!out.status.success(), "expected non-zero exit");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("not a regular file"),
        "expected not-a-regular-file error, got: {combined}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// I2: Installer staging startup gates
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn staging_patterns_without_notify_source_fails_startup() {
    let source = empty_source();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let config_dir = tempfile::tempdir().expect("config dir");
    let config_path = config_dir.path().join("security.toml");
    std::fs::write(
        &config_path,
        r#"
[activation]
mode = "file"

[install]
staging_patterns = [".openclaw-install-stage-*"]
"#,
    )
    .unwrap();

    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--config",
            config_path.to_str().unwrap(),
        ])
        .output()
        .expect("invoke skillfs");
    assert!(!out.status.success(), "expected non-zero exit");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains(
            "install.staging_patterns requires --notify-socket or --activation-events-log"
        ),
        "expected staging-requires-notify error, got: {combined}"
    );
}

#[test]
fn staging_patterns_with_activation_events_log_passes_gate() {
    // Source and events log must be non-tmp: with the PrivateTmp gate a
    // /tmp events log or source would abort startup, making the "staging
    // gate did not fire" assertion pass for the wrong reason.
    let source = non_tmp_dir();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let config_dir = tempfile::tempdir().expect("config dir");
    let config_path = config_dir.path().join("security.toml");
    let events_dir = non_tmp_dir();
    let events_log = events_dir.path().join("events.jsonl");
    std::fs::write(
        &config_path,
        r#"
[activation]
mode = "file"

[install]
staging_patterns = [".openclaw-install-stage-*"]
"#,
    )
    .unwrap();

    let mut child = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--config",
            config_path.to_str().unwrap(),
            "--activation-events-log",
            events_log.to_str().unwrap(),
        ])
        .stderr(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn skillfs");

    std::thread::sleep(std::time::Duration::from_secs(2));
    stop_mount_child(&mut child, mount.path());
    let out = child.wait_with_output().expect("wait for child");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        !combined.contains("install.staging_patterns requires"),
        "staging gate must not fire when --activation-events-log is set, got: {combined}"
    );
    assert!(
        !combined.contains("PrivateTmp=true"),
        "PrivateTmp gate must not fire for a non-tmp source + events log, got: {combined}"
    );
}

#[test]
fn staging_patterns_with_notify_socket_passes_gate() {
    // Source and notify socket must be non-tmp for the same reason as
    // above: otherwise the PrivateTmp gate would mask the staging check.
    let source = non_tmp_dir();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let config_dir = tempfile::tempdir().expect("config dir");
    let config_path = config_dir.path().join("security.toml");
    std::fs::write(
        &config_path,
        r#"
[activation]
mode = "file"

[install]
staging_patterns = [".openclaw-install-stage-*"]
"#,
    )
    .unwrap();

    let mut child = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--config",
            config_path.to_str().unwrap(),
            "--notify-socket",
            "/run/skillfs-staging-test.sock",
        ])
        .stderr(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn skillfs");

    std::thread::sleep(std::time::Duration::from_secs(2));
    stop_mount_child(&mut child, mount.path());
    let out = child.wait_with_output().expect("wait for child");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        !combined.contains("install.staging_patterns requires"),
        "staging gate must not fire when --notify-socket is set, got: {combined}"
    );
    assert!(
        !combined.contains("PrivateTmp=true"),
        "PrivateTmp gate must not fire for a non-tmp source + notify socket, got: {combined}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Trusted peer control socket startup gates
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn control_socket_without_trusted_peer_exe_fails_startup() {
    let source = empty_source();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--control-socket",
            "/tmp/test-skillfs.sock",
        ])
        .output()
        .expect("invoke skillfs");
    assert!(!out.status.success(), "expected non-zero exit");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("--control-socket") && combined.contains("requires a trusted peer"),
        "expected requires-trusted-peer error, got: {combined}"
    );
}

#[test]
fn trusted_peer_auth_modes_are_mutually_exclusive() {
    let source = empty_source();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--control-socket",
            "/run/skillfs-test.sock",
            "--trusted-peer-exe",
            "/usr/bin/env",
            "--trusted-peer-key-file",
            "/run/skillfs-test.key",
        ])
        .output()
        .expect("invoke skillfs");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(!out.status.success());
    assert!(combined.contains("mutually exclusive"), "got: {combined}");
}

#[test]
fn trusted_peer_key_requires_explicit_socket() {
    let source = empty_source();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--trusted-peer-key-file",
            "/run/skillfs-test.key",
        ])
        .output()
        .expect("invoke skillfs");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(!out.status.success());
    assert!(
        combined.contains("requires an explicit --control-socket"),
        "got: {combined}"
    );
}

#[test]
fn trusted_peer_key_with_insecure_permissions_fails_startup() {
    use std::os::unix::fs::PermissionsExt;

    let source = non_tmp_dir();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let socket_dir = non_tmp_socket_dir();
    let socket_path = socket_dir.path().join("control.sock");
    let key_dir = non_tmp_dir();
    let key_path = key_dir.path().join("peer.key");
    std::fs::write(&key_path, [3_u8; 32]).expect("write key");
    std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o644)).expect("chmod key");
    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--activation-mode",
            "file",
            "--control-socket",
            socket_path.to_str().unwrap(),
            "--trusted-peer-key-file",
            key_path.to_str().unwrap(),
        ])
        .output()
        .expect("invoke skillfs");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(!out.status.success());
    assert!(
        combined.contains("must not grant group or other permissions"),
        "got: {combined}"
    );
    assert!(!socket_path.exists(), "socket must not bind with bad key");
}

#[test]
fn container_hmac_control_rejects_plain_notify_channel() {
    let source = empty_source();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--activation-mode",
            "file",
            "--control-socket",
            "/run/skillfs-test-control.sock",
            "--trusted-peer-key-file",
            "/run/skillfs-test.key",
            "--notify-socket",
            "/run/skillfs-test-notify.sock",
        ])
        .output()
        .expect("invoke skillfs");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(!out.status.success());
    assert!(
        combined.contains("requires --notify-auth-key-file"),
        "got: {combined}"
    );
}

#[test]
fn trusted_peer_exe_without_security_fails_startup() {
    // A trusted peer with no explicit --control-socket enables the control
    // plane on the default per-user endpoint, so it is no longer rejected
    // for "requires --control-socket". It still requires --security like
    // any control-plane configuration.
    let source = empty_source();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--trusted-peer-exe",
            "/usr/bin/env",
        ])
        .output()
        .expect("invoke skillfs");
    assert!(!out.status.success(), "expected non-zero exit");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("requires --security"),
        "expected requires-security error, got: {combined}"
    );
    assert!(
        !combined.contains("requires --control-socket"),
        "trusted-peer-exe alone must no longer require --control-socket: {combined}"
    );
}

#[test]
fn control_socket_trusted_peer_exe_nonexistent_fails() {
    // Daemon-visible source and socket so the trusted-peer-exe validation
    // is the gate that fires — not the PrivateTmp gate on a /tmp fixture.
    let source = non_tmp_dir();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let sock_dir = non_tmp_socket_dir();
    let sock_path = sock_dir.path().join("skillfs.sock");
    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--activation-mode",
            "file",
            "--control-socket",
            sock_path.to_str().unwrap(),
            "--trusted-peer-exe",
            "/nonexistent/binary/path",
        ])
        .output()
        .expect("invoke skillfs");
    assert!(!out.status.success(), "expected non-zero exit");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("--trusted-peer-exe"),
        "expected trusted-peer-exe error, got: {combined}"
    );
}

#[test]
fn control_socket_trusted_peer_exe_directory_fails() {
    // Daemon-visible source and socket so the trusted-peer-exe validation
    // is the gate that fires — not the PrivateTmp gate on a /tmp fixture.
    let source = non_tmp_dir();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let sock_dir = non_tmp_socket_dir();
    let sock_path = sock_dir.path().join("skillfs.sock");
    let dir = tempfile::tempdir().expect("directory for test");
    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--activation-mode",
            "file",
            "--control-socket",
            sock_path.to_str().unwrap(),
            "--trusted-peer-exe",
            dir.path().to_str().unwrap(),
        ])
        .output()
        .expect("invoke skillfs");
    assert!(!out.status.success(), "expected non-zero exit");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("not a regular file"),
        "expected not-a-regular-file error, got: {combined}"
    );
}

#[test]
fn control_socket_without_security_fails_startup() {
    let source = empty_source();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--control-socket",
            "/tmp/test-skillfs.sock",
            "--trusted-peer-exe",
            bin_path(),
        ])
        .output()
        .expect("invoke skillfs");
    assert!(!out.status.success(), "expected non-zero exit");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("--control-socket requires --security"),
        "expected requires-security error, got: {combined}"
    );
}

#[test]
fn control_socket_without_activation_mode_file_fails_startup() {
    let source = empty_source();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--control-socket",
            "/tmp/test-skillfs.sock",
            "--trusted-peer-exe",
            bin_path(),
        ])
        .output()
        .expect("invoke skillfs");
    assert!(!out.status.success(), "expected non-zero exit");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("--control-socket requires --activation-mode file"),
        "expected requires-activation-mode-file error, got: {combined}"
    );
}

#[test]
fn control_socket_with_decision_command_fails_startup() {
    let source = empty_source();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--activation-mode",
            "file",
            "--decision-command",
            "agent-sec-cli skill-ledger",
            "--control-socket",
            "/tmp/test-skillfs.sock",
            "--trusted-peer-exe",
            bin_path(),
        ])
        .output()
        .expect("invoke skillfs");
    assert!(!out.status.success(), "expected non-zero exit");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("mutually exclusive"),
        "expected mutually-exclusive error, got: {combined}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Managed mount / stop startup gates
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn managed_mount_parses_and_rejects_missing_source() {
    // `--managed` must parse, and the managed client must fail fast with a
    // clear source error before detaching a supervisor — proving the flag is
    // wired without needing FUSE.
    let parent = tempfile::tempdir().expect("parent tempdir");
    let missing_source = parent.path().join("does-not-exist");
    let mount = tempfile::tempdir().expect("mount tempdir");
    let out = Command::new(bin_path())
        .args([
            "mount",
            missing_source.to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--managed",
        ])
        .output()
        .expect("invoke skillfs");
    assert!(!out.status.success(), "expected non-zero exit");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("Source directory does not exist"),
        "expected missing-source error, got: {combined}"
    );
}

#[test]
fn stop_is_idempotent_when_nothing_mounted() {
    // `stop` on a path with no managed instance and nothing mounted must
    // succeed (idempotent teardown).
    let mount = tempfile::tempdir().expect("mount tempdir");
    let out = Command::new(bin_path())
        .args(["stop", mount.path().to_str().unwrap()])
        .output()
        .expect("invoke skillfs");
    assert!(
        out.status.success(),
        "expected success (idempotent stop), stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("no managed mount") || combined.contains("already stopped"),
        "expected idempotent no-op message, got: {combined}"
    );
}

#[test]
fn supervise_missing_instance_fails() {
    // The hidden supervise subcommand must fail cleanly when the instance
    // state file is absent, rather than spinning.
    let out = Command::new(bin_path())
        .args(["supervise", "--instance", "nonexistent-0000000000000000"])
        .output()
        .expect("invoke skillfs");
    assert!(!out.status.success(), "expected non-zero exit");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("failed to load managed state"),
        "expected managed-state load error, got: {combined}"
    );
}

#[test]
fn control_socket_created_and_accepts_ping() {
    // FUSE is required to bring up the mount and the control socket. Gate
    // explicitly rather than silently passing when the socket never binds.
    if !fuse_available() {
        eprintln!("SKIP: FUSE unavailable; cannot bring up the control socket");
        return;
    }
    // Daemon-visible source and socket: with the control plane enabled the
    // PrivateTmp gate rejects a daemon-facing source/socket under /tmp.
    let source = non_tmp_dir();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let sock_dir = non_tmp_socket_dir();
    let sock_path = sock_dir.path().join("skillfs.sock");

    // Trusted peer = this test binary, so our own probe authenticates and
    // must receive a real pong (not a permission_denied).
    let mut child = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--activation-mode",
            "file",
            "--control-socket",
            sock_path.to_str().unwrap(),
            "--trusted-peer-exe",
            &test_exe(),
        ])
        .stderr(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn skillfs");

    std::thread::sleep(std::time::Duration::from_secs(2));

    // Capture state before teardown so the failure path never leaks the
    // mount (all assertions run after stop_mount_child).
    let child_alive = matches!(child.try_wait(), Ok(None));
    let socket_exists = sock_path.exists();
    let probe = if socket_exists {
        probe_control_socket(&sock_path)
    } else {
        None
    };

    stop_mount_child(&mut child, mount.path());
    let output = child.wait_with_output();

    assert!(
        child_alive,
        "child exited before binding the control socket: {:?}",
        output.map(|o| String::from_utf8_lossy(&o.stderr).into_owned())
    );
    assert!(
        socket_exists,
        "FUSE is available but the control socket {} was not created",
        sock_path.display()
    );
    let resp = probe.expect("control socket must respond");
    assert!(
        response_is_authenticated_pong(&resp),
        "control socket did not return an authenticated pong: {resp}"
    );
}

#[test]
fn control_socket_preserves_symlink_source_identity() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE unavailable; cannot test symlink source identity");
        return;
    }

    let physical_source = non_tmp_dir();
    let skill_dir = physical_source.path().join("my-skill");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: my-skill\ndescription: fixture\n---\n",
    )
    .unwrap();
    let identity_parent = non_tmp_dir();
    let identity_root = identity_parent.path().join("skills-link");
    std::os::unix::fs::symlink(physical_source.path(), &identity_root).unwrap();

    let mount = tempfile::tempdir().expect("mount tempdir");
    let sock_dir = non_tmp_socket_dir();
    let sock_path = sock_dir.path().join("skillfs.sock");
    let mut child = Command::new(bin_path())
        .args([
            "mount",
            identity_root.to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--activation-mode",
            "file",
            "--control-socket",
            sock_path.to_str().unwrap(),
            "--trusted-peer-exe",
            &test_exe(),
        ])
        .stderr(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn skillfs");

    std::thread::sleep(Duration::from_secs(2));
    let child_alive = matches!(child.try_wait(), Ok(None));
    let request_identity = identity_root.join("my-skill");
    let request = serde_json::json!({
        "schemaVersion": "1",
        "method": "skill.resolveLiveSource",
        "canonicalSkillDir": request_identity,
    });
    let response = request_control_socket(&sock_path, &request.to_string());

    stop_mount_child(&mut child, mount.path());
    let output = child.wait_with_output();

    assert!(
        child_alive,
        "child exited before serving symlink identity: {:?}",
        output.map(|o| String::from_utf8_lossy(&o.stderr).into_owned())
    );
    let response: serde_json::Value =
        serde_json::from_str(&response.expect("control socket must respond")).unwrap();
    assert_eq!(response["ok"], true);
    assert_eq!(
        response["result"]["canonicalSkillDir"],
        request_identity.to_string_lossy().as_ref()
    );
    assert_eq!(response["result"]["skillId"], "my-skill");
    assert_eq!(
        response["result"]["liveSkillDir"],
        physical_source
            .path()
            .canonicalize()
            .unwrap()
            .join("my-skill")
            .to_string_lossy()
            .as_ref()
    );
}

#[test]
fn control_socket_default_endpoint_used_when_no_path() {
    // A trusted peer with no explicit --control-socket binds the default
    // per-user endpoint /run/user/<uid>/skillfs/control.sock.
    let _serial = DEFAULT_ENDPOINT_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let uid_out = Command::new("id").arg("-u").output().expect("id -u");
    let uid = String::from_utf8_lossy(&uid_out.stdout).trim().to_string();
    let runtime_dir = format!("/run/user/{uid}");
    if !std::path::Path::new(&runtime_dir).is_dir() {
        eprintln!("SKIP: {runtime_dir} unavailable; cannot test default endpoint");
        return;
    }
    let default_sock = format!("{runtime_dir}/skillfs/control.sock");

    if !fuse_available() {
        eprintln!("SKIP: FUSE unavailable; cannot test default endpoint end-to-end");
        return;
    }
    // Refuse to run if the default endpoint is already occupied — otherwise
    // a pre-existing listener would make this test a false positive.
    let sock_path = std::path::PathBuf::from(&default_sock);
    if sock_path.exists() {
        eprintln!("SKIP: default endpoint {default_sock} already in use");
        return;
    }

    // Daemon-visible source: with the control plane enabled and no backing
    // root, the resolver's live root falls back to the source, so a source
    // under /tmp would be rejected by the PrivateTmp gate.
    let source = non_tmp_dir();
    let mount = tempfile::tempdir().expect("mount tempdir");
    // Trusted peer = this test binary, so our own probe authenticates and
    // receives a real pong (not a permission_denied that could mask a
    // different instance answering on a pre-existing socket).
    let mut child = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--activation-mode",
            "file",
            // No --control-socket: the default endpoint must be used.
            "--trusted-peer-exe",
            &test_exe(),
        ])
        .stderr(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn skillfs");

    std::thread::sleep(std::time::Duration::from_secs(2));

    // The child must still be running: a dead child means startup failed,
    // which is a real failure now that FUSE is known available.
    let child_alive = matches!(child.try_wait(), Ok(None));
    let socket_exists = sock_path.exists();
    let uid_ok = if socket_exists {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(&sock_path).map(|m| m.uid()).ok()
    } else {
        None
    };
    let probe = if socket_exists {
        probe_control_socket(&sock_path)
    } else {
        None
    };

    stop_mount_child(&mut child, mount.path());
    let output = child.wait_with_output();

    assert!(
        child_alive,
        "child exited before binding the default endpoint: {:?}",
        output.map(|o| String::from_utf8_lossy(&o.stderr).into_owned())
    );
    assert!(
        socket_exists,
        "FUSE is available but the default endpoint {default_sock} was not created"
    );
    // Under /run/user, never a public temp directory.
    assert!(
        !default_sock.contains("/tmp") && !default_sock.contains("/var/tmp"),
        "default endpoint must not use a public temp dir"
    );
    // Owned by the current uid.
    let our_uid: u32 = uid.parse().expect("uid");
    assert_eq!(
        uid_ok,
        Some(our_uid),
        "default endpoint must be owned by us"
    );
    // An authenticated pong proves a live server that trusts this binary —
    // not some other instance that pre-occupied the path.
    let resp = probe.expect("default endpoint must respond");
    assert!(
        response_is_authenticated_pong(&resp),
        "default endpoint did not return an authenticated pong: {resp}"
    );
}

#[test]
fn control_socket_config_only_default_endpoint() {
    // Config-only trusted peer (no path, no CLI control-socket flags) must
    // enable the control plane on the default per-user endpoint — the
    // config loader no longer rejects a trusted_peer_exe without a path.
    let _serial = DEFAULT_ENDPOINT_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let uid_out = Command::new("id").arg("-u").output().expect("id -u");
    let uid = String::from_utf8_lossy(&uid_out.stdout).trim().to_string();
    let runtime_dir = format!("/run/user/{uid}");
    if !Path::new(&runtime_dir).is_dir() {
        eprintln!("SKIP: {runtime_dir} unavailable; cannot test default endpoint");
        return;
    }
    let default_sock = format!("{runtime_dir}/skillfs/control.sock");
    if !fuse_available() {
        eprintln!("SKIP: FUSE unavailable; cannot test default endpoint end-to-end");
        return;
    }
    let sock_path = std::path::PathBuf::from(&default_sock);
    if sock_path.exists() {
        eprintln!("SKIP: default endpoint {default_sock} already in use");
        return;
    }

    // Daemon-visible source (see the sibling default-endpoint test): the
    // PrivateTmp gate rejects a /tmp source-fallback when the control plane
    // is enabled.
    let source = non_tmp_dir();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let config_dir = tempfile::tempdir().expect("config dir");
    let config_path = config_dir.path().join("security.toml");
    // Only a trusted peer, no [control_socket].path.
    std::fs::write(
        &config_path,
        format!(
            r#"
[activation]
mode = "file"

[control_socket]
trusted_peer_exe = "{}"
"#,
            test_exe()
        ),
    )
    .unwrap();

    let mut child = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--config",
            config_path.to_str().unwrap(),
        ])
        .stderr(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn skillfs");

    std::thread::sleep(std::time::Duration::from_secs(2));
    let child_alive = matches!(child.try_wait(), Ok(None));
    let socket_exists = sock_path.exists();
    let probe = if socket_exists {
        probe_control_socket(&sock_path)
    } else {
        None
    };

    stop_mount_child(&mut child, mount.path());
    let output = child.wait_with_output();

    assert!(
        child_alive,
        "child exited before binding the config-only default endpoint: {:?}",
        output.map(|o| String::from_utf8_lossy(&o.stderr).into_owned())
    );
    assert!(
        socket_exists,
        "FUSE is available but the config-only default endpoint was not created"
    );
    let resp = probe.expect("config-only default endpoint must respond");
    assert!(
        response_is_authenticated_pong(&resp),
        "config-only default endpoint did not return an authenticated pong: {resp}"
    );
}

#[test]
fn control_socket_in_place_without_backing_root_fails_startup() {
    // An in-place mount (source == mountpoint) with the control plane
    // enabled must require --ledger-backing-root: the FUSE over-mount hides
    // the physical source, so the resolver would otherwise read the
    // current/fallback/hidden view instead of the live source. This must
    // fail startup, not silently mount.
    //
    // Daemon-visible source and socket so the PrivateTmp gate passes and the
    // in-place backing-root gate is the one that fires.
    let source = non_tmp_dir();
    let sock_dir = non_tmp_socket_dir();
    let sock_path = sock_dir.path().join("skillfs.sock");
    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            // Same path → in-place mount.
            source.path().to_str().unwrap(),
            "--security",
            "--activation-mode",
            "file",
            "--control-socket",
            sock_path.to_str().unwrap(),
            "--trusted-peer-exe",
            bin_path(),
        ])
        .output()
        .expect("invoke skillfs");
    assert!(
        !out.status.success(),
        "in-place control-socket mount without backing root must fail startup"
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("--ledger-backing-root"),
        "error must require --ledger-backing-root, got: {combined}"
    );
    // The socket must not have been created — startup failed first.
    assert!(
        !sock_path.exists(),
        "no control socket must be bound on a rejected startup"
    );
}

#[test]
fn in_place_notify_without_resolver_fails_startup() {
    let source = non_tmp_dir();
    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            source.path().to_str().unwrap(),
            "--security",
            "--activation-mode",
            "file",
            "--notify-socket",
            "/run/skillfs-in-place-notify.sock",
        ])
        .output()
        .expect("invoke skillfs");
    assert!(!out.status.success(), "expected non-zero exit");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("authenticated live-source resolver")
            && combined.contains("--trusted-peer-exe"),
        "in-place notify must require the resolver control plane, got: {combined}"
    );
    assert!(
        !combined.contains("--ledger-backing-root setup failed"),
        "resolver gate must run before backing-root setup, got: {combined}"
    );
}

#[test]
fn in_place_notify_with_resolver_reaches_backing_root_gate() {
    let source = non_tmp_dir();
    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            source.path().to_str().unwrap(),
            "--security",
            "--activation-mode",
            "file",
            "--notify-socket",
            "/run/skillfs-in-place-notify.sock",
            "--trusted-peer-exe",
            bin_path(),
        ])
        .output()
        .expect("invoke skillfs");
    assert!(!out.status.success(), "expected non-zero exit");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("--ledger-backing-root"),
        "resolver-enabled in-place notify must reach backing-root gate, got: {combined}"
    );
    assert!(
        !combined.contains("authenticated live-source resolver"),
        "configured resolver must satisfy the notify gate, got: {combined}"
    );
}

#[test]
fn out_of_place_notify_with_backing_root_without_resolver_fails_startup() {
    let source = non_tmp_dir();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let backing = mount.path().join("backing");
    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--activation-mode",
            "file",
            "--notify-socket",
            "/run/skillfs-out-of-place-notify.sock",
            "--ledger-backing-root",
            backing.to_str().unwrap(),
        ])
        .output()
        .expect("invoke skillfs");
    assert!(!out.status.success(), "expected non-zero exit");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("authenticated live-source resolver")
            && combined.contains("--trusted-peer-exe")
            && combined.contains("--ledger-backing-root"),
        "backing-root notify must require the resolver control plane, got: {combined}"
    );
    assert!(
        !backing.exists(),
        "resolver gate must run before backing-root setup"
    );
}

#[test]
fn config_backing_root_with_notify_without_resolver_fails_startup() {
    let source = non_tmp_dir();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let backing = mount.path().join("backing");
    let config_dir = tempfile::tempdir().expect("config dir");
    let config_path = config_dir.path().join("security.toml");
    std::fs::write(
        &config_path,
        format!(
            "[activation]\nmode = \"file\"\n[ledger]\nbacking_root = \"{}\"\n",
            backing.display()
        ),
    )
    .unwrap();

    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--config",
            config_path.to_str().unwrap(),
            "--notify-socket",
            "/run/skillfs-config-backing-notify.sock",
        ])
        .output()
        .expect("invoke skillfs");
    assert!(!out.status.success(), "expected non-zero exit");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("authenticated live-source resolver")
            && combined.contains("--ledger-backing-root"),
        "merged config backing root must require the resolver, got: {combined}"
    );
    assert!(
        !backing.exists(),
        "resolver gate must run before config backing-root setup"
    );
}

#[test]
fn out_of_place_notify_with_backing_root_and_resolver_passes_gate() {
    let source = non_tmp_dir();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let combined = run_briefly(
        mount.path(),
        &[
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--activation-mode",
            "file",
            "--notify-socket",
            "/run/skillfs-out-of-place-notify.sock",
            "--ledger-backing-root",
            source.path().to_str().unwrap(),
            "--trusted-peer-exe",
            bin_path(),
        ],
    );
    assert!(
        !combined.contains("authenticated live-source resolver"),
        "configured resolver must satisfy backing-root notify gate, got: {combined}"
    );
}

// ---------------------------------------------------------------------------
// Hermes layout + security gates (H2)
// ---------------------------------------------------------------------------

#[test]
fn hermes_security_activation_file_passes_gate() {
    let source = empty_source();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let mut child = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--skill-layout",
            "hermes",
            "--security",
            "--activation-mode",
            "file",
            "--foreground",
        ])
        .stderr(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn skillfs");

    std::thread::sleep(std::time::Duration::from_secs(2));
    stop_mount_child(&mut child, mount.path());
    let output = child.wait_with_output().expect("wait");
    assert!(
        !is_mounted(mount.path()),
        "test mount must be removed after child shutdown: {}",
        mount.path().display()
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        !combined.contains("incompatible"),
        "hermes + security + activation-mode file must pass gate: {combined}"
    );
}

#[test]
fn hermes_decision_command_rejected() {
    let source = empty_source();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--skill-layout",
            "hermes",
            "--security",
            "--decision-command",
            "echo",
        ])
        .output()
        .expect("invoke skillfs");
    assert!(
        !out.status.success(),
        "hermes + decision-command must fail startup"
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("incompatible"),
        "error must mention incompatibility: {combined}"
    );
}

#[test]
fn hermes_control_socket_allowed() {
    // The read-only resolver derives full nested skill ids from the
    // canonical path, so Hermes layout is compatible with the control
    // socket. The blanket incompatibility gate has been removed. Prove a
    // live, authenticated server actually comes up — not merely that no
    // "incompatible" text was printed (which an unrelated startup failure
    // could mask).
    if !fuse_available() {
        eprintln!("SKIP: FUSE unavailable; cannot bring up the Hermes control socket");
        return;
    }
    // Daemon-visible source and socket so the PrivateTmp gate does not
    // reject the control plane on a /tmp fixture.
    let source = non_tmp_dir();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let sock_dir = non_tmp_socket_dir();
    let sock_path = sock_dir.path().join("skillfs.sock");
    let mut child = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--skill-layout",
            "hermes",
            "--security",
            "--activation-mode",
            "file",
            "--control-socket",
            sock_path.to_str().unwrap(),
            "--trusted-peer-exe",
            &test_exe(),
            "--foreground",
        ])
        .stderr(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn skillfs");

    std::thread::sleep(std::time::Duration::from_secs(2));
    // Capture all state before teardown so the failure path never leaks the
    // mount (assertions run after stop_mount_child).
    let child_alive = matches!(child.try_wait(), Ok(None));
    let socket_exists = sock_path.exists();
    let probe = if socket_exists {
        probe_control_socket(&sock_path)
    } else {
        None
    };
    stop_mount_child(&mut child, mount.path());
    let output = child.wait_with_output().expect("wait");
    let still_mounted = is_mounted(mount.path());
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    assert!(
        !still_mounted,
        "test mount must be removed after child shutdown: {}",
        mount.path().display()
    );
    assert!(
        !combined.contains("incompatible"),
        "hermes + control-socket must not trigger an incompatibility gate: {combined}"
    );
    assert!(
        child_alive,
        "child exited before binding the Hermes control socket: {combined}"
    );
    assert!(
        socket_exists,
        "FUSE is available but the Hermes control socket was not created: {combined}"
    );
    let resp = probe.expect("hermes control socket must respond");
    assert!(
        response_is_authenticated_pong(&resp),
        "hermes control socket server did not return an authenticated pong: {resp}"
    );
}

#[test]
fn hermes_layout_without_security_passes_gate() {
    let source = empty_source();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let mut child = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--skill-layout",
            "hermes",
            "--foreground",
        ])
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn skillfs");

    std::thread::sleep(std::time::Duration::from_secs(2));
    stop_mount_child(&mut child, mount.path());
    let output = child.wait_with_output().expect("wait");
    assert!(
        !is_mounted(mount.path()),
        "test mount must be removed after child shutdown: {}",
        mount.path().display()
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("incompatible"),
        "hermes without security must not trigger incompatibility gate: {stderr}"
    );
}

#[test]
fn hermes_config_decision_command_rejected() {
    let source = empty_source();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let config_dir = tempfile::tempdir().expect("config dir");
    let config_path = config_dir.path().join("security.toml");
    std::fs::write(
        &config_path,
        r#"
[skills]
layout = "hermes"

[decision]
command = "agent-sec-cli skill-ledger"
"#,
    )
    .unwrap();

    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--config",
            config_path.to_str().unwrap(),
        ])
        .output()
        .expect("invoke skillfs");
    assert!(
        !out.status.success(),
        "hermes (config) + decision-command (config) must fail"
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("incompatible"),
        "config-sourced hermes + decision-command must be rejected: {combined}"
    );
}

#[test]
fn hermes_config_control_socket_allowed() {
    // Config-sourced Hermes + control socket is allowed: the read-only
    // resolver derives full nested skill ids, so no incompatibility gate
    // fires. Prove a live, authenticated server comes up rather than just
    // asserting the absence of "incompatible".
    if !fuse_available() {
        eprintln!("SKIP: FUSE unavailable; cannot bring up the Hermes control socket");
        return;
    }
    // Daemon-visible source and socket so the PrivateTmp gate does not
    // reject the control plane on a /tmp fixture.
    let source = non_tmp_dir();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let sock_dir = non_tmp_socket_dir();
    let sock_path = sock_dir.path().join("skillfs.sock");
    let config_dir = tempfile::tempdir().expect("config dir");
    let config_path = config_dir.path().join("security.toml");
    std::fs::write(
        &config_path,
        format!(
            r#"
[skills]
layout = "hermes"

[activation]
mode = "file"

[control_socket]
path = "{}"
trusted_peer_exe = "{}"
"#,
            sock_path.display(),
            test_exe()
        ),
    )
    .unwrap();

    let mut child = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--config",
            config_path.to_str().unwrap(),
            "--foreground",
        ])
        .stderr(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn skillfs");

    std::thread::sleep(std::time::Duration::from_secs(2));
    // Capture all state before teardown so the failure path never leaks the
    // mount (assertions run after stop_mount_child).
    let child_alive = matches!(child.try_wait(), Ok(None));
    let socket_exists = sock_path.exists();
    let probe = if socket_exists {
        probe_control_socket(&sock_path)
    } else {
        None
    };
    stop_mount_child(&mut child, mount.path());
    let output = child.wait_with_output().expect("wait");
    let still_mounted = is_mounted(mount.path());
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    assert!(
        !still_mounted,
        "test mount must be removed after child shutdown: {}",
        mount.path().display()
    );
    assert!(
        !combined.contains("incompatible"),
        "config-sourced hermes + control-socket must not trigger an incompatibility gate: {combined}"
    );
    assert!(
        child_alive,
        "child exited before binding the config-sourced Hermes control socket: {combined}"
    );
    assert!(
        socket_exists,
        "FUSE is available but the config-sourced Hermes control socket was not created: {combined}"
    );
    let resp = probe.expect("hermes (config) control socket must respond");
    assert!(
        response_is_authenticated_pong(&resp),
        "hermes (config) control socket server did not return an authenticated pong: {resp}"
    );
}

#[test]
fn mount_file_rejects_invalid_configuration_and_option_mixing() {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("mount.toml");
    let mount = temp.path().join("mount");
    let source = temp.path().join("source");
    std::fs::create_dir(&source).unwrap();
    let valid = format!("mountpoint = {:?}\nsources = [{:?}]\n", mount, source);
    let cases = [
        (
            "mountpoint = '/mnt/view'\nsources = []\n".to_string(),
            vec![],
            "must not be empty",
        ),
        (
            "mountpoint = '/mnt/view'\nsources = ['relative']\n".to_string(),
            vec![],
            "absolute",
        ),
        (
            format!("{valid}unexpected = true\n"),
            vec![],
            "unknown field",
        ),
        (valid.clone(), vec!["--security"], "cannot be combined"),
        (valid.clone(), vec!["--managed"], "cannot be combined"),
        (
            valid.clone(),
            vec!["--skill-layout", "hermes"],
            "cannot be combined",
        ),
        (
            valid.clone(),
            vec![source.to_str().unwrap(), mount.to_str().unwrap()],
            "cannot be combined",
        ),
        ("[audit]\n".to_string(), vec![], "SOURCE is required"),
    ];
    for (body, extra, expected) in cases {
        std::fs::write(&config, body).unwrap();
        let out = Command::new(bin_path())
            .args(["mount", "--config"])
            .arg(&config)
            .args(extra)
            .output()
            .unwrap();
        assert!(!out.status.success());
        assert!(
            String::from_utf8_lossy(&out.stderr).contains(expected),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!mount.exists());
    }
}

#[test]
fn mount_file_aggregates_whole_skills_read_only() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE unavailable");
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let high = temp.path().join("high");
    let low = temp.path().join("low");
    let mount = temp.path().join("mount");
    for (root, name, body) in [
        (&high, "demo", "winner"),
        (&low, "demo", "shadowed"),
        (&low, "other", "second source"),
        (&low, "reserve", "secondary view"),
        (&low, "unassigned", "not assigned to a view"),
    ] {
        std::fs::create_dir_all(root.join(name)).unwrap();
        std::fs::write(
            root.join(name).join("SKILL.md"),
            format!("---\nname: {name}\ndescription: fixture\n---\n{body}\n"),
        )
        .unwrap();
        std::fs::write(root.join(name).join("resource.txt"), body).unwrap();
    }
    let views = "[[view]]\nname = 'default'\ndefault = true\nskills = ['demo', 'other']\n[[view]]\nname = 'reserve'\nskills = ['reserve']\n";
    std::fs::write(high.join("skillfs-views.toml"), views).unwrap();
    std::fs::write(
        low.join("skillfs-views.toml"),
        "[[view]]\nname = 'default'\ndefault = true\nskills = []\n",
    )
    .unwrap();
    std::fs::write(low.join("demo/low-only.txt"), "must not merge").unwrap();
    let config = temp.path().join("mount.toml");
    std::fs::write(
        &config,
        format!(
            "mountpoint = {:?}\nsources = [{:?}, {:?}]\n",
            mount, high, low
        ),
    )
    .unwrap();
    struct RunningMount(Child, std::path::PathBuf);
    impl Drop for RunningMount {
        fn drop(&mut self) {
            stop_mount_child(&mut self.0, &self.1);
            let _ = self.0.wait();
        }
    }
    let log = std::fs::File::create(temp.path().join("mount.log")).unwrap();
    let mut child = RunningMount(
        Command::new(bin_path())
            .args(["mount", "--config"])
            .arg(&config)
            .arg("--foreground")
            .stderr(log)
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap(),
        mount.clone(),
    );
    for _ in 0..100 {
        if is_mounted(&mount) || child.0.try_wait().unwrap().is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        is_mounted(&mount),
        "{}",
        std::fs::read_to_string(temp.path().join("mount.log")).unwrap()
    );
    let skill = mount.join("skills/demo");
    assert!(
        std::fs::read_to_string(skill.join("SKILL.md"))
            .unwrap()
            .contains("winner")
    );
    assert_eq!(
        std::fs::read_to_string(skill.join("resource.txt")).unwrap(),
        "winner"
    );
    assert_eq!(
        std::fs::read_to_string(mount.join("skills/other/resource.txt")).unwrap(),
        "second source"
    );
    assert!(!skill.join("low-only.txt").exists());
    let visible: Vec<_> = std::fs::read_dir(mount.join("skills"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert!(visible.iter().any(|name| name == "unassigned"));
    assert!(!visible.iter().any(|name| name == "reserve"));
    assert_eq!(
        std::fs::read_to_string(mount.join("skills/unassigned/resource.txt")).unwrap(),
        "not assigned to a view"
    );
    assert_eq!(
        std::fs::read_to_string(high.join("skillfs-views.toml")).unwrap(),
        views
    );
    let discovery = std::fs::read_to_string(mount.join("skills/skill-discover/SKILL.md")).unwrap();
    assert!(discovery.contains(&mount.join("skills/reserve").display().to_string()));
    for result in [
        std::fs::write(skill.join("resource.txt"), "changed"),
        std::fs::remove_file(skill.join("resource.txt")),
        std::fs::create_dir(mount.join("skills/new")),
        std::fs::rename(skill.join("resource.txt"), skill.join("renamed")),
    ] {
        assert_eq!(result.unwrap_err().raw_os_error(), Some(libc::EROFS));
    }
    assert_eq!(
        std::fs::read_to_string(high.join("demo/resource.txt")).unwrap(),
        "winner"
    );
    drop(child);
    assert!(!is_mounted(&mount));
}
