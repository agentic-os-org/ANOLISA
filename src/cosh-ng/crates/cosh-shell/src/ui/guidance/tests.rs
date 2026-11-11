use super::*;
use crate::diagnostics::health::{
    HealthFact, HealthFactCategory, HealthFactSource, HealthFactValue, HealthMessageId,
    HealthSeverity, HealthTryItem, HealthTryKind,
};
use crate::insight::guidance::{CommandIdentity, ExecutionScope};
use std::collections::BTreeMap;

#[test]
fn startup_entry_has_only_the_shell_and_guide_actions() {
    for language in [Language::EnUs, Language::ZhCn] {
        let text = login_entry(language).text;
        assert_eq!(text.lines().count(), 1, "{text}");
        assert!(text.contains("cosh guide"), "{text}");
        assert!(
            !text.contains(" off") && !text.contains("login set"),
            "{text}"
        );
    }
}

#[test]
fn health_and_failure_actions_share_the_same_styled_heading() {
    for language in [Language::ZhCn, Language::EnUs] {
        for width in [40, 80, 100] {
            let candidate = health_candidate(&report(), language).unwrap();
            let failure = question_text("Analyze this failure", language).unwrap();
            for styled in [false, true] {
                let mut renderer =
                    RatatuiInlineRenderer::with_width(width).with_language(language);
                renderer.styled = styled;
                let health =
                    render_health(&renderer, &report(), true, Target::Bash, Some(&candidate))
                        .unwrap();
                let failure = style_invitation(&failure, styled);
                let heading = failure.lines().next().unwrap();
                assert!(health.lines().any(|line| line == heading), "{health:?}");
                let plain = crate::ui::wrap::strip_ansi_escape(&health);
                assert!(plain.lines().any(|line| line == candidate.text));
                if styled {
                    assert!(heading.contains("┃ Cosh AI"));
                } else {
                    assert!(!health.contains('\u{1b}') && !heading.contains('┃'));
                }
            }
        }
    }
}

#[test]
fn health_action_uses_a_panel_without_stacking_shutdown_instructions() {
    for language in [Language::EnUs, Language::ZhCn] {
        let candidate = health_candidate(&report(), language).unwrap();
        let renderer = RatatuiInlineRenderer::with_width(80).with_language(language);
        let text =
            render_health(&renderer, &report(), true, Target::Bash, Some(&candidate)).unwrap();
        let heading = if language == Language::ZhCn {
            "Cosh AI · 排查建议 · 复制执行"
        } else {
            "Cosh AI · Investigate · Copy and run"
        };
        assert!(text.contains(heading), "{text}");
        assert!(text.contains("cosh --ask"), "{text}");
        let action =
            render_health(&renderer, &report(), false, Target::Bash, Some(&candidate)).unwrap();
        assert!(!action.contains('╭') && !action.contains('╰'), "{action}");
        assert!(
            !text.contains("guide off") && !text.contains("banner off"),
            "{text}"
        );
    }
}

#[test]
fn health_action_keeps_the_complete_command_copyable_at_narrow_widths() {
    let mut report = report();
    report.try_items[0]
        .prompt_args
        .insert("mount".into(), format!("/data/{}", "x".repeat(140)));
    let candidate = health_candidate(&report, Language::EnUs).unwrap();
    for renderer in [
        RatatuiInlineRenderer::with_width(40),
        RatatuiInlineRenderer::plain_with_width(40),
    ] {
        let text = render_health(&renderer, &report, true, Target::Bash, Some(&candidate)).unwrap();
        assert!(
            text.contains(&candidate.text),
            "command acquired hard wraps or border characters: {text}"
        );
    }
}

