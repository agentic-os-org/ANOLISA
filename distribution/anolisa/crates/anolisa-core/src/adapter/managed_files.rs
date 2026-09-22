//! Package-owned adapter input revisions and materialized-file verification.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use anolisa_platform::pkg_files::{PackageFileDigestAlgorithm, PackageFileKind, PackageFileQuery};
use sha2::{Digest, Sha256};

use super::AdapterError;
use super::claim::{
    AdapterClaim, AdapterSourceRevision, ClaimResourceKind, ManagedFileKind as RevisionFileKind,
    ManagedSourceFile, MaterializedFile, MaterializedSourceRevision,
};
use super::driver::AdapterOps;
use crate::domain::{Installation, PackageIdentity, ProviderBinding};
use crate::state::{FileOwner, OwnedFileKind};

/// Package-inventory file type before unsupported entries are scoped out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedInventoryKind {
    /// Regular file whose bytes can be verified.
    File,
    /// Symbolic link whose literal target can be verified.
    Symlink,
    /// Device, socket, FIFO, or another unsupported adapter input.
    Unsupported,
}

/// One authoritative package-managed path before it is scoped to an adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedFile {
    /// Absolute installed path.
    pub path: PathBuf,
    /// File or symbolic link.
    pub kind: ManagedInventoryKind,
    /// Lowercase SHA-256 for regular files.
    pub sha256: Option<String>,
    /// Literal link target for symbolic links.
    pub symlink_target: Option<PathBuf>,
}

/// Package-manager-owned files for one component.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedInventory {
    /// Sorted authoritative file entries.
    pub files: Vec<ManagedFile>,
}

/// A driver-declared source-to-resource copy performed by ANOLISA.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializedMapping {
    /// Receipt resource identifying the destination root.
    pub resource_id: String,
    /// Absolute source directory copied into the resource.
    pub source_root: PathBuf,
    /// Source-relative prefixes deliberately excluded from this copy.
    pub excluded_prefixes: Vec<PathBuf>,
}

/// Tri-state managed-file verdict with an operator-facing reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagedMatch {
    /// The condition was verified.
    Matched,
    /// A managed file or revision differs.
    Changed(String),
    /// The authoritative inventory or on-disk bytes could not be verified.
    Unknown(String),
}

