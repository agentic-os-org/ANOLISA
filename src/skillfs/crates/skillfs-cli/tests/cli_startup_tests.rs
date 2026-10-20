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

/// The instance id the previous binary derived for a normalized
/// mountpoint — the sanitization of `instance_id_for` before the
/// leading-dash fix (map every non-`[a-zA-Z0-9_-]` to `-`, then take 24,
/// no stripping). Valid ids from that formula — anything not starting
/// with a dash — are the live upgrade surface: an old supervisor may be
/// holding exactly these state/pid file names when the binary is
/// upgraded, so the new derivation must keep them byte-identical.
fn legacy_instance_id(normalized: &Path) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    normalized.as_os_str().hash(&mut hasher);
    let hash = hasher.finish();
    let base: String = normalized
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "root".to_string())
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .take(24)
        .collect();
    let base = if base.is_empty() {
        "root".to_string()
    } else {
        base
    };
    format!("{base}-{hash:016x}")
}

/// Comma-separated file names under a runtime dir, for assertion
/// messages: shows exactly which instance ids the CLI materialized.
fn list_runtime_files(dir: &Path) -> String {
    match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(", "),
        Err(_) => "<unreadable>".to_string(),
    }
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
fn events_log_inside_source_fails_startup() {
    // Sibling of the --audit-log / --activation-events-log gates: a security
    // event log inside the source tree makes every write observable by the
    // drift watcher and can overwrite a SKILL.md.
    let source = empty_source();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let inside = source.path().join("events.jsonl");
    let mut child = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--decision-command",
            "/bin/true",
            "--events-log",
            inside.to_str().unwrap(),
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn skillfs");

    // The gate rejects during validation, before the FUSE session starts, so
    // the child must exit promptly. If it is still alive the guard did not
    // fire (a regression would leave a live mount behind): tear it down and
    // fail.
    let mut status = None;
    for _ in 0..50 {
        match child.try_wait() {
            Ok(Some(s)) => {
                status = Some(s);
                break;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => panic!("wait for skillfs: {e}"),
        }
    }
    let status = match status {
        Some(s) => s,
        None => {
            stop_mount_child(&mut child, mount.path());
            panic!("mount with an in-source --events-log must not start");
        }
    };
    assert!(
        !status.success(),
        "expected non-zero exit for an in-source log"
    );

    use std::io::Read;
    let mut stdout = String::new();
    let mut stderr = String::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_string(&mut stdout);
    }
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_string(&mut stderr);
    }
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains("lies inside the SkillFS source root"),
        "expected the events-log source-tree rejection, got: {combined}"
    );
    assert!(
        !inside.exists(),
        "rejected startup must not create the log file"
    );
}

#[test]
fn events_log_dangling_symlink_into_source_fails_startup() {
    // Sibling of `events_log_inside_source_fails_startup`: an EXTERNAL
    // symlink whose target does not exist yet must not be validated by its
    // own (external) location. The later open(O_CREAT) would follow the
    // link and create the log inside the source tree, so
    // resolve_events_path follows the dangling link and the inside-source
    // guard must abort startup before any file is created.
    let source = empty_source();
    let mount = tempfile::tempdir().expect("mount tempdir");
    let link_parent = tempfile::tempdir().expect("link parent tempdir");
    let link = link_parent.path().join("events-dangling.jsonl");
    let planted = source.path().join("planted-events.jsonl");
    std::os::unix::fs::symlink(&planted, &link).expect("create dangling symlink");
    let mut child = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--decision-command",
            "/bin/true",
            "--events-log",
            link.to_str().unwrap(),
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn skillfs");

    // The gate rejects during validation, before the FUSE session starts, so
    // the child must exit promptly. If it is still alive the guard did not
    // fire (a regression would leave a live mount behind): tear it down and
    // fail.
    let mut status = None;
    for _ in 0..50 {
        match child.try_wait() {
            Ok(Some(s)) => {
                status = Some(s);
                break;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => panic!("wait for skillfs: {e}"),
        }
    }
    let status = match status {
        Some(s) => s,
        None => {
            stop_mount_child(&mut child, mount.path());
            panic!("mount with a symlinked in-source --events-log must not start");
        }
    };
    assert!(
        !status.success(),
        "expected non-zero exit for a dangling symlink into the source"
    );

    use std::io::Read;
    let mut stdout = String::new();
    let mut stderr = String::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_string(&mut stdout);
    }
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_string(&mut stderr);
    }
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains("lies inside the SkillFS source root"),
        "expected the events-log source-tree rejection for the link target, got: {combined}"
    );
    assert!(
        !planted.exists(),
        "rejected startup must not create the link's target file"
    );
}

