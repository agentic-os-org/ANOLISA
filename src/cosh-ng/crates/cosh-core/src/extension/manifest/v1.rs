//! Strict v1 manifest parsing, discovery, and capability validation.

use super::*;

pub(super) fn parse_v1_manifest(
    content: &str,
    package_root: &Path,
) -> Result<ParsedManifest, ManifestError> {
    let manifest: ExtensionManifestV1 = serde_json::from_str(content).map_err(|error| {
        ManifestError::new(
            "extension_manifest_v1_invalid",
            format!("failed to parse strict extension manifest v1: {error}"),
        )
    })?;
    if manifest.schema_version != 1 {
        return Err(ManifestError::new(
            "extension_manifest_schema_unsupported",
            format!("unsupported schemaVersion: {}", manifest.schema_version),
        ));
    }
    validate_package_name(&manifest.name)
        .map_err(|error| ManifestError::new(error.code(), error.to_string()))?;
    Version::parse(&manifest.version).map_err(|error| {
        ManifestError::new(
            "extension_version_invalid",
            format!("extension version is not valid SemVer: {error}"),
        )
    })?;
    let requirement = VersionReq::parse(&manifest.compatibility.cosh).map_err(|error| {
        ManifestError::new(
            "extension_compatibility_invalid",
            format!("invalid cosh compatibility requirement: {error}"),
        )
    })?;
    let current = Version::parse(env!("CARGO_PKG_VERSION")).map_err(|error| {
        ManifestError::new(
            "extension_runtime_version_invalid",
            format!("cosh-core package version is invalid: {error}"),
        )
    })?;
    if !requirement.matches(&current) {
        return Err(ManifestError::new(
            "extension_incompatible",
            format!(
                "extension requires cosh {}, current version is {current}",
                manifest.compatibility.cosh
            ),
        ));
    }
    if manifest
        .description
        .as_ref()
        .is_some_and(|description| description.len() > 1024)
    {
        return Err(ManifestError::new(
            "extension_description_too_long",
            "extension description exceeds 1024 UTF-8 bytes",
        ));
    }

    for path in manifest
        .skills
        .iter()
        .chain(manifest.agents.iter())
        .chain(manifest.context_files.iter().map(|context| &context.path))
    {
        validate_relative_package_path(package_root, path)?;
    }

    let mut diagnostics = Vec::new();
    let mut records = discover_skill_records(&manifest.name, package_root, &manifest.skills, true)?;
    let mut host_executables = BTreeSet::new();
    let (runtime_hooks, mut hook_records) = validate_and_convert_hooks(
        &manifest.name,
        package_root,
        manifest.hooks,
        &mut host_executables,
    )?;
    records.append(&mut hook_records);

    let mut mcp_servers = Vec::new();
    for (local, server) in &manifest.mcp_servers {
        let id = CapabilityId::new(&manifest.name, CapabilityKind::Mcp, local)
            .map_err(|error| ManifestError::new(error.code(), error.to_string()))?;
        if server.transport != "stdio" {
            return Err(ManifestError::new(
                "extension_mcp_transport_unsupported",
                format!("MCP server {local} must use stdio transport"),
            ));
        }
        validate_mcp_executable(package_root, &server.command)?;
        for argument in &server.args {
            validate_extension_path_references(package_root, argument, false, false)?;
        }
        record_host_executable(&server.command, &mut host_executables);
        records.push(CapabilityRecord {
            id: id.canonical(),
            projection: json!({
                "args": server.args,
                "command": normalize_extension_command(&server.command),
                "env": server.env,
                "id": id.canonical(),
                "kind": "mcp",
                "required": server.required,
                "transport": "stdio"
            }),
        });
        mcp_servers.push(McpServerContribution {
            id: id.canonical(),
            name: local.clone(),
            command: server.command.clone(),
            args: server.args.clone(),
            env: server.env.clone(),
            required: server.required,
        });
    }

    let mut context_ids = BTreeSet::new();
    let mut contexts = Vec::new();
    for context in &manifest.context_files {
        validate_local_id(&context.id)
            .map_err(|error| ManifestError::new(error.code(), error.to_string()))?;
        if !context_ids.insert(context.id.clone()) {
            return Err(ManifestError::new(
                "extension_capability_duplicate",
                format!("duplicate context capability: {}", context.id),
            ));
        }
        let id = CapabilityId::new(&manifest.name, CapabilityKind::Context, &context.id)
            .map_err(|error| ManifestError::new(error.code(), error.to_string()))?;
        records.push(CapabilityRecord {
            id: id.canonical(),
            projection: json!({
                "id": id.canonical(),
                "kind": "context",
                "path": context.path,
                "required": context.required
            }),
        });
        contexts.push(ContextContribution {
            id: id.canonical(),
            path: package_root.join(&context.path),
            required: context.required,
        });
    }

    records.extend(discover_agent_records(
        &manifest.name,
        package_root,
        &manifest.agents,
    )?);

    let mut setting_keys = BTreeSet::new();
    let mut setting_projection = Vec::new();
    let mut settings = Vec::new();
    for setting in &manifest.settings {
        validate_setting_key(&setting.key)
            .map_err(|error| ManifestError::new(error.code(), error.to_string()))?;
        if !setting_keys.insert(setting.key.clone()) {
            return Err(ManifestError::new(
                "extension_setting_duplicate",
                format!("duplicate extension setting: {}", setting.key),
            ));
        }
        validate_setting(setting)?;
        setting_projection.push(json!({
            "key": setting.key,
            "required": setting.required,
            "sensitive": setting.sensitive,
            "type": setting.setting_type
        }));
        settings.push(SettingDefinition {
            key: setting.key.clone(),
            setting_type: match setting.setting_type.as_str() {
                "string" => SettingType::String,
                "boolean" => SettingType::Boolean,
                "integer" => SettingType::Integer,
                _ => unreachable!("validated setting type"),
            },
            description: setting.description.clone(),
            required: setting.required,
            sensitive: setting.sensitive,
            default: setting.default.clone(),
        });
    }
    setting_projection.sort_by(|left, right| left["key"].as_str().cmp(&right["key"].as_str()));

    records.sort_by(|left, right| left.id.cmp(&right.id));
    let capabilities = records.iter().map(|record| record.id.clone()).collect();
    let projection = json!({
        "capabilities": records.into_iter().map(|record| record.projection).collect::<Vec<_>>(),
        "extension": manifest.name,
        "hostExecutables": host_executables,
        "policyVersion": 1,
        "settings": setting_projection
    });
    let capability_fingerprint = fingerprint_projection(projection).map_err(|error| {
        ManifestError::new(
            "extension_fingerprint_failed",
            format!("failed to fingerprint extension manifest: {error}"),
        )
    })?;

    if !manifest.agents.is_empty() {
        diagnostics.push(ExtensionDiagnostic::new(
            "declared_not_executable",
            "agent contributions are declared but not executable in this runtime generation",
        ));
    }

    Ok(ParsedManifest {
        schema_version: ManifestSchemaVersion::V1,
        config: ExtensionConfig {
            name: manifest.name,
            version: manifest.version,
            skills: SkillsDirs(manifest.skills),
            hooks: runtime_hooks,
        },
        capability_fingerprint,
        capabilities,
        diagnostics,
        settings,
        contexts,
        mcp_servers,
        agent_directories: manifest
            .agents
            .iter()
            .map(|directory| package_root.join(directory))
            .collect(),
    })
}

