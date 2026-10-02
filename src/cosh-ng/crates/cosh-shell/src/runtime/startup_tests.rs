use std::collections::BTreeMap;
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
use nix::pty::Winsize;

use super::{
    append_startup_auth_hint, bootstrap_path_probe_plan, extract_bootstrap_path,
    merge_bootstrap_paths, merge_path_additions_at_anchors, merge_path_lists,
    plan_startup_for_render, record_visible_personal_impressions,
    render_pending_recommendation_notice, startup_suggestion_mode, visible_personal_candidates,
    write_startup_suggestion_card, BootstrapPathProbeIo, RawShellKind, StartupSuggestionMode,
};
#[cfg(target_os = "linux")]
use super::{
    bootstrap_path_command, run_bootstrap_path_probe, BootstrapPathProbeError,
    BOOTSTRAP_PATH_PROBE_TIMEOUT,
};
use crate::config::Language;
use crate::diagnostics::health::{
    HealthCollector, HealthFinding, HealthFindingCategory, HealthMessageId, HealthScanReport,
    HealthSeverity, HealthTryItem, HealthTryKind, HealthUnavailableReason, UnavailableCollector,
};
use crate::recommendation::personal_feedback::FrozenPromptBinding;
use crate::recommendation::personal_model::{
    ActivityPayload, CandidateEvidenceSummary, CandidateSource, ContextAffinity, FeedbackAction,
    ScopeKind, DISCLOSURE_VERSION,
};
use crate::recommendation::personal_planner::{PlannerCandidate, PlannerContext};
use crate::recommendation::personal_runtime::PersonalRuntime;
use crate::runtime::state::{
    AnalysisMode, InlineState, PendingInputGhostBinding, StartupAuthState,
};
use crate::ui::RatatuiInlineRenderer;
use crate::I18n;

#[cfg(target_os = "linux")]
const BOOTSTRAP_PATH_TEST_WINSIZE: Winsize = Winsize {
    ws_row: 40,
    ws_col: 100,
    ws_xpixel: 0,
    ws_ypixel: 0,
};