fn report() -> HealthScanReport {
    let mut report = HealthScanReport::new("projection-fixture", 1000);
    report.overall_severity = HealthSeverity::Warning;
    report.facts.push(HealthFact {
        id: "fixture-fact".to_string(),
        elapsed_ms: 0,
        key: "filesystem.root_used_ratio".to_string(),
        category: HealthFactCategory::Disk,
        value: HealthFactValue::Float(0.92),
        source: HealthFactSource::DfP,
        unit: None,
    });
    for (id, prompt, score) in [
        ("disk", HealthMessageId::HealthPromptInspectDiskUsage, 100),
        (
            "memory",
            HealthMessageId::HealthPromptAnalyzeMemoryPressure,
            50,
        ),
    ] {
        report.try_items.push(HealthTryItem {
            id: id.to_string(),
            label_id: HealthMessageId::HealthTryInspectDiskUsage,
            label_args: BTreeMap::new(),
            prompt_id: Some(prompt),
            prompt_args: BTreeMap::new(),
            kind: HealthTryKind::AskAgent,
            command: None,
            reason_id: HealthMessageId::HealthTryReasonDiskHigh,
            reason_args: BTreeMap::new(),
            score,
            finding_id: String::new(),
        });
    }
    report
}

#[test]
fn health_projection_reuses_one_layout_and_preserves_report_facts_and_candidates() {
    let report = report();
    let original = report.clone();
    let candidate = health_candidate(&report, Language::EnUs).unwrap();
    let renderer = RatatuiInlineRenderer::with_width(80);
    let text = render_health(&renderer, &report, true, Target::Bash, Some(&candidate)).unwrap();
    assert_eq!(text.matches("cosh --ask").count(), 1);
    assert!(text.contains("92%"), "{text}");
    let suppressed = render_health(&renderer, &report, true, Target::Bash, None).unwrap();
    assert!(suppressed.contains("92%"));
    assert!(suppressed.contains("cosh guide"));
    assert!(!suppressed.contains("cosh --ask"));
    assert_eq!(report, original);
}

#[test]
fn cosh_projection_uses_the_same_question_without_a_nested_cosh_command() {
    let report = report();
    let bash = health_candidate_for(&report, Language::EnUs, Target::Bash).unwrap();
    let cosh = health_candidate_for(&report, Language::EnUs, Target::Cosh).unwrap();
    let question =
        crate::ui::primary_health_prompt_suggestion(&report, crate::I18n::new(Language::EnUs))
            .unwrap();
    assert_eq!(bash.problem, cosh.problem);
    assert!(cosh.text.contains(&question));
    assert!(!cosh.text.contains("cosh --ask"));
    assert!(bash.text.contains("cosh --ask"));
}

#[test]
fn narrow_plain_projection_has_no_ansi_or_box_and_does_not_invent_invitations() {
    let report = report();
    let renderer = RatatuiInlineRenderer::plain_with_width(30);
    let text = render_health(&renderer, &report, true, Target::Bash, None).unwrap();
    assert!(!text.contains('\x1b'));
    assert!(!text.contains('╭'));
    assert!(text.contains("cosh guide"));
    assert!(!text.contains("cosh --ask"));
}

#[test]
fn a_report_without_completed_facts_does_not_claim_that_the_system_is_healthy() {
    let report = HealthScanReport::new("not-checked", 1000);
    let text = render_health(
        &RatatuiInlineRenderer::plain_with_width(80),
        &report,
        true,
        Target::Bash,
        None,
    )
    .unwrap();
    assert!(!text.to_lowercase().contains(": ok"), "{text}");
}

#[test]
fn very_narrow_plain_projection_stays_within_the_actual_column_limit() {
    let renderer = RatatuiInlineRenderer::plain_with_width(10).within_columns(10);
    let text = render_health(&renderer, &report(), true, Target::Bash, None).unwrap();
    for line in text.lines() {
        assert!(crate::ui::display_width(line) <= 10, "{line:?}");
    }
}

#[test]
fn terminal_control_data_is_escaped_without_mutating_the_original_report() {
    let mut report = report();
    report.facts.push(HealthFact {
        id: "fixture-fact".to_string(),
        elapsed_ms: 0,
        key: "host.os_pretty_name".to_string(),
        category: HealthFactCategory::Host,
        value: HealthFactValue::String("OS\x1b[2J\rspoof".to_string()),
        source: HealthFactSource::OsRelease,
        unit: None,
    });
    let original = report.clone();
    let text = render_health(
        &RatatuiInlineRenderer::plain_with_width(60),
        &report,
        true,
        Target::Bash,
        None,
    )
    .unwrap();
    assert!(!text.contains('\x1b'));
    assert!(!text.contains('\r'));
    assert_eq!(report, original);
}