#[test]
fn events_log_hard_link_into_source_fails_startup() {
    // Hard links are the alias the containment guard cannot see: the external
    // name canonicalizes outside the source root, yet it is the same inode as
    // the file inside it, so an append through the external name rewrites the
    // source file. The writer must refuse the shared inode and abort startup.
    let source = empty_source();
    let manifest = source.path().join("SKILL.md");
    std::fs::write(&manifest, "# trusted manifest\n").expect("write the source file");
    let mount = tempfile::tempdir().expect("mount tempdir");
    let link_parent = tempfile::tempdir().expect("link parent tempdir");
    let link = link_parent.path().join("events.jsonl");
    std::fs::hard_link(&manifest, &link).expect("create the external hard link");

    let mut child = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--decision-command",
            "/bin/true",
            "--events-log",
            link.to_str().unwrap(),
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn skillfs");

    // The gate rejects while the writer is opened, before the FUSE session
    // starts, so the child must exit promptly. If it is still alive the guard
    // did not fire (a regression would leave a live mount behind): tear it
    // down and fail.
    let mut status = None;
    for _ in 0..50 {
        match child.try_wait() {
            Ok(Some(s)) => {
                status = Some(s);
                break;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => panic!("wait for skillfs: {e}"),
        }
    }
    let status = match status {
        Some(s) => s,
        None => {
            stop_mount_child(&mut child, mount.path());
            panic!("mount with a hard-linked in-source --events-log must not start");
        }
    };
    assert!(
        !status.success(),
        "expected non-zero exit for a hard link into the source"
    );

    use std::io::Read;
    let mut stdout = String::new();
    let mut stderr = String::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_string(&mut stdout);
    }
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_string(&mut stderr);
    }
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains("hard links"),
        "expected the hard-link refusal, got: {combined}"
    );
    assert!(
        !is_mounted(mount.path()),
        "the refusal must happen before the FUSE session starts"
    );
    assert_eq!(
        std::fs::read(&manifest).expect("read the source file"),
        b"# trusted manifest\n",
        "a refused events log must not append to the source file it aliases"
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
fn control_socket_relative_path_on_unsafe_cwd_fails_startup() {
    // A relative --control-socket is served from the cwd: a multi-level
    // relative path must validate the REAL ancestor chain — the cwd and
    // everything above it — not just the literal relative components. With
    // the child's cwd inside a world-writable directory the ancestor
    // policy rejects the mount; the PrivateTmp gate passes because the cwd
    // is outside /tmp and /var/tmp.
    let source = non_tmp_dir();
    let mount = non_tmp_dir();
    let unsafe_cwd = non_tmp_dir();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(unsafe_cwd.path(), std::fs::Permissions::from_mode(0o0777))
        .expect("open up the cwd");

    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--activation-mode",
            "file",
            "--control-socket",
            "sub/deeper/control.sock",
            "--trusted-peer-exe",
            &test_exe(),
        ])
        .current_dir(unsafe_cwd.path())
        .output()
        .expect("invoke skillfs");
    assert!(
        !out.status.success(),
        "expected non-zero exit for a multi-level relative socket on an unsafe cwd"
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("group/other writable"),
        "expected the ancestor-writability rejection, got: {combined}"
    );
}

