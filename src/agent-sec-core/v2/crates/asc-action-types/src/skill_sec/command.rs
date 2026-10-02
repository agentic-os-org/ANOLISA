//! Closed `SkillSec` operation contract; physical roots and caller identity are injected separately.

use super::{DecisionAction, SkillIdentity, SkillSecInputError};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

/// Version-two daemon business request. Unknown fields cannot supply identities or physical paths.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "command",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum SkillSecCommand {
    /// Initialize system keys and optionally scan discovered or already registered roots.
    Init {
        /// False corresponds to CLI --no-baseline.
        #[serde(default = "yes")]
        baseline: bool,
        /// Root-only trust reset, identical to rotate-keys when keys exist.
        #[serde(default)]
        force_keys: bool,
        /// Exact additional roots discovered by the CLI, never parent globs.
        #[serde(default)]
        skill_dirs: Vec<SkillIdentity>,
        /// Optional built-in scanner selection.
        scanners: Option<Vec<String>>,
    },
    /// Check one root or all registered/discovered roots.
    Check {
        /// Exactly one of skillDir and all must be selected.
        skill_dir: Option<SkillIdentity>,
        /// Aggregate check, retaining per-Skill failures.
        #[serde(default)]
        all: bool,
        /// Exact additional roots for an aggregate operation.
        #[serde(default)]
        skill_dirs: Vec<SkillIdentity>,
    },
    /// Analyze without signing keys or Ledger writes.
    Analyze {
        /// Root to analyze.
        skill_dir: SkillIdentity,
    },
    /// Run built-in scanners and commit results.
    Scan {
        /// Exactly one of skillDir and all must be selected.
        skill_dir: Option<SkillIdentity>,
        /// Aggregate scan, retaining per-Skill failures.
        #[serde(default)]
        all: bool,
        /// Exact additional roots for an aggregate operation.
        #[serde(default)]
        skill_dirs: Vec<SkillIdentity>,
        /// Replace existing scanner results.
        #[serde(default)]
        force: bool,
        /// Optional selected scanners.
        scanners: Option<Vec<String>>,
    },
    /// Import external scanner findings, already read by the unprivileged CLI.
    Certify {
        /// Root being certified.
        skill_dir: SkillIdentity,
        /// Configured scanner identifier.
        scanner: String,
        /// Optional scanner version.
        scanner_version: Option<String>,
        /// Findings-array input; no daemon-side arbitrary file read.
        findings: Value,
    },
    /// Query system readiness and aggregate integrity health.
    Status {
        /// Include per-Skill results.
        #[serde(default)]
        verbose: bool,
    },
    /// Verify history and optionally snapshots.
    Audit {
        /// Root whose history is verified.
        skill_dir: SkillIdentity,
        /// Also verify snapshot bytes.
        #[serde(default)]
        verify_snapshots: bool,
    },
    /// List system-configured scanner metadata.
    ListScanners {},
    /// Apply or clear a manual decision.
    Decide {
        /// Root whose latest decision is changed.
        skill_dir: SkillIdentity,
        /// Omit only when clearing a decision.
        action: Option<DecisionAction>,
        /// Optional rollback target.
        version: Option<String>,
        /// Human explanation, excluded from public audit.
        reason: Option<String>,
        /// Clear the latest decision.
        #[serde(default)]
        clear: bool,
    },
    /// Inspect latest, active and source consistency without publication.
    Show {
        /// Root being inspected.
        skill_dir: SkillIdentity,
    },
    /// Export a verified snapshot to an empty caller-owned directory.
    Export {
        /// Root being exported.
        skill_dir: SkillIdentity,
        /// latest, active or a version identifier.
        version: String,
        /// Absolute destination created by the caller.
        output: PathBuf,
    },
    /// Retry publication of the current selected exposure.
    Activate {
        /// Root being activated.
        skill_dir: SkillIdentity,
    },
    /// Startup-only recovery; never accepted from an RPC body.
    #[serde(skip)]
    Reconcile {
        /// Canonical root prepared by daemon startup.
        skill_dir: SkillIdentity,
    },
    /// Replace the current system key; requires kernel UID zero.
    RotateKeys {},
}

const fn yes() -> bool {
    true
}

impl SkillSecCommand {
    /// Stable operation name for audit, independent of caller strings.
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Init { .. } => "init",
            Self::Check { .. } => "check",
            Self::Analyze { .. } => "analyze",
            Self::Scan { .. } => "scan",
            Self::Certify { .. } => "certify",
            Self::Status { .. } => "status",
            Self::Audit { .. } => "audit",
            Self::ListScanners {} => "list-scanners",
            Self::Decide { .. } => "decide",
            Self::Show { .. } => "show",
            Self::Export { .. } => "export",
            Self::Activate { .. } => "activate",
            Self::RotateKeys {} => "rotate-keys",
            Self::Reconcile { .. } => "reconcile",
        }
    }

    /// Returns exact canonical identities requiring runtime resolution, after argument validation.
    ///
    /// # Errors
    /// Rejects conflicting selectors, excessive batches, invalid decisions and oversized reasons.
    pub fn identities(
        &self,
        managed: &[SkillIdentity],
    ) -> Result<Vec<SkillIdentity>, SkillSecInputError> {
        if let Self::Init { skill_dirs, .. }
        | Self::Scan { skill_dirs, .. }
        | Self::Check { skill_dirs, .. } = self
            && skill_dirs.len() > 1024
        {
            return Err(SkillSecInputError::Invalid(
                "caller discovery supports at most 1024 roots".into(),
            ));
        }
        let mut roots = match self {
            Self::Init {
                baseline,
                force_keys,
                skill_dirs,
                ..
            } => {
                if *baseline || *force_keys {
                    [managed, skill_dirs].concat()
                } else {
                    Vec::new()
                }
            }
            Self::Scan {
                skill_dir,
                all,
                skill_dirs,
                ..
            }
            | Self::Check {
                skill_dir,
                all,
                skill_dirs,
            } => {
                if *all == skill_dir.is_some() || (!*all && !skill_dirs.is_empty()) {
                    return Err(SkillSecInputError::Invalid(
                        "select skillDir or all, exclusively".into(),
                    ));
                }
                if *all {
                    [managed, skill_dirs].concat()
                } else {
                    skill_dir.iter().cloned().collect()
                }
            }
            Self::Status { .. } | Self::RotateKeys {} => managed.to_vec(),
            Self::ListScanners {} => Vec::new(),
            Self::Analyze { skill_dir }
            | Self::Certify { skill_dir, .. }
            | Self::Audit { skill_dir, .. }
            | Self::Decide { skill_dir, .. }
            | Self::Show { skill_dir }
            | Self::Export { skill_dir, .. }
            | Self::Activate { skill_dir }
            | Self::Reconcile { skill_dir } => vec![skill_dir.clone()],
        };
        if let Self::Decide {
            action,
            clear,
            version,
            reason,
            ..
        } = self
        {
            if *clear == action.is_some()
                || (version.is_some() && *action != Some(DecisionAction::Rollback))
            {
                return Err(SkillSecInputError::Invalid(
                    "select clear or a decision; version is only valid for rollback".into(),
                ));
            }
            if reason.as_ref().is_some_and(|s| s.len() > 4096) {
                return Err(SkillSecInputError::Invalid(
                    "decision reason exceeds 4096 bytes".into(),
                ));
            }
        }
        roots.sort();
        roots.dedup();
        Ok(roots)
    }
}
