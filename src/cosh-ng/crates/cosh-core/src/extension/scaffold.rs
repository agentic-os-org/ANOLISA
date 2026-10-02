//! Safe manifest v1 extension scaffolding without installation or activation.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{json, Value};

use super::manifest::parse_manifest;
use super::EXTENSION_CONFIG_FILENAME;

/// Supported phase 1 extension scaffold templates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExtensionTemplate {
    /// Manifest-only package.
    Minimal,
    /// Package containing one example skill.
    Skill,
    /// Package containing one command hook.
    Hook,
    /// Package declaring a phase 2 MCP server.
    Mcp,
    /// Package declaring a phase 2 context file.
    Context,
    /// Package declaring a phase 4 agent.
    Agent,
}

impl ExtensionTemplate {
    /// Parses a user-facing template name.
    pub fn parse(value: &str) -> Result<Self, ScaffoldError> {
        match value {
            "minimal" => Ok(Self::Minimal),
            "skill" => Ok(Self::Skill),
            "hook" => Ok(Self::Hook),
            "mcp" => Ok(Self::Mcp),
            "context" => Ok(Self::Context),
            "agent" => Ok(Self::Agent),
            _ => Err(ScaffoldError::new(
                "extension_template_unsupported",
                format!("unsupported extension template: {value}"),
            )),
        }
    }
}

/// Result of creating a package scaffold.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct ScaffoldResult {
    /// Package identity derived from the target directory name.
    pub name: String,
    /// Absolute package directory created by the operation.
    pub path: PathBuf,
    /// Selected template.
    pub template: ExtensionTemplate,
    /// Manifest schema version.
    pub schema_version: u32,
    /// Whether declared capabilities are executable in phase 1.
    pub executable_in_phase_one: bool,
}

/// Creates a new extension package without installing or enabling it.
pub fn scaffold_extension(
    target: &Path,
    template: ExtensionTemplate,
) -> Result<ScaffoldResult, ScaffoldError> {
    if target.exists() {
        return Err(ScaffoldError::new(
            "extension_scaffold_target_exists",
            format!("target already exists: {}", target.display()),
        ));
    }
    let name = target
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            ScaffoldError::new(
                "extension_scaffold_name_invalid",
                "target directory must have a UTF-8 package name",
            )
        })?;
    super::identity::validate_package_name(name)
        .map_err(|error| ScaffoldError::new(error.code(), error.to_string()))?;
    let absolute = absolute_target(target)?;
    fs::create_dir(&absolute).map_err(|error| {
        ScaffoldError::new(
            "extension_scaffold_write_failed",
            format!("failed to create {}: {error}", absolute.display()),
        )
    })?;
    let created = (|| {
        let manifest = build_template(&absolute, name, template)?;
        let bytes = serde_json::to_vec_pretty(&manifest).map_err(|error| {
            ScaffoldError::new(
                "extension_scaffold_write_failed",
                format!("failed to serialize manifest: {error}"),
            )
        })?;
        fs::write(absolute.join(EXTENSION_CONFIG_FILENAME), bytes).map_err(|error| {
            ScaffoldError::new(
                "extension_scaffold_write_failed",
                format!("failed to write extension manifest: {error}"),
            )
        })?;
        let content =
            fs::read_to_string(absolute.join(EXTENSION_CONFIG_FILENAME)).map_err(|error| {
                ScaffoldError::new(
                    "extension_scaffold_validation_failed",
                    format!("failed to reread generated manifest: {error}"),
                )
            })?;
        parse_manifest(&content, &absolute).map_err(|error| {
            ScaffoldError::new(
                "extension_scaffold_validation_failed",
                format!("generated package failed validation: {error}"),
            )
        })?;
        Ok(ScaffoldResult {
            name: name.to_string(),
            path: absolute.clone(),
            template,
            schema_version: 1,
            executable_in_phase_one: !matches!(
                template,
                ExtensionTemplate::Mcp | ExtensionTemplate::Context | ExtensionTemplate::Agent
            ),
        })
    })();
    if created.is_err() {
        let _ = fs::remove_dir_all(&absolute);
    }
    created
}

fn absolute_target(target: &Path) -> Result<PathBuf, ScaffoldError> {
    if target.is_absolute() {
        return Ok(target.to_path_buf());
    }
    std::env::current_dir()
        .map(|current| current.join(target))
        .map_err(|error| {
            ScaffoldError::new(
                "extension_scaffold_path_unavailable",
                format!("failed to resolve current directory: {error}"),
            )
        })
}