#[test]
#[cfg(unix)]
fn control_socket_bare_relative_name_tightens_unsafe_cwd() {
    // A bare relative socket name (`control.sock`) has no parent component
    // and lives directly in the cwd, which is therefore the socket parent.
    // The old code returned Ok for an empty parent, silently skipping every
    // check. Now the real location participates in the same policy as an
    // absolute path: a world-writable cwd is either tightened to 0700
    // (when the ancestor chain above it is acceptable) or the mount is
    // rejected with the ancestor error. Binding with the cwd still
    // world-writable is the one forbidden outcome.
    let source = non_tmp_dir();
    let mount = non_tmp_dir();
    let unsafe_cwd = non_tmp_dir();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(unsafe_cwd.path(), std::fs::Permissions::from_mode(0o0777))
        .expect("open up the cwd");

    let mut child = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.path().to_str().unwrap(),
            "--security",
            "--activation-mode",
            "file",
            "--control-socket",
            "control.sock",
            "--trusted-peer-exe",
            &test_exe(),
        ])
        .current_dir(unsafe_cwd.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("invoke skillfs");

    // Give the child time to reach the socket setup: gate, tighten-or-
    // reject, and (if tightened) the bind and FUSE session. Under the full
    // suite's parallel load the child's own startup can take seconds, so
    // wait generously before tearing it down; the security outcome is
    // already decided by the time the mount starts.
    for _ in 0..200 {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => panic!("wait for skillfs: {e}"),
        }
    }
    let mut stdout = String::new();
    let mut stderr = String::new();
    if child.try_wait().ok().flatten().is_none() {
        stop_mount_child(&mut child, mount.path());
    }
    use std::io::Read;
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_string(&mut stdout);
    }
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_string(&mut stderr);
    }
    let status = child.wait().expect("wait for skillfs child");
    let combined = format!("{stdout}{stderr}\n[child status: {status}]");

    let mode = std::fs::metadata(unsafe_cwd.path())
        .expect("stat the child's cwd")
        .permissions()
        .mode()
        & 0o777;
    assert!(
        mode == 0o700 || combined.contains("socket ancestor"),
        "an unsafe cwd must be tightened (mode 0700) or the bind rejected \
         with the ancestor error, got mode {mode:o} and output: {combined}"
    );
}

#[test]
#[cfg(unix)]
fn control_socket_parent_created_0700_under_permissive_umask() {
    // Under umask 0000, create_dir_all builds the missing socket-parent
    // levels as 0777 and the ancestor validation then rejects the path the
    // mount itself just created. Every level must come out 0700 instead,
    // so the mount survives the leakiest umask. The child process carries
    // the umask (`sh -c 'umask 0000 && exec ...'`) because umask is
    // process-global.
    let source = non_tmp_dir();
    let mount = non_tmp_dir();
    let socket_root = non_tmp_dir();
    let socket = socket_root.path().join("a").join("b").join("control.sock");

    let mut child = Command::new("/bin/sh")
        .arg("-c")
        .arg(format!(
            "umask 0000 && exec {} mount {} {} --security --activation-mode file \
             --control-socket {} --trusted-peer-exe {}",
            bin_path(),
            source.path().display(),
            mount.path().display(),
            socket.display(),
            test_exe(),
        ))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn skillfs under umask 0000");

    // Give the child time to reach the socket setup. Whatever it does
    // afterwards (a later gate, a successful mount that keeps running),
    // the levels it created must already be private.
    for _ in 0..50 {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => panic!("wait for skillfs: {e}"),
        }
    }
    if child.try_wait().ok().flatten().is_none() {
        stop_mount_child(&mut child, mount.path());
    }

    use std::os::unix::fs::PermissionsExt;
    for level in ["a", "a/b"] {
        let path = socket_root.path().join(level);
        let mode = std::fs::metadata(&path)
            .unwrap_or_else(|e| panic!("level {level} must exist: {e}"))
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode, 0o700,
            "level {level} must be created 0700 under umask 0000, got {mode:o}"
        );
    }
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
fn managed_mount_serves_a_dot_prefixed_mountpoint() {
    // Every test harness's tempdir is dot-prefixed (`tempfile` uses a
    // `.tmp*` prefix), and the derived instance id used to sanitize the
    // leading dot into a leading dash — which the supervisor's `--instance`
    // argument rejects as an option-looking value. The supervisor then
    // died before writing its pid file, so the client's liveness check
    // never fired either: it polled the full readiness budget and failed
    // with "did not become ready" and an empty worker log tail, pointing
    // every investigation at the wrong layer.
    if !fuse_available() {
        eprintln!("SKIP: FUSE unavailable; cannot bring up a managed mount");
        return;
    }
    let source = empty_source();
    let mount_parent = tempfile::tempdir().expect("mount parent tempdir");
    let mount = mount_parent.path().join(".mnt");

    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.to_str().unwrap(),
            "--managed",
        ])
        .output()
        .expect("invoke skillfs");

    // Stop the instance before asserting, so a failure does not leak a
    // detached supervisor and a live FUSE mount into later tests.
    let stop = Command::new(bin_path())
        .args(["stop", mount.to_str().unwrap()])
        .output()
        .expect("invoke skillfs stop");

    assert!(
        out.status.success(),
        "a dot-prefixed mountpoint must be servable, got: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("managed mount ready"),
        "expected the ready line, got: {combined}"
    );
    assert!(
        stop.status.success(),
        "stop must clean up the instance, got: {}{}",
        String::from_utf8_lossy(&stop.stdout),
        String::from_utf8_lossy(&stop.stderr),
    );
}

