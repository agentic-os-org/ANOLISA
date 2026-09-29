//! Rendering and error-mapping helpers for the `install` command.

use anolisa_core::ArtifactType;

use crate::repo_config::RepoConfigError;
use crate::response::CliError;

/// Route a [`RepoConfigError`] to the CLI error surface.
///
/// `caller_fixable` decides the bucket: selection/substitution/override
/// errors are actionable by the caller (pass a different `--backend`,
/// fix `[vars]`, fix the `--repo` URL) → INVALID_ARGUMENT (exit 2);
/// discovery/IO/parse failures mean the config asset itself is broken →
/// EXECUTION_FAILED (exit 1), mirroring the execution-policy split.
pub(crate) fn repo_config_err(err: RepoConfigError, caller_fixable: bool) -> CliError {
    if caller_fixable {
        CliError::InvalidArgument {
            command: super::COMMAND.to_string(),
            reason: err.to_string(),
        }
    } else {
        CliError::Runtime {
            command: super::COMMAND.to_string(),
            reason: format!("failed to load repo config: {err}"),
        }
    }
}

/// `{ext}` placeholder value for the conventional file name. Single-file
/// artifacts ship bare; OCI rows are references, not downloadable files,
/// and never resolve through URL derivation.
pub(crate) fn artifact_ext(t: &ArtifactType) -> &'static str {
    match t {
        ArtifactType::TarGz => ".tar.gz",
        ArtifactType::Zip => ".zip",
        ArtifactType::Rpm => ".rpm",
        ArtifactType::Deb => ".deb",
        ArtifactType::Binary | ArtifactType::File | ArtifactType::Oci => "",
    }
}

/// Wire-form artifact type string for the install runner.
pub(crate) fn artifact_type_wire(t: &ArtifactType) -> &'static str {
    match t {
        ArtifactType::TarGz => "tar_gz",
        ArtifactType::Binary => "binary",
        ArtifactType::Rpm => "rpm",
        ArtifactType::Deb => "deb",
        ArtifactType::Zip => "zip",
        ArtifactType::Oci => "oci",
        ArtifactType::File => "file",
    }
}
