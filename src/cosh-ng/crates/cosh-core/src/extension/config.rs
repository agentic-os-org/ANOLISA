use std::collections::BTreeMap;

use serde::Deserialize;

use crate::config::HookDefinition;

/// Extension configuration parsed from `cosh-extension.json`.
#[derive(Debug, Clone, Deserialize)]
pub struct ExtensionConfig {
    pub name: String,
    #[serde(default = "default_version")]
    pub version: String,
    /// Skill directories relative to the extension root.
    /// Accepts a single string or an array of strings.
    #[serde(default)]
    pub skills: SkillsDirs,
    /// Hook definitions grouped by event name.
    #[serde(default)]
    pub hooks: ExtensionHooks,
}

fn default_version() -> String {
    "0.0.0".to_string()
}

// ─── SkillsDirs: string | string[] with default "skills" ─────────────────

/// Wrapper for flexible JSON deserialization: accepts `"skills"` or `["skills", "more"]`.
#[derive(Debug, Clone)]
pub struct SkillsDirs(pub Vec<String>);

impl Default for SkillsDirs {
    fn default() -> Self {
        Self(vec!["skills".to_string()])
    }
}

impl<'de> Deserialize<'de> for SkillsDirs {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de;

        struct SkillsDirsVisitor;

        impl<'de> de::Visitor<'de> for SkillsDirsVisitor {
            type Value = SkillsDirs;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a string or array of strings")
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(SkillsDirs(vec![value.to_string()]))
            }

            fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut dirs = Vec::new();
                while let Some(s) = seq.next_element::<String>()? {
                    dirs.push(s);
                }
                Ok(SkillsDirs(dirs))
            }
        }

        deserializer.deserialize_any(SkillsDirsVisitor)
    }
}

// ─── HookGroup: copilot-shell nested hook format ─────────────────────────

/// A single command hook configuration within a hook group.
/// Corresponds to copilot-shell's `CommandHookConfig` interface.
#[derive(Debug, Clone, Deserialize)]
pub struct CommandHookConfig {
    /// Hook type, always "command" for now.
    #[serde(default, rename = "type")]
    pub hook_type: Option<String>,
    /// The shell command to execute.
    pub command: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub timeout: Option<u64>,
    /// Explicitly permit this hook to fail open.
    #[serde(default)]
    pub fail_open: bool,
    /// Environment variables for this hook's child process. Passed through to
    /// [`HookDefinition::env`] verbatim; see there for the scoping guarantees.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

/// A hook group containing an optional matcher and an array of hook configs.
/// Corresponds to copilot-shell's `HookDefinition` interface:
/// ```typescript
/// interface HookDefinition {
///   matcher?: string;
///   sequential?: boolean;
///   hooks: HookConfig[];
/// }
/// ```
#[derive(Debug, Clone, Deserialize)]
pub struct HookGroup {
    /// Matcher pattern (regex or exact string) for tool-based events.
    #[serde(default)]
    pub matcher: Option<String>,
    /// Whether hooks in this group run sequentially.
    #[serde(default)]
    pub sequential: Option<bool>,
    /// The actual hook configurations in this group.
    pub hooks: Vec<CommandHookConfig>,
}

impl HookGroup {
    /// Flatten this hook group into individual `HookDefinition`s.
    /// The group-level `matcher` and `sequential` are propagated to each hook.
    pub fn flatten(&self) -> Vec<HookDefinition> {
        self.hooks
            .iter()
            .map(|h| HookDefinition {
                command: h.command.clone(),
                name: h.name.clone(),
                matcher: self.matcher.clone(),
                timeout: h.timeout,
                sequential: self.sequential,
                fail_open: h.fail_open,
                env: h.env.clone(),
            })
            .collect()
    }
}

/// Flatten a list of hook groups into a flat `Vec<HookDefinition>`.
pub fn flatten_hook_groups(groups: &[HookGroup]) -> Vec<HookDefinition> {
    groups.iter().flat_map(|g| g.flatten()).collect()
}

// ─── ExtensionHooks ──────────────────────────────────────────────────────

/// Hook definitions declared by an extension, grouped by event type.
/// Uses the copilot-shell nested hook group format.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ExtensionHooks {
    #[serde(default, rename = "PreToolUse")]
    pub pre_tool_use: Vec<HookGroup>,
    #[serde(default, rename = "PostToolUse")]
    pub post_tool_use: Vec<HookGroup>,
    #[serde(default, rename = "PostToolUseFailure")]
    pub post_tool_use_failure: Vec<HookGroup>,
    #[serde(default, rename = "UserPromptSubmit")]
    pub user_prompt_submit: Vec<HookGroup>,
    #[serde(default, rename = "SessionStart")]
    pub session_start: Vec<HookGroup>,
    #[serde(default, rename = "Stop")]
    pub stop: Vec<HookGroup>,
    #[serde(default, rename = "BeforeModel")]
    pub before_model: Vec<HookGroup>,
    #[serde(default, rename = "AfterModel")]
    pub after_model: Vec<HookGroup>,
}

