use crate::runtime::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RecommendationPresentation {
    Suppressed {
        reason: &'static str,
    },
    SummaryOnly {
        summary: String,
    },
    InsightNextStep {
        summary: String,
        command: String,
    },
    Legacy {
        summary: Option<String>,
        commands: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecommendationNormalization {
    pub(crate) presentation: RecommendationPresentation,
    pub(crate) audit_reasons: Vec<&'static str>,
}

pub(crate) fn normalize_recommendations(
    origin: AgentRunOrigin,
    governed_events: &[GovernedEvent],
) -> RecommendationNormalization {
    let recommendations = governed_events.iter().filter_map(|event| {
        if matches!(event.decision, GovernanceDecision::Rejected) {
            return None;
        }
        match &event.event {
            AgentEvent::Recommendation {
                summary, commands, ..
            } => Some((summary.as_str(), commands.as_slice())),
            _ => None,
        }
    });

    let mut summaries = Vec::new();
    let mut commands = Vec::new();
    let mut audit_reasons = Vec::new();
    let mut saw_recommendation = false;
    for (summary, event_commands) in recommendations {
        saw_recommendation = true;
        let summary = summary.trim();
        if !summary.is_empty() && !summaries.iter().any(|existing| existing == summary) {
            summaries.push(summary.to_string());
        }
        if origin.is_insight_triggered() && summary.is_empty() && !event_commands.is_empty() {
            push_unique_reason(&mut audit_reasons, "recommendation_summary_missing");
            continue;
        }
        for command in event_commands {
            let command = command.trim();
            if !command.is_empty() && !commands.iter().any(|existing| existing == command) {
                commands.push(command.to_string());
            }
        }
    }

    if !saw_recommendation {
        return RecommendationNormalization {
            presentation: RecommendationPresentation::Suppressed {
                reason: "no_recommendation_events",
            },
            audit_reasons,
        };
    }

    if !origin.is_insight_triggered() {
        return RecommendationNormalization {
            presentation: if commands.is_empty() {
                RecommendationPresentation::Suppressed {
                    reason: "no_recommendation_commands",
                }
            } else {
                RecommendationPresentation::Legacy {
                    summary: (!summaries.is_empty()).then(|| summaries.join("\n\n")),
                    commands,
                }
            },
            audit_reasons,
        };
    }

    if summaries.is_empty() {
        push_unique_reason(&mut audit_reasons, "recommendation_summary_missing");
        return RecommendationNormalization {
            presentation: RecommendationPresentation::Suppressed {
                reason: "recommendation_summary_missing",
            },
            audit_reasons,
        };
    }

    let summary = summaries.join("\n\n");
    let presentation = match commands.as_slice() {
        [] => RecommendationPresentation::SummaryOnly { summary },
        [command] => RecommendationPresentation::InsightNextStep {
            summary,
            command: command.clone(),
        },
        _ => {
            push_unique_reason(
                &mut audit_reasons,
                "multiple_commands_require_structured_result",
            );
            RecommendationPresentation::SummaryOnly { summary }
        }
    };
    RecommendationNormalization {
        presentation,
        audit_reasons,
    }
}

fn push_unique_reason(reasons: &mut Vec<&'static str>, reason: &'static str) {
    if !reasons.contains(&reason) {
        reasons.push(reason);
    }
}

pub(crate) fn render_selection_actions<W: Write>(
    events: &[ShellEvent],
    state: &mut InlineState,
    output: &mut W,
    event_index_base: usize,
) -> std::io::Result<()> {
    for (idx, event) in events.iter().enumerate() {
        let event_index = event_index_base + idx;
        let Some(action) = recommendation_action_from_event(event) else {
            continue;
        };

        let key = format!("select-{event_index}");
        if !state.handled_selections.insert(key) {
            continue;
        }

        if state
            .control
            .selectable_commands_available_after()
            .map(|available_after| event_index <= available_after)
            .unwrap_or(true)
            || !state.control.has_selectable_commands()
        {
            let i18n = state.i18n();
            render_recommendation_unavailable(
                state.language,
                i18n.t(MessageId::RecommendationNoSelectableTitle),
                vec![i18n
                    .t(MessageId::RecommendationNoSelectableBody)
                    .to_string()],
                output,
            )?;
            output.flush()?;
            continue;
        }

        let Some(command) = state.control.selectable_command(action.index - 1) else {
            let i18n = state.i18n();
            let index = action.index.to_string();
            let total = state.control.selectable_command_count().to_string();
            render_recommendation_unavailable(
                state.language,
                i18n.t(MessageId::RecommendationUnavailableTitle),
                vec![i18n.format(
                    MessageId::RecommendationUnavailableBody,
                    &[("index", index.as_str()), ("total", total.as_str())],
                )],
                output,
            )?;
            output.flush()?;
            continue;
        };

        render_recommendation_action(state.language, action.kind, action.index, command, output)?;
        output.flush()?;
    }

    Ok(())
}

fn render_recommendation_action<W: Write>(
    language: Language,
    kind: RecommendationActionKind,
    index: usize,
    command: &str,
    output: &mut W,
) -> std::io::Result<()> {
    let renderer = RatatuiInlineRenderer::for_terminal().with_language(language);
    let i18n = I18n::new(language);
    let index = index.to_string();
    let (title, primary_id, message_id) = match kind {
        RecommendationActionKind::Select => (
            MessageId::RecommendationSelectedTitle,
            MessageId::RecommendationSelectedBody,
            MessageId::RecommendationDisplayOnlyBody,
        ),
        RecommendationActionKind::Copy => (
            MessageId::RecommendationCopiedTitle,
            MessageId::RecommendationCopiedBody,
            MessageId::RecommendationCopyOnlyBody,
        ),
        RecommendationActionKind::Insert => (
            MessageId::RecommendationInsertTitle,
            MessageId::RecommendationInsertBody,
            MessageId::RecommendationInsertOnlyBody,
        ),
        RecommendationActionKind::Details => (
            MessageId::RecommendationDetailsTitle,
            MessageId::RecommendationDetailsBody,
            MessageId::RecommendationDetailsOnlyBody,
        ),
    };
    renderer.write_recommendation_action_panel(
        output,
        RecommendationActionPanelModel {
            title: i18n.t(title),
            primary: i18n.format(primary_id, &[("index", index.as_str())]),
            command: Some(command),
            message: i18n.t(message_id),
        },
    )?;
    Ok(())
}

fn render_recommendation_unavailable<W: Write>(
    language: Language,
    title: &str,
    body: Vec<String>,
    output: &mut W,
) -> std::io::Result<()> {
    RatatuiInlineRenderer::for_terminal()
        .with_language(language)
        .write_notice_panel(
            output,
            NoticePanelModel {
                title,
                body,
                footer: None,
            },
        )
}

pub(crate) fn record_selectable_recommendations(
    state: &mut InlineState,
    governed_events: &[GovernedEvent],
    origin: AgentRunOrigin,
    selectable_after_event_index: Option<usize>,
) {
    let commands =
        selectable_commands_from_normalization(&normalize_recommendations(origin, governed_events));
    if commands.is_empty() {
        return;
    }

    state
        .control
        .remember_selectable_commands(commands, selectable_after_event_index);
}

pub(crate) fn render_selectable_recommendations<W: Write>(
    governed_events: &[GovernedEvent],
    origin: AgentRunOrigin,
    language: Language,
    output: &mut W,
) -> std::io::Result<()> {
    let normalized = normalize_recommendations(origin, governed_events);
    for reason in &normalized.audit_reasons {
        tracing::debug!(origin = ?origin, reason, "recommendation presentation normalized");
    }
    let renderer = RatatuiInlineRenderer::for_terminal().with_language(language);
    let i18n = I18n::new(language);
    match &normalized.presentation {
        RecommendationPresentation::Suppressed { reason } => {
            tracing::debug!(origin = ?origin, reason, "recommendation presentation suppressed");
        }
        RecommendationPresentation::SummaryOnly { summary } => {
            renderer.write_notice_panel(
                output,
                NoticePanelModel {
                    title: i18n.t(MessageId::AnalysisResultTitle),
                    body: vec![summary.clone()],
                    footer: None,
                },
            )?;
        }
        RecommendationPresentation::InsightNextStep { summary, command } => {
            renderer.write_recommendation_panel(
                output,
                RecommendationPanelModel {
                    title: i18n.t(MessageId::RecommendationNextStepTitle),
                    summary: Some(summary),
                    commands: std::slice::from_ref(command),
                },
            )?;
        }
        RecommendationPresentation::Legacy { summary, commands } => {
            renderer.write_recommendation_panel(
                output,
                RecommendationPanelModel {
                    title: i18n.t(MessageId::RecommendationTitle),
                    summary: summary.as_deref(),
                    commands,
                },
            )?;
        }
    }
    Ok(())
}

fn selectable_commands_from_normalization(normalized: &RecommendationNormalization) -> Vec<String> {
    match &normalized.presentation {
        RecommendationPresentation::InsightNextStep { command, .. } => vec![command.clone()],
        RecommendationPresentation::Legacy { commands, .. } => commands.clone(),
        RecommendationPresentation::Suppressed { .. }
        | RecommendationPresentation::SummaryOnly { .. } => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recommendation_event(
        summary: &str,
        commands: &[&str],
        decision: GovernanceDecision,
    ) -> GovernedEvent {
        GovernedEvent {
            decision,
            policy_decision: GovernancePolicyDecision::DisplayOnly,
            event: AgentEvent::Recommendation {
                run_id: "run-1".to_string(),
                summary: summary.to_string(),
                commands: commands
                    .iter()
                    .map(|command| (*command).to_string())
                    .collect(),
                auto_execute: true,
            },
            reason: "test".to_string(),
            display_text: String::new(),
            auto_execute: false,
        }
    }

    #[test]
    fn insight_recommendation_requires_a_summary() {
        let events = vec![recommendation_event(
            "  ",
            &["pwd"],
            GovernanceDecision::Degraded,
        )];

        let normalized = normalize_recommendations(AgentRunOrigin::InsightPrompt, &events);

        assert_eq!(
            normalized.presentation,
            RecommendationPresentation::Suppressed {
                reason: "recommendation_summary_missing"
            }
        );
        assert_eq!(
            normalized.audit_reasons,
            vec!["recommendation_summary_missing"]
        );
    }

    #[test]
    fn insight_run_without_recommendation_has_no_malformed_audit() {
        let normalized = normalize_recommendations(AgentRunOrigin::InsightPrompt, &[]);

        assert_eq!(
            normalized.presentation,
            RecommendationPresentation::Suppressed {
                reason: "no_recommendation_events"
            }
        );
        assert!(normalized.audit_reasons.is_empty());
    }

    #[test]
    fn insight_recommendation_normalizes_zero_one_and_multiple_commands_per_run() {
        let summary_only = normalize_recommendations(
            AgentRunOrigin::AutoFailure,
            &[recommendation_event(
                "Disk pressure is high.",
                &[],
                GovernanceDecision::Display,
            )],
        );
        assert_eq!(
            summary_only.presentation,
            RecommendationPresentation::SummaryOnly {
                summary: "Disk pressure is high.".to_string()
            }
        );

        let one = normalize_recommendations(
            AgentRunOrigin::InsightPrompt,
            &[
                recommendation_event(
                    " Disk pressure is high. ",
                    &[" df -h ", "df -h"],
                    GovernanceDecision::Degraded,
                ),
                recommendation_event(
                    "Disk pressure is high.",
                    &["df -h"],
                    GovernanceDecision::Display,
                ),
            ],
        );
        assert_eq!(
            one.presentation,
            RecommendationPresentation::InsightNextStep {
                summary: "Disk pressure is high.".to_string(),
                command: "df -h".to_string(),
            }
        );

        let multiple = normalize_recommendations(
            AgentRunOrigin::InsightPrompt,
            &[
                recommendation_event("First finding.", &["df -h"], GovernanceDecision::Display),
                recommendation_event(
                    "Second finding.",
                    &["du -sh ."],
                    GovernanceDecision::Display,
                ),
            ],
        );
        assert_eq!(
            multiple.presentation,
            RecommendationPresentation::SummaryOnly {
                summary: "First finding.\n\nSecond finding.".to_string()
            }
        );
        assert_eq!(
            multiple.audit_reasons,
            vec!["multiple_commands_require_structured_result"]
        );
    }

    #[test]
    fn insight_command_cannot_borrow_another_events_summary() {
        let normalized = normalize_recommendations(
            AgentRunOrigin::InsightPrompt,
            &[
                recommendation_event("Evidence is incomplete.", &[], GovernanceDecision::Display),
                recommendation_event("", &["pwd"], GovernanceDecision::Display),
            ],
        );

        assert_eq!(
            normalized.presentation,
            RecommendationPresentation::SummaryOnly {
                summary: "Evidence is incomplete.".to_string()
            }
        );
        assert_eq!(
            normalized.audit_reasons,
            vec!["recommendation_summary_missing"]
        );
    }

    #[test]
    fn standard_recommendation_keeps_empty_summary_commands_only_compatibility() {
        let events = vec![
            recommendation_event("", &["pwd", "pwd"], GovernanceDecision::Display),
            recommendation_event("ignored", &["do-not-show"], GovernanceDecision::Rejected),
            recommendation_event("", &["echo $PATH"], GovernanceDecision::Degraded),
        ];

        let normalized = normalize_recommendations(AgentRunOrigin::Standard, &events);

        assert_eq!(
            normalized.presentation,
            RecommendationPresentation::Legacy {
                summary: None,
                commands: vec!["pwd".to_string(), "echo $PATH".to_string()],
            }
        );
        assert!(normalized.audit_reasons.is_empty());
    }

    #[test]
    fn insight_next_step_renders_summary_one_command_and_no_fake_buttons() {
        let events = vec![recommendation_event(
            "Disk pressure is high.",
            &["df -h"],
            GovernanceDecision::Degraded,
        )];
        let mut output = Vec::new();

        render_selectable_recommendations(
            &events,
            AgentRunOrigin::AutoFailure,
            Language::EnUs,
            &mut output,
        )
        .expect("render insight recommendation");

        let output = String::from_utf8(output).expect("utf8 recommendation");
        assert!(output.contains("Suggested next step"), "{output}");
        assert!(output.contains("Disk pressure is high."), "{output}");
        assert!(output.contains("df -h"), "{output}");
        assert!(output.contains("no command was executed"), "{output}");
        assert!(!output.contains("[Copy]"), "{output}");
        assert!(!output.contains("[Insert]"), "{output}");
    }

    #[test]
    fn insight_multiple_commands_render_one_summary_only_owner() {
        let events = vec![
            recommendation_event("First finding.", &["df -h"], GovernanceDecision::Display),
            recommendation_event(
                "Second finding.",
                &["du -sh ."],
                GovernanceDecision::Display,
            ),
        ];
        let mut state = InlineState::default();
        let mut output = Vec::new();

        record_selectable_recommendations(&mut state, &events, AgentRunOrigin::InsightPrompt, None);
        render_selectable_recommendations(
            &events,
            AgentRunOrigin::InsightPrompt,
            Language::EnUs,
            &mut output,
        )
        .expect("render summary only");

        let output = String::from_utf8(output).expect("utf8 summary");
        assert!(output.contains("Analysis result"), "{output}");
        assert!(output.contains("First finding."), "{output}");
        assert!(output.contains("Second finding."), "{output}");
        assert!(!output.contains("df -h"), "{output}");
        assert!(!output.contains("du -sh ."), "{output}");
        assert!(!state.control.has_selectable_commands());
    }

    fn recommendation_card_event(index: usize, message: &str) -> ShellEvent {
        let mut event = ShellEvent::user_input_intercepted("session-1", index.to_string());
        event.component = Some("card".to_string());
        event.message = Some(message.to_string());
        event
    }

    #[test]
    fn card_insert_renders_pending_prompt_guidance_without_executing() {
        let mut state = InlineState::default();
        state
            .control
            .remember_selectable_commands(vec!["echo SHOULD_NOT_RUN".to_string()], Some(0));
        let event = recommendation_card_event(1, "recommendation_insert");
        let mut output = Vec::new();

        render_selection_actions(&[event], &mut state, &mut output, 1)
            .expect("render insert action");

        let output = String::from_utf8(output).expect("utf8 output");
        assert!(output.contains("Recommendation insert"), "{output}");
        assert!(
            output.contains("Prepared recommendation 1 for manual input"),
            "{output}"
        );
        assert!(output.contains("echo SHOULD_NOT_RUN"), "{output}");
        assert!(
            output.contains(
                "Insert is pending editable input only; nothing was submitted or written to the child shell."
            ),
            "{output}"
        );
        assert!(!output.contains("$ echo SHOULD_NOT_RUN"), "{output}");
    }

    #[test]
    fn card_details_renders_recommendation_details_without_executing() {
        let mut state = InlineState::default();
        state
            .control
            .remember_selectable_commands(vec!["pwd".to_string()], Some(0));
        let event = recommendation_card_event(1, "recommendation_details");
        let mut output = Vec::new();

        render_selection_actions(&[event], &mut state, &mut output, 1)
            .expect("render details action");

        let output = String::from_utf8(output).expect("utf8 output");
        assert!(output.contains("Recommendation details"), "{output}");
        assert!(output.contains("Details for recommendation 1"), "{output}");
        assert!(output.contains("pwd"), "{output}");
        assert!(output.contains("Details-only"), "{output}");
        assert!(!output.contains("$ pwd"), "{output}");
    }
}
