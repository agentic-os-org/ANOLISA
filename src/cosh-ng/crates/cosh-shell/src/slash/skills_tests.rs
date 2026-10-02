use super::commands::render_slash_command;
use super::parser::SlashCommand;
use super::skills::{
    completion_skill_names, format_skill_detail, format_skills_list, render_skills_command,
};
use crate::runtime::prelude::*;
use crate::types::ShellEvent;

fn zh_state() -> InlineState {
    InlineState {
        language: Language::ZhCn,
        ..InlineState::default()
    }
}

fn en_state() -> InlineState {
    InlineState {
        language: Language::EnUs,
        ..InlineState::default()
    }
}

#[test]
fn skills_non_cosh_core_shows_unavailable_zh() {
    let adapter = AdapterInstance::Fake(crate::adapter::FakeAgentAdapter);
    let mut state = zh_state();
    let mut buf = Vec::new();
    render_skills_command(None, None, &adapter, &mut state, &mut buf).unwrap();
    let output = String::from_utf8(buf).unwrap();
    assert!(
        output.contains("cosh-core") || output.contains("后端"),
        "should contain degradation message: {output}"
    );
}

#[test]
fn skills_non_cosh_core_shows_unavailable_en() {
    let adapter = AdapterInstance::Fake(crate::adapter::FakeAgentAdapter);
    let mut state = en_state();
    let mut buf = Vec::new();
    render_skills_command(None, None, &adapter, &mut state, &mut buf).unwrap();
    let output = String::from_utf8(buf).unwrap();
    assert!(
        output.contains("cosh-core backend"),
        "should contain English degradation message: {output}"
    );
}

fn mock_core(body: &str) -> (AdapterInstance, std::path::PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let script = std::env::temp_dir().join(format!(
        "cosh-skills-slash-test-{}-{}.sh",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_file(&script);
    std::fs::write(&script, format!("#!/bin/sh\n{body}\n")).unwrap();
    let mut permissions = std::fs::metadata(&script).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&script, permissions).unwrap();
    let adapter =
        crate::adapter::CoshCoreAdapter::new(script.to_string_lossy().into_owned(), false);
    (AdapterInstance::CoshCore(adapter), script)
}

/// A description long enough that appending it to a list row would wrap over
/// several terminal lines, mixing scripts and embedding a newline.
const NOISY_DESC: &str = "Comprehensive skill for interacting with the Aone platform \
     通过 mcp2cli 命令行工具集与平台交互的综合技能\nsecond physical line of the description";

#[test]
fn skills_list_omits_description_regardless_of_length_or_script() {
    let data = serde_json::json!([
        { "name": "agentsight", "description": NOISY_DESC, "level": "system" },
        { "name": "example-skill", "description": NOISY_DESC, "level": "user", "disabled": true },
    ]);
    let lines = format_skills_list(&data, &I18n::new(Language::EnUs));
    assert_eq!(
        lines,
        vec![
            "  • agentsight [system]".to_string(),
            "  ○ example-skill [user] [disabled]".to_string(),
        ],
        "list rows must stay one short line each with no description"
    );
}

#[test]
fn skills_list_emits_exactly_one_line_per_skill() {
    let data = serde_json::json!([
        { "name": "a", "description": NOISY_DESC, "level": "system" },
        { "name": "b", "description": NOISY_DESC, "level": "project", "disabled": true },
        { "name": "c", "level": "user" },
    ]);
    let lines = format_skills_list(&data, &I18n::new(Language::EnUs));
    assert_eq!(lines.len(), 3, "one row per skill: {lines:?}");
    for line in &lines {
        assert!(!line.contains('\n'), "row must be a single line: {line:?}");
    }
}

#[test]
fn skills_list_keeps_level_fallback_and_skips_nameless_entries() {
    let data = serde_json::json!([
        { "name": "no-level" },
        { "description": "nameless entries are dropped" },
    ]);
    let lines = format_skills_list(&data, &I18n::new(Language::EnUs));
    assert_eq!(lines, vec!["  • no-level [?]".to_string()]);
}

#[test]
fn skills_list_empty_and_non_array_payloads_degrade() {
    let i18n = I18n::new(Language::EnUs);
    let empty = i18n.t(MessageId::SlashSkillsEmptyBody).to_string();
    assert_eq!(
        format_skills_list(&serde_json::json!([]), &i18n),
        vec![empty.clone()]
    );
    assert_eq!(
        format_skills_list(&serde_json::json!({ "unexpected": true }), &i18n),
        vec![empty.clone()]
    );
    assert_eq!(
        format_skills_list(&serde_json::Value::Null, &i18n),
        vec![empty]
    );
}

#[test]
fn skills_detail_still_shows_full_description() {
    let data = serde_json::json!({
        "name": "agentsight",
        "description": NOISY_DESC,
        "level": "system",
        "base_dir": "/tmp/skills/agentsight",
    });
    let lines = format_skill_detail(&data);
    assert!(
        lines.contains(&format!("  Description: {NOISY_DESC}")),
        "detail must keep the untruncated description: {lines:?}"
    );
}

#[test]
fn skills_list_rendering_does_not_leak_description_text() {
    let (adapter, script) = mock_core(
        r#"read REQUEST
case "$REQUEST" in
  *'"action":"list"'*'"domain":"skills"'*|*'"domain":"skills"'*'"action":"list"'*)
    printf '%s\n' '{"success":true,"data":[{"name":"agentsight","description":"DESCRIPTION_MUST_NOT_LEAK into the compact list row","level":"system"}]}'
    ;;
  *) printf '%s\n' '{"success":false,"error":"unexpected action"}' ;;
esac"#,
    );
    let mut state = en_state();
    let mut buf = Vec::new();
    render_skills_command(Some("list"), None, &adapter, &mut state, &mut buf).unwrap();
    let _ = std::fs::remove_file(script);
    let output = String::from_utf8(buf).unwrap();
    assert!(output.contains("agentsight"), "{output}");
    assert!(output.contains("system"), "{output}");
    assert!(!output.contains("DESCRIPTION_MUST_NOT_LEAK"), "{output}");
}

