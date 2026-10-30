//! Pure lexical normalization for structured file-protection policies.

use std::fmt;

use agentsight_enforcement_protocol::{
    FileProtectionExclusionKind, FileProtectionPolicy, FileProtectionTarget,
    FileProtectionTargetKind, FileProtectionTopologyMode,
};

const MAX_STRUCTURED_TARGETS: usize = 64;

/// Deterministic structured policy retained for later classification and lowering.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct NormalizedFileProtectionPolicy {
    /// Topology semantics retained without performing topology discovery.
    pub(super) topology_mode: FileProtectionTopologyMode,
    /// Targets sorted by lexical path and target kind.
    pub(super) targets: Vec<NormalizedFileProtectionTarget>,
}

/// One validated target with merged protections and normalized exclusions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct NormalizedFileProtectionTarget {
    /// Original policy path after lexical validation.
    pub(super) path: String,
    /// Scope authored for this target.
    pub(super) kind: FileProtectionTargetKind,
    /// Whether deletion protection was requested by any identical target.
    pub(super) delete: bool,
    /// Whether ordinary-write protection was requested by any identical target.
    pub(super) ordinary_write: bool,
    /// Deduplicated exclusions sorted by lexical path and exclusion kind.
    pub(super) exclusions: Vec<NormalizedFileProtectionExclusion>,
}

/// One validated exclusion scoped to its owning directory target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct NormalizedFileProtectionExclusion {
    /// Original policy path after lexical validation.
    pub(super) path: String,
    /// Scope authored for this exclusion.
    pub(super) kind: FileProtectionExclusionKind,
}

/// Contextual failure produced without consulting the filesystem.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FileProtectionNormalizationError {
    /// Zero-based target position in the wire payload.
    pub(super) target_index: usize,
    /// Zero-based exclusion position when the failure belongs to an exclusion.
    pub(super) exclusion_index: Option<usize>,
    /// Escaped policy-authored path safe for diagnostic output.
    pub(super) path: String,
    reason: String,
}

impl fmt::Display for FileProtectionNormalizationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.exclusion_index {
            Some(exclusion_index) => write!(
                formatter,
                "file protection target {} exclusion {} path {}: {}",
                self.target_index, exclusion_index, self.path, self.reason
            ),
            None => write!(
                formatter,
                "file protection target {} path {}: {}",
                self.target_index, self.path, self.reason
            ),
        }
    }
}

impl std::error::Error for FileProtectionNormalizationError {}

struct IndexedTarget {
    source_index: usize,
    target: NormalizedFileProtectionTarget,
}

/// Validates lexical paths and resolves only same-priority duplicate declarations.
///
/// # Errors
///
/// Returns contextual target or exclusion errors for invalid paths, scopes, or
/// same-priority conflicts.
pub(super) fn normalize(
    policy: &FileProtectionPolicy,
) -> Result<NormalizedFileProtectionPolicy, FileProtectionNormalizationError> {
    let mut targets: Vec<IndexedTarget> = Vec::new();
    for (target_index, target) in policy.targets.iter().enumerate() {
        let normalized = normalize_target(target_index, target)?;
        if let Some(existing) = targets
            .iter_mut()
            .find(|candidate| candidate.target.path == normalized.path)
        {
            if existing.target.kind != normalized.kind {
                return Err(target_error(
                    target_index,
                    &target.path,
                    format!(
                        "same-priority conflict with target {} using a different target kind",
                        existing.source_index
                    ),
                ));
            }
            if existing.target.exclusions != normalized.exclusions {
                return Err(target_error(
                    target_index,
                    &target.path,
                    format!(
                        "same-priority conflict with target {} using different normalized exclusions",
                        existing.source_index
                    ),
                ));
            }
            existing.target.delete |= normalized.delete;
            existing.target.ordinary_write |= normalized.ordinary_write;
        } else {
            if targets.len() == MAX_STRUCTURED_TARGETS {
                return Err(target_error(
                    target_index,
                    &target.path,
                    format!(
                        "structured file protection supports at most {MAX_STRUCTURED_TARGETS} normalized targets"
                    ),
                ));
            }
            targets.push(IndexedTarget {
                source_index: target_index,
                target: normalized,
            });
        }
    }
    let mut targets: Vec<_> = targets.into_iter().map(|entry| entry.target).collect();
    targets.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| target_kind_rank(left.kind).cmp(&target_kind_rank(right.kind)))
    });
    Ok(NormalizedFileProtectionPolicy {
        topology_mode: policy.topology_mode,
        targets,
    })
}

