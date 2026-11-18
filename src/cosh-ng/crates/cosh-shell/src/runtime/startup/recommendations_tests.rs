use super::*;

#[test]
fn first_disclosure_is_two_short_lines_with_controls() {
    let lines = recommendation_notice_lines(&I18n::new(Language::ZhCn));

    assert_eq!(lines.len(), 2);
    assert!(lines.iter().all(|line| line.chars().count() <= 70));
    let text = lines.join("\n");
    assert!(text.contains("/recommendations off"));
    assert!(text.contains("/recommendations privacy"));
}

#[test]
fn review_regression_first_disclosure_limits_redaction_guarantees() {
    for (language, required) in [
        (Language::ZhCn, ["尝试", "常见凭证", "不能保证"]),
        (
            Language::EnUs,
            ["attempt", "common credentials", "cannot guarantee"],
        ),
    ] {
        let text = recommendation_notice_lines(&I18n::new(language)).join("\n");
        for term in required {
            assert!(text.contains(term), "missing {term}: {text}");
        }
        assert!(text.contains("/recommendations off"));
        assert!(text.contains("/recommendations privacy"));
    }
}
#[test]
fn a38_candidate_content_baseline_is_frozen() {
    use crate::diagnostics::health::{
        HealthFinding, HealthFindingCategory, HealthMessageId, HealthScanReport, HealthSeverity,
        HealthTryItem, HealthTryKind,
    };
    use std::os::unix::fs::PermissionsExt;
    let _guard = crate::diagnostics::test_env::env_guard();
    let home = tempfile::tempdir().unwrap();
    std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let saved: Vec<(&str, Option<std::ffi::OsString>)> = [
        "HOME",
        "TERM",
        "COSH_SHELL_ISOLATED",
        "COSH_SHELL_HEALTH_SUPPRESSION_STORE",
    ]
    .iter()
    .map(|n| (*n, std::env::var_os(n)))
    .collect();
    std::env::set_var("HOME", home.path().canonicalize().unwrap());
    std::env::set_var("TERM", "xterm-256color");
    std::env::remove_var("COSH_SHELL_ISOLATED");
    std::env::remove_var("COSH_SHELL_HEALTH_SUPPRESSION_STORE");
    let mut report = HealthScanReport::new("a38-content", 1000);
    report.overall_severity = HealthSeverity::Warning;
    report.findings.push(HealthFinding {
        id: "J06".to_string(),
        severity: HealthSeverity::Warning,
        category: HealthFindingCategory::RootCause,
        title_id: HealthMessageId::HealthFindingMemoryAvailableLow,
        detail_id: None,
        detail_args: Default::default(),
        evidence_fact_ids: vec![],
        suggested_try_ids: vec!["worker-memory".to_string()],
    });
    report.try_items.push(HealthTryItem {
        id: "worker-memory".to_string(),
        label_id: HealthMessageId::HealthTryAnalyzeMemoryPressure,
        label_args: Default::default(),
        prompt_id: Some(HealthMessageId::HealthPromptAnalyzeMemoryPressure),
        prompt_args: Default::default(),
        kind: HealthTryKind::AskAgent,
        command: None,
        reason_id: HealthMessageId::HealthTryReasonMemoryLow,
        reason_args: Default::default(),
        score: 100,
        finding_id: "J06".to_string(),
    });
    let mut state = InlineState {
        language: Language::EnUs,
        ..Default::default()
    };
    state.startup_auth.resolved = Some(true);
    state.startup_health.report = Some(report);
    let prepared = prepare_startup_suggestions(&mut state, "/tmp");
    let rows: Vec<String> = prepared
        .candidates
        .iter()
        .map(|c| format!("{:?}|{}|{}", c.source, c.candidate_id, c.prompt_text))
        .collect();
    for (n, v) in saved {
        match v {
            Some(v) => std::env::set_var(n, v),
            None => std::env::remove_var(n),
        }
    }
    // A38 candidate-content oracle: with a fresh suppression store (temp HOME),
    // Interactive mode and an injected AskAgent health try item, the rendered
    // candidate id and i18n prompt text must stay byte-identical when startup
    // preparation is moved out of the input_mode critical section.
    assert_eq!(prepared.mode, StartupSuggestionMode::Interactive);
    assert_eq!(
        rows,
        vec![
            "Health|worker-memory|Help me check memory pressure, top consumers and cgroup limits"
                .to_string()
        ],
        "A38 startup candidate content drifted"
    );
}

