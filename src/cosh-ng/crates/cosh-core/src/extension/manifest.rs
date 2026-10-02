//! Versioned extension manifest parsing and security projection validation.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Component, Path, PathBuf};

use semver::{Version, VersionReq};
use serde::Deserialize;
use serde_json::{json, Value};

use super::agent::load_agent_files;
use super::config::{CommandHookConfig, ExtensionConfig, ExtensionHooks, HookGroup, SkillsDirs};
use super::identity::{
    fingerprint_projection, validate_local_id, validate_package_name, validate_setting_key,
    CapabilityId, CapabilityKind,
};
use super::ExtensionDiagnostic;
use crate::skill::loader::load_skills_from_dir;
use crate::skill::SkillLevel;

mod v1;

use v1::{discover_skill_records, legacy_hook_records, parse_v1_manifest};

/// Parsed manifest schema generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ManifestSchemaVersion {
    /// Legacy manifest without `schemaVersion`.
    LegacyV0,
    /// Strict manifest schema version 1.
    V1,
}

/// Validated manifest plus the compatibility runtime projection.
#[derive(Debug, Clone)]
pub struct ParsedManifest {
    /// Parsed schema generation.
    pub schema_version: ManifestSchemaVersion,
    /// Runtime configuration consumed by existing skill and hook owners.
    pub config: ExtensionConfig,
    /// Stable capability security fingerprint.
    pub capability_fingerprint: String,
    /// Canonical capability IDs discovered from the package.
    pub capabilities: Vec<String>,
    /// Non-fatal validation and compatibility diagnostics.
    pub diagnostics: Vec<ExtensionDiagnostic>,
    /// Typed setting schema declared by the package.
    pub settings: Vec<SettingDefinition>,
    /// Validated context contributions in manifest order.
    pub contexts: Vec<ContextContribution>,
    /// Validated MCP server contributions keyed by canonical identity.
    pub mcp_servers: Vec<McpServerContribution>,
    /// Explicit package-relative directories containing agent definitions.
    pub agent_directories: Vec<PathBuf>,
}

/// Scalar type accepted by an extension setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingType {
    /// UTF-8 string value.
    String,
    /// Boolean value.
    Boolean,
    /// Signed 64-bit integer value.
    Integer,
}

/// Validated setting schema without any persisted value.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SettingDefinition {
    /// Manifest key unique within the extension.
    pub key: String,
    /// Expected scalar type.
    pub setting_type: SettingType,
    /// User-facing description supplied by the package.
    pub description: String,
    /// Whether activation requires a resolved value.
    pub required: bool,
    /// Whether the value must live in the operating-system secret store.
    pub sensitive: bool,
    /// Optional non-sensitive manifest default.
    pub default: Option<Value>,
}

/// Validated extension context file contribution.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ContextContribution {
    /// Canonical capability identity.
    pub id: String,
    /// Canonicalized file path inside the package.
    pub path: PathBuf,
    /// Whether failure prevents extension activation.
    pub required: bool,
}

/// Validated stdio MCP server declaration.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct McpServerContribution {
    /// Canonical server capability identity.
    pub id: String,
    /// Local manifest key.
    pub name: String,
    /// Typed executable string resolved by the runtime owner.
    pub command: String,
    /// Exact child process arguments.
    pub args: Vec<String>,
    /// Explicit child environment declarations.
    pub env: BTreeMap<String, String>,
    /// Whether server failure blocks extension activation.
    pub required: bool,
}

/// Stable manifest validation error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestError {
    code: &'static str,
    message: String,
}

impl ManifestError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// Returns the stable diagnostic code.
    pub fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Display for ManifestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ManifestError {}

/// Parses a legacy v0 or strict v1 extension manifest.
pub fn parse_manifest(content: &str, package_root: &Path) -> Result<ParsedManifest, ManifestError> {
    let probe: Value = serde_json::from_str(content).map_err(|error| {
        ManifestError::new(
            "extension_manifest_invalid_json",
            format!("failed to parse extension manifest: {error}"),
        )
    })?;
    match probe.get("schemaVersion") {
        None => parse_legacy_manifest(content, package_root),
        Some(Value::Number(version)) if version.as_u64() == Some(1) => {
            parse_v1_manifest(content, package_root)
        }
        Some(_) => Err(ManifestError::new(
            "extension_manifest_schema_unsupported",
            "schemaVersion must be the integer 1",
        )),
    }
}

