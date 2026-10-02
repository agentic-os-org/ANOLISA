use std::path::Path;

use super::config::ExtensionConfig;

/// Context values for variable substitution in extension configs.
pub struct VariableContext<'a> {
    /// Absolute path to the extension directory.
    pub extension_path: &'a Path,
    /// Current workspace / project root directory.
    pub workspace_path: &'a Path,
}

/// Perform variable substitution on all string fields in an ExtensionConfig.
///
/// Supported variables:
/// - `${extensionPath}` → extension directory absolute path
/// - `${workspacePath}` → current workspace directory
/// - `${/}` → path separator (always `/` on Linux)
pub fn hydrate_config(config: &mut ExtensionConfig, ctx: &VariableContext) {
    let ext_path = ctx.extension_path.to_string_lossy();
    let ws_path = ctx.workspace_path.to_string_lossy();

    // Hydrate skill directory paths
    for dir in &mut config.skills.0 {
        *dir = hydrate_string(dir, &ext_path, &ws_path);
    }

    // Hydrate hook commands in all hook groups
    hydrate_hook_groups(&mut config.hooks.pre_tool_use, &ext_path, &ws_path);
    hydrate_hook_groups(&mut config.hooks.post_tool_use, &ext_path, &ws_path);
    hydrate_hook_groups(&mut config.hooks.post_tool_use_failure, &ext_path, &ws_path);
    hydrate_hook_groups(&mut config.hooks.user_prompt_submit, &ext_path, &ws_path);
    hydrate_hook_groups(&mut config.hooks.session_start, &ext_path, &ws_path);
    hydrate_hook_groups(&mut config.hooks.stop, &ext_path, &ws_path);
    hydrate_hook_groups(&mut config.hooks.before_model, &ext_path, &ws_path);
    hydrate_hook_groups(&mut config.hooks.after_model, &ext_path, &ws_path);
}

fn hydrate_hook_groups(groups: &mut [super::config::HookGroup], ext_path: &str, ws_path: &str) {
    for group in groups.iter_mut() {
        for hook in group.hooks.iter_mut() {
            hook.command = hydrate_string(&hook.command, ext_path, ws_path);
            // Values only: an env *name* is an identifier, and substituting a
            // path into it could synthesise a name the manifest never declared
            // and the fingerprint never covered.
            for value in hook.env.values_mut() {
                *value = hydrate_string(value, ext_path, ws_path);
            }
        }
    }
}

fn hydrate_string(s: &str, ext_path: &str, ws_path: &str) -> String {
    s.replace("${extensionPath}", ext_path)
        .replace("${workspacePath}", ws_path)
        .replace("${/}", "/")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn test_hydrate_extension_path() {
        let result = hydrate_string(
            "${extensionPath}/hooks/check.sh",
            "/home/user/.copilot-shell/extensions/my-ext",
            "/workspace",
        );
        assert_eq!(
            result,
            "/home/user/.copilot-shell/extensions/my-ext/hooks/check.sh"
        );
    }

    #[test]
    fn test_hydrate_workspace_path() {
        let result = hydrate_string("${workspacePath}/.cache", "/ext", "/home/user/project");
        assert_eq!(result, "/home/user/project/.cache");
    }

    #[test]
    fn test_hydrate_path_separator() {
        let result = hydrate_string("a${/}b${/}c", "/ext", "/ws");
        assert_eq!(result, "a/b/c");
    }

    #[test]
    fn test_hydrate_multiple_vars() {
        let result = hydrate_string(
            "${extensionPath}${/}scripts${/}run.sh --dir=${workspacePath}",
            "/opt/ext",
            "/home/user/proj",
        );
        assert_eq!(result, "/opt/ext/scripts/run.sh --dir=/home/user/proj");
    }

    #[test]
    fn test_hydrate_config_full() {
        let json = r#"{
            "name": "test",
            "skills": ["${extensionPath}/skills", "${extensionPath}/extra"],
            "hooks": {
                "PreToolUse": [
                    {
                        "hooks": [
                            {"type": "command", "command": "${extensionPath}/hooks/pre.sh --ws=${workspacePath}"}
                        ]
                    }
                ]
            }
        }"#;
        let mut config: ExtensionConfig = serde_json::from_str(json).unwrap();
        let ctx = VariableContext {
            extension_path: &PathBuf::from("/opt/ext"),
            workspace_path: &PathBuf::from("/workspace"),
        };
        hydrate_config(&mut config, &ctx);

        assert_eq!(config.skills.0, vec!["/opt/ext/skills", "/opt/ext/extra"]);
        assert_eq!(
            config.hooks.pre_tool_use[0].hooks[0].command,
            "/opt/ext/hooks/pre.sh --ws=/workspace"
        );
    }

    #[test]
    fn hydrates_hook_env_values_but_never_names() {
        let json = r#"{
            "name": "test",
            "hooks": {
                "PreToolUse": [
                    {
                        "hooks": [
                            {
                                "type": "command",
                                "command": "run.sh",
                                "env": {
                                    "CACHE_DIR": "${workspacePath}/.cache",
                                    "${extensionPath}": "literal-name"
                                }
                            }
                        ]
                    }
                ]
            }
        }"#;
        let mut config: ExtensionConfig = serde_json::from_str(json).unwrap();
        let ctx = VariableContext {
            extension_path: &PathBuf::from("/opt/ext"),
            workspace_path: &PathBuf::from("/workspace"),
        };
        hydrate_config(&mut config, &ctx);

        let env = &config.hooks.pre_tool_use[0].hooks[0].env;
        assert_eq!(env.get("CACHE_DIR").unwrap(), "/workspace/.cache");
        // Names are identifiers: substitution there could synthesise a name the
        // manifest never declared and the fingerprint never covered.
        assert!(env.contains_key("${extensionPath}"));
    }

    #[test]
    fn test_no_vars_unchanged() {
        let result = hydrate_string("plain command", "/ext", "/ws");
        assert_eq!(result, "plain command");
    }
}
