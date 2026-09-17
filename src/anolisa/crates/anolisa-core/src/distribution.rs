//! DistributionIndex: typed view over the artifact registry.
//!
//! ANOLISA component manifests declare *what* a component is; the
//! `DistributionIndex` declares *where* concrete pre-built artifacts live
//! (URL, checksum, signature, backend, os/arch/libc/pkg_base selectors).
//!
//! This module is a pure metadata layer:
//!   * NO network IO,
//!   * NO file download,
//!   * NO signature verification.
//!
//! It only loads TOML and resolves a query to a single matching entry.

use semver::Version;
use serde::{Deserialize, Deserializer, Serialize};
use std::path::Path;

/// Top-level DistributionIndex document.
///
/// This is the in-memory shape used by the resolver. The on-disk TOML uses
/// `[[entries]]` array-of-tables so each entry is self-describing.
///
/// Optional top-level meta fields (`channel`, `generated_at`, `expires_at`,
/// `publisher`, `signature`) are descriptive: they document the index as a
/// whole and may default values per `[[entries]]` rows (today only `channel`
/// participates in resolver matching when explicitly set on a row).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DistributionIndex {
    /// On-disk schema version for distribution index parsing.
    pub schema_version: u32,
    /// Default channel for this index. Entries with an explicit
    /// `channel` override take precedence over this default.
    #[serde(default)]
    pub channel: Option<String>,
    /// ISO-8601 timestamp when this index was published.
    #[serde(default)]
    pub generated_at: Option<String>,
    /// ISO-8601 timestamp after which this index should be considered stale.
    #[serde(default)]
    pub expires_at: Option<String>,
    /// Publishing party (e.g. `"anolisa"`, `"internal-mirror"`).
    #[serde(default)]
    pub publisher: Option<String>,
    /// Index-level signature scheme (e.g. `"cosign"`).
    #[serde(default)]
    pub signature: Option<String>,
    /// Concrete artifact rows available to the resolver.
    #[serde(default)]
    pub entries: Vec<DistributionEntry>,
}

/// One concrete artifact binding for a (component, version, channel, target).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DistributionEntry {
    /// Component this artifact installs.
    pub component: String,
    /// Artifact version.
    pub version: String,
    /// Release channel: "stable" | "beta" | "experimental".
    pub channel: String,
    /// Artifact packaging format.
    pub artifact_type: ArtifactType,
    /// Backend hint for the install runner: "rpm" | "deb" | "tar" | "oci" | "file" | ...
    pub backend: String,
    /// Fetch URL. Resolved rows become live downloads during execute.
    ///
    /// Optional when the published artifact follows the raw repository
    /// layout convention. Empty means "derive from the consumer's repo
    /// root"; consumers without such a root must treat empty as a hard
    /// error rather than guessing.
    #[serde(default)]
    pub url: String,
    /// OS selector: "linux" | "darwin" | ...
    pub os: String,
    /// CPU arch selector: "x86_64" | "aarch64" | "any".
    pub arch: String,
    /// libc selector: "glibc" | "musl" | None (any).
    #[serde(default)]
    pub libc: Option<String>,
    /// OS base selector: "anolis23" | "anolis8" | None (any).
    #[serde(default)]
    pub pkg_base: Option<String>,
    /// Allowed install modes: e.g. ["system", "user"].
    #[serde(default)]
    pub install_modes: Vec<String>,
    /// Expected SHA256 for downloaded bytes. Execute refuses missing checksums
    /// for installable artifacts.
    #[serde(default)]
    pub sha256: Option<String>,
    /// Inline signature metadata, when an index carries it directly.
    #[serde(default)]
    pub signature: Option<String>,
    /// Stable artifact identifier (e.g. `"agentsight-0.5.0-alinux4-x86_64-rpm"`).
    #[serde(default)]
    pub artifact_id: Option<String>,
    /// Digest of the component manifest this artifact was built from.
    #[serde(default)]
    pub manifest_digest: Option<String>,
    /// Artifact size in bytes (purely informational).
    #[serde(default)]
    pub size: Option<u64>,
    /// External signature URL (e.g. `*.sig` companion file).
    #[serde(default)]
    pub signature_url: Option<String>,
    /// Metadata retained for compatibility with existing indexes; not used
    /// to filter or select artifact versions.
    #[serde(default)]
    pub os_version: Option<String>,
    /// Dependency metadata retained for compatibility with existing indexes;
    /// not used by dependency resolution or installation. Raw runtime
    /// dependencies belong in `[[component.dependencies]]` in the artifact's
    /// `component.toml`.
    #[serde(default)]
    pub dependencies: Vec<String>,
}

/// Supported on-the-wire artifact types.
///
/// Wire form is snake_case (`rpm`, `deb`, `tar_gz`, `zip`, `oci`, `file`,
/// `binary`). The custom `Deserialize` impl is lenient: it accepts the
/// legacy spellings `tar.gz` and `tar` and normalizes them to
/// [`ArtifactType::TarGz`].
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactType {
    /// RPM package artifact.
    Rpm,
    /// Debian package artifact.
    Deb,
    /// Gzipped tar archive artifact.
    TarGz,
    /// Zip archive artifact.
    Zip,
    /// OCI image artifact.
    Oci,
    /// Raw file artifact.
    File,
    /// Single executable/binary artifact.
    Binary,
}

