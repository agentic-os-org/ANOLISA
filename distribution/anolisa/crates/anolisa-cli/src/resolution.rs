//! Component identity resolution across install backends.
//!
//! Two questions are answered here, deliberately kept apart (issue #2630):
//!
//! 1. **Identity** — is this input a supported component? Exact identities
//!    already recorded in local state are settled by the caller; every other
//!    identity resolves only through the repo-side component index
//!    ([`resolve_index_identity`]). Site-local `package_map` entries and RPM
//!    `Provides: anolisa-component(...)` metadata never establish an identity.
//! 2. **Package** — which backend package implements a known component?
//!    [`ComponentResolver`] answers this for a *settled* component identity,
//!    where `package_map`, explicit package overrides, and RPM `Provides`
//!    may select, validate, or discover the backend package.

use std::collections::BTreeSet;
use std::path::Path;

use anolisa_core::download::{DownloadCache, DownloadError};
use anolisa_platform::fs_layout::FsLayout;
use anolisa_platform::pkg_query::{PackageQuery, PackageQueryError};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::repo_config::{BackendConfig, HostVars, RepoConfig, component_index_v2_url};

/// On-disk schema version for repo-side `components-v2.toml`.
pub(crate) const COMPONENT_INDEX_SCHEMA_VERSION: u32 = 2;

/// Repository-side component identity and backend mapping index.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub(crate) struct ComponentIndex {
    /// Wire schema version; loaders reject versions they do not understand.
    pub(crate) schema_version: u32,
    /// Optional publish timestamp for diagnostics.
    ///
    /// Absent fields deserialize as `None` to keep hand-authored indexes small.
    #[serde(default)]
    pub(crate) generated_at: Option<String>,
    /// Optional publishing party for diagnostics.
    ///
    /// This is informational metadata, not part of resolution.
    #[serde(default)]
    pub(crate) publisher: Option<String>,
    /// Component rows.
    ///
    /// Empty indexes are valid so repositories can publish the file before
    /// every backend mapping has been populated.
    #[serde(default)]
    pub(crate) components: Vec<ComponentIndexEntry>,
}

/// One ANOLISA component and its backend-native identities.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub(crate) struct ComponentIndexEntry {
    /// Stable ANOLISA component name.
    pub(crate) name: String,
    /// Optional human label; not used by identity resolution.
    #[serde(default)]
    pub(crate) display_name: Option<String>,
    /// Optional one-line summary; not used by identity resolution.
    #[serde(default)]
    pub(crate) summary: Option<String>,
    /// Supported host OS/architecture combinations.
    pub(crate) targets: Vec<ComponentTarget>,
    /// Backend-native package names for this component.
    ///
    /// Components may initially ship on only one backend, so the list defaults
    /// to empty rather than forcing placeholder rows.
    #[serde(default)]
    pub(crate) backends: Vec<ComponentBackendEntry>,
    /// Alternate user inputs that should resolve to this component.
    ///
    /// Alias rows are optional and mainly cover historical RPM package names.
    #[serde(default)]
    pub(crate) aliases: Vec<ComponentAliasEntry>,
}

/// One host target supported by a component.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct ComponentTarget {
    /// Operating system identifier used by distribution indexes.
    pub(crate) os: String,
    /// CPU architecture identifier used by distribution indexes.
    pub(crate) arch: String,
}

/// Backend-native identity for a component.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub(crate) struct ComponentBackendEntry {
    /// Backend kind such as `raw` or `rpm`.
    pub(crate) kind: String,
    /// Backend-native package/artifact name.
    pub(crate) package: String,
    /// Expected RPM Provides capability, when this backend is `rpm`.
    ///
    /// This lets repo publishers document the intended package metadata while
    /// still allowing historical rows to exist before RPM specs are updated.
    #[serde(default)]
    pub(crate) provides: Option<String>,
    /// Whether repo metadata may identify a historical installed RPM that lacks
    /// the newer installed `Provides: anolisa-component(...)` declaration.
    ///
    /// Defaulting to false keeps new package mappings strict unless the repo
    /// publisher explicitly marks them as legacy-adoptable.
    #[serde(default)]
    pub(crate) legacy_adopt: bool,
}

/// Alternate input name for a component.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub(crate) struct ComponentAliasEntry {
    /// Alias kind, e.g. `rpm-package`.
    pub(crate) kind: String,
    /// Alias value.
    pub(crate) name: String,
}

fn is_canonical_os(os: &str) -> bool {
    let bytes = os.as_bytes();
    bytes.first().is_some_and(u8::is_ascii_lowercase)
        && bytes
            .last()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
}

fn is_canonical_architecture(architecture: &str) -> bool {
    let bytes = architecture.as_bytes();
    bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.iter().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-' || *byte == b'_'
        })
}

/// Parse or validation failures for `components-v2.toml`.
#[derive(Debug, Error)]
pub(crate) enum ComponentIndexError {
    /// TOML parse or read error.
    #[error("failed to parse component index at {path}: {reason}")]
    Parse { path: String, reason: String },
    /// Unsupported schema version.
    #[error("unsupported component index schema_version {actual} (expected {expected})")]
    UnsupportedSchema { actual: u32, expected: u32 },
    /// Invalid component row.
    #[error("invalid component index entry: {reason}")]
    Invalid { reason: String },
    /// Backend resolution or download failure.
    #[error("failed to fetch component index: {reason}")]
    Fetch { reason: String },
}

