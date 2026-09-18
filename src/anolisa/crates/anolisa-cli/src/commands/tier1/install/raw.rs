//! Raw backend resolution and manifest contract parsing for the `install`
//! command. Execution moved to the planner-driven pipeline: `dispatch.rs`
//! drives the plan, `owned_ops.rs` performs the side effects.

use std::path::Path;

use anolisa_core::download::{DownloadCache, DownloadError};
use anolisa_core::install_runner::{
    RenderMode, RenderSpec, ResolvedInstallFile, SUPPORTED_ARTIFACT_TYPES,
    read_embedded_component_manifest_text,
};
use anolisa_core::path_safety::validate_owned_path;
use anolisa_core::{
    ArtifactType, CapabilityRequest, ComponentManifest, DistributionIndex, FileKind, HookPhase,
    HookSpec, ResolveError, ResolveQuery, ServiceRequest, expand_layout_placeholders,
    resolve_manifest_hooks,
};
use anolisa_platform::fs_layout::FsLayout;
use sha2::{Digest, Sha256};

use crate::context::CliContext;
use crate::repo_config::{
    HostVars, RepoConfig, raw_artifact_url, raw_index_url, raw_index_v2_url, raw_relative_root,
};
use crate::response::CliError;

use super::COMMAND;
use super::render::{artifact_ext, artifact_type_wire, repo_config_err};
use super::types::*;

pub(super) fn index_fetch_error(
    index_url: &str,
    err: DownloadError,
    repository_origin: Option<&RawRepositoryOrigin>,
) -> CliError {
    match err {
        DownloadError::Io {
            ref path,
            ref source,
        } if source.kind() == std::io::ErrorKind::NotFound
            && index_url
                .strip_prefix("file://")
                .is_some_and(|expected| path.as_path() == Path::new(expected)) =>
        {
            let guidance = match repository_origin {
                Some(RawRepositoryOrigin::Config(config_path)) => format!(
                    "repository config: {}; move it aside and retry to restore the default config, or pass --repo <URL> once",
                    config_path.display()
                ),
                Some(RawRepositoryOrigin::CliOverride) =>
                    "the one-off --repo <URL> override selected this repository; pass a reachable URL or omit the override"
                        .to_string(),
                None => "check the raw repository URL in repo.toml or, if supplied, the one-off --repo <URL> override".to_string(),
            };
            CliError::Runtime {
                command: COMMAND.to_string(),
                reason: format!(
                    "local raw repository index not found at {}; {guidance}",
                    path.display(),
                ),
            }
        }
        err => CliError::Runtime {
            command: COMMAND.to_string(),
            reason: format!("failed to fetch distribution index {index_url}: {err}"),
        },
    }
}

/// Whether a fetch failure means "this file is not published", as opposed to
/// a transport or repository fault. Every optional-file fallback in this
/// module is gated on it: falling back on a transient error would silently
/// serve a *different* file than the one the repository actually publishes.
fn remote_file_absent(err: &DownloadError) -> bool {
    match err {
        DownloadError::HttpStatus { status, .. } => *status == 404 || *status == 410,
        // file:// repositories surface a missing index as I/O NotFound.
        DownloadError::Io { source, .. } => source.kind() == std::io::ErrorKind::NotFound,
        _ => false,
    }
}

/// Fetch the raw index, preferring the complete generation-2 file and
/// falling back to `index.toml` only when the repository has not published
/// one (split-index bootstrap; see `raw_index_v2_url`). Returns the URL the
/// index was actually served from, for error attribution downstream.
fn fetch_raw_index(
    cache: &DownloadCache,
    base_url: &str,
    repository_origin: Option<&RawRepositoryOrigin>,
) -> Result<(String, std::path::PathBuf), CliError> {
    let v2_url = raw_index_v2_url(base_url);
    match cache.fetch(&v2_url, None) {
        Ok(downloaded) => Ok((v2_url, downloaded.cached_path)),
        // Absence only: a transport fault here must not downgrade a gen-2
        // repository to its gen-1 view.
        Err(err) if remote_file_absent(&err) => {
            let v1_url = raw_index_url(base_url);
            let downloaded = cache
                .fetch(&v1_url, None)
                .map_err(|err| index_fetch_error(&v1_url, err, repository_origin))?;
            Ok((v1_url, downloaded.cached_path))
        }
        Err(err) => Err(index_fetch_error(&v2_url, err, repository_origin)),
    }
}