#[derive(Debug)]
pub(super) struct CapabilityRecord {
    pub(super) id: String,
    pub(super) projection: Value,
}

pub(super) fn discover_skill_records(
    extension: &str,
    package_root: &Path,
    skill_dirs: &[String],
    strict: bool,
) -> Result<Vec<CapabilityRecord>, ManifestError> {
    let mut records = Vec::new();
    let mut ids = BTreeSet::new();
    for relative in skill_dirs {
        let path = if Path::new(relative).is_absolute() {
            Path::new(relative).to_path_buf()
        } else {
            package_root.join(relative)
        };
        for skill in load_skills_from_dir(&path, SkillLevel::Extension) {
            if strict {
                validate_local_id(&skill.name)
                    .map_err(|error| ManifestError::new(error.code(), error.to_string()))?;
            }
            let local = if strict {
                skill.name
            } else {
                legacy_local_id(&skill.name)
            };
            let id = CapabilityId::new(legacy_package_id(extension), CapabilityKind::Skill, local)
                .map_err(|error| ManifestError::new(error.code(), error.to_string()))?;
            if !ids.insert(id.canonical()) {
                return Err(ManifestError::new(
                    "extension_capability_duplicate",
                    format!("duplicate skill capability: {}", id.canonical()),
                ));
            }
            records.push(CapabilityRecord {
                id: id.canonical(),
                projection: json!({
                    "id": id.canonical(),
                    "kind": "skill"
                }),
            });
        }
    }
    Ok(records)
}