/// Backend selected for identity resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BackendKind {
    /// Raw artifact backend.
    Raw,
    /// RPM package backend.
    Rpm,
    /// Any other configured backend.
    Other,
}

impl BackendKind {
    pub(crate) fn from_name(name: &str) -> Self {
        match name {
            "raw" => Self::Raw,
            "rpm" => Self::Rpm,
            _ => Self::Other,
        }
    }
}

/// Source that produced a resolved component/package pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResolutionSource {
    /// Repository component index.
    ComponentIndex,
    /// Site-local `[backends.rpm.package_map]`.
    RepoPackageMap,
    /// RPM metadata declares or provides `anolisa-component(...)` on host.
    InstalledRpmProvides,
    /// RPM repository metadata declares or provides `anolisa-component(...)`.
    AvailableRpmProvides,
    /// Raw distribution index fallback.
    RawDistributionIndex,
}

/// Final identity pair used by command handlers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedTarget {
    pub(crate) component: String,
    pub(crate) backend: BackendKind,
    pub(crate) package: String,
    pub(crate) source: ResolutionSource,
    pub(crate) legacy_adopt: bool,
}

impl ResolvedTarget {
    fn new(
        component: impl Into<String>,
        backend: BackendKind,
        package: impl Into<String>,
        source: ResolutionSource,
        legacy_adopt: bool,
    ) -> Self {
        Self {
            component: component.into(),
            backend,
            package: package.into(),
            source,
            legacy_adopt,
        }
    }
}

/// Cardinality result for resolving an input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResolutionSet {
    /// No backend package could be resolved for the component.
    None,
    /// Exactly one target.
    Unique(ResolvedTarget),
    /// Several targets match and the caller must disambiguate.
    Ambiguous(Vec<ResolvedTarget>),
}

/// Options that affect resolution.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ResolveOptions<'a> {
    /// CLI package override, when supplied.
    pub(crate) package_override: Option<&'a str>,
}

/// Backend package resolver for a settled component identity.
///
/// Every `resolve` input must already be a component identity established by
/// the caller — an exact name from local state or an index-resolved canonical
/// name. The resolver never returns a target whose `component` differs from
/// the input: `package_map`, package overrides, and RPM `Provides` select or
/// validate the backing package, they do not create identities.
pub(crate) struct ComponentResolver<'a> {
    component_index: Option<&'a ComponentIndex>,
    rpm_backend: Option<&'a BackendConfig>,
    rpm_query: Option<&'a dyn PackageQuery>,
}

impl<'a> ComponentResolver<'a> {
    pub(crate) fn new(
        component_index: Option<&'a ComponentIndex>,
        rpm_backend: Option<&'a BackendConfig>,
        rpm_query: Option<&'a dyn PackageQuery>,
    ) -> Self {
        Self {
            component_index,
            rpm_backend,
            rpm_query,
        }
    }

    /// Resolve the backend package for component identity `component`.
    pub(crate) fn resolve(
        &self,
        component: &str,
        backend: BackendKind,
        opts: ResolveOptions<'_>,
    ) -> Result<ResolutionSet, PackageQueryError> {
        match backend {
            BackendKind::Rpm => self.resolve_rpm(component, opts),
            BackendKind::Raw => Ok(self.resolve_raw(component)),
            BackendKind::Other => Ok(ResolutionSet::None),
        }
    }

    fn resolve_raw(&self, component: &str) -> ResolutionSet {
        let targets = self
            .component_index
            .map(|idx| idx.targets_for_component(component, BackendKind::Raw))
            .unwrap_or_default();
        normalize_resolution_set(if targets.is_empty() {
            // Not an identity claim: the component is already settled, and a
            // same-named distribution entry is the raw backend's default
            // package guess. The distribution index refuses unknown packages.
            vec![ResolvedTarget::new(
                component,
                BackendKind::Raw,
                component,
                ResolutionSource::RawDistributionIndex,
                false,
            )]
        } else {
            targets
        })
    }

    fn resolve_rpm(
        &self,
        component: &str,
        opts: ResolveOptions<'_>,
    ) -> Result<ResolutionSet, PackageQueryError> {
        let query = self
            .rpm_query
            .expect("rpm resolution requires a PackageQuery");
        let mapped = self.rpm_backend.and_then(|b| b.package_map.get(component));

        if let Some(package) = opts.package_override {
            let mut targets = Vec::new();
            if mapped.is_some_and(|mapped| mapped == package) {
                targets.push(ResolvedTarget::new(
                    component,
                    BackendKind::Rpm,
                    package,
                    ResolutionSource::RepoPackageMap,
                    true,
                ));
            }
            if let Some(idx) = self.component_index {
                targets.extend(idx.targets_for_component_package(
                    component,
                    BackendKind::Rpm,
                    package,
                ));
            }
            if let Some(target) = rpm_package_provides_component(query, package, component)? {
                targets.push(target);
            }
            return Ok(normalize_resolution_set(targets));
        }

        if let Some(idx) = self.component_index {
            let targets = idx.targets_for_component(component, BackendKind::Rpm);
            if !targets.is_empty() {
                return Ok(normalize_resolution_set(targets));
            }
        }

        if let Some(package) = mapped {
            return Ok(ResolutionSet::Unique(ResolvedTarget::new(
                component,
                BackendKind::Rpm,
                package,
                ResolutionSource::RepoPackageMap,
                true,
            )));
        }

        let provide = rpm_component_provide(component);
        let installed_providers = query.what_provides_installed(&provide)?;
        if !installed_providers.is_empty() {
            return Ok(normalize_resolution_set(
                installed_providers
                    .into_iter()
                    .map(|package| {
                        ResolvedTarget::new(
                            component,
                            BackendKind::Rpm,
                            package,
                            ResolutionSource::InstalledRpmProvides,
                            true,
                        )
                    })
                    .collect(),
            ));
        }

        let available_providers = query.what_provides_available(&provide)?;
        if !available_providers.is_empty() {
            return Ok(normalize_resolution_set(
                available_providers
                    .into_iter()
                    .map(|package| {
                        ResolvedTarget::new(
                            component,
                            BackendKind::Rpm,
                            package,
                            ResolutionSource::AvailableRpmProvides,
                            true,
                        )
                    })
                    .collect(),
            ));
        }

        Ok(ResolutionSet::None)
    }
}

