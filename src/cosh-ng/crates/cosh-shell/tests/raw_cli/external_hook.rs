use super::*;
use std::sync::mpsc;

const HOOK_TIMEOUT_MS: u64 = 2_000;
const HOOK_START_TIMEOUT_SECS: u64 = 10;
const MARKER_DELAY_SECS: u64 = 5;

/// Best-effort SIGKILL of recorded PIDs and their groups so a failed
/// assertion does not leak processes into CI.
struct HookPidCleanup(Vec<i32>);

impl Drop for HookPidCleanup {
    fn drop(&mut self) {
        for pid in &self.0 {
            let _ = Command::new("sh")
                .arg("-c")
                .arg(format!("kill -9 -- -{pid} {pid} 2>/dev/null"))
                .status();
        }
    }
}

fn install_user_hook(home: &Path, name: &str, body: &str) {
    let hooks_dir = home.join(".copilot-shell/cosh/hooks");
    fs::create_dir_all(&hooks_dir).expect("create user hooks directory");
    write_executable(&hooks_dir.join(name), body);
}

fn hook_status_count(output: &str, label: &str) -> usize {
    let start = output.find(label).expect("hook status label") + label.len();
    output[start..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .expect("hook status count")
}

fn spawn_hook_session(
    home: &Path,
    command: &[u8],
) -> (thread::JoinHandle<String>, mpsc::Receiver<()>) {
    let home = home.to_path_buf();
    let command = command.to_vec();
    let (session_started_tx, session_started_rx) = mpsc::channel();
    let session = thread::spawn(move || {
        let home = home.to_string_lossy().into_owned();
        // These tests measure hook deadlines after observing the hook PID.
        // Keep startup outside the independent hook-start watchdog and away
        // from competing raw CLI sessions.
        run_raw_cli_serial_with_args_env_and_delayed_input_after_ready(
            "fake",
            &[],
            &[("HOME", home.as_str())],
            vec![
                (command, Duration::ZERO),
                (b"exit\n".to_vec(), Duration::from_millis(2_500)),
            ],
            session_started_tx,
        )
    });
    (session, session_started_rx)
}

fn large_hook_input_command() -> Vec<u8> {
    // Produce more than the maximum Linux pipe capacity using lines below
    // the redaction line limit. The short source avoids spending the hook
    // startup watchdog on echoing and parsing a 1 MiB interactive command.
    b"printf '%32767s\\n' {1..40}\n".to_vec()
}

#[test]
fn raw_cli_external_hook_executes_pinned_file_after_path_replacement() {
    let home = temp_shell_home("external-hook-pinned-exec");
    let hook_path = home.join(".copilot-shell/cosh/hooks/pinned.sh");
    let outside_path = home.join("outside.sh");
    let original_marker = home.join("original.marker");
    let outside_marker = home.join("outside.marker");

    install_user_hook(
        &home,
        "pinned.sh",
        &format!(
            "#!/bin/sh\n# cosh-hook: pinned-hook\n# match-commands: echo\n\
             : > '{}'\nprintf '%s\\n' '{{\"hook_id\":\"pinned-hook\",\"severity\":\"warning\",\"title\":\"PINNED ORIGINAL HOOK\",\"description\":\"original\",\"suggestion\":\"none\"}}'\n",
            original_marker.display()
        ),
    );
    write_executable(
        &outside_path,
        &format!(
            "#!/bin/sh\n: > '{}'\nprintf '%s\\n' '{{\"hook_id\":\"outside\",\"severity\":\"warning\",\"title\":\"REPLACED OUTSIDE HOOK\",\"description\":\"outside\",\"suggestion\":\"none\"}}'\n",
            outside_marker.display()
        ),
    );

    // The engine loads user hooks before launching bash. Replacing the path
    // from .bashrc therefore exercises the load-to-exec window end to end.
    fs::write(
        home.join(".bashrc"),
        format!(
            "rm -f -- '{}'\nln -s -- '{}' '{}'\n",
            hook_path.display(),
            outside_path.display(),
            hook_path.display()
        ),
    )
    .unwrap();

    let home_str = home.to_string_lossy().into_owned();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &["--shell", "bash"],
        &[("HOME", home_str.as_str()), ("COSH_SHELL_ISOLATED", "0")],
        vec![
            (b"echo pinned-hook-check\n".to_vec(), Duration::ZERO),
            (b"exit\n".to_vec(), Duration::from_millis(500)),
        ],
    );

    assert!(output.contains("PINNED ORIGINAL HOOK"), "{output}");
    assert!(!output.contains("REPLACED OUTSIDE HOOK"), "{output}");
    assert!(original_marker.exists(), "original pinned hook did not run");
    assert!(
        !outside_marker.exists(),
        "replacement hook unexpectedly ran"
    );
    let _ = fs::remove_dir_all(&home);
}

