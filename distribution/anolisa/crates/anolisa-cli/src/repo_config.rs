//! Repository configuration (`repo.toml`): which install backends exist,
//! where each one points, and which is the default.
//!
//! Design (see the install-backend discussion):
//!
//! * **One configuration table per backend** (`[backends.raw]`,
//!   `[backends.rpm]`, `[backends.npm]`) — no repo list, no priorities.
//!   Selection is explicit: CLI `--backend` > `default_backend`. There is
//!   no cross-backend fallback, so the origin of an installed component
//!   is always deterministic.
//! * **`base_url` is a directory root**, never a file. The per-backend
//!   convention decides what lives under it: raw treats it as the
//!   `v1/` distribution root containing `index.toml`; the rpm backend
//!   hands it to dnf; npm treats it as the registry API root.
//! * **Variables** `$os` / `$arch` / `$basearch` / `$releasever` /
//!   `$channel` substitute into `base_url` only. Values come from host
//!   detection and can be overridden in `[vars]`; an unknown or unset
//!   variable is a hard error — a URL with a silently-preserved `$typo`
//!   is the hardest failure to diagnose downstream.
//! * **Schemes**: `file://` and `https://` always allowed; `http://`
//!   requires `insecure = true` on the entry; query strings and
//!   fragments are rejected.
//! * **Raw layout**: the repository path layout is code-owned. Index rows
//!   with empty `url` resolve to
//!   `<base_url>/{component}/{version}/{os}/{arch}/{component}-{version}-{os}-{arch}{ext}`.
//!   Older configs that still include `{component}` placeholders in
//!   `base_url` are tolerated by using the static prefix before the first
//!   placeholder as the raw root.
//!
//! Discovery order (first hit wins):
//!
//!   1. **User/site config** — `<etc_dir>/repo.toml`
//!      (`/etc/anolisa/repo.toml` for system mode,
//!      `~/.config/anolisa/repo.toml` for user mode). The user-editable
//!      location; unlike the execution policy this file is configuration,
//!      not a packaged asset, so it is probed first.
//!   2. **Packaged** — `<datadir>/templates/repo.toml`.
//!   3. **Dev-tree** — `<workspace>/templates/repo.toml` via
//!      `CARGO_MANIFEST_DIR` (what `cargo run` / `cargo test` see).
//!
//! [`RepoConfig::load`] uses the discovery chain first and provisions a site
//! config only when every local source is missing. Callers do not need separate
//! first-run handling around repo config reads.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anolisa_platform::fs_layout::FsLayout;
use serde::Deserialize;

use crate::packaged;

/// Filename probed in every discovery location.
const REPO_FILE: &str = "repo.toml";
/// Subdirectory under `datadir` for the packaged copy.
const REPO_SUBDIR: &str = "templates";
/// Published copy of `templates/repo.toml` used when no local source exists.
const DEFAULT_REPO_CONFIG_URL: &str =
    "https://anolisa.oss-cn-hangzhou.aliyuncs.com/anolisa-releases/anolisa/v1/repo.toml";
/// Hard cap on bootstrap downloads; valid repo configs are only a few KiB.
const MAX_REPO_CONFIG_BYTES: u64 = 256 * 1024;
const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const HTTP_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Wire-format schema version of `repo.toml`.
pub const REPO_CONFIG_SCHEMA_VERSION: u32 = 1;

/// Raw-backend artifact file name (publish-contract I2), appended to the
/// rendered artifact directory for index rows that omit `url`.
pub const RAW_ARTIFACT_FILENAME: &str = "{component}-{version}-{os}-{arch}{ext}";

/// Code-owned artifact directory layout under a raw `base_url`.
pub const RAW_ARTIFACT_DIR: &str = "{component}/{version}/{os}/{arch}";

/// Backend names this binary knows how to drive (or will: `rpm`/`npm` are
/// configuration-valid before their executors land so a site can stage
/// config ahead of the rollout).
///
/// `yum` was the first spelling used for the RPM backend in generated
/// `repo.toml` files. Disk configs using that spelling are normalized to
/// `rpm` before validation so callers never need to handle both names.
pub const KNOWN_BACKENDS: &[&str] = &["raw", RPM_BACKEND, "npm"];

const RPM_BACKEND: &str = "rpm";
const LEGACY_RPM_BACKEND: &str = "yum";
const LEGACY_RPM_BACKEND_WARNING: &str = concat!(
    "uses deprecated backend name 'yum'; treating it as 'rpm'. ",
    "Rename default_backend to 'rpm' and [backends.yum] to [backends.rpm]."
);
const LEGACY_RPM_BACKEND_NAME_WARNING: &str =
    "backend name 'yum' is deprecated; treating it as 'rpm'. Use 'rpm' instead.";

/// Errors surfaced while loading, parsing, or resolving `repo.toml`.
#[derive(Debug, thiserror::Error)]
pub enum RepoConfigError {
    /// No config found in any discovery location.
    #[error("repo config not found (searched etc dir, packaged datadir, dev-tree)")]
    NotFound,

    /// Disk read failed (e.g. permission denied).
    #[error("failed to read repo config at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// TOML parse failed.
    #[error("failed to parse repo config at {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    /// Schema version on disk does not match what this binary understands.
    #[error("unsupported repo config schema_version {actual} (expected {expected})")]
    UnsupportedSchema { actual: u32, expected: u32 },

    /// A `[backends.<name>]` key is not in [`KNOWN_BACKENDS`].
    #[error("unknown backend '{name}' in repo config (known: {})", KNOWN_BACKENDS.join(", "))]
    UnknownBackend { name: String },

    /// `default_backend` names a backend with no `[backends.<name>]` table.
    #[error(
        "default_backend '{name}' has no [backends.{name}] table — add one or change default_backend"
    )]
    DefaultBackendNotConfigured { name: String },

    /// The caller selected a backend (via `--backend`) that has no
    /// configuration table.
    #[error("backend '{name}' is not configured — add a [backends.{name}] table to repo.toml")]
    BackendNotConfigured { name: String },

    /// `base_url` violated the scheme/shape rules.
    #[error("backend '{backend}': invalid base_url '{url}': {reason}")]
    InvalidBaseUrl {
        backend: String,
        url: String,
        reason: String,
    },

    /// `base_url` referenced a `$variable` outside the supported set.
    #[error("backend '{backend}': base_url references unknown variable '${name}'")]
    UnknownVariable { backend: String, name: String },

    /// `base_url` referenced a supported variable that has no value on
    /// this host and no `[vars]` override (today only `$releasever`).
    #[error(
        "backend '{backend}': base_url references '${name}' which is not set — set it under [vars]"
    )]
    UnsetVariable { backend: String, name: String },

    /// Raw layout rendering referenced a `{placeholder}` outside the
    /// supported set (`component`, `version`, `os`, `arch`, `libc`, `ext`).
    #[error("backend '{backend}': raw artifact layout references unknown placeholder '{{{name}}}'")]
    UnknownPlaceholder { backend: String, name: String },

    /// Raw layout rendering referenced a supported placeholder the
    /// resolved index row carries no value for (e.g. `{libc}` on a
    /// libc-less row).
    #[error(
        "backend '{backend}': raw artifact layout references '{{{name}}}' but the resolved index entry has no value for it"
    )]
    UnsetPlaceholder { backend: String, name: String },
}