impl ExtensionHooks {
    /// Returns true if no hooks are defined for any event.
    pub fn is_empty(&self) -> bool {
        self.pre_tool_use.is_empty()
            && self.post_tool_use.is_empty()
            && self.post_tool_use_failure.is_empty()
            && self.user_prompt_submit.is_empty()
            && self.session_start.is_empty()
            && self.stop.is_empty()
            && self.before_model.is_empty()
            && self.after_model.is_empty()
    }

    /// Merge another `ExtensionHooks` into this one (appending).
    pub fn merge(&mut self, other: &ExtensionHooks) {
        self.pre_tool_use.extend(other.pre_tool_use.clone());
        self.post_tool_use.extend(other.post_tool_use.clone());
        self.post_tool_use_failure
            .extend(other.post_tool_use_failure.clone());
        self.user_prompt_submit
            .extend(other.user_prompt_submit.clone());
        self.session_start.extend(other.session_start.clone());
        self.stop.extend(other.stop.clone());
        self.before_model.extend(other.before_model.clone());
        self.after_model.extend(other.after_model.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_minimal_config() {
        let json = r#"{"name": "test-ext", "version": "1.0.0"}"#;
        let config: ExtensionConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.name, "test-ext");
        assert_eq!(config.version, "1.0.0");
        assert_eq!(config.skills.0, vec!["skills"]);
        assert!(config.hooks.is_empty());
    }

    #[test]
    fn test_parse_skills_as_string() {
        let json = r#"{"name": "ext", "skills": "my-skills"}"#;
        let config: ExtensionConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.skills.0, vec!["my-skills"]);
    }