/// Identity verdict for an input not backed by exact local state.
///
/// Produced by [`resolve_index_identity`]; callers translate the two failure
/// verdicts into their command's public errors so "the index rejected this
/// name" and "no index was available to consult" stay distinguishable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum IndexIdentity {
    /// The index recognizes the input and names this canonical component.
    Resolved(String),
    /// The index is authoritative and has no entry matching the input.
    Unsupported,
    /// No index was available, so the input cannot be validated.
    Unavailable,
}

/// Resolve an input to a component identity through the component index.
///
/// The index is the sole authority for identities that are not already
/// present in exact local state (issue #2630): canonical component names,
/// declared aliases, and backend package names resolve to the entry's
/// canonical name; anything else is [`IndexIdentity::Unsupported`]. Without
/// an index no new identity can be established at all.
pub(crate) fn resolve_index_identity(
    input: &str,
    component_index: Option<&ComponentIndex>,
) -> IndexIdentity {
    let Some(index) = component_index else {
        return IndexIdentity::Unavailable;
    };
    // Name-level matching over canonical names, declared aliases, and backend
    // package names, deliberately ignoring alias kinds and backend rows:
    // identity is about what the index calls the name, not which backends
    // exist for it. Canonical spelling earns no precedence — if two distinct
    // components claim the same name in any role, the lookup is ambiguous and
    // must not silently pick one.
    let mut matches: BTreeSet<&str> = BTreeSet::new();
    for entry in &index.components {
        if entry.name == input
            || entry.aliases.iter().any(|alias| alias.name == input)
            || entry
                .backends
                .iter()
                .any(|backend| backend.package == input)
        {
            matches.insert(entry.name.as_str());
        }
    }
    let mut names = matches.into_iter();
    match (names.next(), names.next()) {
        (Some(component), None) => IndexIdentity::Resolved(component.to_string()),
        _ => IndexIdentity::Unsupported,
    }
}

impl ComponentIndex {
    /// Parse and validate a component index.
    pub(crate) fn from_toml_str(
        s: &str,
        path: impl AsRef<Path>,
    ) -> Result<Self, ComponentIndexError> {
        let path = path.as_ref().display().to_string();
        let parsed: Self = toml::from_str(s).map_err(|err| ComponentIndexError::Parse {
            path: path.clone(),
            reason: err.to_string(),
        })?;
        parsed.validate()?;
        Ok(parsed)
    }

    /// Load and validate from disk.
    pub(crate) fn load(path: impl AsRef<Path>) -> Result<Self, ComponentIndexError> {
        let path_ref = path.as_ref();
        let content =
            std::fs::read_to_string(path_ref).map_err(|err| ComponentIndexError::Parse {
                path: path_ref.display().to_string(),
                reason: err.to_string(),
            })?;
        Self::from_toml_str(&content, path_ref)
    }

    fn validate(&self) -> Result<(), ComponentIndexError> {
        if self.schema_version != COMPONENT_INDEX_SCHEMA_VERSION {
            return Err(ComponentIndexError::UnsupportedSchema {
                actual: self.schema_version,
                expected: COMPONENT_INDEX_SCHEMA_VERSION,
            });
        }
        let mut names = BTreeSet::new();
        for entry in &self.components {
            let name = entry.name.trim();
            if name.is_empty() {
                return Err(ComponentIndexError::Invalid {
                    reason: "component name must not be empty".to_string(),
                });
            }
            if !names.insert(name.to_string()) {
                return Err(ComponentIndexError::Invalid {
                    reason: format!("duplicate component '{name}'"),
                });
            }
            if entry.targets.is_empty() {
                return Err(ComponentIndexError::Invalid {
                    reason: format!("component '{name}' must declare at least one target"),
                });
            }
            let mut targets = BTreeSet::new();
            for target in &entry.targets {
                if !is_canonical_os(&target.os) || !is_canonical_architecture(&target.arch) {
                    return Err(ComponentIndexError::Invalid {
                        reason: format!(
                            "component '{name}' target '{}/{}' must use canonical lowercase OS and architecture identifiers",
                            target.os, target.arch
                        ),
                    });
                }
                if !targets.insert((target.os.as_str(), target.arch.as_str())) {
                    return Err(ComponentIndexError::Invalid {
                        reason: format!(
                            "component '{name}' declares duplicate target '{}/{}'",
                            target.os, target.arch
                        ),
                    });
                }
            }
            for backend in &entry.backends {
                if backend.kind.trim().is_empty() {
                    return Err(ComponentIndexError::Invalid {
                        reason: format!("component '{name}' has an empty backend kind"),
                    });
                }
                if backend.package.trim().is_empty() {
                    return Err(ComponentIndexError::Invalid {
                        reason: format!("component '{name}' has an empty backend package"),
                    });
                }
                if let Some(provides) = backend.provides.as_deref()
                    && BackendKind::from_name(&backend.kind) == BackendKind::Rpm
                    && provides != rpm_component_provide(name)
                {
                    return Err(ComponentIndexError::Invalid {
                        reason: format!(
                            "component '{name}' rpm provides must be '{}', got '{provides}'",
                            rpm_component_provide(name)
                        ),
                    });
                }
            }
            for alias in &entry.aliases {
                if alias.kind.trim().is_empty() || alias.name.trim().is_empty() {
                    return Err(ComponentIndexError::Invalid {
                        reason: format!("component '{name}' has an empty alias kind or name"),
                    });
                }
            }
        }
        Ok(())
    }