/// Result of loading repo config through the source-access provisioning path.
#[derive(Debug)]
pub(crate) struct RepoConfigLoadResult {
    pub config: RepoConfig,
    pub provisioning: RepoConfigProvisioning,
}

/// Side effect, if any, performed while making repo config available.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RepoConfigProvisioning {
    Existing,
    Downloaded {
        url: String,
        dest: PathBuf,
    },
    FetchedForDryRun {
        url: String,
        dest: PathBuf,
    },
    /// Downloaded and parsed successfully, but writing to disk failed.
    ///
    /// The in-memory config is valid; callers that only need read access may
    /// proceed and warn, while mutating commands should treat this as an error.
    DownloadedPersistFailed {
        url: String,
        dest: PathBuf,
        reason: String,
    },
}

/// Errors raised while provisioning a missing repo config.
#[derive(Debug, thiserror::Error)]
pub(crate) enum RepoConfigProvisionError {
    #[error("failed to load repo config: {0}")]
    Load(#[from] RepoConfigError),

    #[error("failed to fetch repo config from {url}: {reason}")]
    Fetch { url: String, reason: String },

    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("downloaded repo config is invalid: {reason}")]
    InvalidDownloaded { reason: String },
}

/// Parsed `repo.toml`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoConfig {
    pub schema_version: u32,
    /// Backend used when the CLI does not pass `--backend`.
    pub default_backend: String,
    #[serde(default)]
    pub vars: RepoVars,
    #[serde(default)]
    pub backends: BTreeMap<String, BackendConfig>,
    #[serde(skip)]
    legacy_rpm_backend: bool,
    /// Discovery path that supplied this config; absent for in-memory parses.
    #[serde(skip)]
    source_path: Option<PathBuf>,
}

/// `[vars]` overrides for `base_url` substitution. Every field is
/// optional; unset fields fall back to host detection (`$releasever`
/// has no probe yet, so referencing it requires an override here).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoVars {
    pub os: Option<String>,
    pub arch: Option<String>,
    pub releasever: Option<String>,
    pub channel: Option<String>,
}

/// One `[backends.<name>]` table. A single struct covers all backend
/// kinds; fields irrelevant to a kind are simply unused (e.g. `gpgcheck`
/// outside rpm) — the executor for each backend consumes its own subset.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackendConfig {
    /// Directory root the backend resolves against (see module docs).
    pub base_url: String,
    /// Opt-in for plaintext `http://` sources.
    #[serde(default)]
    pub insecure: bool,
    /// rpm: signature verification toggle, handed to dnf.
    #[serde(default)]
    pub gpgcheck: Option<bool>,
    /// npm: default package scope (`@anolis` → `@anolis/<component>`).
    #[serde(default)]
    pub scope: Option<String>,
    /// Accepted but ignored: the raw backend re-fetches the index every run.
    ///
    /// Both knobs were written for an index cache that no CLI path uses;
    /// `deny_unknown_fields` means dropping them would reject configs already
    /// on disk, so they stay parseable and inert. Shipped templates no longer
    /// emit them.
    #[serde(default)]
    #[allow(dead_code)]
    pub cache_ttl_secs: Option<u64>,
    /// Accepted but ignored; see [`BackendConfig::cache_ttl_secs`].
    #[serde(default)]
    #[allow(dead_code)]
    pub offline_fallback: Option<bool>,
    /// Site-local component -> package-name fallback.
    ///
    /// Normal repository mappings belong in the repo-side component index and
    /// take precedence during component resolution. This map is for
    /// deployments that need local names before, or outside, the published
    /// index contract.
    #[serde(default)]
    pub package_map: BTreeMap<String, String>,
}

/// Host-derived values feeding `base_url` substitution. Decoupled from
/// `anolisa_env::EnvFacts` so tests can pin values without probing.
#[derive(Debug, Clone)]
pub struct HostVars {
    pub os: String,
    pub arch: String,
}

impl RepoConfig {
    /// Load repo config for commands that need a configured repository source.
    ///
    /// Uses the discovery chain first and provisions `<etc_dir>/repo.toml` only
    /// when every local source is missing. `dry_run` fetches and validates the
    /// published config without writing it.
    pub(crate) fn load(
        layout: &FsLayout,
        dry_run: bool,
        packaged_data_probe: &packaged::PackagedDataProbe,
    ) -> Result<RepoConfigLoadResult, RepoConfigProvisionError> {
        let url = repo_config_url();
        Self::load_with_sources(
            RepoConfigSources::for_layout(layout, packaged_data_probe),
            dry_run,
            &url,
        )
    }

    fn load_with_sources(
        sources: RepoConfigSources,
        dry_run: bool,
        bootstrap_url: &str,
    ) -> Result<RepoConfigLoadResult, RepoConfigProvisionError> {
        match Self::load_local_with_sources(sources.clone()) {
            Ok(config) => {
                return Ok(RepoConfigLoadResult {
                    config,
                    provisioning: RepoConfigProvisioning::Existing,
                });
            }
            Err(RepoConfigError::NotFound) => {}
            Err(err) => return Err(RepoConfigProvisionError::Load(err)),
        }

        let dest = sources
            .etc
            .clone()
            .ok_or(RepoConfigProvisionError::Load(RepoConfigError::NotFound))?;
        let body = fetch_repo_config_body(bootstrap_url)?;
        let mut config = Self::from_toml_str(&body).map_err(|err| {
            RepoConfigProvisionError::InvalidDownloaded {
                reason: err.to_string(),
            }
        })?;

        if dry_run {
            return Ok(RepoConfigLoadResult {
                config,
                provisioning: RepoConfigProvisioning::FetchedForDryRun {
                    url: bootstrap_url.to_string(),
                    dest,
                },
            });
        }

        match write_repo_config(&dest, &body) {
            Ok(()) => {
                config.source_path = Some(dest.clone());
                Ok(RepoConfigLoadResult {
                    config,
                    provisioning: RepoConfigProvisioning::Downloaded {
                        url: bootstrap_url.to_string(),
                        dest,
                    },
                })
            }
            Err(err) => Ok(RepoConfigLoadResult {
                config,
                provisioning: RepoConfigProvisioning::DownloadedPersistFailed {
                    url: bootstrap_url.to_string(),
                    dest,
                    reason: err.to_string(),
                },
            }),
        }
    }