#[test]
fn banner_off_projection_can_show_only_an_independently_claimed_entry() {
    let entry = login_entry(Language::ZhCn);
    let text = render_health(
        &RatatuiInlineRenderer::plain_with_width(80),
        &report(),
        false,
        Target::Bash,
        Some(&entry),
    )
    .unwrap();
    assert!(text.contains("cosh"));
    assert!(!text.contains("92%"));
    assert!(!text.contains("cosh --ask"));
}

#[test]
fn default_login_invitation_has_a_standalone_primary_command() {
    for language in [Language::EnUs, Language::ZhCn] {
        let invitation = default_login(language);
        assert_eq!(invitation.problem, "default-login");
        assert_eq!(invitation.text.lines().count(), 3);
        assert!(invitation.text.contains("cosh login set"));
        assert!(invitation.text.contains("cosh login restore"));
    }
}

#[test]
fn failure_question_uses_a_short_explicit_reference_without_copying_diagnostic_policy() {
    let id = "1234567890abcdef1234567890abcdef";
    for language in [Language::EnUs, Language::ZhCn] {
        let event = BashEvent {
            expression: "cargo build".into(),
            status: 101,
            cwd: "/project".into(),
            observed_at_ms: 1000,
            scope: ExecutionScope::ObservedCommandFragment,
            identity: CommandIdentity::ExternalUnshadowed,
        };
        let text = failure_invitation(&event, FailureKind::CargoBuild, language, id)
            .unwrap()
            .text;
        assert!(text.contains(&format!("--context {id}")));
        assert!(!text.contains("cargo build") && !text.contains("101"));
        assert!(!text.contains("粘贴") && !text.contains("paste") && !text.contains("cosh guide"));
        assert_eq!(text.lines().count(), 2);
        assert!(crate::ui::display_width(text.lines().nth(1).unwrap()) <= 80);
    }
}

#[test]
fn cosh_projection_preserves_existing_read_only_and_service_questions_without_expanding_bash() {
    for prompt in [
        HealthMessageId::HealthPromptReviewUnavailableChecks,
        HealthMessageId::HealthPromptInspectServiceStatus,
    ] {
        let mut report = report();
        report.try_items.truncate(1);
        report.try_items[0].prompt_id = Some(prompt);
        assert!(health_candidate_for(&report, Language::EnUs, Target::Cosh).is_some());
        assert!(health_candidate_for(&report, Language::EnUs, Target::Bash).is_none());
    }
}

#[test]
fn failure_invitation_has_one_short_question_without_repeating_the_startup_guide() {
    let event = BashEvent {
        expression: "cargo test".into(),
        status: 101,
        cwd: "/project".into(),
        observed_at_ms: 1000,
        scope: ExecutionScope::ObservedCommandFragment,
        identity: CommandIdentity::ExternalUnshadowed,
    };
    for language in [Language::EnUs, Language::ZhCn] {
        let text = failure_invitation(
            &event,
            FailureKind::CargoTest,
            language,
            "1234567890abcdef1234567890abcdef",
        )
        .unwrap()
        .text;
        let lines = text.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("Cosh"), "{text}");
        assert!(lines[1].starts_with("  cosh --ask "));
        let limit = if language == Language::ZhCn { 96 } else { 120 };
        assert!(crate::ui::display_width(lines[1]) <= limit, "{}", lines[1]);
        assert!(!text.contains("cosh guide"));
        assert!(!text.contains("summary setting") && !text.contains("资源摘要独立"));
    }
}

#[test]
fn user_facing_health_questions_name_the_object_without_assuming_a_remedy() {
    let mut report = report();
    report.try_items[0]
        .prompt_args
        .insert("mount".into(), "/data".into());
    for language in [Language::EnUs, Language::ZhCn] {
        let text = health_candidate(&report, language).unwrap().text;
        assert!(text.contains("/data"), "{text}");
        let selected = crate::diagnostics::health::startup_prompt_suggestions(
            &report,
            crate::I18n::new(language),
            1,
        );
        assert!(text.contains(&selected[0].1), "{text}");
        assert!(!text.contains("cleanup targets") && !text.contains("清理目标"));
        assert!(!text.contains("health banner off"));
    }
}

