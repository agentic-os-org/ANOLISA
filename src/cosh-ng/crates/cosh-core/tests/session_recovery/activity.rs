use super::*;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::process::Child;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long `receive` waits for a message the test knows must arrive.
///
/// The five-second bound flaked on a loaded CI runner
/// (`quota::clear_and_commit_share_the_snapshot_lock_and_freed_space_is_retried`,
/// issue #10010): the quota fixture stages ~100 MiB of session files and
/// coordinates three processes, so scheduling and I/O delay on a busy runner
/// can exceed five seconds without any lock or progress failure. The helper
/// only ever waits for messages the protocol guarantees, so a longer bound
/// costs nothing on the happy path and turns a load flake back into either a
/// pass or a genuine failure with diagnostics.
const RECEIVE_DEADLINE: Duration = Duration::from_secs(15);

/// Bounded tail of the child's stderr, kept for the receive-timeout diagnostic.
///
/// `start` used to send the child's stderr to `/dev/null`, so a runtime that
/// stalled or crashed took its reason to the grave: the deadline panic could
/// not distinguish an initialization, first-result, or retry-result timeout,
/// nor a crash from a lock wait. A small bounded tail restores that evidence
/// without changing any test's semantics.
struct StderrTail(Arc<Mutex<String>>);

impl StderrTail {
    fn capture(child: &mut Child) -> Self {
        let stderr = child.stderr.take().expect("child stderr is piped");
        let buffer = Arc::new(Mutex::new(String::new()));
        let sink = Arc::clone(&buffer);
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines() {
                let Ok(line) = line else { break };
                let mut tail = sink.lock().unwrap();
                tail.push_str(&line);
                tail.push('\n');
                // Keep the tail bounded: a chatty runtime must not grow the
                // buffer without limit over a long test.
                let excess = tail.len().saturating_sub(4096);
                if excess > 0 {
                    tail.drain(..excess);
                }
            }
        });
        Self(buffer)
    }

    fn text(&self) -> String {
        self.0.lock().unwrap().clone()
    }
}

pub(super) struct RuntimeProcess {
    pub(super) child: Child,
    pub(super) lines: Receiver<Value>,
    stderr: StderrTail,
    /// Counts of the message types already read from this process. The
    /// timeout diagnostic prints this history, so a stall can be located to
    /// the phase that produced (or failed to produce) the last message.
    consumed: Arc<Mutex<BTreeMap<String, usize>>>,
}

impl RuntimeProcess {
    pub(super) fn start(home: &Path, workspace: &Path, args: &[&str]) -> Self {
        let mut child = isolated_command(home)
            .args(["--headless", "--workspace"])
            .arg(workspace)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start runtime");
        writeln!(child.stdin.as_mut().unwrap(), "{}", json!({"type":"control_request", "request_id":"init", "request":{"subtype":"initialize"}})).expect("initialize protocol");
        Self::from_child(child)
    }

