//! Shared types for the `install` command: resolution shapes and execution
//! inputs consumed by the planner-driven pipeline.

use std::path::PathBuf;

use anolisa_core::{
    CapabilityRequest, ComponentManifest, DistributionEntry, ResolvedInstallFile, ServiceRequest,
};

/// Raw backend resolution shared by the install pipeline and execution.
///
/// `pub(crate)` so the `update` command can reuse the same resolution shape
/// when refreshing a raw-managed component to the latest published version.
pub(crate) struct RawResolution {
    pub(crate) component: String,
    pub(crate) package: String,
    pub(crate) entry: DistributionEntry,
    pub(crate) artifact_url: String,
    /// Repository base URL the index was fetched from, kept so a
    /// version-pinned install can attribute the resolved candidate to its
    /// source repository in the result envelope.
    pub(crate) base_url: String,
    pub(crate) warnings: Vec<String>,
}

/// Execution input after the artifact has been verified and its install
/// contract has been resolved.
///
/// `pub(crate)` so the `update` command can drive the same download-verify
/// step and then replace the on-disk files transactionally.
pub(crate) struct PreparedInstall {
    pub(crate) resolution: RawResolution,
    pub(crate) artifact_path: PathBuf,
    pub(crate) files: Vec<ResolvedInstallFile>,
    /// Declared service activations (unit + scope + enable/start), applied
    /// after files land. Carried resolved with template instances expanded.
    pub(crate) services: Vec<ServiceRequest>,
    /// Linux file capabilities to apply after files land (raw, system mode
    /// only). Carried resolved — path already layout-expanded and bounded.
    pub(crate) capabilities: Vec<CapabilityRequest>,
    pub(crate) manifest_toml: String,
}

/// Parsed install contract plus the TOML persisted as the local install fact.
pub(crate) struct LoadedInstallContract {
    pub(crate) manifest: ComponentManifest,
    pub(crate) source: InstallContractSource,
    pub(crate) toml: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InstallContractSource {
    EmbeddedArtifact,
    SidecarMeta,
}

/// Source that selected the raw repository URL for this resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RawRepositoryOrigin {
    /// URL loaded from the active repository configuration file.
    Config(PathBuf),
    /// URL supplied by the invocation's one-off `--repo` option.
    CliOverride,
}

/// What `handle_one` did, so `--all` can distinguish outcomes in its batch
/// summary (§7.5). The dry-run vs real distinction is layered on by the
/// caller from `CliContext::dry_run`. Install never adopts (I3 refuses and
/// points at `anolisa adopt`), so there is no adopt outcome here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InstallOutcome {
    /// An install that executed (or its dry-run preview).
    Installed,
    /// The record already covers the request (idempotent NoOp); nothing was
    /// downloaded or written.
    AlreadyInstalled,
}

/// Caller-side inputs to `resolve_raw`, grouped to keep the signature flat.
pub(crate) struct ResolveInputs<'a> {
    pub(crate) component: String,
    pub(crate) package: String,
    pub(crate) backend: String,
    pub(crate) base_url: String,
    pub(crate) repository_origin: Option<RawRepositoryOrigin>,
    pub(crate) version: Option<&'a str>,
    pub(crate) warnings: Vec<String>,
}
