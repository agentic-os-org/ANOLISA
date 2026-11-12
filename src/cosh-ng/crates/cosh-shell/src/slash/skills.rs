use serde_json::Value;

use crate::runtime::prelude::*;
use crate::slash::panel::render_notice_panel;

fn text<'a>(i18n: &I18n, en: &'a str, zh: &'a str) -> &'a str {
    match i18n.language() {
        Language::EnUs => en,
        Language::ZhCn => zh,
    }
}

pub(super) fn render_skills_command<W: Write>(
    sub: Option<&str>,
    arg: Option<&str>,
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    let AdapterInstance::CoshCore(cosh_core) = adapter else {
        let i18n = state.i18n();
        return render_notice_panel(
            output,
            i18n.t(MessageId::SlashSkillsTitle),
            vec![i18n.t(MessageId::SlashRegistryUnavailable).to_string()],
            None,
        );
    };

    let action = sub.unwrap_or("list");
    let i18n = state.i18n();

    match action {
        "list" => {
            let params = Value::Null;
            match cosh_core.registry_query("skills", "list", params) {
                Ok(data) => {
                    let body = format_skills_list(&data, &i18n);
                    render_notice_panel(output, i18n.t(MessageId::SlashSkillsTitle), body, None)
                }
                Err(e) => render_notice_panel(
                    output,
                    i18n.t(MessageId::SlashSkillsTitle),
                    vec![format!("{}: {e}", text(&i18n, "Error", "错误"))],
                    None,
                ),
            }
        }
        "detail" => {
            let name = arg.unwrap_or("");
            if name.is_empty() {
                return render_notice_panel(
                    output,
                    i18n.t(MessageId::SlashSkillsTitle),
                    vec![format!(
                        "{}: /skills detail <name>",
                        text(&i18n, "Usage", "用法")
                    )],
                    None,
                );
            }
            let params = serde_json::json!({ "name": name });
            match cosh_core.registry_query("skills", "detail", params) {
                Ok(data) => {
                    let body = format_skill_detail(&data, &i18n);
                    render_notice_panel(output, i18n.t(MessageId::SlashSkillsTitle), body, None)
                }
                Err(e) => render_notice_panel(
                    output,
                    i18n.t(MessageId::SlashSkillsTitle),
                    vec![format!("{}: {e}", text(&i18n, "Error", "错误"))],
                    None,
                ),
            }
        }
        "enable" => {
            let name = arg.unwrap_or("");
            if name.is_empty() {
                return render_notice_panel(
                    output,
                    i18n.t(MessageId::SlashSkillsTitle),
                    vec![format!(
                        "{}: /skills enable <name>",
                        text(&i18n, "Usage", "用法")
                    )],
                    None,
                );
            }
            let params = serde_json::json!({ "name": name });
            match cosh_core.registry_query("skills", "enable", params) {
                Ok(_) => render_notice_panel(
                    output,
                    i18n.t(MessageId::SlashSkillsTitle),
                    vec![text(
                        &i18n,
                        "  Skill \"{name}\" enabled.",
                        "  技能“{name}”已启用。",
                    )
                    .replace("{name}", name)],
                    None,
                ),
                Err(e) => render_notice_panel(
                    output,
                    i18n.t(MessageId::SlashSkillsTitle),
                    vec![format!("{}: {e}", text(&i18n, "Error", "错误"))],
                    None,
                ),
            }
        }
        "disable" => {
            let name = arg.unwrap_or("");
            if name.is_empty() {
                return render_notice_panel(
                    output,
                    i18n.t(MessageId::SlashSkillsTitle),
                    vec![format!(
                        "{}: /skills disable <name>",
                        text(&i18n, "Usage", "用法")
                    )],
                    None,
                );
            }
            let params = serde_json::json!({ "name": name });
            match cosh_core.registry_query("skills", "disable", params) {
                Ok(_) => render_notice_panel(
                    output,
                    i18n.t(MessageId::SlashSkillsTitle),
                    vec![text(
                        &i18n,
                        "  Skill \"{name}\" disabled.",
                        "  技能“{name}”已禁用。",
                    )
                    .replace("{name}", name)],
                    None,
                ),
                Err(e) => render_notice_panel(
                    output,
                    i18n.t(MessageId::SlashSkillsTitle),
                    vec![format!("{}: {e}", text(&i18n, "Error", "错误"))],
                    None,
                ),
            }
        }
        _ => render_notice_panel(
            output,
            i18n.t(MessageId::SlashSkillsTitle),
            vec![
                text(
                    &i18n,
                    "Usage: /skills <subcommand>",
                    "用法: /skills <subcommand>",
                )
                .to_string(),
                text(
                    &i18n,
                    "  list              List all skills",
                    "  list              列出所有技能",
                )
                .to_string(),
                text(
                    &i18n,
                    "  detail <name>     Show skill details",
                    "  detail <name>     查看技能详情",
                )
                .to_string(),
                text(
                    &i18n,
                    "  enable <name>     Enable a disabled skill",
                    "  enable <name>     启用已禁用的技能",
                )
                .to_string(),
                text(
                    &i18n,
                    "  disable <name>    Disable a skill",
                    "  disable <name>    禁用技能",
                )
                .to_string(),
            ],
            None,
        ),
    }
}

/// Renders one short line per skill.
///
/// Descriptions are deliberately omitted: they are unbounded free text, so
/// appending them made every row wrap over several terminal lines and turned
/// the list into a wall of prose. Full text stays available via
/// `/skills detail <name>`.
pub(super) fn format_skills_list(data: &Value, i18n: &I18n) -> Vec<String> {
    let Some(arr) = data.as_array() else {
        return vec![i18n.t(MessageId::SlashSkillsEmptyBody).to_string()];
    };
    if arr.is_empty() {
        return vec![i18n.t(MessageId::SlashSkillsEmptyBody).to_string()];
    }
    arr.iter()
        .filter_map(|skill| {
            let name = skill.get("name")?.as_str()?;
            let level = skill.get("level").and_then(|v| v.as_str()).unwrap_or("?");
            let disabled = skill
                .get("disabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if disabled {
                Some(format!(
                    "  ○ {name} [{level}] [{}]",
                    text(i18n, "disabled", "已禁用")
                ))
            } else {
                Some(format!("  • {name} [{level}]"))
            }
        })
        .collect()
}

pub(super) fn format_skill_detail(data: &Value, i18n: &I18n) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(name) = data.get("name").and_then(|v| v.as_str()) {
        lines.push(format!("  {}: {name}", text(i18n, "Name", "名称")));
    }
    if let Some(desc) = data.get("description").and_then(|v| v.as_str()) {
        lines.push(format!("  {}: {desc}", text(i18n, "Description", "描述")));
    }
    if let Some(level) = data.get("level").and_then(|v| v.as_str()) {
        lines.push(format!("  {}: {level}", text(i18n, "Level", "级别")));
    }
    if let Some(base_dir) = data.get("base_dir").and_then(|v| v.as_str()) {
        lines.push(format!("  {}: {base_dir}", text(i18n, "Base Dir", "目录")));
    }
    lines
}

pub(super) fn completion_skill_names(data: &Value) -> Vec<String> {
    let Some(skills) = data.as_array() else {
        return Vec::new();
    };
    let mut names = skills
        .iter()
        .filter(|skill| {
            !skill
                .get("disabled")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .filter_map(|skill| skill.get("name").and_then(Value::as_str))
        .map(str::to_string)
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    names
}