    pub(super) fn from_child(mut child: Child) -> Self {
        let stderr = StderrTail::capture(&mut child);
        let stdout = child.stdout.take().unwrap();
        let (sender, lines) = mpsc::channel();
        let consumed = Arc::new(Mutex::new(BTreeMap::new()));
        let consumed_events = Arc::clone(&consumed);
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let value = match serde_json::from_str::<Value>(&line.unwrap()) {
                    Ok(value) => value,
                    Err(error) => panic!("runtime emitted invalid JSONL: {error}"),
                };
                if let Some(kind) = value["type"].as_str() {
                    *consumed_events
                        .lock()
                        .unwrap()
                        .entry(kind.to_owned())
                        .or_default() += 1;
                }
                if sender.send(value).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            lines,
            stderr,
            consumed,
        }
    }

    pub(super) fn receive(&self, kind: &str) -> Value {
        let started = Instant::now();
        loop {
            let deadline = started + RECEIVE_DEADLINE;
            let value = self
                .lines
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap_or_else(|_| {
                    panic!(
                        "runtime did not send a {kind:?} message within {}s\n\
                         awaited from: {:?} ago\n\
                         messages consumed so far: {:?}\n\
                         child stderr tail:\n{}",
                        RECEIVE_DEADLINE.as_secs(),
                        started.elapsed(),
                        self.consumed.lock().unwrap(),
                        self.stderr.text(),
                    )
                });
            if value["type"] == kind {
                return value;
            }
        }
    }

    fn assert_no_result(&self, duration: Duration) {
        let deadline = Instant::now() + duration;
        while let Ok(value) = self
            .lines
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            assert_ne!(
                value["type"], "result",
                "commit completed while budget was held: {value}"
            );
        }
    }

    pub(super) fn turn(&mut self, prompt: &str) -> Value {
        writeln!(
            self.child.stdin.as_mut().unwrap(),
            "{}",
            json!({"type":"user", "message":{"role":"user", "content":prompt}})
        )
        .expect("send turn");
        self.receive("result")
    }
}

impl Drop for RuntimeProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub(super) fn scope_directory(root: &Path) -> PathBuf {
    fs::read_dir(root)
        .unwrap()
        .filter_map(Result::ok)
        .find(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.len() == 24
                && name.chars().all(|character| character.is_ascii_hexdigit())
                && entry.file_type().is_ok_and(|kind| kind.is_dir())
        })
        .expect("scope directory")
        .path()
}

#[test]
fn second_resume_is_bounded_and_does_not_change_the_active_envelope() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let workspace = temp.path().join("workspace");
    let root = temp.path().join("sessions");
    fs::create_dir_all(&workspace).unwrap();
    configure(&home, &root);
    let mut active = RuntimeProcess::start(&home, &workspace, &[]);
    let result = active.turn("first persisted turn");
    assert_eq!(result["is_error"], false);
    let id = result["session_id"].as_str().unwrap();
    let path = scope_directory(&root).join(format!("{id}.json"));
    let before = fs::read(&path).unwrap();
    let contender = RuntimeProcess::start(&home, &workspace, &["--resume", id]);
    let failure = contender.receive("result");
    assert_eq!(failure["session_error_code"], "conflict");
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(
        active.turn("first runtime can still commit")["is_error"],
        false
    );
    assert_ne!(fs::read(&path).unwrap(), before);
}

