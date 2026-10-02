use serde_json::Value;

use crate::runtime::prelude::*;
use crate::slash::panel::render_notice_panel;

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
                    vec![format!("Error: {e}")],
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
                    vec!["Usage: /skills detail <name>".to_string()],
                    None,
                );
            }
            let params = serde_json::json!({ "name": name });
            match cosh_core.registry_query("skills", "detail", params) {
                Ok(data) => {
                    let body = format_skill_detail(&data);
                    render_notice_panel(output, i18n.t(MessageId::SlashSkillsTitle), body, None)
                }
                Err(e) => render_notice_panel(
                    output,
                    i18n.t(MessageId::SlashSkillsTitle),
                    vec![format!("Error: {e}")],
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
                    vec!["Usage: /skills enable <name>".to_string()],
                    None,
                );
            }
            let params = serde_json::json!({ "name": name });
            match cosh_core.registry_query("skills", "enable", params) {
                Ok(_) => render_notice_panel(
                    output,
                    i18n.t(MessageId::SlashSkillsTitle),
                    vec![format!("  Skill \"{name}\" enabled.")],
                    None,
                ),
                Err(e) => render_notice_panel(
                    output,
                    i18n.t(MessageId::SlashSkillsTitle),
                    vec![format!("Error: {e}")],
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
                    vec!["Usage: /skills disable <name>".to_string()],
                    None,
                );
            }
            let params = serde_json::json!({ "name": name });
            match cosh_core.registry_query("skills", "disable", params) {
                Ok(_) => render_notice_panel(
                    output,
                    i18n.t(MessageId::SlashSkillsTitle),
                    vec![format!("  Skill \"{name}\" disabled.")],
                    None,
                ),
                Err(e) => render_notice_panel(
                    output,
                    i18n.t(MessageId::SlashSkillsTitle),
                    vec![format!("Error: {e}")],
                    None,
                ),
            }
        }
        _ => render_notice_panel(
            output,
            i18n.t(MessageId::SlashSkillsTitle),
            vec![
                "Usage: /skills <subcommand>".to_string(),
                "  list              List all skills".to_string(),
                "  detail <name>     Show skill details".to_string(),
                "  enable <name>     Enable a disabled skill".to_string(),
                "  disable <name>    Disable a skill".to_string(),
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
                Some(format!("  ○ {name} [{level}] [disabled]"))
            } else {
                Some(format!("  • {name} [{level}]"))
            }
        })
        .collect()
}

pub(super) fn format_skill_detail(data: &Value) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(name) = data.get("name").and_then(|v| v.as_str()) {
        lines.push(format!("  Name: {name}"));
    }
    if let Some(desc) = data.get("description").and_then(|v| v.as_str()) {
        lines.push(format!("  Description: {desc}"));
    }
    if let Some(level) = data.get("level").and_then(|v| v.as_str()) {
        lines.push(format!("  Level: {level}"));
    }
    if let Some(base_dir) = data.get("base_dir").and_then(|v| v.as_str()) {
        lines.push(format!("  Base Dir: {base_dir}"));
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