pub(crate) fn resolve_raw(
    ctx: &CliContext,
    layout: &FsLayout,
    env: &anolisa_env::EnvFacts,
    inputs: ResolveInputs<'_>,
) -> Result<RawResolution, CliError> {
    let ResolveInputs {
        component,
        package,
        backend,
        base_url,
        repository_origin,
        version,
        mut warnings,
    } = inputs;

    // The index is always re-fetched (DownloadCache overwrites on conflict),
    // so a republished repo is picked up without a cache flush.
    let cache = DownloadCache::new(layout.cache_dir.clone());
    let (index_url, cached_index_path) =
        fetch_raw_index(&cache, &base_url, repository_origin.as_ref())?;
    // Entry-tolerant parse: a row shaped for a future CLI fails closed for
    // its own component (skipped, surfaced as a warning) instead of taking
    // the whole shared index — and every unrelated component — down with it.
    // The unfiltered index is kept for error attribution: a pinned version
    // that only ships non-installable artifact types must be reported as
    // "published but not installable", not as unpublished.
    let (full_index, skipped_entries) = DistributionIndex::load_lenient(&cached_index_path)
        .map_err(|err| CliError::Runtime {
            command: COMMAND.to_string(),
            reason: format!("failed to parse distribution index {index_url}: {err}"),
        })?;
    let index = installable_raw_index(full_index.clone());

    // The index is keyed by the backend-native package name so that
    // `package_map` / `--package` select between alternate publications.
    let query = ResolveQuery {
        component: &package,
        version,
        channel: None,
        install_mode: ctx.install_mode.as_str(),
        os: &env.os,
        arch: &env.arch,
        libc: env.libc.as_deref(),
        pkg_base: env.pkg_base.as_deref(),
        preferred_types: &[],
    };
    // A skipped row that may answer this query forces a refusal *before*
    // resolution: picking the best of the remaining rows could silently
    // substitute an older parsable version for the one this build cannot
    // read (and `update` could even downgrade). Rows for other components,
    // targets, or non-matching pinned versions stay mere warnings.
    if let Some(blocking) = skipped_entries.iter().find(|s| s.may_match(&query)) {
        return Err(CliError::Runtime {
            command: COMMAND.to_string(),
            reason: format!(
                "distribution index {index_url} contains an entry for '{package}' this CLI cannot parse ({}); refusing to resolve '{package}' against the remaining entries to avoid a silent downgrade — run 'anolisa self-update' and retry",
                blocking.reason
            ),
        });
    }
    warnings.extend(
        skipped_entries
            .into_iter()
            .map(|s| format!("distribution index {index_url}: {}", s.reason)),
    );
    let entry = index.resolve(&query).map_err(|err| {
        // A pinned version the installable index cannot satisfy gets a
        // dedicated refusal, symmetric with the rpm backend's version-pin
        // errors. The unfiltered index decides the wording: a version that
        // exists there but not in the installable view is published — only
        // its artifact types are outside what the raw backend can place.
        // Every other resolution failure keeps the generic query rendering.
        if let (Some(pinned), ResolveError::NotFound) = (version, &err) {
            let unversioned = ResolveQuery {
                version: None,
                ..query.clone()
            };
            let installable = index.matching_versions(&unversioned);
            let installable_note = if installable.is_empty() {
                String::new()
            } else {
                format!(" — installable versions: {}", installable.join(", "))
            };
            let published_any_type = full_index
                .matching_versions(&unversioned)
                .iter()
                .any(|v| v == pinned);
            if published_any_type {
                return CliError::InvalidArgument {
                    command: COMMAND.to_string(),
                    reason: format!(
                        "version '{pinned}' of component '{component}' (package '{package}') is published in the raw repository {index_url} but only with artifact types the raw backend cannot install (supported: {}); nothing was changed{installable_note}",
                        SUPPORTED_ARTIFACT_TYPES.join(", "),
                    ),
                };
            }
            if !installable.is_empty() {
                return CliError::InvalidArgument {
                    command: COMMAND.to_string(),
                    reason: format!(
                        "version '{pinned}' of component '{component}' (package '{package}') is not published in the raw repository {index_url} for {}/{} ({} mode); nothing was changed{installable_note}",
                        env.os,
                        env.arch,
                        ctx.install_mode.as_str(),
                    ),
                };
            }
        }
        // A component the installable index *does* publish, just not for this
        // host, is a platform gap rather than an unknown name — the generic
        // rendering below reports both identically, which sends an operator
        // hunting a nonexistent component instead of a wrong build host. Only
        // reached when no row matches the requested os/arch (`arch = "any"`
        // counts as a match, exactly as `resolve` reads it), so a failure a
        // later filter caused — version, install mode, libc, package base —
        // keeps its existing diagnostic instead of being misreported as a
        // platform gap. The list comes from the installable view, so platforms
        // published only with artifact types this backend cannot place are not
        // offered as alternatives.
        if matches!(err, ResolveError::NotFound) {
            let platforms = index.published_platforms(&query);
            let host_published = platforms
                .iter()
                .any(|(os, arch)| os == query.os && (arch == query.arch || arch == "any"));
            if !platforms.is_empty() && !host_published {
                let available = platforms
                    .iter()
                    .map(|(os, arch)| format!("  - {os}/{arch}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                return CliError::InvalidArgument {
                    command: COMMAND.to_string(),
                    reason: format!(
                        "component '{component}' (package '{package}') is not available for {}/{} from {index_url}; nothing was changed\n\navailable platforms:\n{available}",
                        query.os,
                        query.arch,
                    ),
                };
            }
        }
        CliError::InvalidArgument {
            command: COMMAND.to_string(),
            reason: format!(
                "cannot resolve package '{package}' (component '{component}', version {}, {}/{}, {} mode) from {index_url}: {err}",
                version.unwrap_or("latest"),
                env.os,
                env.arch,
                ctx.install_mode.as_str(),
            ),
        }
    })?;

    let wire_type = artifact_type_wire(&entry.artifact_type);
    if !SUPPORTED_ARTIFACT_TYPES.contains(&wire_type) {
        return Err(CliError::InvalidArgument {
            command: COMMAND.to_string(),
            reason: format!(
                "resolved artifact type '{wire_type}' is not installable by the raw backend (supported: {})",
                SUPPORTED_ARTIFACT_TYPES.join(", ")
            ),
        });
    }
    // Three URL forms, most-mirror-friendly first: an omitted url uses the
    // code-owned raw layout, a repo-relative url resolves against the index
    // directory (self-contained mirrors), and an absolute url is used as-is
    // (escape hatch for off-repo artifacts).
    let artifact_url = if entry.url.is_empty() {
        let values = std::collections::BTreeMap::from([
            ("component", Some(entry.component.clone())),
            ("version", Some(entry.version.clone())),
            ("os", Some(entry.os.clone())),
            ("arch", Some(entry.arch.clone())),
            ("libc", entry.libc.clone()),
            ("ext", Some(artifact_ext(&entry.artifact_type).to_string())),
        ]);
        raw_artifact_url(&backend, &base_url, &values).map_err(|err| CliError::Runtime {
            command: COMMAND.to_string(),
            reason: format!(
                "cannot derive artifact URL for '{package}' {} from raw repository layout: {err}",
                entry.version
            ),
        })?
    } else if entry.url.contains("://") {
        entry.url.clone()
    } else {
        format!(
            "{}/{}",
            raw_relative_root(&base_url),
            entry.url.trim_start_matches('/')
        )
    };

    Ok(RawResolution {
        component,
        package,
        artifact_url,
        entry,
        base_url,
        warnings,
    })
}

/// Rebuild [`ResolveInputs`] for an already-installed component from its
/// recorded backend plus repo.toml, for the `update` path (which has no CLI
/// `--backend` / `--repo` / `--version` to read). Always targets the latest
/// published version (`version: None`).
///
/// `recorded_package` is the package captured at install time
/// ([`InstalledObject::raw_package`](anolisa_core::state::InstalledObject::raw_package));
/// when present it takes precedence over repo.toml derivation, so a component
/// installed with `--package` updates against the same package rather than a
/// re-derived (possibly different) one.
///
/// # Errors
///
/// Returns [`CliError`] when `backend_name` is unknown or unconfigured in
/// repo.toml, when its `base_url` variables cannot be resolved, or — until a
/// non-raw raw-like executor exists — when the backend is not `raw`.
pub(crate) fn resolve_raw_inputs_for_component(
    component: String,
    backend_name: &str,
    recorded_package: Option<&str>,
    env: &anolisa_env::EnvFacts,
    repo_config: &RepoConfig,
    command: &str,
) -> Result<ResolveInputs<'static>, CliError> {
    let (backend_name, backend) = repo_config
        .select_backend(Some(backend_name))
        .map_err(|err| repo_config_err(err, true).with_command(command))?;
    if backend_name != "raw" {
        return Err(CliError::not_implemented_with_hint(
            command.to_string(),
            format!(
                "the '{backend_name}' backend has no update executor yet — only 'raw' updates today"
            ),
        ));
    }
    let host = HostVars {
        os: env.os.clone(),
        arch: env.arch.clone(),
    };
    let base_url = repo_config
        .resolved_base_url(backend_name, backend, &host)
        .map_err(|err| repo_config_err(err, true).with_command(command))?;
    // recorded_package wins via package_name's CLI-override slot, so a
    // `--package` install resolves the same package on update; None falls
    // through to repo.toml's package_map / component-name derivation.
    let package = repo_config.package_name(backend, &component, recorded_package);
    Ok(ResolveInputs {
        component,
        package,
        backend: backend_name.to_string(),
        base_url,
        repository_origin: repo_config
            .source_path()
            .map(|path| RawRepositoryOrigin::Config(path.to_path_buf())),
        version: None,
        warnings: Vec::new(),
    })
}

fn installable_raw_index(mut index: DistributionIndex) -> DistributionIndex {
    index.entries.retain(|entry| {
        SUPPORTED_ARTIFACT_TYPES.contains(&artifact_type_wire(&entry.artifact_type))
    });
    index
}

impl InstallContractSource {
    fn label(self) -> &'static str {
        match self {
            Self::EmbeddedArtifact => "embedded artifact manifest",
            Self::SidecarMeta => "sidecar meta.toml",
        }
    }
}

/// Load the published lightweight install contract without fetching the full
/// artifact, so dry-run can enforce manifest-backed refusals such as component
/// conflicts.
///
/// The contract must describe the artifact that was *resolved*, not merely the
/// component and version: a repository may publish a different contract per
/// target (a Linux system-only payload beside a macOS build that also supports
/// user mode). [`meta_url_candidates`] therefore prefers the metadata beside
/// the resolved artifact, and the version-level file is consulted only when
/// that sibling is absent.
pub(crate) fn load_dry_run_install_contract(
    ctx: &CliContext,
    layout: &FsLayout,
    resolution: &RawResolution,
) -> Result<Option<LoadedInstallContract>, CliError> {
    let expected_sha = manifest_digest_sha256(resolution.entry.manifest_digest.as_deref())?;
    let cache = DownloadCache::new(layout.cache_dir.clone());
    // Candidates are keyed by full URL in the download cache, so a sibling and
    // a version-level `meta.toml` never share a cache entry.
    for meta_url in meta_url_candidates(
        &resolution.artifact_url,
        &resolution.entry.component,
        &resolution.entry.version,
    ) {
        let downloaded = match cache.fetch(&meta_url, expected_sha) {
            Ok(downloaded) => downloaded,
            // Absence only. A network, digest, or parse failure on metadata
            // the repository *does* publish must fail the dry-run: falling
            // through would validate another target's contract and report a
            // preview the execution path would never honour.
            Err(err) if remote_file_absent(&err) => continue,
            Err(err) => {
                return Err(CliError::Runtime {
                    command: COMMAND.to_string(),
                    reason: format!("failed to fetch sidecar metadata {meta_url}: {err}"),
                });
            }
        };
        let toml =
            std::fs::read_to_string(&downloaded.cached_path).map_err(|err| CliError::Runtime {
                command: COMMAND.to_string(),
                reason: format!(
                    "failed to read sidecar metadata {} from cache: {err}",
                    downloaded.cached_path.display()
                ),
            })?;
        let manifest =
            ComponentManifest::from_toml_str(&toml).map_err(|err| CliError::Runtime {
                command: COMMAND.to_string(),
                reason: format!("failed to parse sidecar metadata {meta_url}: {err}"),
            })?;
        validate_manifest_contract_header(
            &manifest,
            resolution,
            ctx.install_mode.as_str(),
            InstallContractSource::SidecarMeta,
        )?;
        return Ok(Some(LoadedInstallContract {
            manifest,
            source: InstallContractSource::SidecarMeta,
            toml,
        }));
    }
    Ok(None)
}