#[cfg(unix)]
#[test]
fn raw_cli_project_hook_rejects_symlinked_cosh_directory() {
    use std::os::unix::fs::symlink;

    let home = temp_shell_home("project-hook-symlinked-cosh");
    let real_project = home.join("project-real");
    let link_project = home.join("project-link");
    let outside_cosh = home.join("outside-cosh");
    let outside_hooks = outside_cosh.join("hooks");
    let real_marker = home.join("real-hook.marker");
    let outside_marker = home.join("outside-hook.marker");
    fs::create_dir_all(real_project.join(".cosh/hooks")).unwrap();
    fs::create_dir_all(&link_project).unwrap();
    fs::create_dir_all(&outside_hooks).unwrap();

    let hook_body = |id: &str, title: &str, marker: &Path| {
        format!(
            "#!/bin/sh\n# cosh-hook: {id}\n# match-commands: echo\n\
             : > '{}'\nprintf '%s\\n' '{{\"hook_id\":\"{id}\",\"severity\":\"warning\",\"title\":\"{title}\",\"description\":\"outside\",\"suggestion\":\"none\"}}'\n",
            marker.display()
        )
    };
    write_executable(
        &real_project.join(".cosh/hooks/real.sh"),
        &hook_body("real-project", "REAL PROJECT HOOK", &real_marker),
    );
    write_executable(
        &outside_hooks.join("outside.sh"),
        &hook_body("outside-project", "OUTSIDE PROJECT HOOK", &outside_marker),
    );
    symlink(&outside_cosh, link_project.join(".cosh")).unwrap();

    // Trust comes from an administrator policy root, the one source that needs no
    // content fingerprint. That is deliberate: this case must reach the loader, so
    // what refuses the hook is the loader's O_NOFOLLOW component check and not the
    // trust decision. A bare-path store entry no longer grants trust, which would
    // let the case pass without loading anything and stop covering the boundary.
    write_cosh_config(
        &home,
        &format!(
            "[shell]\ntrusted_project_roots = [\"{}\", \"{}\"]\n",
            real_project.display(),
            link_project.display()
        ),
    );
    let home_str = home.to_string_lossy().into_owned();

    // Positive control: with a real `.cosh/hooks`, the same policy-root trust does
    // execute the hook. Without this, the refusal below would also hold in a build
    // where project hooks cannot load at all.
    let real_output = run_raw_cli_with_args_env_current_dir_and_delayed_input(
        "fake",
        &[],
        &[("HOME", home_str.as_str())],
        &real_project,
        vec![
            (b"echo project-real-check\n".to_vec(), Duration::ZERO),
            (b"exit\n".to_vec(), Duration::from_millis(500)),
        ],
    );
    assert!(
        real_output.contains("REAL PROJECT HOOK"),
        "policy-root trust must let a real project hook run, otherwise the \
         symlink refusal below proves nothing\n{real_output}"
    );
    assert!(real_marker.exists(), "the real project hook did not run");

    let output = run_raw_cli_with_args_env_current_dir_and_delayed_input(
        "fake",
        &[],
        &[("HOME", home_str.as_str())],
        &link_project,
        vec![
            (b"echo project-symlink-check\n".to_vec(), Duration::ZERO),
            (b"exit\n".to_vec(), Duration::from_millis(500)),
        ],
    );

    assert!(!output.contains("OUTSIDE PROJECT HOOK"), "{output}");
    assert!(
        !outside_marker.exists(),
        "symlinked project hook unexpectedly ran"
    );
    let _ = fs::remove_dir_all(&home);
}

