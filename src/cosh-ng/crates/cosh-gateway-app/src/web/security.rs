//! Descriptor-first credential validation for the loopback Web adapter.

use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

use cosh_gateway::daemon::{GatewayCapabilities, GatewayResult, LocalGatewayClient};
use cosh_gateway::runtime::TrustedWorkspaceResolver;
use cosh_gateway_contracts::{
    common::WorkspaceRef,
    ids::RequestId,
    profile::GatewayCapabilityProfile,
    task::{TaskRuntime, TASK_LAUNCH_SPEC_V1},
};

use super::{CliError, MAX_TOKEN_BYTES};

pub(super) fn attest_gateway(
    client: &LocalGatewayClient,
    workspace: &Path,
) -> Result<(), CliError> {
    let resolver = TrustedWorkspaceResolver::new(
        GatewayCapabilityProfile::task_only_v1().governed_target(),
        workspace,
    )
    .map_err(|error| CliError::Web(error.safe_message.as_str().to_owned()))?;
    let result = client
        .capabilities(RequestId::new())
        .map_err(|error| CliError::Web(format!("cannot attest Gateway capabilities: {error}")))?;
    let GatewayResult::Capabilities(capabilities) = result else {
        return Err(CliError::Web(
            "Gateway did not return capabilities".to_owned(),
        ));
    };
    validate_capabilities(&capabilities, resolver.workspace_ref())
}

fn validate_capabilities(
    capabilities: &GatewayCapabilities,
    workspace: &WorkspaceRef,
) -> Result<(), CliError> {
    if capabilities.launch_schema_version != TASK_LAUNCH_SPEC_V1
        || capabilities.default_workspace.scope_digest != workspace.scope_digest
    {
        return Err(CliError::Web(
            "Gateway launch schema or admitted workspace does not match Web configuration"
                .to_owned(),
        ));
    }
    // Unavailable entries still describe authority of recoverable historical Tasks.
    // Moving the token outside a workspace cannot isolate it from local-user authority.
    if capabilities.runtimes.len() != 2
        || [TaskRuntime::Core, TaskRuntime::Codex]
            .into_iter()
            .any(|runtime| {
                capabilities
                    .runtimes
                    .iter()
                    .filter(|entry| entry.runtime == runtime)
                    .count()
                    != 1
            })
        || capabilities.runtimes.iter().any(|entry| {
            entry.security.delegated_local_authority || !entry.security.gateway_brokered_effects
        })
    {
        return Err(CliError::Web(
            "Gateway cannot attest a brokered-only token boundary; current Core/Codex local-user authority is unsupported by Web; use the Task CLI".to_owned(),
        ));
    }
    Ok(())
}

#[derive(Debug)]
pub(super) struct LoadedToken {
    pub(super) bytes: Vec<u8>,
    pub(super) path: PathBuf,
}

pub(super) fn canonical_workspace(path: &Path) -> Result<PathBuf, CliError> {
    if !path.is_absolute() {
        return Err(CliError::Web("workspace path must be absolute".to_owned()));
    }
    let canonical =
        std::fs::canonicalize(path).map_err(|error| CliError::Web(error.to_string()))?;
    if !canonical.is_dir() {
        return Err(CliError::Web(
            "workspace must be an existing directory".to_owned(),
        ));
    }
    Ok(canonical)
}