#[test]
fn common_health_questions_fit_a_standard_terminal_without_losing_the_object() {
    for language in [Language::EnUs, Language::ZhCn] {
        for prompt in [
            HealthMessageId::HealthPromptAnalyzeMemoryPressure,
            HealthMessageId::HealthPromptCheckSwapPressure,
            HealthMessageId::HealthPromptCheckRecentOom,
            HealthMessageId::HealthPromptInspectDiskUsage,
            HealthMessageId::HealthPromptInspectHighLoad,
            HealthMessageId::HealthPromptInspectProcessMemory,
        ] {
            let mut report = report();
            report.try_items.truncate(1);
            report.try_items[0].prompt_id = Some(prompt);
            report.try_items[0]
                .prompt_args
                .insert("mount".into(), "/data".into());
            report.try_items[0]
                .prompt_args
                .insert("process".into(), "worker".into());
            let bash = health_candidate_for(&report, language, Target::Bash).unwrap();
            let cosh = health_candidate_for(&report, language, Target::Cosh).unwrap();
            assert!(crate::ui::display_width(&bash.text) <= 96, "{}", bash.text);
            let question = cosh.text.lines().nth(1).unwrap();
            assert!(bash.text.contains(question), "{}", bash.text);
        }
    }
}

#[test]
fn localization_actions_are_standalone_and_secondary_copy_is_short() {
    for language in [Language::ZhCn, Language::EnUs] {
        let entry = login_entry(language);
        assert_eq!(entry.text.lines().count(), 1);
        assert!(
            entry.text.contains("cosh") && entry.text.contains("cosh guide"),
            "{}",
            entry.text
        );
        let login = default_login(language);
        assert!(
            login.text.lines().any(|line| line == "  cosh login set"),
            "{}",
            login.text
        );
        let ask = health_candidate(&report(), language).unwrap();
        assert!(
            ask.text
                .lines()
                .any(|line| line.starts_with("  cosh --ask ")),
            "{}",
            ask.text
        );
        assert!(!entry.text.contains("AI invitations"));
        assert!(!entry.text.contains("AI 使用邀请"));
        assert!(!ask.text.contains('\x1b'));
    }
}

#[test]
fn localization_failure_uses_the_same_user_setting_as_login_and_cosh() {
    let _guard = crate::diagnostics::test_env::env_guard();
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir(home.path().join(".copilot-shell")).unwrap();
    std::fs::write(
        home.path().join(".copilot-shell/config.toml"),
        "[ui]\nlanguage='zh-CN'\n",
    )
    .unwrap();
    let names = ["HOME", "COSH_SHELL_LANG", "LC_ALL", "LC_MESSAGES", "LANG"];
    let saved = names.map(|name| (name, std::env::var_os(name)));
    std::env::set_var("HOME", home.path());
    std::env::remove_var("COSH_SHELL_LANG");
    std::env::set_var("LC_ALL", "en_US.UTF-8");
    let event = BashEvent {
        expression: "cargo build".into(),
        status: 101,
        cwd: "/project".into(),
        observed_at_ms: 1,
        scope: ExecutionScope::ObservedCommandFragment,
        identity: CommandIdentity::ExternalUnshadowed,
    };
    let configured = render_failure(
        &event,
        FailureKind::CargoBuild,
        "1234567890abcdef1234567890abcdef",
    );
    std::env::set_var("COSH_SHELL_LANG", "en-US");
    let overridden = render_failure(
        &event,
        FailureKind::CargoBuild,
        "1234567890abcdef1234567890abcdef",
    );
    std::env::remove_var("COSH_SHELL_LANG");
    let path = home.path().join(".copilot-shell/config.toml");
    let target = home.path().join("linked.toml");
    std::fs::rename(&path, &target).unwrap();
    std::os::unix::fs::symlink(&target, &path).unwrap();
    let unsafe_config = render_failure(
        &event,
        FailureKind::CargoBuild,
        "1234567890abcdef1234567890abcdef",
    );
    for (name, value) in saved {
        match value {
            Some(value) => std::env::set_var(name, value),
            None => std::env::remove_var(name),
        }
    }
    assert!(configured.unwrap().contains("分析这次失败"));
    assert!(overridden.unwrap().contains("Analyze this failure"));
    assert!(unsafe_config.is_none());
}

