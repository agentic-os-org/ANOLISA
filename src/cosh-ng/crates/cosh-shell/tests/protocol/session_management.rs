use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cosh_shell::adapter::{
    CoshCoreAdapter, SessionHealth, SessionManagementClient, SessionRecoveryState,
};

fn assert_recorded_process_is_gone(pid_file: &Path) {
    let pid: i32 = fs::read_to_string(pid_file)
        .expect("read session-control pid")
        .trim()
        .parse()
        .expect("parse session-control pid");
    let result = unsafe { nix::libc::kill(pid, 0) };
    let error = std::io::Error::last_os_error();
    assert_eq!(result, -1, "session-control PID {pid} is still alive");
    assert_eq!(
        error.raw_os_error(),
        Some(nix::libc::ESRCH),
        "unexpected PID probe error for {pid}: {error}"
    );
}

fn assert_recorded_process_is_not_running(pid_file: &Path) {
    let pid = fs::read_to_string(pid_file)
        .expect("read session-control pid")
        .trim()
        .to_string();
    let status = fs::read_to_string(format!("/proc/{pid}/status"));
    if let Ok(status) = status {
        assert!(
            status
                .lines()
                .any(|line| { line.starts_with("State:\tZ") || line.starts_with("State:\tX") }),
            "session-control descendant PID {pid} is still running: {status}"
        );
    }
}

#[cfg(target_os = "linux")]
fn terminate_recorded_process(pid_file: &Path) {
    let pid: i32 = fs::read_to_string(pid_file)
        .expect("read session-control pid")
        .trim()
        .parse()
        .expect("parse session-control pid");
    let result = unsafe { nix::libc::kill(pid, nix::libc::SIGKILL) };
    if result < 0 {
        let error = std::io::Error::last_os_error();
        assert_eq!(
            error.raw_os_error(),
            Some(nix::libc::ESRCH),
            "failed to terminate escaped session-control descendant {pid}: {error}"
        );
    }
}

fn wait_for_path(path: &Path) {
    // Generous bound: slow CI runners may need seconds before a detached
    // descendant gets scheduled and writes its marker file.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if path.exists() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out waiting for {}", path.display());
}

#[test]
fn session_management_client_parses_pages_and_per_item_clear_errors() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    let temp = std::env::temp_dir().join(format!(
        "cosh-session-protocol-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(&temp).expect("create tempdir");
    let script = temp.join("cosh-core");
    fs::write(
        &script,
        r#"#!/bin/sh
request=$(cat)
case "$request" in
  *'"action":"prepare_clear_all"'*)
    printf '%s\n' '{"ok":true,"data":{"action":"prepare_clear_all","session_ids":["11111111-1111-4111-8111-111111111111"],"protected_session_ids":["00000000-0000-4000-8000-000000000000"]}}'
    ;;
  *'"action":"list"'*)
    printf '%s\n' '{"ok":true,"data":{"action":"list","sessions":[{"session_id":"00000000-0000-4000-8000-000000000000","workspace_scope":"/tmp","created_at_ms":1,"updated_at_ms":2,"model":"mock","message_count":2,"first_prompt":"remember","schema_version":1,"health":"ready"}],"next_cursor":"00000000-0000-4000-8000-000000000000"}}'
    ;;
  *'"action":"clear"'*)
    printf '%s\n' '{"ok":true,"data":{"action":"clear","deleted":[],"skipped":[{"session_id":"00000000-0000-4000-8000-000000000000","error":{"code":"active_session","message":"protected","recoverable":true,"hint":"select another session"}}]}}'
    ;;
  *)
    printf '%s\n' '{"ok":false,"error":{"code":"corrupt","message":"unexpected request","recoverable":true,"hint":null}}'
    exit 1
    ;;
