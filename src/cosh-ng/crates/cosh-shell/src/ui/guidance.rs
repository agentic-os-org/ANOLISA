use crate::diagnostics::health::{
    select_try_items, AdviceSurface, HealthFactValue, HealthMessageId, HealthScanReport,
    HealthTryKind,
};
use crate::insight::guidance::{quote::bash_arg, BashEvent, FailureKind};
use crate::ui::{primary_health_prompt_suggestion, HealthBannerModel, RatatuiInlineRenderer};
use crate::Language;

mod suggestions;
pub(crate) use suggestions::{
    session_health_questions, suggestion_surface_payload, SuggestionItem, SuggestionListModel,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Invitation {
    pub(crate) problem: String,
    pub(crate) text: String,
    pub(crate) evidence: Option<String>,
    pub(crate) event: Option<(&'static str, String)>,
    pub(crate) severity: Option<u8>,
    pub(crate) legacy_problems: Vec<String>,
    target: Target,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Target {
    Bash,
    Cosh,
}

pub(crate) fn health_candidate(
    report: &HealthScanReport,
    language: Language,
) -> Option<Invitation> {
    let item = select_try_items(report, &report.try_items, AdviceSurface::Bash, 1)
        .into_iter()
        .next()?;
    if item.kind != HealthTryKind::AskAgent {
        return None;
    }
    let prompt = primary_health_prompt_suggestion(report, crate::I18n::new(language))?;
    if prompt.is_empty() || prompt.len() > 2048 || prompt.chars().any(char::is_control) {
        return None;
    }
    let text = format!("  cosh --ask {}", bash_arg(&prompt)?);
    Some(Invitation {
        problem: crate::diagnostics::health::prompt_selection::exposure_problem(report, item),
        text,
        evidence: Some(crate::diagnostics::health::exposure_evidence(report, item)),
        event: crate::diagnostics::health::prompt_selection::exposure_event(report, item),
        severity: report
            .findings
            .iter()
            .find(|finding| finding.id == item.finding_id)
            .map(|finding| finding.severity.precedence()),
        legacy_problems: crate::diagnostics::health::prompt_selection::legacy_exposure_problems(
            item,
        ),
        target: Target::Bash,
    })
}

pub(crate) fn login_entry(language: Language) -> Invitation {
    let text = match language {
        Language::ZhCn => "运行 cosh 进入 AI Shell（命令 / 自然语言） · 指南：cosh guide",
        Language::EnUs => "Run cosh: AI Shell for commands / questions · Guide: cosh guide",
    };
    Invitation {
        target: Target::Bash,
        problem: "login-entry".to_string(),
        text: text.to_string(),
        evidence: None,
        event: None,
        severity: None,
        legacy_problems: Vec::new(),
    }
}

pub(crate) fn default_login(language: Language) -> Invitation {
    let text = match language {
        Language::ZhCn => "下次登录直接进入 AI Shell：\n  cosh login set\n恢复原 Bash：cosh login restore；关闭 AI 使用提示：cosh guide off",
        Language::EnUs => "Start AI Shell on your next login:\n  cosh login set\nRestore Bash: cosh login restore; Hide AI tips: cosh guide off",
    };
    Invitation {
        target: Target::Cosh,
        problem: "default-login".to_string(),
        text: text.to_string(),
        evidence: None,
        event: None,
        severity: None,
        legacy_problems: Vec::new(),
    }
}

pub(crate) fn failure_invitation(
    event: &BashEvent,
    _kind: FailureKind,
    language: Language,
    context_id: &str,
) -> Option<Invitation> {
    if !crate::config::guide::valid_context_id(context_id) {
        return None;
    }
    let prompt = match language {
        Language::ZhCn => "分析这次失败",
        Language::EnUs => "Analyze this failure",
    };
    let mut text = question_text(prompt, language)?;
    text.push_str(&format!(" --context {context_id}"));
    Some(Invitation {
        target: Target::Bash,
        problem: event.expression.clone(),
        text,
        evidence: None,
        event: None,
        severity: None,
        legacy_problems: Vec::new(),
    })
}

pub(crate) fn render_failure(
    event: &BashEvent,
    kind: FailureKind,
    context_id: &str,
) -> Option<String> {
    failure_invitation(
        event,
        kind,
        crate::config::read_guidance_language().ok()?,
        context_id,
    )
    .map(|invitation| invitation.text)
}

pub(crate) fn style_invitation(text: &str, styled: bool) -> String {
    if !styled {
        return text.to_string();
    }
    let mut lines = text.lines();
    let (Some(title), Some(command), None) = (lines.next(), lines.next(), lines.next()) else {
        return text.to_string();
    };
    let Some((brand, description)) = title.trim().split_once(" · ") else {
        return text.to_string();
    };
    let Some((label, hint)) = description.rsplit_once(" · ") else {
        return text.to_string();
    };
    let command = command.replacen("cosh --ask", "\x1b[36;1mcosh --ask\x1b[0m", 1);
    format!("\x1b[36;1m┃ {brand}\x1b[0m  {label} · {hint}\n{command}")
}

fn question_text(prompt: &str, language: Language) -> Option<String> {
    let command = format!("  cosh --ask {}", bash_arg(prompt)?);
    Some(invitation_text(&command, language))
}

fn invitation_text(command: &str, language: Language) -> String {
    let intro = match language {
        Language::ZhCn => "Cosh AI · 排查建议 · 复制执行",
        Language::EnUs => "Cosh AI · Investigate · Copy and run",
    };
    format!("  {intro}\n{command}")
}

pub(crate) fn render_health(
    renderer: &RatatuiInlineRenderer,
    report: &HealthScanReport,
    show_facts: bool,
    target: Target,
    invitation: Option<&Invitation>,
) -> Option<String> {
    let text = render_health_full(renderer, report, show_facts, target, invitation);
    if target != Target::Bash
        || text.as_ref().is_some_and(|text| {
            text.len() < crate::diagnostics::health::banner_policy::MAX_TEXT_BYTES
        })
    {
        return text;
    }
    let mut compact = String::new();
    if show_facts {
        let findings = crate::diagnostics::health::sorted_findings(report);
        let finding = findings.first()?;
        compact.push_str(
            &crate::diagnostics::health::prompt_selection::format_health_message(
                renderer.i18n(),
                finding.title_id,
                &finding.detail_args,
            ),
        );
        compact.push('\n');
        for key in ["mount", "service", "path", "cgroup"] {
            if let Some(value) = finding.detail_args.get(key) {
                if value.chars().count() > 2048 {
                    return None;
                }
                compact.push_str(&visible_data(value));
                compact.push('\n');
            }
        }
    }
    compact.push_str(&render_health_full(
        renderer, report, false, target, invitation,
    )?);
    (compact.len() < crate::diagnostics::health::banner_policy::MAX_TEXT_BYTES).then_some(compact)
}

fn render_health_full(
    renderer: &RatatuiInlineRenderer,
    report: &HealthScanReport,
    show_facts: bool,
    target: Target,
    invitation: Option<&Invitation>,
) -> Option<String> {
    let mut output = Vec::new();
    if show_facts {
        let mut presentation = report.clone();
        presentation.try_items.clear();
        for fact in &mut presentation.facts {
            if let HealthFactValue::String(value) = &mut fact.value {
                *value = visible_data(value);
            }
        }
        for metadata in presentation.fact_metadata.values_mut() {
            match &mut metadata.scope {
                crate::diagnostics::health::model::HealthFactScope::Path(value)
                | crate::diagnostics::health::model::HealthFactScope::Cgroup(value) => {
                    *value = visible_data(value);
                }
                _ => {}
            }
        }
        for finding in &mut presentation.findings {
            for value in finding.detail_args.values_mut() {
                *value = visible_data(value);
            }
        }
        let mut model = HealthBannerModel::new(&presentation);
        if target == Target::Bash {
            model = model
                .with_guide()
                .with_login_entry(invitation.is_some_and(|item| item.problem == "login-entry"));
        }
        renderer.write_health_banner(&mut output, model).ok()?;
    }
    let mut text = String::from_utf8(output).ok()?;
    if show_facts && target == Target::Bash {
        if let Some(finding) = crate::diagnostics::health::sorted_findings(report).first() {
            for key in ["mount", "service", "path", "cgroup"] {
                if let Some(value) = finding.detail_args.get(key) {
                    if value.chars().count() > 2048 {
                        return None;
                    }
                    let value = visible_data(value);
                    if !text.contains(&value) {
                        text.push_str("  ");
                        text.push_str(&value);
                        text.push('\n');
                    }
                }
            }
        }
    }
    if let Some(invitation) = invitation.filter(|item| !show_facts || item.problem != "login-entry")
    {
        if invitation.problem == "login-entry" {
            let entry = &invitation.text;
            if renderer.styled {
                let line = entry.replacen("cosh", "\x1b[36;1mcosh\x1b[0m", 1).replacen(
                    "cosh guide",
                    "\x1b[36;1mcosh guide\x1b[0m",
                    1,
                );
                if show_facts {
                    text.push_str("  ");
                }
                text.push_str(&line);
            } else {
                if show_facts {
                    text.push_str("  ");
                }
                text.push_str(entry);
            }
            text.push('\n');
        } else {
            let language = renderer.i18n().language();
            let action = invitation_text(&invitation.text, language);
            text.push_str(&style_invitation(&action, renderer.styled));
            text.push('\n');
            if !show_facts {
                text.push_str(match language {
                    Language::ZhCn => "使用指南：运行 cosh guide\n",
                    Language::EnUs => "Guide: Run cosh guide\n",
                });
            }
        }
    }
    if show_facts && !text.is_empty() {
        text.push('\n');
    }
    if text.len() > 16 * 1024 {
        return None;
    }
    Some(text)
}

fn visible_data(value: &str) -> String {
    let mut output = String::new();
    for ch in value.chars().take(2048) {
        use std::fmt::Write;
        if ch.is_control() {
            let _ = write!(output, "\\u{{{:x}}}", ch as u32);
        } else {
            output.push(ch);
        }
    }
    if value.chars().count() > 2048 {
        output.push('…');
    }
    output
}

#[cfg(test)]
library_tests! {
    #[path = "guidance/tests.rs"]
    mod tests;
}