    /// Backend targets for the entry whose canonical name is `component`.
    ///
    /// Package-resolution matching: aliases and backend package names are
    /// deliberately not consulted, so a settled identity can never be
    /// remapped to another component by its backend rows.
    fn targets_for_component(&self, component: &str, backend: BackendKind) -> Vec<ResolvedTarget> {
        let mut targets = Vec::new();
        for entry in &self.components {
            if entry.name != component {
                continue;
            }
            for backend_entry in entry.backends_for(backend) {
                targets.push(index_target(entry, backend, backend_entry));
            }
        }
        targets
    }

    fn targets_for_component_package(
        &self,
        component: &str,
        backend: BackendKind,
        package: &str,
    ) -> Vec<ResolvedTarget> {
        let mut targets = Vec::new();
        for entry in &self.components {
            if entry.name != component {
                continue;
            }
            for backend_entry in entry.backends_for(backend) {
                if backend_entry.package == package {
                    targets.push(index_target(entry, backend, backend_entry));
                }
            }
        }
        targets
    }
}

impl ComponentIndexEntry {
    /// Whether the component index permits installation on `os`/`arch`.
    pub(crate) fn supports_target(&self, os: &str, arch: &str) -> bool {
        self.targets
            .iter()
            .any(|candidate| candidate.os == os && candidate.arch == arch)
    }

    fn backends_for(&self, backend: BackendKind) -> impl Iterator<Item = &ComponentBackendEntry> {
        self.backends
            .iter()
            .filter(move |entry| BackendKind::from_name(&entry.kind) == backend)
    }
}

fn index_target(
    entry: &ComponentIndexEntry,
    backend: BackendKind,
    backend_entry: &ComponentBackendEntry,
) -> ResolvedTarget {
    ResolvedTarget::new(
        entry.name.clone(),
        backend,
        backend_entry.package.clone(),
        ResolutionSource::ComponentIndex,
        backend_entry.legacy_adopt,
    )
}

fn normalize_resolution_set(mut targets: Vec<ResolvedTarget>) -> ResolutionSet {
    let mut deduped = Vec::new();
    for target in targets.drain(..) {
        if !deduped.iter().any(|seen: &ResolvedTarget| {
            seen.component == target.component
                && seen.backend == target.backend
                && seen.package == target.package
        }) {
            deduped.push(target);
        }
    }
    match deduped.len() {
        0 => ResolutionSet::None,
        1 => ResolutionSet::Unique(deduped.remove(0)),
        _ => ResolutionSet::Ambiguous(deduped),
    }
}

pub(crate) fn rpm_component_provide(component: &str) -> String {
    format!("anolisa-component({component})")
}

fn rpm_package_provides_component(
    query: &dyn PackageQuery,
    package: &str,
    component: &str,
) -> Result<Option<ResolvedTarget>, PackageQueryError> {
    let capability = rpm_component_provide(component);
    let (providers, source) = match query.query_installed(package)? {
        Some(_) => (
            query.what_provides_installed(&capability)?,
            ResolutionSource::InstalledRpmProvides,
        ),
        None => (
            query.what_provides_available(&capability)?,
            ResolutionSource::AvailableRpmProvides,
        ),
    };
    Ok(providers
        .iter()
        .any(|provider| provider == package)
        .then(|| ResolvedTarget::new(component, BackendKind::Rpm, package, source, true)))
}

/// Load repo-side `components-v2.toml`, returning a structured error on failure.
///
/// Used by commands (`ls`, `install --all`) that require the component index
/// to function. For best-effort usage where a missing index is acceptable,
/// use [`load_optional_component_index`] instead.
pub(crate) fn load_component_index(
    layout: &FsLayout,
    env: &anolisa_env::EnvFacts,
    repo_config: &RepoConfig,
) -> Result<ComponentIndex, ComponentIndexError> {
    let host = HostVars {
        os: env.os.clone(),
        arch: env.arch.clone(),
    };
    let (name, backend) =
        repo_config
            .select_backend(Some("raw"))
            .map_err(|err| ComponentIndexError::Fetch {
                reason: format!("cannot resolve raw backend in repo.toml: {err}"),
            })?;
    let base_url = repo_config
        .resolved_base_url(name, backend, &host)
        .map_err(|err| ComponentIndexError::Fetch {
            reason: format!("cannot resolve base_url for raw backend: {err}"),
        })?;
    load_component_index_from_base(layout, &base_url)
}

