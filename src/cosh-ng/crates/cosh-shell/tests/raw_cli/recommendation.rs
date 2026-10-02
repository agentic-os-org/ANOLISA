use super::*;
use std::path::PathBuf;

#[test]
fn raw_cli_foreground_command_wins_before_analyzer_body_write() {
    let home = temp_shell_home("analyzer-foreground-race");
    let home_guard = TestDirectoryGuard::new(home.clone());
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let recommendation_root = home.join(".copilot-shell/cosh/recommendations");
    fs::create_dir_all(&recommendation_root).unwrap();
    fs::set_permissions(&recommendation_root, fs::Permissions::from_mode(0o700)).unwrap();
    let cosh_core_path = bin_dir.join("cosh-core");
    let started_marker = home.join("analyzer-started");
    let continue_marker = home.join("analyzer-continue");
    let analyzer_body = home.join("analyzer-body.json");
    write_executable(
        &cosh_core_path,
        r#"#!/bin/sh
if [ "$1" = "--registry" ]; then
  read -r request
  printf '%s\n' '{"type":"registry_response","request_id":"registry","success":true,"data":{"model":"main-model","auth_hmac":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","configured":true}}'
  exit 0
fi
case " $* " in
  *" --bare "*)
    trap 'exit 0' TERM INT HUP
    read -r init
    printf '1' > "$ANALYZER_STARTED_FILE"
    while [ ! -f "$ANALYZER_CONTINUE_FILE" ]; do sleep 0.01; done
    printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"recommendation-init","response":{"subtype":"initialize","capabilities":{}}}}'
    printf '%s\n' '{"type":"system","subtype":"init","session_id":"analyzer-session","model":"main-model","tools":[]}'
    if read -r body; then
      printf '%s\n' "$body" > "$ANALYZER_BODY_FILE"
    fi
    ;;
  *)
    read -r init
    printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{}}}}'
    printf '%s\n' '{"type":"system","subtype":"init","session_id":"foreground-trigger","model":"main-model","tools":[]}'
    read -r request
    printf '%s\n' '{"type":"assistant","session_id":"foreground-trigger","message":{"content":[{"type":"text","text":"Trigger recorded."}]}}'
    printf '%s\n' '{"type":"result","subtype":"success","session_id":"foreground-trigger","is_error":false,"result":"done"}'
    ;;
esac
"#,
    );
    let home_str = home.to_string_lossy().to_string();
    let core_str = cosh_core_path.to_string_lossy().to_string();
    let started_str = started_marker.to_string_lossy().to_string();
    let continue_str = continue_marker.to_string_lossy().to_string();
    let body_str = analyzer_body.to_string_lossy().to_string();

    let output = run_raw_cli_with_analyzer_start_barrier(
        "cosh-core",
        &[
            ("HOME", &home_str),
            ("COSH_CORE_PATH", &core_str),
            ("ANALYZER_STARTED_FILE", &started_str),
            ("ANALYZER_CONTINUE_FILE", &continue_str),
            ("ANALYZER_BODY_FILE", &body_str),
            ("COSH_RECOMMENDATIONS_ENABLED", "1"),
            ("COSH_SHELL_STARTUP_BANNER", "1"),
        ],
        b"?? inspect foreground race\n",
        &started_marker,
        &continue_marker,
        b"printf 'FOREGROUND-%s\\n' 'COMMAND-VISIBLE'\n",
        "FOREGROUND-COMMAND-VISIBLE",
    );
    let body_written = analyzer_body.exists();

    assert!(output.contains("FOREGROUND-COMMAND-VISIBLE"), "{output}");
    assert!(!body_written, "Analyzer body was written\n{output}");
    home_guard.remove();
}

struct TestDirectoryGuard(Option<PathBuf>);

impl TestDirectoryGuard {
    fn new(path: PathBuf) -> Self {
        Self(Some(path))
    }

    fn remove(mut self) {
        let path = self.0.take().expect("test directory is present");
        fs::remove_dir_all(path).expect("remove Analyzer raw CLI test directory");
    }
}

impl Drop for TestDirectoryGuard {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = fs::remove_dir_all(path);
        }
    }
}

