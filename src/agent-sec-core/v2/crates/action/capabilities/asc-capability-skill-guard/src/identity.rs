//! Canonical Skill identity is lexical; physical I/O resolution is a separate operation.

use crate::GuardError;
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

/// Absolute canonical source identity, shared by aliases resolved through `SkillFS`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "PathBuf", into = "PathBuf")]
pub struct SkillIdentity(PathBuf);

impl SkillIdentity {
    /// Validates an already expanded, lexically normalized source path.
    ///
    /// # Errors
    /// Rejects relative paths, traversal, root itself, non-UTF-8 and ambiguous separators.
    pub fn new(path: impl AsRef<Path>) -> Result<Self, GuardError> {
        let path = path.as_ref();
        let raw = path
            .to_str()
            .ok_or_else(|| GuardError::Invalid("Skill path must be UTF-8".into()))?;
        if !path.is_absolute()
            || raw.contains('\0')
            || raw.contains("//")
            || raw.ends_with('/')
            || path
                .components()
                .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
            || raw.split('/').any(|part| part == "." || part == "..")
        {
            return Err(GuardError::Invalid(
                "Skill path must be absolute and lexically normalized".into(),
            ));
        }
        Ok(Self(path.to_path_buf()))
    }

    /// Canonical source path used in requests, signed records and lock identity.
    pub fn path(&self) -> &Path {
        &self.0
    }

    /// Display name derived from the canonical identity, never the live backing name.
    pub fn name(&self) -> &str {
        // new() rejects root and requires UTF-8; the last component therefore exists.
        self.0
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
    }
}

impl TryFrom<PathBuf> for SkillIdentity {
    type Error = GuardError;
    fn try_from(path: PathBuf) -> Result<Self, Self::Error> {
        Self::new(path)
    }
}

impl From<SkillIdentity> for PathBuf {
    fn from(identity: SkillIdentity) -> Self {
        identity.0
    }
}