fn discover_agent_records(
    extension: &str,
    package_root: &Path,
    agent_dirs: &[String],
) -> Result<Vec<CapabilityRecord>, ManifestError> {
    let directories = agent_dirs
        .iter()
        .map(|relative| package_root.join(relative))
        .collect::<Vec<_>>();
    load_agent_files(extension, package_root, &directories)
        .map_err(|error| ManifestError::new(error.code(), error.to_string()))
        .map(|agents| {
            agents
                .into_iter()
                .map(|agent| CapabilityRecord {
                    id: agent.id.clone(),
                    projection: json!({
                        "id": agent.id,
                        "kind": "agent",
                        "mcpServers": agent.mcp_servers,
                        "skills": agent.skills,
                        "tools": agent.tools
                    }),
                })
                .collect()
        })
}

pub(super) fn legacy_hook_records(config: &ExtensionConfig) -> Vec<CapabilityRecord> {
    let mut records = Vec::new();
    for (event, groups) in hook_events(&config.hooks) {
        for (group_index, group) in groups.iter().enumerate() {
            for (hook_index, hook) in group.hooks.iter().enumerate() {
                let local = hook.name.clone().unwrap_or_else(|| {
                    format!(
                        "legacy-{}-{group_index}-{hook_index}",
                        event.to_ascii_lowercase()
                    )
                });
                let local = legacy_local_id(&local);
                let extension = legacy_package_id(&config.name);
                let Ok(id) = CapabilityId::new(extension, CapabilityKind::Hook, local) else {
                    continue;
                };
                let mut projection = json!({
                    "command": hook.command,
                    "event": event,
                    "id": id.canonical(),
                    "kind": "hook",
                    "matcher": group.matcher,
                    "type": hook.hook_type.as_deref().unwrap_or("command")
                });
                if !hook.env.is_empty() {
                    projection["env"] = json!(hook.env);
                }
                // Fail-open changes whether hook execution failures authorize
                // the guarded action, so enabling it requires fresh consent.
                // Omit false to preserve existing legacy fingerprints.
                if hook.fail_open {
                    projection["fail_open"] = Value::Bool(true);
                }
                records.push(CapabilityRecord {
                    id: id.canonical(),
                    projection,
                });
            }
        }
    }
    records
}