    /// Local discovery helper used before the provisioning fallback.
    fn load_local_with_sources(sources: RepoConfigSources) -> Result<Self, RepoConfigError> {
        for candidate in [&sources.etc, &sources.packaged, &sources.dev_tree] {
            if let Some(path) = candidate.as_deref()
                && path.is_file()
            {
                let mut config = Self::from_path(path)?;
                config.emit_deprecation_warnings(path);
                config.source_path = Some(path.to_path_buf());
                return Ok(config);
            }
        }
        Err(RepoConfigError::NotFound)
    }

    /// Load from an explicit path. Test-friendly hook.
    pub fn from_path(path: &Path) -> Result<Self, RepoConfigError> {
        let body = std::fs::read_to_string(path).map_err(|source| RepoConfigError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse_with_path(&body, path)
    }

    /// Parse a TOML body and run structural validation. Used by unit
    /// tests today; kept public mirroring `ExecutionPolicy::from_toml_str`
    /// so integration harnesses can exercise the parser without disk.
    #[allow(dead_code)]
    pub fn from_toml_str(s: &str) -> Result<Self, RepoConfigError> {
        Self::parse_with_path(s, Path::new("<memory>"))
    }

    /// Path selected by local config discovery, when this config came from disk.
    pub(crate) fn source_path(&self) -> Option<&Path> {
        self.source_path.as_deref()
    }

    fn parse_with_path(s: &str, path: &Path) -> Result<Self, RepoConfigError> {
        let mut parsed: RepoConfig =
            toml::from_str(s).map_err(|source| RepoConfigError::Parse {
                path: path.to_path_buf(),
                source,
            })?;
        parsed.normalize_legacy_backend_names();
        parsed.validate()?;
        Ok(parsed)
    }

    pub(crate) fn canonical_backend_name(name: &str) -> &str {
        if name == LEGACY_RPM_BACKEND {
            RPM_BACKEND
        } else {
            name
        }
    }

    /// Canonical spelling of `name` when it is a known backend, otherwise the
    /// same unknown-backend error [`Self::select_backend`] reports. Name
    /// validation only — whether the backend is configured in repo.toml is a
    /// separate question some callers (a `--repo` override) do not ask.
    pub(crate) fn known_backend_name(name: &str) -> Result<&'static str, RepoConfigError> {
        let canonical = Self::canonical_backend_name(name);
        KNOWN_BACKENDS
            .iter()
            .find(|known| **known == canonical)
            .copied()
            .ok_or_else(|| RepoConfigError::UnknownBackend {
                name: canonical.to_string(),
            })
    }

    pub(crate) fn backend_name_deprecation_warning(name: &str) -> Option<&'static str> {
        (name == LEGACY_RPM_BACKEND).then_some(LEGACY_RPM_BACKEND_NAME_WARNING)
    }

    fn normalize_legacy_backend_names(&mut self) {
        let default_was_legacy = self.default_backend == LEGACY_RPM_BACKEND;
        let legacy_backend = self.backends.remove(LEGACY_RPM_BACKEND);
        let backend_was_legacy = legacy_backend.is_some();

        if self.default_backend == LEGACY_RPM_BACKEND {
            self.default_backend = RPM_BACKEND.to_string();
        }
        if let Some(legacy) = legacy_backend {
            self.backends
                .entry(RPM_BACKEND.to_string())
                .or_insert(legacy);
        }
        self.legacy_rpm_backend = default_was_legacy || backend_was_legacy;
    }

    pub(crate) fn deprecation_warning(&self) -> Option<&'static str> {
        self.legacy_rpm_backend
            .then_some(LEGACY_RPM_BACKEND_WARNING)
    }

    fn emit_deprecation_warnings(&self, path: &Path) {
        if let Some(warning) = self.deprecation_warning() {
            // Routed through `suspend_output` so this persistent line cannot
            // interleave with a live activity spinner (issue #1452).
            crate::progress::suspend_output(|| {
                eprintln!("warning: repo config at {} {warning}", path.display());
            });
        }
    }

    /// Structural validation run on every successful parse:
    /// schema version, backend-name allow-list, default-backend
    /// presence, and per-backend base_url scheme rules. Variable
    /// substitution is deliberately NOT validated here — it needs host
    /// values and only the selected backend's URL is ever resolved.
    fn validate(&self) -> Result<(), RepoConfigError> {
        if self.schema_version != REPO_CONFIG_SCHEMA_VERSION {
            return Err(RepoConfigError::UnsupportedSchema {
                actual: self.schema_version,
                expected: REPO_CONFIG_SCHEMA_VERSION,
            });
        }
        for name in self.backends.keys() {
            if !KNOWN_BACKENDS.contains(&name.as_str()) {
                return Err(RepoConfigError::UnknownBackend { name: name.clone() });
            }
        }
        if !KNOWN_BACKENDS.contains(&self.default_backend.as_str()) {
            return Err(RepoConfigError::UnknownBackend {
                name: self.default_backend.clone(),
            });
        }
        if !self.backends.contains_key(&self.default_backend) {
            return Err(RepoConfigError::DefaultBackendNotConfigured {
                name: self.default_backend.clone(),
            });
        }
        for (name, backend) in &self.backends {
            validate_base_url(name, &backend.base_url, backend.insecure)?;
        }
        Ok(())
    }

    /// Resolve the backend to use: `cli_override` (`--backend`) when
    /// given, otherwise `default_backend`. Returns the backend name and
    /// its config table.
    pub fn select_backend(
        &self,
        cli_override: Option<&str>,
    ) -> Result<(&str, &BackendConfig), RepoConfigError> {
        let name = Self::known_backend_name(cli_override.unwrap_or(&self.default_backend))?;
        match self.backends.get_key_value(name) {
            Some((key, cfg)) => Ok((key.as_str(), cfg)),
            None => Err(RepoConfigError::BackendNotConfigured {
                name: name.to_string(),
            }),
        }
    }

    /// Substitute `$variables` into `base_url` for `backend_name` and
    /// return the normalized URL (no trailing slash). `host` supplies
    /// detected values; `[vars]` overrides win over detection.
    pub fn resolved_base_url(
        &self,
        backend_name: &str,
        backend: &BackendConfig,
        host: &HostVars,
    ) -> Result<String, RepoConfigError> {
        let arch = self.vars.arch.clone().unwrap_or_else(|| host.arch.clone());
        let values: BTreeMap<&str, Option<String>> = BTreeMap::from([
            ("os", Some(self.vars.os.clone().unwrap_or(host.os.clone()))),
            ("arch", Some(arch.clone())),
            // $basearch is a dnf/rpm-style alias of $arch; kept so existing
            // dnf baseurls can be pasted verbatim.
            ("basearch", Some(arch)),
            // No host probe for the distro release yet — referencing
            // $releasever without a [vars] override is an error.
            ("releasever", self.vars.releasever.clone()),
            (
                "channel",
                Some(self.vars.channel.clone().unwrap_or("stable".to_string())),
            ),
        ]);
        let substituted = substitute_vars(backend_name, &backend.base_url, &values)?;
        Ok(substituted.trim_end_matches('/').to_string())
    }

    /// Resolve the backend-native package name for `component`.
    /// Chain: CLI `--package` > backend `package_map` > npm `scope`
    /// prefix > the component name itself. (The component-manifest
    /// `packaging` layer slots in between map and scope once the
    /// manifest schema grows it.)
    pub fn package_name(
        &self,
        backend: &BackendConfig,
        component: &str,
        cli_override: Option<&str>,
    ) -> String {
        if let Some(name) = cli_override {
            return name.to_string();
        }
        if let Some(mapped) = backend.package_map.get(component) {
            return mapped.clone();
        }
        if let Some(scope) = backend.scope.as_deref() {
            return format!("{scope}/{component}");
        }
        component.to_string()
    }
}