#[test]
fn recommendation_notice_is_nonblocking_persisted_and_shown_once() {
    let root = std::env::temp_dir().join(format!(
        "cosh-startup-notice-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let writer = PersonalRuntime::open(true, &root, 1)
        .unwrap()
        .spawn_writer()
        .unwrap();
    wait_for_writer(|| writer.poll_snapshot());
    let mut state = InlineState {
        personalization: crate::recommendation::personal_state::PersonalizationState {
            writer: Some(writer),
            ..Default::default()
        },
        analysis_mode: AnalysisMode::Smart,
        ..InlineState::default()
    };
    let mut first = Vec::new();

    render_pending_recommendation_notice(&mut state, &mut first).unwrap();

    let text = String::from_utf8(first).unwrap();
    assert!(text.starts_with("\r\u{1b}[2K"), "{text:?}");
    assert!(text.contains("Prompt recommendations are on"));
    assert!(text.contains("current AI"));
    for hidden in ["gate4", "endpoint", "provider_id", "fingerprint"] {
        assert!(!text.contains(hidden));
    }
    assert!(state.personalization.notice_shown);
    assert!(state.trigger_pty_prompt);
    let persisted = wait_for_writer(|| {
        state
            .personalization
            .writer
            .as_ref()
            .and_then(|writer| writer.poll_snapshot())
            .filter(|snapshot| snapshot.preferences.notice_version_seen == DISCLOSURE_VERSION)
    });
    assert_eq!(
        persisted.preferences.notice_version_seen,
        crate::recommendation::personal_model::DISCLOSURE_VERSION
    );

    state.personalization.notice_shown = false;
    let mut second = Vec::new();
    render_pending_recommendation_notice(&mut state, &mut second).unwrap();
    assert!(second.is_empty());

    let mut writer = state.personalization.writer.take().unwrap();
    writer.shutdown(1, Duration::from_secs(5)).unwrap();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn recommendation_notice_waits_until_smart_or_auto_ai_mode() {
    let root = std::env::temp_dir().join(format!(
        "cosh-startup-notice-mode-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let writer = PersonalRuntime::open(true, &root, 1)
        .unwrap()
        .spawn_writer()
        .unwrap();
    let mut state = InlineState {
        personalization: crate::recommendation::personal_state::PersonalizationState {
            writer: Some(writer),
            ..Default::default()
        },
        analysis_mode: AnalysisMode::Manual,
        ..InlineState::default()
    };
    let mut output = Vec::new();

    render_pending_recommendation_notice(&mut state, &mut output).unwrap();
    assert!(output.is_empty());
    state.analysis_mode = AnalysisMode::Smart;
    state.personalization.ai_disabled = true;
    render_pending_recommendation_notice(&mut state, &mut output).unwrap();
    assert!(output.is_empty());

    let mut writer = state.personalization.writer.take().unwrap();
    writer.shutdown(1, Duration::from_secs(5)).unwrap();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn repeated_candidate_impressions_use_distinct_activity_identities() {
    let root = std::env::temp_dir().join(format!(
        "cosh-startup-impression-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut runtime = PersonalRuntime::open(true, &root, 1).unwrap();
    runtime.mark_notice_seen(DISCLOSURE_VERSION, 1).unwrap();
    let writer = runtime.spawn_writer().unwrap();
    let mut state = InlineState {
        personalization: crate::recommendation::personal_state::PersonalizationState {
            writer: Some(writer),
            ..Default::default()
        },
        ..InlineState::default()
    };
    for lifecycle in ["intent-1", "intent-2"] {
        state.pending_prompt_suggestion_bindings.insert(
            "candidate-1".to_string(),
            PendingInputGhostBinding::Personal(FrozenPromptBinding {
                candidate_id: "candidate-1".to_string(),
                task_ref: "task-1".to_string(),
                original_prompt: "continue payment investigation".to_string(),
                source: CandidateSource::RecentTask,
                suppression_key: "suppress-1".to_string(),
                profile_generation: 1,
                intent_lifecycle_id: lifecycle.to_string(),
            }),
        );
        record_visible_personal_impressions(&mut state, ".");
        state.pending_prompt_suggestion_bindings.clear();
    }

    let mut writer = state.personalization.writer.take().unwrap();
    writer.shutdown(1, Duration::from_secs(5)).unwrap();
    let snapshot = writer.poll_snapshot().unwrap();
    let impressions = snapshot
        .journal
        .records
        .iter()
        .filter(|record| {
            matches!(
                record.payload,
                ActivityPayload::RecommendationFeedback {
                    action: FeedbackAction::Impression,
                    ..
                }
            )
        })
        .count();
    assert_eq!(impressions, 2);
    let _ = std::fs::remove_dir_all(root);
}

fn wait_for_writer<T>(mut poll: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(value) = poll() {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "recommendation writer did not become observable before the fixture deadline"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn health_try(id: &str, score: i32) -> HealthTryItem {
    let prompt_id = match id {
        "health-a" => HealthMessageId::HealthTryAnalyzeMemoryPressure,
        "health-b" => HealthMessageId::HealthTryInspectDiskUsage,
        _ => HealthMessageId::HealthTryCheckSwapPressure,
    };
    HealthTryItem {
        id: id.to_string(),
        label_id: prompt_id,
        label_args: BTreeMap::new(),
        prompt_id: Some(prompt_id),
        prompt_args: BTreeMap::new(),
        kind: HealthTryKind::AskAgent,
        command: None,
        reason_id: HealthMessageId::HealthTryReasonMemoryLow,
        reason_args: BTreeMap::new(),
        score,
        finding_id: format!("finding-{id}"),
    }
}

fn personal(id: &str) -> PlannerCandidate {
    PlannerCandidate {
        candidate_id: id.to_string(),
        source: CandidateSource::RecentTask,
        task_ref: format!("task-{id}"),
        prompt_text: format!("continue {id}"),
        context_affinity: ContextAffinity {
            scope_kind: ScopeKind::Repo,
            repo_id: Some("repo-a".to_string()),
            host_id: Some("host-a".to_string()),
        },
        last_seen_hour_bucket: 10_000,
        evidence: CandidateEvidenceSummary {
            snapshot_ids: vec![format!("snapshot-{id}")],
            agent_request_count: 1,
            compatible_shell_count: 0,
            submitted_feedback_count: 0,
            intent_occurrence_count: 1,
            active_day_buckets: vec![1],
            continuation_evidence: false,
        },
        entities: Vec::new(),
        suppression_key: format!("suppress-{id}"),
        last_action_failed: false,
        consecutive_explicit_dismissals: 0,
        suppressed: false,
    }
}

fn planner_context() -> PlannerContext {
    PlannerContext {
        now_hour_bucket: 10_000,
        repo_id: Some("repo-a".to_string()),
        host_id: Some("host-a".to_string()),
    }
}

#[test]
fn startup_render_plan_suppresses_personal_when_health_did_not_resolve() {
    let rendered = plan_startup_for_render(
        I18n::new(Language::EnUs),
        None,
        &planner_context(),
        &[personal("recent")],
    );

    assert!(rendered.visible_candidates.is_empty());
    assert!(rendered.ghost_candidate.is_none());
}

#[test]
fn startup_render_plan_is_health_first_and_caps_actual_visible_at_three() {
    let mut report = HealthScanReport::new("health", 0);
    report.try_items = vec![health_try("health-a", 100), health_try("health-b", 90)];

    let rendered = plan_startup_for_render(
        I18n::new(Language::EnUs),
        Some(&report),
        &planner_context(),
        &[
            personal("recent-a"),
            personal("recent-b"),
            personal("z-omitted"),
        ],
    );

    assert_eq!(
        rendered
            .visible_candidates
            .iter()
            .map(|candidate| candidate.candidate_id.as_str())
            .collect::<Vec<_>>(),
        vec!["health-a", "health-b", "recent-a"]
    );
    assert_eq!(
        rendered
            .ghost_candidate
            .as_ref()
            .map(|candidate| candidate.candidate_id.as_str()),
        Some("health-a")
    );
    assert_eq!(
        visible_personal_candidates(&rendered)
            .iter()
            .map(|candidate| candidate.candidate_id.as_str())
            .collect::<Vec<_>>(),
        vec!["recent-a"]
    );
}

#[test]
fn suggestion_card_labels_health_before_personal_and_explains_all_keys() {
    let mut report = HealthScanReport::new("health", 0);
    report.try_items = vec![health_try("health-a", 100)];
    let rendered = plan_startup_for_render(
        I18n::new(Language::EnUs),
        Some(&report),
        &planner_context(),
        &[personal("recent-a"), personal("recent-b")],
    );
    let state = InlineState {
        language: Language::ZhCn,
        ..InlineState::default()
    };
    let mut output = Vec::new();

    write_startup_suggestion_card(
        &state,
        &RatatuiInlineRenderer::with_width(120).with_language(Language::ZhCn),
        StartupSuggestionMode::Interactive,
        &rendered.visible_candidates,
        &mut output,
    )
    .unwrap();

    let text = String::from_utf8(output).unwrap();
    assert!(text.find("[异常排查]").unwrap() < text.find("[个性化]").unwrap());
    assert!(text.contains("Shift+Tab 切换"));
    assert!(text.contains("Tab 填入"));
    assert!(text.contains("Enter 直接提问"));
    assert_eq!(rendered.visible_candidates.len(), 3);
}

#[test]
fn single_suggestion_hides_cycle_instruction() {
    let state = InlineState::default();
    let mut output = Vec::new();
    write_startup_suggestion_card(
        &state,
        &RatatuiInlineRenderer::with_width(120),
        StartupSuggestionMode::Interactive,
        &[personal("recent-a")],
        &mut output,
    )
    .unwrap();

    let text = String::from_utf8(output).unwrap();
    assert!(text.contains("Tab insert · Enter ask"));
    assert!(!text.contains("Shift+Tab cycle"));
}

#[test]
fn startup_suggestion_mode_separates_display_and_interaction_policy() {
    let healthy = HealthScanReport::new("healthy", 0);
    let mut warning = HealthScanReport::new("warning", 0);
    warning.overall_severity = HealthSeverity::Warning;
    let mut critical = HealthScanReport::new("critical", 0);
    critical.overall_severity = HealthSeverity::Critical;

    assert_eq!(
        startup_suggestion_mode(false, Some("xterm-256color"), &healthy),
        StartupSuggestionMode::Interactive
    );
    assert_eq!(
        startup_suggestion_mode(false, Some("xterm-256color"), &warning),
        StartupSuggestionMode::Interactive
    );
    assert_eq!(
        startup_suggestion_mode(false, Some("xterm-256color"), &critical),
        StartupSuggestionMode::Interactive
    );

    let degraded_gap = collection_gap_report(HealthSeverity::Degraded);
    let unavailable_gap = collection_gap_report(HealthSeverity::Unavailable);
    let warning_gap = collection_gap_report(HealthSeverity::Warning);
    let critical_gap = collection_gap_report(HealthSeverity::Critical);
    for report in [&degraded_gap, &unavailable_gap, &warning_gap, &critical_gap] {
        assert_eq!(
            startup_suggestion_mode(false, Some("xterm-256color"), report),
            StartupSuggestionMode::ReadOnly
        );
    }

    assert_eq!(
        startup_suggestion_mode(true, Some("xterm-256color"), &healthy),
        StartupSuggestionMode::Hidden
    );
    assert_eq!(
        startup_suggestion_mode(false, Some("dumb"), &healthy),
        StartupSuggestionMode::Hidden
    );

    for (collector, severity) in [
        (HealthCollector::Memory, HealthSeverity::Degraded),
        (HealthCollector::KernelSignal, HealthSeverity::Unavailable),
        (HealthCollector::ConfiguredService, HealthSeverity::Degraded),
    ] {
        let mut report = HealthScanReport::new("collector-unavailable", 0);
        report.unavailable.push(UnavailableCollector {
            collector,
            reason: HealthUnavailableReason::Timeout,
            severity,
            elapsed_ms: 0,
        });
        assert_eq!(
            startup_suggestion_mode(false, Some("xterm-256color"), &report),
            StartupSuggestionMode::ReadOnly
        );
    }
}

fn collection_gap_report(overall_severity: HealthSeverity) -> HealthScanReport {
    let mut report = HealthScanReport::new("collection-gap", 0);
    report.overall_severity = overall_severity;
    report.findings.push(HealthFinding {
        id: "collection-gap".to_string(),
        severity: HealthSeverity::Degraded,
        category: HealthFindingCategory::CollectionGap,
        title_id: HealthMessageId::HealthFindingCoreCollectorUnavailable,
        detail_id: None,
        detail_args: BTreeMap::new(),
        evidence_fact_ids: Vec::new(),
        suggested_try_ids: Vec::new(),
    });
    report
}

#[test]
fn startup_render_plan_keeps_three_health_prompts_at_narrow_width() {
    let mut report = HealthScanReport::new("health", 0);
    report.try_items = vec![
        health_try("health-a", 100),
        health_try("health-b", 90),
        health_try("health-c", 80),
    ];

    let rendered = plan_startup_for_render(
        I18n::new(Language::EnUs),
        Some(&report),
        &planner_context(),
        &[
            personal("recent-a"),
            personal("recent-b"),
            personal("z-omitted"),
        ],
    );

    assert_eq!(rendered.visible_candidates.len(), 3);
    assert_eq!(
        rendered
            .visible_candidates
            .iter()
            .map(|candidate| candidate.candidate_id.as_str())
            .collect::<Vec<_>>(),
        vec!["health-a", "health-b", "health-c"]
    );
    assert!(visible_personal_candidates(&rendered).is_empty());
}

#[test]
fn startup_renderer_planner_fixture_delta_stays_within_p95_p99_budget() {
    let mut report = HealthScanReport::new("health", 0);
    report.try_items = vec![health_try("health-a", 100)];
    let personal = vec![personal("recent-a"), personal("recent-b")];
    let context = planner_context();

    for _ in 0..32 {
        std::hint::black_box(plan_startup_for_render(
            I18n::new(Language::EnUs),
            Some(&report),
            &context,
            &personal,
        ));
    }

    let mut deltas = Vec::with_capacity(512);
    for _ in 0..512 {
        let baseline_started = std::time::Instant::now();
        std::hint::black_box(plan_startup_for_render(
            I18n::new(Language::EnUs),
            Some(&report),
            &context,
            &[],
        ));
        let baseline = baseline_started.elapsed();

        let personalized_started = std::time::Instant::now();
        std::hint::black_box(plan_startup_for_render(
            I18n::new(Language::EnUs),
            Some(&report),
            &context,
            &personal,
        ));
        deltas.push(personalized_started.elapsed().saturating_sub(baseline));
    }
    deltas.sort_unstable();
    let p95 = deltas[(deltas.len() * 95).div_ceil(100) - 1];
    let p99 = deltas[(deltas.len() * 99).div_ceil(100) - 1];

    eprintln!("startup renderer+planner delta p95={p95:?} p99={p99:?}");
    assert!(p95 <= std::time::Duration::from_millis(20), "p95={p95:?}");
    assert!(p99 <= std::time::Duration::from_millis(50), "p99={p99:?}");
}

#[test]
fn bootstrap_path_extracts_last_marked_value() {
    let text = "plugin noise\n__COSH_PATH_BEGIN__/a:/b__COSH_PATH_END__\n";
    assert_eq!(extract_bootstrap_path(text), Some("/a:/b".to_string()));
    assert_eq!(extract_bootstrap_path("plugin noise"), None);
}

#[test]
fn bootstrap_path_merge_keeps_existing_and_common_dirs() {
    assert_eq!(
        merge_path_lists(&[
            "/opt/homebrew/bin:/usr/bin:/bin",
            "/usr/local/bin:/bin",
            "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin",
        ]),
        "/opt/homebrew/bin:/usr/bin:/bin:/usr/local/bin:/usr/sbin:/sbin"
    );
}

#[test]
fn bash_non_login_merge_preserves_login_profile_precedence() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().unwrap();
    let inherited_dir = root.path().join("inherited");
    let prepend_dir = root.path().join("prepend");
    let append_dir = root.path().join("append");
    for directory in [&inherited_dir, &prepend_dir, &append_dir] {
        std::fs::create_dir(directory).unwrap();
        let command = directory.join("path-precedence-probe");
        std::fs::write(&command, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(command, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let inherited = format!("{}:/usr/bin:/bin", inherited_dir.display());

    for (login_path, expected) in [
        (
            format!("{}:{inherited}", prepend_dir.display()),
            prepend_dir.join("path-precedence-probe"),
        ),
        (
            format!("{inherited}:{}", append_dir.display()),
            inherited_dir.join("path-precedence-probe"),
        ),
        (
            format!("{}:/usr/local/bin:{inherited}", prepend_dir.display()),
            prepend_dir.join("path-precedence-probe"),
        ),
        (
            format!("/usr/local/bin:{inherited}:{}", append_dir.display()),
            inherited_dir.join("path-precedence-probe"),
        ),
    ] {
        let discovered = [Some(inherited.to_string()), Some(login_path.to_string())];
        let merged = merge_bootstrap_paths(&RawShellKind::Bash, false, &discovered, &inherited);
        let resolved = merged.split(':').find_map(|entry| {
            let candidate = std::path::Path::new(entry).join("path-precedence-probe");
            let metadata = candidate.metadata().ok()?;
            (metadata.permissions().mode() & 0o111 != 0).then_some(candidate)
        });

        assert_eq!(resolved.as_deref(), Some(expected.as_path()), "{merged}");
    }
}

#[test]
fn bash_non_login_merge_adds_fallbacks_after_login_paths() {
    for (base, login, expected) in [
        (
            "/usr/bin:/bin",
            "/login:/usr/local/bin:/usr/bin:/bin",
            "/login:/usr/local/bin:/usr/bin:/bin:/opt/homebrew/bin:/usr/sbin:/sbin",
        ),
        (
            "/usr/bin:/bin",
            "/usr/bin:/bin:/usr/local/bin:/login",
            "/usr/bin:/bin:/usr/local/bin:/login:/opt/homebrew/bin:/usr/sbin:/sbin",
        ),
        (
            "/usr/bin:/usr/local/bin:/bin",
            "/usr/bin:/login:/usr/local/bin:/bin",
            "/usr/bin:/login:/usr/local/bin:/bin:/opt/homebrew/bin:/usr/sbin:/sbin",
        ),
    ] {
        assert_eq!(
            merge_bootstrap_paths(
                &RawShellKind::Bash,
                false,
                &[Some(base.to_string()), Some(login.to_string())],
                base,
            ),
            expected,
            "base={base}; login={login}",
        );
    }
}

#[test]
fn login_path_additions_keep_internal_anchor_order() {
    assert_eq!(
        merge_path_additions_at_anchors(
            "/early:/usr/bin:/middle:/bin:/late",
            "/rc:/usr/bin:/bin:/fallback",
        ),
        "/rc:/early:/usr/bin:/middle:/bin:/late:/fallback"
    );
}

#[cfg(target_os = "linux")]
fn probe_bash_path(
    home: &std::path::Path,
    flags: &str,
    io: BootstrapPathProbeIo,
) -> Result<String, BootstrapPathProbeError> {
    let mut command = bootstrap_path_command("bash", flags);
    command
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin")
        .env_remove("BASH_ENV")
        .env_remove("ENV");
    run_bootstrap_path_probe(
        command,
        BOOTSTRAP_PATH_PROBE_TIMEOUT,
        io,
        &BOOTSTRAP_PATH_TEST_WINSIZE,
    )
}

#[test]
#[cfg(target_os = "linux")]
fn bash_non_login_probe_reads_bashrc_then_interactive_login_path() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join(".bashrc"),
        "export PATH=\"$HOME/rc-only:$PATH\"\n",
    )
    .unwrap();
    std::fs::write(
        home.path().join(".bash_profile"),
        "case $- in *i*) [ -t 0 ] && [ -t 1 ] && \
         export PATH=\"$HOME/login-only:$PATH\";; esac\n",
    )
    .unwrap();
    let (_, probes) = bootstrap_path_probe_plan(&RawShellKind::Bash, false, true).unwrap();

    let discovered = probes
        .iter()
        .map(|probe| Some(probe_bash_path(home.path(), probe.flags, probe.io).unwrap()))
        .collect::<Vec<_>>();
    let merged = merge_bootstrap_paths(&RawShellKind::Bash, false, &discovered, "/usr/bin:/bin");
    let rc = format!("{}/rc-only", home.path().display());
    let login = format!("{}/login-only", home.path().display());

    assert!(merged.split(':').any(|entry| entry == rc));
    assert!(merged.split(':').any(|entry| entry == login));
    // Both startup sources must beat the inherited command directory during
    // bootstrap. The managed non-login Bash sources .bashrc again afterward,
    // so .bashrc still owns the final interactive-session ordering.
    let inherited = merged.find("/usr/bin").unwrap();
    assert!(merged.find(&login).unwrap() < inherited);
    assert!(merged.find(&rc).unwrap() < inherited);
    assert_eq!(probes[0].flags, "-ic");
    assert_eq!(probes[0].io, BootstrapPathProbeIo::Pipes);
    assert_eq!(probes[1].flags, "-lic");
    assert_eq!(probes[1].io, BootstrapPathProbeIo::Pty);
}

#[test]
#[cfg(target_os = "linux")]
fn bash_login_probe_uses_standard_profile_precedence() {
    let home = tempfile::tempdir().unwrap();
    for (file, entry) in [
        (".bash_profile", "bash-profile"),
        (".bash_login", "bash-login"),
        (".profile", "profile"),
    ] {
        std::fs::write(
            home.path().join(file),
            format!("export PATH=\"$HOME/{entry}:$PATH\"\n"),
        )
        .unwrap();
    }

    let profile_path = probe_bash_path(home.path(), "-lic", BootstrapPathProbeIo::Pty).unwrap();
    assert!(profile_path.contains("/bash-profile:"), "{profile_path}");
    assert!(!profile_path.contains("/bash-login:"), "{profile_path}");
    assert!(!profile_path.contains("/profile:"), "{profile_path}");

    std::fs::remove_file(home.path().join(".bash_profile")).unwrap();
    let bash_login_path = probe_bash_path(home.path(), "-lic", BootstrapPathProbeIo::Pty).unwrap();
    assert!(
        bash_login_path.contains("/bash-login:"),
        "{bash_login_path}"
    );
    assert!(!bash_login_path.contains("/profile:"), "{bash_login_path}");

    std::fs::remove_file(home.path().join(".bash_login")).unwrap();
    let profile_path = probe_bash_path(home.path(), "-lic", BootstrapPathProbeIo::Pty).unwrap();
    assert!(profile_path.contains("/profile:"), "{profile_path}");
}

#[test]
fn bootstrap_path_probe_plan_honors_disabled_switch() {
    assert!(bootstrap_path_probe_plan(&RawShellKind::Bash, false, false).is_none());
    assert!(bootstrap_path_probe_plan(&RawShellKind::Bash, true, false).is_none());
    assert!(bootstrap_path_probe_plan(&RawShellKind::Zsh, false, false).is_none());
}

#[test]
fn bootstrap_path_probe_plan_preserves_login_and_zsh_modes() {
    let (_, bash_login) = bootstrap_path_probe_plan(&RawShellKind::Bash, true, true).unwrap();
    assert_eq!(bash_login.len(), 1);
    assert_eq!(bash_login[0].flags, "-lic");
    assert_eq!(bash_login[0].io, BootstrapPathProbeIo::Pipes);

    let (_, zsh_non_login) = bootstrap_path_probe_plan(&RawShellKind::Zsh, false, true).unwrap();
    assert_eq!(zsh_non_login.len(), 1);
    assert_eq!(zsh_non_login[0].flags, "-ic");
    assert_eq!(zsh_non_login[0].io, BootstrapPathProbeIo::Pipes);
    let (_, zsh_login) = bootstrap_path_probe_plan(&RawShellKind::Zsh, true, true).unwrap();
    assert_eq!(zsh_login.len(), 1);
    assert_eq!(zsh_login[0].flags, "-lic");
    assert_eq!(zsh_login[0].io, BootstrapPathProbeIo::Pipes);
}

#[test]
#[cfg(target_os = "linux")]
fn bash_login_probe_reports_startup_failure() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join(".bash_profile"), "exit 23\n").unwrap();

    let error = probe_bash_path(home.path(), "-lic", BootstrapPathProbeIo::Pty).unwrap_err();
    assert!(matches!(
        error,
        BootstrapPathProbeError::Failed(status) if status.code() == Some(23)
    ));
}

#[test]
#[cfg(target_os = "linux")]
fn bash_login_probe_timeout_preserves_non_login_path() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join(".bashrc"),
        "export PATH=\"$HOME/rc-only:$PATH\"\n",
    )
    .unwrap();
    std::fs::write(home.path().join(".bash_profile"), "sleep 30\n").unwrap();

    let non_login = probe_bash_path(home.path(), "-ic", BootstrapPathProbeIo::Pipes).unwrap();
    let mut login_command = bootstrap_path_command("bash", "-lic");
    login_command
        .env("HOME", home.path())
        .env("PATH", "/usr/bin:/bin")
        .env_remove("BASH_ENV")
        .env_remove("ENV");
    let started = Instant::now();
    let error = run_bootstrap_path_probe(
        login_command,
        Duration::from_millis(50),
        BootstrapPathProbeIo::Pty,
        &BOOTSTRAP_PATH_TEST_WINSIZE,
    )
    .unwrap_err();

    assert!(matches!(
        error,
        BootstrapPathProbeError::TimedOut(timeout) if timeout == Duration::from_millis(50)
    ));
    assert!(started.elapsed() < Duration::from_secs(2));
    let discovered = [Some(non_login), None];
    let merged = merge_bootstrap_paths(&RawShellKind::Bash, false, &discovered, "/usr/bin:/bin");
    let rc = format!("{}/rc-only", home.path().display());
    assert!(merged.split(':').any(|entry| entry == rc));
    assert!(merged.find(&rc).unwrap() < merged.find("/usr/bin").unwrap());
}

#[test]
fn startup_auth_hint_appends_only_when_probe_reports_unconfigured() {
    let mut state = InlineState::default();
    let mut body = vec!["/help".to_string()];

    // Unresolved probe stays quiet.
    append_startup_auth_hint(&mut state, &mut body);
    assert_eq!(body.len(), 1);

    // Configured credentials stay quiet.
    state.startup_auth.resolved = Some(true);
    append_startup_auth_hint(&mut state, &mut body);
    assert_eq!(body.len(), 1);

    // Explicitly unconfigured credentials add the /auth hint line.
    state.startup_auth.resolved = Some(false);
    append_startup_auth_hint(&mut state, &mut body);
    assert_eq!(body.len(), 3);
    assert_eq!(body[1], "");
    assert!(body[2].contains("/auth"), "{:?}", body[2]);
    assert!(body[2].contains("AI not configured"), "{:?}", body[2]);

    // AI disabled by config suppresses the hint even when unconfigured.
    let mut disabled = InlineState::default();
    disabled.personalization.ai_disabled = true;
    disabled.startup_auth.resolved = Some(false);
    let mut quiet = Vec::new();
    append_startup_auth_hint(&mut disabled, &mut quiet);
    assert!(quiet.is_empty());
}

#[test]
fn startup_auth_state_resolves_bounded_and_fails_quiet() {
    // Result arrives within the wait budget.
    let mut ready = StartupAuthState::default();
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    ready.pending = Some(receiver);
    sender.send(Some(false)).unwrap();
    ready.wait_ready(Duration::from_millis(150));
    assert!(ready.ai_unconfigured());
    assert!(ready.pending.is_none());

    // Timeout leaves the probe unresolved without consuming it.
    let mut slow = StartupAuthState::default();
    let (slow_sender, receiver) = std::sync::mpsc::sync_channel(1);
    slow.pending = Some(receiver);
    slow.wait_ready(Duration::from_millis(10));
    assert!(!slow.ai_unconfigured());
    assert!(slow.pending.is_some());
    // A late result is still picked up by a later non-blocking poll.
    slow_sender.send(Some(false)).unwrap();
    slow.poll_ready();
    assert!(slow.ai_unconfigured());

    // A dropped probe (failed thread) resolves to quiet, not to a hint.
    let mut dropped = StartupAuthState::default();
    let (broken_sender, receiver) = std::sync::mpsc::sync_channel::<Option<bool>>(1);
    dropped.pending = Some(receiver);
    drop(broken_sender);
    dropped.wait_ready(Duration::from_millis(150));
    assert!(!dropped.ai_unconfigured());
    assert!(dropped.pending.is_none());
}

#[test]
fn recommendation_notice_is_suppressed_while_auth_is_unconfigured() {
    let root = std::env::temp_dir().join(format!(
        "cosh-startup-notice-noauth-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let writer = PersonalRuntime::open(true, &root, 1)
        .unwrap()
        .spawn_writer()
        .unwrap();
    wait_for_writer(|| writer.poll_snapshot());
    let mut state = InlineState {
        personalization: crate::recommendation::personal_state::PersonalizationState {
            writer: Some(writer),
            ..Default::default()
        },
        analysis_mode: AnalysisMode::Smart,
        ..InlineState::default()
    };
    state.startup_auth.resolved = Some(false);

    let mut suppressed = Vec::new();
    render_pending_recommendation_notice(&mut state, &mut suppressed).unwrap();
    assert!(suppressed.is_empty());
    // The first-time disclosure is preserved for a later configured startup.
    assert!(!state.personalization.notice_shown);

    state.startup_auth.resolved = Some(true);
    let mut shown = Vec::new();
    render_pending_recommendation_notice(&mut state, &mut shown).unwrap();
    let text = String::from_utf8(shown).unwrap();
    assert!(text.contains("Prompt recommendations are on"));
    assert!(state.personalization.notice_shown);

    let mut writer = state.personalization.writer.take().unwrap();
    writer.shutdown(1, Duration::from_secs(5)).unwrap();
    let _ = std::fs::remove_dir_all(root);
}