/// Load `components-v2.toml` published under an explicit repository base URL.
///
/// Used directly for a CLI `--repo` override, where the named repository is
/// the identity authority for that invocation.
pub(crate) fn load_component_index_from_base(
    layout: &FsLayout,
    base_url: &str,
) -> Result<ComponentIndex, ComponentIndexError> {
    let url = component_index_v2_url(base_url);

    let cache = DownloadCache::new(layout.cache_dir.clone());
    #[cfg(test)]
    if !url.starts_with("file://") {
        return Err(ComponentIndexError::Fetch {
            reason: format!("test mode: refusing non-file URL {url}"),
        });
    }
    let temporary_cache;
    let downloaded = match cache.fetch(&url, None) {
        Err(DownloadError::Io { path, source })
            if source.kind() == std::io::ErrorKind::PermissionDenied
                && path.starts_with(&layout.cache_dir) =>
        {
            // System previews must not require a writable system cache or home.
            // Keep the private cache alive until the index has been parsed.
            temporary_cache = tempfile::tempdir().map_err(|err| ComponentIndexError::Fetch {
                reason: format!("cannot create temporary component index cache: {err}"),
            })?;
            DownloadCache::new(temporary_cache.path().to_path_buf()).fetch(&url, None)
        }
        result => result,
    }
    .map_err(|err| ComponentIndexError::Fetch {
        reason: format!("failed to fetch {url}: {err}"),
    })?;
    ComponentIndex::load(&downloaded.cached_path)
}