/// A project hook body that touches `marker` and reports itself, in the shape
/// the external-hook loader expects.
fn project_hook_body(hook_id: &str, marker: &Path) -> String {
    format!(
        "#!/bin/sh\n# cosh-hook: {hook_id}\n# match-commands: echo\n\
         : > '{}'\nprintf '%s\\n' '{{\"hook_id\":\"{hook_id}\",\"severity\":\"warning\",\
         \"title\":\"{hook_id} ran\",\"description\":\"project\",\"suggestion\":\"none\"}}'\n",
        marker.display()
    )
}

/// D01, end to end: consent binds the approved *content*, not the path. The
/// decision-level halves of this live in `src/hooks/loading.rs`; this is the
/// part that needs a real shell, because only execution proves a withheld hook
/// really did not run.
#[cfg(unix)]
#[test]
fn raw_cli_project_hook_content_change_is_withheld_until_reconsent() {
    let home = temp_shell_home("project-hook-content-change");
    let project = home.join("project");
    let hooks_dir = project.join(".cosh/hooks");
    let store = home.join("trusted-project-hooks");
    let marker_v1 = home.join("v1.marker");
    let marker_v2 = home.join("v2.marker");
    fs::create_dir_all(&hooks_dir).unwrap();
    let hook = hooks_dir.join("project.sh");
    let home_str = home.to_string_lossy().into_owned();
    let store_str = store.to_string_lossy().into_owned();
    let envs = [
        ("HOME", home_str.as_str()),
        ("COSH_SHELL_PROJECT_TRUST_STORE", store_str.as_str()),
    ];

    write_executable(&hook, &project_hook_body("v1", &marker_v1));
    let consented = run_raw_cli_with_args_env_current_dir_and_delayed_input(
        "fake",
        &[],
        &envs,
        &project,
        vec![
            (b"/hooks trust-project\n".to_vec(), Duration::ZERO),
            (b"echo consent-run\n".to_vec(), Duration::from_millis(2_500)),
            (b"exit\n".to_vec(), Duration::from_millis(5_000)),
        ],
    );
    assert!(
        marker_v1.exists(),
        "positive control: the approved body must run once consent is recorded\n{consented}"
    );
    assert!(store.exists(), "consent must be persisted\n{consented}");

    // The project owner replaces the body. Path and store line are unchanged.
    write_executable(&hook, &project_hook_body("v2", &marker_v2));

    let withheld = run_raw_cli_with_args_env_current_dir_and_delayed_input(
        "fake",
        &[],
        &envs,
        &project,
        vec![
            (b"echo changed-run\n".to_vec(), Duration::ZERO),
            (b"exit\n".to_vec(), Duration::from_millis(2_500)),
        ],
    );
    assert!(
        !marker_v2.exists(),
        "D01: a modified project hook ran without re-consent\n{withheld}"
    );

    let reconsented = run_raw_cli_with_args_env_current_dir_and_delayed_input(
        "fake",
        &[],
        &envs,
        &project,
        vec![
            (b"/hooks trust-project\n".to_vec(), Duration::ZERO),
            (
                b"echo reconsent-run\n".to_vec(),
                Duration::from_millis(2_500),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(5_000)),
        ],
    );
    assert!(
        marker_v2.exists(),
        "re-consent must bind the new body and let it run\n{reconsented}"
    );

    let _ = fs::remove_dir_all(&home);
}

