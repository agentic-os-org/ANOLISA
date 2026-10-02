use std::io::Write;

use super::aggregate::severity_label;
use super::policy::{classify_command_intent, CommandIntent};
use super::prelude::{
    ConsultationCardModel, I18n, Language, MessageId, NoticePanelModel, RatatuiInlineRenderer,
};
use super::state::PendingConsultation;

pub(crate) fn render_consultation_card<W: Write>(
    consultation: &PendingConsultation,
    language: Language,
    output: &mut W,
) -> std::io::Result<()> {
    let Some(finding) = consultation.hook_finding.as_ref() else {
        return Ok(());
    };
    RatatuiInlineRenderer::for_terminal()
        .with_language(language)
        .write_consultation_card(
            output,
            &ConsultationCardModel {
                details_id: consultation.finding_id.clone(),
                severity: severity_label(finding.severity).to_string(),
                title: finding.title.clone(),
                finding: finding.description.clone(),
                suggestion: finding.suggestion.clone(),
            },
        )?;
    Ok(())
}

pub(crate) fn render_consultation_details<W: Write>(
    consultation: &PendingConsultation,
    i18n: I18n,
    debug: bool,
    output: &mut W,
) -> std::io::Result<()> {
    let Some(finding) = consultation.hook_finding.as_ref() else {
        return Ok(());
    };
    let created_at = consultation.created_at_ms.to_string();
    let user_interest_reason = user_interest_reason_for_consultation(consultation);
    let user_interest_reason_args = user_interest_reason.template_args(&i18n);
    let output_capture_status = if consultation.output_ref.is_some() {
        "captured"
    } else {
        "missing"
    };
    let mut body = vec![
        finding.description.clone(),
        i18n.format(
            MessageId::HookDetailsConfidenceLine,
            &[
                ("confidence", consultation.confidence.as_str()),
                ("reason", consultation.display_reason.as_str()),
            ],
        ),
        i18n.format(
            MessageId::HookDetailsUserInterestLine,
            &user_interest_reason_args.as_template_args(),
        ),
        i18n.format(
            MessageId::HookDetailsTopicLine,
            &[
                ("topic", consultation.topic.as_str()),
                ("entity", consultation.entity_key.as_str()),
            ],
        ),
        i18n.format(
            MessageId::HookDetailsOriginLine,
            &[(
                "origin",
                command_origin_from_suppression_key(&consultation.suppression_key),
            )],
        ),
        i18n.format(
            MessageId::HookDetailsSuppressionKeyLine,
            &[("key", consultation.suppression_key.as_str())],
        ),
        i18n.format(
            MessageId::HookDetailsOutputRefLine,
            &[("ref", output_capture_status)],
        ),
        i18n.format(
            MessageId::HookDetailsCreatedAtLine,
            &[("created_at", created_at.as_str())],
        ),
        i18n.format(
            MessageId::HookDetailsPromptHintLine,
            &[("hint", consultation.prompt_hint.as_str())],
        ),
    ];
    if let Some(skill) = consultation.recommended_skill.as_ref() {
        body.push(i18n.format(
            MessageId::HookDetailsRecommendedSkillLine,
            &[("skill", skill.as_str())],
        ));
    }
    if let Some(cli_hint) = finding.cli_hint.as_ref() {
        body.push(i18n.format(
            MessageId::HookDetailsReadOnlyCliHintLine,
            &[("hint", cli_hint.as_str())],
        ));
    }
    if debug {
        if let Some(output_ref) = consultation.output_ref.as_ref() {
            body.push(format!("debug_output_ref: {output_ref}"));
        }
    }
    RatatuiInlineRenderer::for_terminal().write_notice_panel(
        output,
        NoticePanelModel {
            title: i18n.t(MessageId::HookFindingDetailsTitle),
            body,
            footer: Some(i18n.t(MessageId::HookDetailsFooter)),
        },
    )
}