esac
"#,
    )
    .expect("write mock cosh-core");
    let mut permissions = fs::metadata(&script).expect("metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script, permissions).expect("chmod");

    let client = SessionManagementClient::new(script.display().to_string());
    let page = client.list("/tmp", 1, None, false).expect("list page");
    assert_eq!(page.sessions.len(), 1);
    assert_eq!(page.sessions[0].health, SessionHealth::Ready);
    assert_eq!(
        page.next_cursor.as_deref(),
        Some("00000000-0000-4000-8000-000000000000")
    );

    let id = "00000000-0000-4000-8000-000000000000".to_string();
    let plan = client
        .prepare_clear_all("/tmp", std::slice::from_ref(&id))
        .expect("clear-all plan");
    assert_eq!(
        plan.session_ids,
        vec!["11111111-1111-4111-8111-111111111111"]
    );
    assert_eq!(plan.protected_session_ids, vec![id.clone()]);
    let clear = client
        .clear("/tmp", std::slice::from_ref(&id), std::slice::from_ref(&id))
        .expect("clear result");
    assert!(clear.deleted.is_empty());
    assert_eq!(clear.skipped.len(), 1);
    assert_eq!(clear.interruption, None);
    assert_eq!(clear.skipped[0].error.code, "active_session");
    assert!(clear.skipped[0].error.recoverable);

    fs::remove_dir_all(temp).expect("remove tempdir");
}

#[test]
fn session_management_paginates_clear_all_and_batches_clear_requests() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    let temp = std::env::temp_dir().join(format!(
        "cosh-session-batched-clear-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(&temp).expect("create tempdir");
    let clear_log = temp.join("clear.log");
    let script = temp.join("cosh-core");
    fs::write(
        &script,
        format!(
            r#"#!/bin/sh
request=$(cat)
case "$request" in
  *'"action":"prepare_clear_all"'*'"cursor":"11111111-1111-4111-8111-111111111111"'*)
    printf '%s\n' '{{"ok":true,"data":{{"action":"prepare_clear_all","session_ids":["33333333-3333-4333-8333-333333333333"],"protected_session_ids":[],"next_cursor":null}}}}'
    ;;
  *'"action":"prepare_clear_all"'*)
    printf '%s\n' '{{"ok":true,"data":{{"action":"prepare_clear_all","session_ids":["11111111-1111-4111-8111-111111111111"],"protected_session_ids":["22222222-2222-4222-8222-222222222222"],"next_cursor":"11111111-1111-4111-8111-111111111111"}}}}'
    ;;
  *'"action":"clear"'*)
    printf '%s\n' "${{#request}}" >> "{}"
    printf '%s\n' '{{"ok":true,"data":{{"action":"clear","deleted":[],"skipped":[]}}}}'
    ;;
esac
"#,
            clear_log.display()
        ),
    )
    .expect("write mock cosh-core");
    let mut permissions = fs::metadata(&script).expect("metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script, permissions).expect("chmod");

    let client = SessionManagementClient::new(script.display().to_string());
    let plan = client
        .prepare_clear_all("/tmp", &[])
        .expect("paginated clear-all plan");
    assert_eq!(
        plan.session_ids,
        vec![
            "11111111-1111-4111-8111-111111111111",
            "33333333-3333-4333-8333-333333333333"
        ]
    );
    assert_eq!(
        plan.protected_session_ids,
        vec!["22222222-2222-4222-8222-222222222222"]
    );

    let requested = vec!["44444444-4444-4444-8444-444444444444".to_string(); 257];
    let result = client
        .clear("/tmp", &requested, &[])
        .expect("batched clear result");
    assert!(result.deleted.is_empty());
    assert!(result.skipped.is_empty());
    assert_eq!(result.interruption, None);
    let request_sizes = fs::read_to_string(&clear_log).expect("read clear request log");
    let request_sizes = request_sizes
        .lines()
        .map(|value| value.parse::<usize>().expect("clear request size"))
        .collect::<Vec<_>>();
    assert_eq!(request_sizes.len(), 3);
    assert!(request_sizes.iter().all(|size| *size < 16 * 1024));

    fs::remove_dir_all(temp).expect("remove tempdir");
}