#[cfg(target_os = "linux")]
#[test]
fn first_commit_holds_the_live_lease_while_waiting_for_the_user_budget_lock() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let workspace = temp.path().join("workspace");
    let root = temp.path().join("sessions");
    fs::create_dir_all(&workspace).unwrap();
    configure(&home, &root);
    json_lines(&run_prompt(&home, &workspace, &["seed scope"]));
    let shim = temp.path().join("budget-close.so");
    let status = Command::new("cc")
        .args(["-shared", "-fPIC"])
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/session_recovery/budget_close.c"))
        .args(["-ldl", "-o"])
        .arg(&shim)
        .status()
        .unwrap();
    assert!(status.success(), "compile deterministic close barrier");
    let ready = temp.path().join("ready");
    let release = temp.path().join("release");
    let mut child = isolated_command(&home)
        .args(["--headless", "--workspace"])
        .arg(&workspace)
        .env("LD_PRELOAD", &shim)
        .env("COSH_TEST_BUDGET_READY", &ready)
        .env("COSH_TEST_BUDGET_RELEASE", &release)
        .env(
            "COSH_TEST_BUDGET_LOCK",
            home.join(".copilot-shell/cosh-core/sessions/.budget/.budget.lock"),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(
        child.stdin.as_mut().unwrap(),
        "{}",
        json!({"type":"control_request", "request_id":"init", "request":{"subtype":"initialize"}})
    )
    .unwrap();
    let mut active = RuntimeProcess::from_child(child);
    writeln!(
        active.child.stdin.as_mut().unwrap(),
        "{}",
        json!({"type":"user", "message":{"role":"user", "content":"first commit"}})
    )
    .unwrap();
    let init = active.receive("system");
    let id = init["session_id"].as_str().unwrap();
    let scope = scope_directory(&root);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready.exists() {
        assert!(
            Instant::now() < deadline,
            "lease-claim budget guard must reach close barrier"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !scope.join(format!("{id}.json")).exists(),
        "barrier must precede the first envelope write"
    );
    let lease = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(scope.join(format!(".{id}.live")))
        .expect("lease exists before first write");
    assert_eq!(
        flock(&lease, FlockOperation::NonBlockingLockExclusive),
        Err(rustix::io::Errno::WOULDBLOCK)
    );
    let budget = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(home.join(".copilot-shell/cosh-core/sessions/.budget/.budget.lock"))
        .unwrap();
    flock(&budget, FlockOperation::LockExclusive).unwrap();
    fs::write(&release, b"release").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !scope.join(format!(".{id}.lock")).exists() {
        assert!(
            Instant::now() < deadline,
            "commit must reach generation lock"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!scope.join(format!("{id}.json")).exists());
    active.assert_no_result(Duration::from_millis(100));
    flock(&budget, FlockOperation::Unlock).unwrap();
    assert_eq!(active.receive("result")["is_error"], false);
    assert!(scope.join(format!("{id}.json")).exists());
}

#[test]
fn active_process_survives_expiration_then_its_exit_allows_reclamation() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let workspace = temp.path().join("workspace");
    let root = temp.path().join("sessions");
    fs::create_dir_all(&workspace).unwrap();
    configure(&home, &root);
    let mut active = RuntimeProcess::start(&home, &workspace, &[]);
    let result = active.turn("active history");
    let id = result["session_id"].as_str().unwrap();
    let idle = json_lines(&run_prompt(&home, &workspace, &["ordinary idle history"]));
    let idle_id = idle.iter().find(|line| line["type"] == "result").unwrap()["session_id"]
        .as_str()
        .unwrap();
    let scope = scope_directory(&root);
    let expire = |session_id: &str| {
        let path = scope.join(format!("{session_id}.json"));
        let mut envelope: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        envelope["updated_at_ms"] = json!(1);
        fs::write(path, serde_json::to_vec(&envelope).unwrap()).unwrap();
    };
    expire(id);
    expire(idle_id);
    json_lines(&run_prompt(&home, &workspace, &["concurrent sweep"]));
    assert!(scope.join(format!("{id}.json")).exists());
    assert!(!scope.join(format!("{idle_id}.json")).exists());
    assert_eq!(
        active.turn("still commits after seven days")["is_error"],
        false
    );
    expire(id);
    drop(active);
    json_lines(&run_prompt(
        &home,
        &workspace,
        &["sweep after process exit"],
    ));
    assert!(!scope.join(format!("{id}.json")).exists());
}

#[test]
fn explicit_clear_is_not_revived_by_an_active_runtime() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let workspace = temp.path().join("workspace");
    let root = temp.path().join("sessions");
    fs::create_dir_all(&workspace).unwrap();
    configure(&home, &root);
    let mut active = RuntimeProcess::start(&home, &workspace, &[]);
    let result = active.turn("before explicit clear");
    let id = result["session_id"].as_str().unwrap();
    let path = scope_directory(&root).join(format!("{id}.json"));
    let clear = session_control(
        &home,
        json!({"action":"clear", "workspace_scope":workspace, "session_ids":[id], "protected_session_ids":[]}),
    );
    assert_eq!(clear["ok"], true);
    assert!(!path.exists());
    let failure = active.turn("cannot recreate cleared history");
    assert_eq!(failure["session_error_code"], "conflict");
    assert!(!path.exists());
    let scope = scope_directory(&root);
    assert!(
        !scope.join(format!(".{id}.lock")).exists(),
        "cleared history must not leave a new generation control"
    );
    assert!(!scope.join(format!(".{id}.live")).exists());
    let resumer = RuntimeProcess::start(&home, &workspace, &["--resume", id]);
    assert_eq!(resumer.receive("result")["session_error_code"], "not_found");
    assert!(!path.exists());
}

