//! Private configuration publication for selected Agents and explicit policy templates.

use crate::{filesystem as paths, require, Result};
mod installed;
mod template;
mod templates;

use serde_json::Value;
use std::{
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::Path,
};

/// External locations and native Agent executables for one AW configuration.
pub struct Settings<'a> {
    /// Immutable installed package prefix.
    pub prefix: &'a Path,
    /// New configuration file, outside the prefix.
    pub config: &'a Path,
    /// AW-owned runtime/audit directory, outside the prefix.
    pub state: &'a Path,
    /// Explicit policy selection; installed Providers are never enabled implicitly.
    pub policy: Policy<'a>,
    /// Qoder CLI executable with compatible native command Hooks.
    pub qoder: Option<&'a Path>,
    /// Node executable used by the selected OpenClaw installation.
    pub node: Option<&'a Path>,
    /// OpenClaw openclaw.mjs entrypoint.
    pub openclaw: Option<&'a Path>,
}

/// Policy template applied to a newly generated configuration.
pub enum Policy<'a> {
    /// No business policy or Provider calls.
    None,
    /// Explicit distributor or Provider document; AW never selects a business preset.
    Template {
        /// Generic template with package requirements, Providers and events.
        definition: Value,
        /// Literal values used for named template placeholders.
        variables: std::collections::BTreeMap<String, String>,
    },
    /// A command returning a JSON boolean; AW constructs protocol replies.
    Command {
        /// Literal command arguments; `argv[0]` must be an absolute executable.
        argv: &'a [String],
        /// Effect requested when the command returns true: block or observe.
        effect: &'a str,
        /// Machine-readable reason for a true result.
        reason_code: &'a str,
    },
}

/// Exclusively create a private, schema-validated configuration.
/// No template executes user commands or starts a Provider during generation.
/// Native compatibility is checked by `aw run`; configuration creation never invokes a model.
pub fn configure(settings: &Settings<'_>) -> Result<()> {
    configure_using(settings, paths::write_private)
}

/// Hold package read ownership through configuration validation and publication.
pub(crate) fn configure_using(
    settings: &Settings<'_>,
    publish: impl FnOnce(&Path, &[u8]) -> Result<()>,
) -> Result<()> {
    let Settings {
        prefix,
        config,
        state,
        policy,
        qoder,
        node,
        openclaw,
    } = settings;
    let requirements = template::required_packages(policy)?;
    let names: Vec<_> = requirements.iter().map(String::as_str).collect();
    let selection = installed::Selection::read(prefix, &names)?;
    for name in &requirements {
        require(
            selection.packages.contains_key(name),
            format!("install required template package: {name}"),
        )?;
    }
    selection.verify(prefix)?;
    template::verify_files(policy, &selection.packages)?;
    for path in [*config, *state] {
        paths::absolute(path)?;
    }
    require(
        !state.starts_with(config) && (!state.exists() || state.is_dir()),
        "state must be a directory distinct from the configuration file",
    )?;
    paths::parent(config)?;
    paths::parent(state)?;
    if state.exists() {
        paths::owned_directory(state)?;
        require(
            state.metadata()?.mode() & 0o7777 == 0o700,
            format!(
                "existing state directory must have mode 0700: {}",
                state.display()
            ),
        )?;
    }
    require(
        !config.starts_with(prefix) && !state.starts_with(prefix),
        "configuration and state must be outside the immutable prefix",
    )?;
    socket_length("AW state", &state.join("aw.sock"))?;
    require(
        qoder.is_some() || openclaw.is_some(),
        "select at least one Agent entrypoint",
    )?;
    require(
        node.is_some() == openclaw.is_some(),
        "--node and --openclaw must be provided together",
    )?;
    for (path, access, permission) in [
        (*qoder, libc::X_OK, "executable"),
        (*node, libc::X_OK, "executable"),
        (*openclaw, libc::R_OK, "readable"),
    ] {
        let Some(path) = path else { continue };
        entrypoint(path, access, permission)?;
    }
    if let Policy::Command { argv, .. } = policy {
        if let Some(program) = argv.first() {
            entrypoint(Path::new(program), libc::X_OK, "executable")?;
        }
    }
    let value = templates::document(settings, Some(&selection.packages))?;
    let yaml = serde_yaml_ng::to_string(&value)?;
    aw_config::Validator::new()
        .and_then(|validator| validator.parse(yaml.as_bytes()))
        .map_err(|error| crate::Error::Invalid(error.to_string()))?;
    selection.verify(prefix)?;
    publish(config, yaml.as_bytes())
}

fn socket_length(name: &str, path: &Path) -> Result<()> {
    require(
        path.as_os_str().as_bytes().len() < 108,
        format!("{name} path is too long for a Unix socket"),
    )
}

/// Construct a complete desired document without filesystem writes or command execution.
/// Publication separately verifies installed components, entrypoints and private paths.
///
/// # Errors
/// Rejects inconsistent Agent inputs or invalid command template parameters.
pub fn document(settings: &Settings<'_>) -> Result<Value> {
    templates::document(settings, None)
}

fn entrypoint(path: &Path, access: i32, permission: &str) -> Result<()> {
    require(
        path.is_absolute() && path.is_file(),
        format!(
            "Agent entrypoint must be an existing absolute file: {}",
            path.display()
        ),
    )?;
    let name = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
    // SAFETY: name is NUL-terminated and live; use the launcher's effective identity.
    require(
        unsafe { libc::faccessat(libc::AT_FDCWD, name.as_ptr(), access, libc::AT_EACCESS) } == 0,
        format!(
            "Agent entrypoint must be {permission} by the current user: {}",
            path.display()
        ),
    )?;
    Ok(())
}
