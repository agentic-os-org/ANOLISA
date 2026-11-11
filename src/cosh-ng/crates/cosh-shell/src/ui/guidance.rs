use crate::diagnostics::health::{
    sorted_try_items, HealthFactValue, HealthMessageId, HealthScanReport, HealthTryKind,
};
use crate::insight::guidance::{quote::bash_arg, BashEvent, FailureKind};
use crate::ui::{primary_health_prompt_suggestion, HealthBannerModel, RatatuiInlineRenderer};
use crate::Language;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Invitation {
    pub(crate) problem: String,
    pub(crate) text: String,
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
    health_candidate_for(report, language, Target::Bash)
}

pub(crate) fn health_candidate_for(
    report: &HealthScanReport,
    language: Language,
    target: Target,
) -> Option<Invitation> {
    let item = sorted_try_items(report).into_iter().next()?;
    if item.kind != HealthTryKind::AskAgent
        || (matches!(target, Target::Bash)
            && !matches!(
                item.prompt_id,
                Some(
                    HealthMessageId::HealthPromptAnalyzeMemoryPressure
                        | HealthMessageId::HealthPromptCheckSwapPressure
                        | HealthMessageId::HealthPromptCheckRecentOom
                        | HealthMessageId::HealthPromptInspectProcessMemory
                        | HealthMessageId::HealthPromptInspectDiskUsage
                        | HealthMessageId::HealthPromptInspectHighLoad
                )
            ))
    {
        return None;
    }
    let prompt = primary_health_prompt_suggestion(report, crate::I18n::new(language))?;
    if prompt.is_empty() || prompt.len() > 2048 || prompt.chars().any(char::is_control) {
        return None;
    }
    let text = match target {
        Target::Bash => format!("  cosh --ask {}", bash_arg(&prompt)?),
        Target::Cosh => match language {
            Language::ZhCn => format!("可以直接提问：\n{prompt}"),
            Language::EnUs => {
                format!("Try asking:\n{prompt}")
            }
        },
    };
    Some(Invitation {
        problem: item.id.clone(),
        text,
        target,
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
        } else if matches!(invitation.target, Target::Cosh) {
            text.push_str(&invitation.text);
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