/// `meta.toml` URLs to try for a resolved artifact, in preference order.
///
/// The metadata published beside the artifact wins. Replacing the final
/// artifact URL segment follows the same-directory publication convention,
/// and it is the only form that can describe a
/// single target: `…/0.10.1/macos/aarch64/meta.toml` documents the macOS
/// payload, while `…/0.10.1/meta.toml` documents whatever target the
/// publisher happened to make version-wide.
///
/// The version root stays as a fallback so legacy repositories — which
/// publish one contract for every target — keep working unchanged. It is
/// omitted when the artifact already sits in the version root, where both
/// forms derive the same URL and a second fetch would be pure waste.
fn meta_url_candidates(artifact_url: &str, component: &str, version: &str) -> Vec<String> {
    let mut candidates = Vec::with_capacity(2);
    if let Some(index) = artifact_url.rfind('/') {
        candidates.push(format!("{}/meta.toml", &artifact_url[..index]));
    }
    let version_marker = format!("/{component}/{version}/");
    if let Some(index) = artifact_url.rfind(&version_marker) {
        let version_root = format!("{}meta.toml", &artifact_url[..index + version_marker.len()]);
        if !candidates.contains(&version_root) {
            candidates.push(version_root);
        }
    }
    candidates
}

fn manifest_digest_sha256(digest: Option<&str>) -> Result<Option<&str>, CliError> {
    match digest {
        None => Ok(None),
        Some(value) => value
            .strip_prefix("sha256:")
            .map(Some)
            .ok_or_else(|| CliError::Runtime {
                command: COMMAND.to_string(),
                reason: format!("unsupported manifest_digest '{value}' in the distribution index"),
            }),
    }
}

fn validate_manifest_digest(toml: &str, resolution: &RawResolution) -> Result<(), CliError> {
    let Some(expected) = manifest_digest_sha256(resolution.entry.manifest_digest.as_deref())?
    else {
        return Ok(());
    };
    let actual = format!("{:x}", Sha256::digest(toml.as_bytes()));
    if expected.eq_ignore_ascii_case(&actual) {
        return Ok(());
    }
    Err(CliError::Runtime {
        command: COMMAND.to_string(),
        reason: format!(
            "embedded component manifest digest for '{}' {} does not match the distribution index: expected {expected}, got {actual}",
            resolution.component, resolution.entry.version
        ),
    })
}

pub(crate) fn prepare_raw_execution(
    ctx: &CliContext,
    layout: &FsLayout,
    resolution: RawResolution,
) -> Result<PreparedInstall, CliError> {
    let sha256 = resolution.entry.sha256.as_deref().ok_or_else(|| {
        CliError::Runtime {
            command: COMMAND.to_string(),
            reason: format!(
                "distribution entry for '{}' {} has no sha256 — refusing to install an unverifiable artifact",
                resolution.package, resolution.entry.version
            ),
        }
    })?;

    let cache = DownloadCache::new(layout.cache_dir.clone());
    let artifact = cache
        .fetch(&resolution.artifact_url, Some(sha256))
        .map_err(|err| CliError::Runtime {
            command: COMMAND.to_string(),
            reason: format!(
                "failed to download artifact {}: {err}",
                resolution.artifact_url
            ),
        })?;

    let contract = load_execution_install_contract(&resolution, &artifact.cached_path)?;
    let (files, services, capabilities) = resolve_manifest_contract(
        &contract.manifest,
        layout,
        &resolution,
        ctx.install_mode.as_str(),
        contract.source,
    )?;

    Ok(PreparedInstall {
        resolution,
        artifact_path: artifact.cached_path,
        files,
        services,
        capabilities,
        manifest_toml: contract.toml,
    })
}

fn load_execution_install_contract(
    resolution: &RawResolution,
    artifact_path: &Path,
) -> Result<LoadedInstallContract, CliError> {
    match resolution.entry.artifact_type {
        ArtifactType::TarGz => {
            let toml = read_embedded_component_manifest_text(artifact_path)
                .map_err(|err| CliError::Runtime {
                    command: COMMAND.to_string(),
                    reason: format!(
                        "failed to read embedded component manifest from {}: {err}",
                        resolution.artifact_url
                    ),
                })?
                .ok_or_else(|| CliError::Runtime {
                    command: COMMAND.to_string(),
                    reason: format!(
                        "published artifact for package '{}' has no embedded .anolisa/component.toml",
                        resolution.package
                    ),
                })?;
            let manifest =
                ComponentManifest::from_toml_str(&toml).map_err(|err| CliError::Runtime {
                    command: COMMAND.to_string(),
                    reason: format!(
                        "failed to parse embedded component manifest from {}: {err}",
                        resolution.artifact_url
                    ),
                })?;
            validate_manifest_digest(&toml, resolution)?;
            Ok(LoadedInstallContract {
                manifest,
                source: InstallContractSource::EmbeddedArtifact,
                toml,
            })
        }
        other => Err(CliError::InvalidArgument {
            command: COMMAND.to_string(),
            reason: format!(
                "resolved artifact type '{}' is not installable by the raw backend (supported: {})",
                artifact_type_wire(&other),
                SUPPORTED_ARTIFACT_TYPES.join(", ")
            ),
        }),
    }
}

/// Resolved install contract: laid files, recorded service unit names, and
/// capability requests to apply once those files are on disk.
type ResolvedContract = (
    Vec<ResolvedInstallFile>,
    Vec<ServiceRequest>,
    Vec<CapabilityRequest>,
);

fn resolve_manifest_contract(
    manifest: &ComponentManifest,
    layout: &FsLayout,
    resolution: &RawResolution,
    mode: &str,
    source: InstallContractSource,
) -> Result<ResolvedContract, CliError> {
    validate_manifest_contract_header(manifest, resolution, mode, source)?;

    let mut files = resolve_manifest_files(manifest, layout, &resolution.component)?;
    if files.is_empty() {
        return Err(CliError::Runtime {
            command: COMMAND.to_string(),
            reason: format!(
                "component '{}' declares no [install.files] — nothing to install",
                resolution.component
            ),
        });
    }
    // Adapter resources are laid alongside the component's own files, from
    // the same artifact. Install only *places* them under the standard
    // `{datadir}/adapters/<component>/<framework>/` tree — enabling them
    // against a framework is the separate `anolisa adapter enable` step.
    files.extend(resolve_adapter_files(
        manifest,
        layout,
        &resolution.component,
    )?);

    let services = resolve_manifest_services(manifest, &resolution.component, mode)?;
    let capabilities = resolve_manifest_capabilities(manifest, layout, &resolution.component)?;

    Ok((files, services, capabilities))
}

fn validate_manifest_contract_header(
    manifest: &ComponentManifest,
    resolution: &RawResolution,
    mode: &str,
    source: InstallContractSource,
) -> Result<(), CliError> {
    if manifest.component.name.as_str() != resolution.component {
        return Err(CliError::Runtime {
            command: COMMAND.to_string(),
            reason: format!(
                "{} for package '{}' declares component '{}', expected '{}'",
                source.label(),
                resolution.package,
                manifest.component.name,
                resolution.component
            ),
        });
    }
    if manifest.component.version.as_str() != resolution.entry.version.as_str() {
        return Err(CliError::Runtime {
            command: COMMAND.to_string(),
            reason: format!(
                "{} for component '{}' declares version {}, but the distribution index resolved {}",
                source.label(),
                resolution.component,
                manifest.component.version,
                resolution.entry.version
            ),
        });
    }

    if !manifest.install.modes.iter().any(|m| m == mode) {
        return Err(CliError::InvalidArgument {
            command: COMMAND.to_string(),
            reason: format!(
                "{} for component '{}' is inconsistent with the distribution index: index resolved {mode}-mode support, but manifest declares modes: {}",
                source.label(),
                resolution.component,
                manifest.install.modes.join(", ")
            ),
        });
    }

    validate_min_anolisa_version(manifest, &resolution.component, source)?;
    Ok(())
}