    #[test]
    fn test_parse_skills_as_array() {
        let json = r#"{"name": "ext", "skills": ["s1", "s2"]}"#;
        let config: ExtensionConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.skills.0, vec!["s1", "s2"]);
    }

    #[test]
    fn test_parse_copilot_shell_hook_format() {
        let json = r#"{
            "name": "ext",
            "hooks": {
                "PreToolUse": [
                    {
                        "matcher": "run_shell_command",
                        "hooks": [
                            {
                                "type": "command",
                                "command": "python3 /ext/hooks/scanner.py",
                                "name": "code-scanner",
                                "timeout": 5000
                            }
                        ]
                    },
                    {
                        "hooks": [
                            {
                                "type": "command",
                                "command": "python3 /ext/hooks/guard.py",
                                "name": "sandbox-guard"
                            }
                        ]
                    }
                ],
                "SessionStart": [
                    {
                        "hooks": [
                            {"type": "command", "command": "echo start", "name": "init"}
                        ]
                    }
                ]
            }
        }"#;
        let config: ExtensionConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.hooks.pre_tool_use.len(), 2);
        assert_eq!(config.hooks.session_start.len(), 1);
        assert!(config.hooks.post_tool_use.is_empty());

        // Verify first group has matcher
        let group0 = &config.hooks.pre_tool_use[0];
        assert_eq!(group0.matcher.as_deref(), Some("run_shell_command"));
        assert_eq!(group0.hooks.len(), 1);
        assert_eq!(group0.hooks[0].command, "python3 /ext/hooks/scanner.py");
        assert_eq!(group0.hooks[0].name.as_deref(), Some("code-scanner"));
        assert_eq!(group0.hooks[0].timeout, Some(5000));

        // Verify second group has no matcher (matches all)
        let group1 = &config.hooks.pre_tool_use[1];
        assert!(group1.matcher.is_none());
        assert_eq!(group1.hooks[0].name.as_deref(), Some("sandbox-guard"));
    }

    #[test]
    fn test_flatten_hook_groups() {
        let groups = vec![
            HookGroup {
                matcher: Some("skill".to_string()),
                sequential: None,
                hooks: vec![CommandHookConfig {
                    hook_type: Some("command".to_string()),
                    command: "echo a".to_string(),
                    name: Some("hook-a".to_string()),
                    description: None,
                    timeout: Some(3000),
                    fail_open: false,
                    env: BTreeMap::from([("TOKENLESS_AGENT_ID".to_string(), "cosh".to_string())]),
                }],
            },
            HookGroup {
                matcher: None,
                sequential: Some(true),
                hooks: vec![CommandHookConfig {
                    hook_type: None,
                    command: "echo b".to_string(),
                    name: None,
                    description: None,
                    timeout: None,
                    fail_open: false,
                    env: BTreeMap::new(),
                }],
            },
        ];
        let flat = flatten_hook_groups(&groups);
        assert_eq!(flat.len(), 2);
        // First hook inherits matcher from group
        assert_eq!(flat[0].command, "echo a");
        assert_eq!(flat[0].matcher.as_deref(), Some("skill"));
        assert_eq!(flat[0].timeout, Some(3000));
        assert_eq!(
            flat[0].env.get("TOKENLESS_AGENT_ID").map(String::as_str),
            Some("cosh")
        );
        // Second hook inherits sequential from group, no matcher
        assert_eq!(flat[1].command, "echo b");
        assert!(flat[1].matcher.is_none());
        assert_eq!(flat[1].sequential, Some(true));
        assert!(flat[1].env.is_empty());
    }

    #[test]
    fn test_parse_real_agent_sec_core_format() {
        // Simulates the real agent-sec-core cosh-extension.json structure
        let json = r#"{
            "name": "agent-sec-core",
            "version": "0.6.0",
            "hooks": {
                "PreToolUse": [
                    {
                        "matcher": "skill",
                        "hooks": [
                            {"type": "command", "name": "skill-ledger", "command": "python3 ${extensionPath}/hooks/skill_ledger_hook.py", "timeout": 5000}
                        ]
                    },
                    {
                        "hooks": [
                            {"type": "command", "command": "python3 ${extensionPath}/hooks/sandbox-guard.py", "name": "sandbox-guard"}
                        ]
                    }
                ],
                "UserPromptSubmit": [
                    {
                        "hooks": [
                            {"type": "command", "name": "prompt-scanner", "command": "python3 hooks/prompt_scanner_hook.py", "timeout": 10000},
                            {"type": "command", "name": "pii-checker", "command": "python3 hooks/pii_checker_hook.py", "timeout": 10000}
                        ]
                    }
                ],
                "BeforeModel": [
                    {"hooks": [{"type": "command", "name": "obs", "command": "echo obs", "timeout": 5000}]}
                ],
                "AfterModel": [
                    {"hooks": [{"type": "command", "name": "obs", "command": "echo obs"}]}
                ],
                "PostToolUseFailure": [
                    {"hooks": [{"type": "command", "command": "echo fail", "name": "fail-handler"}]}
                ],
                "Stop": [
                    {"hooks": [{"type": "command", "name": "obs", "command": "echo stop"}]}
                ]
            }
        }"#;
        let config: ExtensionConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.name, "agent-sec-core");
        assert_eq!(config.hooks.pre_tool_use.len(), 2);
        assert_eq!(config.hooks.user_prompt_submit.len(), 1);
        // UserPromptSubmit group has 2 hooks inside
        assert_eq!(config.hooks.user_prompt_submit[0].hooks.len(), 2);
        assert_eq!(config.hooks.before_model.len(), 1);
        assert_eq!(config.hooks.after_model.len(), 1);
        assert_eq!(config.hooks.post_tool_use_failure.len(), 1);
        assert_eq!(config.hooks.stop.len(), 1);

        // Flatten all PreToolUse hooks
        let flat = flatten_hook_groups(&config.hooks.pre_tool_use);
        assert_eq!(flat.len(), 2);
        assert_eq!(flat[0].matcher.as_deref(), Some("skill"));
        assert_eq!(flat[0].name.as_deref(), Some("skill-ledger"));
        assert!(flat[1].matcher.is_none());
        assert_eq!(flat[1].name.as_deref(), Some("sandbox-guard"));
    }
}