/// Validate and normalize a one-off `--repo <URL>` override. Same shape
/// rules as configured base_urls, except plaintext `http://` is allowed:
/// typing the flag is itself the explicit opt-in that `insecure = true`
/// provides in the file. Returns the URL with any trailing slash trimmed.
pub fn normalize_override_url(url: &str) -> Result<String, RepoConfigError> {
    validate_base_url("<cli-override>", url, true)?;
    Ok(url.trim_end_matches('/').to_string())
}

/// Enforce the base_url shape rules (see module docs). Runs on the raw
/// string before substitution — the scheme is always literal.
fn validate_base_url(backend: &str, url: &str, insecure: bool) -> Result<(), RepoConfigError> {
    let invalid = |reason: &str| RepoConfigError::InvalidBaseUrl {
        backend: backend.to_string(),
        url: url.to_string(),
        reason: reason.to_string(),
    };
    let Some((scheme, rest)) = url.split_once("://") else {
        return Err(invalid("missing scheme separator '://'"));
    };
    match scheme {
        "file" | "https" => {}
        "http" => {
            if !insecure {
                return Err(invalid(
                    "plaintext http requires `insecure = true` on the backend entry",
                ));
            }
        }
        other => {
            return Err(invalid(&format!(
                "unsupported scheme '{other}' (supported: file, https, http with insecure = true)"
            )));
        }
    }
    if rest.is_empty() {
        return Err(invalid("empty authority/path"));
    }
    if url.contains('?') || url.contains('#') {
        return Err(invalid("query strings and fragments are not allowed"));
    }
    Ok(())
}