/// The approved bytes are frozen at load, so a project that rewrites its own hook
/// **in place** after the session pinned it cannot get the new content executed.
/// Pinning a descriptor fixes the inode, not the bytes, which is why this is a
/// separate case from the path-replacement one above.
///
/// The rewrite is timed against the hook's own log so the case cannot pass
/// vacuously: it asserts the rewrite landed strictly *between* two executions of the
/// approved body, so a build that never ran the second command, or ran both before
/// the rewrite, fails instead of looking green.
#[cfg(target_os = "linux")]
#[test]
fn raw_cli_project_hook_in_place_rewrite_does_not_execute_new_content() {
    let home = temp_shell_home("project-hook-in-place-rewrite");
    let project = home.join("project");
    let hooks_dir = project.join(".cosh/hooks");
    let store = home.join("trusted-project-hooks");
    let log = home.join("hook-runs.log");
    fs::create_dir_all(&hooks_dir).unwrap();
    let hook = hooks_dir.join("project.sh");
    // Each run appends its own version plus a nanosecond stamp from the same clock
    // the test reads, which is what makes the ordering checkable.
    let body = |version: &str| {
        format!(
            "#!/bin/sh\n# cosh-hook: {version}\n# match-commands: echo\n\
             printf '%s %s\\n' '{version}' \"$(date +%s%N)\" >> '{}'\n",
            log.display()
        )
    };
    let home_str = home.to_string_lossy().into_owned();
    let store_str = store.to_string_lossy().into_owned();
    let envs = [
        ("HOME", home_str.as_str()),
        ("COSH_SHELL_PROJECT_TRUST_STORE", store_str.as_str()),
    ];

    write_executable(&hook, &body("v1"));
    let consented = run_raw_cli_with_args_env_current_dir_and_delayed_input(
        "fake",
        &[],
        &envs,
        &project,
        vec![
            (b"/hooks trust-project\n".to_vec(), Duration::ZERO),
            (b"exit\n".to_vec(), Duration::from_millis(2_500)),
        ],
    );
    assert!(
        store.exists(),
        "consent must be persisted before the rewrite case can mean anything\n{consented}"
    );

    let rewrite_hook = hook.clone();
    let rewrite_log = log.clone();
    let rewrite_body = body("v2");
    let (rewritten_tx, rewritten_rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = wait_for_hook_runs(&rewrite_log, 1, Duration::from_secs(90));
        let at = nanos_since_epoch();
        // Truncate and rewrite the same file: the in-place swap a pinned descriptor
        // alone does not defend against.
        fs::write(&rewrite_hook, rewrite_body).expect("rewrite the hook in place");
        let _ = rewritten_tx.send(at);
    });

    let output = run_raw_cli_with_args_env_current_dir_and_delayed_input(
        "fake",
        &[],
        &envs,
        &project,
        vec![
            (b"echo first\n".to_vec(), Duration::from_millis(3_000)),
            (b"echo second\n".to_vec(), Duration::from_millis(9_000)),
            (b"exit\n".to_vec(), Duration::from_millis(13_000)),
        ],
    );
    let rewritten_at = rewritten_rx
        .recv_timeout(Duration::from_secs(120))
        .expect("the in-place rewrite must happen while the session is running");

    let runs = fs::read_to_string(&log).unwrap_or_default();
    let entries = runs
        .lines()
        .map(|line| {
            let mut fields = line.split(' ');
            (
                fields.next().unwrap_or("").to_string(),
                fields
                    .next()
                    .and_then(|stamp| stamp.parse::<u128>().ok())
                    .unwrap_or(0),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        entries.len(),
        2,
        "both commands must have reached a hook; log={runs:?}\nsession={output}"
    );
    assert_eq!(entries[0].0, "v1", "log={runs:?}");
    assert_eq!(
        entries[1].0, "v1",
        "the rewritten body executed in the session that approved the original: log={runs:?}"
    );
    assert!(
        entries[0].1 < rewritten_at && rewritten_at < entries[1].1,
        "the rewrite must land between the two runs or this case proves nothing: \
         first={} rewrite={rewritten_at} second={}",
        entries[0].1,
        entries[1].1
    );

    let _ = fs::remove_dir_all(&home);
}

fn nanos_since_epoch() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default()
}