/// Load the package manager's authoritative file inventory for an installation.
pub fn inventory_for_installation(
    installation: &Installation,
    rpm_query: &dyn PackageFileQuery,
) -> Result<ManagedInventory, String> {
    let mut files = match &installation.binding {
        ProviderBinding::Owned { artifact } => artifact
            .files
            .iter()
            .filter(|file| file.owner == FileOwner::Anolisa)
            .map(|file| match file.kind {
                OwnedFileKind::File | OwnedFileKind::Config => ManagedFile {
                    path: file.path.clone(),
                    kind: ManagedInventoryKind::File,
                    sha256: file.sha256.clone(),
                    symlink_target: None,
                },
                OwnedFileKind::Symlink => ManagedFile {
                    path: file.path.clone(),
                    kind: ManagedInventoryKind::Symlink,
                    sha256: None,
                    symlink_target: file.referent.clone(),
                },
            })
            .collect::<Vec<_>>(),
        ProviderBinding::Delegated { package, .. } => {
            let PackageIdentity::Resolved { name } = package else {
                return Err("native package identity is unresolved; re-enable the adapter".into());
            };
            let inventory = rpm_query.query_file_inventory(name).map_err(|err| {
                format!("native package file query failed: {err}; re-enable the adapter")
            })?;
            if inventory.digest_algorithm != PackageFileDigestAlgorithm::Sha256 {
                return Err(format!(
                    "native package uses unsupported file digest algorithm {:?}; re-enable the adapter",
                    inventory.digest_algorithm
                ));
            }
            inventory
                .files
                .into_iter()
                .filter_map(|file| {
                    let (kind, sha256, symlink_target) = match file.kind {
                        PackageFileKind::Regular => (ManagedInventoryKind::File, file.digest, None),
                        PackageFileKind::Symlink => (
                            ManagedInventoryKind::Symlink,
                            None,
                            file.link_target.map(PathBuf::from),
                        ),
                        PackageFileKind::Directory => return None,
                        PackageFileKind::Other => (ManagedInventoryKind::Unsupported, None, None),
                    };
                    Some(ManagedFile {
                        path: PathBuf::from(file.path),
                        kind,
                        sha256,
                        symlink_target,
                    })
                })
                .collect()
        }
    };

    for file in &files {
        if !file.path.is_absolute() {
            return Err(format!(
                "managed file path '{}' is not absolute; re-enable the adapter",
                file.path.display()
            ));
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    if files.windows(2).any(|pair| pair[0].path == pair[1].path) {
        return Err(
            "package inventory contains duplicate managed paths; re-enable the adapter".into(),
        );
    }
    Ok(ManagedInventory { files })
}

/// Scope an authoritative component inventory to one resolved adapter root.
pub fn source_revision(
    inventory: &ManagedInventory,
    source_root: &Path,
    mappings: &[MaterializedMapping],
) -> Result<AdapterSourceRevision, String> {
    let lexical_root = normalize_absolute(source_root)?;
    let canonical_root = std::fs::canonicalize(&lexical_root).map_err(|err| {
        format!(
            "cannot resolve adapter source root '{}': {err}; re-enable the adapter",
            lexical_root.display()
        )
    })?;
    let files = source_files_below(inventory, &lexical_root, &canonical_root, &[])?;
    if files.is_empty() {
        return Err(format!(
            "package manager reports no managed adapter files below '{}'; re-enable the adapter",
            lexical_root.display()
        ));
    }
    let mut materialized_sources = Vec::new();
    for mapping in mappings {
        let lexical_mapping_root = normalize_absolute(&mapping.source_root)?;
        let canonical_mapping_root =
            std::fs::canonicalize(&lexical_mapping_root).map_err(|err| {
                format!(
                    "cannot resolve materialized source root '{}': {err}; re-enable the adapter",
                    lexical_mapping_root.display()
                )
            })?;
        let files = source_files_below(
            inventory,
            &lexical_mapping_root,
            &canonical_mapping_root,
            &mapping.excluded_prefixes,
        )?;
        if files.is_empty() {
            return Err(format!(
                "package manager reports no managed files for materialized source '{}' below '{}'; re-enable the adapter",
                mapping.resource_id,
                lexical_mapping_root.display()
            ));
        }
        materialized_sources.push(MaterializedSourceRevision {
            resource_id: mapping.resource_id.clone(),
            source_root: canonical_mapping_root,
            files,
        });
    }
    materialized_sources.sort();
    if materialized_sources.windows(2).any(|pair| {
        pair[0].resource_id == pair[1].resource_id && pair[0].source_root == pair[1].source_root
    }) {
        return Err(
            "materialized source mappings contain duplicates; re-enable the adapter".into(),
        );
    }
    Ok(AdapterSourceRevision {
        source_root: canonical_root,
        files,
        materialized_sources,
    })
}

/// Verify the current package-owned source bytes against current metadata.
pub fn verify_managed_bundle(revision: &AdapterSourceRevision) -> ManagedMatch {
    if let Err(verdict) = verify_source_files(&revision.source_root, &revision.files) {
        return verdict;
    }
    for source in &revision.materialized_sources {
        if let Err(verdict) = verify_source_files(&source.source_root, &source.files) {
            return verdict;
        }
    }
    ManagedMatch::Matched
}

/// Compare a claim's enable-time revision with the current authoritative one.
pub fn compare_source_revision(
    claim: &AdapterClaim,
    current: &AdapterSourceRevision,
) -> ManagedMatch {
    if let Some(recorded) = &claim.source_revision {
        return if recorded == current {
            ManagedMatch::Matched
        } else {
            ManagedMatch::Changed(
                "adapter source revision changed since enable; re-enable the adapter".into(),
            )
        };
    }

    let Ok(recorded_root) = std::fs::canonicalize(&claim.resource_root) else {
        return ManagedMatch::Unknown(
            "legacy receipt source root is unavailable; re-enable the adapter".into(),
        );
    };
    if recorded_root != current.source_root {
        return ManagedMatch::Changed(
            "adapter source root changed since enable; re-enable the adapter".into(),
        );
    }
    let Some(recorded_digest) = claim.bundle_digest.as_deref() else {
        return ManagedMatch::Unknown(
            "receipt has no managed source revision; re-enable the adapter".into(),
        );
    };
    match legacy_subset_digest(current) {
        Some(digest) if digest == recorded_digest => ManagedMatch::Matched,
        Some(_) => ManagedMatch::Unknown(
            "legacy receipt cannot distinguish an older package revision from enable-time unmanaged files; re-enable the adapter".into(),
        ),
        None => ManagedMatch::Unknown(
            "legacy managed source subset could not be read; re-enable the adapter".into(),
        ),
    }
}

/// Map package-managed source entries to files ANOLISA explicitly copied.
pub fn materialized_files(
    inventory: &ManagedInventory,
    mappings: &[MaterializedMapping],
) -> Result<Vec<MaterializedFile>, String> {
    let mut files = Vec::new();
    for mapping in mappings {
        let lexical_root = normalize_absolute(&mapping.source_root)?;
        let canonical_root = std::fs::canonicalize(&lexical_root).map_err(|err| {
            format!(
                "cannot resolve materialized source root '{}': {err}; re-enable the adapter",
                lexical_root.display()
            )
        })?;
        for file in &inventory.files {
            let path = normalize_absolute(&file.path)?;
            let Some(relative_path) =
                relative_below_root_aliases(&path, &lexical_root, &canonical_root)
            else {
                continue;
            };
            validate_relative(&relative_path)?;
            if mapping
                .excluded_prefixes
                .iter()
                .any(|prefix| relative_path.starts_with(prefix))
            {
                continue;
            }
            let (kind, sha256, symlink_target) = integrity_metadata(file)?;
            files.push(MaterializedFile {
                resource_id: mapping.resource_id.clone(),
                relative_path,
                kind,
                sha256,
                symlink_target,
            });
        }
    }
    files.sort();
    files.dedup();
    if files.windows(2).any(|pair| {
        pair[0].resource_id == pair[1].resource_id && pair[0].relative_path == pair[1].relative_path
    }) {
        return Err("materialized mappings assign conflicting metadata to one path".into());
    }
    Ok(files)
}

/// Copy one declared materialized resource using only its managed receipt entries.
///
/// The source tree is never enumerated here. Runtime-created or otherwise
/// unowned source entries therefore cannot be delivered merely because a
/// framework driver materializes the surrounding directory.
pub(crate) fn copy_materialized_resource(
    claim: &AdapterClaim,
    resource_id: &str,
    source_root: &Path,
    ops: &dyn AdapterOps,
) -> Result<(), AdapterError> {
    let invalid = |reason: String| AdapterError::InvalidAdapterInput {
        component: claim.component.clone(),
        framework: claim.framework.clone(),
        reason,
    };
    let revision = claim.source_revision.as_ref().ok_or_else(|| {
        invalid("receipt has no source revision for materialized copy".to_string())
    })?;
    let source = revision
        .materialized_sources
        .iter()
        .find(|source| source.resource_id == resource_id)
        .ok_or_else(|| {
            invalid(format!(
                "receipt has no materialized source revision for resource '{resource_id}'"
            ))
        })?;
    let canonical_source = std::fs::canonicalize(source_root).map_err(|err| {
        invalid(format!(
            "cannot resolve materialized source root '{}': {err}",
            source_root.display()
        ))
    })?;
    if canonical_source != source.source_root {
        return Err(invalid(format!(
            "materialized source root for resource '{resource_id}' changed before copy"
        )));
    }

    let destination_root = match &claim
        .resource(resource_id)
        .ok_or_else(|| invalid(format!("receipt has no resource '{resource_id}'")))?
        .kind
    {
        ClaimResourceKind::OwnedPath { path } | ClaimResourceKind::ExternalPath { path } => path,
        _ => {
            return Err(invalid(format!(
                "materialized resource '{resource_id}' is not a filesystem root"
            )));
        }
    };
    let materialized = claim
        .materialized_files
        .iter()
        .filter(|file| file.resource_id == resource_id)
        .collect::<Vec<_>>();
    let metadata_matches = materialized.len() == source.files.len()
        && materialized
            .iter()
            .zip(&source.files)
            .all(|(output, input)| {
                output.relative_path == input.relative_path
                    && output.kind == input.kind
                    && output.sha256 == input.sha256
                    && output.symlink_target == input.symlink_target
            });
    if !metadata_matches {
        return Err(invalid(format!(
            "materialized files for resource '{resource_id}' do not match its source revision"
        )));
    }
    if let Some(file) = source
        .files
        .iter()
        .find(|file| file.kind == RevisionFileKind::Symlink)
    {
        return Err(invalid(format!(
            "materialized source '{}' contains managed symlink '{}'; explicit copies do not follow source symlinks",
            source_root.display(),
            file.relative_path.display()
        )));
    }

    for file in &source.files {
        let source_path = source_root.join(&file.relative_path);
        verify_file(
            &source_path,
            file.kind,
            file.sha256.as_deref(),
            file.symlink_target.as_deref(),
        )
        .map_err(|verdict| invalid(managed_reason(verdict)))?;
        let destination_path = destination_root.join(&file.relative_path);
        ops.copy_file(&source_path, &destination_path)?;
        verify_file(
            &destination_path,
            file.kind,
            file.sha256.as_deref(),
            file.symlink_target.as_deref(),
        )
        .map_err(|verdict| invalid(managed_reason(verdict)))?;
    }
    Ok(())
}

/// Remove outputs owned only by a prior receipt before replacing it.
///
/// Exact-entry removal deliberately leaves unrelated files in materialized
/// destination directories intact. When a former directory prefix must
/// become a file, only an empty prefix directory is removed; runtime content
/// makes cleanup fail while the prior receipt is still durable.
pub(crate) fn cleanup_replaced_materialized_files(
    prior: &AdapterClaim,
    next: &AdapterClaim,
    ops: &dyn AdapterOps,
) -> Result<Vec<String>, AdapterError> {
    let prior_paths = materialized_paths(prior)?;
    let next_paths = materialized_paths(next)?;
    let next_set = next_paths
        .iter()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    let mut stale = prior_paths
        .iter()
        .filter(|path| !next_set.contains(*path))
        .cloned()
        .collect::<Vec<_>>();
    stale.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    stale.dedup();

    let mut messages = Vec::new();
    for path in &stale {
        if ops.remove_path(path)? {
            messages.push(format!(
                "removed stale materialized file {}",
                path.display()
            ));
        }
    }

    // Remove every empty ancestor below a file-shaped replacement. Stopping
    // at the first non-empty directory preserves runtime-created content.
    for path in replacement_prefixes(&stale, &next_paths) {
        if ops.remove_path(&path)? {
            messages.push(format!(
                "removed empty stale materialized directory {}",
                path.display()
            ));
        }
    }

    Ok(messages)
}

/// Describe generic materialized-file cleanup for a re-enable dry-run.
pub(crate) fn plan_replaced_materialized_files(
    prior: &AdapterClaim,
    next_files: &[MaterializedFile],
    next_roots: &BTreeMap<String, PathBuf>,
) -> Result<Vec<String>, AdapterError> {
    let next_paths = next_files
        .iter()
        .map(|file| {
            validate_relative(&file.relative_path)
                .map_err(|reason| invalid_materialized(prior, reason))?;
            let root = next_roots.get(&file.resource_id).ok_or_else(|| {
                invalid_materialized(
                    prior,
                    format!(
                        "next materialized resource '{}' has no destination root",
                        file.resource_id
                    ),
                )
            })?;
            Ok(root.join(&file.relative_path))
        })
        .collect::<Result<Vec<_>, AdapterError>>()?;
    let next_set = next_paths.iter().collect::<std::collections::BTreeSet<_>>();

    let mut stale = prior
        .materialized_files
        .iter()
        .map(|file| materialized_path(prior, file))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|path| !next_set.contains(path))
        .collect::<Vec<_>>();
    stale.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    stale.dedup();

    let mut actions = stale
        .iter()
        .map(|path| format!("remove stale materialized file {}", path.display()))
        .collect::<Vec<_>>();
    actions.extend(
        replacement_prefixes(&stale, &next_paths)
            .into_iter()
            .map(|path| {
                format!(
                    "remove empty stale materialized directory {}",
                    path.display()
                )
            }),
    );
    Ok(actions)
}

fn materialized_paths(claim: &AdapterClaim) -> Result<Vec<PathBuf>, AdapterError> {
    claim
        .materialized_files
        .iter()
        .map(|file| materialized_path(claim, file))
        .collect()
}

fn materialized_path(
    claim: &AdapterClaim,
    file: &MaterializedFile,
) -> Result<PathBuf, AdapterError> {
    validate_relative(&file.relative_path).map_err(|reason| invalid_materialized(claim, reason))?;
    let resource = claim.resource(&file.resource_id).ok_or_else(|| {
        invalid_materialized(
            claim,
            format!(
                "materialized resource '{}' is missing from the receipt",
                file.resource_id
            ),
        )
    })?;
    let root = match &resource.kind {
        ClaimResourceKind::OwnedPath { path } | ClaimResourceKind::ExternalPath { path } => path,
        _ => {
            return Err(invalid_materialized(
                claim,
                format!(
                    "materialized resource '{}' is not a filesystem root",
                    file.resource_id
                ),
            ));
        }
    };
    Ok(root.join(&file.relative_path))
}

fn invalid_materialized(claim: &AdapterClaim, reason: String) -> AdapterError {
    AdapterError::InvalidAdapterInput {
        component: claim.component.clone(),
        framework: claim.framework.clone(),
        reason,
    }
}

fn replacement_prefixes(stale: &[PathBuf], next: &[PathBuf]) -> Vec<PathBuf> {
    let mut prefixes = Vec::new();
    for stale_path in stale {
        for next_path in next {
            if !stale_path.starts_with(next_path) {
                continue;
            }
            let mut ancestor = stale_path.parent();
            while let Some(path) = ancestor.filter(|path| path.starts_with(next_path)) {
                prefixes.push(path.to_path_buf());
                if path == next_path {
                    break;
                }
                ancestor = path.parent();
            }
        }
    }
    prefixes.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    prefixes.dedup();
    prefixes
}

/// Verify recorded materialized files without scanning for extra entries.
pub fn verify_materialized_bundle(claim: &AdapterClaim) -> ManagedMatch {
    for file in &claim.materialized_files {
        let Some(resource) = claim.resource(&file.resource_id) else {
            return ManagedMatch::Unknown(format!(
                "materialized resource '{}' is missing from the receipt; re-enable the adapter",
                file.resource_id
            ));
        };
        let root = match &resource.kind {
            ClaimResourceKind::OwnedPath { path } | ClaimResourceKind::ExternalPath { path } => {
                path
            }
            _ => {
                return ManagedMatch::Unknown(format!(
                    "materialized resource '{}' is not a filesystem root; re-enable the adapter",
                    file.resource_id
                ));
            }
        };
        let path = root.join(&file.relative_path);
        if let Err(verdict) = verify_file(
            &path,
            file.kind,
            file.sha256.as_deref(),
            file.symlink_target.as_deref(),
        ) {
            return verdict;
        }
    }
    ManagedMatch::Matched
}

fn verify_file(
    path: &Path,
    kind: RevisionFileKind,
    sha256: Option<&str>,
    symlink_target: Option<&Path>,
) -> Result<(), ManagedMatch> {
    let metadata =
        std::fs::symlink_metadata(path).map_err(|err| io_verdict(path, "inspect", err))?;
    match kind {
        RevisionFileKind::File => {
            if !metadata.file_type().is_file() {
                return Err(ManagedMatch::Changed(format!(
                    "managed file '{}' changed type",
                    path.display()
                )));
            }
            let expected = sha256.ok_or_else(|| {
                ManagedMatch::Unknown(format!(
                    "managed file '{}' has no recorded SHA-256; re-enable the adapter",
                    path.display()
                ))
            })?;
            let bytes = std::fs::read(path).map_err(|err| io_verdict(path, "read", err))?;
            let actual = format!("{:x}", Sha256::digest(bytes));
            if actual != expected {
                return Err(ManagedMatch::Changed(format!(
                    "managed file '{}' content changed",
                    path.display()
                )));
            }
        }
        RevisionFileKind::Symlink => {
            if !metadata.file_type().is_symlink() {
                return Err(ManagedMatch::Changed(format!(
                    "managed symlink '{}' changed type",
                    path.display()
                )));
            }
            let expected = symlink_target.ok_or_else(|| {
                ManagedMatch::Unknown(format!(
                    "managed symlink '{}' has no recorded target; re-enable the adapter",
                    path.display()
                ))
            })?;
            let actual = std::fs::read_link(path).map_err(|err| io_verdict(path, "read", err))?;
            if actual != expected {
                return Err(ManagedMatch::Changed(format!(
                    "managed symlink '{}' target changed",
                    path.display()
                )));
            }
        }
    }
    Ok(())
}

fn verify_source_files(
    source_root: &Path,
    files: &[ManagedSourceFile],
) -> Result<(), ManagedMatch> {
    for file in files {
        let path = source_root.join(&file.relative_path);
        verify_file(
            &path,
            file.kind,
            file.sha256.as_deref(),
            file.symlink_target.as_deref(),
        )?;
    }
    Ok(())
}

fn source_files_below(
    inventory: &ManagedInventory,
    lexical_root: &Path,
    canonical_root: &Path,
    excluded_prefixes: &[PathBuf],
) -> Result<Vec<ManagedSourceFile>, String> {
    let mut files = Vec::new();
    for file in &inventory.files {
        let path = normalize_absolute(&file.path)?;
        let Some(relative_path) = relative_below_root_aliases(&path, lexical_root, canonical_root)
        else {
            continue;
        };
        validate_relative(&relative_path)?;
        if excluded_prefixes
            .iter()
            .any(|prefix| relative_path.starts_with(prefix))
        {
            continue;
        }
        let (kind, sha256, symlink_target) = integrity_metadata(file)?;
        files.push(ManagedSourceFile {
            relative_path,
            kind,
            sha256,
            symlink_target,
        });
    }
    files.sort();
    files.dedup();
    if files
        .windows(2)
        .any(|pair| pair[0].relative_path == pair[1].relative_path)
    {
        return Err(
            "package inventory assigns conflicting metadata to one adapter path; re-enable the adapter"
                .into(),
        );
    }
    Ok(files)
}

fn relative_below_root_aliases(
    path: &Path,
    lexical_root: &Path,
    canonical_root: &Path,
) -> Option<PathBuf> {
    // Do not canonicalize the inventory entry itself: doing so would follow a
    // managed leaf symlink and discard the path whose literal target we verify.
    if path == lexical_root || path == canonical_root {
        return None;
    }
    path.strip_prefix(lexical_root)
        .or_else(|_| path.strip_prefix(canonical_root))
        .ok()
        .map(Path::to_path_buf)
}

fn io_verdict(path: &Path, action: &str, err: std::io::Error) -> ManagedMatch {
    if err.kind() == std::io::ErrorKind::NotFound {
        ManagedMatch::Changed(format!("managed file '{}' is missing", path.display()))
    } else {
        ManagedMatch::Unknown(format!(
            "managed file '{}' could not be {action}: {err}; re-enable the adapter",
            path.display()
        ))
    }
}

fn managed_reason(verdict: ManagedMatch) -> String {
    match verdict {
        ManagedMatch::Matched => "managed file verification unexpectedly returned matched".into(),
        ManagedMatch::Changed(reason) | ManagedMatch::Unknown(reason) => reason,
    }
}

fn legacy_subset_digest(revision: &AdapterSourceRevision) -> Option<String> {
    let mut hasher = Sha256::new();
    for file in &revision.files {
        let path = revision.source_root.join(&file.relative_path);
        let bytes = std::fs::read(path).ok()?;
        hasher.update(file.relative_path.to_string_lossy().as_bytes());
        hasher.update([0]);
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update([0]);
        hasher.update(bytes);
    }
    Some(format!("sha256:{:x}", hasher.finalize()))
}

fn normalize_sha256(value: &str) -> Option<String> {
    let value = value.strip_prefix("sha256:").unwrap_or(value);
    (value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| value.to_ascii_lowercase())
}

fn integrity_metadata(
    file: &ManagedFile,
) -> Result<(RevisionFileKind, Option<String>, Option<PathBuf>), String> {
    match file.kind {
        ManagedInventoryKind::File => {
            let digest = file.sha256.as_deref().ok_or_else(|| {
                format!(
                    "managed file '{}' has no SHA-256 digest; re-enable the adapter",
                    file.path.display()
                )
            })?;
            let digest = normalize_sha256(digest).ok_or_else(|| {
                format!(
                    "managed file '{}' has an invalid SHA-256 digest; re-enable the adapter",
                    file.path.display()
                )
            })?;
            Ok((RevisionFileKind::File, Some(digest), None))
        }
        ManagedInventoryKind::Symlink => {
            let target = file.symlink_target.clone().ok_or_else(|| {
                format!(
                    "managed symlink '{}' has no target; re-enable the adapter",
                    file.path.display()
                )
            })?;
            Ok((RevisionFileKind::Symlink, None, Some(target)))
        }
        ManagedInventoryKind::Unsupported => Err(format!(
            "managed path '{}' has an unsupported package file type; re-enable the adapter",
            file.path.display()
        )),
    }
}

fn normalize_absolute(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err(format!("managed path '{}' is not absolute", path.display()));
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir | Component::Prefix(_) | Component::Normal(_) => {
                normalized.push(component.as_os_str())
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err(format!(
                        "managed path '{}' escapes its root",
                        path.display()
                    ));
                }
            }
        }
    }
    Ok(normalized)
}