fn validate_and_convert_hooks(
    extension: &str,
    package_root: &Path,
    hooks: ExtensionHooksV1,
    host_executables: &mut BTreeSet<String>,
) -> Result<(ExtensionHooks, Vec<CapabilityRecord>), ManifestError> {
    let mut records = Vec::new();
    let mut ids = BTreeSet::new();
    let mut runtime = ExtensionHooks::default();
    for (event, groups, target) in hooks.into_events(&mut runtime) {
        let mut converted = Vec::new();
        for group in groups {
            if group.hooks.is_empty() {
                return Err(ManifestError::new(
                    "extension_hook_group_empty",
                    format!("hook group for {event} cannot be empty"),
                ));
            }
            let mut runtime_hooks = Vec::new();
            for hook in group.hooks {
                if hook.hook_type != "command" {
                    return Err(ManifestError::new(
                        "extension_hook_type_unsupported",
                        format!("hook {} must use command type", hook.name),
                    ));
                }
                validate_local_id(&hook.name)
                    .map_err(|error| ManifestError::new(error.code(), error.to_string()))?;
                let id = CapabilityId::new(extension, CapabilityKind::Hook, &hook.name)
                    .map_err(|error| ManifestError::new(error.code(), error.to_string()))?;
                if !ids.insert(id.canonical()) {
                    return Err(ManifestError::new(
                        "extension_capability_duplicate",
                        format!("duplicate hook capability: {}", id.canonical()),
                    ));
                }
                if hook
                    .timeout
                    .is_some_and(|timeout| !(1..=300).contains(&timeout))
                {
                    return Err(ManifestError::new(
                        "extension_hook_timeout_invalid",
                        format!(
                            "hook {} timeout must be between 1 and 300 seconds",
                            hook.name
                        ),
                    ));
                }
                validate_hook_command(package_root, &hook.command)?;
                record_host_executable(&hook.command, host_executables);
                for name in hook.env.keys() {
                    if !crate::config::is_valid_env_name(name) {
                        return Err(ManifestError::new(
                            "extension_hook_env_name_invalid",
                            format!("hook {} declares invalid env name: {name}", hook.name),
                        ));
                    }
                }
                let mut projection = json!({
                    "command": normalize_extension_command(&hook.command),
                    "event": event,
                    "id": id.canonical(),
                    "kind": "hook",
                    "matcher": group.matcher,
                    "type": "command"
                });
                // `env` is executable capability, so declaring or changing it
                // must move the fingerprint and force the user to re-consent.
                // Omitted when empty so extensions that never used env keep
                // their existing fingerprint across this upgrade.
                if !hook.env.is_empty() {
                    projection["env"] = json!(hook.env);
                }
                if group.sequential {
                    projection["sequential"] = Value::Bool(true);
                }
                if let Some(timeout) = hook.timeout {
                    projection["timeout"] = Value::Number(timeout.into());
                }
                if hook.fail_open {
                    projection["fail_open"] = Value::Bool(true);
                }
                records.push(CapabilityRecord {
                    id: id.canonical(),
                    projection,
                });
                runtime_hooks.push(CommandHookConfig {
                    hook_type: Some(hook.hook_type),
                    command: hook.command,
                    name: Some(hook.name),
                    description: hook.description,
                    timeout: hook.timeout,
                    fail_open: hook.fail_open,
                    env: hook.env,
                });
            }
            converted.push(HookGroup {
                matcher: group.matcher,
                sequential: Some(group.sequential),
                hooks: runtime_hooks,
            });
        }
        *target = converted;
    }
    Ok((runtime, records))
}

fn hook_events(hooks: &ExtensionHooks) -> [(&'static str, &Vec<HookGroup>); 8] {
    [
        ("PreToolUse", &hooks.pre_tool_use),
        ("PostToolUse", &hooks.post_tool_use),
        ("PostToolUseFailure", &hooks.post_tool_use_failure),
        ("UserPromptSubmit", &hooks.user_prompt_submit),
        ("SessionStart", &hooks.session_start),
        ("Stop", &hooks.stop),
        ("BeforeModel", &hooks.before_model),
        ("AfterModel", &hooks.after_model),
    ]
}

fn validate_relative_package_path(
    package_root: &Path,
    relative: &str,
) -> Result<(), ManifestError> {
    if relative.is_empty()
        || relative.contains(['\0', '\\'])
        || Path::new(relative).is_absolute()
        || Path::new(relative)
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(ManifestError::new(
            "extension_path_invalid",
            format!("invalid extension-relative path: {relative}"),
        ));
    }
    let candidate = package_root.join(relative);
    if candidate.exists() {
        let root = package_root.canonicalize().map_err(|error| {
            ManifestError::new(
                "extension_path_unreadable",
                format!("failed to canonicalize package root: {error}"),
            )
        })?;
        let resolved = candidate.canonicalize().map_err(|error| {
            ManifestError::new(
                "extension_path_unreadable",
                format!("failed to canonicalize package path {relative}: {error}"),
            )
        })?;
        if !resolved.starts_with(root) {
            return Err(ManifestError::new(
                "extension_path_escape",
                format!("extension path escapes package root: {relative}"),
            ));
        }
    }
    Ok(())
}

fn validate_mcp_executable(package_root: &Path, command: &str) -> Result<(), ManifestError> {
    if command.is_empty()
        || command.contains(['\0', '\n', '\r'])
        || command.chars().any(char::is_whitespace)
    {
        return Err(ManifestError::new(
            "extension_command_invalid",
            "MCP command must be one non-empty executable argument",
        ));
    }
    validate_extension_path_references(package_root, command, true, false)
}