#[test]
fn raw_cli_default_on_recommendations_keep_foreground_shell_usable() {
    let output = run_raw_cli_serial_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[
            ("COSH_RECOMMENDATIONS_ENABLED", "1"),
            ("COSH_SHELL_LANG", "en-US"),
            ("COSH_SHELL_STARTUP_BANNER", "1"),
        ],
        vec![
            (
                b"echo default-on-foreground-ready\n".to_vec(),
                Duration::from_millis(500),
            ),
            (b"exit 0\n".to_vec(), Duration::from_millis(100)),
        ],
    );

    assert!(
        output.contains(
            "Prompt recommendations are on; the current AI uses recent Shell and Agent activity."
        ),
        "{output}"
    );
    assert!(output.contains("default-on-foreground-ready"), "{output}");
}

#[test]
fn raw_cli_fresh_home_initializes_recommendation_store() {
    let home = temp_shell_home("recommendation-fresh-home");
    let home_guard = TestDirectoryGuard::new(home.clone());
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &[],
        &[
            ("HOME", &home_str),
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_HEALTH_SCAN", "disabled"),
            ("COSH_SHELL_LANG", "en-US"),
            ("COSH_RECOMMENDATIONS_ENABLED", "1"),
        ],
        &home,
        &[
            (
                "Prompt recommendations are on; the current AI uses recent Shell and Agent activity.",
                b"/recommendations on\n",
            ),
            ("Prompt recommendations are on.", b"exit\n"),
        ],
    );

    assert!(
        output.contains("Prompt recommendations are on."),
        "{output}"
    );
    assert!(
        home.join(".copilot-shell/cosh/recommendations/state.json")
            .is_file(),
        "{output}"
    );
    home_guard.remove();
}

#[test]
fn raw_cli_recommendations_off_renders_insight_cross_reference_hint() {
    let home = temp_shell_home("recommendation-off-insight-hint");
    let home_guard = TestDirectoryGuard::new(home.clone());
    let home_str = home.to_string_lossy().to_string();
    // Drive the real slash pipeline: off -> on -> clear. The cross-reference line
    // must be rendered by the successful `off` result and by nothing else, so it
    // catches a regression that drops the production wiring (a helper-only test
    // would stay green). `on` and `clear` succeed but must not carry the hint.
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &[],
        &[
            ("HOME", &home_str),
            ("COSH_SHELL_STARTUP_BANNER", "1"),
            ("COSH_SHELL_HEALTH_SCAN", "disabled"),
            ("COSH_SHELL_LANG", "en-US"),
            ("COSH_RECOMMENDATIONS_ENABLED", "1"),
        ],
        &home,
        &[
            (
                "Prompt recommendations are on; the current AI uses recent Shell and Agent activity.",
                b"/recommendations off\n",
            ),
            (
                "Prompt recommendations are off and local recommendation data was cleared.",
                b"/recommendations on\n",
            ),
            ("Prompt recommendations are on.", b"/recommendations clear\n"),
            ("Local recommendation data was cleared.", b"exit\n"),
        ],
    );

    let hint = "Command failure insights are controlled separately with /mode analysis manual.";
    assert!(
        output
            .contains("Prompt recommendations are off and local recommendation data was cleared."),
        "{output}"
    );
    // Rendered exactly once — emitted by `off`, withheld from `on` and `clear`.
    assert_eq!(output.matches(hint).count(), 1, "{output}");
    home_guard.remove();
}

#[test]
fn raw_cli_selects_recommendation_without_executing_it() {
    let output = run_raw_cli_serial_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[("COSH_SHELL_LANG", "en-US")],
        vec![
            (b"?? recommendation fixture\n".to_vec(), Duration::ZERO),
            (b"/select 2\n".to_vec(), Duration::from_millis(1500)),
            (b"echo after-select\n".to_vec(), Duration::from_millis(100)),
            (b"exit 0\n".to_vec(), Duration::from_millis(100)),
        ],
    );

    assert!(output.contains("Recommendations"));
    assert!(output.contains("  1. pwd"));
    assert!(output.contains("  2. echo $PATH"));
    assert!(output.contains("Selected recommendation 2"));
    assert!(output.contains("echo $PATH"));
    assert!(output.contains("Display-only: command was not executed; copy or re-enter it to run"));
    assert!(output.contains("after-select"));
    assert!(!output.contains("/.cargo/bin"));
}

