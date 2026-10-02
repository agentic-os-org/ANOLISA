use crate::tools::command_risk::HIGH_RISK_EXPLANATION_REASONS;

/// Display-width budget for card risk phrases (ARP SDD validation.md S2:
/// <= 32 columns / 16 fullwidth chars). Referenced by the exhaustive i18n
/// test below and by full-card rendering tests so the budget cannot drift
/// from the SDD contract.
pub(crate) const CARD_REASON_PHRASE_MAX_WIDTH: usize = 32;

/// Card-facing reason policy (ARP SDD design.md §2).
///
/// Returns the natural-language risk phrase for the approval card
/// continuation line if and only if the request risk is `high` and the
/// primary reason belongs to the display whitelist. Every other case —
/// low/medium risk, structural or fallback codes, and unknown future
/// codes — is fail-quiet (`None`, no line rendered).
pub(crate) fn card_reason_phrase(
    risk: &str,
    primary_reason: &str,
    i18n: crate::I18n,
) -> Option<String> {
    if risk != "high" {
        return None;
    }
    phrase_message_id(primary_reason).map(|id| i18n.t(id).to_string())
}

fn phrase_message_id(code: &str) -> Option<crate::MessageId> {
    use crate::MessageId::*;
    Some(match code {
        "privilege-escalation" => ApprovalRiskPhrasePrivilegeEscalation,
        "credential-access" => ApprovalRiskPhraseCredentialAccess,
        "filesystem-delete" => ApprovalRiskPhraseFilesystemDelete,
        "filesystem-write" => ApprovalRiskPhraseFilesystemWrite,
        "permission-change" => ApprovalRiskPhrasePermissionChange,
        "process-control" => ApprovalRiskPhraseProcessControl,
        "system-control" => ApprovalRiskPhraseSystemControl,
        "service-control" => ApprovalRiskPhraseServiceControl,
        "service-or-container-control" => ApprovalRiskPhraseServiceOrContainerControl,
        "package-manager-mutation" => ApprovalRiskPhrasePackageManagerMutation,
        "interactive-editor" => ApprovalRiskPhraseInteractiveEditor,
        "remote-code-execution" => ApprovalRiskPhraseRemoteCodeExecution,
        "sensitive-path" => ApprovalRiskPhraseSensitivePath,
        "sensitive-search" => ApprovalRiskPhraseSensitiveSearch,
        "command-substitution" => ApprovalRiskPhraseCommandSubstitution,
        "redirection-write" => ApprovalRiskPhraseRedirectionWrite,
        "awk-shell-execution" => ApprovalRiskPhraseAwkShellExecution,
        _ => return None,
    })
}

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use super::approval::wrapped_preview_rows;

/// V6a continuation line under the metadata row: `└ 风险: <phrase>`.
/// `reason` is already the localized natural-language phrase (never a raw
/// code) — the policy lives in `card_reason_phrase` above.
pub(super) fn approval_reason_line(reason: &str, i18n: crate::I18n) -> String {
    format!(
        "\u{2514} {}{reason}",
        i18n.t(crate::MessageId::ApprovalRiskDetailLabel)
    )
}

pub(super) fn approval_reason_rows(reason: &str, width: usize, i18n: crate::I18n) -> Vec<String> {
    wrapped_preview_rows(&approval_reason_line(reason, i18n), width, 2)
}

/// Styled continuation lines: label in the border color, phrase dimmed.
pub(super) fn approval_reason_styled_lines(
    reason_rows: Vec<String>,
    border: Color,
    i18n: crate::I18n,
) -> Vec<Line<'static>> {
    let label = format!(
        "\u{2514} {}",
        i18n.t(crate::MessageId::ApprovalRiskDetailLabel)
    );
    reason_rows
        .into_iter()
        .map(|row| {
            if let Some(phrase) = row.strip_prefix(&label) {
                Line::from(vec![
                    Span::styled(label.clone(), Style::default().fg(border)),
                    Span::styled(phrase.to_string(), Style::default().fg(Color::DarkGray)),
                ])
            } else {
                Line::from(Span::styled(row, Style::default().fg(Color::DarkGray)))
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::display_width;
    use super::*;
    use crate::{I18n, Language};

    #[test]
    fn whitelist_codes_have_localized_phrases_within_width_budget() {
        for language in [Language::ZhCn, Language::EnUs] {
            let i18n = I18n::new(language);
            for code in HIGH_RISK_EXPLANATION_REASONS {
                let phrase = card_reason_phrase("high", code, i18n)
                    .unwrap_or_else(|| panic!("missing phrase for {code} ({language:?})"));
                assert_ne!(&phrase, code, "phrase must not echo the raw code: {code}");
                assert!(
                    display_width(&phrase) <= CARD_REASON_PHRASE_MAX_WIDTH,
                    "phrase too wide for {code} ({language:?}): {phrase}"
                );
            }
        }
    }

    #[test]
    fn non_high_risk_never_shows_a_phrase() {
        let i18n = I18n::new(Language::ZhCn);
        assert_eq!(
            card_reason_phrase("medium", "privilege-escalation", i18n),
            None
        );
        assert_eq!(card_reason_phrase("low", "bounded-readonly", i18n), None);
    }

    #[test]
    fn structural_fallback_and_unknown_codes_are_fail_quiet() {
        let i18n = I18n::new(Language::EnUs);
        for code in [
            "unknown-command",
            "and-or-list-not-auto-executable",
            "pipeline-not-auto-executable",
            "compound-readonly",
            "unsafe-binding",
            "parse-failed",
            "not-a-real-code",
        ] {
            assert_eq!(card_reason_phrase("high", code, i18n), None, "{code}");
        }
    }
}