#[test]
fn native_cosh_health_suggests_a_question_instead_of_copying_a_shell_command() {
    for language in [Language::ZhCn, Language::EnUs] {
        let candidate = health_candidate_for(&report(), language, Target::Cosh).unwrap();
        let renderer = RatatuiInlineRenderer::with_width(80).with_language(language);
        let text =
            render_health(&renderer, &report(), true, Target::Cosh, Some(&candidate)).unwrap();
        assert!(
            text.contains(if language == Language::ZhCn {
                "可以直接提问"
            } else {
                "Try asking"
            }),
            "{text}"
        );
        for action in ["复制执行", "copy and run", "cosh --ask"] {
            assert!(!text.contains(action), "{text}");
        }
    }
}

#[test]
fn startup_entry_highlights_both_actions_without_coloring_the_whole_line() {
    for language in [Language::ZhCn, Language::EnUs] {
        let mut renderer = RatatuiInlineRenderer::with_width(80).with_language(language);
        renderer.styled = true;
        let text = render_health(
            &renderer,
            &report(),
            false,
            Target::Bash,
            Some(&login_entry(language)),
        )
        .unwrap();
        assert!(text.contains("\x1b[36;1mcosh\x1b[0m"), "{text:?}");
        assert!(text.contains("\x1b[36;1mcosh guide\x1b[0m"), "{text:?}");
        assert!(crate::ui::display_width(&login_entry(language).text) <= 76);
        assert_eq!(text.lines().count(), 1);
    }
}

#[test]
fn health_guide_is_an_explicit_command_inside_the_card() {
    for language in [Language::ZhCn, Language::EnUs] {
        for width in [40, 80, 100] {
            let renderer = RatatuiInlineRenderer::with_width(width).with_language(language);
            let report = report();
            for invitation in [
                None,
                Some(login_entry(language)),
                health_candidate(&report, language),
            ] {
                let text =
                    render_health(&renderer, &report, true, Target::Bash, invitation.as_ref())
                        .unwrap();
                let guide = text
                    .lines()
                    .find(|line| line.contains("cosh guide"))
                    .unwrap();
                assert!(guide.starts_with('│') && guide.ends_with('│'), "{text}");
                assert!(
                    guide.contains(if language == Language::ZhCn {
                        "输入 cosh guide"
                    } else {
                        "Type cosh guide"
                    }),
                    "{text}"
                );
                assert_eq!(text.matches("cosh guide").count(), 1, "{text}");
            }
        }
    }
}

#[test]
fn health_actions_align_with_card_content_without_wrapping_the_command() {
    for language in [Language::ZhCn, Language::EnUs] {
        let renderer = RatatuiInlineRenderer::with_width(80).with_language(language);
        let report = report();
        for invitation in [
            login_entry(language),
            health_candidate(&report, language).unwrap(),
        ] {
            let text =
                render_health(&renderer, &report, true, Target::Bash, Some(&invitation)).unwrap();
            let lines = text.lines().collect::<Vec<_>>();
            let bottom = lines.iter().position(|line| line.starts_with('╰')).unwrap();
            assert!(text.ends_with("\n\n"), "{text}");
            if invitation.problem == "login-entry" {
                let card = lines[..=bottom].join("\n");
                assert!(
                    card.contains(if language == Language::ZhCn {
                        "输入 cosh 开始 AI 问答"
                    } else {
                        "Type cosh to ask AI"
                    }),
                    "{text}"
                );
                assert!(
                    lines[bottom + 1..].iter().all(|line| line.is_empty()),
                    "{text}"
                );
            } else {
                assert!(lines[bottom + 1].starts_with("  "), "{text}");
                assert_eq!(lines[bottom + 2], invitation.text, "{text}");
            }
        }
    }
}