fn build_template(
    root: &Path,
    name: &str,
    template: ExtensionTemplate,
) -> Result<Value, ScaffoldError> {
    let mut manifest = json!({
        "schemaVersion": 1,
        "name": name,
        "version": "0.1.0",
        "description": format!("{name} extension"),
        "compatibility": {"cosh": ">=0.12.0"}
    });
    let fields = manifest.as_object_mut().ok_or_else(|| {
        ScaffoldError::new(
            "extension_scaffold_validation_failed",
            "generated manifest is not an object",
        )
    })?;
    match template {
        ExtensionTemplate::Minimal => {}
        ExtensionTemplate::Skill => {
            let directory = root.join("skills/example");
            fs::create_dir_all(&directory).map_err(scaffold_write_error)?;
            fs::write(
                directory.join("SKILL.md"),
                "---\nname: example\ndescription: Example extension skill\n---\n\n# Example\n",
            )
            .map_err(scaffold_write_error)?;
            fields.insert("skills".to_string(), json!(["skills"]));
        }
        ExtensionTemplate::Hook => {
            let directory = root.join("hooks");
            fs::create_dir_all(&directory).map_err(scaffold_write_error)?;
            let hook = directory.join("guard.sh");
            fs::write(&hook, "#!/bin/sh\nexit 0\n").map_err(scaffold_write_error)?;
            set_executable(&hook)?;
            fields.insert(
                "hooks".to_string(),
                json!({
                    "PreToolUse": [{
                        "matcher": "shell",
                        "hooks": [{
                            "type": "command",
                            "name": "guard",
                            "command": "${extensionPath}/hooks/guard.sh"
                        }]
                    }]
                }),
            );
        }
        ExtensionTemplate::Mcp => {
            let directory = root.join("mcp");
            fs::create_dir_all(&directory).map_err(scaffold_write_error)?;
            fs::write(
                directory.join("README.md"),
                "# MCP server\n\nReplace the manifest command with a bundled executable or an explicitly consented host executable.\n",
            )
            .map_err(scaffold_write_error)?;
            fields.insert(
                "mcpServers".to_string(),
                json!({"example": {"transport": "stdio", "command": "example-mcp"}}),
            );
        }
        ExtensionTemplate::Context => {
            let directory = root.join("context");
            fs::create_dir_all(&directory).map_err(scaffold_write_error)?;
            fs::write(directory.join("example.md"), "# Example context\n")
                .map_err(scaffold_write_error)?;
            fields.insert(
                "contextFiles".to_string(),
                json!([{"id": "example", "path": "context/example.md", "required": false}]),
            );
        }
        ExtensionTemplate::Agent => {
            let directory = root.join("agents");
            fs::create_dir_all(&directory).map_err(scaffold_write_error)?;
            fs::write(
                directory.join("example.md"),
                "---\nname: example\ntools: []\nskills: []\nmcpServers: []\n---\n\n# Example agent\n",
            )
            .map_err(scaffold_write_error)?;
            fields.insert("agents".to_string(), json!(["agents"]));
        }
    }
    Ok(manifest)
}

fn scaffold_write_error(error: std::io::Error) -> ScaffoldError {
    ScaffoldError::new(
        "extension_scaffold_write_failed",
        format!("failed to write scaffold file: {error}"),
    )
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<(), ScaffoldError> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path)
        .map_err(scaffold_write_error)?
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).map_err(scaffold_write_error)
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<(), ScaffoldError> {
    Ok(())
}

/// Stable scaffold failure for registry and slash protocol consumers.
#[derive(Debug)]
pub struct ScaffoldError {
    code: &'static str,
    message: String,
}

impl ScaffoldError {
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

impl fmt::Display for ScaffoldError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ScaffoldError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_template_generates_a_valid_v1_package() {
        for template in [
            ExtensionTemplate::Minimal,
            ExtensionTemplate::Skill,
            ExtensionTemplate::Hook,
            ExtensionTemplate::Mcp,
            ExtensionTemplate::Context,
            ExtensionTemplate::Agent,
        ] {
            let temporary = tempfile::tempdir().unwrap();
            let target = temporary.path().join("example.scaffold");
            let result = scaffold_extension(&target, template).unwrap();
            assert_eq!(result.schema_version, 1);
            assert!(target.join(EXTENSION_CONFIG_FILENAME).is_file());
            assert_eq!(
                result.executable_in_phase_one,
                !matches!(
                    template,
                    ExtensionTemplate::Mcp | ExtensionTemplate::Context | ExtensionTemplate::Agent
                )
            );
        }
    }

    #[test]
    fn existing_target_is_not_modified() {
        let temporary = tempfile::tempdir().unwrap();
        let target = temporary.path().join("example.existing");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("keep"), "data").unwrap();
        let error = scaffold_extension(&target, ExtensionTemplate::Minimal).unwrap_err();
        assert_eq!(error.code(), "extension_scaffold_target_exists");
        assert_eq!(fs::read_to_string(target.join("keep")).unwrap(), "data");
    }
}