fn parse_legacy_manifest(
    content: &str,
    package_root: &Path,
) -> Result<ParsedManifest, ManifestError> {
    let config: ExtensionConfig = serde_json::from_str(content).map_err(|error| {
        ManifestError::new(
            "extension_manifest_v0_invalid",
            format!("failed to parse legacy extension manifest: {error}"),
        )
    })?;
    validate_package_name(&config.name)
        .map_err(|error| ManifestError::new(error.code(), error.to_string()))?;

    let mut records = legacy_hook_records(&config);
    records.extend(discover_skill_records(
        &config.name,
        package_root,
        &config.skills.0,
        false,
    )?);
    records.sort_by(|left, right| left.id.cmp(&right.id));
    let capabilities = records.iter().map(|record| record.id.clone()).collect();
    let projection = json!({
        "capabilities": records.into_iter().map(|record| record.projection).collect::<Vec<_>>(),
        "extension": config.name,
        "hostExecutables": [],
        "policyVersion": 1,
        "settings": []
    });
    let capability_fingerprint = fingerprint_projection(projection).map_err(|error| {
        ManifestError::new(
            "extension_fingerprint_failed",
            format!("failed to fingerprint legacy manifest: {error}"),
        )
    })?;
    Ok(ParsedManifest {
        schema_version: ManifestSchemaVersion::LegacyV0,
        config,
        capability_fingerprint,
        capabilities,
        diagnostics: vec![ExtensionDiagnostic::new(
            "legacy_manifest",
            "manifest has no schemaVersion and uses legacy v0 compatibility",
        )],
        settings: Vec::new(),
        contexts: Vec::new(),
        mcp_servers: Vec::new(),
        agent_directories: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn package_root() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn parses_legacy_manifest_without_schema_version() {
        let root = package_root();
        let parsed = parse_manifest(
            r#"{"name":"legacy-ext","version":"1.0.0","skills":[]}"#,
            root.path(),
        )
        .unwrap();
        assert_eq!(parsed.schema_version, ManifestSchemaVersion::LegacyV0);
        assert_eq!(parsed.diagnostics[0].code, "legacy_manifest");
    }

    #[test]
    fn legacy_manifest_rejects_path_like_package_names() {
        let root = package_root();
        for name in ["..", "../outside", "nested/package", r"nested\package"] {
            let content = serde_json::json!({
                "name": name,
                "version": "1.0.0",
                "skills": []
            })
            .to_string();
            let error = parse_manifest(&content, root.path()).unwrap_err();
            assert_eq!(error.code(), "extension_name_invalid", "name={name}");
        }
    }

    #[test]
    fn v1_rejects_unknown_fields() {
        let root = package_root();
        let error = parse_manifest(
            r#"{
                "schemaVersion":1,
                "name":"example.ops",
                "version":"1.0.0",
                "compatibility":{"cosh":">=0.12.0"},
                "unknown":true
            }"#,
            root.path(),
        )
        .unwrap_err();
        assert_eq!(error.code(), "extension_manifest_v1_invalid");
    }

    #[test]
    fn v1_builds_normative_hook_fingerprint() {
        let root = package_root();
        fs::create_dir_all(root.path().join("hooks")).unwrap();
        let parsed = parse_manifest(
            r#"{
                "schemaVersion":1,
                "name":"example.ops",
                "version":"1.0.0",
                "compatibility":{"cosh":">=0.12.0"},
                "hooks":{"PreToolUse":[{"matcher":"shell","hooks":[{
                    "type":"command",
                    "name":"guard",
                    "command":"${extensionPath}/hooks/guard"
                }]}]}
            }"#,
            root.path(),
        )
        .unwrap();
        assert_eq!(
            parsed.capability_fingerprint,
            "f678fe77434f8ed6a87de660a42db17c06aa29411280150fd92f2c29f8012b13"
        );
        assert_eq!(parsed.capabilities, ["example.ops/hook/guard"]);
    }

    fn v1_hook_manifest(env: &str) -> String {
        format!(
            r#"{{
                "schemaVersion":1,
                "name":"example.ops",
                "version":"1.0.0",
                "compatibility":{{"cosh":">=0.12.0"}},
                "hooks":{{"PreToolUse":[{{"matcher":"shell","hooks":[{{
                    "type":"command",
                    "name":"guard",
                    "command":"${{extensionPath}}/hooks/guard"
                    {env}
                }}]}}]}}
            }}"#
        )
    }

    #[test]
    fn v1_hook_env_is_preserved_and_changes_the_capability_fingerprint() {
        let root = package_root();
        fs::create_dir_all(root.path().join("hooks")).unwrap();

        let without_env = parse_manifest(&v1_hook_manifest(""), root.path()).unwrap();
        let with_env = parse_manifest(
            &v1_hook_manifest(r#","env":{"TOKENLESS_AGENT_ID":"cosh-ng"}"#),
            root.path(),
        )
        .unwrap();

        let hook = &with_env.config.hooks.pre_tool_use[0].hooks[0];
        assert_eq!(
            hook.env.get("TOKENLESS_AGENT_ID").map(String::as_str),
            Some("cosh-ng")
        );
        // env is executable capability, so declaring it must force re-consent.
        assert_ne!(
            with_env.capability_fingerprint,
            without_env.capability_fingerprint
        );
        // Extensions that never declared env keep their existing fingerprint.
        assert_eq!(
            without_env.capability_fingerprint,
            "f678fe77434f8ed6a87de660a42db17c06aa29411280150fd92f2c29f8012b13"
        );
    }

    #[test]
    fn v1_rejects_invalid_hook_env_names() {
        let root = package_root();
        fs::create_dir_all(root.path().join("hooks")).unwrap();
        for name in ["1BAD", "WITH-DASH", "WITH SPACE", "", "a=b"] {
            let env = format!(r#","env":{{"{name}":"value"}}"#);
            let error = parse_manifest(&v1_hook_manifest(&env), root.path()).unwrap_err();
            assert_eq!(
                error.code(),
                "extension_hook_env_name_invalid",
                "name={name}"
            );
        }
    }

    #[test]
    fn legacy_manifest_hook_env_changes_the_capability_fingerprint() {
        let root = package_root();
        let manifest = |env: &str| {
            format!(
                r#"{{"name":"legacy-ext","version":"1.0.0","hooks":{{"PreToolUse":[{{"hooks":[{{
                    "type":"command","name":"guard","command":"echo guard"{env}
                }}]}}]}}}}"#
            )
        };

        let without_env = parse_manifest(&manifest(""), root.path()).unwrap();
        let with_env =
            parse_manifest(&manifest(r#","env":{"AGENT":"cosh-ng"}"#), root.path()).unwrap();

        assert_eq!(
            with_env.config.hooks.pre_tool_use[0].hooks[0]
                .env
                .get("AGENT")
                .map(String::as_str),
            Some("cosh-ng")
        );
        assert_ne!(
            with_env.capability_fingerprint,
            without_env.capability_fingerprint
        );
    }

    #[test]
    fn legacy_manifest_fail_open_requires_fresh_consent() {
        let root = package_root();
        let manifest = |fail_open: &str| {
            format!(
                r#"{{"name":"legacy-ext","version":"1.0.0","hooks":{{"PreToolUse":[{{"hooks":[{{
                    "type":"command","name":"guard","command":"echo guard"{fail_open}
                }}]}}]}}}}"#
            )
        };

        let omitted = parse_manifest(&manifest(""), root.path()).unwrap();
        let explicit_false =
            parse_manifest(&manifest(r#", "fail_open":false"#), root.path()).unwrap();
        let enabled = parse_manifest(&manifest(r#", "fail_open":true"#), root.path()).unwrap();

        assert_eq!(
            omitted.capability_fingerprint,
            explicit_false.capability_fingerprint
        );
        assert_ne!(
            enabled.capability_fingerprint,
            omitted.capability_fingerprint
        );
    }

    #[test]
    fn v1_rejects_path_escape() {
        let root = package_root();
        let error = parse_manifest(
            r#"{
                "schemaVersion":1,
                "name":"example.ops",
                "version":"1.0.0",
                "compatibility":{"cosh":">=0.12.0"},
                "skills":["../outside"]
            }"#,
            root.path(),
        )
        .unwrap_err();
        assert_eq!(error.code(), "extension_path_invalid");
    }

    #[test]
    fn v1_exposes_mcp_runtime_contribution() {
        let root = package_root();
        let parsed = parse_manifest(
            r#"{
                "schemaVersion":1,
                "name":"example.ops",
                "version":"1.0.0",
                "compatibility":{"cosh":">=0.12.0"},
                "mcpServers":{"inventory":{"transport":"stdio","command":"inventory-mcp"}}
            }"#,
            root.path(),
        )
        .unwrap();
        assert!(parsed.diagnostics.is_empty());
        assert_eq!(parsed.mcp_servers.len(), 1);
        assert!(!parsed.mcp_servers[0].required);
    }

    #[test]
    fn v1_rejects_extension_path_escape_in_hook_command() {
        let root = package_root();
        let error = parse_manifest(
            r#"{
                "schemaVersion":1,
                "name":"example.ops",
                "version":"1.0.0",
                "compatibility":{"cosh":">=0.12.0"},
                "hooks":{"PreToolUse":[{"hooks":[{
                    "type":"command",
                    "name":"guard",
                    "command":"${extensionPath}/../outside"
                }]}]}
            }"#,
            root.path(),
        )
        .unwrap_err();
        assert_eq!(error.code(), "extension_path_invalid");
    }

    #[test]
    fn v1_rejects_extension_path_escape_in_mcp_argv() {
        let root = package_root();
        fs::create_dir_all(root.path().join("bin")).unwrap();
        fs::write(root.path().join("bin/server"), "fixture").unwrap();
        let error = parse_manifest(
            r#"{
                "schemaVersion":1,
                "name":"example.ops",
                "version":"1.0.0",
                "compatibility":{"cosh":">=0.12.0"},
                "mcpServers":{"inventory":{
                    "transport":"stdio",
                    "command":"${extensionPath}/bin/server",
                    "args":["--config=${extensionPath}/../outside"]
                }}
            }"#,
            root.path(),
        )
        .unwrap_err();
        assert_eq!(error.code(), "extension_path_invalid");
    }

    #[test]
    fn v1_accepts_absolute_host_executable() {
        let root = package_root();
        let parsed = parse_manifest(
            r#"{
                "schemaVersion":1,
                "name":"example.ops",
                "version":"1.0.0",
                "compatibility":{"cosh":">=0.12.0"},
                "mcpServers":{"inventory":{
                    "transport":"stdio",
                    "command":"/usr/bin/inventory-mcp"
                }}
            }"#,
            root.path(),
        )
        .unwrap();
        assert!(!parsed.capability_fingerprint.is_empty());
    }

    #[test]
    fn v1_rejects_sensitive_setting_default() {
        let root = package_root();
        let error = parse_manifest(
            r#"{
                "schemaVersion":1,
                "name":"example.ops",
                "version":"1.0.0",
                "compatibility":{"cosh":">=0.12.0"},
                "settings":[{
                    "key":"inventoryToken",
                    "type":"string",
                    "description":"token",
                    "sensitive":true,
                    "default":"secret"
                }]
            }"#,
            root.path(),
        )
        .unwrap_err();
        assert_eq!(
            error.code(),
            "extension_sensitive_setting_default_forbidden"
        );
    }

    #[test]
    fn v1_rejects_non_string_sensitive_setting() {
        let root = package_root();
        let error = parse_manifest(
            r#"{
                "schemaVersion":1,
                "name":"example.ops",
                "version":"1.0.0",
                "compatibility":{"cosh":">=0.12.0"},
                "settings":[{
                    "key":"secretFlag",
                    "type":"boolean",
                    "description":"secret flag",
                    "sensitive":true
                }]
            }"#,
            root.path(),
        )
        .unwrap_err();
        assert_eq!(error.code(), "extension_sensitive_setting_type_invalid");
    }
}