#[test]
fn managed_mount_survives_a_slow_environment_probe() {
    // The managed worker pays the full mount startup cost before the FUSE
    // session serves: the 24-command environment probe
    // (`EnvironmentProfile::detect` spawns one `which` per whitelisted
    // command) plus store discovery and the session start. On a host with
    // slow process spawns (WSL, network home directories) that probe alone
    // costs ~10 s, so a 10 s readiness budget tore down a worker that was
    // still probing — `skillfs mount --managed` failed on hardware the
    // blocking mode serves fine. The budget must cover that regime.
    //
    // Deterministic on any host: a `which` shim prepended to the PATH
    // sleeps half a second per probe, so the worker's startup holds at
    // least 24 x 0.5 s = 12 s of wall clock regardless of machine speed —
    // comfortably past a 10 s budget, well inside a 30 s one.
    //
    // A non-dot mountpoint keeps this test about the budget alone: a
    // dot-prefixed one would fail for the instance-id reason covered by
    // `managed_mount_serves_a_dot_prefixed_mountpoint` instead.
    if !fuse_available() {
        eprintln!("SKIP: FUSE unavailable; cannot bring up a managed mount");
        return;
    }
    let source = empty_source();
    let mount_parent = tempfile::tempdir().expect("mount parent tempdir");
    let mount = mount_parent.path().join("mnt");

    // The shim answers "present" for every probe: claiming the whole
    // whitelist is harmless for an empty source (nothing compiles against
    // the profile), and skipping the delegation to the real `which` keeps
    // the per-probe cost to one shell spawn plus the sleep, so the bounds
    // hold on slow-spawn hosts (WSL) as well as fast CI runners.
    let shim_dir = tempfile::tempdir().expect("shim tempdir");
    let which = shim_dir.path().join("which");
    std::fs::write(&which, "#!/bin/sh\nsleep 0.5\nexit 0\n").expect("write which shim");
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&which, std::fs::Permissions::from_mode(0o755))
            .expect("chmod which shim");
    }

    let path = std::env::var_os("PATH").expect("PATH");
    let mut prefixed_path = std::ffi::OsString::from(shim_dir.path());
    prefixed_path.push(":");
    prefixed_path.push(&path);

    let out = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.to_str().unwrap(),
            "--managed",
        ])
        .env("PATH", &prefixed_path)
        .output()
        .expect("invoke skillfs");

    // Stop the instance before asserting, so a failure does not leak a
    // detached supervisor and a live FUSE mount into later tests.
    let stop = Command::new(bin_path())
        .args(["stop", mount.to_str().unwrap()])
        .env("PATH", &prefixed_path)
        .output()
        .expect("invoke skillfs stop");

    assert!(
        out.status.success(),
        "managed mount must survive a slow environment probe, got: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        combined.contains("managed mount ready"),
        "expected the ready line, got: {combined}"
    );
    assert!(
        stop.status.success(),
        "stop must clean up the instance, got: {}{}",
        String::from_utf8_lossy(&stop.stdout),
        String::from_utf8_lossy(&stop.stderr),
    );
}