#[test]
fn a38_baseline_startup_suggestion_mode_matrix_is_frozen() {
    let _guard = crate::diagnostics::test_env::env_guard();
    let names = ["COSH_SHELL_ISOLATED", "TERM"];
    let saved = names.map(|name| (name, std::env::var_os(name)));
    std::env::set_var("TERM", "xterm-256color");
    let mut rows = Vec::new();
    for isolated in [false, true] {
        if isolated {
            std::env::set_var("COSH_SHELL_ISOLATED", "1");
        } else {
            std::env::remove_var("COSH_SHELL_ISOLATED");
        }
        for auth in [None, Some(true), Some(false)] {
            for cwd in ["/tmp", "/home/user/project"] {
                let mut state = InlineState::default();
                state.startup_auth.resolved = auth;
                state.startup_health.report = Some(HealthScanReport::new("a38-baseline", 1000));
                let prepared = prepare_startup_suggestions(&mut state, cwd);
                rows.push(format!(
                    "{isolated}|{auth:?}|{cwd}|{:?}|{}",
                    prepared.mode,
                    prepared.candidates.len()
                ));
            }
        }
    }
    for (name, value) in saved {
        match value {
            Some(value) => std::env::set_var(name, value),
            None => std::env::remove_var(name),
        }
    }
    // A38 oracle (mode/suppression behavior): the startup-suggestion mode
    // matrix for controlled auth/isolation/cwd inputs must stay byte-identical
    // when startup preparation is moved out of the input_mode critical section.
    // Candidate-content equivalence with realistic personalization data and the
    // real-terminal first-keypress latency oracle are set up in the A38 fix increment.
    let expected = [
        "false|None|/tmp|Interactive|0",
        "false|None|/home/user/project|Interactive|0",
        "false|Some(true)|/tmp|Interactive|0",
        "false|Some(true)|/home/user/project|Interactive|0",
        "false|Some(false)|/tmp|Hidden|0",
        "false|Some(false)|/home/user/project|Hidden|0",
        "true|None|/tmp|Hidden|0",
        "true|None|/home/user/project|Hidden|0",
        "true|Some(true)|/tmp|Hidden|0",
        "true|Some(true)|/home/user/project|Hidden|0",
        "true|Some(false)|/tmp|Hidden|0",
        "true|Some(false)|/home/user/project|Hidden|0",
    ];
    assert_eq!(rows, expected, "A38 startup-suggestion mode matrix drifted");
}

#[test]
fn known_unconfigured_auth_hides_suggestions_without_blocking_unknown_auth() {
    let _guard = crate::diagnostics::test_env::env_guard();
    let names = ["COSH_SHELL_ISOLATED", "TERM"];
    let saved = names.map(|name| (name, std::env::var_os(name)));
    std::env::remove_var("COSH_SHELL_ISOLATED");
    std::env::set_var("TERM", "xterm-256color");
    let mut modes = Vec::new();
    for auth in [None, Some(true), Some(false)] {
        let mut state = InlineState::default();
        state.startup_auth.resolved = auth;
        state.startup_health.report = Some(HealthScanReport::new("auth-gate", 1000));
        let prepared = prepare_startup_suggestions(&mut state, "/tmp");
        modes.push(prepared.mode);
        assert!(state.startup_health.report.is_some());
    }
    for (name, value) in saved {
        match value {
            Some(value) => std::env::set_var(name, value),
            None => std::env::remove_var(name),
        }
    }
    assert_ne!(modes[0], StartupSuggestionMode::Hidden);
    assert_ne!(modes[1], StartupSuggestionMode::Hidden);
    assert_eq!(modes[2], StartupSuggestionMode::Hidden);
}
