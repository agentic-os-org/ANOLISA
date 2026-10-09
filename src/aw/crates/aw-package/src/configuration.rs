//! One configuration shares sec-core policy across Qoder Bash and OpenClaw exec.

use crate::{
    filesystem as paths,
    install::{receipts, verified_files},
    require, Result,
};
use serde_json::json;
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
    /// Existing system sec-core daemon socket.
    pub socket: &'a Path,
    /// Qoder CLI 1.1.64 executable.
    pub qoder: &'a Path,
    /// Node executable used by the pinned OpenClaw installation.
    pub node: &'a Path,
    /// OpenClaw 2026.9.6 openclaw.mjs entrypoint.
    pub openclaw: &'a Path,
}

/// Exclusively create a private, schema-validated dual-Agent configuration.
/// Agent versions are checked by `aw run`; configuration creation never invokes a model.
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
        socket,
        qoder,
        node,
        openclaw,
    } = settings;
    let _lock = paths::shared_lock(prefix)?;
    let installed = receipts(prefix)?;
    require(
        installed.len() == 2
            && installed.contains_key("aw-core")
            && installed.contains_key("aw-provider-sec-core"),
        "install both AW core and sec-core Provider before configuring",
    )?;
    verified_files(prefix, &installed)?;
    for path in [*config, *state, *socket] {
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
    for (name, path) in [
        ("AW state", state.join("aw.sock")),
        ("sec-core", socket.to_path_buf()),
    ] {
        require(
            path.as_os_str().as_bytes().len() < 108,
            format!("{name} path is too long for a Unix socket"),
        )?;
    }
    for (path, access, permission) in [
        (*qoder, libc::X_OK, "executable"),
        (*node, libc::X_OK, "executable"),
        (*openclaw, libc::R_OK, "readable"),
    ] {
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
            unsafe { libc::faccessat(libc::AT_FDCWD, name.as_ptr(), access, libc::AT_EACCESS) }
                == 0,
            format!(
                "Agent entrypoint must be {permission} by the current user: {}",
                path.display()
            ),
        )?;
    }
    let provider = prefix.join("libexec/aw/providers/sec-core");
    let value = json!({"apiVersion":"aw/v1alpha1","kind":"AWConfiguration",
        "metadata":{"name":"dual-agent-sec-core-preview"},"spec":{
        "daemon":{"startup":"on_demand","state_dir":state,"endpoint":"auto"},
        "execution":{"guarantee":"native_hook","default_event_budget_ms":5000},
        "audit":{"enabled":true,"payload":"metadata_only"},
        "agents":{
            "qoder":{"adapter":"qoder","argv":[qoder]},
            "openclaw":{"adapter":"openclaw","argv":[node,openclaw,"gateway","run"]}},
        "providers":{"security":{
            "protocol":"aw-provider/v1alpha1",
            "transport":{"type":"stdio","location":"agent","argv":[provider.join("aw-provider-sec-core"),"--cli",provider.join("agent-sec-cli"),"--socket",socket]},
            "timeout_ms":2000,"max_output_bytes":4096,
            "config":{"version":1,"mode":"block","tools":{
                "Bash":{"language":"bash","input_pointer":"/command"},
                "exec":{"language":"bash","input_pointer":"/command"}}}}},
        "events":{
            "tool.before":{"enabled":true,"required":true,"steps":[{"id":"scan-code","provider":"security","operation":"scan_code","effects":["observe","block"],"on_error":"block"}]},
            "tool.after":{"enabled":true,"required":true,"steps":[{"id":"observe-completion","provider":"security","operation":"observe_tool","effects":["observe"],"on_error":"report"}]}}
    }});
    let yaml = serde_yaml_ng::to_string(&value)?;
    aw_config::Validator::new()
        .and_then(|validator| validator.parse(yaml.as_bytes()))
        .map_err(|error| crate::Error::Invalid(error.to_string()))?;
    publish(config, yaml.as_bytes())
}