#[test]
fn two_process_writers_share_one_remaining_envelope_of_budget() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let workspace = temp.path().join("workspace");
    let root = temp.path().join("sessions");
    fs::create_dir_all(&workspace).unwrap();
    configure(&home, &root);
    json_lines(&run_prompt(&home, &workspace, &["writer"]));
    let scope = scope_directory(&root);
    let seed = fs::read_dir(&scope)
        .unwrap()
        .filter_map(Result::ok)
        .find(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "json")
        })
        .unwrap()
        .path();
    let bytes = fs::read(&seed).unwrap();
    let template: Value = serde_json::from_slice(&bytes).unwrap();
    fs::remove_file(seed).unwrap();
    let filler_bytes = 100 * 1024 * 1024 - bytes.len() - 64;
    let mut leases = Vec::new();
    for index in 0..4 {
        let id = format!("{index:08x}-1111-4111-8111-111111111111");
        let mut envelope = template.clone();
        envelope["session_id"] = json!(id);
        envelope["padding"] = json!("");
        let overhead = serde_json::to_vec(&envelope).unwrap().len();
        let size = filler_bytes / 4 + usize::from(index < filler_bytes % 4);
        envelope["padding"] = json!("x".repeat(size - overhead));
        fs::write(
            scope.join(format!("{id}.json")),
            serde_json::to_vec(&envelope).unwrap(),
        )
        .unwrap();
        let lease = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(scope.join(format!(".{id}.live")))
            .unwrap();
        flock(&lease, FlockOperation::LockExclusive).unwrap();
        leases.push(lease);
    }
    let budget = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(home.join(".copilot-shell/cosh-core/sessions/.budget/.budget.lock"))
        .unwrap();
    flock(&budget, FlockOperation::LockExclusive).unwrap();
    let mut writers = (0..2)
        .map(|_| RuntimeProcess::start(&home, &workspace, &[]))
        .collect::<Vec<_>>();
    for writer in &mut writers {
        writeln!(
            writer.child.stdin.as_mut().unwrap(),
            "{}",
            json!({"type":"user", "message":{"role":"user", "content":"writer"}})
        )
        .unwrap();
        let init = writer.receive("system");
        assert!(init["session_id"].as_str().is_some());
        writer.assert_no_result(Duration::from_millis(100));
    }
    flock(&budget, FlockOperation::Unlock).unwrap();
    let outcomes = writers
        .iter()
        .map(|writer| writer.receive("result"))
        .collect::<Vec<_>>();
    assert_eq!(
        outcomes
            .iter()
            .filter(|result| result["is_error"] == false)
            .count(),
        1,
        "{outcomes:?}"
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|result| result["session_error_code"] == "budget_exhausted")
            .count(),
        1,
        "{outcomes:?}"
    );
    let accounted: u64 = fs::read_dir(scope)
        .unwrap()
        .map(|entry| entry.unwrap())
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "json")
        })
        .map(|entry| entry.metadata().unwrap().len())
        .sum();
    assert!(accounted <= 100 * 1024 * 1024, "stored {accounted} bytes");
    drop(leases);
}