fn validate_relative(path: &Path) -> Result<(), String> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(format!(
            "managed relative path '{}' is invalid; re-enable the adapter",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use anolisa_platform::pkg_files::{PackageFile, PackageFileInventory};
    use anolisa_platform::pkg_query::PackageQueryError;

    use crate::adapter::claim::{
        CLAIM_SCHEMA_VERSION, ClaimResource, ClaimStatus, CoshClaim, DRIVER_SCHEMA_VERSION,
        DriverPayload,
    };
    use crate::domain::{
        InstallationScope, LifecycleStatus, ManagementRelation, NativePm, OwnedArtifact,
    };
    use crate::state::{ObjectKind, OwnedFile};

    struct FileQuery(Result<PackageFileInventory, &'static str>);

    impl PackageFileQuery for FileQuery {
        fn query_file_inventory(
            &self,
            _package: &str,
        ) -> Result<PackageFileInventory, PackageQueryError> {
            self.0
                .clone()
                .map_err(|detail| PackageQueryError::QueryFailed {
                    command: "rpm".into(),
                    code: Some(1),
                    stderr: detail.into(),
                })
        }
    }

    fn sha256(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    fn legacy_tree_digest(root: &Path, paths: &[&str]) -> String {
        let mut paths = paths.to_vec();
        paths.sort_unstable();
        let mut hasher = Sha256::new();
        for relative in paths {
            let bytes = std::fs::read(root.join(relative)).expect("legacy digest input");
            hasher.update(relative.as_bytes());
            hasher.update([0]);
            hasher.update((bytes.len() as u64).to_le_bytes());
            hasher.update([0]);
            hasher.update(bytes);
        }
        format!("sha256:{:x}", hasher.finalize())
    }

    fn owned_file(path: PathBuf, bytes: &[u8]) -> OwnedFile {
        OwnedFile {
            path,
            owner: FileOwner::Anolisa,
            sha256: Some(sha256(bytes)),
            kind: OwnedFileKind::File,
            referent: None,
            mode: None,
            capabilities: Vec::new(),
        }
    }

    fn raw_installation(files: Vec<OwnedFile>) -> Installation {
        Installation {
            kind: ObjectKind::Component,
            name: "tokenless".into(),
            scope: InstallationScope::System,
            binding: ProviderBinding::Owned {
                artifact: OwnedArtifact {
                    version: "1.0.0".into(),
                    distribution_source: None,
                    raw_package: None,
                    manifest_digest: None,
                    files,
                    services: Vec::new(),
                    external_modified_files: Vec::new(),
                    provisioned_packages: Vec::new(),
                },
            },
            status: LifecycleStatus::Installed,
            installed_at: "2026-08-01T00:00:00Z".into(),
            last_operation_id: None,
            subscription_scope: Default::default(),
            enabled_features: Vec::new(),
            health: Vec::new(),
        }
    }

    fn rpm_installation() -> Installation {
        Installation {
            kind: ObjectKind::Component,
            name: "tokenless".into(),
            scope: InstallationScope::System,
            binding: ProviderBinding::Delegated {
                pm: NativePm::Rpm,
                package: PackageIdentity::Resolved {
                    name: "tokenless".into(),
                },
                relation: ManagementRelation::Managed {
                    since: "2026-08-01T00:00:00Z".into(),
                },
                last_observed: None,
            },
            status: LifecycleStatus::Installed,
            installed_at: "2026-08-01T00:00:00Z".into(),
            last_operation_id: None,
            subscription_scope: Default::default(),
            enabled_features: Vec::new(),
            health: Vec::new(),
        }
    }

    fn claim(root: &Path) -> AdapterClaim {
        AdapterClaim {
            claim_schema: CLAIM_SCHEMA_VERSION,
            component: "tokenless".into(),
            framework: "cosh".into(),
            plugin_id: None,
            adapter_type: Some("extension".into()),
            enabled_at: "2026-08-01T01:00:00Z".into(),
            resource_root: root.to_path_buf(),
            bundle_digest: None,
            source_revision: None,
            materialized_files: Vec::new(),
            driver_schema: DRIVER_SCHEMA_VERSION,
            status: ClaimStatus::Enabled,
            notices: Vec::new(),
            resources: vec![ClaimResource {
                id: "copy".into(),
                purpose: "test copy".into(),
                kind: ClaimResourceKind::OwnedPath {
                    path: root.to_path_buf(),
                },
            }],
            driver_payload: DriverPayload::Cosh(CoshClaim {
                extension_dir_resource: "copy".into(),
            }),
        }
    }

    #[test]
    fn dry_run_cleanup_compares_concrete_destination_roots() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let old_root = tmp.path().join("plugins/old-id");
        let new_root = tmp.path().join("plugins/new-id");
        let file = MaterializedFile {
            resource_id: "copy".into(),
            relative_path: PathBuf::from("hook.py"),
            kind: RevisionFileKind::File,
            sha256: Some(sha256(b"managed")),
            symlink_target: None,
        };
        let mut prior = claim(&old_root);
        prior.materialized_files = vec![file.clone()];
        let next_files = vec![file];
        let next_roots = BTreeMap::from([("copy".to_string(), new_root)]);

        let actions = plan_replaced_materialized_files(&prior, &next_files, &next_roots)
            .expect("plan replacement cleanup");

        assert_eq!(
            actions,
            vec![format!(
                "remove stale materialized file {}",
                old_root.join("hook.py").display()
            )]
        );
    }

    #[test]
    fn raw_revision_ignores_unmanaged_files_but_detects_managed_changes() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let root = tmp.path().join("adapter");
        std::fs::create_dir_all(root.join("__pycache__")).expect("adapter dirs");
        let managed = root.join("hook.py");
        let runtime = root.join("__pycache__/hook.pyc");
        let external = root.join("operator.log");
        std::fs::write(&managed, b"managed").expect("managed file");
        std::fs::write(&runtime, b"bytecode").expect("runtime file");
        std::fs::write(&external, b"log").expect("external file");
        let mut external_record = owned_file(external, b"log");
        external_record.owner = FileOwner::External;
        let installation = raw_installation(vec![
            owned_file(managed.clone(), b"managed"),
            external_record,
        ]);

        let inventory =
            inventory_for_installation(&installation, &FileQuery(Err("raw must not query rpm")))
                .expect("raw inventory");
        let revision = source_revision(&inventory, &root, &[]).expect("source revision");
        assert_eq!(revision.files.len(), 1);
        assert_eq!(verify_managed_bundle(&revision), ManagedMatch::Matched);

        std::fs::write(&managed, b"changed").expect("mutate managed file");
        assert!(matches!(
            verify_managed_bundle(&revision),
            ManagedMatch::Changed(_)
        ));
        std::fs::remove_file(&managed).expect("remove managed file");
        assert!(matches!(
            verify_managed_bundle(&revision),
            ManagedMatch::Changed(reason) if reason.contains("missing")
        ));
    }

    #[test]
    fn revision_tracks_inventory_additions_and_canonical_root_migration() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let first_root = tmp.path().join("first");
        let second_root = tmp.path().join("second");
        std::fs::create_dir_all(&first_root).expect("first root");
        std::fs::create_dir_all(&second_root).expect("second root");
        std::fs::write(first_root.join("a"), b"a").expect("first file");
        std::fs::write(first_root.join("b"), b"b").expect("second file");
        std::fs::write(second_root.join("a"), b"a").expect("migrated file");
        let one = ManagedInventory {
            files: vec![ManagedFile {
                path: first_root.join("a"),
                kind: ManagedInventoryKind::File,
                sha256: Some(sha256(b"a")),
                symlink_target: None,
            }],
        };
        let mut two = one.clone();
        two.files.push(ManagedFile {
            path: first_root.join("b"),
            kind: ManagedInventoryKind::File,
            sha256: Some(sha256(b"b")),
            symlink_target: None,
        });
        let recorded = source_revision(&one, &first_root, &[]).expect("recorded revision");
        let mut enabled = claim(&first_root);
        enabled.source_revision = Some(recorded);

        let added = source_revision(&two, &first_root, &[]).expect("added-file revision");
        assert!(matches!(
            compare_source_revision(&enabled, &added),
            ManagedMatch::Changed(_)
        ));

        let migrated = ManagedInventory {
            files: vec![ManagedFile {
                path: second_root.join("a"),
                kind: ManagedInventoryKind::File,
                sha256: Some(sha256(b"a")),
                symlink_target: None,
            }],
        };
        let migrated = source_revision(&migrated, &second_root, &[]).expect("migrated revision");
        assert!(matches!(
            compare_source_revision(&enabled, &migrated),
            ManagedMatch::Changed(_)
        ));
    }

    #[test]
    fn revision_includes_declared_copy_sources_outside_the_adapter_root() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let adapter_root = tmp.path().join("adapters/tokenless/openclaw");
        let skill_root = tmp.path().join("skills/sec-audit");
        std::fs::create_dir_all(&adapter_root).expect("adapter root");
        std::fs::create_dir_all(&skill_root).expect("skill root");
        std::fs::write(adapter_root.join("plugin.json"), b"plugin").expect("plugin");
        std::fs::write(skill_root.join("SKILL.md"), b"skill-v1").expect("skill");
        let inventory = ManagedInventory {
            files: vec![
                ManagedFile {
                    path: adapter_root.join("plugin.json"),
                    kind: ManagedInventoryKind::File,
                    sha256: Some(sha256(b"plugin")),
                    symlink_target: None,
                },
                ManagedFile {
                    path: skill_root.join("SKILL.md"),
                    kind: ManagedInventoryKind::File,
                    sha256: Some(sha256(b"skill-v1")),
                    symlink_target: None,
                },
            ],
        };
        let mappings = vec![MaterializedMapping {
            resource_id: "openclaw_skill_sec-audit".into(),
            source_root: skill_root.clone(),
            excluded_prefixes: Vec::new(),
        }];
        let recorded =
            source_revision(&inventory, &adapter_root, &mappings).expect("recorded revision");
        assert_eq!(recorded.materialized_sources.len(), 1);
        assert_eq!(verify_managed_bundle(&recorded), ManagedMatch::Matched);

        let mut claim = claim(&adapter_root);
        claim.source_revision = Some(recorded.clone());
        std::fs::write(skill_root.join("SKILL.md"), b"skill-v2").expect("changed skill");
        assert!(matches!(
            verify_managed_bundle(&recorded),
            ManagedMatch::Changed(_)
        ));

        let mut updated_inventory = inventory;
        updated_inventory.files[1].sha256 = Some(sha256(b"skill-v2"));
        let updated = source_revision(&updated_inventory, &adapter_root, &mappings)
            .expect("updated revision");
        assert!(matches!(
            compare_source_revision(&claim, &updated),
            ManagedMatch::Changed(_)
        ));
    }

    #[test]
    fn legacy_subset_ignores_later_runtime_files_and_keeps_ambiguity_explicit() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let root = tmp.path().join("adapter");
        std::fs::create_dir_all(&root).expect("adapter root");
        let managed = root.join("hook.py");
        std::fs::write(&managed, b"managed").expect("managed file");
        let inventory = ManagedInventory {
            files: vec![ManagedFile {
                path: managed,
                kind: ManagedInventoryKind::File,
                sha256: Some(sha256(b"managed")),
                symlink_target: None,
            }],
        };
        let revision = source_revision(&inventory, &root, &[]).expect("revision");
        let mut legacy = claim(&root);
        legacy.claim_schema = 1;
        legacy.bundle_digest = legacy_subset_digest(&revision);
        std::fs::write(root.join("runtime.pyc"), b"runtime").expect("runtime file");

        assert_eq!(
            compare_source_revision(&legacy, &revision),
            ManagedMatch::Matched
        );
        legacy.bundle_digest = Some(legacy_tree_digest(&root, &["hook.py", "runtime.pyc"]));
        assert!(matches!(
            compare_source_revision(&legacy, &revision),
            ManagedMatch::Unknown(reason) if reason.contains("legacy receipt")
        ));
    }

    #[test]
    fn materialized_verification_ignores_extras_but_checks_recorded_files() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let source = tmp.path().join("source");
        let destination = tmp.path().join("destination");
        std::fs::create_dir_all(&source).expect("source root");
        std::fs::create_dir_all(&destination).expect("destination root");
        std::fs::write(source.join("hook.py"), b"managed").expect("source file");
        std::fs::write(destination.join("hook.py"), b"managed").expect("copied file");
        std::fs::write(destination.join("runtime.log"), b"runtime").expect("runtime file");
        let inventory = ManagedInventory {
            files: vec![ManagedFile {
                path: source.join("hook.py"),
                kind: ManagedInventoryKind::File,
                sha256: Some(sha256(b"managed")),
                symlink_target: None,
            }],
        };
        let mut receipt = claim(&destination);
        receipt.materialized_files = materialized_files(
            &inventory,
            &[MaterializedMapping {
                resource_id: "copy".into(),
                source_root: source,
                excluded_prefixes: Vec::new(),
            }],
        )
        .expect("materialized inventory");

        assert_eq!(verify_materialized_bundle(&receipt), ManagedMatch::Matched);
        std::fs::write(destination.join("hook.py"), b"changed").expect("mutate copy");
        assert!(matches!(
            verify_materialized_bundle(&receipt),
            ManagedMatch::Changed(_)
        ));
        std::fs::remove_file(destination.join("hook.py")).expect("remove copy");
        assert!(matches!(
            verify_materialized_bundle(&receipt),
            ManagedMatch::Changed(reason) if reason.contains("missing")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn managed_symlink_uses_literal_target() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().expect("tmpdir");
        let root = tmp.path().join("adapter");
        std::fs::create_dir_all(&root).expect("adapter root");
        symlink("first.py", root.join("current.py")).expect("managed symlink");
        let revision = AdapterSourceRevision {
            source_root: root.clone(),
            files: vec![ManagedSourceFile {
                relative_path: PathBuf::from("current.py"),
                kind: RevisionFileKind::Symlink,
                sha256: None,
                symlink_target: Some(PathBuf::from("first.py")),
            }],
            materialized_sources: Vec::new(),
        };
        assert_eq!(verify_managed_bundle(&revision), ManagedMatch::Matched);
        std::fs::remove_file(root.join("current.py")).expect("remove symlink");
        symlink("second.py", root.join("current.py")).expect("replace symlink");
        assert!(matches!(
            verify_managed_bundle(&revision),
            ManagedMatch::Changed(reason) if reason.contains("target changed")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_source_root_scopes_canonical_package_files() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().expect("tmpdir");
        let release_root = tmp.path().join("releases/v2");
        let lexical_root = tmp.path().join("current");
        std::fs::create_dir_all(&release_root).expect("release root");
        std::fs::write(release_root.join("hook.py"), b"managed").expect("managed file");
        symlink("releases/v2", &lexical_root).expect("source root symlink");
        let inventory = ManagedInventory {
            files: vec![
                ManagedFile {
                    path: lexical_root.clone(),
                    kind: ManagedInventoryKind::Symlink,
                    sha256: None,
                    symlink_target: Some(PathBuf::from("releases/v2")),
                },
                ManagedFile {
                    path: release_root.join("hook.py"),
                    kind: ManagedInventoryKind::File,
                    sha256: Some(sha256(b"managed")),
                    symlink_target: None,
                },
            ],
        };

        let revision = source_revision(&inventory, &lexical_root, &[]).expect("source revision");
        assert_eq!(revision.source_root, release_root);
        assert_eq!(revision.files.len(), 1);
        assert_eq!(revision.files[0].relative_path, PathBuf::from("hook.py"));
        assert_eq!(verify_managed_bundle(&revision), ManagedMatch::Matched);

        let copied = materialized_files(
            &inventory,
            &[MaterializedMapping {
                resource_id: "copy".into(),
                source_root: lexical_root,
                excluded_prefixes: Vec::new(),
            }],
        )
        .expect("materialized files");
        assert_eq!(copied.len(), 1);
        assert_eq!(copied[0].relative_path, PathBuf::from("hook.py"));
    }

    #[test]
    fn rpm_inventory_is_authoritative_and_rejects_unverifiable_metadata() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let path = tmp.path().join("adapter.py");
        std::fs::write(&path, b"adapter").expect("managed adapter file");
        std::fs::write(tmp.path().join("runtime.pyc"), b"runtime").expect("runtime file");
        let inventory = PackageFileInventory {
            digest_algorithm: PackageFileDigestAlgorithm::Sha256,
            files: vec![PackageFile {
                path: path.display().to_string(),
                kind: PackageFileKind::Regular,
                digest: Some(sha256(b"adapter")),
                link_target: None,
            }],
        };
        let managed = inventory_for_installation(&rpm_installation(), &FileQuery(Ok(inventory)))
            .expect("rpm inventory");
        assert_eq!(managed.files.len(), 1);
        let revision = source_revision(&managed, tmp.path(), &[]).expect("rpm revision");
        assert_eq!(verify_managed_bundle(&revision), ManagedMatch::Matched);
        std::fs::write(&path, b"changed").expect("mutate managed adapter file");
        assert!(matches!(
            verify_managed_bundle(&revision),
            ManagedMatch::Changed(_)
        ));
        std::fs::write(&path, b"adapter").expect("restore managed adapter file");

        let empty_digest = PackageFileInventory {
            digest_algorithm: PackageFileDigestAlgorithm::Sha256,
            files: vec![PackageFile {
                path: path.display().to_string(),
                kind: PackageFileKind::Regular,
                digest: None,
                link_target: None,
            }],
        };
        let managed = inventory_for_installation(&rpm_installation(), &FileQuery(Ok(empty_digest)))
            .expect("the package inventory remains queryable");
        assert!(
            source_revision(&managed, tmp.path(), &[])
                .expect_err("an adapter file without a digest is unverifiable")
                .contains("no SHA-256")
        );

        let unsupported = PackageFileInventory {
            digest_algorithm: PackageFileDigestAlgorithm::Unsupported(1),
            files: Vec::new(),
        };
        assert!(
            inventory_for_installation(&rpm_installation(), &FileQuery(Ok(unsupported)),)
                .expect_err("unsupported digest must remain unknown")
                .contains("unsupported")
        );
        assert!(
            inventory_for_installation(&rpm_installation(), &FileQuery(Err("rpmdb unavailable")),)
                .expect_err("query failure must remain unknown")
                .contains("query failed")
        );
    }

    #[test]
    fn rpm_inventory_rejects_scoped_unsupported_file_types() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let root = tmp.path().join("adapter");
        std::fs::create_dir_all(&root).expect("adapter root");
        let managed_path = root.join("hook.py");
        std::fs::write(&managed_path, b"adapter").expect("managed file");
        let inventory = PackageFileInventory {
            digest_algorithm: PackageFileDigestAlgorithm::Sha256,
            files: vec![
                PackageFile {
                    path: managed_path.display().to_string(),
                    kind: PackageFileKind::Regular,
                    digest: Some(sha256(b"adapter")),
                    link_target: None,
                },
                PackageFile {
                    path: root.join("control.fifo").display().to_string(),
                    kind: PackageFileKind::Other,
                    digest: None,
                    link_target: None,
                },
                PackageFile {
                    path: tmp.path().join("unrelated.sock").display().to_string(),
                    kind: PackageFileKind::Other,
                    digest: None,
                    link_target: None,
                },
            ],
        };
        let mut managed =
            inventory_for_installation(&rpm_installation(), &FileQuery(Ok(inventory)))
                .expect("rpm inventory");

        assert!(
            source_revision(&managed, &root, &[])
                .expect_err("unsupported adapter input must be unverifiable")
                .contains("unsupported package file type")
        );
        managed
            .files
            .retain(|file| file.path != root.join("control.fifo"));
        let revision = source_revision(&managed, &root, &[])
            .expect("unrelated unsupported entries do not affect the adapter");
        assert_eq!(revision.files.len(), 1);
    }
}