#[test]
fn adapter_serializes_clear_with_concurrent_selection() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    let temp = std::env::temp_dir().join(format!(
        "cosh-session-clear-selection-race-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(&temp).expect("create tempdir");
    let session_file = temp.join("session.exists");
    let clear_started = temp.join("clear.started");
    let clear_release = temp.join("clear.release");
    let validate_started = temp.join("validate.started");
    fs::write(&session_file, b"present").expect("create mock session");
    let script = temp.join("cosh-core");
    let session_id = "00000000-0000-4000-8000-000000000000";
    fs::write(
        &script,
        format!(
            r#"#!/bin/sh
request=$(cat)
case "$request" in
  *'"action":"clear"'*)
    : > "{}"
    while [ ! -f "{}" ]; do sleep 0.01; done
    rm -f "{}"
    printf '%s\n' '{{"ok":true,"data":{{"action":"clear","deleted":["{session_id}"],"skipped":[]}}}}'
    ;;
  *'"action":"validate"'*)
    : > "{}"
    if [ -f "{}" ]; then
      printf '%s\n' '{{"ok":true,"data":{{"action":"validate","session":{{"session_id":"{session_id}","workspace_scope":"/tmp","created_at_ms":1,"updated_at_ms":2,"model":"mock","message_count":1,"first_prompt":"remember","schema_version":1,"health":"ready"}}}}}}'
    else
      printf '%s\n' '{{"ok":false,"error":{{"code":"not_found","message":"session was cleared","recoverable":true,"hint":"Refresh and retry."}}}}'
    fi
    ;;
esac
"#,
            clear_started.display(),
            clear_release.display(),
            session_file.display(),
            validate_started.display(),
            session_file.display(),
        ),
    )
    .expect("write mock cosh-core");
    let mut permissions = fs::metadata(&script).expect("metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script, permissions).expect("chmod");
    let adapter = CoshCoreAdapter::new(script.display().to_string(), false);

    let clear_adapter = adapter.clone();
    let clear_id = session_id.to_string();
    let clear_thread =
        std::thread::spawn(move || clear_adapter.clear_sessions("/tmp", &[clear_id]));
    wait_for_path(&clear_started);

    let select_adapter = adapter.clone();
    let select_thread =
        std::thread::spawn(move || select_adapter.select_session("/tmp", session_id));
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        !validate_started.exists(),
        "selection validation started while clear held the state lease"
    );

    fs::write(&clear_release, b"release").expect("release clear");
    let clear = clear_thread
        .join()
        .expect("join clear")
        .expect("clear response");
    assert_eq!(clear.deleted, vec![session_id]);
    let selection = select_thread.join().expect("join selection");
    assert_eq!(
        selection
            .expect_err("cleared session must not become selected")
            .code,
        "not_found"
    );
    assert_eq!(
        adapter.recovery_snapshot().state,
        SessionRecoveryState::Failed
    );
    fs::remove_dir_all(temp).expect("remove tempdir");
}

#[test]
fn session_management_preserves_partial_clear_results_after_later_batch_failure() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    let temp = std::env::temp_dir().join(format!(
        "cosh-session-partial-clear-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(&temp).expect("create tempdir");
    let script = temp.join("cosh-core");
    let first = "11111111-1111-4111-8111-111111111111";
    let second = "22222222-2222-4222-8222-222222222222";
    let third = "33333333-3333-4333-8333-333333333333";
    fs::write(
        &script,
        format!(
            r#"#!/bin/sh
request=$(cat)
case "$request" in
  *'{second}'*)
    printf '%s\n' '{{"ok":false,"error":{{"code":"io","message":"second batch response failed","recoverable":true,"hint":"retry"}}}}'
    exit 1
    ;;
  *)
    printf '%s\n' '{{"ok":true,"data":{{"action":"clear","deleted":["{first}"],"skipped":[]}}}}'
    ;;
esac
"#
        ),
    )
    .expect("write partial clear mock");
    let mut permissions = fs::metadata(&script).expect("metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script, permissions).expect("chmod");
    let mut requested = vec![first.to_string(); 128];
    requested.extend(vec![second.to_string(); 128]);
    requested.push(third.to_string());

    let result = SessionManagementClient::new(script.display().to_string())
        .clear("/tmp", &requested, &[])
        .expect("partial clear result must remain inspectable");

    assert_eq!(result.deleted, vec![first]);
    assert!(result.skipped.is_empty());
    let interruption = result.interruption.expect("partial clear interruption");
    assert_eq!(interruption.error.code, "io");
    assert_eq!(interruption.unknown_session_ids, vec![second; 128]);
    assert_eq!(interruption.unattempted_session_ids, vec![third]);
    fs::remove_dir_all(temp).expect("remove tempdir");
}