fn command_origin_from_suppression_key(suppression_key: &str) -> &str {
    let Some(origin) = suppression_key.rsplit(':').next() else {
        return "unknown";
    };
    if matches!(
        origin,
        "user_interactive"
            | "user_send_to_shell"
            | "user_analysis_action"
            | "agent_handoff"
            | "provider_tool"
            | "shell_internal"
            | "unknown"
    ) {
        origin
    } else {
        "unknown"
    }
}

struct UserInterestReason {
    code: &'static str,
    description: MessageId,
}

impl UserInterestReason {
    fn template_args<'a>(&'a self, i18n: &I18n) -> UserInterestReasonTemplateArgs<'a> {
        UserInterestReasonTemplateArgs {
            code: self.code,
            description: i18n.t(self.description).to_string(),
        }
    }
}

struct UserInterestReasonTemplateArgs<'a> {
    code: &'a str,
    description: String,
}

impl<'a> UserInterestReasonTemplateArgs<'a> {
    fn as_template_args(&'a self) -> [(&'a str, &'a str); 2] {
        [
            ("code", self.code),
            ("description", self.description.as_str()),
        ]
    }
}

fn user_interest_reason_for_consultation(consultation: &PendingConsultation) -> UserInterestReason {
    let intent_reason = match classify_command_intent(&consultation.command) {
        CommandIntent::Lookup => Some(UserInterestReason {
            code: "lookup-intent",
            description: MessageId::HookDetailsReasonLookupIntent,
        }),
        CommandIntent::Pipeline => Some(UserInterestReason {
            code: "pipeline-intent",
            description: MessageId::HookDetailsReasonPipelineIntent,
        }),
        CommandIntent::Script => Some(UserInterestReason {
            code: "script-intent",
            description: MessageId::HookDetailsReasonScriptIntent,
        }),
        CommandIntent::Wrapper => Some(UserInterestReason {
            code: "wrapper-low-confidence",
            description: MessageId::HookDetailsReasonWrapperLowConfidence,
        }),
        CommandIntent::Interactive => Some(UserInterestReason {
            code: "interactive-intent",
            description: MessageId::HookDetailsReasonInteractiveIntent,
        }),
        CommandIntent::Diagnostic | CommandIntent::Other => None,
    };
    if let Some(reason) = intent_reason {
        return reason;
    }

    match consultation.display_reason.as_str() {
        "active-agent-run-deferred" => UserInterestReason {
            code: "active-run-deferred",
            description: MessageId::HookDetailsReasonActiveRunDeferred,
        },
        "user-continued-input" => UserInterestReason {
            code: "user-continued-input",
            description: MessageId::HookDetailsReasonUserContinuedInput,
        },
        "non-diagnostic-success-command" => UserInterestReason {
            code: "non-diagnostic-success-command",
            description: MessageId::HookDetailsReasonNonDiagnosticSuccessCommand,
        },
        "feedback-noisy" | "feedback-group-noisy" => UserInterestReason {
            code: "feedback-noisy",
            description: MessageId::HookDetailsReasonFeedbackNoisy,
        },
        "ignored-same-finding" => UserInterestReason {
            code: "ignored-same-finding",
            description: MessageId::HookDetailsReasonIgnoredSameFinding,
        },
        "same-card-already-rendered" => UserInterestReason {
            code: "same-card-already-rendered",
            description: MessageId::HookDetailsReasonSameCardAlreadyRendered,
        },
        "interruption-budget" => UserInterestReason {
            code: "interruption-budget",
            description: MessageId::HookDetailsReasonInterruptionBudget,
        },
        _ => {
            if consultation.display_reason == "low-confidence" {
                UserInterestReason {
                    code: "low-confidence",
                    description: MessageId::HookDetailsReasonLowConfidence,
                }
            } else if matches!(
                classify_command_intent(&consultation.command),
                CommandIntent::Diagnostic
            ) {
                UserInterestReason {
                    code: "diagnostic-intent",
                    description: MessageId::HookDetailsReasonDiagnosticIntent,
                }
            } else {
                UserInterestReason {
                    code: "other-intent",
                    description: MessageId::HookDetailsReasonOtherIntent,
                }
            }
        }
    }
}