#[test]
fn completion_names_are_sorted_unique_and_enabled() {
    let data = serde_json::json!([
        {"name": "repo-review", "disabled": false},
        {"name": "disabled", "disabled": true},
        {"name": "release-notes"},
        {"name": "repo-review"}
    ]);
    assert_eq!(
        completion_skill_names(&data),
        vec!["release-notes".to_string(), "repo-review".to_string()]
    );
}

/// Pins the dispatch-layer cwd fallback: the Rust intercept path (zsh,
/// or bash with `COSH_SLASH_VIA_SHELL=0`) delivers events with
/// `cwd=None` because the input never reaches the shell, so
/// `render_slash_command` must fall back to the dispatcher-tracked
/// `shell_prompt_cwd` (last `ShellReady` report) when forwarding
/// `--workspace` to the registry subprocess. The adapter-layer
/// priority is covered by `registry_query_short_workspace_priority`.
#[test]
fn dispatch_falls_back_to_shell_prompt_cwd_when_event_cwd_missing() {
    let (adapter, script) = mock_core(
        r#"workspace=""
while [ $# -gt 0 ]; do
  case "$1" in
    --workspace) shift; workspace="${1:-}";;
  esac
  shift
done
IFS= read -r line || exit 1
printf '{"success":true,"data":[{"name":"ws:%s","level":"system"}]}\n' "$workspace"
exit 0"#,
    );

    // Post-ShellReady state: the dispatcher recorded the shell cwd
    // from the OSC 1337 report, but the intercept event itself has
    // no cwd to offer.
    let mut state = InlineState {
        language: Language::EnUs,
        shell_prompt_cwd: Some("/shell/prompt/cwd".to_string()),
        ..InlineState::default()
    };

    let event = ShellEvent::user_input_intercepted("test", "/skills list");
    let mut buf = Vec::new();
    let command = SlashCommand::Skills(Some("list"), None);

    // shell_cwd=None simulates the Rust intercept path.
    render_slash_command(command, &event, &[], &adapter, &mut state, None, &mut buf)
        .expect("render_slash_command should succeed");

    let _ = std::fs::remove_file(script);
    let output = String::from_utf8(buf).unwrap();

    assert!(
        output.contains("/shell/prompt/cwd"),
        "dispatch should fall back to shell_prompt_cwd when event cwd is None: {output}"
    );
}

/// After a valid cwd has been cached, a subsequent `cd` may have its
/// OSC 1337 markers lost. The dispatcher clears `shell_prompt_cwd`
/// on the next PTY write, so when an intercept event then arrives
/// with `cwd=None` the dispatch layer must clear the adapter cache
/// instead of reusing the stale `/repo-a` value.
#[test]
fn dispatch_clears_stale_shell_cwd_after_pty_invalidation() {
    let (adapter, script) = mock_core(
        r#"workspace=""
while [ $# -gt 0 ]; do
  case "$1" in
    --workspace) shift; workspace="${1:-}";;
  esac
  shift
done
IFS= read -r line || exit 1
printf '{"success":true,"data":[{"name":"ws:%s","level":"system"}]}\n' "$workspace"
exit 0"#,
    );

    // Seed the adapter with /repo-a as if an earlier slash command
    // in that directory ran successfully.
    if let crate::adapter::AdapterInstance::CoshCore(core) = &adapter {
        core.set_shell_cwd(Some("/repo-a"));
    }

    // Simulate the PTY-write invalidation that follows a marker-lost
    // `cd /repo-b`: the dispatcher no longer trusts shell_prompt_cwd.
    let mut state = InlineState {
        language: Language::EnUs,
        shell_prompt_cwd: None,
        ..InlineState::default()
    };

    let event = ShellEvent::user_input_intercepted("test", "/skills list");
    let mut buf = Vec::new();
    let command = SlashCommand::Skills(Some("list"), None);

    // Both event cwd and dispatcher prompt cwd are unavailable.
    render_slash_command(command, &event, &[], &adapter, &mut state, None, &mut buf)
        .expect("render_slash_command should succeed");

    let _ = std::fs::remove_file(script);

    if let crate::adapter::AdapterInstance::CoshCore(core) = &adapter {
        assert!(
            core.shell_cwd.lock().unwrap().is_none(),
            "stale shell_cwd should be cleared when both cwd sources are unavailable"
        );
    }
}