#[test]
fn session_management_write_failure_terminates_and_reaps_process() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    let temp =
        std::env::temp_dir().join(format!("cosh-session-epipe-{}-{nonce}", std::process::id()));
    fs::create_dir_all(&temp).expect("create tempdir");
    let pid_file = temp.join("session-control.pid");
    let script = temp.join("cosh-core");
    fs::write(
        &script,
        format!(
            r#"#!/bin/sh
printf '%s\n' "$$" > "{}"
exec 0<&-
exec sleep 30
"#,
            pid_file.display()
        ),
    )
    .expect("write mock cosh-core");
    let mut permissions = fs::metadata(&script).expect("metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script, permissions).expect("chmod");

    let client = SessionManagementClient::new(script.display().to_string());
    let oversized_id = "x".repeat(2 * 1024 * 1024);
    let result = client
        .clear("/tmp", &[oversized_id], &[])
        .expect("write failure must retain the unknown clear batch");
    let interruption = result.interruption.expect("write failure interruption");

    assert_eq!(interruption.error.code, "transport");
    assert!(
        interruption
            .error
            .message
            .contains("failed to encode session request")
            || interruption
                .error
                .message
                .contains("failed to send session request")
            || interruption.error.message.contains("request write failed"),
        "{}",
        interruption.error.message
    );
    assert_eq!(interruption.unknown_session_ids.len(), 1);
    assert!(interruption.unattempted_session_ids.is_empty());
    assert_recorded_process_is_gone(&pid_file);
    fs::remove_dir_all(temp).expect("remove tempdir");
}