#[test]
fn raw_cli_zh_selects_recommendation_without_executing_it() {
    let output = run_raw_cli_serial_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[("COSH_SHELL_LANG", "zh-CN")],
        vec![
            (b"?? recommendation fixture\n".to_vec(), Duration::ZERO),
            (b"/select 2\n".to_vec(), Duration::from_millis(1500)),
            (b"echo after-select\n".to_vec(), Duration::from_millis(100)),
            (b"exit 0\n".to_vec(), Duration::from_millis(100)),
        ],
    );

    assert!(output.contains("推荐"), "{output}");
    assert!(
        output.contains("用于验证仅展示推荐兼容性的显式 fixture。"),
        "{output}"
    );
    assert!(
        !output.contains("Explicit compatibility fixture"),
        "{output}"
    );
    assert!(!output.contains("[Copy] [Insert]"), "{output}");
    assert!(output.contains("仅展示：未执行任何命令"), "{output}");
    assert!(!output.contains("[Details]"), "{output}");
    assert!(output.contains("已选择推荐 2"), "{output}");
    assert!(output.contains("echo $PATH"), "{output}");
    assert!(output.contains("仅展示：命令未执行；复制或重新输入后才会运行"));
    assert!(output.contains("after-select"));
    assert!(!output.contains("/.cargo/bin"));
}

#[test]
fn raw_cli_copy_fallback_shows_recommendation_without_executing_it() {
    let output = run_raw_cli_serial_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[("COSH_SHELL_LANG", "en-US")],
        vec![
            (b"?? recommendation fixture\n".to_vec(), Duration::ZERO),
            (b"/copy 1\n".to_vec(), Duration::from_millis(2_000)),
            (b"echo after-copy\n".to_vec(), Duration::from_millis(200)),
            (b"exit 0\n".to_vec(), Duration::from_millis(100)),
        ],
    );

    assert!(output.contains("Recommendation copy"));
    assert!(output.contains("Copy recommendation 1"));
    assert!(output.contains("pwd"));
    assert!(output.contains("Copy-only: command was shown for copying; it was not executed."));
    assert!(output.contains("after-copy"));
    assert!(!output.contains("bash: /copy"));
}

#[test]
fn raw_cli_select_before_recommendation_is_display_only_noop() {
    let output = run_raw_cli_with_input("fake", "/select 1\necho after-early-select\nexit\n");

    assert!(output.contains("No selectable recommendation is available yet"));
    assert!(output.contains("after-early-select"));
    assert!(!output.contains("The command ls "));
}

#[test]
fn raw_cli_zh_select_before_recommendation_uses_catalog_fallback() {
    let output = run_raw_cli_with_env(
        "fake",
        "/select 1\necho after-early-select\nexit\n",
        &[("COSH_SHELL_LANG", "zh-CN")],
    );

    assert!(output.contains("没有可选择的推荐"), "{output}");
    assert!(output.contains("当前还没有可选择的推荐"), "{output}");
    assert!(output.contains("after-early-select"), "{output}");
    assert!(!output.contains("No selectable recommendation"), "{output}");
    assert!(
        !output.contains("No selectable recommendation is available yet"),
        "{output}"
    );
    assert!(!output.contains("The command ls "), "{output}");
    assert_no_migrated_english_ui_labels(&output, RENDERER_ZH_FORBIDDEN_UI);
}

#[test]
fn raw_cli_select_out_of_range_uses_structured_notice() {
    let output = run_raw_cli_serial_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[("COSH_SHELL_LANG", "en-US")],
        vec![
            (b"?? recommendation fixture\n".to_vec(), Duration::ZERO),
            (b"/select 99\n".to_vec(), Duration::from_millis(1500)),
            (
                b"echo after-missing-select\n".to_vec(),
                Duration::from_millis(100),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(100)),
        ],
    );

    assert!(output.contains("Recommendation unavailable"), "{output}");
    assert!(
        output.contains("Recommendation 99 is not available; choose 1..3"),
        "{output}"
    );
    assert!(output.contains("after-missing-select"), "{output}");
    assert!(!output.contains("bash: /select"), "{output}");
}