/// Enforce the `[component.contract].min_anolisa_version` gate: refuse the
/// contract when this CLI is older than the version it requires.
///
/// A contract may depend on install behavior (content rendering,
/// backend-aware adapter roots, …) that an older CLI silently lacks —
/// tolerant parsing would drop the unknown fields and install a broken
/// result. Fail-closed: a `min_anolisa_version` that is not valid SemVer is
/// rejected too, with no `--force` escape hatch.
fn validate_min_anolisa_version(
    manifest: &ComponentManifest,
    component: &str,
    source: InstallContractSource,
) -> Result<(), CliError> {
    validate_min_anolisa_version_against(manifest, component, source, env!("CARGO_PKG_VERSION"))
}

/// [`validate_min_anolisa_version`] with the CLI version injected, so tests
/// can pin release-boundary scenarios (e.g. a released binary that predates
/// a capability meeting the first contract that requires it) without
/// depending on the workspace version at build time.
fn validate_min_anolisa_version_against(
    manifest: &ComponentManifest,
    component: &str,
    source: InstallContractSource,
    current: &str,
) -> Result<(), CliError> {
    let Some(required) = manifest.contract.min_anolisa_version.as_deref() else {
        return Ok(());
    };
    let required_version =
        semver::Version::parse(required).map_err(|err| CliError::InvalidArgument {
            command: COMMAND.to_string(),
            reason: format!(
                "{} for component '{component}' declares min_anolisa_version '{required}', which is not a valid SemVer version: {err}",
                source.label(),
            ),
        })?;
    let current_version = semver::Version::parse(current).map_err(|err| CliError::Runtime {
        command: COMMAND.to_string(),
        reason: format!("cannot parse this CLI's own version '{current}': {err}"),
    })?;
    if current_version < required_version {
        return Err(CliError::InvalidArgument {
            command: COMMAND.to_string(),
            reason: format!(
                "component '{component}' requires anolisa >= {required}, but this CLI is {current}; run 'anolisa self-update' and retry",
            ),
        });
    }
    Ok(())
}

/// Render the manifest's `[[component.services]]` into activation requests:
/// substitute the template instance into the unit name and carry
/// scope/enable/start through to the executor. No filesystem or layout
/// expansion — unit names are systemd identifiers, not paths.
///
/// # Errors
///
/// Returns [`CliError::Runtime`] if a service entry has an empty `unit`.
pub(crate) fn resolve_manifest_services(
    manifest: &ComponentManifest,
    component: &str,
    mode: &str,
) -> Result<Vec<ServiceRequest>, CliError> {
    // The `%u` instance specifier resolves to the caller's login name, but
    // only in a user-mode install where the unit is activated as that user.
    // A system-mode install merely *places* a user-scope template for later
    // per-user `systemctl --user enable`, so it leaves `%u` un-resolved
    // (the bare template) rather than baking in root's name. Detect the user
    // at most once, and only when a `%u` instance actually needs it.
    let caller = if mode == "user"
        && manifest
            .install
            .services
            .iter()
            .any(|s| s.instance.as_deref().is_some_and(|i| i.contains("%u")))
    {
        Some(anolisa_env::EnvService::detect().user)
    } else {
        None
    };

    let mut requests = Vec::with_capacity(manifest.install.services.len());
    for spec in &manifest.install.services {
        if spec.unit.trim().is_empty() {
            return Err(CliError::Runtime {
                command: COMMAND.to_string(),
                reason: format!(
                    "component '{component}' has a [[component.services]] entry with an empty unit"
                ),
            });
        }
        // Template unit (`name@.service`) + instance → `name@<instance>.service`.
        let unit = match &spec.instance {
            Some(instance) if spec.unit.contains("@.") => {
                match resolve_service_instance(instance, caller.as_deref()) {
                    Some(resolved) => spec.unit.replacen("@.", &format!("@{resolved}."), 1),
                    // `%u` with no resolved user (system-mode place-only):
                    // keep the bare template; per-user enable instantiates it.
                    None => spec.unit.clone(),
                }
            }
            _ => spec.unit.clone(),
        };
        requests.push(ServiceRequest {
            unit,
            scope: spec.scope,
            enable: spec.enable,
            start: spec.start,
        });
    }
    Ok(requests)
}

/// Resolve a systemd template instance, expanding the `%u` specifier to the
/// caller's login name.
///
/// `%u` is a systemd specifier that systemd does *not* expand in the instance
/// portion of a command-line unit name, so anolisa resolves it itself. Returns
/// `None` when the instance uses `%u` but no caller name is available (a
/// system-mode install that only places the template) — the caller then keeps
/// the bare template. A literal instance is returned verbatim in every mode.
pub(crate) fn resolve_service_instance(instance: &str, caller: Option<&str>) -> Option<String> {
    if !instance.contains("%u") {
        return Some(instance.to_string());
    }
    caller.map(|user| instance.replace("%u", user))
}

/// Render the manifest's `[install.files]` against the layout: expand
/// `{bindir}`-style placeholders and reject any destination escaping the
/// ANOLISA-owned roots before a single byte is written.
fn resolve_manifest_files(
    manifest: &ComponentManifest,
    layout: &FsLayout,
    component: &str,
) -> Result<Vec<ResolvedInstallFile>, CliError> {
    let mut files = Vec::with_capacity(manifest.install.files.len());
    for spec in &manifest.install.files {
        let template = spec.install_path().ok_or_else(|| CliError::Runtime {
            command: COMMAND.to_string(),
            reason: format!(
                "component '{component}' has an [install.files] entry with neither source nor dest"
            ),
        })?;
        let dest = expand_layout_placeholders(template, layout, &[("component", component)])
            .map_err(|err| CliError::Runtime {
                command: COMMAND.to_string(),
                reason: format!("failed to expand install path '{template}': {err}"),
            })?;
        validate_owned_path(layout, &dest).map_err(|err| CliError::Runtime {
            command: COMMAND.to_string(),
            reason: format!(
                "install destination '{}' failed path safety check: {err}",
                dest.display()
            ),
        })?;
        // A symlink's source is its referent — a layout template like the
        // dest, not an archive path. Expand and bound-check it the same way.
        let source = match (spec.kind, spec.source.as_deref()) {
            (FileKind::Symlink, Some(template)) => {
                let referent =
                    expand_layout_placeholders(template, layout, &[("component", component)])
                        .map_err(|err| CliError::Runtime {
                            command: COMMAND.to_string(),
                            reason: format!(
                                "failed to expand symlink referent '{template}': {err}"
                            ),
                        })?;
                validate_owned_path(layout, &referent).map_err(|err| CliError::Runtime {
                    command: COMMAND.to_string(),
                    reason: format!(
                        "symlink referent '{}' failed path safety check: {err}",
                        referent.display()
                    ),
                })?;
                Some(referent.to_string_lossy().into_owned())
            }
            _ => spec.source.clone(),
        };
        let render = resolve_render_spec(spec, component)?;
        files.push(ResolvedInstallFile {
            source,
            dest,
            mode: spec.mode.clone(),
            kind: spec.kind,
            render,
        });
    }
    Ok(files)
}