impl<'de> Deserialize<'de> for ArtifactType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        match raw.as_str() {
            "rpm" => Ok(Self::Rpm),
            "deb" => Ok(Self::Deb),
            // Accept `tar_gz`, `tar.gz`, and the legacy `tar` spelling.
            "tar_gz" | "tar.gz" | "tar" => Ok(Self::TarGz),
            "zip" => Ok(Self::Zip),
            "oci" => Ok(Self::Oci),
            "file" => Ok(Self::File),
            "binary" => Ok(Self::Binary),
            other => Err(serde::de::Error::unknown_variant(
                other,
                &["rpm", "deb", "tar_gz", "zip", "oci", "file", "binary"],
            )),
        }
    }
}

/// An index row that lenient loading could not represent, with the
/// selector fields recovered best-effort from the raw TOML so callers can
/// tell whether the row could have answered their query.
///
/// Fail-closed contract: a query that *may* match a skipped row must be
/// refused rather than answered from the remaining rows — answering would
/// silently substitute an older parsable version for the one this build
/// cannot read (worst case: `update` downgrading an installed component).
/// Unreadable selector fields are `None` and match conservatively.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedIndexEntry {
    /// `component` field, when readable.
    pub component: Option<String>,
    /// `version` field, when readable.
    pub version: Option<String>,
    /// `channel` field, when readable.
    pub channel: Option<String>,
    /// `os` field, when readable.
    pub os: Option<String>,
    /// `arch` field, when readable.
    pub arch: Option<String>,
    /// `install_modes` field, when readable.
    pub install_modes: Option<Vec<String>>,
    /// Human-facing diagnostic (row position, component, parse error).
    pub reason: String,
}

impl SkippedIndexEntry {
    /// Whether this skipped row could have answered `q`, mirroring the
    /// selector rules of [`DistributionIndex::resolve`] with `None`
    /// (unreadable) fields treated as matching. A `version: None` query
    /// ("latest") matches any skipped version of the component — the row
    /// might be the newest — while a pinned query matches only its exact
    /// version. `libc`/`pkg_base` are not recovered and never narrow the
    /// answer, erring on refusal.
    pub fn may_match(&self, q: &ResolveQuery<'_>) -> bool {
        let component_matches = self.component.as_deref().is_none_or(|c| c == q.component);
        let version_matches = match q.version {
            Some(pinned) => self.version.as_deref().is_none_or(|v| v == pinned),
            None => true,
        };
        let channel_matches = self
            .channel
            .as_deref()
            .is_none_or(|c| c == q.channel.unwrap_or("stable"));
        let os_matches = self.os.as_deref().is_none_or(|os| os == q.os);
        let arch_matches = self
            .arch
            .as_deref()
            .is_none_or(|arch| arch == q.arch || arch == "any");
        let mode_matches = self
            .install_modes
            .as_ref()
            .is_none_or(|modes| modes.iter().any(|m| m == q.install_mode));
        component_matches
            && version_matches
            && channel_matches
            && os_matches
            && arch_matches
            && mode_matches
    }
}

/// Resolver query. Borrowed so callers can build it without allocating.
#[derive(Debug, Clone)]
pub struct ResolveQuery<'a> {
    /// Component name to resolve.
    pub component: &'a str,
    /// None => pick highest version in the channel.
    pub version: Option<&'a str>,
    /// None => "stable".
    pub channel: Option<&'a str>,
    /// Requested install mode.
    pub install_mode: &'a str,
    /// Target operating system.
    pub os: &'a str,
    /// Target CPU architecture.
    pub arch: &'a str,
    /// Target libc, when known.
    pub libc: Option<&'a str>,
    /// Target OS package base, when known.
    pub pkg_base: Option<&'a str>,
    /// Ordered tiebreaker. When more than one entry survives version
    /// selection, the first listed type that matches *any* candidate is
    /// preferred. An empty slice preserves legacy ambiguity behavior.
    pub preferred_types: &'a [ArtifactType],
}

/// Resolver errors. These are vocabulary errors — IO and parse errors live in
/// `DistributionError`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResolveError {
    /// No row matched the requested target tuple.
    #[error("no distribution entry matches the query")]
    NotFound,
    /// More than one row survived resolver filtering.
    #[error("multiple distribution entries match the query ({} candidates)", .0.len())]
    Ambiguous(Vec<DistributionEntry>),
    /// Rows exist for the component but none support the requested mode.
    #[error("install mode is not supported by any candidate entry")]
    UnsupportedMode,
    /// A matching row is missing checksum metadata.
    #[error("matching entry has no sha256 but checksum was requested")]
    ChecksumMissing,
}

/// IO / parse errors when loading an index.
#[derive(Debug, thiserror::Error)]
pub enum DistributionError {
    /// Index file could not be read.
    #[error("cannot read distribution index '{0}': {1}")]
    Io(String, std::io::Error),
    /// Index TOML could not be parsed.
    #[error("cannot parse distribution index '{0}': {1}")]
    Parse(String, String),
}