/// Waits until `log` holds at least `lines` lines, so a caller can act at a point
/// the session has demonstrably reached rather than after a guessed delay.
fn wait_for_hook_runs(log: &Path, lines: usize, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        let count = fs::read_to_string(log)
            .map(|content| content.lines().count())
            .unwrap_or(0);
        if count >= lines {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    false
}

/// D01, end to end: a store line written before content binding existed is a
/// bare path with no fingerprint. It must not be upgraded into consent for
/// whatever happens to be on disk. The positive control — the same tree running
/// once consent carries a fingerprint — is the first phase of
/// `raw_cli_project_hook_content_change_is_withheld_until_reconsent`.
#[cfg(unix)]
#[test]
fn raw_cli_legacy_path_only_consent_does_not_execute_project_hook() {
    let home = temp_shell_home("project-hook-legacy-consent");
    let project = home.join("project");
    let hooks_dir = project.join(".cosh/hooks");
    let store = home.join("trusted-project-hooks");
    let marker = home.join("legacy.marker");
    fs::create_dir_all(&hooks_dir).unwrap();
    write_executable(
        &hooks_dir.join("project.sh"),
        &project_hook_body("legacy", &marker),
    );
    let root = project.canonicalize().unwrap();
    fs::write(
        &store,
        format!(
            "# cosh-shell trusted project hook roots\n{}\n",
            root.display()
        ),
    )
    .unwrap();
    let home_str = home.to_string_lossy().into_owned();
    let store_str = store.to_string_lossy().into_owned();

    let output = run_raw_cli_with_args_env_current_dir_and_delayed_input(
        "fake",
        &[],
        &[
            ("HOME", home_str.as_str()),
            ("COSH_SHELL_PROJECT_TRUST_STORE", store_str.as_str()),
        ],
        &project,
        vec![
            (b"echo legacy-run\n".to_vec(), Duration::ZERO),
            (b"exit\n".to_vec(), Duration::from_millis(2_500)),
        ],
    );
    assert!(
        !marker.exists(),
        "D01: a legacy path-only entry executed a project hook\n{output}"
    );

    let _ = fs::remove_dir_all(&home);
}

#[test]
fn raw_cli_untrusted_project_hook_registry_is_bounded() {
    const EXPECTED_PROJECT_HOOK_LIMIT: usize = 64;

    // This same regression is also run under `prlimit --nofile=64:64` so the
    // assertion stays valid when real descriptor headroom lowers the count.
    let home = temp_shell_home("project-hook-registry-limit");
    let project = home.join("project");
    let hooks_dir = project.join(".cosh/hooks");
    fs::create_dir_all(&hooks_dir).unwrap();
    for index in 0..EXPECTED_PROJECT_HOOK_LIMIT {
        write_executable(
            &hooks_dir.join(format!("project-{index}.sh")),
            &format!("#!/bin/sh\n# cosh-hook: project-{index}\n"),
        );
    }
    let home_str = home.to_string_lossy().into_owned();

    let output = run_raw_cli_with_args_env_current_dir_and_delayed_input(
        "fake",
        &[],
        &[("HOME", home_str.as_str())],
        &project,
        vec![
            (b"/hooks\n".to_vec(), Duration::ZERO),
            (b"exit\n".to_vec(), Duration::from_millis(500)),
        ],
    );

    let project_count = hook_status_count(&output, "project=");
    let untrusted_count = hook_status_count(&output, "untrusted=");
    assert!(project_count > 0, "{output}");
    assert!(project_count <= EXPECTED_PROJECT_HOOK_LIMIT, "{output}");
    assert_eq!(untrusted_count, project_count, "{output}");
    write_executable(
        &hooks_dir.join("overflow.sh"),
        "#!/bin/sh\n# cosh-hook: overflow\n",
    );
    let rejected = run_raw_cli_with_args_env_current_dir_and_delayed_input(
        "fake",
        &[],
        &[("HOME", home_str.as_str())],
        &project,
        vec![
            (b"/hooks\n".to_vec(), Duration::ZERO),
            (b"exit\n".to_vec(), Duration::from_millis(500)),
        ],
    );
    if !rejected.contains("No hooks registered.") {
        assert_eq!(hook_status_count(&rejected, "project="), 0, "{rejected}");
    }
    let _ = fs::remove_dir_all(&home);
}

fn wait_for_pids(path: &Path, count: usize) -> Option<Vec<i32>> {
    let deadline = std::time::Instant::now() + Duration::from_secs(HOOK_START_TIMEOUT_SECS);
    loop {
        if let Ok(text) = fs::read_to_string(path) {
            let pids: Vec<i32> = text
                .split_whitespace()
                .filter_map(|token| token.parse().ok())
                .collect();
            if pids.len() == count {
                return Some(pids);
            }
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// Whether `pid` can still execute code. Zombies and dead entries cannot
/// produce the delayed marker even if their PID table entries remain.
fn process_can_run(pid: i32) -> bool {
    let output = Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .expect("failed to run ps to check process state");
    let stat = String::from_utf8_lossy(&output.stdout);
    match stat.trim().chars().next() {
        Some('Z' | 'X') => false,
        Some(_) => true,
        None => {
            assert!(
                !output.status.success() && output.stderr.is_empty(),
                "ps failed without reporting that pid {pid} is absent: status={}, stderr={}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            );
            false
        }
    }
}

fn assert_process_gone(pid: i32, context: &str) {
    let gone = (0..125).any(|_| {
        if process_can_run(pid) {
            thread::sleep(Duration::from_millis(20));
            false
        } else {
            true
        }
    });
    assert!(gone, "process {pid} survived {context}");
}

fn assert_marker_never_written(marker: &Path, ready_at: std::time::Instant, context: &str) {
    let deadline = ready_at + Duration::from_millis(5_500);
    if std::time::Instant::now() < deadline {
        thread::sleep(deadline - std::time::Instant::now());
    }
    assert!(!marker.exists(), "grandchild survived {context}");
}

#[test]
fn raw_cli_external_hook_timeout_kills_grandchildren() {
    let home = temp_shell_home("external-hook-group-kill");
    let marker = home.join("marker");
    let pid_file = home.join("pids");
    let body = format!(
        "#!/bin/sh\n# cosh-hook: group-kill-hook\n# match-commands: echo\n# timeout: {HOOK_TIMEOUT_MS}ms\n(sleep {MARKER_DELAY_SECS}; : > '{}') &\necho $$ $! > '{}'\nsleep 30\n",
        marker.display(),
        pid_file.display()
    );
    install_user_hook(&home, "group_kill.sh", &body);

    let (session, session_started) = spawn_hook_session(&home, b"echo hook-group-kill\n");
    session_started.recv().expect("raw CLI session to start");
    let pids = wait_for_pids(&pid_file, 2);
    let ready_at = std::time::Instant::now();
    let output = session.join().expect("join raw CLI hook session");
    let pids = pids.unwrap_or_else(|| panic!("hook did not record PIDs before timeout:\n{output}"));
    let _cleanup = HookPidCleanup(pids.clone());

    for pid in pids {
        assert_process_gone(pid, "the external hook timeout");
    }
    assert_marker_never_written(&marker, ready_at, "the external hook timeout");
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn raw_cli_external_hook_ignoring_large_stdin_respects_deadline() {
    let home = temp_shell_home("external-hook-large-stdin");
    let pid_file = home.join("pid");
    let body = format!(
        "#!/bin/sh\n# cosh-hook: ignore-stdin-hook\n# match-commands: printf\n# timeout: {HOOK_TIMEOUT_MS}ms\necho $$ > '{}'\nsleep 30\n",
        pid_file.display()
    );
    install_user_hook(&home, "ignore_stdin.sh", &body);

    // Keep the serialized hook input larger than a pipe buffer without an
    // oversized output line, which bounded retention replaces fail-closed.
    let command = large_hook_input_command();
    let (session, session_started) = spawn_hook_session(&home, &command);
    session_started.recv().expect("raw CLI session to start");
    let pids = wait_for_pids(&pid_file, 1);
    let ready_at = std::time::Instant::now();
    let output = session.join().expect("join raw CLI hook session");
    let pids =
        pids.unwrap_or_else(|| panic!("hook did not record its PID before timeout:\n{output}"));
    let _cleanup = HookPidCleanup(pids.clone());

    assert!(
        ready_at.elapsed() < Duration::from_secs(4),
        "blocked stdin exceeded the hook deadline:\n{output}"
    );
    assert_process_gone(pids[0], "the external hook stdin-write timeout");
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn raw_cli_external_hook_stdout_holder_is_killed() {
    let home = temp_shell_home("external-hook-stdout-holder");
    let marker = home.join("marker");
    let pid_file = home.join("pids");
    let body = format!(
        "#!/bin/sh\n# cosh-hook: stdout-holder-hook\n# match-commands: echo\n# timeout: {HOOK_TIMEOUT_MS}ms\n(sleep {MARKER_DELAY_SECS}; : > '{}') &\necho $$ $! > '{}'\nexit 0\n",
        marker.display(),
        pid_file.display()
    );
    install_user_hook(&home, "stdout_holder.sh", &body);

    let (session, session_started) = spawn_hook_session(&home, b"echo hook-stdout-holder\n");
    session_started.recv().expect("raw CLI session to start");
    let pids = wait_for_pids(&pid_file, 2);
    let ready_at = std::time::Instant::now();
    let output = session.join().expect("join raw CLI hook session");
    let pids = pids.unwrap_or_else(|| panic!("hook did not record PIDs before timeout:\n{output}"));
    let _cleanup = HookPidCleanup(pids.clone());

    assert!(
        ready_at.elapsed() < Duration::from_secs(4),
        "stdout drain waited for the grandchild:\n{output}"
    );
    assert_process_gone(pids[1], "the external hook output-drain timeout");
    assert_marker_never_written(&marker, ready_at, "the output-drain timeout");
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn raw_cli_external_hook_stdin_holder_is_killed() {
    let home = temp_shell_home("external-hook-stdin-holder");
    let marker = home.join("marker");
    let pid_file = home.join("pids");
    let body = format!(
        "#!/bin/sh\n# cosh-hook: stdin-holder-hook\n# match-commands: printf\n# timeout: {HOOK_TIMEOUT_MS}ms\nexec 3<&0\n(sleep {MARKER_DELAY_SECS}; : > '{}') <&3 >/dev/null 2>&1 &\necho $$ $! > '{}'\nexit 0\n",
        marker.display(),
        pid_file.display()
    );
    install_user_hook(&home, "stdin_holder.sh", &body);

    let command = large_hook_input_command();
    let (session, session_started) = spawn_hook_session(&home, &command);
    session_started.recv().expect("raw CLI session to start");
    let pids = wait_for_pids(&pid_file, 2);
    let ready_at = std::time::Instant::now();
    let output = session.join().expect("join raw CLI hook session");
    let pids = pids.unwrap_or_else(|| panic!("hook did not record PIDs before timeout:\n{output}"));
    let _cleanup = HookPidCleanup(pids.clone());

    assert!(
        ready_at.elapsed() < Duration::from_secs(4),
        "stdin delivery waited for the grandchild:\n{output}"
    );
    assert_process_gone(pids[1], "the external hook stdin timeout");
    assert_marker_never_written(&marker, ready_at, "the stdin timeout");
    let _ = fs::remove_dir_all(&home);
}