/// Map a layout entry's `render` string onto a runner [`RenderSpec`].
///
/// Fail-closed on values this CLI does not implement: tolerant manifest
/// parsing keeps read-only commands working, but installing without the
/// requested rendering would place literal `{bindir}`-style placeholders
/// (e.g. an unstartable systemd unit). Rendering targets a single regular
/// file — a directory source or symlink entry with `render` is a contract
/// defect and is rejected here with the same rationale.
///
/// This is the early, user-facing gate: it fires at contract-resolution
/// time with `InvalidArgument` and full layout-entry context. The runner
/// re-checks the same invariant defensively for callers that build
/// [`ResolvedInstallFile`] directly (see `InstallRunner` symlink/directory
/// validation); keep the two in sync when the rule changes.
fn resolve_render_spec(
    spec: &anolisa_core::manifest::InstallFileSpec,
    component: &str,
) -> Result<Option<RenderSpec>, CliError> {
    let Some(raw) = spec.render.as_deref() else {
        return Ok(None);
    };
    // Present-but-blank is a contract defect, not an undeclared render:
    // treating it as `None` would copy the template verbatim and land
    // literal placeholders — the exact failure `render` exists to prevent.
    let render = raw.trim();
    if render.is_empty() {
        return Err(CliError::InvalidArgument {
            command: COMMAND.to_string(),
            reason: format!(
                "component '{component}' layout entry '{}' declares an empty render value; declare a supported mode (e.g. '{}') or remove the key",
                spec.display(),
                anolisa_core::manifest::RENDER_ANOLISA_PATHS_V1,
            ),
        });
    }
    let mode = RenderMode::parse(render).ok_or_else(|| CliError::InvalidArgument {
        command: COMMAND.to_string(),
        reason: format!(
            "component '{component}' layout entry '{}' requests render '{render}', which this CLI does not support (supported: '{}'); run 'anolisa self-update' and retry",
            spec.display(),
            anolisa_core::manifest::RENDER_ANOLISA_PATHS_V1,
        ),
    })?;
    if spec.kind == FileKind::Symlink || spec.source.as_deref().is_some_and(|s| s.ends_with('/')) {
        return Err(CliError::InvalidArgument {
            command: COMMAND.to_string(),
            reason: format!(
                "component '{component}' layout entry '{}' requests render '{render}', but render applies to single regular files — not directory sources or symlinks",
                spec.display(),
            ),
        });
    }
    Ok(Some(RenderSpec {
        mode,
        component: component.to_string(),
    }))
}

/// Render the manifest's `[[adapters]]` entries into install file mappings.
///
/// Install only *places* adapter resources under the standard
/// `{datadir}/adapters/<component>/<framework>/` tree; it never runs a
/// framework CLI or touches user framework state — that is
/// `anolisa adapter enable`.
///
/// Each entry is linted up front for the fields install needs: a framework,
/// a source, and a destination. The framework does not have to be supported
/// by this ANOLISA build; install only lays data down, while
/// `anolisa adapter enable` decides whether a built-in driver exists.
pub(crate) fn resolve_adapter_files(
    manifest: &ComponentManifest,
    layout: &FsLayout,
    component: &str,
) -> Result<Vec<ResolvedInstallFile>, CliError> {
    if manifest.adapters.is_empty() {
        return Ok(Vec::new());
    }
    let mut files = Vec::with_capacity(manifest.adapters.len());
    for adapter in &manifest.adapters {
        let framework = adapter
            .framework
            .as_deref()
            .ok_or_else(|| CliError::InvalidArgument {
                command: COMMAND.to_string(),
                reason: format!(
                    "component '{component}' has an [[adapters]] entry with no framework"
                ),
            })?;
        let source = adapter
            .source
            .as_deref()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| CliError::InvalidArgument {
                command: COMMAND.to_string(),
                reason: format!(
                    "component '{component}' adapter for '{framework}' declares no source"
                ),
            })?;
        let dest_template = adapter
            .dest
            .as_deref()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| CliError::InvalidArgument {
                command: COMMAND.to_string(),
                reason: format!(
                    "component '{component}' adapter for '{framework}' declares no dest"
                ),
            })?;
        let dest = expand_layout_placeholders(dest_template, layout, &[("component", component)])
            .map_err(|err| CliError::Runtime {
            command: COMMAND.to_string(),
            reason: format!("failed to expand adapter dest '{dest_template}': {err}"),
        })?;
        validate_owned_path(layout, &dest).map_err(|err| CliError::Runtime {
            command: COMMAND.to_string(),
            reason: format!(
                "adapter destination '{}' failed path safety check: {err}",
                dest.display()
            ),
        })?;
        // The runner lays an entire archive subtree only when the source key
        // ends with '/'. An adapter bundle is always a directory, so force
        // directory-prefix semantics regardless of how the manifest wrote it.
        let source = if source.ends_with('/') {
            source.to_string()
        } else {
            format!("{source}/")
        };
        files.push(ResolvedInstallFile {
            source: Some(source),
            dest,
            // Preserve per-entry archive modes so framework-executed adapter
            // hooks and scripts keep their executable bit after raw installs.
            mode: None,
            kind: FileKind::Data,
            render: None,
        });
    }
    Ok(files)
}

/// Render the manifest's `[[component.capabilities]]` against the layout:
/// expand `{bindir}`-style placeholders in the target path and reject any
/// path escaping the ANOLISA-owned roots before `setcap` ever runs.
///
/// Rows with empty `caps` are skipped — there is nothing to grant. A row
/// that lists caps but no `path` is a contract error: we will not guess
/// which binary to harden.
pub(crate) fn resolve_manifest_capabilities(
    manifest: &ComponentManifest,
    layout: &FsLayout,
    component: &str,
) -> Result<Vec<CapabilityRequest>, CliError> {
    let mut requests = Vec::new();
    for spec in &manifest.install.capabilities {
        if spec.caps.is_empty() {
            continue;
        }
        let template = spec.path.as_deref().ok_or_else(|| CliError::Runtime {
            command: COMMAND.to_string(),
            reason: format!(
                "component '{component}' has a [[component.capabilities]] entry with caps but no path"
            ),
        })?;
        let path = expand_layout_placeholders(template, layout, &[("component", component)])
            .map_err(|err| CliError::Runtime {
                command: COMMAND.to_string(),
                reason: format!("failed to expand capability path '{template}': {err}"),
            })?;
        validate_owned_path(layout, &path).map_err(|err| CliError::Runtime {
            command: COMMAND.to_string(),
            reason: format!(
                "capability target '{}' failed path safety check: {err}",
                path.display()
            ),
        })?;
        requests.push(CapabilityRequest {
            path,
            caps: spec.caps.clone(),
            optional: spec.optional,
        });
    }
    Ok(requests)
}

/// Contract-declared lifecycle hooks for the three raw-install phases,
/// placeholder-expanded with `strict`/`timeout` carried from the contract.
///
/// `pre_install` runs before any files are laid down. On a fresh raw install
/// the hook script ships in the same artifact and is therefore not on disk
/// yet, so [`run_hook`](anolisa_core::run_hook) reports it as `Missing`. With
/// `strict = false` — the only sensible choice for `pre_install`, since the
/// script cannot exist on a first install — that is a silent no-op; a
/// `strict = true` `pre_install` would instead abort the install (the script
/// it requires is unreachable). The phase becomes meaningful on the update
/// path (out of scope here) where a prior version already laid the script.
#[derive(Debug)]
pub(crate) struct InstallHooks {
    pub(crate) pre_install: Vec<HookSpec>,
    pub(crate) post_install: Vec<HookSpec>,
    pub(crate) post_enable: Vec<HookSpec>,
}

/// Resolve a component's `[[component.hooks]]` for the three install phases.
///
/// Unlike the uninstall side (which degrades a missing/invalid snapshot to
/// "no hooks"), install resolves strictly: an unresolvable script path is a
/// contract authoring bug and aborts before any IO so it surfaces early.
pub(crate) fn resolve_install_hooks(
    manifest: &ComponentManifest,
    layout: &FsLayout,
    component: &str,
) -> Result<InstallHooks, CliError> {
    let resolve = |phase: HookPhase| -> Result<Vec<HookSpec>, CliError> {
        resolve_manifest_hooks(&manifest.install.hooks, layout, component, phase).map_err(|err| {
            CliError::Runtime {
                command: COMMAND.to_string(),
                reason: format!(
                    "component '{component}' has an invalid [[component.hooks]] script path: {err}"
                ),
            }
        })
    };
    Ok(InstallHooks {
        pre_install: resolve(HookPhase::PreInstall)?,
        post_install: resolve(HookPhase::PostInstall)?,
        post_enable: resolve(HookPhase::PostEnable)?,
    })
}

#[cfg(test)]
mod tests {
    use super::super::tests::*;
    use super::*;
    use anolisa_core::ComponentManifest;
    use anolisa_platform::fs_layout::FsLayout;
    use tempfile::tempdir;

