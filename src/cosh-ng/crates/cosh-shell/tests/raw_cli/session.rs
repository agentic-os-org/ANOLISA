use super::*;

const SESSION_ONE: &str = "00000000-0000-4000-8000-000000000000";
const SESSION_TWO: &str = "11111111-1111-4111-8111-111111111111";
const SESSION_THREE: &str = "22222222-2222-4222-8222-222222222222";

#[test]
fn raw_cli_session_picker_navigates_selects_and_restores_prompt() {
    let fixture = SessionFixture::new("picker-select", FixtureMode::Ready);
    let output = fixture.run(
        &[],
        vec![
            (b"/session\n".to_vec(), Duration::from_millis(400)),
            (b"\x1b[B\n".to_vec(), Duration::from_millis(300)),
            (b"/session status\n".to_vec(), Duration::from_millis(300)),
            (
                b"echo after-session-select\nexit\n".to_vec(),
                Duration::from_millis(200),
            ),
        ],
    );

    assert!(output.contains("Agent sessions"), "{output}");
    assert!(output.contains("second prompt"), "{output}");
    assert!(output.contains("00000000…"), "{output}");
    assert!(output.contains("11111111…"), "{output}");
    assert!(output.contains("Session selected"), "{output}");
    assert!(output.contains(SESSION_TWO), "{output}");
    assert!(output.contains("recovery state: selected"), "{output}");
    assert!(output.contains("after-session-select"), "{output}");
    assert!(!output.contains("bash: /session"), "{output}");
}

#[test]
fn raw_cli_session_list_prints_full_canonical_ids() {
    let fixture = SessionFixture::new("list-full-ids", FixtureMode::Ready);
    let output = fixture.run(
        &[],
        vec![
            (b"/session list\n".to_vec(), Duration::from_millis(400)),
            (
                b"echo after-session-list\nexit\n".to_vec(),
                Duration::from_millis(200),
            ),
        ],
    );

    // Full canonical UUIDs must be copyable into /session resume|clear even
    // though both fixture sessions carry a non-empty first prompt.
    assert!(output.contains(SESSION_ONE), "{output}");
    assert!(output.contains(SESSION_TWO), "{output}");
    assert!(output.contains("first prompt"), "{output}");
    assert!(output.contains("second prompt"), "{output}");
    assert!(output.contains("after-session-list"), "{output}");
}

#[test]
fn raw_cli_session_picker_space_then_enter_confirms_and_deletes_without_resuming() {
    let fixture = SessionFixture::new("picker-space-enter", FixtureMode::Ready);
    let output = fixture.run(
        &[],
        vec![
            (b"/session\n".to_vec(), Duration::from_millis(400)),
            (b" ".to_vec(), Duration::from_millis(300)),
            (b"\n".to_vec(), Duration::from_millis(300)),
            (b"\n".to_vec(), Duration::from_millis(300)),
            (
                b"echo after-space-enter\nexit\n".to_vec(),
                Duration::from_millis(200),
            ),
        ],
    );

    let request = fs::read_to_string(&fixture.clear_log).expect("clear request log");
    assert!(output.contains("Confirm session clear"), "{output}");
    assert!(request.contains(SESSION_ONE), "{request}");
    assert!(!output.contains("Session selected"), "{output}");
    assert!(
        output.contains("Deleted 1 persisted session(s)"),
        "{output}"
    );
    assert!(output.contains("after-space-enter"), "{output}");
}

#[test]
fn raw_cli_session_picker_cancel_keeps_shell_usable() {
    let fixture = SessionFixture::new("picker-cancel", FixtureMode::Ready);
    let output = fixture.run(
        &[],
        vec![
            (b"/session\n".to_vec(), Duration::from_millis(400)),
            (b"\x1b".to_vec(), Duration::from_millis(300)),
            (
                b"echo after-session-cancel\nexit\n".to_vec(),
                Duration::from_millis(200),
            ),
        ],
    );

    assert!(output.contains("Session manager closed"), "{output}");
    assert!(output.contains("after-session-cancel"), "{output}");
    assert!(!fixture.clear_log.exists());
}