#[cfg(target_os = "linux")]
#[test]
fn unreadable_scope_refuses_new_optional_persistence() {
    use std::os::unix::{fs::PermissionsExt, process::CommandExt};
    let identity = Command::new("id").arg("-u").output().unwrap();
    assert!(identity.status.success());
    if String::from_utf8(identity.stdout).unwrap().trim() == "0" {
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "activity::unreadable_scope_refuses_new_optional_persistence",
                "--nocapture",
            ])
            .uid(65534)
            .gid(65534)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let workspace = temp.path().join("workspace");
    let other_workspace = temp.path().join("other-workspace");
    let root = temp.path().join("sessions");
    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(&other_workspace).unwrap();
    configure(&home, &root);
    json_lines(&run_prompt(&home, &workspace, &["seed budget"]));
    let scope = scope_directory(&root);
    fs::set_permissions(&scope, fs::Permissions::from_mode(0o000)).unwrap();
    let output = run_prompt(&home, &other_workspace, &["refuse unmeasured persistence"]);
    fs::set_permissions(&scope, fs::Permissions::from_mode(0o700)).unwrap();
    let result = json_lines(&output)
        .into_iter()
        .find(|line| line["type"] == "result")
        .unwrap();
    assert_eq!(result["session_error_code"], "io");
    let id = result["session_id"].as_str().unwrap();
    for entry in fs::read_dir(root)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
    {
        assert!(!entry.path().join(format!("{id}.json")).exists());
        assert!(
            !entry.path().join(format!(".{id}.lock")).exists(),
            "failed first commit leaked generation control"
        );
        assert!(
            !entry.path().join(format!(".{id}.live")).exists(),
            "failed first commit leaked activity control"
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn first_commit_sync_failures_preserve_only_published_controls() {
    for phase in ["before", "after"] {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let workspace = temp.path().join("workspace");
        let root = temp.path().join("sessions");
        fs::create_dir_all(&workspace).unwrap();
        configure(&home, &root);
        json_lines(&run_prompt(&home, &workspace, &["seed scope"]));
        let scope = scope_directory(&root);
        let shim = temp.path().join("commit-sync.so");
        let status = Command::new("cc")
            .args(["-shared", "-fPIC"])
            .arg(
                Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/session_recovery/budget_close.c"),
            )
            .args(["-ldl", "-o"])
            .arg(&shim)
            .status()
            .unwrap();
        assert!(status.success());
        let mut child = isolated_command(&home)
            .args(["--headless", "--workspace"])
            .arg(&workspace)
            .env("LD_PRELOAD", &shim)
            .env("COSH_TEST_COMMIT_FSYNC_TARGET", &scope)
            .env("COSH_TEST_COMMIT_FSYNC_PHASE", phase)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        writeln!(child.stdin.as_mut().unwrap(), "{}", json!({"type":"control_request", "request_id":"init", "request":{"subtype":"initialize"}})).unwrap();
        let mut runtime = RuntimeProcess::from_child(child);
        let result = runtime.turn("first sync-failed turn");
        assert_eq!(result["session_error_code"], "io");
        let id = result["session_id"].as_str().unwrap();
        let envelope = scope.join(format!("{id}.json"));
        let generation = scope.join(format!(".{id}.lock"));
        let activity = scope.join(format!(".{id}.live"));
        if phase == "before" {
            assert!(!envelope.exists());
            assert!(
                !generation.exists(),
                "pre-rename error leaked generation control"
            );
            assert!(
                !activity.exists(),
                "pre-rename error leaked activity control"
            );
            let retry = runtime.turn("retry after sync fault");
            assert_eq!(retry["is_error"], false);
            assert_eq!(retry["session_id"], result["session_id"]);
        } else {
            assert!(envelope.exists());
            assert!(generation.exists());
            assert!(activity.exists());
        }
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&activity)
            .expect("retry must reacquire the current activity inode");
        assert_eq!(
            flock(&file, FlockOperation::NonBlockingLockExclusive),
            Err(rustix::io::Errno::WOULDBLOCK),
            "published first commit must remain actively leased"
        );
        let stored: Value = serde_json::from_slice(&fs::read(&envelope).unwrap()).unwrap();
        assert!(stored["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| message["content"] == "first sync-failed turn"));
    }
}