/// Replace every `$name` token in `input` from `values`. `name` is the
/// longest run of `[a-z_]` after `$`. A key missing from `values` is
/// [`RepoConfigError::UnknownVariable`]; a key present with `None` is
/// [`RepoConfigError::UnsetVariable`].
fn substitute_vars(
    backend: &str,
    input: &str,
    values: &BTreeMap<&str, Option<String>>,
) -> Result<String, RepoConfigError> {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(idx) = rest.find('$') {
        out.push_str(&rest[..idx]);
        let after = &rest[idx + 1..];
        let name_len = after
            .find(|c: char| !(c.is_ascii_lowercase() || c == '_'))
            .unwrap_or(after.len());
        if name_len == 0 {
            return Err(RepoConfigError::UnknownVariable {
                backend: backend.to_string(),
                name: "$".to_string(),
            });
        }
        let name = &after[..name_len];
        match values.get(name) {
            Some(Some(value)) => out.push_str(value),
            Some(None) => {
                return Err(RepoConfigError::UnsetVariable {
                    backend: backend.to_string(),
                    name: name.to_string(),
                });
            }
            None => {
                return Err(RepoConfigError::UnknownVariable {
                    backend: backend.to_string(),
                    name: name.to_string(),
                });
            }
        }
        rest = &after[name_len..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Raw distribution root, i.e. the directory containing `index.toml`.
///
/// New configs should point `base_url` directly at that root, usually a
/// `.../v1/` URL. Two legacy forms are accepted during migration:
/// `.../{component}/{version}/{os}/{arch}/` is reduced to the static prefix
/// before `{component}`, and parent roots without a trailing `/v1` get `/v1`
/// appended.
fn raw_root(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    if let Some(brace) = trimmed.find('{') {
        let cut = trimmed[..brace].rfind('/').unwrap_or(0);
        return trimmed[..cut].trim_end_matches('/').to_string();
    }
    if trimmed.rsplit('/').next() == Some("v1") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/v1")
    }
}

/// Index location for a raw backend. `base_url` is the raw distribution
/// root containing `index.toml`.
pub fn raw_index_url(base_url: &str) -> String {
    format!("{}/index.toml", raw_root(base_url))
}

/// Generation-2 index location under the same raw distribution root.
///
/// `index-v2.toml` is a complete index (not a delta): clients that
/// understand it read only this file, falling back to `index.toml` when the
/// repository has not published one. Split-index bootstrap: entries whose
/// embedded contract depends on ≥ 0.2.17 semantics (`render`,
/// `min_anolisa_version` enforcement, backend adapter roots) are published
/// only here, so released pre-0.2.17 CLIs — which parse the shared index
/// atomically and would fail it wholesale on any entry shape they cannot
/// represent — never observe them: they keep installing unrelated
/// components from `index.toml` and resolve gated components as not-found
/// (fail closed).
pub fn raw_index_v2_url(base_url: &str) -> String {
    format!("{}/index-v2.toml", raw_root(base_url))
}

/// Generation-1 component identity index location under the raw repository root.
///
/// Released clients continue reading this path, so publishers must preserve its
/// schema v1 contents when publishing later component-index generations.
#[cfg(test)]
pub fn component_index_url(base_url: &str) -> String {
    format!("{}/components.toml", raw_root(base_url))
}

/// Generation-2 component identity index location under the raw repository root.
///
/// Keeping v2 at a distinct path prevents its incompatible target rows from
/// breaking released clients that parse `components.toml` atomically.
pub fn component_index_v2_url(base_url: &str) -> String {
    format!("{}/components-v2.toml", raw_root(base_url))
}

/// Root that repo-relative index-row `url`s join onto. Same prefix the
/// index itself lives under, so a mirrored tree stays self-contained.
pub fn raw_relative_root(base_url: &str) -> String {
    raw_root(base_url)
}

/// Artifact URL for an index row that omits `url`: append the code-owned
/// artifact directory layout ([`RAW_ARTIFACT_DIR`]) and conventional file
/// name ([`RAW_ARTIFACT_FILENAME`]) under the raw distribution root.
pub fn raw_artifact_url(
    backend: &str,
    base_url: &str,
    values: &BTreeMap<&str, Option<String>>,
) -> Result<String, RepoConfigError> {
    let root = raw_root(base_url);
    let dir_template = format!("{root}/{RAW_ARTIFACT_DIR}");
    let dir = render_placeholders(backend, &dir_template, values)?;
    let file = render_placeholders(backend, RAW_ARTIFACT_FILENAME, values)?;
    Ok(format!("{}/{file}", dir.trim_end_matches('/')))
}

/// Replace every `{name}` placeholder in `template` from `values`. A name
/// missing from `values` is [`RepoConfigError::UnknownPlaceholder`]; a
/// name present with `None` (e.g. `{libc}` for a libc-less index row) is
/// [`RepoConfigError::UnsetPlaceholder`]. Both are hard errors for the
/// same reason `$var` typos are: a half-substituted URL is the hardest
/// failure to diagnose downstream.
fn render_placeholders(
    backend: &str,
    template: &str,
    values: &BTreeMap<&str, Option<String>>,
) -> Result<String, RepoConfigError> {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(idx) = rest.find('{') {
        out.push_str(&rest[..idx]);
        let after = &rest[idx + 1..];
        let Some(close) = after.find('}') else {
            return Err(RepoConfigError::UnknownPlaceholder {
                backend: backend.to_string(),
                name: after.to_string(),
            });
        };
        let name = &after[..close];
        match values.get(name) {
            Some(Some(value)) => out.push_str(value),
            Some(None) => {
                return Err(RepoConfigError::UnsetPlaceholder {
                    backend: backend.to_string(),
                    name: name.to_string(),
                });
            }
            None => {
                return Err(RepoConfigError::UnknownPlaceholder {
                    backend: backend.to_string(),
                    name: name.to_string(),
                });
            }
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Discovery candidates for local repo config discovery.
#[derive(Clone)]
pub(crate) struct RepoConfigSources {
    /// User/site config under the active layout's etc dir.
    pub etc: Option<PathBuf>,
    /// Packaged copy under the datadir.
    pub packaged: Option<PathBuf>,
    /// Dev-tree copy resolved from `CARGO_MANIFEST_DIR`.
    pub dev_tree: Option<PathBuf>,
}

impl RepoConfigSources {
    fn for_layout(layout: &FsLayout, packaged_data_probe: &packaged::PackagedDataProbe) -> Self {
        let packaged_root = packaged::packaged_datadir_root(layout, packaged_data_probe)
            .unwrap_or_else(|| layout.datadir.clone());
        Self {
            etc: Some(layout.etc_dir.join(REPO_FILE)),
            packaged: Some(packaged_root.join(REPO_SUBDIR).join(REPO_FILE)),
            dev_tree: Some(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("..")
                    .join("..")
                    .join(REPO_SUBDIR)
                    .join(REPO_FILE),
            ),
        }
    }
}

fn repo_config_url() -> String {
    std::env::var("ANOLISA_REPO_CONFIG_URL").unwrap_or_else(|_| DEFAULT_REPO_CONFIG_URL.to_string())
}

fn fetch_repo_config_body(url: &str) -> Result<String, RepoConfigProvisionError> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(HTTP_CONNECT_TIMEOUT)
        .timeout_read(HTTP_READ_TIMEOUT)
        .build();
    let response = agent
        .get(url)
        .call()
        .map_err(|err| RepoConfigProvisionError::Fetch {
            url: url.to_string(),
            reason: err.to_string(),
        })?;
    let mut body = String::new();
    response
        .into_reader()
        .take(MAX_REPO_CONFIG_BYTES + 1)
        .read_to_string(&mut body)
        .map_err(|err| RepoConfigProvisionError::Fetch {
            url: url.to_string(),
            reason: err.to_string(),
        })?;
    if body.len() as u64 > MAX_REPO_CONFIG_BYTES {
        return Err(RepoConfigProvisionError::Fetch {
            url: url.to_string(),
            reason: format!(
                "download exceeded size limit: {} bytes received, limit is {} bytes",
                body.len(),
                MAX_REPO_CONFIG_BYTES
            ),
        });
    }
    Ok(body)
}

fn write_repo_config(dest: &Path, body: &str) -> Result<(), RepoConfigProvisionError> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|source| RepoConfigProvisionError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let tmp = dest.with_extension("toml.tmp");
    std::fs::write(&tmp, body).map_err(|source| RepoConfigProvisionError::Io {
        path: tmp.clone(),
        source,
    })?;
    std::fs::rename(&tmp, dest).map_err(|source| {
        let _ = std::fs::remove_file(&tmp);
        RepoConfigProvisionError::Io {
            path: dest.to_path_buf(),
            source,
        }
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host() -> HostVars {
        HostVars {
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
        }
    }

    fn serve_once(body: String) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                use std::io::{Read, Write};
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
            }
        });
        format!("http://{addr}/repo.toml")
    }

    fn missing_sources(dest: PathBuf) -> RepoConfigSources {
        RepoConfigSources {
            etc: Some(dest),
            packaged: None,
            dev_tree: None,
        }
    }

    /// The packaged template must parse, default to raw, and resolve a
    /// clean base_url.
    #[test]
    fn packaged_template_parses_and_defaults_to_raw() {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let repo_path = manifest_dir.join("../../templates/repo.toml");
        let content = std::fs::read_to_string(&repo_path).expect("read packaged template");
        let cfg = RepoConfig::from_toml_str(&content).expect("packaged template");
        assert_eq!(cfg.schema_version, REPO_CONFIG_SCHEMA_VERSION);
        assert_eq!(cfg.default_backend, "raw");
        let (name, backend) = cfg.select_backend(None).expect("default backend");
        assert_eq!(name, "raw");
        let url = cfg
            .resolved_base_url(name, backend, &host())
            .expect("resolve");
        assert!(url.starts_with("https://"), "got: {url}");
        assert!(!url.ends_with('/'), "trailing slash must be trimmed: {url}");
    }

    #[test]
    fn repository_manifest_parses() {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let repo_path = manifest_dir.join("../../manifests/repo.toml");
        let content = std::fs::read_to_string(&repo_path).expect("read repo manifest");
        let cfg = RepoConfig::from_toml_str(&content).expect("repo manifest");
        cfg.select_backend(Some("rpm")).expect("rpm backend");
    }

    /// Neither shipped file may advertise the inert raw cache knobs.
    #[test]
    fn shipped_repo_configs_omit_raw_cache_knobs() {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        for relative in ["../../templates/repo.toml", "../../manifests/repo.toml"] {
            let path = manifest_dir.join(relative);
            let content = std::fs::read_to_string(&path).expect("read shipped repo config");
            let cfg = RepoConfig::from_toml_str(&content).expect("shipped repo config");
            let raw = cfg.backends.get("raw").expect("raw backend");
            assert_eq!(raw.cache_ttl_secs, None, "{relative}");
            assert_eq!(raw.offline_fallback, None, "{relative}");
        }
    }

    /// Configs written by older releases still carry the knobs and must load.
    #[test]
    fn legacy_raw_cache_knobs_still_parse() {
        let cfg = RepoConfig::from_toml_str(
            r#"schema_version = 1
default_backend = "raw"
[backends.raw]
base_url = "https://repo.test/v1/"
cache_ttl_secs = 3600
offline_fallback = true
"#,
        )
        .expect("legacy repo config must load");

        let (name, backend) = cfg.select_backend(None).expect("default backend");
        assert_eq!(name, "raw");
        assert_eq!(backend.cache_ttl_secs, Some(3600));
        assert_eq!(backend.offline_fallback, Some(true));
        assert_eq!(
            cfg.resolved_base_url(name, backend, &host()).expect("url"),
            "https://repo.test/v1"
        );
    }

    #[test]
    fn missing_disk_sources_returns_not_found() {
        let tmp = tempfile::tempdir().expect("tmp");
        let sources = RepoConfigSources {
            etc: Some(tmp.path().join("nope.toml")),
            packaged: Some(tmp.path().join("nope2.toml")),
            dev_tree: Some(tmp.path().join("nope3.toml")),
        };
        let err = RepoConfig::load_local_with_sources(sources).expect_err("no fallback");
        assert!(matches!(err, RepoConfigError::NotFound));
    }

    #[test]
    fn load_uses_existing_config_without_bootstrap() {
        let tmp = tempfile::tempdir().expect("tmp");
        let dest = tmp.path().join("etc/repo.toml");
        std::fs::create_dir_all(dest.parent().expect("parent")).expect("mkdir");
        std::fs::write(
            &dest,
            r#"schema_version = 1
default_backend = "raw"
[backends.raw]
base_url = "file:///srv/existing"
"#,
        )
        .expect("write");
        let result = RepoConfig::load_with_sources(missing_sources(dest), false, "bad://url")
            .expect("load existing");

        assert!(matches!(
            result.provisioning,
            RepoConfigProvisioning::Existing
        ));
        assert_eq!(
            result.config.backends["raw"].base_url,
            "file:///srv/existing"
        );
    }

    #[test]
    fn load_downloads_missing_config() {
        let body = "schema_version = 1\ndefault_backend = \"raw\"\n\n[backends.raw]\nbase_url = \"https://example.com/v1/\"\n";
        let url = serve_once(body.to_string());
        let tmp = tempfile::tempdir().expect("tmp");
        let dest = tmp.path().join("etc/repo.toml");

        let result = RepoConfig::load_with_sources(missing_sources(dest.clone()), false, &url)
            .expect("download");

        assert!(matches!(
            result.provisioning,
            RepoConfigProvisioning::Downloaded { .. }
        ));
        assert_eq!(std::fs::read_to_string(&dest).expect("read dest"), body);
        assert!(!dest.with_extension("toml.tmp").exists());
        assert_eq!(result.config.default_backend, "raw");
    }

    #[test]
    fn load_dry_run_fetches_without_writing() {
        let body = "schema_version = 1\ndefault_backend = \"raw\"\n\n[backends.raw]\nbase_url = \"https://example.com/v1/\"\n";
        let url = serve_once(body.to_string());
        let tmp = tempfile::tempdir().expect("tmp");
        let dest = tmp.path().join("etc/repo.toml");

        let result = RepoConfig::load_with_sources(missing_sources(dest.clone()), true, &url)
            .expect("dry-run fetch");

        assert!(matches!(
            result.provisioning,
            RepoConfigProvisioning::FetchedForDryRun { .. }
        ));
        assert!(!dest.exists(), "dry-run must not write repo config");
        assert_eq!(result.config.default_backend, "raw");
    }

    #[test]
    fn load_refuses_invalid_download() {
        let url = serve_once("this is not a repo config".to_string());
        let tmp = tempfile::tempdir().expect("tmp");
        let dest = tmp.path().join("etc/repo.toml");

        let err = RepoConfig::load_with_sources(missing_sources(dest.clone()), false, &url)
            .expect_err("must refuse");

        assert!(matches!(
            err,
            RepoConfigProvisionError::InvalidDownloaded { .. }
        ));
        assert!(!dest.exists(), "invalid config must not land on disk");
    }

    /// Etc-dir config wins over every other source — that is the
    /// user-editable override point.
    #[test]
    fn etc_config_wins_over_other_disk_sources() {
        let tmp = tempfile::tempdir().expect("tmp");
        let path = tmp.path().join("repo.toml");
        std::fs::write(
            &path,
            r#"schema_version = 1
default_backend = "raw"
[backends.raw]
base_url = "file:///srv/local-repo"
"#,
        )
        .expect("write");
        let sources = RepoConfigSources {
            etc: Some(path),
            packaged: None,
            dev_tree: None,
        };
        let cfg = RepoConfig::load_local_with_sources(sources).expect("load");
        assert_eq!(cfg.backends["raw"].base_url, "file:///srv/local-repo");
    }

    #[test]
    fn unknown_backend_table_is_rejected() {
        let err = RepoConfig::from_toml_str(
            r#"schema_version = 1
default_backend = "raw"
[backends.raw]
base_url = "file:///srv/repo"
[backends.pip]
base_url = "https://pypi.org"
"#,
        )
        .expect_err("must reject");
        assert!(matches!(err, RepoConfigError::UnknownBackend { name } if name == "pip"));
    }

    #[test]
    fn default_backend_without_table_is_rejected() {
        // `rpm` is a known backend but has no `[backends.rpm]` table here, so
        // the default-backend presence check (not the allow-list) must fire.
        let err = RepoConfig::from_toml_str(
            r#"schema_version = 1
default_backend = "rpm"
[backends.raw]
base_url = "file:///srv/repo"
"#,
        )
        .expect_err("must reject");
        assert!(matches!(
            err,
            RepoConfigError::DefaultBackendNotConfigured { name } if name == "rpm"
        ));
    }

    #[test]
    fn legacy_yum_backend_table_is_migrated_to_rpm() {
        let cfg = RepoConfig::from_toml_str(
            r#"schema_version = 1
default_backend = "yum"
[backends.yum]
base_url = "https://mirrors.openanolis.cn/anolis/23/agents/$basearch/"
[backends.yum.package_map]
agentsight = "anolis-agentsight"
"#,
        )
        .expect("legacy yum spelling must load");

        assert_eq!(cfg.default_backend, "rpm");
        assert!(
            cfg.deprecation_warning()
                .is_some_and(|warning| warning.contains("deprecated backend name 'yum'"))
        );
        assert!(!cfg.backends.contains_key("yum"));
        assert!(cfg.backends.contains_key("rpm"));
        let (name, backend) = cfg.select_backend(None).expect("default backend");
        assert_eq!(name, "rpm");
        assert_eq!(
            cfg.resolved_base_url(name, backend, &host()).expect("url"),
            "https://mirrors.openanolis.cn/anolis/23/agents/x86_64"
        );
        assert_eq!(
            cfg.package_name(backend, "agentsight", None),
            "anolis-agentsight"
        );
    }

    #[test]
    fn http_without_insecure_is_rejected_and_with_insecure_accepted() {
        let body = |insecure: &str| {
            format!(
                r#"schema_version = 1
default_backend = "raw"
[backends.raw]
base_url = "http://10.0.0.8/anolisa"
{insecure}
"#
            )
        };
        let err = RepoConfig::from_toml_str(&body("")).expect_err("plain http must be rejected");
        assert!(
            matches!(err, RepoConfigError::InvalidBaseUrl { .. }),
            "got: {err:?}"
        );
        RepoConfig::from_toml_str(&body("insecure = true")).expect("insecure http must load");
    }

    #[test]
    fn query_string_in_base_url_is_rejected() {
        let err = RepoConfig::from_toml_str(
            r#"schema_version = 1
default_backend = "raw"
[backends.raw]
base_url = "https://example.com/repo?token=x"
"#,
        )
        .expect_err("must reject");
        assert!(matches!(err, RepoConfigError::InvalidBaseUrl { .. }));
    }

    #[test]
    fn schema_version_mismatch_is_rejected() {
        let err = RepoConfig::from_toml_str(
            r#"schema_version = 999
default_backend = "raw"
[backends.raw]
base_url = "file:///srv/repo"
"#,
        )
        .expect_err("must reject");
        assert!(matches!(
            err,
            RepoConfigError::UnsupportedSchema { actual: 999, .. }
        ));
    }

    fn rpm_cfg(vars: &str) -> RepoConfig {
        RepoConfig::from_toml_str(&format!(
            r#"schema_version = 1
default_backend = "rpm"
{vars}
[backends.rpm]
base_url = "https://mirrors.openanolis.cn/anolis/$releasever/agents/$basearch/"
[backends.rpm.package_map]
agentsight = "anolis-agentsight"
"#
        ))
        .expect("parse")
    }

    #[test]
    fn variable_substitution_uses_vars_overrides_and_detection() {
        let cfg = rpm_cfg("[vars]\nreleasever = \"23\"");
        let (name, backend) = cfg.select_backend(None).expect("rpm");
        let url = cfg.resolved_base_url(name, backend, &host()).expect("url");
        // $releasever from [vars], $basearch from host detection,
        // trailing slash trimmed.
        assert_eq!(url, "https://mirrors.openanolis.cn/anolis/23/agents/x86_64");
    }

    #[test]
    fn unset_releasever_is_a_hard_error() {
        let cfg = rpm_cfg("");
        let (name, backend) = cfg.select_backend(None).expect("rpm");
        let err = cfg
            .resolved_base_url(name, backend, &host())
            .expect_err("must reject");
        assert!(matches!(
            err,
            RepoConfigError::UnsetVariable { name, .. } if name == "releasever"
        ));
    }

    #[test]
    fn unknown_variable_is_a_hard_error() {
        let cfg = RepoConfig::from_toml_str(
            r#"schema_version = 1
default_backend = "raw"
[backends.raw]
base_url = "https://example.com/$typo_var/repo"
"#,
        )
        .expect("scheme-level validation passes");
        let (name, backend) = cfg.select_backend(None).expect("raw");
        let err = cfg
            .resolved_base_url(name, backend, &host())
            .expect_err("must reject");
        assert!(matches!(
            err,
            RepoConfigError::UnknownVariable { name, .. } if name == "typo_var"
        ));
    }

    #[test]
    fn select_backend_cli_override_and_unconfigured_error() {
        let cfg = rpm_cfg("[vars]\nreleasever = \"23\"");
        let (name, _) = cfg.select_backend(Some("rpm")).expect("explicit rpm");
        assert_eq!(name, "rpm");
        let (name, _) = cfg
            .select_backend(Some("yum"))
            .expect("legacy explicit yum");
        assert_eq!(name, "rpm");
        assert!(
            RepoConfig::backend_name_deprecation_warning("yum")
                .is_some_and(|warning| warning.contains("deprecated"))
        );
        let err = cfg
            .select_backend(Some("npm"))
            .expect_err("npm not configured");
        assert!(matches!(
            err,
            RepoConfigError::BackendNotConfigured { name } if name == "npm"
        ));
        let err = cfg.select_backend(Some("pip")).expect_err("pip unknown");
        assert!(matches!(err, RepoConfigError::UnknownBackend { name } if name == "pip"));
    }

    #[test]
    fn known_backend_name_validates_without_requiring_configuration() {
        assert_eq!(RepoConfig::known_backend_name("raw").expect("raw"), "raw");
        assert_eq!(RepoConfig::known_backend_name("yum").expect("yum"), "rpm");
        assert_eq!(RepoConfig::known_backend_name("npm").expect("npm"), "npm");
        let err = RepoConfig::known_backend_name("pip").expect_err("pip unknown");
        assert!(matches!(err, RepoConfigError::UnknownBackend { name } if name == "pip"));
    }

    fn artifact_values(libc: Option<&str>) -> BTreeMap<&'static str, Option<String>> {
        BTreeMap::from([
            ("component", Some("tokenless".to_string())),
            ("version", Some("0.5.0".to_string())),
            ("os", Some("linux".to_string())),
            ("arch", Some("x86_64".to_string())),
            ("libc", libc.map(str::to_string)),
            ("ext", Some(".tar.gz".to_string())),
        ])
    }

    /// A raw `base_url` points at the distribution root that contains
    /// `index.toml`; the artifact directory layout is code-owned.
    #[test]
    fn raw_v1_root_derives_index_and_artifact_urls() {
        let base = "https://mirror.example.com/anolisa-releases/anolisa/v1/";
        assert_eq!(
            raw_index_url(base),
            "https://mirror.example.com/anolisa-releases/anolisa/v1/index.toml"
        );
        assert_eq!(
            raw_relative_root(base),
            "https://mirror.example.com/anolisa-releases/anolisa/v1"
        );
        let url = raw_artifact_url("raw", base, &artifact_values(None)).expect("render");
        assert_eq!(
            url,
            "https://mirror.example.com/anolisa-releases/anolisa/v1/tokenless/0.5.0/linux/x86_64/tokenless-0.5.0-linux-x86_64.tar.gz"
        );
    }

    #[test]
    fn component_index_urls_are_generation_specific() {
        assert_eq!(
            component_index_url("file:///srv/repo"),
            "file:///srv/repo/v1/components.toml"
        );
        assert_eq!(
            component_index_v2_url("file:///srv/repo"),
            "file:///srv/repo/v1/components-v2.toml"
        );
    }

    /// A legacy template-form `base_url` is reduced to its static v1 root.
    #[test]
    fn legacy_template_base_url_uses_static_prefix() {
        let base = "https://mirror.example.com/anolisa-releases/anolisa/v1/{component}/{version}/{os}/{arch}/";
        assert_eq!(
            raw_index_url(base),
            "https://mirror.example.com/anolisa-releases/anolisa/v1/index.toml"
        );
        assert_eq!(
            raw_relative_root(base),
            "https://mirror.example.com/anolisa-releases/anolisa/v1"
        );
        let url = raw_artifact_url("raw", base, &artifact_values(None)).expect("render");
        assert_eq!(
            url,
            "https://mirror.example.com/anolisa-releases/anolisa/v1/tokenless/0.5.0/linux/x86_64/tokenless-0.5.0-linux-x86_64.tar.gz"
        );
    }

    /// A parent-root `base_url` keeps the legacy convention: `/v1/` is
    /// appended for the index and code-owned artifact layout.
    #[test]
    fn parent_base_url_falls_back_to_v1_layout() {
        let base = "file:///srv/repo";
        assert_eq!(raw_index_url(base), "file:///srv/repo/v1/index.toml");
        assert_eq!(raw_relative_root(base), "file:///srv/repo/v1");
        let url = raw_artifact_url("raw", base, &artifact_values(None)).expect("render");
        assert_eq!(
            url,
            "file:///srv/repo/v1/tokenless/0.5.0/linux/x86_64/tokenless-0.5.0-linux-x86_64.tar.gz"
        );
    }

    /// Unknown placeholders are hard errors, and `{libc}` is valid only
    /// when the resolved row carries a libc selector.
    #[test]
    fn render_placeholders_errors_unknown_name_and_unset_libc() {
        let err = render_placeholders("raw", "{typo}", &artifact_values(None))
            .expect_err("must reject unknown placeholder");
        assert!(matches!(
            err,
            RepoConfigError::UnknownPlaceholder { name, .. } if name == "typo"
        ));

        let rendered =
            render_placeholders("raw", "{libc}", &artifact_values(Some("musl"))).expect("render");
        assert_eq!(rendered, "musl");
        let err = render_placeholders("raw", "{libc}", &artifact_values(None))
            .expect_err("must reject unset placeholder");
        assert!(matches!(
            err,
            RepoConfigError::UnsetPlaceholder { name, .. } if name == "libc"
        ));
    }

    #[test]
    fn package_name_chain_cli_then_map_then_scope_then_component() {
        let cfg = rpm_cfg("[vars]\nreleasever = \"23\"");
        let (_, rpm) = cfg.select_backend(None).expect("rpm");
        // CLI override wins over the map.
        assert_eq!(
            cfg.package_name(rpm, "agentsight", Some("agentsight-0917test")),
            "agentsight-0917test"
        );
        // package_map applies.
        assert_eq!(
            cfg.package_name(rpm, "agentsight", None),
            "anolis-agentsight"
        );
        // Fallback is the component name itself.
        assert_eq!(cfg.package_name(rpm, "tokenless", None), "tokenless");

        // npm scope prefixes the default name.
        let npm_cfg = RepoConfig::from_toml_str(
            r#"schema_version = 1
default_backend = "npm"
[backends.npm]
base_url = "https://registry.npmjs.org"
scope = "@anolis"
"#,
        )
        .expect("parse");
        let (_, npm) = npm_cfg.select_backend(None).expect("npm");
        assert_eq!(
            npm_cfg.package_name(npm, "tokenless", None),
            "@anolis/tokenless"
        );
    }

    /// When all local sources are missing and the dest directory is not writable,
    /// `load_with_sources` still returns the valid in-memory config with a
    /// `DownloadedPersistFailed` provisioning status.
    #[test]
    fn load_returns_config_when_persist_fails() {
        let body = "schema_version = 1\ndefault_backend = \"raw\"\n\n[backends.raw]\nbase_url = \"https://example.com/v1/\"\n";
        let url = serve_once(body.to_string());
        // Point dest to a path that cannot be created (parent is a file, not a dir).
        let tmp = tempfile::tempdir().expect("tmp");
        let blocker = tmp.path().join("blocker");
        std::fs::write(&blocker, "I am a file").expect("create blocker file");
        let dest = blocker.join("repo.toml"); // parent is a file → write will fail

        let result = RepoConfig::load_with_sources(missing_sources(dest.clone()), false, &url)
            .expect("must succeed with in-memory config");

        assert!(
            matches!(
                &result.provisioning,
                RepoConfigProvisioning::DownloadedPersistFailed { reason, .. } if !reason.is_empty()
            ),
            "expected DownloadedPersistFailed, got {:?}",
            result.provisioning
        );
        // The config is valid and usable despite the write failure.
        assert_eq!(result.config.default_backend, "raw");
        assert_eq!(
            result.config.backends["raw"].base_url,
            "https://example.com/v1/"
        );
    }

    /// `DownloadedPersistFailed` carries the correct url and dest metadata.
    #[test]
    fn persist_failed_carries_metadata() {
        let body = "schema_version = 1\ndefault_backend = \"raw\"\n\n[backends.raw]\nbase_url = \"file:///srv/repo\"\n";
        let url = serve_once(body.to_string());
        let tmp = tempfile::tempdir().expect("tmp");
        let blocker = tmp.path().join("blocker");
        std::fs::write(&blocker, "I am a file").expect("create blocker");
        let dest = blocker.join("repo.toml");

        let result = RepoConfig::load_with_sources(missing_sources(dest.clone()), false, &url)
            .expect("load");

        match &result.provisioning {
            RepoConfigProvisioning::DownloadedPersistFailed {
                url: got_url,
                dest: got_dest,
                ..
            } => {
                assert_eq!(got_dest, &dest);
                assert!(got_url.contains("127.0.0.1"));
            }
            other => panic!("expected DownloadedPersistFailed, got {other:?}"),
        }
    }
}
