use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use super::{Tool, ToolContext, ToolKind, ToolResult};
use crate::skill::SkillManager;

/// Thin adapter that exposes `SkillManager` as a tool callable by the LLM.
/// All discovery / caching / priority logic lives in `SkillManager`.
pub struct SkillTool {
    manager: Arc<SkillManager>,
}

impl SkillTool {
    pub fn new(manager: Arc<SkillManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for SkillTool {
    fn name(&self) -> &str {
        "skill"
    }

    fn description(&self) -> &str {
        "Load and invoke a skill (SKILL.md) from the skills directory. For troubleshooting or diagnostic requests, invoke a clearly matching skill before broad ad-hoc shell investigation. Use action 'list' only when no listed skill clearly matches yet, or provide a skill name to get its prompt."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["invoke", "list"],
                    "description": "Action to perform: 'invoke' a skill or 'list' available skills"
                },
                "name": {
                    "type": "string",
                    "description": "Name of the skill to invoke (required for 'invoke' action)"
                }
            },
            "required": ["action"]
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Other
    }

    async fn invoke(&self, params: Value, _ctx: &ToolContext) -> Result<ToolResult, String> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("invoke");

        match action {
            "list" => {
                let disabled = crate::state::load_disabled(crate::state::SKILLS_STATE);
                let skills = self.manager.list().await;
                let active_skills: Vec<_> = skills
                    .iter()
                    .filter(|s| !disabled.contains(&s.name))
                    .collect();
                if active_skills.is_empty() {
                    return Ok(ToolResult::success("No skills found."));
                }
                let list: Vec<String> = active_skills
                    .iter()
                    .map(|s| format!("- {} ({}): {}", s.name, s.level, s.description))
                    .collect();
                Ok(ToolResult::success(list.join("\n")))
            }
            "invoke" => {
                let name = params
                    .get("name")
                    .and_then(|v| v.as_str())
                    .ok_or("missing 'name' parameter for 'invoke' action")?;

                // Check if skill is disabled
                let disabled = crate::state::load_disabled(crate::state::SKILLS_STATE);
                if disabled.contains(name) {
                    return Ok(ToolResult::error(format!(
                        "Skill '{}' is disabled. Use /skills enable {} to re-enable it.",
                        name, name
                    )));
                }

                match self.manager.load(name).await {
                    Some(skill) => {
                        let base_dir_hint = format!(
                            "\nBase directory for this skill: {}\nImportant: ALWAYS resolve absolute paths from this base directory when working with skills.",
                            skill.base_dir.display()
                        );
                        Ok(ToolResult::success(format!(
                            "[Skill '{}' loaded]{}\n\n{}",
                            skill.name, base_dir_hint, skill.body
                        )))
                    }
                    None => Ok(ToolResult::error(format!(
                        "Skill '{}' not found. Use action 'list' to see available skills.",
                        name
                    ))),
                }
            }
            other => Ok(ToolResult::error(format!(
                "Unknown action '{}', expected 'list' or 'invoke'",
                other
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;

    fn make_manager_with_dir(project_dir: &std::path::Path) -> Arc<SkillManager> {
        SkillManager::new_isolated(
            project_dir.to_path_buf(),
            vec![],
            Some(PathBuf::from("/nonexistent-user")),
            Some(PathBuf::from("/nonexistent-sys")),
        )
    }

    #[tokio::test]
    async fn skill_list_empty() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = make_manager_with_dir(dir.path());
        mgr.refresh().await;

        let tool = SkillTool::new(mgr);
        let ctx = ToolContext::new(
            PathBuf::from("/nonexistent"),
            "test".to_string(),
            PathBuf::from("/nonexistent"),
        );
        let result = tool
            .invoke(serde_json::json!({"action": "list"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("No skills found"));
    }

    #[tokio::test]
    async fn skill_invoke_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = make_manager_with_dir(dir.path());
        mgr.refresh().await;

        let tool = SkillTool::new(mgr);
        let ctx = ToolContext::new(
            PathBuf::from("/nonexistent"),
            "test".to_string(),
            PathBuf::from("/nonexistent"),
        );
        let result = tool
            .invoke(
                serde_json::json!({"action": "invoke", "name": "missing"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("not found"));
    }

    #[tokio::test]
    async fn skill_invoke_found() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join(".copilot-shell/skills/demo");
        std::fs::create_dir_all(&skills_dir).unwrap();
        let skill_path = skills_dir.join("SKILL.md");
        let mut f = std::fs::File::create(&skill_path).unwrap();
        writeln!(
            f,
            "---\nname: demo\ndescription: Demo skill\n---\n\nYou are demo."
        )
        .unwrap();

        let mgr = make_manager_with_dir(dir.path());
        mgr.refresh().await;

        let tool = SkillTool::new(mgr);
        let ctx = ToolContext::new(
            dir.path().to_path_buf(),
            "test".to_string(),
            dir.path().to_path_buf(),
        );
        let result = tool
            .invoke(
                serde_json::json!({"action": "invoke", "name": "demo"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("You are demo"));
        assert!(result.output.contains("Base directory for this skill:"));
        assert!(result
            .output
            .contains("ALWAYS resolve absolute paths from this base directory"));
    }

    #[tokio::test]
    async fn skill_list_shows_level() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join(".copilot-shell/skills/my-skill");
        std::fs::create_dir_all(&skills_dir).unwrap();
        std::fs::write(
            skills_dir.join("SKILL.md"),
            "---\nname: my-skill\ndescription: Test\n---\n\nBody.",
        )
        .unwrap();

        let mgr = make_manager_with_dir(dir.path());
        mgr.refresh().await;

        let tool = SkillTool::new(mgr);
        let ctx = ToolContext::new(
            dir.path().to_path_buf(),
            "test".to_string(),
            dir.path().to_path_buf(),
        );
        let result = tool
            .invoke(serde_json::json!({"action": "list"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("project"));
    }
}