fn normalize_target(
    target_index: usize,
    target: &FileProtectionTarget,
) -> Result<NormalizedFileProtectionTarget, FileProtectionNormalizationError> {
    validate_path(&target.path)
        .map_err(|reason| target_error(target_index, &target.path, reason))?;
    if target.kind == FileProtectionTargetKind::ExactFile
        && let Some(exclusion) = target.exclusions.first()
    {
        return Err(exclusion_error(
            target_index,
            0,
            &exclusion.path,
            format!(
                "exact-file target path {:?} cannot have exclusions",
                target.path
            ),
        ));
    }

    let mut exclusions: Vec<(usize, NormalizedFileProtectionExclusion)> = Vec::new();
    for (exclusion_index, exclusion) in target.exclusions.iter().enumerate() {
        validate_path(&exclusion.path).map_err(|reason| {
            exclusion_error(target_index, exclusion_index, &exclusion.path, reason)
        })?;
        if !is_strict_descendant(&target.path, &exclusion.path) {
            return Err(exclusion_error(
                target_index,
                exclusion_index,
                &exclusion.path,
                format!(
                    "must be a strict descendant of target path {:?}",
                    target.path
                ),
            ));
        }
        if let Some((first_index, existing)) = exclusions
            .iter()
            .find(|(_, candidate)| candidate.path == exclusion.path)
        {
            if existing.kind != exclusion.kind {
                return Err(exclusion_error(
                    target_index,
                    exclusion_index,
                    &exclusion.path,
                    format!(
                        "same-priority conflict with exclusion {first_index} using a different exclusion kind"
                    ),
                ));
            }
            continue;
        }
        exclusions.push((
            exclusion_index,
            NormalizedFileProtectionExclusion {
                path: exclusion.path.clone(),
                kind: exclusion.kind,
            },
        ));
    }
    let mut exclusions: Vec<_> = exclusions
        .into_iter()
        .map(|(_, exclusion)| exclusion)
        .collect();
    exclusions.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| exclusion_kind_rank(left.kind).cmp(&exclusion_kind_rank(right.kind)))
    });
    Ok(NormalizedFileProtectionTarget {
        path: target.path.clone(),
        kind: target.kind,
        delete: target.protections.delete,
        ordinary_write: target.protections.ordinary_write,
        exclusions,
    })
}

fn validate_path(path: &str) -> Result<(), &'static str> {
    if path.is_empty() {
        return Err("path must not be empty");
    }
    if !path.starts_with('/') {
        return Err("path must be absolute");
    }
    if path.contains('\0') {
        return Err("path must not contain NUL");
    }
    if path.contains(['*', '?', '[', ']', '{', '}', '\\']) {
        return Err("path must be a literal without glob or metacharacters");
    }
    if path.contains("//") {
        return Err("path must not contain duplicate separators");
    }
    if path != "/" && path.ends_with('/') {
        return Err("path must not have a trailing separator");
    }
    if path.split('/').any(|component| component == ".") {
        return Err("path must not contain a '.' component");
    }
    if path.split('/').any(|component| component == "..") {
        return Err("path must not contain a '..' component");
    }
    Ok(())
}

fn is_strict_descendant(root: &str, candidate: &str) -> bool {
    if root == "/" {
        return candidate != "/";
    }
    candidate
        .strip_prefix(root)
        .is_some_and(|suffix| suffix.starts_with('/'))
}

fn target_kind_rank(kind: FileProtectionTargetKind) -> u8 {
    match kind {
        FileProtectionTargetKind::ExactFile => 0,
        FileProtectionTargetKind::DirectoryTree => 1,
    }
}

fn exclusion_kind_rank(kind: FileProtectionExclusionKind) -> u8 {
    match kind {
        FileProtectionExclusionKind::ExactEntry => 0,
        FileProtectionExclusionKind::DirectorySubtree => 1,
    }
}

fn target_error(
    target_index: usize,
    path: &str,
    reason: impl Into<String>,
) -> FileProtectionNormalizationError {
    FileProtectionNormalizationError {
        target_index,
        exclusion_index: None,
        path: format!("{path:?}"),
        reason: reason.into(),
    }
}

fn exclusion_error(
    target_index: usize,
    exclusion_index: usize,
    path: &str,
    reason: impl Into<String>,
) -> FileProtectionNormalizationError {
    FileProtectionNormalizationError {
        target_index,
        exclusion_index: Some(exclusion_index),
        path: format!("{path:?}"),
        reason: reason.into(),
    }
}

#[cfg(test)]
#[path = "file_protection_policy_tests.rs"]
mod tests;
