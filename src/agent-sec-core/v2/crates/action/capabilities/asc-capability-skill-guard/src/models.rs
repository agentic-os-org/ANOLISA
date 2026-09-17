//! Signed records retain the SkillFS-consumed fields and bind the canonical source identity.

use crate::{GuardError, SkillIdentity};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Scanner severity; integrity failures are reported separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanStatus {
    /// No scanner has certified the content.
    None,
    /// Scanned without actionable findings.
    Pass,
    /// Soft findings or incomplete scanner coverage.
    Warn,
    /// Hard findings.
    Deny,
}

/// Scanner-independent finding retained in a signed scan entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    /// Stable rule identifier.
    pub rule: String,
    /// Risk severity; producers must not use None.
    pub level: ScanStatus,
    /// Description intended for the caller.
    pub message: String,
    /// Relative evidence path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// One-based evidence line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u64>,
    /// Structured scanner evidence; audit projection selects safe fields separately.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, serde_json::Value>,
}

/// One scanner's result for the manifest's exact content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScanEntry {
    /// Canonical scanner identifier.
    pub scanner: String,
    /// Scanner version for reproducibility.
    pub version: String,
    /// Aggregate finding severity.
    pub status: ScanStatus,
    /// Normalized findings.
    pub findings: Vec<Finding>,
    /// UTC time of the scan.
    pub scanned_at: String,
}

/// Persisted user decision, distinct from a Hook's one-operation confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionAction {
    /// Approve this version.
    Allow,
    /// Persist the existing always-allow behavior.
    AlwaysAllow,
    /// Hide this Skill.
    Block,
    /// Restore a selected trusted version.
    Rollback,
}

/// Decision signed into a specific version record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UserDecision {
    /// Decision selected by the caller.
    pub action: DecisionAction,
    /// Required only for rollback.
    pub target_version_id: Option<String>,
    /// Optional explanation, not trusted identity.
    pub reason: Option<String>,
}

/// Ed25519 signature of the UTF-8 manifestHash string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManifestSignature {
    /// Exactly ed25519.
    pub algorithm: String,
    /// Standard base64 signature bytes.
    pub value: String,
    /// SHA-256 fingerprint of the raw public key.
    pub key_fingerprint: String,
}

/// V2 signed manifest; V1 manifests without canonical binding are not imported.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Manifest {
    /// V2 record format, independent of activation and notify protocol versions.
    pub version: u32,
    /// Monotonic version identifier within this Skill's current trust generation.
    pub version_id: String,
    /// Preceding version when this is not the first record.
    pub previous_version_id: Option<String>,
    /// Canonical source binding prevents replay into a different same-named Skill.
    pub canonical_skill_dir: SkillIdentity,
    /// Canonical leaf name consumed by existing integrations.
    pub skill_name: String,
    /// Sorted relative paths and content digests.
    pub file_hashes: BTreeMap<String, String>,
    /// Scanner results for this content.
    pub scans: Vec<ScanEntry>,
    /// Most severe scanner result; empty scans produce None.
    pub scan_status: ScanStatus,
    /// Optional signed user decision.
    pub user_decision: Option<UserDecision>,
    /// Existing warning/allow/block compatibility projection.
    pub policy: String,
    /// UTC record creation time.
    pub created_at: String,
    /// UTC last mutation time.
    pub updated_at: String,
    /// Hash of all fields except manifestHash and signature.
    pub manifest_hash: String,
    /// Signature link to the preceding version.
    pub previous_manifest_signature: Option<String>,
    /// Current system signing identity's signature.
    pub signature: Option<ManifestSignature>,
}

impl Manifest {
    /// Creates an unsigned initial record; no storage is changed.
    pub fn initial(identity: SkillIdentity, file_hashes: BTreeMap<String, String>) -> Self {
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
        Self {
            version: 2,
            version_id: "v000001".into(),
            previous_version_id: None,
            skill_name: identity.name().into(),
            canonical_skill_dir: identity,
            file_hashes,
            scans: Vec::new(),
            scan_status: ScanStatus::None,
            user_decision: None,
            policy: "warning".into(),
            created_at: now.clone(),
            updated_at: now,
            manifest_hash: String::new(),
            previous_manifest_signature: None,
            signature: None,
        }
    }

    /// Validates domain invariants before signing or trusting a decoded record.
    ///
    /// # Errors
    /// Rejects malformed versions, path/hash entries, scanner summaries or decisions.
    pub fn validate(&self) -> Result<(), GuardError> {
        if self.version != 2
            || !valid_version(&self.version_id)
            || self.skill_name != self.canonical_skill_dir.name()
            || self
                .previous_version_id
                .as_ref()
                .is_some_and(|v| !valid_version(v) || v >= &self.version_id)
            || self.previous_version_id.is_some() != self.previous_manifest_signature.is_some()
            || !matches!(self.policy.as_str(), "warning" | "allow" | "block")
        {
            return Err(GuardError::Integrity(
                "invalid manifest identity or version".into(),
            ));
        }
        for (path, digest) in &self.file_hashes {
            if !valid_content_path(path) || !valid_digest(digest) {
                return Err(GuardError::Integrity(
                    "invalid content path or digest".into(),
                ));
            }
        }
        let aggregate = self
            .scans
            .iter()
            .map(|scan| scan.status)
            .max()
            .unwrap_or(ScanStatus::None);
        if aggregate != self.scan_status
            || self.scans.iter().any(|scan| {
                scan.status == ScanStatus::None
                    || scan
                        .findings
                        .iter()
                        .any(|f| f.level == ScanStatus::None || f.level > scan.status)
            })
        {
            return Err(GuardError::Integrity("invalid scanner summary".into()));
        }
        if let Some(decision) = &self.user_decision
            && ((decision.action == DecisionAction::Rollback)
                != decision.target_version_id.is_some()
                || decision
                    .target_version_id
                    .as_ref()
                    .is_some_and(|v| !valid_version(v)))
        {
            return Err(GuardError::Integrity("invalid user decision".into()));
        }
        Ok(())
    }
}

pub(crate) fn valid_version(value: &str) -> bool {
    value.len() == 7
        && value.starts_with('v')
        && value[1..].bytes().all(|c| c.is_ascii_digit())
        && value != "v000000"
}

pub(crate) fn valid_content_path(value: &str) -> bool {
    !value.is_empty()
        && !value.contains('\0')
        && value
            .split('/')
            .all(|c| !matches!(c, "" | "." | ".." | ".skill-meta" | ".git"))
}

pub(crate) fn valid_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|v| {
        v.len() == 64
            && v.bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    })
}
