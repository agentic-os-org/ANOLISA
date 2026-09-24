//! Explicit system configuration, independent of caller HOME and environment variables.

use crate::{SkillIdentity, SkillSecError};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Validated inputs supplied by the daemon composition root.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillSecConfig {
    /// Private service-owned directory containing the current signing identity.
    pub state_dir: PathBuf,
    /// Explicit canonical Skill roots; ownership is not an authorization boundary in phase one.
    // TODO(SkillSec maintainers): add per-user isolation before restricting the phase-one
    // contract that allows every local caller to operate every managed Skill.
    pub managed_skill_dirs: Vec<SkillIdentity>,
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
