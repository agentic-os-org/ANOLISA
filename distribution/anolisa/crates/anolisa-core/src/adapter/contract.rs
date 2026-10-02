//! Component contract discovery.
//!
//! Resolves a [`ComponentManifest`] for an installed component by searching
//! a caller-supplied list of candidate paths in priority order. The typical
//! ordering is:
//!
//! 1. **State snapshots** — one per state root
//!    (`{state_dir}/component-manifests/<component>/component.toml`).
//! 2. **Datadir contracts** — one per datadir root
//!    (`{datadir}/components/<component>/component.toml`).
//!
//! The first file found wins. A TOML parse error is surfaced immediately
//! (it must not be masked as "unavailable").

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use anolisa_platform::fs_layout::FsLayout;

use crate::manifest::{ComponentManifest, ManifestError};

// ---------------------------------------------------------------------------
// Provenance
// ---------------------------------------------------------------------------

/// How a contract snapshot was originally sourced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContractSourceKind {
    /// Sourced from a `{datadir}/components/<component>/component.toml`.
    Datadir,
}

/// Provenance sidecar for a state-snapshotted component contract.
///
/// Written alongside `component.toml` during install/adopt so that later
/// adapter operations can resolve `{datadir}` without content-matching.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractProvenance {
    /// Sidecar schema version (currently `1`).
    pub schema_version: u32,
    /// How the contract was obtained.
    pub source_kind: ContractSourceKind,
    /// Absolute path of the original contract file.
    pub source_path: PathBuf,
    /// The datadir root that `source_path` lives under.
    pub datadir_root: PathBuf,
}

/// Read the provenance sidecar for a snapshot at `snapshot_path`.
///
/// Returns `None` when the file is absent or unparseable — callers
/// must fall back to content matching.
pub fn read_snapshot_provenance(snapshot_path: &Path) -> Option<ContractProvenance> {
    let provenance_path = FsLayout::provenance_path_for_snapshot(snapshot_path);
    let content = std::fs::read_to_string(&provenance_path).ok()?;
    toml::from_str(&content).ok()
}

/// Write a provenance sidecar alongside the snapshot at `snapshot_path`.
pub fn write_snapshot_provenance(
    snapshot_path: &Path,
    provenance: &ContractProvenance,
) -> Result<(), std::io::Error> {
    let provenance_path = FsLayout::provenance_path_for_snapshot(snapshot_path);
    let content = toml::to_string_pretty(provenance).map_err(std::io::Error::other)?;
    std::fs::write(provenance_path, content)
}

/// Determine the effective datadir root for a resolved contract.
///
/// - If `contract_path` is a direct datadir hit (lives under one of
///   `scoped_datadir_roots`), returns that root directly.
/// - If `contract_path` is a state snapshot, tries provenance first: reads
///   `provenance.toml` and uses its `datadir_root` if the source path is
///   consistent and the root is in `scoped_datadir_roots`.
/// - Falls back to content matching: compares the snapshot content against
///   each scoped datadir contract and returns the first match.
/// - Returns `None` when no root can be determined.
pub fn infer_contract_datadir_root(
    component: &str,
    contract_path: &Path,
    scoped_datadir_roots: &[PathBuf],
) -> Option<PathBuf> {
    // Direct datadir hit.
    if let Some(root) = scoped_datadir_roots
        .iter()
        .find(|root| FsLayout::component_contract_path(root, component) == contract_path)
        .cloned()
    {
        return Some(root);
    }

    // Snapshot hit — try provenance sidecar.
    if let Some(prov) = read_snapshot_provenance(contract_path)
        && prov.source_kind == ContractSourceKind::Datadir
        && scoped_datadir_roots.contains(&prov.datadir_root)
        && FsLayout::component_contract_path(&prov.datadir_root, component) == prov.source_path
    {
        return Some(prov.datadir_root);
    }

    // Fallback: content match against scoped datadir contracts.
    let snapshot_content = std::fs::read(contract_path).ok()?;
    scoped_datadir_roots
        .iter()
        .find(|root| {
            let candidate = FsLayout::component_contract_path(root, component);
            std::fs::read(candidate).is_ok_and(|content| content == snapshot_content)
        })
        .cloned()
}