impl DistributionIndex {
    /// Load a `DistributionIndex` from a TOML file on disk.
    pub fn load(path: &Path) -> Result<Self, DistributionError> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| DistributionError::Io(path.display().to_string(), e))?;
        Self::from_toml_str(&content)
            .map_err(|e| DistributionError::Parse(path.display().to_string(), e))
    }

    /// Parse from a TOML string. Returned error is the raw `toml` message.
    pub fn from_toml_str(s: &str) -> Result<Self, String> {
        toml::from_str(s).map_err(|e| e.to_string())
    }

    /// Load like [`Self::load`], but tolerate individually unparsable
    /// entries instead of failing the whole index.
    ///
    /// Forward compatibility for consumers of a shared index: when a future
    /// repository publishes an entry this build cannot represent (e.g. a new
    /// `artifact_type`), that row is skipped while every other entry stays
    /// installable; atomic parsing would instead reject the entire index and
    /// break unrelated components. Callers MUST hold each returned
    /// [`SkippedIndexEntry`] against their query via
    /// [`SkippedIndexEntry::may_match`] and refuse to resolve when one may
    /// match — otherwise a query for "latest" would silently fall back to
    /// an older parsable version of the same component, which is a silent
    /// downgrade, not fail-closed. File-level errors (I/O, TOML syntax,
    /// malformed header) still fail the whole load: a damaged index is not
    /// something to shop around in.
    pub fn load_lenient(path: &Path) -> Result<(Self, Vec<SkippedIndexEntry>), DistributionError> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| DistributionError::Io(path.display().to_string(), e))?;
        Self::from_toml_str_lenient(&content)
            .map_err(|e| DistributionError::Parse(path.display().to_string(), e))
    }

    /// Entry-tolerant variant of [`Self::from_toml_str`]; see
    /// [`Self::load_lenient`] for the compatibility rationale and the
    /// obligation the skipped rows place on callers.
    pub fn from_toml_str_lenient(s: &str) -> Result<(Self, Vec<SkippedIndexEntry>), String> {
        /// Index shell with entries left as raw TOML values so one bad row
        /// cannot poison the header or its siblings.
        #[derive(Deserialize)]
        struct LenientIndex {
            schema_version: u32,
            #[serde(default)]
            channel: Option<String>,
            #[serde(default)]
            generated_at: Option<String>,
            #[serde(default)]
            expires_at: Option<String>,
            #[serde(default)]
            publisher: Option<String>,
            #[serde(default)]
            signature: Option<String>,
            #[serde(default)]
            entries: Vec<toml::Value>,
        }

        fn get_str(value: &toml::Value, key: &str) -> Option<String> {
            value.get(key).and_then(|v| v.as_str()).map(str::to_owned)
        }

        let raw: LenientIndex = toml::from_str(s).map_err(|e| e.to_string())?;
        let mut entries = Vec::with_capacity(raw.entries.len());
        let mut skipped = Vec::new();
        for (i, value) in raw.entries.into_iter().enumerate() {
            // Selector fields are captured best-effort *before* the strict
            // parse so a skipped row can still be matched against queries;
            // a field too broken to read stays None and matches
            // conservatively (see [`SkippedIndexEntry::may_match`]).
            let component = get_str(&value, "component");
            let version = get_str(&value, "version");
            let channel = get_str(&value, "channel");
            let os = get_str(&value, "os");
            let arch = get_str(&value, "arch");
            // All-or-nothing recovery: one non-string element makes the
            // whole selector unreadable (`None`, matches conservatively).
            // A partially recovered list would under-block — e.g. valid
            // TOML `install_modes = [1]` recovered as an empty list
            // matches no install mode, letting the skipped row bypass the
            // downgrade gate.
            let install_modes = value
                .get("install_modes")
                .and_then(|v| v.as_array())
                .and_then(|modes| {
                    modes
                        .iter()
                        .map(|m| m.as_str().map(str::to_owned))
                        .collect::<Option<Vec<_>>>()
                });
            match value.try_into::<DistributionEntry>() {
                Ok(entry) => entries.push(entry),
                Err(e) => {
                    let who = component
                        .as_deref()
                        .map(|c| format!(" (component '{c}')"))
                        .unwrap_or_default();
                    skipped.push(SkippedIndexEntry {
                        component,
                        version,
                        channel,
                        os,
                        arch,
                        install_modes,
                        reason: format!("skipped index entry #{i}{who}: {e}"),
                    });
                }
            }
        }
        Ok((
            Self {
                schema_version: raw.schema_version,
                channel: raw.channel,
                generated_at: raw.generated_at,
                expires_at: raw.expires_at,
                publisher: raw.publisher,
                signature: raw.signature,
                entries,
            },
            skipped,
        ))
    }

    /// Serialize to TOML. Useful for tests and tooling.
    pub fn to_toml_string(&self) -> Result<String, String> {
        toml::to_string(self).map_err(|e| e.to_string())
    }

    /// Resolve a query to a single matching entry.
    ///
    /// `os_version` is compatibility metadata and does not participate in
    /// matching. Raw runtime dependencies are declared in the artifact's
    /// `component.toml` and handled by the install workflow.
    ///
    /// Filter rules (in order):
    ///   1. `component` exact match.
    ///   2. `channel` exact match (query default "stable").
    ///   3. `install_mode` must appear in the entry's `install_modes`.
    ///   4. `os` exact match.
    ///   5. `arch` exact match OR entry arch == "any".
    ///   6. `libc` and `pkg_base`: if entry has Some, query must match.
    ///      If entry has None, accepted for any query value.
    ///   7. `version`: if Some, exact match. If None, keep only entries with
    ///      the highest semver version (lexicographic fallback).
    ///   8. Tiebreaker: if `preferred_types` is non-empty and more than one
    ///      candidate remains, the first type in `preferred_types` that
    ///      matches any candidate wins; non-matching entries are dropped.
    ///   9. Exactly one candidate -> Ok; zero -> NotFound; more -> Ambiguous.
    pub fn resolve(&self, q: &ResolveQuery<'_>) -> Result<DistributionEntry, ResolveError> {
        let want_channel = q.channel.unwrap_or("stable");

        // 1-6: filter without considering version.
        let mut candidates: Vec<&DistributionEntry> = self
            .entries
            .iter()
            .filter(|e| e.component == q.component)
            .filter(|e| e.channel == want_channel)
            .filter(|e| e.os == q.os)
            .filter(|e| e.arch == q.arch || e.arch == "any")
            .filter(|e| matches_optional(e.libc.as_deref(), q.libc))
            .filter(|e| matches_optional(e.pkg_base.as_deref(), q.pkg_base))
            .collect();

        if candidates.is_empty() {
            return Err(ResolveError::NotFound);
        }

        // 7a: install_mode filter — track separately so we can distinguish
        // "would have matched but the install mode is wrong" from a generic
        // NotFound.
        let before_mode = candidates.len();
        candidates.retain(|e| e.install_modes.iter().any(|m| m.as_str() == q.install_mode));
        if candidates.is_empty() {
            return if before_mode > 0 {
                Err(ResolveError::UnsupportedMode)
            } else {
                Err(ResolveError::NotFound)
            };
        }

        // 7b: version selection — narrow `candidates` rather than picking
        // eagerly, so the preferred-type tiebreaker can run afterwards.
        match q.version {
            Some(v) => {
                candidates.retain(|e| e.version == v);
                if candidates.is_empty() {
                    return Err(ResolveError::NotFound);
                }
            }
            None => {
                retain_highest_version(&mut candidates);
            }
        }

        // 8: preferred-type tiebreaker. Empty preferences keep legacy
        // behavior — multiple candidates surface as Ambiguous below.
        if candidates.len() > 1 && !q.preferred_types.is_empty() {
            for preferred in q.preferred_types {
                if candidates.iter().any(|e| e.artifact_type == *preferred) {
                    candidates.retain(|e| e.artifact_type == *preferred);
                    break;
                }
            }
        }

        // 9: final cardinality check.
        match candidates.len() {
            0 => Err(ResolveError::NotFound),
            1 => Ok(candidates[0].clone()),
            _ => Err(ResolveError::Ambiguous(
                candidates.into_iter().cloned().collect(),
            )),
        }
    }

    /// All distinct versions that pass the same component / channel / os / arch
    /// / libc / pkg_base / install_mode filters as [`resolve`](Self::resolve),
    /// highest-first by semver (lexicographic fallback). Ignores `q.version`
    /// and the preferred-type tiebreaker — it answers "what could this query
    /// resolve to", for dry-run previews that must agree with `resolve`.
    pub fn matching_versions(&self, q: &ResolveQuery<'_>) -> Vec<String> {
        let want_channel = q.channel.unwrap_or("stable");
        let mut versions: Vec<String> = self
            .entries
            .iter()
            .filter(|e| e.component == q.component)
            .filter(|e| e.channel == want_channel)
            .filter(|e| e.os == q.os)
            .filter(|e| e.arch == q.arch || e.arch == "any")
            .filter(|e| matches_optional(e.libc.as_deref(), q.libc))
            .filter(|e| matches_optional(e.pkg_base.as_deref(), q.pkg_base))
            .filter(|e| e.install_modes.iter().any(|m| m.as_str() == q.install_mode))
            .map(|e| e.version.clone())
            .collect();
        // Highest-first so the head of the list is the version `resolve` picks.
        versions.sort_by(|a, b| match (Version::parse(a), Version::parse(b)) {
            (Ok(va), Ok(vb)) => vb.cmp(&va),
            _ => b.cmp(a),
        });
        versions.dedup();
        versions
    }

    /// Distinct `os`/`arch` pairs this index publishes for the query's
    /// component and channel, sorted and deduplicated.
    ///
    /// Ignores every other selector — including `q.os` and `q.arch` — so it
    /// answers "where is this component published at all" rather than
    /// [`matching_versions`]' "what could this query resolve to". Callers use
    /// the difference to tell a host the index carries no artifact for apart
    /// from a component the index never carried.
    ///
    /// An `arch = "any"` row is reported verbatim instead of being expanded
    /// per architecture; match it against a requested arch with the same rule
    /// [`resolve`](Self::resolve) applies.
    ///
    /// [`matching_versions`]: Self::matching_versions
    pub fn published_platforms(&self, q: &ResolveQuery<'_>) -> Vec<(String, String)> {
        let want_channel = q.channel.unwrap_or("stable");
        let mut platforms: Vec<(String, String)> = self
            .entries
            .iter()
            .filter(|e| e.component == q.component)
            .filter(|e| e.channel == want_channel)
            .map(|e| (e.os.clone(), e.arch.clone()))
            .collect();
        // Sorted so a rendered list does not depend on how the published index
        // happens to order its rows; sorting is also what groups the repeats
        // `dedup` then removes (one row per version of the same target).
        platforms.sort_unstable();
        platforms.dedup();
        platforms
    }
}