/// Best-effort load of repo-side `components-v2.toml`.
pub(crate) fn load_optional_component_index(
    layout: &FsLayout,
    env: &anolisa_env::EnvFacts,
    repo_config: &RepoConfig,
) -> Option<ComponentIndex> {
    load_component_index(layout, env, repo_config).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use anolisa_platform::pkg_query::{PackageInfo, PackageVersion};

    #[cfg(unix)]
    #[test]
    fn component_index_loads_without_write_access_to_system_cache() {
        use std::os::unix::fs::PermissionsExt;

        // Root bypasses mode bits; run this regression as an unprivileged user.
        if anolisa_platform::privilege::is_root() {
            return;
        }
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = FsLayout::system(Some(tmp.path().join("system")));
        let repo = tmp.path().join("repo/v1");
        std::fs::create_dir_all(&repo).expect("repo directory");
        let index_path = repo.join("components-v2.toml");
        std::fs::write(
            &index_path,
            "schema_version = 2\n[[components]]\nname = 'cosh'\ntargets = [{ os = 'linux', arch = 'x86_64' }]\n",
        )
        .expect("index");
        std::fs::create_dir_all(&layout.cache_dir).expect("cache directory");
        std::fs::set_permissions(&layout.cache_dir, std::fs::Permissions::from_mode(0o555))
            .expect("read-only cache");

        let base_url = format!("file://{}", repo.display());
        let loaded = load_component_index_from_base(&layout, &base_url);
        std::fs::write(&index_path, "schema_version = 1\n").expect("invalid index");
        let invalid = load_component_index_from_base(&layout, &base_url);
        let cache_entries = std::fs::read_dir(&layout.cache_dir)
            .expect("read cache")
            .count();
        std::fs::set_permissions(&layout.cache_dir, std::fs::Permissions::from_mode(0o755))
            .expect("restore cache permissions");

        let index = loaded.expect("read-only cache must not prevent index validation");
        assert_eq!(
            resolve_index_identity("cosh", Some(&index)),
            IndexIdentity::Resolved("cosh".to_string())
        );
        assert_eq!(
            resolve_index_identity("unknown", Some(&index)),
            IndexIdentity::Unsupported
        );
        assert!(matches!(
            invalid,
            Err(ComponentIndexError::UnsupportedSchema { .. })
        ));
        assert_eq!(cache_entries, 0, "system cache must remain untouched");
    }

    #[derive(Default)]
    struct FakeQuery {
        installed: Vec<(String, PackageInfo)>,
        component_providers: Vec<(String, Vec<String>)>,
        available_component_providers: Vec<(String, Vec<String>)>,
        package_provides: Vec<(String, Vec<String>)>,
        available_package_provides: Vec<(String, Vec<String>)>,
    }

    impl PackageQuery for FakeQuery {
        fn query_installed(&self, package: &str) -> Result<Option<PackageInfo>, PackageQueryError> {
            Ok(self
                .installed
                .iter()
                .find(|(name, _)| name == package)
                .map(|(_, info)| info.clone()))
        }

        fn query_available(&self, _package: &str) -> Result<Vec<PackageInfo>, PackageQueryError> {
            Ok(Vec::new())
        }

        fn what_provides_installed(
            &self,
            capability: &str,
        ) -> Result<Vec<String>, PackageQueryError> {
            Ok(self
                .component_providers
                .iter()
                .find(|(cap, _)| cap == capability)
                .map(|(_, providers)| providers.clone())
                .unwrap_or_default())
        }

        fn what_provides_available(
            &self,
            capability: &str,
        ) -> Result<Vec<String>, PackageQueryError> {
            Ok(self
                .available_component_providers
                .iter()
                .find(|(cap, _)| cap == capability)
                .map(|(_, providers)| providers.clone())
                .unwrap_or_default())
        }

        fn provided_capabilities_installed(
            &self,
            package: &str,
        ) -> Result<Vec<String>, PackageQueryError> {
            Ok(self
                .package_provides
                .iter()
                .find(|(pkg, _)| pkg == package)
                .map(|(_, caps)| caps.clone())
                .unwrap_or_default())
        }

        fn provided_capabilities_available(
            &self,
            package: &str,
        ) -> Result<Vec<String>, PackageQueryError> {
            Ok(self
                .available_package_provides
                .iter()
                .find(|(pkg, _)| pkg == package)
                .map(|(_, caps)| caps.clone())
                .unwrap_or_default())
        }
    }

    fn pkg_info(name: &str) -> PackageInfo {
        PackageInfo {
            name: name.to_string(),
            version: PackageVersion {
                epoch: None,
                version: "1.0.0".to_string(),
                release: Some("1.al8".to_string()),
            },
            arch: "x86_64".to_string(),
            origin: None,
        }
    }

    fn index() -> ComponentIndex {
        ComponentIndex::from_toml_str(
            r#"
schema_version = 2
publisher = "anolisa"

[[components]]
name = "cosh"
display_name = "Copilot Shell"
summary = "shell"
targets = [{ os = "linux", arch = "x86_64" }]

[[components.backends]]
kind = "raw"
package = "cosh"

[[components.backends]]
kind = "rpm"
package = "copilot-shell"
provides = "anolisa-component(cosh)"
legacy_adopt = true

[[components.aliases]]
kind = "rpm-package"
name = "copilot-shell"
"#,
            "components.toml",
        )
        .expect("parse index")
    }

    #[test]
    fn repository_component_index_template_is_valid() {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let index_path = manifest_dir.join("../../manifests/components-v2.toml");
        ComponentIndex::load(&index_path).expect("component index template must parse");
    }

    #[test]
    fn repository_component_indexes_v1_and_v2_register_the_same_components() {
        // The CLI resolves identities only through the v2 index, while released
        // clients keep reading v1; a component registered in one file but not
        // the other becomes unresolvable for one of the two populations.
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let v1_path = manifest_dir.join("../../manifests/components.toml");
        let v2_path = manifest_dir.join("../../manifests/components-v2.toml");

        let v2 = ComponentIndex::load(&v2_path).expect("component index v2 must parse");
        let v2_names: BTreeSet<String> = v2
            .components
            .iter()
            .map(|component| component.name.clone())
            .collect();

        let v1_body = std::fs::read_to_string(&v1_path).expect("read v1 component index");
        let v1: toml::Table = toml::from_str(&v1_body).expect("parse v1 component index");
        let v1_names: BTreeSet<String> = v1["components"]
            .as_array()
            .expect("v1 index declares [[components]] rows")
            .iter()
            .map(|entry| {
                entry["name"]
                    .as_str()
                    .expect("v1 component row carries a name")
                    .to_string()
            })
            .collect();

        assert_eq!(
            v1_names, v2_names,
            "components registered in only one of components.toml / components-v2.toml \
             cannot be resolved by clients reading the other"
        );
    }

    #[test]
    fn component_index_requires_at_least_one_target() {
        let source = r#"
schema_version = 2

[[components]]
name = "test"
targets = []
"#;

        let err = ComponentIndex::from_toml_str(source, "components.toml")
            .expect_err("empty targets must be rejected");
        assert!(
            matches!(
                err,
                ComponentIndexError::Invalid { ref reason }
                    if reason.contains("at least one target")
            ),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn invalid_or_duplicate_targets_are_rejected() {
        for (targets, expected) in [
            (
                r#"[{ os = "Linux", arch = "x86_64" }]"#,
                "canonical lowercase",
            ),
            (
                r#"[{ os = "linux", arch = "X86_64" }]"#,
                "canonical lowercase",
            ),
            (
                r#"[{ os = "linux", arch = "x86_64" }, { os = "linux", arch = "x86_64" }]"#,
                "duplicate target",
            ),
        ] {
            let source = format!(
                r#"
schema_version = 2

[[components]]
name = "test"
targets = {targets}
"#
            );

            let err = ComponentIndex::from_toml_str(&source, "components.toml")
                .expect_err("invalid target must be rejected");
            assert!(
                matches!(
                    err,
                    ComponentIndexError::Invalid { ref reason }
                        if reason.contains(expected)
                ),
                "unexpected error for {targets}: {err}"
            );
        }
    }

    #[test]
    fn repository_component_index_declares_current_target_matrix() {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let index_path = manifest_dir.join("../../manifests/components-v2.toml");
        let index = ComponentIndex::load(&index_path).expect("component index template must parse");

        assert!(
            index
                .components
                .iter()
                .all(|component| component.supports_target("linux", "x86_64"))
        );
        let macos_components = index
            .components
            .iter()
            .filter(|component| component.targets.iter().any(|target| target.os == "macos"))
            .map(|component| component.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(macos_components, ["cosh-ng", "agentsight", "tokenless"]);

        let agentsight = index
            .components
            .iter()
            .find(|component| component.name == "agentsight")
            .expect("agentsight entry");
        assert!(agentsight.supports_target("macos", "aarch64"));
        assert!(!agentsight.supports_target("macos", "x86_64"));
        assert!(!agentsight.supports_target("linux", "aarch64"));

        for name in ["cosh-ng", "tokenless"] {
            let component = index
                .components
                .iter()
                .find(|component| component.name == name)
                .expect("component entry");
            assert!(component.supports_target("linux", "aarch64"));
            assert!(component.supports_target("macos", "aarch64"));
            assert!(!component.supports_target("macos", "x86_64"));
        }
    }

    #[test]
    fn repository_component_index_uses_sec_core_as_canonical_name() {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let index_path = manifest_dir.join("../../manifests/components-v2.toml");
        let idx = ComponentIndex::load(&index_path).expect("component index template must parse");
        let query = FakeQuery::default();
        let resolver = ComponentResolver::new(Some(&idx), None, Some(&query));

        let got = resolver
            .resolve("sec-core", BackendKind::Rpm, ResolveOptions::default())
            .expect("resolve sec-core");
        match got {
            ResolutionSet::Unique(target) => {
                assert_eq!(target.component, "sec-core");
                assert_eq!(target.package, "agent-sec-core");
                assert_eq!(target.source, ResolutionSource::ComponentIndex);
            }
            other => panic!("expected unique, got {other:?}"),
        }

        assert_eq!(
            resolve_index_identity("agent-sec-core", Some(&idx)),
            IndexIdentity::Resolved("sec-core".to_string()),
            "the rpm package name is an index-declared identity for sec-core",
        );
    }

    #[test]
    fn repository_component_index_maps_cosh_ng_rpm_to_cosh_ng() {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let index_path = manifest_dir.join("../../manifests/components-v2.toml");
        let idx = ComponentIndex::load(&index_path).expect("component index template must parse");
        let query = FakeQuery::default();
        let resolver = ComponentResolver::new(Some(&idx), None, Some(&query));

        let got = resolver
            .resolve("cosh-ng", BackendKind::Rpm, ResolveOptions::default())
            .expect("resolve cosh-ng");

        match got {
            ResolutionSet::Unique(target) => {
                assert_eq!(target.component, "cosh-ng");
                assert_eq!(target.package, "cosh-ng");
                assert_eq!(target.source, ResolutionSource::ComponentIndex);
            }
            other => panic!("expected unique, got {other:?}"),
        }
    }

    #[test]
    fn load_optional_component_index_uses_generation_2_raw_index_for_rpm_resolution() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let layout =
            anolisa_platform::fs_layout::FsLayout::system(Some(tmp.path().join("install-root")));
        let raw_parent = tmp.path().join("a-raw");
        let raw_v1 = raw_parent.join("v1");
        let rpm_root = tmp.path().join("z-rpm");
        std::fs::create_dir_all(&raw_v1).expect("mkdir raw repo");
        std::fs::create_dir_all(&rpm_root).expect("mkdir rpm repo");
        std::fs::write(
            raw_v1.join("components.toml"),
            r#"
schema_version = 1

[[components]]
name = "legacy-index"
platforms = ["linux"]
"#,
        )
        .expect("write generation-1 component index");
        std::fs::write(
            raw_v1.join("components-v2.toml"),
            r#"
schema_version = 2

[[components]]
name = "raw-index"
targets = [{ os = "linux", arch = "x86_64" }]
"#,
        )
        .expect("write generation-2 component index");
        std::fs::write(
            rpm_root.join("components-v2.toml"),
            r#"
schema_version = 2

[[components]]
name = "rpm-ignored"
targets = [{ os = "linux", arch = "x86_64" }]
"#,
        )
        .expect("write rpm component index");
        let repo_config = RepoConfig::from_toml_str(&format!(
            r#"
schema_version = 1
default_backend = "rpm"

[backends.raw]
base_url = "file://{}"

[backends.rpm]
base_url = "file://{}"
"#,
            raw_parent.display(),
            rpm_root.display()
        ))
        .expect("repo config");
        let env = anolisa_env::EnvFacts {
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
            libc: None,
            kernel: None,
            pkg_base: None,
            os_id: None,
            os_id_like: None,
            os_version: None,
            btf: None,
            cap_bpf: None,
            container: None,
            user: "tester".to_string(),
            uid: 1000,
            home: tmp.path().join("home"),
        };

        let idx =
            load_optional_component_index(&layout, &env, &repo_config).expect("load raw index");

        assert_eq!(idx.components.len(), 1);
        assert_eq!(idx.components[0].name, "raw-index");
    }

    #[test]
    fn component_index_resolves_component_name_to_rpm_package() {
        let idx = index();
        let query = FakeQuery::default();
        let resolver = ComponentResolver::new(Some(&idx), None, Some(&query));
        let got = resolver
            .resolve("cosh", BackendKind::Rpm, ResolveOptions::default())
            .expect("resolve");
        match got {
            ResolutionSet::Unique(target) => {
                assert_eq!(target.component, "cosh");
                assert_eq!(target.package, "copilot-shell");
                assert_eq!(target.source, ResolutionSource::ComponentIndex);
                assert!(target.legacy_adopt);
            }
            other => panic!("expected unique, got {other:?}"),
        }
    }

    #[test]
    fn component_index_precedes_package_map() {
        let idx = index();
        let repo = RepoConfig::from_toml_str(
            r#"
schema_version = 1
default_backend = "rpm"

[backends.rpm]
base_url = "https://example.invalid/rpm"

[backends.rpm.package_map]
cosh = "site-copilot"
"#,
        )
        .expect("repo config");
        let query = FakeQuery::default();
        let resolver = ComponentResolver::new(Some(&idx), repo.backends.get("rpm"), Some(&query));
        let got = resolver
            .resolve("cosh", BackendKind::Rpm, ResolveOptions::default())
            .expect("resolve");
        match got {
            ResolutionSet::Unique(target) => {
                assert_eq!(target.package, "copilot-shell");
                assert_eq!(target.source, ResolutionSource::ComponentIndex);
            }
            other => panic!("expected unique, got {other:?}"),
        }
    }

    #[test]
    fn index_identity_resolves_canonical_alias_and_package_names() {
        let idx = index();
        assert_eq!(
            resolve_index_identity("cosh", Some(&idx)),
            IndexIdentity::Resolved("cosh".to_string()),
        );
        assert_eq!(
            resolve_index_identity("copilot-shell", Some(&idx)),
            IndexIdentity::Resolved("cosh".to_string()),
        );
        assert_eq!(
            resolve_index_identity("unknown-component", Some(&idx)),
            IndexIdentity::Unsupported,
        );
        assert_eq!(
            resolve_index_identity("cosh", None),
            IndexIdentity::Unavailable,
        );
    }

    #[test]
    fn index_identity_resolves_entries_without_backend_rows() {
        let idx = ComponentIndex::from_toml_str(
            r#"
schema_version = 2

[[components]]
name = "backendless"
targets = [{ os = "linux", arch = "x86_64" }]
"#,
            "components.toml",
        )
        .expect("parse index");
        assert_eq!(
            resolve_index_identity("backendless", Some(&idx)),
            IndexIdentity::Resolved("backendless".to_string()),
        );
    }

    #[test]
    fn index_identity_refuses_canonical_names_claimed_by_another_component() {
        // Canonical spelling earns no precedence: when a second component
        // also claims the name — through an alias or a backend package —
        // the lookup is ambiguous and must not silently pick the canonical
        // entry. The claimant's own canonical name stays unambiguous.
        for claim in [
            r#"aliases = [{ kind = "legacy", name = "cosh" }]"#,
            "[[components.backends]]\nkind = \"raw\"\npackage = \"cosh\"",
        ] {
            let idx = ComponentIndex::from_toml_str(
                &format!(
                    r#"
schema_version = 2

[[components]]
name = "cosh"
targets = [{{ os = "linux", arch = "x86_64" }}]

[[components]]
name = "claimant"
targets = [{{ os = "linux", arch = "x86_64" }}]
{claim}
"#
                ),
                "components.toml",
            )
            .expect("parse index");
            assert_eq!(
                resolve_index_identity("cosh", Some(&idx)),
                IndexIdentity::Unsupported,
            );
            assert_eq!(
                resolve_index_identity("claimant", Some(&idx)),
                IndexIdentity::Resolved("claimant".to_string()),
            );
        }
    }

    #[test]
    fn package_resolution_never_remaps_a_settled_identity() {
        // `copilot-shell` is an alias/package name of `cosh` in the index and
        // its installed RPM declares the cosh component capability. Neither
        // may turn the settled identity `copilot-shell` into `cosh` at the
        // package layer — identity resolution happens before, through
        // `resolve_index_identity`.
        let idx = index();
        let query = FakeQuery {
            installed: vec![("copilot-shell".to_string(), pkg_info("copilot-shell"))],
            package_provides: vec![(
                "copilot-shell".to_string(),
                vec!["anolisa-component(cosh) = 1.0.0".to_string()],
            )],
            ..Default::default()
        };
        let resolver = ComponentResolver::new(Some(&idx), None, Some(&query));
        let got = resolver
            .resolve("copilot-shell", BackendKind::Rpm, ResolveOptions::default())
            .expect("resolve");
        assert_eq!(got, ResolutionSet::None);
    }

    #[test]
    fn component_index_resolves_raw_component() {
        let idx = index();
        let resolver = ComponentResolver::new(Some(&idx), None, None);
        let got = resolver
            .resolve("cosh", BackendKind::Raw, ResolveOptions::default())
            .expect("resolve");
        match got {
            ResolutionSet::Unique(target) => {
                assert_eq!(target.component, "cosh");
                assert_eq!(target.package, "cosh");
                assert_eq!(target.source, ResolutionSource::ComponentIndex);
            }
            other => panic!("expected unique, got {other:?}"),
        }
    }

    #[test]
    fn available_component_provider_identifies_absent_package() {
        let query = FakeQuery {
            available_component_providers: vec![(
                "anolisa-component(cosh)".to_string(),
                vec!["copilot-shell".to_string()],
            )],
            ..Default::default()
        };
        let resolver = ComponentResolver::new(None, None, Some(&query));
        let got = resolver
            .resolve("cosh", BackendKind::Rpm, ResolveOptions::default())
            .expect("resolve");
        match got {
            ResolutionSet::Unique(target) => {
                assert_eq!(target.component, "cosh");
                assert_eq!(target.package, "copilot-shell");
                assert_eq!(target.source, ResolutionSource::AvailableRpmProvides);
            }
            other => panic!("expected unique, got {other:?}"),
        }
    }

    #[test]
    fn plain_rpm_package_without_metadata_is_none() {
        let query = FakeQuery {
            installed: vec![("bash".to_string(), pkg_info("bash"))],
            ..Default::default()
        };
        let resolver = ComponentResolver::new(None, None, Some(&query));
        let got = resolver
            .resolve("bash", BackendKind::Rpm, ResolveOptions::default())
            .expect("resolve");
        assert_eq!(got, ResolutionSet::None);
    }

    #[test]
    fn unsupported_schema_is_rejected() {
        for actual in [1, 99] {
            let source = format!("schema_version = {actual}");
            let err = ComponentIndex::from_toml_str(&source, "components.toml")
                .expect_err("unsupported schema");
            assert!(matches!(
                err,
                ComponentIndexError::UnsupportedSchema {
                    actual: rejected,
                    ..
                } if rejected == actual
            ));
        }
    }
}
