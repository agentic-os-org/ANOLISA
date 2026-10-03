//! Tests for the `/recommendations` slash command, kept out of the
//! implementation file per the shell test-organization guideline.

use super::recommendations::{
    privacy_lines, render_recommendations_command, render_status_lines, RecommendationReadiness,
    RecommendationStatusView,
};
use crate::adapter::{AdapterInstance, FakeAgentAdapter};
use crate::config::Language;
use crate::i18n::I18n;

#[test]
fn status_uses_user_language_and_hides_technical_fields() {
    let lines = render_status_lines(
        &I18n::new(Language::ZhCn),
        &RecommendationStatusView {
            readiness: RecommendationReadiness::ReadyWithProfile,
            bash_history: false,
        },
    );
    let text = lines.join("\n");

    assert!(text.contains("近期 Shell 与 Agent"));
    assert!(text.contains("Bash history：未纳入"));
    for hidden in [
        "gate4",
        "endpoint",
        "fingerprint",
        "小时桶",
        "容量",
        "错误数",
    ] {
        assert!(!text.contains(hidden));
    }
}

#[test]
fn privacy_explains_sources_retention_and_current_ai_boundary() {
    let lines = privacy_lines(&I18n::new(Language::ZhCn));
    let text = lines.join("\n");

    assert!(lines.iter().all(|line| line.chars().count() <= 55));

    for required in [
        "Shell 命令",
        "Agent 请求",
        "Bash history",
        "Pod",
        "当前 AI 服务",
        "7 天",
        "14 天",
        "90 天",
    ] {
        assert!(text.contains(required), "missing privacy text: {required}");
    }
    for hidden in ["gate4", "http://", "https://", "provider_id"] {
        assert!(!text.contains(hidden));
    }
}

#[test]
fn off_panel_points_to_mode_analysis_for_failure_insights() {
    let root = std::env::temp_dir().join(format!(
        "cosh-slash-off-hint-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let runtime =
        crate::recommendation::personal_runtime::PersonalRuntime::open(true, &root, 1).unwrap();
    let mut state = crate::runtime::state::InlineState {
        personalization: crate::recommendation::personal_state::PersonalizationState {
            store_root: Some(root.clone()),
            configured_enabled: true,
            writer: Some(runtime.spawn_writer().unwrap()),
            ..Default::default()
        },
        ..crate::runtime::state::InlineState::default()
    };
    let mut output = Vec::new();

    render_recommendations_command(
        Some("off"),
        None,
        None,
        &crate::types::ShellEvent::user_input_intercepted("test-session", "/recommendations off"),
        &AdapterInstance::Fake(FakeAgentAdapter),
        &mut state,
        &mut output,
    )
    .unwrap();

    let raw = String::from_utf8(output).unwrap();
    let normalized = raw
        .replace(['│', '╭', '╮', '╰', '╯', '─'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        normalized
            .contains("Prompt recommendations are off and local recommendation data was cleared."),
        "panel output: {normalized}"
    );
    assert!(
        normalized.contains(
            "Command failure insights are controlled separately with /mode analysis manual."
        ),
        "panel output: {normalized}"
    );

    let mut writer = state.personalization.writer.take().unwrap();
    writer
        .shutdown(1, std::time::Duration::from_secs(1))
        .unwrap();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn off_panel_hint_stays_accurate_in_manual_mode() {
    // In Manual mode failure insights are already silenced, so the hint
    // must stay state-agnostic instead of claiming insights are on.
    let root = std::env::temp_dir().join(format!(
        "cosh-slash-off-hint-manual-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let runtime =
        crate::recommendation::personal_runtime::PersonalRuntime::open(true, &root, 1).unwrap();
    let mut state = crate::runtime::state::InlineState {
        analysis_mode: crate::runtime::state::AnalysisMode::Manual,
        personalization: crate::recommendation::personal_state::PersonalizationState {
            store_root: Some(root.clone()),
            configured_enabled: true,
            writer: Some(runtime.spawn_writer().unwrap()),
            ..Default::default()
        },
        ..crate::runtime::state::InlineState::default()
    };
    let mut output = Vec::new();

    render_recommendations_command(
        Some("off"),
        None,
        None,
        &crate::types::ShellEvent::user_input_intercepted("test-session", "/recommendations off"),
        &AdapterInstance::Fake(FakeAgentAdapter),
        &mut state,
        &mut output,
    )
    .unwrap();

    let raw = String::from_utf8(output).unwrap();
    let normalized = raw
        .replace(['│', '╭', '╮', '╰', '╯', '─'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        normalized.contains(
            "Command failure insights are controlled separately with /mode analysis manual."
        ),
        "panel output: {normalized}"
    );
    assert!(
        !normalized.contains("stay on"),
        "panel output: {normalized}"
    );

    let mut writer = state.personalization.writer.take().unwrap();
    writer
        .shutdown(1, std::time::Duration::from_secs(1))
        .unwrap();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn clear_recovers_corrupt_state_without_retaining_quarantine_payloads() {
    use std::os::unix::fs::PermissionsExt;

    let root = std::env::temp_dir().join(format!(
        "cosh-slash-recover-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let store = crate::recommendation::personal_store::PersonalStore::open(&root).unwrap();
    store.initialize(1).unwrap();
    std::fs::write(root.join("state.json"), b"broken").unwrap();
    std::fs::set_permissions(
        root.join("state.json"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let _ = std::fs::remove_file(root.join("state.backup.json"));
    let mut state = crate::runtime::state::InlineState {
        personalization: crate::recommendation::personal_state::PersonalizationState {
            store_root: Some(root.clone()),
            configured_enabled: true,
            ..Default::default()
        },
        ..crate::runtime::state::InlineState::default()
    };
    let mut output = Vec::new();

    render_recommendations_command(
        Some("clear"),
        None,
        None,
        &crate::types::ShellEvent::user_input_intercepted("test-session", "/recommendations clear"),
        &AdapterInstance::Fake(FakeAgentAdapter),
        &mut state,
        &mut output,
    )
    .unwrap();

    assert!(String::from_utf8(output)
        .unwrap()
        .contains("Damaged recommendation data"));
    assert!(
        !state
            .personalization
            .writer
            .as_ref()
            .unwrap()
            .poll_status()
            .unwrap()
            .enabled
    );
    assert!(!root.join("state.quarantine").exists());
    let mut writer = state.personalization.writer.take().unwrap();
    writer
        .shutdown(1, std::time::Duration::from_secs(1))
        .unwrap();
    let _ = std::fs::remove_dir_all(root);
}