#[test]
fn health_action_highlights_only_the_command_prefix() {
    for language in [Language::ZhCn, Language::EnUs] {
        for width in [40, 80, 100] {
            let mut renderer = RatatuiInlineRenderer::with_width(width).with_language(language);
            renderer.styled = true;
            let candidate = health_candidate(&report(), language).unwrap();
            let text =
                render_health(&renderer, &report(), true, Target::Bash, Some(&candidate)).unwrap();
            assert!(text.contains("  \x1b[36;1mcosh --ask\x1b[0m "), "{text:?}");
            assert!(
                !text.contains(&format!("\x1b[1m{}", candidate.text)),
                "{text:?}"
            );
        }
    }
}

#[test]
fn native_health_has_no_duplicate_guide_with_or_without_an_invitation() {
    for language in [Language::ZhCn, Language::EnUs] {
        for width in [40, 80, 100] {
            for renderer in [
                RatatuiInlineRenderer::with_width(width),
                RatatuiInlineRenderer::plain_with_width(width),
            ] {
                let renderer = renderer.with_language(language);
                let report = report();
                for invitation in [None, health_candidate_for(&report, language, Target::Cosh)] {
                    let text =
                        render_health(&renderer, &report, true, Target::Cosh, invitation.as_ref())
                            .unwrap();
                    assert!(!text.contains("cosh guide"), "{text}");
                    assert!(!text.contains("cosh --ask"), "{text}");
                    assert!(text.contains("92%"), "{text}");
                }
            }
        }
    }
}

#[test]
fn bash_can_offer_the_same_oom_process_question_as_cosh() {
    let mut report = report();
    report.try_items.truncate(1);
    report.try_items[0].prompt_id = Some(HealthMessageId::HealthPromptInspectProcessMemory);
    report.try_items[0]
        .prompt_args
        .insert("process".into(), "java".into());
    for language in [Language::ZhCn, Language::EnUs] {
        let bash = health_candidate_for(&report, language, Target::Bash);
        let cosh = health_candidate_for(&report, language, Target::Cosh).unwrap();
        assert!(
            bash.is_some(),
            "Cosh question has no Bash entry: {}",
            cosh.text
        );
        let bash = bash.unwrap();
        assert!(bash.text.starts_with("  cosh --ask "));
        assert!(bash.text.contains("java") && cosh.text.contains("java"));
    }
}

#[test]
fn failure_style_separates_the_invitation_without_boxing_the_copyable_command() {
    for language in [Language::ZhCn, Language::EnUs] {
        let plain = question_text("Show errors; don't rerun.", language).unwrap();
        assert_eq!(style_invitation(&plain, false), plain);
        let styled = style_invitation(&plain, true);
        let unstyled = crate::ui::wrap::strip_ansi_escape(&styled);
        let lines = unstyled.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("┃ Cosh AI  "), "{unstyled}");
        assert!(!lines[0].contains("──"));
        assert!(crate::ui::display_width(lines[0]) <= 40);
        assert!(styled.contains("\x1b[36;1m┃ Cosh AI\x1b[0m"));
        assert!(!styled.contains("\x1b[2m"));
        assert_eq!(lines[1], plain.lines().nth(1).unwrap());
        assert!(!unstyled.contains('│') && !unstyled.contains('╭'));
        assert!(styled.contains("\x1b[36;1mcosh --ask\x1b[0m $'"));
        assert!(!styled.contains("cosh guide"));
        assert!(!styled.contains("\x1b[90m"), "{styled:?}");
    }
}

#[test]
fn failure_style_preserves_a_long_quoted_question_as_one_logical_line() {
    let question = format!("{}; don't rerun.", "请核实错误 ".repeat(30));
    let plain = question_text(&question, Language::ZhCn).unwrap();
    let styled = style_invitation(&plain, true);
    let unstyled = crate::ui::wrap::strip_ansi_escape(&styled);
    assert_eq!(unstyled.lines().nth(1), plain.lines().nth(1));
    assert_eq!(unstyled.lines().count(), 2);
}