#[test]
fn raw_cli_session_picker_loads_next_page_near_viewport_end() {
    let fixture = SessionFixture::new("picker-pagination", FixtureMode::Paginated);
    let navigation = "\x1b[B".repeat(17);
    let output = fixture.run(
        &[],
        vec![
            (b"/session\n".to_vec(), Duration::from_millis(400)),
            (navigation.into_bytes(), Duration::from_millis(500)),
            (b"\x1b".to_vec(), Duration::from_millis(200)),
            (
                b"echo after-lazy-session-page\nexit\n".to_vec(),
                Duration::from_millis(300),
            ),
        ],
    );
    let requests = fs::read_to_string(&fixture.request_log).expect("session request log");

    assert!(output.contains("lazy page prompt"), "{output}");
    assert_eq!(
        requests
            .lines()
            .filter(|request| request.contains(r#""action":"list""#))
            .count(),
        2,
        "{requests}"
    );
    assert!(output.contains("after-lazy-session-page"), "{output}");
}

#[test]
fn raw_cli_session_picker_surfaces_unhealthy_and_empty_entries() {
    let unhealthy = SessionFixture::new("picker-unhealthy", FixtureMode::Unhealthy);
    let output = unhealthy.run(
        &[],
        vec![
            (b"/session\n".to_vec(), Duration::from_millis(400)),
            (b"\x1b[B\n".to_vec(), Duration::from_millis(300)),
            (b"/session\n".to_vec(), Duration::from_millis(400)),
            (b"\x1b[Bd".to_vec(), Duration::from_millis(300)),
            (b"y".to_vec(), Duration::from_millis(300)),
            (
                b"echo after-unhealthy-session\nexit\n".to_vec(),
                Duration::from_millis(200),
            ),
        ],
    );
    assert!(output.contains("corrupt"), "{output}");
    assert!(output.contains("incompatible"), "{output}");
    assert!(output.contains("cannot be resumed"), "{output}");
    assert!(fs::read_to_string(&unhealthy.clear_log)
        .expect("unhealthy clear request")
        .contains(SESSION_TWO));
    assert!(output.contains("after-unhealthy-session"), "{output}");

    let empty = SessionFixture::new("picker-empty", FixtureMode::Empty);
    let output = empty.run(
        &[],
        vec![(
            b"/session\necho after-empty-session\nexit\n".to_vec(),
            Duration::from_millis(400),
        )],
    );
    assert!(
        output.contains("No persisted sessions exist for this workspace"),
        "{output}"
    );
    assert!(output.contains("after-empty-session"), "{output}");
}

#[test]
fn raw_cli_session_picker_keeps_list_workspace_for_validate_and_clear() {
    let fixture = SessionFixture::new("picker-scope-mismatch", FixtureMode::ScopeMismatchFirst);
    let output = fixture.run(
        &[],
        vec![
            (b"/session\n".to_vec(), Duration::from_millis(400)),
            (b"\x1b[B\n".to_vec(), Duration::from_millis(300)),
            (b"/session\n".to_vec(), Duration::from_millis(400)),
            (b"d".to_vec(), Duration::from_millis(300)),
            (b"y".to_vec(), Duration::from_millis(300)),
            (b"exit\n".to_vec(), Duration::from_millis(200)),
        ],
    );

    let requests = fs::read_to_string(&fixture.request_log).expect("session request log");
    let workspace = fixture.workspace.to_string_lossy();
    assert!(output.contains("scope_mismatch"), "{output}");
    assert!(requests.contains(r#""action":"validate""#), "{requests}");
    assert!(requests.contains(r#""action":"clear""#), "{requests}");
    assert!(requests.contains(&format!(r#""workspace_scope":"{workspace}""#)));
    assert!(!requests.contains("/other/workspace"), "{requests}");
}

#[test]
fn raw_cli_session_list_all_groups_foreign_workspace_and_resume_fails() {
    let fixture = SessionFixture::new("list-all-foreign", FixtureMode::ScopeMismatchFirst);
    let output = fixture.run(
        &[],
        vec![
            (
                b"/session list --all\n".to_vec(),
                Duration::from_millis(400),
            ),
            (
                format!("/session resume {SESSION_ONE}\n").into_bytes(),
                Duration::from_millis(300),
            ),
            (
                b"echo after-list-all-foreign\nexit\n".to_vec(),
                Duration::from_millis(200),
            ),
        ],
    );

    assert!(output.contains("/other/workspace"), "{output}");
    assert!(output.contains(SESSION_ONE), "{output}");
    assert!(output.contains("session-list-all-foreign"), "{output}");
    assert!(output.contains(SESSION_TWO), "{output}");
    assert!(output.contains("scope_mismatch"), "{output}");
    assert!(output.contains("(current)"), "{output}");
    assert!(output.contains("after-list-all-foreign"), "{output}");
}

#[test]
fn raw_cli_session_multi_clear_requires_confirmation_and_cancel_is_safe() {
    let confirmed = SessionFixture::new("clear-confirmed", FixtureMode::Ready);
    let output = confirmed.run(
        &[],
        vec![
            (b"/session\n".to_vec(), Duration::from_millis(400)),
            (b" \x1b[B d".to_vec(), Duration::from_millis(300)),
            (b"y".to_vec(), Duration::from_millis(300)),
            (
                b"echo after-session-clear\nexit\n".to_vec(),
                Duration::from_millis(200),
            ),
        ],
    );
    let request = fs::read_to_string(&confirmed.clear_log).expect("clear request log");
    assert!(output.contains("Confirm session clear"), "{output}");
    assert!(request.contains(SESSION_ONE), "{request}");
    assert!(request.contains(SESSION_TWO), "{request}");
    assert!(
        output.contains("Deleted 2 persisted session(s)"),
        "{output}"
    );
    assert!(output.contains("after-session-clear"), "{output}");

    let cancelled = SessionFixture::new("clear-cancelled", FixtureMode::Ready);
    let output = cancelled.run(
        &[],
        vec![
            (b"/session\n".to_vec(), Duration::from_millis(400)),
            (b"d".to_vec(), Duration::from_millis(300)),
            (b"n".to_vec(), Duration::from_millis(300)),
            (
                b"echo after-clear-cancel\nexit\n".to_vec(),
                Duration::from_millis(200),
            ),
        ],
    );
    assert!(output.contains("Session manager closed"), "{output}");
    assert!(output.contains("after-clear-cancel"), "{output}");
    assert!(!cancelled.clear_log.exists());
}

#[test]
fn raw_cli_session_clear_all_skips_active_session_in_request() {
    let fixture = SessionFixture::new("clear-protected", FixtureMode::Ready);
    let output = fixture.run(
        &[],
        vec![
            (b"/session\n".to_vec(), Duration::from_millis(400)),
            (b"\n".to_vec(), Duration::from_millis(300)),
            (
                b"?? activate selected session\n".to_vec(),
                Duration::from_millis(1_200),
            ),
            (
                b"/session status\n/session clear --all\n".to_vec(),
                Duration::from_millis(300),
            ),
            (b"y".to_vec(), Duration::from_millis(300)),
            (b"exit\n".to_vec(), Duration::from_millis(200)),
        ],
    );
    let request = fs::read_to_string(&fixture.clear_log).expect("protected clear request");
    let session_ids = request
        .split("\"session_ids\":")
        .nth(1)
        .and_then(|tail| tail.split("],").next())
        .unwrap_or_default();
    assert!(!session_ids.contains(SESSION_ONE), "{request}");
    assert!(session_ids.contains(SESSION_TWO), "{request}");
    assert!(request.contains(SESSION_ONE), "{request}");
    assert!(output.contains("recovery state: active"), "{output}");
    assert!(output.contains("Skipped 1 protected"), "{output}");
}

#[test]
fn raw_cli_session_clear_all_reports_when_every_session_is_protected() {
    let fixture = SessionFixture::new("clear-protected-only", FixtureMode::ProtectedOnly);
    let output = fixture.run_marker(
        &[],
        &[
            (
                "cosh-osc$",
                format!("/session resume {SESSION_ONE}\n?? activate selected session\n"),
            ),
            (
                "resumed provider session",
                "/session clear --all\n".to_string(),
            ),
            (
                "Active or selected provider sessions are protected",
                "echo after-protected-only\nexit\n".to_string(),
            ),
        ],
    );

    assert!(
        output.contains("Active or selected provider sessions are protected"),
        "{output}"
    );
    assert!(output.contains("Skipped 1 protected"), "{output}");
    assert!(!output.contains("No persisted sessions exist"), "{output}");
    assert!(output.contains("after-protected-only"), "{output}");
    assert!(!fixture.clear_log.exists());
}

#[test]
fn raw_cli_session_selection_race_is_recoverable() {
    let fixture = SessionFixture::new("selection-race", FixtureMode::MissingOnValidate);
    let output = fixture.run(
        &[],
        vec![
            (b"/session\n".to_vec(), Duration::from_millis(400)),
            (b"\n".to_vec(), Duration::from_millis(300)),
            (
                b"echo after-missing-session\nexit\n".to_vec(),
                Duration::from_millis(1_000),
            ),
        ],
    );

    assert!(output.contains("[not_found]"), "{output}");
    assert!(output.contains("after-missing-session"), "{output}");
}

#[test]
fn raw_cli_session_status_shows_active_and_selected_ids() {
    let fixture = SessionFixture::new("status-active-selected", FixtureMode::Ready);
    let output = fixture.run(
        &[],
        vec![
            (
                format!("/session resume {SESSION_ONE}\n?? activate first session\n").into_bytes(),
                Duration::from_millis(1_200),
            ),
            (
                format!("/session resume {SESSION_TWO}\n/session status\nexit\n").into_bytes(),
                Duration::from_millis(400),
            ),
        ],
    );

    assert!(
        output.contains(&format!("active provider session: {SESSION_ONE}")),
        "{output}"
    );
    assert!(
        output.contains(&format!("selected provider session: {SESSION_TWO}")),
        "{output}"
    );
}

#[test]
fn raw_cli_launch_resume_enables_integration_for_value_and_picker() {
    let direct = SessionFixture::new("launch-direct", FixtureMode::Ready);
    let output = direct.run_with_env(
        &["--resume", SESSION_ONE],
        &[("COSH_SHELL_INTEGRATION", RAW_CLI_UNSET_ENV)],
        vec![
            (
                b"/session status\n?? continue recovered task\n".to_vec(),
                Duration::from_millis(500),
            ),
            (
                b"/session status\nexit\n".to_vec(),
                Duration::from_millis(1_200),
            ),
        ],
    );
    assert!(output.contains("Session selected"), "{output}");
    assert!(output.contains("recovery state: selected"), "{output}");
    assert!(
        output.contains("resumed provider session 00000000-0000-4000-8000-000000000000"),
        "{output}"
    );
    assert!(output.contains("recovery state: active"), "{output}");

    let picker = SessionFixture::new("launch-picker", FixtureMode::Ready);
    let output = picker.run_with_env(
        &["--resume"],
        &[("COSH_SHELL_INTEGRATION", RAW_CLI_UNSET_ENV)],
        vec![
            (b"\x1b".to_vec(), Duration::from_millis(700)),
            (
                b"echo after-launch-picker\nexit\n".to_vec(),
                Duration::from_millis(200),
            ),
        ],
    );
    assert!(output.contains("Agent sessions"), "{output}");
    assert!(output.contains("Session manager closed"), "{output}");
    assert!(output.contains("after-launch-picker"), "{output}");
}

#[test]
fn raw_cli_session_new_detaches_active_without_deleting_or_forcing_resume() {
    let fixture = SessionFixture::new("session-new-detach", FixtureMode::Ready);
    let output = fixture.run(
        &[],
        vec![
            (
                format!("/session resume {SESSION_ONE}\n?? activate first session\n").into_bytes(),
                Duration::from_millis(1_200),
            ),
            (
                b"/session new\n/session status\n".to_vec(),
                Duration::from_millis(400),
            ),
            (
                b"?? continue on a fresh conversation\n".to_vec(),
                Duration::from_millis(1_200),
            ),
            (
                b"/session status\n/session list\nexit\n".to_vec(),
                Duration::from_millis(400),
            ),
        ],
    );

    // The fresh-session notice reports the detached id and the fresh contract.
    assert!(
        output.contains(&format!("Detached from provider session {SESSION_ONE}")),
        "{output}"
    );
    assert!(
        output.contains("The next Agent request starts a fresh conversation"),
        "{output}"
    );
    // The next Agent request must not carry the old --resume id; the mock only
    // reports its default provider id when invoked without --resume.
    assert!(
        output.contains("active provider session: 33333333-3333-4333-8333-333333333333"),
        "{output}"
    );
    // Old persisted sessions survive the detach and remain listable.
    assert!(output.contains("first prompt"), "{output}");
    assert!(output.contains("second prompt"), "{output}");
    assert!(!output.contains("bash: /session"), "{output}");
    // Detach never deletes persisted sessions.
    assert!(!fixture.clear_log.exists());
}

#[test]
fn raw_cli_new_alias_starts_fresh_without_reaching_bash_or_changing_cwd() {
    let fixture = SessionFixture::new("session-new-alias", FixtureMode::Ready);
    let workspace = fixture.workspace.to_string_lossy().into_owned();
    let output = fixture.run(
        &[],
        vec![
            (b"pwd\n".to_vec(), Duration::from_millis(300)),
            (b"/new\n".to_vec(), Duration::from_millis(400)),
            (
                b"pwd\necho after-new-alias\nexit\n".to_vec(),
                Duration::from_millis(300),
            ),
        ],
    );

    // `/new` is intercepted as the session `new` path, never sent to bash.
    assert!(!output.contains("bash: /new"), "{output}");
    assert!(output.contains("Fresh session"), "{output}");
    // No session was attached, so the alias reports the idempotent contract.
    assert!(
        output.contains("No provider session was attached"),
        "{output}"
    );
    assert!(
        output.contains("The next Agent request starts a fresh conversation"),
        "{output}"
    );
    // The shell keeps running with an unchanged cwd after the fresh session.
    assert!(output.contains("after-new-alias"), "{output}");
    assert!(output.contains(&workspace), "{output}");
}

#[derive(Clone, Copy)]
enum FixtureMode {
    Ready,
    ProtectedOnly,
    Unhealthy,
    Empty,
    MissingOnValidate,
    ScopeMismatchFirst,
    Paginated,
}

struct SessionFixture {
    home: std::path::PathBuf,
    workspace: std::path::PathBuf,
    core: std::path::PathBuf,
    clear_log: std::path::PathBuf,
    request_log: std::path::PathBuf,
}

impl SessionFixture {
    fn new(label: &str, mode: FixtureMode) -> Self {
        let home = temp_shell_home(&format!("session-{label}"));
        let workspace = home.join("workspace");
        let bin = home.join("bin");
        fs::create_dir_all(&workspace).expect("create session workspace");
        fs::create_dir_all(&bin).expect("create session fixture bin");
        let workspace = fs::canonicalize(workspace).expect("canonical session workspace");
        let core = bin.join("cosh-core");
        let clear_log = home.join("clear-request.json");
        let request_log = home.join("session-requests.jsonl");
        let workspace_text = workspace.to_string_lossy();
        let clear_log_text = clear_log.to_string_lossy();
        let request_log_text = request_log.to_string_lossy();
        let sessions = match mode {
            FixtureMode::Empty => String::new(),
            FixtureMode::Ready | FixtureMode::ProtectedOnly | FixtureMode::MissingOnValidate => {
                format!(
                    "{},{}",
                    session_json(SESSION_ONE, "first prompt", "ready"),
                    session_json(SESSION_TWO, "second prompt", "ready")
                )
            }
            FixtureMode::Unhealthy => format!(
                "{},{},{}",
                session_json(SESSION_ONE, "ready prompt", "ready"),
                session_json(SESSION_TWO, "damaged prompt", "corrupt"),
                session_json(SESSION_THREE, "future prompt", "incompatible")
            ),
            FixtureMode::ScopeMismatchFirst => format!(
                "{},{}",
                session_json(SESSION_ONE, "foreign prompt", "scope_mismatch")
                    .replace("__WORKSPACE__", "/other/workspace"),
                session_json(SESSION_TWO, "local prompt", "ready")
            ),
            FixtureMode::Paginated => (0..20)
                .map(|index| {
                    session_json(
                        &format!("{index:08x}-0000-4000-8000-{index:012x}"),
                        &format!("initial page prompt {index}"),
                        "ready",
                    )
                })
                .collect::<Vec<_>>()
                .join(","),
        };
        let (next_cursor, next_sessions) = if matches!(mode, FixtureMode::Paginated) {
            (
                r#""page-1""#,
                session_json(
                    "99999999-9999-4999-8999-999999999999",
                    "lazy page prompt",
                    "ready",
                ),
            )
        } else {
            ("null", String::new())
        };
        let validate_missing = matches!(mode, FixtureMode::MissingOnValidate);
        let protected_only = matches!(mode, FixtureMode::ProtectedOnly);
        let scope_mismatch_first = matches!(mode, FixtureMode::ScopeMismatchFirst);
        let script = SESSION_CORE_SCRIPT
            .replace("__SESSIONS__", &sessions)
            .replace("__WORKSPACE__", &workspace_text)
            .replace("__CLEAR_LOG__", &clear_log_text)
            .replace("__REQUEST_LOG__", &request_log_text)
            .replace("__NEXT_CURSOR__", next_cursor)
            .replace("__NEXT_SESSIONS__", &next_sessions)
            .replace(
                "__VALIDATE_MISSING__",
                if validate_missing { "1" } else { "0" },
            )
            .replace("__PROTECTED_ONLY__", if protected_only { "1" } else { "0" })
            .replace(
                "__SCOPE_MISMATCH_FIRST__",
                if scope_mismatch_first { "1" } else { "0" },
            );
        write_executable(&core, &script);
        Self {
            home,
            workspace,
            core,
            clear_log,
            request_log,
        }
    }

    fn run(&self, args: &[&str], chunks: Vec<(Vec<u8>, Duration)>) -> String {
        self.run_with_env(args, &[], chunks)
    }

    fn run_with_env(
        &self,
        args: &[&str],
        extra_env: &[(&str, &str)],
        chunks: Vec<(Vec<u8>, Duration)>,
    ) -> String {
        let home = self.home.to_string_lossy().into_owned();
        let core = self.core.to_string_lossy().into_owned();
        let mut env = vec![("HOME", home.as_str()), ("COSH_CORE_PATH", core.as_str())];
        env.extend_from_slice(extra_env);
        run_raw_cli_with_args_env_current_dir_and_delayed_input_after_marker(
            "cosh-core",
            args,
            &env,
            &self.workspace,
            "cosh-osc$ ",
            chunks,
        )
    }

    fn run_marker(&self, args: &[&str], steps: &[(&str, String)]) -> String {
        let home = self.home.to_string_lossy().into_owned();
        let core = self.core.to_string_lossy().into_owned();
        let borrowed_steps: Vec<(&str, &[u8])> = steps
            .iter()
            .map(|(marker, input)| (*marker, input.as_bytes()))
            .collect();
        run_raw_cli_with_args_env_current_dir_and_marker_input(
            "cosh-core",
            args,
            &[("HOME", &home), ("COSH_CORE_PATH", &core)],
            &self.workspace,
            &borrowed_steps,
        )
    }
}

fn session_json(id: &str, prompt: &str, health: &str) -> String {
    format!(
        r#"{{"session_id":"{id}","workspace_scope":"__WORKSPACE__","created_at_ms":1,"updated_at_ms":2,"model":"mock-history","message_count":2,"first_prompt":"{prompt}","schema_version":1,"health":"{health}"}}"#
    )
}

const SESSION_CORE_SCRIPT: &str = r#"#!/bin/sh
if [ "$1" = "--session-control" ]; then
  read -r request
  printf '%s\n' "$request" >> "__REQUEST_LOG__"
  case "$request" in
    *'"action":"prepare_clear_all"'*)
      if [ "__PROTECTED_ONLY__" = "1" ]; then
        printf '%s\n' '{"ok":true,"data":{"action":"prepare_clear_all","session_ids":[],"protected_session_ids":["00000000-0000-4000-8000-000000000000"]}}'
      elif printf '%s' "$request" | grep -q '00000000-0000-4000-8000-000000000000'; then
        printf '%s\n' '{"ok":true,"data":{"action":"prepare_clear_all","session_ids":["11111111-1111-4111-8111-111111111111"],"protected_session_ids":["00000000-0000-4000-8000-000000000000"]}}'
      else
        printf '%s\n' '{"ok":true,"data":{"action":"prepare_clear_all","session_ids":["00000000-0000-4000-8000-000000000000","11111111-1111-4111-8111-111111111111"],"protected_session_ids":[]}}'
      fi
      ;;
    *'"action":"list"'*)
      if printf '%s' "$request" | grep -q '"cursor":"page-1"'; then
        printf '%s\n' '{"ok":true,"data":{"action":"list","sessions":[__NEXT_SESSIONS__],"next_cursor":null}}'
      else
        printf '%s\n' '{"ok":true,"data":{"action":"list","sessions":[__SESSIONS__],"next_cursor":__NEXT_CURSOR__}}'
      fi
      ;;
    *'"action":"validate"'*)
      if [ "__VALIDATE_MISSING__" = "1" ]; then
        printf '%s\n' '{"ok":false,"error":{"code":"not_found","message":"session disappeared after listing","recoverable":true,"hint":"Refresh the session list and retry."}}'
      elif printf '%s' "$request" | grep -q '11111111-1111-4111-8111-111111111111'; then
        printf '%s\n' '{"ok":true,"data":{"action":"validate","session":{"session_id":"11111111-1111-4111-8111-111111111111","workspace_scope":"__WORKSPACE__","created_at_ms":1,"updated_at_ms":2,"model":"mock-history","message_count":2,"first_prompt":"second prompt","schema_version":1,"health":"ready"}}}'
      elif [ "__SCOPE_MISMATCH_FIRST__" = "1" ]; then
        printf '%s\n' '{"ok":false,"error":{"code":"scope_mismatch","message":"session belongs to another workspace","recoverable":true,"hint":"Return to the original workspace to resume this session."}}'
      else
        printf '%s\n' '{"ok":true,"data":{"action":"validate","session":{"session_id":"00000000-0000-4000-8000-000000000000","workspace_scope":"__WORKSPACE__","created_at_ms":1,"updated_at_ms":2,"model":"mock-history","message_count":2,"first_prompt":"first prompt","schema_version":1,"health":"ready"}}}'
      fi
      ;;
    *'"action":"inspect"'*)
      printf '%s\n' '{"ok":true,"data":{"action":"inspect","session":{"session_id":"00000000-0000-4000-8000-000000000000","workspace_scope":"__WORKSPACE__","created_at_ms":1,"updated_at_ms":2,"model":"mock-history","message_count":2,"first_prompt":"first prompt","schema_version":1,"health":"ready"}}}'
      ;;
    *'"action":"clear"'*)
      printf '%s\n' "$request" > "__CLEAR_LOG__"
      if printf '%s' "$request" | grep -q '"session_ids":\["00000000-0000-4000-8000-000000000000","11111111-1111-4111-8111-111111111111"\]'; then
        printf '%s\n' '{"ok":true,"data":{"action":"clear","deleted":["00000000-0000-4000-8000-000000000000","11111111-1111-4111-8111-111111111111"],"skipped":[]}}'
      else
        printf '%s\n' '{"ok":true,"data":{"action":"clear","deleted":["11111111-1111-4111-8111-111111111111"],"skipped":[]}}'
      fi
      ;;
    *)
      printf '%s\n' '{"ok":false,"error":{"code":"corrupt","message":"unexpected request","recoverable":true,"hint":null}}'
      ;;
  esac
  exit 0
fi

provider_id="33333333-3333-4333-8333-333333333333"
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--resume" ]; then
    shift
    provider_id="$1"
    break
  fi
  shift
done
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
printf '{"type":"system","subtype":"init","session_id":"%s","model":"mock-history","tools":[]}\n' "$provider_id"
read -r user_message
printf '{"type":"assistant","session_id":"%s","message":{"content":[{"type":"text","text":"resumed provider session %s"}]}}\n' "$provider_id" "$provider_id"
printf '{"type":"result","subtype":"success","session_id":"%s","is_error":false,"duration_ms":1,"result":"done"}\n' "$provider_id"
"#;