/// Optional selector match: entry None => wildcard accept; entry Some =>
/// query must be Some and equal.
fn matches_optional(entry_val: Option<&str>, query_val: Option<&str>) -> bool {
    match entry_val {
        None => true,
        Some(ev) => query_val.is_some_and(|qv| qv == ev),
    }
}

/// Narrow `candidates` to the entries that share the highest version. Uses
/// semver when every candidate version parses; otherwise falls back to
/// lexicographic comparison. `candidates` is mutated in place and is
/// guaranteed non-empty on input.
fn retain_highest_version(candidates: &mut Vec<&DistributionEntry>) {
    if candidates.len() <= 1 {
        return;
    }

    let parsed: Option<Vec<Version>> = candidates
        .iter()
        .map(|e| Version::parse(&e.version).ok())
        .collect();

    if let Some(versions) = parsed {
        let mut best = versions[0].clone();
        for v in versions.iter().skip(1) {
            if *v > best {
                best = v.clone();
            }
        }
        let best_str = best.to_string();
        candidates.retain(|e| e.version == best_str);
    } else {
        let best = candidates
            .iter()
            .map(|e| e.version.clone())
            .max()
            .unwrap_or_default();
        candidates.retain(|e| e.version == best);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn sample_entry() -> DistributionEntry {
        DistributionEntry {
            component: "agentsight".into(),
            version: "0.1.0".into(),
            channel: "stable".into(),
            artifact_type: ArtifactType::Rpm,
            backend: "rpm".into(),
            url: "https://example.invalid/agentsight-0.1.0.rpm".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
            libc: Some("glibc".into()),
            pkg_base: Some("anolis23".into()),
            install_modes: vec!["system".into()],
            sha256: Some("0".repeat(64)),
            signature: None,
            artifact_id: None,
            manifest_digest: None,
            size: None,
            signature_url: None,
            os_version: None,
            dependencies: vec!["kernel-headers".into()],
        }
    }

    fn linux_x86_query<'a>(component: &'a str, mode: &'a str) -> ResolveQuery<'a> {
        ResolveQuery {
            component,
            version: None,
            channel: None,
            install_mode: mode,
            os: "linux",
            arch: "x86_64",
            libc: Some("glibc"),
            pkg_base: Some("anolis23"),
            preferred_types: &[],
        }
    }

    #[test]
    fn toml_roundtrip_preserves_entries() {
        let index = DistributionIndex {
            schema_version: 1,
            channel: None,
            generated_at: None,
            expires_at: None,
            publisher: None,
            signature: None,
            entries: vec![sample_entry()],
        };

        let serialized = index.to_toml_string().expect("serialize");
        let parsed: DistributionIndex =
            DistributionIndex::from_toml_str(&serialized).expect("deserialize");

        assert_eq!(parsed.schema_version, 1);
        assert_eq!(parsed.entries.len(), 1);
        assert_eq!(parsed.entries[0], index.entries[0]);
    }

    /// The bundled distribution-index may ship reviewed release entries,
    /// but it must not contain template placeholders. `example.invalid`
    /// rows became a footgun once `download::Download` graduated to
    /// HTTP(S), because a resolved row becomes a real fetch on execute.
    ///
    /// This test pins the current dev-tree contract: `cosh` resolves from
    /// the built-in index, while unreleased template URLs remain excluded.
    #[test]
    fn bundled_distribution_index_contains_only_reviewed_entries() {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../manifests/components/index.toml");
        let index = DistributionIndex::load(&fixture).expect("load fixture");
        assert!(
            index
                .entries
                .iter()
                .all(|entry| !entry.url.contains("example.invalid")),
            "bundled distribution-index must not ship template placeholder URLs",
        );

        let q = linux_x86_query("cosh", "system");
        let resolved = index.resolve(&q).expect("cosh entry resolves");
        assert_eq!(resolved.component, "cosh");
        assert_eq!(resolved.artifact_type, ArtifactType::TarGz);
        assert!(
            resolved
                .sha256
                .as_deref()
                .is_some_and(|sha| sha.len() == 64),
            "bundled release entries must carry concrete sha256 values",
        );
    }

    #[test]
    fn resolve_wrong_arch_returns_not_found() {
        let index = DistributionIndex {
            schema_version: 1,
            channel: None,
            generated_at: None,
            expires_at: None,
            publisher: None,
            signature: None,
            entries: vec![sample_entry()],
        };
        let mut q = linux_x86_query("agentsight", "system");
        q.arch = "aarch64";

        assert_eq!(index.resolve(&q), Err(ResolveError::NotFound));
    }

    #[test]
    fn resolve_without_version_picks_highest_semver() {
        let mut newer = sample_entry();
        newer.version = "0.2.0".into();
        newer.url = "https://example.invalid/agentsight-0.2.0.rpm".into();

        let index = DistributionIndex {
            schema_version: 1,
            channel: None,
            generated_at: None,
            expires_at: None,
            publisher: None,
            signature: None,
            entries: vec![sample_entry(), newer.clone()],
        };

        let q = linux_x86_query("agentsight", "system");
        let entry = index.resolve(&q).expect("resolve");
        assert_eq!(entry.version, "0.2.0");
        assert_eq!(entry.url, newer.url);
    }

    #[test]
    fn resolve_ambiguous_when_two_entries_share_version_query() {
        // Two entries with the same component/channel/os/arch/version but
        // differing libc=None (wildcard) — both match a query with libc=Some.
        let a = sample_entry();
        let mut b = sample_entry();
        b.libc = None;
        b.url = "https://example.invalid/agentsight-0.1.0.alt.rpm".into();

        let index = DistributionIndex {
            schema_version: 1,
            channel: None,
            generated_at: None,
            expires_at: None,
            publisher: None,
            signature: None,
            entries: vec![a, b],
        };

        let mut q = linux_x86_query("agentsight", "system");
        q.version = Some("0.1.0");

        match index.resolve(&q) {
            Err(ResolveError::Ambiguous(list)) => assert_eq!(list.len(), 2),
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn resolve_unsupported_mode_distinguishes_from_not_found() {
        let index = DistributionIndex {
            schema_version: 1,
            channel: None,
            generated_at: None,
            expires_at: None,
            publisher: None,
            signature: None,
            entries: vec![sample_entry()],
        };
        let q = linux_x86_query("agentsight", "user");
        assert_eq!(index.resolve(&q), Err(ResolveError::UnsupportedMode));
    }

    #[test]
    fn load_from_tempfile_roundtrips() {
        let index = DistributionIndex {
            schema_version: 1,
            channel: None,
            generated_at: None,
            expires_at: None,
            publisher: None,
            signature: None,
            entries: vec![sample_entry()],
        };
        let toml_str = index.to_toml_string().expect("serialize");

        let mut tmp = NamedTempFile::new().expect("tempfile");
        tmp.write_all(toml_str.as_bytes()).expect("write");
        let loaded = DistributionIndex::load(tmp.path()).expect("load");

        assert_eq!(loaded.entries.len(), 1);
        assert_eq!(loaded.entries[0], index.entries[0]);
    }

    #[test]
    fn template_distribution_index_loads_with_expected_entries() {
        let template = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../templates/distribution-index.toml");
        let index = DistributionIndex::load(&template).expect("load template");

        assert_eq!(index.schema_version, 1);
        assert_eq!(index.channel.as_deref(), Some("stable"));
        assert_eq!(index.publisher.as_deref(), Some("anolisa"));
        assert_eq!(index.signature.as_deref(), Some("cosign"));
        assert_eq!(
            index.entries.len(),
            3,
            "template should ship 3 example entries"
        );
        // All template entries should belong to the agentsight component.
        assert!(index.entries.iter().all(|e| e.component == "agentsight"));
    }

    fn rpm_and_targz_entries() -> Vec<DistributionEntry> {
        let rpm = DistributionEntry {
            component: "agentsight".into(),
            version: "0.1.0".into(),
            channel: "stable".into(),
            artifact_type: ArtifactType::Rpm,
            backend: "rpm".into(),
            url: "https://example.invalid/agentsight-0.1.0.rpm".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
            libc: Some("glibc".into()),
            pkg_base: Some("anolis23".into()),
            install_modes: vec!["system".into()],
            sha256: Some("0".repeat(64)),
            signature: None,
            artifact_id: None,
            manifest_digest: None,
            size: None,
            signature_url: None,
            os_version: None,
            dependencies: vec![],
        };
        let mut tar = rpm.clone();
        tar.artifact_type = ArtifactType::TarGz;
        tar.backend = "tar".into();
        tar.url = "https://example.invalid/agentsight-0.1.0.tar.gz".into();
        vec![rpm, tar]
    }

    #[test]
    fn resolve_preferred_types_prefers_rpm_when_listed_first() {
        let index = DistributionIndex {
            schema_version: 1,
            channel: None,
            generated_at: None,
            expires_at: None,
            publisher: None,
            signature: None,
            entries: rpm_and_targz_entries(),
        };
        let prefs = [ArtifactType::Rpm, ArtifactType::TarGz];
        let mut q = linux_x86_query("agentsight", "system");
        q.version = Some("0.1.0");
        q.preferred_types = &prefs;

        let entry = index.resolve(&q).expect("resolve");
        assert_eq!(entry.artifact_type, ArtifactType::Rpm);
    }

    #[test]
    fn resolve_preferred_types_prefers_tar_gz_when_listed_first() {
        let index = DistributionIndex {
            schema_version: 1,
            channel: None,
            generated_at: None,
            expires_at: None,
            publisher: None,
            signature: None,
            entries: rpm_and_targz_entries(),
        };
        let prefs = [ArtifactType::TarGz, ArtifactType::Rpm];
        let mut q = linux_x86_query("agentsight", "system");
        q.version = Some("0.1.0");
        q.preferred_types = &prefs;

        let entry = index.resolve(&q).expect("resolve");
        assert_eq!(entry.artifact_type, ArtifactType::TarGz);
    }

    #[test]
    fn resolve_empty_preferred_types_keeps_ambiguous() {
        let index = DistributionIndex {
            schema_version: 1,
            channel: None,
            generated_at: None,
            expires_at: None,
            publisher: None,
            signature: None,
            entries: rpm_and_targz_entries(),
        };
        let mut q = linux_x86_query("agentsight", "system");
        q.version = Some("0.1.0");

        match index.resolve(&q) {
            Err(ResolveError::Ambiguous(list)) => assert_eq!(list.len(), 2),
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn resolve_picks_highest_version_then_preferred() {
        // Two versions (0.1.0 and 0.2.0), each with rpm + tar_gz. With no
        // explicit version, the resolver must first narrow to 0.2.0, then
        // apply preferred_types.
        let mut entries = rpm_and_targz_entries();
        for e in rpm_and_targz_entries() {
            let mut newer = e;
            newer.version = "0.2.0".into();
            newer.url = newer.url.replace("0.1.0", "0.2.0");
            entries.push(newer);
        }

        let index = DistributionIndex {
            schema_version: 1,
            channel: None,
            generated_at: None,
            expires_at: None,
            publisher: None,
            signature: None,
            entries,
        };

        let prefs = [ArtifactType::TarGz, ArtifactType::Rpm];
        let mut q = linux_x86_query("agentsight", "system");
        q.preferred_types = &prefs;

        let entry = index.resolve(&q).expect("resolve");
        assert_eq!(entry.version, "0.2.0");
        assert_eq!(entry.artifact_type, ArtifactType::TarGz);
    }

    #[test]
    fn artifact_type_deserialize_accepts_legacy_spellings() {
        // `tar.gz` and `tar` must both normalize to TarGz.
        let toml_str = r#"
            schema_version = 1
            [[entries]]
            component = "x"
            version = "0.1.0"
            channel = "stable"
            artifact_type = "tar.gz"
            backend = "tar"
            url = "https://example.invalid/x.tar.gz"
            os = "linux"
            arch = "x86_64"
            install_modes = ["user"]
        "#;
        let index = DistributionIndex::from_toml_str(toml_str).expect("parse");
        assert_eq!(index.entries[0].artifact_type, ArtifactType::TarGz);
    }

    /// Lenient loading skips only the rows this build cannot represent
    /// (fail closed for their component) and keeps siblings installable;
    /// strict parsing of the same document still fails atomically.
    #[test]
    fn lenient_parse_skips_unknown_entry_and_keeps_siblings() {
        let toml_str = r#"
            schema_version = 1
            [[entries]]
            component = "cosh"
            version = "1.2.3"
            channel = "stable"
            artifact_type = "tar_gz"
            backend = "tar"
            url = "https://example.invalid/cosh.tar.gz"
            os = "linux"
            arch = "x86_64"
            install_modes = ["user"]
            [[entries]]
            component = "future-thing"
            version = "0.1.0"
            channel = "stable"
            artifact_type = "hologram_v9"
            backend = "tar"
            url = "https://example.invalid/future.tar.gz"
            os = "linux"
            arch = "x86_64"
            install_modes = ["user"]
        "#;
        assert!(
            DistributionIndex::from_toml_str(toml_str).is_err(),
            "strict parse must stay atomic"
        );
        let (index, skipped) =
            DistributionIndex::from_toml_str_lenient(toml_str).expect("lenient parse");
        assert_eq!(index.entries.len(), 1);
        assert_eq!(index.entries[0].component, "cosh");
        assert_eq!(skipped.len(), 1);
        assert!(
            skipped[0].reason.contains("future-thing"),
            "diagnostic should name the skipped component, got: {}",
            skipped[0].reason
        );
        assert_eq!(skipped[0].component.as_deref(), Some("future-thing"));
        assert_eq!(skipped[0].version.as_deref(), Some("0.1.0"));
    }

    /// A selector that is valid TOML but not the shape this build expects
    /// (e.g. non-string elements in `install_modes`) must be recovered as
    /// unreadable (`None`) — never as a partial list. `Some([])` would
    /// match no install mode and let the row bypass the downgrade gate.
    #[test]
    fn lenient_parse_treats_malformed_selector_as_unreadable() {
        let toml_str = r#"
            schema_version = 1
            [[entries]]
            component = "sec-core"
            version = "2.0.0"
            channel = "stable"
            artifact_type = "hologram_v9"
            backend = "tar"
            url = "https://example.invalid/sec-core.tar.gz"
            os = "linux"
            arch = "x86_64"
            install_modes = [1]
        "#;
        let (index, skipped) =
            DistributionIndex::from_toml_str_lenient(toml_str).expect("lenient parse");
        assert!(index.entries.is_empty());
        assert_eq!(skipped.len(), 1);
        assert_eq!(
            skipped[0].install_modes, None,
            "a partially readable selector must collapse to unreadable"
        );
        // And the unreadable selector must block conservatively.
        let query = ResolveQuery {
            component: "sec-core",
            version: None,
            channel: None,
            install_mode: "user",
            os: "linux",
            arch: "x86_64",
            libc: None,
            pkg_base: None,
            preferred_types: &[],
        };
        assert!(skipped[0].may_match(&query));
    }

    /// The fail-closed matcher: a skipped row blocks exactly the queries it
    /// could have answered — same component and target (any version when
    /// the query wants "latest", the exact version when pinned) — and
    /// unreadable fields block conservatively.
    #[test]
    fn skipped_entry_may_match_mirrors_resolver_selectors() {
        let skipped = SkippedIndexEntry {
            component: Some("sec-core".to_string()),
            version: Some("2.0.0".to_string()),
            channel: Some("stable".to_string()),
            os: Some("linux".to_string()),
            arch: Some("x86_64".to_string()),
            install_modes: Some(vec!["user".to_string()]),
            reason: "skipped index entry #1 (component 'sec-core')".to_string(),
        };
        let query = |component: &'static str, version: Option<&'static str>| ResolveQuery {
            component,
            version,
            channel: None,
            install_mode: "user",
            os: "linux",
            arch: "x86_64",
            libc: None,
            pkg_base: None,
            preferred_types: &[],
        };
        // "latest" for the same component: the skipped row might be newest.
        assert!(skipped.may_match(&query("sec-core", None)));
        // The pinned version the row itself advertises.
        assert!(skipped.may_match(&query("sec-core", Some("2.0.0"))));
        // A different pinned version is answerable from parsable rows.
        assert!(!skipped.may_match(&query("sec-core", Some("1.0.0"))));
        // Unrelated components are unaffected.
        assert!(!skipped.may_match(&query("cosh", None)));
        // Non-matching target never blocks.
        let mut other_os = query("sec-core", None);
        other_os.os = "darwin";
        assert!(!skipped.may_match(&other_os));
        // Unreadable selectors must block conservatively.
        let opaque = SkippedIndexEntry {
            component: None,
            version: None,
            channel: None,
            os: None,
            arch: None,
            install_modes: None,
            reason: "skipped index entry #0".to_string(),
        };
        assert!(opaque.may_match(&query("cosh", Some("1.0.0"))));
    }

    /// A syntactically damaged file must fail even in lenient mode: entry
    /// tolerance is for schema evolution, not for corrupted downloads.
    #[test]
    fn lenient_parse_still_rejects_file_level_damage() {
        assert!(DistributionIndex::from_toml_str_lenient("schema_version = [broken").is_err());
    }

    /// The platform list renders what the index publishes, so it must not
    /// depend on row order, must collapse the one row per version that a
    /// target accumulates, and must not be narrowed by the selectors
    /// [`resolve`](DistributionIndex::resolve) only applies later (install
    /// mode, libc, package base): a platform the index publishes for is an
    /// alternative worth naming even when this query's other selectors would
    /// have rejected its rows.
    #[test]
    fn published_platforms_dedup_sort_and_ignore_narrowing_selectors() {
        let index = DistributionIndex::from_toml_str(
            r#"
            schema_version = 1
            [[entries]]
            component = "agentsight"
            version = "0.3.0"
            channel = "stable"
            artifact_type = "tar_gz"
            backend = "raw"
            os = "linux"
            arch = "s390x"
            install_modes = ["user"]
            [[entries]]
            component = "agentsight"
            version = "0.2.0"
            channel = "stable"
            artifact_type = "tar_gz"
            backend = "raw"
            os = "linux"
            arch = "ppc64le"
            libc = "musl"
            pkg_base = "anolis8"
            install_modes = ["system"]
            [[entries]]
            component = "agentsight"
            version = "0.1.0"
            channel = "stable"
            artifact_type = "tar_gz"
            backend = "raw"
            os = "linux"
            arch = "ppc64le"
            install_modes = ["system"]
            "#,
        )
        .expect("parse");

        let q = linux_x86_query("agentsight", "system");
        assert_eq!(
            index.published_platforms(&q),
            vec![
                ("linux".to_string(), "ppc64le".to_string()),
                ("linux".to_string(), "s390x".to_string()),
            ]
        );
    }

    /// Only the queried component's rows in the queried channel are published
    /// *for that query*. Offering another component's target, or another
    /// channel's, would name an alternative that cannot answer the request.
    #[test]
    fn published_platforms_scope_to_component_and_channel() {
        let index = DistributionIndex::from_toml_str(
            r#"
            schema_version = 1
            [[entries]]
            component = "agentsight"
            version = "0.1.0"
            channel = "stable"
            artifact_type = "tar_gz"
            backend = "raw"
            os = "linux"
            arch = "x86_64"
            install_modes = ["system"]
            [[entries]]
            component = "agentsight"
            version = "0.1.0"
            channel = "beta"
            artifact_type = "tar_gz"
            backend = "raw"
            os = "linux"
            arch = "riscv64"
            install_modes = ["system"]
            [[entries]]
            component = "cosh"
            version = "0.1.0"
            channel = "stable"
            artifact_type = "tar_gz"
            backend = "raw"
            os = "linux"
            arch = "ppc64le"
            install_modes = ["system"]
            "#,
        )
        .expect("parse");

        let stable = linux_x86_query("agentsight", "system");
        assert_eq!(
            index.published_platforms(&stable),
            vec![("linux".to_string(), "x86_64".to_string())]
        );

        let beta = ResolveQuery {
            channel: Some("beta"),
            ..linux_x86_query("agentsight", "system")
        };
        assert_eq!(
            index.published_platforms(&beta),
            vec![("linux".to_string(), "riscv64".to_string())]
        );

        // A component the index never carried publishes no platform at all,
        // which is how the caller tells this from a platform gap.
        assert!(
            index
                .published_platforms(&linux_x86_query("tokenless", "system"))
                .is_empty()
        );
    }

    /// `arch = "any"` stays a published target in its own right instead of
    /// being expanded per architecture; the caller matches it against the
    /// requested arch with the same rule `resolve` applies.
    #[test]
    fn published_platforms_report_any_arch_verbatim() {
        let index = DistributionIndex::from_toml_str(
            r#"
            schema_version = 1
            [[entries]]
            component = "agentsight"
            version = "0.1.0"
            channel = "stable"
            artifact_type = "tar_gz"
            backend = "raw"
            os = "linux"
            arch = "any"
            install_modes = ["system"]
            "#,
        )
        .expect("parse");

        let q = linux_x86_query("agentsight", "system");
        assert_eq!(
            index.published_platforms(&q),
            vec![("linux".to_string(), "any".to_string())]
        );
    }

    #[test]
    fn entry_without_url_parses_as_empty_for_convention_layout() {
        let toml_str = r#"
            schema_version = 1
            [[entries]]
            component = "tokenless"
            version = "0.5.0"
            channel = "stable"
            artifact_type = "tar_gz"
            backend = "raw"
            os = "linux"
            arch = "x86_64"
            install_modes = ["system"]
        "#;
        let index = DistributionIndex::from_toml_str(toml_str).expect("parse");
        assert_eq!(index.entries[0].url, "");
    }
}