pub(super) fn read_token(path: &Path) -> Result<LoadedToken, CliError> {
    if !path.is_absolute() {
        return Err(CliError::Web(
            "Bearer token path must be absolute".to_owned(),
        ));
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
        .open(path)
        .map_err(|error| CliError::Web(error.to_string()))?;
    let opened = file
        .metadata()
        .map_err(|error| CliError::Web(error.to_string()))?;
    if !opened.file_type().is_file() || opened.mode() & 0o777 != 0o600 || opened.nlink() != 1 {
        return Err(CliError::Web(
            "Bearer token must be a single-link regular file with mode 0600".to_owned(),
        ));
    }
    let owner = opened.uid();
    let effective = nix::unistd::Uid::effective().as_raw();
    if owner != 0 && owner != effective {
        return Err(CliError::Web(
            "Bearer token must be owned by root or the current user".to_owned(),
        ));
    }
    let opened_path = std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))
        .map_err(|error| CliError::Web(format!("cannot resolve opened Bearer token: {error}")))?;
    if !opened_path.is_absolute()
        || opened_path
            .as_os_str()
            .to_string_lossy()
            .ends_with(" (deleted)")
    {
        return Err(CliError::Web(
            "opened Bearer token has no stable absolute path".to_owned(),
        ));
    }
    validate_token_ancestors(&opened_path, effective)?;
    let mut token = Vec::new();
    (&mut file)
        .take((MAX_TOKEN_BYTES + 1) as u64)
        .read_to_end(&mut token)
        .map_err(|error| CliError::Web(error.to_string()))?;
    while token.last().is_some_and(u8::is_ascii_whitespace) {
        token.pop();
    }
    if token.len() < 32 || token.len() > MAX_TOKEN_BYTES || !token.iter().all(u8::is_ascii_graphic)
    {
        return Err(CliError::Web(
            "Bearer token must contain 32 to 256 printable ASCII bytes".to_owned(),
        ));
    }
    Ok(LoadedToken {
        bytes: token,
        path: opened_path,
    })
}

fn validate_token_ancestors(path: &Path, effective_uid: u32) -> Result<(), CliError> {
    let parent = path
        .parent()
        .ok_or_else(|| CliError::Web("Bearer token has no parent directory".to_owned()))?;
    for directory in parent.ancestors() {
        let metadata = std::fs::symlink_metadata(directory)
            .map_err(|error| CliError::Web(error.to_string()))?;
        let mode = metadata.mode();
        let sticky = mode & 0o1000 != 0;
        if !metadata.file_type().is_dir()
            || (metadata.uid() != 0 && metadata.uid() != effective_uid)
            || (mode & 0o022 != 0 && !sticky)
        {
            return Err(CliError::Web(format!(
                "Bearer token ancestor {} is not trusted and private",
                directory.display()
            )));
        }
    }
    Ok(())
}

pub(super) fn validate_token_scope(token_path: &Path, workspace: &Path) -> Result<(), CliError> {
    if token_path.starts_with(workspace) {
        return Err(CliError::Web(
            "Bearer token must be outside the admitted workspace".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosh_gateway::daemon::{LaunchReadiness, TaskLaunchCatalog};
    use cosh_gateway_contracts::common::{BoundedText, Digest};

    #[test]
    fn attestation_rejects_unknown_catalogs_and_unavailable_delegated_authority() {
        let workspace = WorkspaceRef {
            scope_digest: Digest::parse("a".repeat(64)).unwrap(),
            display_name: None,
        };
        let unavailable = LaunchReadiness::unavailable(BoundedText::new("disabled").unwrap());
        let mut capabilities = TaskLaunchCatalog::new(
            workspace.clone(),
            unavailable.clone(),
            unavailable.clone(),
            unavailable,
        )
        .capabilities();
        assert!(validate_capabilities(&capabilities, &workspace).is_err());
        for entry in &mut capabilities.runtimes {
            entry.security.delegated_local_authority = false;
            entry.security.gateway_brokered_effects = true;
        }
        assert!(validate_capabilities(&capabilities, &workspace).is_ok());
        capabilities.launch_schema_version += 1;
        assert!(validate_capabilities(&capabilities, &workspace).is_err());
        capabilities.launch_schema_version = TASK_LAUNCH_SPEC_V1;
        capabilities.runtimes[0].security.gateway_brokered_effects = false;
        assert!(validate_capabilities(&capabilities, &workspace).is_err());
        capabilities.runtimes[0].security.gateway_brokered_effects = true;
        capabilities.runtimes[0].runtime = TaskRuntime::Codex;
        assert!(validate_capabilities(&capabilities, &workspace).is_err());
        capabilities.runtimes.clear();
        assert!(validate_capabilities(&capabilities, &workspace).is_err());
    }
}