    /// Split-index bootstrap: a repository that publishes the complete
    /// generation-2 index must be served from it, so gated entries are
    /// visible to this CLI while pre-gate CLIs keep reading `index.toml`.
    #[test]
    fn fetch_raw_index_prefers_v2_when_published() {
        let repo = tempdir().unwrap();
        let root = repo.path().join("v1");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("index.toml"), "schema_version = 1\n").unwrap();
        std::fs::write(root.join("index-v2.toml"), "schema_version = 2\n").unwrap();
        let cache = tempdir().unwrap();
        let dl = DownloadCache::new(cache.path().to_path_buf());
        let (url, path) =
            fetch_raw_index(&dl, &format!("file://{}", root.display()), None).unwrap();
        assert!(url.ends_with("/index-v2.toml"), "got {url}");
        let content = std::fs::read_to_string(path).unwrap();
        assert!(content.contains("schema_version = 2"));
    }

    /// A gen-1 repository (no `index-v2.toml`) keeps working unchanged.
    #[test]
    fn fetch_raw_index_falls_back_to_v1_when_v2_missing() {
        let repo = tempdir().unwrap();
        let root = repo.path().join("v1");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("index.toml"), "schema_version = 1\n").unwrap();
        let cache = tempdir().unwrap();
        let dl = DownloadCache::new(cache.path().to_path_buf());
        let (url, path) =
            fetch_raw_index(&dl, &format!("file://{}", root.display()), None).unwrap();
        assert!(url.ends_with("/index.toml"), "got {url}");
        let content = std::fs::read_to_string(path).unwrap();
        assert!(content.contains("schema_version = 1"));
    }

    /// Fail-closed: a warm download cache never stands in for a repository
    /// that has become unreachable, so a withdrawn or revoked index entry
    /// cannot be resurrected from disk.
    #[test]
    fn fetch_raw_index_does_not_serve_a_cached_index_when_the_repo_is_gone() {
        let repo = tempdir().unwrap();
        let root = repo.path().join("v1");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("index.toml"), "schema_version = 1\n").unwrap();
        let cache = tempdir().unwrap();
        let dl = DownloadCache::new(cache.path().to_path_buf());
        let base_url = format!("file://{}", root.display());
        fetch_raw_index(&dl, &base_url, None).expect("prime the cache");

        std::fs::remove_file(root.join("index.toml")).unwrap();
        let err = fetch_raw_index(&dl, &base_url, None).unwrap_err();
        let CliError::Runtime { reason, .. } = err else {
            panic!("expected runtime error");
        };
        assert!(
            reason.contains("local raw repository index not found"),
            "got: {reason}"
        );
    }

    /// With neither file published the error must attribute the miss to
    /// `index.toml` (the canonical location), not to the optional v2 file.
    #[test]
    fn fetch_raw_index_reports_missing_repo_against_v1() {
        let repo = tempdir().unwrap();
        let root = repo.path().join("v1");
        std::fs::create_dir_all(&root).unwrap();
        let cache = tempdir().unwrap();
        let dl = DownloadCache::new(cache.path().to_path_buf());
        let err = fetch_raw_index(&dl, &format!("file://{}", root.display()), None).unwrap_err();
        let CliError::Runtime { reason, .. } = err else {
            panic!("expected runtime error");
        };
        assert!(
            reason.contains("local raw repository index not found"),
            "got: {reason}"
        );
        assert!(reason.contains("index.toml"), "got: {reason}");
        assert!(!reason.contains("index-v2.toml"), "got: {reason}");
    }

    /// Fallback is reserved for "not published"; transport faults must not
    /// silently downgrade a gen-2 repository to its v1 view, nor let a
    /// resolved artifact's sidecar be replaced by version-level metadata
    /// describing a different target.
    #[test]
    fn remote_file_absent_distinguishes_absence_from_faults() {
        let http = |status| DownloadError::HttpStatus {
            url: "https://example.invalid/index-v2.toml".to_string(),
            status,
        };
        assert!(remote_file_absent(&http(404)));
        assert!(remote_file_absent(&http(410)));
        assert!(!remote_file_absent(&http(500)));
        assert!(!remote_file_absent(&http(403)));
        assert!(remote_file_absent(&DownloadError::Io {
            path: std::path::PathBuf::from("/repo/v1/index-v2.toml"),
            source: std::io::Error::from(std::io::ErrorKind::NotFound),
        }));
        assert!(!remote_file_absent(&DownloadError::Network {
            url: "https://example.invalid/index-v2.toml".to_string(),
            reason: "timed out".to_string(),
        }));
    }

    /// Target-specific metadata is the point of the sibling-first order: the
    /// macOS contract sits beside the macOS artifact, while the version root
    /// holds whichever target the publisher made version-wide.
    #[test]
    fn meta_url_candidates_prefer_the_artifact_sibling() {
        let candidates = meta_url_candidates(
            "file:///repo/v1/agentsight/0.10.1/macos/aarch64/agentsight-0.10.1-macos-aarch64.tar.gz",
            "agentsight",
            "0.10.1",
        );
        assert_eq!(
            candidates,
            vec![
                "file:///repo/v1/agentsight/0.10.1/macos/aarch64/meta.toml".to_string(),
                "file:///repo/v1/agentsight/0.10.1/meta.toml".to_string(),
            ]
        );
    }

    /// An artifact published directly in the version root derives the same
    /// URL both ways; the duplicate must not cost a second fetch.
    #[test]
    fn meta_url_candidates_dedupe_the_version_root() {
        let candidates = meta_url_candidates(
            "file:///repo/v1/agentsight/0.10.1/agentsight-0.10.1.tar.gz",
            "agentsight",
            "0.10.1",
        );
        assert_eq!(
            candidates,
            vec!["file:///repo/v1/agentsight/0.10.1/meta.toml".to_string()]
        );
    }

    /// A flat repository (no `<component>/<version>/` path segment, e.g. an
    /// off-repo `url = "https://…"` escape hatch) still resolves its sibling.
    #[test]
    fn meta_url_candidates_handle_a_flat_layout() {
        let candidates = meta_url_candidates(
            "https://mirror.invalid/downloads/agentsight.tar.gz",
            "agentsight",
            "0.10.1",
        );
        assert_eq!(
            candidates,
            vec!["https://mirror.invalid/downloads/meta.toml".to_string()]
        );
    }

    #[test]
    fn resolve_adapter_files_lays_bundle_under_datadir() {
        let prefix = tempdir().unwrap();
        let layout = FsLayout::system(Some(prefix.path().to_path_buf()));
        let toml = adapter_manifest(
            "openclaw",
            Some("adapters/tokenless/openclaw"),
            Some("{datadir}/adapters/{component}/openclaw/"),
        );
        let manifest = ComponentManifest::from_toml_str(&toml).expect("parse manifest");
        let files = resolve_adapter_files(&manifest, &layout, "tokenless").expect("resolve");

        assert_eq!(files.len(), 1);
        let f = &files[0];
        // Source is normalized to a directory prefix so the whole bundle
        // tree is laid down by the runner.
        assert_eq!(f.source.as_deref(), Some("adapters/tokenless/openclaw/"));
        assert_eq!(f.dest, layout.datadir.join("adapters/tokenless/openclaw"));
        assert_eq!(f.kind, FileKind::Data);
        assert_eq!(f.mode, None);
    }

    #[test]
    fn resolve_adapter_files_allows_unknown_framework() {
        let prefix = tempdir().unwrap();
        let layout = FsLayout::system(Some(prefix.path().to_path_buf()));
        let toml = adapter_manifest(
            "hermes",
            Some("adapters/tokenless/hermes"),
            Some("{datadir}/adapters/{component}/hermes/"),
        );
        let manifest = ComponentManifest::from_toml_str(&toml).expect("parse manifest");
        let files = resolve_adapter_files(&manifest, &layout, "tokenless").expect("resolve");

        assert_eq!(files.len(), 1);
        assert_eq!(
            files[0].source.as_deref(),
            Some("adapters/tokenless/hermes/")
        );
        assert_eq!(
            files[0].dest,
            layout.datadir.join("adapters/tokenless/hermes")
        );
    }

    #[test]
    fn resolve_adapter_files_rejects_missing_source() {
        let prefix = tempdir().unwrap();
        let layout = FsLayout::system(Some(prefix.path().to_path_buf()));
        let toml = adapter_manifest(
            "openclaw",
            None,
            Some("{datadir}/adapters/{component}/openclaw/"),
        );
        let manifest = ComponentManifest::from_toml_str(&toml).expect("parse manifest");
        let err = resolve_adapter_files(&manifest, &layout, "tokenless")
            .expect_err("missing source must be rejected");
        assert!(
            matches!(err, CliError::InvalidArgument { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn resolve_adapter_files_empty_when_no_adapters() {
        let prefix = tempdir().unwrap();
        let layout = FsLayout::system(Some(prefix.path().to_path_buf()));
        let toml = component_manifest_toml("tokenless", "0.1.0", &["system"]);
        let manifest = ComponentManifest::from_toml_str(&toml).expect("parse manifest");
        let files = resolve_adapter_files(&manifest, &layout, "tokenless").expect("resolve");
        assert!(files.is_empty());
    }

    #[test]
    fn resolve_manifest_capabilities_expands_bindir_path() {
        let prefix = tempdir().unwrap();
        let layout = FsLayout::system(Some(prefix.path().to_path_buf()));
        let toml = capability_manifest(Some("{bindir}/agentsight"), &["CAP_BPF"], false);
        let manifest = ComponentManifest::from_toml_str(&toml).expect("parse manifest");
        let reqs =
            resolve_manifest_capabilities(&manifest, &layout, "agentsight").expect("resolve");
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].path, layout.bin_dir.join("agentsight"));
        assert_eq!(reqs[0].caps, vec!["CAP_BPF".to_string()]);
        assert!(!reqs[0].optional);
    }

    #[test]
    fn resolve_manifest_capabilities_rejects_out_of_bounds_path() {
        let prefix = tempdir().unwrap();
        let layout = FsLayout::system(Some(prefix.path().to_path_buf()));
        let toml = capability_manifest(Some("/etc/passwd"), &["CAP_BPF"], false);
        let manifest = ComponentManifest::from_toml_str(&toml).expect("parse manifest");
        let err = resolve_manifest_capabilities(&manifest, &layout, "agentsight")
            .expect_err("path escaping owned roots must be rejected");
        assert!(matches!(err, CliError::Runtime { .. }), "got {err:?}");
    }

    #[test]
    fn resolve_manifest_capabilities_skips_rows_with_empty_caps() {
        let prefix = tempdir().unwrap();
        let layout = FsLayout::system(Some(prefix.path().to_path_buf()));
        // A path but nothing to grant — nothing to do, no setcap invocation.
        let toml = capability_manifest(Some("{bindir}/agentsight"), &[], false);
        let manifest = ComponentManifest::from_toml_str(&toml).expect("parse manifest");
        let reqs =
            resolve_manifest_capabilities(&manifest, &layout, "agentsight").expect("resolve");
        assert!(reqs.is_empty());
    }

    #[test]
    fn resolve_manifest_capabilities_requires_path_when_caps_present() {
        let prefix = tempdir().unwrap();
        let layout = FsLayout::system(Some(prefix.path().to_path_buf()));
        let toml = capability_manifest(None, &["CAP_BPF"], false);
        let manifest = ComponentManifest::from_toml_str(&toml).expect("parse manifest");
        let err = resolve_manifest_capabilities(&manifest, &layout, "agentsight")
            .expect_err("caps without a path is a contract error");
        assert!(matches!(err, CliError::Runtime { .. }), "got {err:?}");
    }

    #[test]
    fn resolve_manifest_services_carries_spec_and_expands_instance() {
        let toml = service_manifest("anolisa-memory@.service", true, false, Some("alice"));
        let manifest = ComponentManifest::from_toml_str(&toml).expect("parse manifest");
        let reqs = resolve_manifest_services(&manifest, "agentsight", "system").expect("resolve");
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].unit, "anolisa-memory@alice.service");
        assert!(reqs[0].enable);
        assert!(!reqs[0].start);
    }

    #[test]
    fn resolve_manifest_services_plain_unit_unchanged() {
        let toml = service_manifest("agentsight.service", true, true, None);
        let manifest = ComponentManifest::from_toml_str(&toml).expect("parse manifest");
        let reqs = resolve_manifest_services(&manifest, "agentsight", "system").expect("resolve");
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].unit, "agentsight.service");
        assert!(reqs[0].enable && reqs[0].start);
        assert_eq!(reqs[0].scope, anolisa_core::ServiceScope::System);
    }

    #[test]
    fn resolve_service_instance_expands_percent_u_only_with_a_caller() {
        // `%u` resolves to the caller; a literal instance passes through in
        // every mode; `%u` with no caller (system-mode place-only) stays None.
        assert_eq!(
            resolve_service_instance("%u", Some("alice")).as_deref(),
            Some("alice")
        );
        assert_eq!(resolve_service_instance("%u", None), None);
        assert_eq!(
            resolve_service_instance("0", Some("alice")).as_deref(),
            Some("0")
        );
        assert_eq!(resolve_service_instance("0", None).as_deref(), Some("0"));
    }

    #[test]
    fn resolve_manifest_services_resolves_percent_u_in_user_mode() {
        let toml = service_manifest("anolisa-memory@.service", false, false, Some("%u"));
        let manifest = ComponentManifest::from_toml_str(&toml).expect("parse manifest");
        let reqs = resolve_manifest_services(&manifest, "agent-memory", "user").expect("resolve");
        // The exact name is the live login user, but `%u` must be gone and the
        // template must be instantiated.
        assert!(
            !reqs[0].unit.contains("%u"),
            "unit must not keep the literal specifier: {}",
            reqs[0].unit
        );
        assert!(reqs[0].unit.starts_with("anolisa-memory@"));
        assert!(reqs[0].unit.ends_with(".service"));
        assert_ne!(reqs[0].unit, "anolisa-memory@.service");
    }

    #[test]
    fn resolve_manifest_services_keeps_percent_u_template_in_system_mode() {
        // System mode is place-only for user-scope templates: leave `%u`
        // un-resolved so per-user `systemctl --user enable` instantiates it.
        let toml = service_manifest("anolisa-memory@.service", false, false, Some("%u"));
        let manifest = ComponentManifest::from_toml_str(&toml).expect("parse manifest");
        let reqs = resolve_manifest_services(&manifest, "agent-memory", "system").expect("resolve");
        assert_eq!(reqs[0].unit, "anolisa-memory@.service");
    }

    #[test]
    fn resolve_install_hooks_classifies_phases_and_filters_uninstall() {
        let tmp = tempdir().expect("tmpdir");
        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let toml = hooks_manifest(&[
            ("pre_install", "{datadir}/hooks/demo/pre-install.sh", false),
            ("post_install", "{datadir}/hooks/demo/post-install.sh", true),
            ("post_enable", "{datadir}/hooks/demo/post-enable.sh", false),
            (
                "pre_uninstall",
                "{datadir}/hooks/demo/pre-uninstall.sh",
                false,
            ),
        ]);
        let manifest = ComponentManifest::from_toml_str(&toml).expect("parse manifest");
        let hooks = resolve_install_hooks(&manifest, &layout, "demo").expect("resolve");

        assert_eq!(hooks.pre_install.len(), 1);
        assert_eq!(hooks.post_install.len(), 1);
        assert!(hooks.post_install[0].strict, "strict carried from contract");
        assert_eq!(hooks.post_enable.len(), 1);
        assert_eq!(
            hooks.pre_install[0].script,
            layout.datadir.join("hooks/demo/pre-install.sh"),
        );
        // The pre_uninstall entry must not leak into any install-phase list.
        let total = hooks.pre_install.len() + hooks.post_install.len() + hooks.post_enable.len();
        assert_eq!(total, 3, "uninstall-phase hook must be excluded");
    }

    #[test]
    fn resolve_install_hooks_rejects_invalid_placeholder() {
        let tmp = tempdir().expect("tmpdir");
        let layout = FsLayout::system(Some(tmp.path().to_path_buf()));
        let toml = hooks_manifest(&[("post_install", "{nope}/x.sh", false)]);
        let manifest = ComponentManifest::from_toml_str(&toml).expect("parse manifest");
        let err = resolve_install_hooks(&manifest, &layout, "demo").expect_err("must error");
        assert!(matches!(err, CliError::Runtime { .. }));
    }

    // -- render mapping ------------------------------------------------------

    fn render_manifest(entry: &str) -> ComponentManifest {
        let toml = format!(
            r#"
            [component]
            name = "sec-core"
            version = "0.9.0"
            [component.layout]
            modes = ["system"]
            {entry}
        "#
        );
        ComponentManifest::from_toml_str(&toml).expect("parse manifest")
    }

    #[test]
    fn resolve_manifest_files_maps_render_v1() {
        let prefix = tempdir().unwrap();
        let layout = FsLayout::system(Some(prefix.path().to_path_buf()));
        let manifest = render_manifest(
            r#"
            [[component.layout.files]]
            source = "share/agent-sec-core.service.in"
            target = "{userunitdir}/agent-sec-core.service"
            render = "anolisa-paths-v1"
        "#,
        );
        let files = resolve_manifest_files(&manifest, &layout, "sec-core").expect("resolve");
        assert_eq!(
            files[0].render,
            Some(RenderSpec {
                mode: RenderMode::AnolisaPathsV1,
                component: "sec-core".to_string(),
            })
        );
    }

    #[test]
    fn resolve_manifest_files_rejects_unknown_render() {
        // A render value this CLI does not implement must fail closed —
        // copying the template verbatim would install an unstartable unit.
        let prefix = tempdir().unwrap();
        let layout = FsLayout::system(Some(prefix.path().to_path_buf()));
        let manifest = render_manifest(
            r#"
            [[component.layout.files]]
            source = "share/unit.in"
            target = "{datadir}/unit"
            render = "anolisa-paths-v2"
        "#,
        );
        let err = resolve_manifest_files(&manifest, &layout, "sec-core")
            .expect_err("unknown render must be rejected");
        match err {
            CliError::InvalidArgument { reason, .. } => {
                assert!(reason.contains("anolisa-paths-v2"), "got: {reason}");
                assert!(reason.contains("self-update"), "got: {reason}");
            }
            other => panic!("expected InvalidArgument, got {other:?}"),
        }
    }

    #[test]
    fn resolve_manifest_files_rejects_render_on_directory_source() {
        let prefix = tempdir().unwrap();
        let layout = FsLayout::system(Some(prefix.path().to_path_buf()));
        let manifest = render_manifest(
            r#"
            [[component.layout.files]]
            source = "share/tree/"
            target = "{datadir}/tree/"
            render = "anolisa-paths-v1"
        "#,
        );
        let err = resolve_manifest_files(&manifest, &layout, "sec-core")
            .expect_err("render on a directory source must be rejected");
        assert!(
            matches!(err, CliError::InvalidArgument { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn resolve_manifest_files_rejects_render_on_symlink() {
        let prefix = tempdir().unwrap();
        let layout = FsLayout::system(Some(prefix.path().to_path_buf()));
        let manifest = render_manifest(
            r#"
            [[component.layout.files]]
            source = "{bindir}/real"
            target = "{bindir}/link"
            type = "symlink"
            render = "anolisa-paths-v1"
        "#,
        );
        let err = resolve_manifest_files(&manifest, &layout, "sec-core")
            .expect_err("render on a symlink must be rejected");
        assert!(
            matches!(err, CliError::InvalidArgument { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn resolve_manifest_files_rejects_blank_render() {
        // Present-but-blank must be rejected, not silently treated as
        // undeclared — that would copy the template verbatim.
        let prefix = tempdir().unwrap();
        let layout = FsLayout::system(Some(prefix.path().to_path_buf()));
        for value in ["", "   "] {
            let manifest = render_manifest(&format!(
                r#"
            [[component.layout.files]]
            source = "share/unit.in"
            target = "{{datadir}}/unit"
            render = "{value}"
        "#
            ));
            let err = resolve_manifest_files(&manifest, &layout, "sec-core")
                .expect_err("blank render must be rejected");
            match err {
                CliError::InvalidArgument { reason, .. } => {
                    assert!(reason.contains("empty render"), "got: {reason}");
                }
                other => panic!("expected InvalidArgument, got {other:?}"),
            }
        }
    }

    #[test]
    fn resolve_manifest_files_without_render_stays_none() {
        let prefix = tempdir().unwrap();
        let layout = FsLayout::system(Some(prefix.path().to_path_buf()));
        let manifest = render_manifest(
            r#"
            [[component.layout.files]]
            source = "bin/agent-sec-cli"
            target = "{bindir}/agent-sec-cli"
            type = "executable"
        "#,
        );
        let files = resolve_manifest_files(&manifest, &layout, "sec-core").expect("resolve");
        assert_eq!(files[0].render, None);
    }

    // -- min_anolisa_version gate --------------------------------------------

    fn contract_manifest(min_version: Option<&str>) -> ComponentManifest {
        let contract = match min_version {
            Some(v) => format!("[component.contract]\nmin_anolisa_version = \"{v}\"\n"),
            None => String::new(),
        };
        let toml = format!(
            r#"
            [component]
            name = "sec-core"
            version = "0.9.0"
            {contract}
        "#
        );
        ComponentManifest::from_toml_str(&toml).expect("parse manifest")
    }

    #[test]
    fn min_anolisa_version_gate_accepts_absent_older_and_equal() {
        let source = InstallContractSource::EmbeddedArtifact;
        validate_min_anolisa_version(&contract_manifest(None), "sec-core", source)
            .expect("absent field must pass");
        validate_min_anolisa_version(&contract_manifest(Some("0.1.0")), "sec-core", source)
            .expect("older requirement must pass");
        let current = env!("CARGO_PKG_VERSION");
        validate_min_anolisa_version(&contract_manifest(Some(current)), "sec-core", source)
            .expect("equal requirement must pass");
    }

    #[test]
    fn min_anolisa_version_gate_rejects_newer_requirement() {
        let err = validate_min_anolisa_version(
            &contract_manifest(Some("99.0.0")),
            "sec-core",
            InstallContractSource::EmbeddedArtifact,
        )
        .expect_err("a newer requirement must be rejected");
        match err {
            CliError::InvalidArgument { reason, .. } => {
                assert!(reason.contains("99.0.0"), "got: {reason}");
                assert!(
                    reason.contains(env!("CARGO_PKG_VERSION")),
                    "must name the current version: {reason}"
                );
                assert!(reason.contains("self-update"), "got: {reason}");
            }
            other => panic!("expected InvalidArgument, got {other:?}"),
        }
    }

    #[test]
    fn min_anolisa_version_gate_rejects_invalid_semver() {
        // Fail-closed: a malformed requirement means the contract's intent
        // is unknown; installing anyway could bypass the gate it wanted.
        let err = validate_min_anolisa_version(
            &contract_manifest(Some("not-a-version")),
            "sec-core",
            InstallContractSource::SidecarMeta,
        )
        .expect_err("invalid SemVer must be rejected");
        match err {
            CliError::InvalidArgument { reason, .. } => {
                assert!(reason.contains("not-a-version"), "got: {reason}");
                assert!(reason.contains("SemVer"), "got: {reason}");
            }
            other => panic!("expected InvalidArgument, got {other:?}"),
        }
    }

    #[test]
    fn min_anolisa_version_gate_protects_first_release_boundary() {
        // Regression for the first contract that depends on unified-contract
        // consumption (sec-core): the capability ships in 0.2.17, one
        // release after 0.2.16 which predates rendering and this gate. A
        // contract declaring 0.2.17 must be refused by a 0.2.16 CLI and
        // accepted from 0.2.17 on — pinned with injected versions so the
        // workspace version bump cannot silently invalidate the scenario.
        let manifest = contract_manifest(Some("0.2.17"));
        let source = InstallContractSource::EmbeddedArtifact;
        let err = validate_min_anolisa_version_against(&manifest, "sec-core", source, "0.2.16")
            .expect_err("a 0.2.16 CLI must refuse a contract requiring 0.2.17");
        assert!(
            matches!(err, CliError::InvalidArgument { .. }),
            "got {err:?}"
        );
        validate_min_anolisa_version_against(&manifest, "sec-core", source, "0.2.17")
            .expect("the release shipping the capability must pass");
        validate_min_anolisa_version_against(&manifest, "sec-core", source, "0.3.0")
            .expect("later releases must keep passing");
    }
}