fn validate_hook_command(package_root: &Path, command: &str) -> Result<(), ManifestError> {
    if command.trim().is_empty() || command.contains(['\0', '\n', '\r']) {
        return Err(ManifestError::new(
            "extension_command_invalid",
            "hook command must be a non-empty single-line command",
        ));
    }
    validate_extension_path_references(package_root, command, false, true)
}

fn validate_extension_path_references(
    package_root: &Path,
    value: &str,
    executable: bool,
    allow_workspace: bool,
) -> Result<(), ManifestError> {
    const MARKER: &str = "${extensionPath}";
    const WORKSPACE_MARKER: &str = "${workspacePath}";
    const SEPARATOR_MARKER: &str = "${/}";
    let mut remainder = value;
    while let Some(index) = remainder.find("${") {
        let variable = &remainder[index..];
        if allow_workspace && variable.starts_with(WORKSPACE_MARKER) {
            remainder = &variable[WORKSPACE_MARKER.len()..];
            continue;
        }
        if allow_workspace && variable.starts_with(SEPARATOR_MARKER) {
            remainder = &variable[SEPARATOR_MARKER.len()..];
            continue;
        }
        if !variable.starts_with(MARKER) {
            return Err(ManifestError::new(
                "extension_variable_unsupported",
                "command and args only support ${extensionPath}",
            ));
        }
        let suffix = &variable[MARKER.len()..];
        if !suffix.is_empty() && !suffix.starts_with('/') {
            return Err(ManifestError::new(
                "extension_path_invalid",
                "${extensionPath} must be followed by '/' or end the argument",
            ));
        }
        let path_end = suffix
            .find(|character: char| {
                character.is_whitespace()
                    || matches!(character, '\'' | '"' | ';' | '|' | '&' | '>' | '<')
            })
            .unwrap_or(suffix.len());
        let relative = suffix[..path_end].strip_prefix('/').unwrap_or_default();
        if relative.is_empty() {
            if executable {
                return Err(ManifestError::new(
                    "extension_command_invalid",
                    "MCP command must identify a file inside the extension package",
                ));
            }
        } else {
            validate_relative_package_path(package_root, relative)?;
            if executable && !package_root.join(relative).is_file() {
                return Err(ManifestError::new(
                    "extension_command_invalid",
                    format!("MCP executable is not a regular package file: {relative}"),
                ));
            }
        }
        remainder = &variable[MARKER.len() + path_end..];
    }
    Ok(())
}

fn validate_setting(setting: &SettingV1) -> Result<(), ManifestError> {
    if setting.description.len() > 1024 {
        return Err(ManifestError::new(
            "extension_setting_description_too_long",
            format!("setting {} description exceeds 1024 bytes", setting.key),
        ));
    }
    if !matches!(
        setting.setting_type.as_str(),
        "string" | "boolean" | "integer"
    ) {
        return Err(ManifestError::new(
            "extension_setting_type_unsupported",
            format!("unsupported setting type for {}", setting.key),
        ));
    }
    if setting.sensitive && setting.default.is_some() {
        return Err(ManifestError::new(
            "extension_sensitive_setting_default_forbidden",
            format!("sensitive setting {} cannot declare a default", setting.key),
        ));
    }
    if setting.sensitive && setting.setting_type != "string" {
        return Err(ManifestError::new(
            "extension_sensitive_setting_type_invalid",
            format!("sensitive setting {} must use string type", setting.key),
        ));
    }
    if let Some(default) = &setting.default {
        let type_matches = match setting.setting_type.as_str() {
            "string" => default.is_string(),
            "boolean" => default.is_boolean(),
            "integer" => default.as_i64().is_some() || default.as_u64().is_some(),
            _ => false,
        };
        if !type_matches {
            return Err(ManifestError::new(
                "extension_setting_default_type_mismatch",
                format!("setting {} default does not match its type", setting.key),
            ));
        }
    }
    Ok(())
}

fn record_host_executable(command: &str, host_executables: &mut BTreeSet<String>) {
    let first = command.split_whitespace().next().unwrap_or_default();
    if !first.is_empty() && !first.starts_with("${extensionPath}/") && !first.starts_with("./") {
        host_executables.insert(first.to_string());
    }
}

fn normalize_extension_command(command: &str) -> String {
    command
        .strip_prefix("${extensionPath}/")
        .unwrap_or(command)
        .to_string()
}