#[test]
fn session_management_drains_large_stderr_before_reaping_process() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    let temp = std::env::temp_dir().join(format!(
        "cosh-session-output-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(&temp).expect("create tempdir");
    let pid_file = temp.join("session-control.pid");
    let script = temp.join("cosh-core");
    fs::write(
        &script,
        format!(
            r#"#!/bin/sh
printf '%s\n' "$$" > "{}"
cat >/dev/null
i=0
while [ "$i" -lt 4096 ]; do
  printf '%s' '0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef' >&2
  i=$((i + 1))
done
printf '%s\n' '{{"ok":true,"data":{{"action":"list","sessions":[],"next_cursor":null}}}}'
"#,
            pid_file.display()
        ),
    )
    .expect("write mock cosh-core");
    let mut permissions = fs::metadata(&script).expect("metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script, permissions).expect("chmod");

    let client = SessionManagementClient::new(script.display().to_string());
    let page = client.list("/tmp", 10, None, false).expect("list page");

    assert!(page.sessions.is_empty());
    assert_eq!(page.next_cursor, None);
    assert_recorded_process_is_gone(&pid_file);
    fs::remove_dir_all(temp).expect("remove tempdir");
}

#[test]
fn session_management_rejects_bounded_stdout_and_stderr_overflow() {
    for (stream, redirect, iterations) in [("stdout", "", 32_768), ("stderr", ">&2", 8_192)] {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let temp = std::env::temp_dir().join(format!(
            "cosh-session-{stream}-overflow-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&temp).expect("create tempdir");
        let pid_file = temp.join("session-control.pid");
        let script = temp.join("cosh-core");
        fs::write(
            &script,
            format!(
                r#"#!/bin/sh
printf '%s\n' "$$" > "{}"
cat >/dev/null
i=0
while [ "$i" -lt {iterations} ]; do
  printf '%s' '0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef' {redirect}
  i=$((i + 1))
done
exec sleep 30
"#,
                pid_file.display()
            ),
        )
        .expect("write mock cosh-core");
        let mut permissions = fs::metadata(&script).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).expect("chmod");

        let client = SessionManagementClient::new(script.display().to_string())
            .with_timeout(Duration::from_secs(2));
        let started = std::time::Instant::now();
        let error = client
            .list("/tmp", 10, None, false)
            .expect_err("oversized output must fail");

        assert_eq!(error.code, "transport");
        assert!(
            error.message.contains(&format!("{stream} exceeded")),
            "{}",
            error.message
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{stream} overflow cleanup was delayed: {:?}",
            started.elapsed()
        );
        assert_recorded_process_is_gone(&pid_file);
        fs::remove_dir_all(temp).expect("remove tempdir");
    }
}

#[test]
fn session_management_timeout_kills_term_ignoring_process() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    let temp = std::env::temp_dir().join(format!(
        "cosh-session-timeout-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(&temp).expect("create tempdir");
    let pid_file = temp.join("session-control.pid");
    let script = temp.join("cosh-core");
    fs::write(
        &script,
        format!(
            r#"#!/bin/sh
printf '%s\n' "$$" > "{}"
cat >/dev/null
trap '' TERM
exec sleep 30
"#,
            pid_file.display()
        ),
    )
    .expect("write mock cosh-core");
    let mut permissions = fs::metadata(&script).expect("metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script, permissions).expect("chmod");

    let client = SessionManagementClient::new(script.display().to_string())
        .with_timeout(Duration::from_millis(500));
    let error = client
        .list("/tmp", 10, None, false)
        .expect_err("hung management process must time out");

    assert_eq!(error.code, "transport");
    assert!(
        error.message.contains("exceeded 500ms"),
        "{}",
        error.message
    );
    assert_recorded_process_is_gone(&pid_file);
    fs::remove_dir_all(temp).expect("remove tempdir");
}

#[test]
fn session_management_timeout_covers_blocked_request_write() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    let temp = std::env::temp_dir().join(format!(
        "cosh-session-write-timeout-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(&temp).expect("create tempdir");
    let pid_file = temp.join("session-control.pid");
    let script = temp.join("cosh-core");
    fs::write(
        &script,
        format!(
            r#"#!/bin/sh
printf '%s\n' "$$" > "{}"
trap '' TERM
exec sleep 30
"#,
            pid_file.display()
        ),
    )
    .expect("write mock cosh-core");
    let mut permissions = fs::metadata(&script).expect("metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script, permissions).expect("chmod");

    let client = SessionManagementClient::new(script.display().to_string())
        .with_timeout(Duration::from_secs(2));
    let oversized_id = "x".repeat(2 * 1024 * 1024);
    let started = std::time::Instant::now();
    let result = client
        .clear("/tmp", &[oversized_id], &[])
        .expect("blocked writer must retain the unknown clear batch");
    let interruption = result.interruption.expect("blocked writer interruption");

    assert_eq!(interruption.error.code, "transport");
    assert!(
        interruption.error.message.contains("exceeded 2000ms"),
        "{}",
        interruption.error.message
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "blocked request exceeded bounded cleanup: {:?}",
        started.elapsed()
    );
    assert_recorded_process_is_gone(&pid_file);
    fs::remove_dir_all(temp).expect("remove tempdir");
}

#[test]
fn session_management_reaps_descendant_that_inherits_output_pipes() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    let temp = std::env::temp_dir().join(format!(
        "cosh-session-descendant-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(&temp).expect("create tempdir");
    let pid_file = temp.join("session-control-descendant.pid");
    let script = temp.join("cosh-core");
    fs::write(
        &script,
        format!(
            r#"#!/bin/sh
cat >/dev/null
sleep 30 &
printf '%s\n' "$!" > "{}"
exit 0
"#,
            pid_file.display()
        ),
    )
    .expect("write mock cosh-core");
    let mut permissions = fs::metadata(&script).expect("metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script, permissions).expect("chmod");

    let client = SessionManagementClient::new(script.display().to_string())
        .with_timeout(Duration::from_millis(500));
    let error = client
        .list("/tmp", 10, None, false)
        .expect_err("empty wrapper response is invalid");

    assert_eq!(error.code, "transport");
    assert_recorded_process_is_not_running(&pid_file);
    fs::remove_dir_all(temp).expect("remove tempdir");
}

#[test]
#[cfg(target_os = "linux")]
fn session_management_deadline_survives_setsid_descendant_with_output_pipes() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    let temp = std::env::temp_dir().join(format!(
        "cosh-session-escaped-descendant-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(&temp).expect("create tempdir");
    let pid_file = temp.join("session-control-descendant.pid");
    let script = temp.join("cosh-core");
    fs::write(
        &script,
        format!(
            r#"#!/bin/sh
cat >/dev/null
setsid sh -c 'printf "%s\n" "$$" > "{}"; exec sleep 5' &
while [ ! -s "{}" ]; do
  sleep 0.01
done
sleep 0.2
exit 0
"#,
            pid_file.display(),
            pid_file.display()
        ),
    )
    .expect("write mock cosh-core");
    let mut permissions = fs::metadata(&script).expect("metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script, permissions).expect("chmod");

    let client = SessionManagementClient::new(script.display().to_string())
        .with_timeout(Duration::from_secs(2));
    let started = std::time::Instant::now();
    let error = client
        .list("/tmp", 10, None, false)
        .expect_err("empty wrapper response is invalid");
    let elapsed = started.elapsed();
    // The wrapper only exits after the marker exists, but a deadline kill on
    // a slow runner can return before the detached descendant writes it.
    wait_for_path(&pid_file);
    terminate_recorded_process(&pid_file);

    assert_eq!(error.code, "transport");
    assert!(
        elapsed < Duration::from_secs(4),
        "escaped output pipe holder exceeded cleanup bound: {elapsed:?}"
    );
    fs::remove_dir_all(temp).expect("remove tempdir");
}

#[test]
#[cfg(target_os = "linux")]
fn session_management_deadline_survives_setsid_descendant_holding_stdin() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    let temp = std::env::temp_dir().join(format!(
        "cosh-session-escaped-stdin-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(&temp).expect("create tempdir");
    let pid_file = temp.join("session-control-descendant.pid");
    let stdin_target_file = temp.join("session-control-descendant.stdin");
    let script = temp.join("cosh-core");
    fs::write(
        &script,
        format!(
            r#"#!/bin/sh
exec 3<&0
setsid sh -c 'printf "%s\n" "$$" > "{}"; readlink "/proc/$$/fd/0" > "{}"; exec sleep 5' <&3 &
exec 3<&-
while [ ! -s "{}" ] || [ ! -s "{}" ]; do
  sleep 0.01
done
exit 0
"#,
            pid_file.display(),
            stdin_target_file.display(),
            pid_file.display(),
            stdin_target_file.display(),
        ),
    )
    .expect("write mock cosh-core");
    let mut permissions = fs::metadata(&script).expect("metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script, permissions).expect("chmod");

    let client = SessionManagementClient::new(script.display().to_string())
        .with_timeout(Duration::from_secs(2));
    let oversized_id = "x".repeat(10 * 1024 * 1024);
    let started = std::time::Instant::now();
    let result = client
        .clear("/tmp", &[oversized_id], &[])
        .expect("blocked batch must remain inspectable");
    let elapsed = started.elapsed();
    // The deadline kill can fire before the detached descendant finishes
    // writing its markers on a slow runner; wait before inspecting them.
    wait_for_path(&pid_file);
    wait_for_path(&stdin_target_file);
    terminate_recorded_process(&pid_file);
    let interruption = result.interruption.expect("write interruption");
    let stdin_target = fs::read_to_string(&stdin_target_file).expect("descendant stdin target");

    assert_eq!(interruption.error.code, "transport");
    assert!(
        interruption.error.message.contains("request write failed"),
        "{}",
        interruption.error.message
    );
    assert!(
        stdin_target.trim().starts_with("pipe:["),
        "detached descendant did not inherit the request pipe: {stdin_target:?}"
    );
    assert!(
        elapsed < Duration::from_secs(4),
        "escaped stdin holder exceeded cleanup bound: {elapsed:?}"
    );
    fs::remove_dir_all(temp).expect("remove tempdir");
}