#[test]
fn stop_terminates_a_legacy_underscore_instance() {
    // Upgrade regression (review): instance ids that never started with a
    // dash — a mount at `/mnt/_skills` used `_skills-<hash>` — were valid
    // `--instance` values and runtime file-name prefixes, so a live
    // supervisor can be sitting on those files when the binary is
    // upgraded. If the new id derivation renamed the instance, `stop`
    // would miss the incumbent's state/pid files, print "no managed
    // mount ... (already stopped)", and leave the old supervisor running
    // — free to remount after any later unmount. Seed the runtime dir
    // exactly as the previous binary wrote it and prove the new `stop`
    // resolves and terminates the incumbent. Needs no FUSE: the incumbent
    // is a stand-in process and the mountpoint stays unmounted.
    let source = empty_source();
    let parent = tempfile::tempdir().expect("parent tempdir");
    let mount = parent.path().join("_skills");
    std::fs::create_dir(&mount).expect("create mountpoint");
    let normalized = mount.canonicalize().expect("canonicalize mountpoint");

    let runtime = tempfile::tempdir().expect("runtime tempdir");
    let dir = runtime.path().join("skillfs");
    std::fs::create_dir(&dir).expect("create runtime dir");
    let legacy_id = legacy_instance_id(&normalized);
    assert!(
        legacy_id.starts_with("_skills-"),
        "the seeded id must keep its underscore prefix, got: {legacy_id}"
    );

    // A stand-in "old supervisor": alive, holding the legacy pid file and
    // state record exactly as the previous binary would have left them.
    let mut incumbent = Command::new("sleep")
        .arg("300")
        .spawn()
        .expect("spawn stand-in supervisor");
    let incumbent_pid_num = incumbent.id();
    std::fs::write(
        dir.join(format!("{legacy_id}.supervisor.pid")),
        format!("{}\n", incumbent.id()),
    )
    .expect("seed supervisor pid");
    let source_norm = source.path().canonicalize().expect("canonicalize source");
    let state = format!(
        "{{\"schema_version\":1,\"instance_id\":\"{legacy_id}\",\
         \"mountpoint\":\"{mp}\",\"source\":\"{src}\",\
         \"worker_program\":\"skillfs\",\"worker_args\":[],\
         \"desired_state\":\"mounted\"}}",
        mp = normalized.display(),
        src = source_norm.display(),
    );
    std::fs::write(dir.join(format!("{legacy_id}.state.json")), state).expect("seed managed state");

    // Reap the stand-in the moment it exits (a helper thread): a zombie
    // still answers kill(pid, 0), so the CLI's liveness wait would
    // otherwise sit out its full stop timeout on an already-dead process.
    let incumbent_exited = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let reaped = std::sync::Arc::clone(&incumbent_exited);
    std::thread::spawn(move || {
        for _ in 0..1200 {
            match incumbent.try_wait() {
                Ok(Some(_)) => {
                    reaped.store(true, std::sync::atomic::Ordering::SeqCst);
                    return;
                }
                Ok(None) => {}
                Err(_) => return,
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    });

    let out = Command::new(bin_path())
        .args(["stop", mount.to_str().unwrap()])
        .env("XDG_RUNTIME_DIR", runtime.path())
        .output()
        .expect("invoke skillfs stop");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    // Give the reaper a bounded window to observe the exit the CLI's stop
    // signals caused, and capture that observation BEFORE any leak cleanup.
    // The fallback below exists only so a failing run does not leak the
    // stand-in; if it could run before the capture it would satisfy the
    // very contract under test — a stop that reports success while the
    // incumbent survives (reproduced by LD_PRELOADing a kill() shim that
    // suppresses the CLI's SIGTERM/SIGKILL).
    for _ in 0..100 {
        if incumbent_exited.load(std::sync::atomic::Ordering::SeqCst) {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let stop_terminated_incumbent = incumbent_exited.load(std::sync::atomic::Ordering::SeqCst);

    // Leak cleanup only, strictly after the observation is captured: if
    // the CLI's stop missed the stand-in, terminate it directly so a
    // failing run leaks nothing; the reaper observes the exit.
    if !stop_terminated_incumbent {
        let _ = Command::new("sh")
            .arg("-c")
            .arg(format!("kill -TERM {incumbent_pid_num}"))
            .output();
        for _ in 0..100 {
            if incumbent_exited.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    assert!(out.status.success(), "stop must succeed, got: {combined}");
    assert!(
        combined.contains("stopped managed mount"),
        "stop must resolve the legacy state/pid files, got: {combined}"
    );
    assert!(
        stop_terminated_incumbent,
        "stop must terminate the incumbent supervisor, not just report success"
    );
    assert!(
        !dir.join(format!("{legacy_id}.state.json")).exists(),
        "stop must remove the legacy state file"
    );
    assert!(
        !dir.join(format!("{legacy_id}.supervisor.pid")).exists(),
        "stop must remove the legacy supervisor pid file"
    );
}

#[test]
fn managed_mount_reuses_a_legacy_underscore_instance() {
    if !fuse_available() {
        eprintln!("SKIP: FUSE unavailable; cannot bring up a managed mount");
        return;
    }
    // Upgrade regression (review), live half: a real supervisor serving
    // under the legacy `_skills-<hash>` id must stay the single owner of
    // its mountpoint. The id derivation must keep the underscore prefix,
    // so a repeated managed request resolves the incumbent's pid/state
    // files ("already active") instead of unmounting it and racing a
    // competing supervisor, and `stop` must tear the incumbent down.
    let source = empty_source();
    let parent = tempfile::tempdir().expect("mount parent tempdir");
    let mount = parent.path().join("_skills");
    let runtime = tempfile::tempdir().expect("runtime tempdir");

    let first = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.to_str().unwrap(),
            "--managed",
        ])
        .env("XDG_RUNTIME_DIR", runtime.path())
        .output()
        .expect("invoke skillfs");

    let normalized = mount.canonicalize().expect("canonicalize mountpoint");
    let legacy_id = legacy_instance_id(&normalized);
    let supervisor_pid = runtime
        .path()
        .join(format!("skillfs/{legacy_id}.supervisor.pid"));

    let second = Command::new(bin_path())
        .args([
            "mount",
            source.path().to_str().unwrap(),
            mount.to_str().unwrap(),
            "--managed",
        ])
        .env("XDG_RUNTIME_DIR", runtime.path())
        .output()
        .expect("invoke skillfs again");

    // Capture the incumbent's pid file before stop removes it.
    let incumbent_pid = std::fs::read_to_string(&supervisor_pid).ok();

    // Stop before asserting, so a failure does not leak a detached
    // supervisor and a live FUSE mount into later tests.
    let stop = Command::new(bin_path())
        .args(["stop", mount.to_str().unwrap()])
        .env("XDG_RUNTIME_DIR", runtime.path())
        .output()
        .expect("invoke skillfs stop");

    // RED-shape leak guard: if `stop` missed the legacy instance (the
    // regression this test pins), tear the incumbent down directly.
    if let Some(raw) = incumbent_pid.as_deref() {
        if let Ok(pid) = raw.trim().parse::<i32>() {
            let _ = Command::new("sh")
                .arg("-c")
                .arg(format!("kill -TERM {pid}"))
                .output();
            for _ in 0..100 {
                if !is_mounted(&mount) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
    force_unmount(&mount);

    let first_out = format!(
        "{}{}",
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&first.stderr),
    );
    let second_out = format!(
        "{}{}",
        String::from_utf8_lossy(&second.stdout),
        String::from_utf8_lossy(&second.stderr),
    );
    let stop_out = format!(
        "{}{}",
        String::from_utf8_lossy(&stop.stdout),
        String::from_utf8_lossy(&stop.stderr),
    );

    assert!(
        first.status.success(),
        "an underscore-prefixed mountpoint must be servable, got: {first_out}"
    );
    assert!(
        first_out.contains("managed mount ready"),
        "expected the ready line, got: {first_out}"
    );
    assert!(
        incumbent_pid.is_some(),
        "the instance must keep its legacy underscore-prefixed id \
         ({legacy_id}.supervisor.pid), runtime files: {}",
        list_runtime_files(&runtime.path().join("skillfs")),
    );
    assert!(
        second.status.success(),
        "a repeated managed request must succeed, got: {second_out}"
    );
    assert!(
        second_out.contains("already active"),
        "a repeated managed request must reuse the incumbent supervisor \
         instead of racing a competing one, got: {second_out}"
    );
    assert!(
        stop.status.success(),
        "stop must clean up the instance, got: {stop_out}"
    );
    assert!(
        stop_out.contains("stopped managed mount"),
        "stop must resolve the legacy state/pid files, got: {stop_out}"
    );
    assert!(
        !supervisor_pid.exists(),
        "stop must remove the legacy supervisor pid file"
    );
}

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
    // 30 s readiness budget (50 ms x 600), the same cap the in-process
    // polling fixtures use: the spawned CLI runs the environment-profile
    // probe loop before its FUSE session comes up, which alone can take
    // seconds on a loaded host - the previous 5 s budget raced it and the
    // assertion reported the startup log instead of the aggregated view.
    for _ in 0..600 {
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

#[test]
#[ignore = "requires CAP_SYS_ADMIN and FUSE; run in a disposable Linux container"]
fn managed_stop_preserves_backing_root_when_fuse_is_busy() {
    assert!(fuse_available(), "this regression requires FUSE");

    struct ManagedMount {
        root: tempfile::TempDir,
        mount: std::path::PathBuf,
        backing: std::path::PathBuf,
        replacement_backing: std::path::PathBuf,
        holder: Option<Child>,
        stopper: Option<Child>,
    }
    impl Drop for ManagedMount {
        fn drop(&mut self) {
            if let Some(mut holder) = self.holder.take() {
                let _ = holder.kill();
                let _ = holder.wait();
            }
            if let Some(mut stopper) = self.stopper.take() {
                let _ = stopper.kill();
                let _ = stopper.wait();
            }
            let _ = Command::new(bin_path())
                .arg("stop")
                .arg(&self.mount)
                .env("XDG_RUNTIME_DIR", self.root.path().join("runtime"))
                .output();
            force_unmount(&self.mount);
            for backing in [&self.backing, &self.replacement_backing] {
                if is_mounted(backing) {
                    let _ = Command::new("umount").arg(backing).output();
                }
                assert!(!is_mounted(backing), "test backing bind leaked");
            }
            assert!(!is_mounted(&self.mount), "test FUSE mount leaked");
        }
    }

    // Delay release past clear_mount's old three-second retry window.
    for (busy, release_after_failure) in [
        (false, None),
        (true, Some(Duration::ZERO)),
        (true, Some(Duration::from_secs(4))),
        (true, None),
    ] {
        for operation in ["stop", "orphan", "supervisor"] {
            let root = non_tmp_dir();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
            let source = root.path().join("source");
            let mount = root.path().join("mount");
            let backing = root.path().join("backing");
            std::fs::create_dir_all(source.join("demo")).unwrap();
            std::fs::create_dir(root.path().join("runtime")).unwrap();
            let skill = "---\nname: demo\ndescription: stop probe\n---\nbody\n";
            std::fs::write(source.join("demo/SKILL.md"), skill).unwrap();
            let mut instance = ManagedMount {
                root,
                mount,
                backing,
                replacement_backing: source.parent().unwrap().join("replacement-backing"),
                holder: None,
                stopper: None,
            };
            let output = Command::new(bin_path())
                .arg("mount")
                .arg(&source)
                .arg("mount")
                .current_dir(instance.root.path())
                .args([
                    "--security",
                    "--activation-mode",
                    "file",
                    "--skill-layout",
                    "flat",
                ])
                .arg("--ledger-backing-root")
                .arg(&instance.backing)
                .arg("--managed")
                .env("XDG_RUNTIME_DIR", instance.root.path().join("runtime"))
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(is_mounted(&instance.mount));
            assert!(is_mounted(&instance.backing));

            if busy {
                // Command::spawn returns only after the child's chdir succeeds,
                // so the mount is already held when stop starts (no timing sleep).
                instance.holder = Some(
                    Command::new("sleep")
                        .arg("60")
                        .current_dir(instance.mount.join("skills"))
                        .spawn()
                        .unwrap(),
                );
            }
            let runtime = instance.root.path().join("runtime/skillfs");
            let supervisor_file = std::fs::read_dir(&runtime)
                .unwrap()
                .map(Result::unwrap)
                .find(|e| e.file_name().to_string_lossy().ends_with(".supervisor.pid"))
                .unwrap()
                .path();
            let supervisor = std::fs::read_to_string(&supervisor_file).unwrap();
            if operation == "orphan" {
                assert!(
                    Command::new("kill")
                        .args(["-KILL", supervisor.trim()])
                        .status()
                        .unwrap()
                        .success()
                );
                // pid_alive also sees zombies; wait for the container init to
                // reap this supervisor before requesting orphan replacement.
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                while std::path::Path::new(&format!("/proc/{}", supervisor.trim())).exists() {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "supervisor did not die"
                    );
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
            let mut command = Command::new(bin_path());
            if operation == "orphan" {
                command
                    .arg("mount")
                    .arg(&source)
                    .arg(&instance.mount)
                    .args([
                        "--security",
                        "--activation-mode",
                        "file",
                        "--skill-layout",
                        "flat",
                    ])
                    .arg("--ledger-backing-root")
                    .arg(&instance.replacement_backing)
                    .arg("--managed");
            } else if operation == "supervisor" {
                command = Command::new("kill");
                command.args(["-TERM", supervisor.trim()]);
            } else {
                command.arg("stop").arg(&instance.mount);
            }
            let started = std::time::Instant::now();
            instance.stopper = Some(
                command
                    .env("XDG_RUNTIME_DIR", instance.root.path().join("runtime"))
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .unwrap(),
            );
            if let Some(delay) = release_after_failure {
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                loop {
                    let logs: String =
                        std::fs::read_dir(instance.root.path().join("runtime/skillfs"))
                            .unwrap()
                            .filter_map(Result::ok)
                            .filter(|e| e.file_name().to_string_lossy().ends_with(".worker.log"))
                            .filter_map(|e| std::fs::read_to_string(e.path()).ok())
                            .collect();
                    // The old early-Drop warning and the retained worker's failed
                    // unmount warning both prove the first unmount was attempted.
                    if logs.contains("leaving backing root in place: FUSE mount still active")
                        || logs.contains("signal-triggered FUSE unmount failed")
                    {
                        assert!(is_mounted(&instance.mount));
                        assert!(is_mounted(&instance.backing));
                        break;
                    }
                    assert!(
                        std::time::Instant::now() < deadline,
                        "no failed unmount observed: {logs}"
                    );
                    std::thread::sleep(Duration::from_millis(20));
                }
                if let Some(remaining) = delay.checked_sub(started.elapsed()) {
                    std::thread::sleep(remaining);
                }
                assert!(is_mounted(&instance.mount));
                assert!(is_mounted(&instance.backing));
                let mut holder = instance.holder.take().unwrap();
                holder.kill().unwrap();
                holder.wait().unwrap();
            }
            let output = instance.stopper.take().unwrap().wait_with_output().unwrap();
            if operation == "supervisor" {
                let deadline = std::time::Instant::now() + Duration::from_secs(15);
                while supervisor_file.exists() {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "supervisor shutdown timed out"
                    );
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
            if busy && release_after_failure.is_none() {
                if operation != "supervisor" {
                    assert_eq!(output.status.code(), Some(1));
                    assert!(String::from_utf8_lossy(&output.stderr).contains("failed to unmount"));
                }
                assert!(is_mounted(&instance.mount), "busy FUSE mount must remain");
                assert!(
                    is_mounted(&instance.backing),
                    "live backing alias must remain"
                );
                assert_eq!(
                    std::fs::read_to_string(instance.backing.join("demo/SKILL.md")).unwrap(),
                    skill
                );
            } else {
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                assert_eq!(is_mounted(&instance.mount), operation == "orphan");
                assert_eq!(
                    is_mounted(&instance.replacement_backing),
                    operation == "orphan"
                );
                assert!(!is_mounted(&instance.backing));
                assert!(
                    !instance.backing.exists(),
                    "successful stop must remove created backing"
                );
            }
            assert_eq!(
                std::fs::read_to_string(source.join("demo/SKILL.md")).unwrap(),
                skill
            );
            drop(instance);
        }
    }
}