// ---------------------------------------------------------------------------
// Resolved contract with source
// ---------------------------------------------------------------------------

/// A resolved component contract plus the concrete file that supplied it.
#[derive(Debug, Clone)]
pub struct ResolvedComponentContract {
    /// Parsed component manifest.
    pub manifest: ComponentManifest,
    /// Candidate path that won contract resolution.
    pub path: PathBuf,
}

/// Errors from component contract resolution.
#[derive(Debug, thiserror::Error)]
pub enum ContractError {
    /// No contract file was found under any searched root.
    #[error(
        "component contract unavailable for '{component}': no file found at any of {searched:?}"
    )]
    Unavailable {
        /// Component whose contract was requested.
        component: String,
        /// Paths that were tried, in search order.
        searched: Vec<PathBuf>,
    },

    /// A contract file exists but its TOML content could not be parsed.
    #[error("malformed component contract at {path}: {reason}")]
    ParseError {
        /// Path of the file that failed to parse.
        path: PathBuf,
        /// Human-readable parse failure detail.
        reason: String,
    },

    /// A filesystem error occurred while reading a contract file (other
    /// than "not found", which is handled by the search loop).
    #[error("io error reading component contract at {path}: {source}")]
    Io {
        /// Path that triggered the error.
        path: PathBuf,
        /// Underlying IO error.
        #[source]
        source: std::io::Error,
    },
}

/// Build the ordered list of candidate contract paths for `component`
/// across `state_roots` (snapshot priority) then `datadir_roots` (package
/// contract fallback). Path computation is delegated to [`FsLayout`] so
/// the segment constants live in one place.
pub fn candidate_paths(
    component: &str,
    state_roots: &[PathBuf],
    datadir_roots: &[PathBuf],
) -> Vec<PathBuf> {
    let mut paths = Vec::with_capacity(state_roots.len() + datadir_roots.len());
    for state_root in state_roots {
        paths.push(FsLayout::component_manifest_snapshot_path(
            state_root, component,
        ));
    }
    for datadir_root in datadir_roots {
        paths.push(FsLayout::component_contract_path(datadir_root, component));
    }
    paths
}

/// Resolve the component contract for `component` by searching state roots
/// then datadir roots in the supplied order.
///
/// Internally builds the candidate list via [`candidate_paths`] and
/// delegates to [`resolve_from_candidates`].
pub fn resolve_component_contract(
    component: &str,
    state_roots: &[PathBuf],
    datadir_roots: &[PathBuf],
) -> Result<ComponentManifest, ContractError> {
    resolve_component_contract_with_source(component, state_roots, datadir_roots)
        .map(|resolved| resolved.manifest)
}

/// Resolve the component contract and return the file path that supplied
/// the manifest. Use this when callers need to keep layout placeholder
/// expansion scoped to the actual contract source.
pub fn resolve_component_contract_with_source(
    component: &str,
    state_roots: &[PathBuf],
    datadir_roots: &[PathBuf],
) -> Result<ResolvedComponentContract, ContractError> {
    let candidates = candidate_paths(component, state_roots, datadir_roots);
    resolve_from_candidates_with_source(component, &candidates)
}

/// Try each candidate path in order and return the first valid manifest.
///
/// An IO error other than `NotFound` (e.g. permission denied) is returned
/// as [`ContractError::Io`]; a present-but-malformed TOML file is returned
/// as [`ContractError::ParseError`] — it is never silently skipped.
pub fn resolve_from_candidates(
    component: &str,
    candidates: &[PathBuf],
) -> Result<ComponentManifest, ContractError> {
    resolve_from_candidates_with_source(component, candidates).map(|resolved| resolved.manifest)
}

/// Try each candidate path in order and return the first valid manifest
/// with its source path.
pub fn resolve_from_candidates_with_source(
    component: &str,
    candidates: &[PathBuf],
) -> Result<ResolvedComponentContract, ContractError> {
    let mut searched = Vec::new();

    for path in candidates {
        match try_load_contract(path) {
            TryLoad::Loaded(manifest) => {
                return Ok(ResolvedComponentContract {
                    manifest: *manifest,
                    path: path.clone(),
                });
            }
            TryLoad::NotFound => {
                searched.push(path.clone());
            }
            TryLoad::Error(err) => return Err(err),
        }
    }

    Err(ContractError::Unavailable {
        component: component.to_string(),
        searched,
    })
}