fn legacy_package_id(value: &str) -> String {
    let normalized = legacy_local_id(value);
    if normalized.is_empty() {
        "legacy".to_string()
    } else {
        normalized
    }
}

fn legacy_local_id(value: &str) -> String {
    let mut normalized = value
        .to_ascii_lowercase()
        .bytes()
        .map(|byte| {
            if byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'.' | b'_' | b'-')
            {
                byte as char
            } else {
                '-'
            }
        })
        .collect::<String>();
    normalized = normalized.trim_matches(['.', '_', '-']).to_string();
    normalized.truncate(super::super::identity::MAX_ID_BYTES);
    while normalized.ends_with(['.', '_', '-']) {
        normalized.pop();
    }
    if normalized.is_empty() {
        "legacy".to_string()
    } else if normalized.as_bytes()[0].is_ascii_digit()
        || normalized.as_bytes()[0].is_ascii_lowercase()
    {
        normalized
    } else {
        format!("legacy-{normalized}")
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ExtensionManifestV1 {
    schema_version: u32,
    name: String,
    version: String,
    #[serde(default)]
    description: Option<String>,
    compatibility: CompatibilityV1,
    #[serde(default)]
    skills: Vec<String>,
    #[serde(default)]
    hooks: ExtensionHooksV1,
    #[serde(default)]
    mcp_servers: BTreeMap<String, McpServerV1>,
    #[serde(default)]
    context_files: Vec<ContextFileV1>,
    #[serde(default)]
    agents: Vec<String>,
    #[serde(default)]
    settings: Vec<SettingV1>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompatibilityV1 {
    cosh: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExtensionHooksV1 {
    #[serde(default, rename = "PreToolUse")]
    pre_tool_use: Vec<HookGroupV1>,
    #[serde(default, rename = "PostToolUse")]
    post_tool_use: Vec<HookGroupV1>,
    #[serde(default, rename = "PostToolUseFailure")]
    post_tool_use_failure: Vec<HookGroupV1>,
    #[serde(default, rename = "UserPromptSubmit")]
    user_prompt_submit: Vec<HookGroupV1>,
    #[serde(default, rename = "SessionStart")]
    session_start: Vec<HookGroupV1>,
    #[serde(default, rename = "Stop")]
    stop: Vec<HookGroupV1>,
    #[serde(default, rename = "BeforeModel")]
    before_model: Vec<HookGroupV1>,
    #[serde(default, rename = "AfterModel")]
    after_model: Vec<HookGroupV1>,
}

type HookEventV1<'a> = (&'static str, Vec<HookGroupV1>, &'a mut Vec<HookGroup>);

impl ExtensionHooksV1 {
    fn into_events<'a>(self, target: &'a mut ExtensionHooks) -> [HookEventV1<'a>; 8] {
        [
            ("PreToolUse", self.pre_tool_use, &mut target.pre_tool_use),
            ("PostToolUse", self.post_tool_use, &mut target.post_tool_use),
            (
                "PostToolUseFailure",
                self.post_tool_use_failure,
                &mut target.post_tool_use_failure,
            ),
            (
                "UserPromptSubmit",
                self.user_prompt_submit,
                &mut target.user_prompt_submit,
            ),
            (
                "SessionStart",
                self.session_start,
                &mut target.session_start,
            ),
            ("Stop", self.stop, &mut target.stop),
            ("BeforeModel", self.before_model, &mut target.before_model),
            ("AfterModel", self.after_model, &mut target.after_model),
        ]
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HookGroupV1 {
    #[serde(default)]
    matcher: Option<String>,
    #[serde(default)]
    sequential: bool,
    hooks: Vec<CommandHookV1>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandHookV1 {
    #[serde(rename = "type")]
    hook_type: String,
    name: String,
    command: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    timeout: Option<u64>,
    #[serde(default)]
    fail_open: bool,
    #[serde(default)]
    env: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct McpServerV1 {
    transport: String,
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    required: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContextFileV1 {
    id: String,
    path: String,
    #[serde(default)]
    required: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingV1 {
    key: String,
    #[serde(rename = "type")]
    setting_type: String,
    description: String,
    #[serde(default)]
    required: bool,
    #[serde(default)]
    sensitive: bool,
    #[serde(default)]
    default: Option<Value>,
}
