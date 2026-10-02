//! Explicit system configuration, independent of caller HOME and environment variables.

use crate::{SkillIdentity, SkillSecError};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Validated inputs supplied by the daemon composition root.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillSecConfig {
    /// Private service-owned directory containing the current signing identity.
    pub state_dir: PathBuf,
    /// Administrator-authorized exact roots, direct-child `/*`, or recursive `/**` patterns.
    // TODO(SkillSec maintainers): add per-user isolation before restricting the phase-one
    // contract that allows every local caller to operate every managed Skill.
    pub managed_skill_dirs: Vec<ManagedSkillDir>,
}

impl SkillSecConfig {
    /// Checks deployment paths before loading keys or accessing Skills.
    ///
    /// # Errors
    /// Rejects a relative, ambiguous or traversal-containing state path.
    pub fn validate(&self) -> Result<(), SkillSecError> {
        SkillIdentity::new(&self.state_dir)?;
        Ok(())
    }
}

/// One administrator-configured directory pattern, distinct from an exact request identity.
/// Only a terminal `/*` or `/**` is expanded; paths are absolute and never use caller HOME.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "PathBuf", into = "PathBuf")]
pub struct ManagedSkillDir {
    pub(crate) root: SkillIdentity,
    pub(crate) scope: Scope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope {
    Exact,
    Children,
    Recursive,
}

impl ManagedSkillDir {
    /// Parses the three supported configuration forms without reading the filesystem.
    ///
    /// # Errors
    /// Rejects ambiguous, relative and unsupported wildcard paths.
    pub fn new(path: impl AsRef<Path>) -> Result<Self, SkillSecError> {
        let text = path
            .as_ref()
            .to_str()
            .ok_or_else(|| SkillSecError::Invalid("managed Skill path must be UTF-8".into()))?;
        let (base, scope) = if let Some(base) = text.strip_suffix("/**") {
            (base, Scope::Recursive)
        } else if let Some(base) = text.strip_suffix("/*") {
            (base, Scope::Children)
        } else {
            (text, Scope::Exact)
        };
        if base.contains(['*', '?', '[', ']']) {
            return Err(SkillSecError::Invalid(
                "only terminal /* and /** patterns are supported".into(),
            ));
        }
        let base = if base.is_empty() && scope != Scope::Exact {
            "/"
        } else {
            base
        };
        Ok(Self {
            root: SkillIdentity::new(base)?,
            scope,
        })
    }

    /// Matches path components, excluding hidden descendants of wildcard roots.
    #[must_use]
    pub fn contains(&self, identity: &SkillIdentity) -> bool {
        if self.scope == Scope::Exact {
            return self.root == *identity;
        }
        let Ok(relative) = identity.path().strip_prefix(self.root.path()) else {
            return false;
        };
        if relative
            .components()
            .any(|part| part.as_os_str().to_string_lossy().starts_with('.'))
        {
            return false;
        }
        self.scope == Scope::Recursive || relative.components().count() == 1
    }
}

impl TryFrom<PathBuf> for ManagedSkillDir {
    type Error = SkillSecError;
    fn try_from(path: PathBuf) -> Result<Self, Self::Error> {
        Self::new(path)
    }
}

impl From<ManagedSkillDir> for PathBuf {
    fn from(pattern: ManagedSkillDir) -> Self {
        match pattern.scope {
            Scope::Exact => pattern.root.path().to_owned(),
            Scope::Children => pattern.root.path().join("*"),
            Scope::Recursive => pattern.root.path().join("**"),
        }
    }
}