/// Three-way outcome of trying to load a single candidate path.
enum TryLoad {
    Loaded(Box<ComponentManifest>),
    NotFound,
    Error(ContractError),
}

/// Attempt to load a contract from `path`, distinguishing "file absent" from
/// "file present but broken" from "file present and valid".
fn try_load_contract(path: &Path) -> TryLoad {
    match ComponentManifest::from_file(path) {
        Ok(manifest) => TryLoad::Loaded(Box::new(manifest)),
        Err(ManifestError::Io(_, ref io_err)) if io_err.kind() == std::io::ErrorKind::NotFound => {
            TryLoad::NotFound
        }
        Err(ManifestError::Io(_, source)) => TryLoad::Error(ContractError::Io {
            path: path.to_path_buf(),
            source,
        }),
        Err(ManifestError::Parse(_, reason)) => TryLoad::Error(ContractError::ParseError {
            path: path.to_path_buf(),
            reason,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Minimal valid component TOML for testing.
    fn valid_toml(name: &str) -> String {
        format!(
            r#"
[component]
name = "{name}"
version = "0.1.0"
layer = "runtime"
"#
        )
    }

    fn write_snapshot(state_root: &Path, component: &str, content: &str) {
        let path = FsLayout::component_manifest_snapshot_path(state_root, component);
        fs::create_dir_all(path.parent().unwrap()).expect("create dir");
        fs::write(&path, content).expect("write");
    }

    fn write_datadir(datadir_root: &Path, component: &str, content: &str) {
        let path = FsLayout::component_contract_path(datadir_root, component);
        fs::create_dir_all(path.parent().unwrap()).expect("create dir");
        fs::write(&path, content).expect("write");
    }

    /// Minimal valid component TOML with a specific version, used to
    /// distinguish which file was loaded.
    fn valid_toml_versioned(name: &str, version: &str) -> String {
        format!(
            r#"
[component]
name = "{name}"
version = "{version}"
layer = "runtime"
"#
        )
    }

    #[test]
    fn snapshot_preferred_over_datadir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = tmp.path().join("state");
        let data = tmp.path().join("data");

        write_snapshot(&state, "mycomp", &valid_toml_versioned("mycomp", "1.0.0"));
        write_datadir(&data, "mycomp", &valid_toml_versioned("mycomp", "2.0.0"));

        let manifest =
            resolve_component_contract("mycomp", &[state], &[data]).expect("should resolve");
        assert_eq!(manifest.component.name, "mycomp");
        assert_eq!(manifest.component.version, "1.0.0");
    }

    #[test]
    fn datadir_found_when_snapshot_absent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = tmp.path().join("state");
        let data = tmp.path().join("data");

        write_datadir(&data, "mycomp", &valid_toml("mycomp"));

        let manifest =
            resolve_component_contract("mycomp", &[state], &[data]).expect("should resolve");
        assert_eq!(manifest.component.name, "mycomp");
    }

    #[test]
    fn both_absent_returns_unavailable() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = tmp.path().join("state");
        let data = tmp.path().join("data");

        let err = resolve_component_contract("mycomp", &[state], &[data])
            .expect_err("should be unavailable");
        assert!(
            matches!(err, ContractError::Unavailable { .. }),
            "expected Unavailable, got: {err}"
        );
    }

    #[test]
    fn malformed_toml_returns_parse_error_not_unavailable() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = tmp.path().join("state");
        let data = tmp.path().join("data");

        write_snapshot(&state, "mycomp", "this is not valid toml = [[[");

        let err = resolve_component_contract("mycomp", &[state], &[data])
            .expect_err("should be parse error");
        assert!(
            matches!(err, ContractError::ParseError { .. }),
            "expected ParseError, got: {err}"
        );
    }

    #[test]
    fn malformed_snapshot_not_masked_by_valid_datadir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = tmp.path().join("state");
        let data = tmp.path().join("data");

        write_snapshot(&state, "mycomp", "bad toml {{{{");
        write_datadir(&data, "mycomp", &valid_toml("mycomp"));

        let err = resolve_component_contract("mycomp", &[state], &[data])
            .expect_err("should be parse error");
        assert!(
            matches!(err, ContractError::ParseError { .. }),
            "expected ParseError, got: {err}"
        );
    }

    #[test]
    fn multiple_state_roots_searched_in_order() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state1 = tmp.path().join("state1");
        let state2 = tmp.path().join("state2");
        let data = tmp.path().join("data");

        write_snapshot(&state2, "mycomp", &valid_toml("mycomp"));

        let manifest = resolve_component_contract("mycomp", &[state1, state2], &[data])
            .expect("should resolve from state2");
        assert_eq!(manifest.component.name, "mycomp");
    }

    #[test]
    fn multiple_datadir_roots_searched_in_order() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = tmp.path().join("state");
        let data1 = tmp.path().join("data1");
        let data2 = tmp.path().join("data2");

        write_datadir(&data2, "mycomp", &valid_toml("mycomp"));

        let manifest = resolve_component_contract("mycomp", &[state], &[data1, data2])
            .expect("should resolve from data2");
        assert_eq!(manifest.component.name, "mycomp");
    }

    // -- provenance read/write ----------------------------------------------

    #[test]
    fn write_then_read_provenance_roundtrip() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = tmp.path().join("state");
        write_snapshot(&state, "mycomp", &valid_toml("mycomp"));
        let snapshot_path = FsLayout::component_manifest_snapshot_path(&state, "mycomp");

        let prov = ContractProvenance {
            schema_version: 1,
            source_kind: ContractSourceKind::Datadir,
            source_path: PathBuf::from("/usr/share/anolisa/components/mycomp/component.toml"),
            datadir_root: PathBuf::from("/usr/share/anolisa"),
        };
        write_snapshot_provenance(&snapshot_path, &prov).expect("write provenance");

        let read_back = read_snapshot_provenance(&snapshot_path).expect("read provenance");
        assert_eq!(read_back.schema_version, 1);
        assert_eq!(read_back.source_kind, ContractSourceKind::Datadir);
        assert_eq!(read_back.datadir_root, PathBuf::from("/usr/share/anolisa"));
    }

    #[test]
    fn read_provenance_returns_none_when_absent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = tmp.path().join("state");
        write_snapshot(&state, "mycomp", &valid_toml("mycomp"));
        let snapshot_path = FsLayout::component_manifest_snapshot_path(&state, "mycomp");

        assert!(read_snapshot_provenance(&snapshot_path).is_none());
    }

    #[test]
    fn read_provenance_returns_none_on_invalid_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = tmp.path().join("state");
        write_snapshot(&state, "mycomp", &valid_toml("mycomp"));
        let snapshot_path = FsLayout::component_manifest_snapshot_path(&state, "mycomp");

        let bad_path = FsLayout::provenance_path_for_snapshot(&snapshot_path);
        fs::write(&bad_path, "not valid {{{toml").expect("write bad");
        assert!(read_snapshot_provenance(&snapshot_path).is_none());
    }

    // -- infer_contract_datadir_root ----------------------------------------

    /// Scenario B: provenance guides resolution when two datadir roots have
    /// identical contracts.
    #[test]
    fn provenance_selects_correct_datadir_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = tmp.path().join("state");
        let local_data = tmp.path().join("local_data");
        let pkg_data = tmp.path().join("pkg_data");

        let contract = valid_toml("sec-core");
        write_snapshot(&state, "sec-core", &contract);
        write_datadir(&local_data, "sec-core", &contract);
        write_datadir(&pkg_data, "sec-core", &contract);

        let snapshot_path = FsLayout::component_manifest_snapshot_path(&state, "sec-core");
        let prov = ContractProvenance {
            schema_version: 1,
            source_kind: ContractSourceKind::Datadir,
            source_path: FsLayout::component_contract_path(&pkg_data, "sec-core"),
            datadir_root: pkg_data.clone(),
        };
        write_snapshot_provenance(&snapshot_path, &prov).expect("write prov");

        let root = infer_contract_datadir_root(
            "sec-core",
            &snapshot_path,
            &[local_data.clone(), pkg_data.clone()],
        );
        assert_eq!(
            root.as_ref(),
            Some(&pkg_data),
            "provenance must select pkg_data, not local_data"
        );
    }

    /// Scenario C: no provenance falls back to content matching.
    #[test]
    fn backward_compat_content_match_without_provenance() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = tmp.path().join("state");
        let data = tmp.path().join("data");

        let contract = valid_toml("sec-core");
        write_snapshot(&state, "sec-core", &contract);
        write_datadir(&data, "sec-core", &contract);

        let snapshot_path = FsLayout::component_manifest_snapshot_path(&state, "sec-core");

        let root =
            infer_contract_datadir_root("sec-core", &snapshot_path, std::slice::from_ref(&data));
        assert_eq!(
            root.as_ref(),
            Some(&data),
            "content matching fallback must find the datadir root"
        );
    }

    /// Scenario D: provenance datadir_root not in scoped roots.
    #[test]
    fn invalid_provenance_falls_back_to_content_match() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = tmp.path().join("state");
        let data = tmp.path().join("data");

        let contract = valid_toml("sec-core");
        write_snapshot(&state, "sec-core", &contract);
        write_datadir(&data, "sec-core", &contract);

        let snapshot_path = FsLayout::component_manifest_snapshot_path(&state, "sec-core");
        let prov = ContractProvenance {
            schema_version: 1,
            source_kind: ContractSourceKind::Datadir,
            source_path: PathBuf::from("/gone/components/sec-core/component.toml"),
            datadir_root: PathBuf::from("/gone"),
        };
        write_snapshot_provenance(&snapshot_path, &prov).expect("write prov");

        let root =
            infer_contract_datadir_root("sec-core", &snapshot_path, std::slice::from_ref(&data));
        assert_eq!(
            root.as_ref(),
            Some(&data),
            "invalid provenance must fall back to content match"
        );
    }

    /// Scenario D variant: malformed provenance TOML.
    #[test]
    fn malformed_provenance_toml_falls_back_to_content_match() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = tmp.path().join("state");
        let data = tmp.path().join("data");

        let contract = valid_toml("sec-core");
        write_snapshot(&state, "sec-core", &contract);
        write_datadir(&data, "sec-core", &contract);

        let snapshot_path = FsLayout::component_manifest_snapshot_path(&state, "sec-core");
        let bad = FsLayout::provenance_path_for_snapshot(&snapshot_path);
        fs::write(&bad, "this is not valid toml [[[").expect("write bad");

        let root =
            infer_contract_datadir_root("sec-core", &snapshot_path, std::slice::from_ref(&data));
        assert_eq!(
            root.as_ref(),
            Some(&data),
            "malformed provenance must fall back to content match"
        );
    }

    /// Scenario D variant: provenance source_path inconsistent with root.
    #[test]
    fn inconsistent_provenance_source_path_falls_back() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = tmp.path().join("state");
        let data = tmp.path().join("data");

        let contract = valid_toml("sec-core");
        write_snapshot(&state, "sec-core", &contract);
        write_datadir(&data, "sec-core", &contract);

        let snapshot_path = FsLayout::component_manifest_snapshot_path(&state, "sec-core");
        let prov = ContractProvenance {
            schema_version: 1,
            source_kind: ContractSourceKind::Datadir,
            source_path: PathBuf::from("/some/other/component.toml"),
            datadir_root: data.clone(),
        };
        write_snapshot_provenance(&snapshot_path, &prov).expect("write prov");

        let root =
            infer_contract_datadir_root("sec-core", &snapshot_path, std::slice::from_ref(&data));
        assert_eq!(
            root.as_ref(),
            Some(&data),
            "inconsistent source_path must fall back to content match"
        );
    }

    /// Direct datadir hit returns that root without provenance.
    #[test]
    fn direct_datadir_hit_returns_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let data = tmp.path().join("data");

        write_datadir(&data, "mycomp", &valid_toml("mycomp"));
        let datadir_path = FsLayout::component_contract_path(&data, "mycomp");

        let root =
            infer_contract_datadir_root("mycomp", &datadir_path, std::slice::from_ref(&data));
        assert_eq!(root.as_ref(), Some(&data));
    }
}
