//! Content hashing and current-key-only Ed25519 record authentication.

mod keys;
mod tree;

use crate::models::ManifestSignature;
use crate::{GuardError, Manifest, SkillIdentity};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ring::signature::{ED25519, Ed25519KeyPair, KeyPair as _, UnparsedPublicKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;

pub use keys::KeyStore;
pub use tree::hash_tree;

/// Sorted relative content paths and SHA-256 digests.
pub type FileHashes = BTreeMap<String, String>;

/// Content changes against an authenticated manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HashDiff {
    /// True only when the entire content map agrees.
    #[serde(rename = "match")]
    pub matches: bool,
    /// Paths present only in current content.
    pub added: Vec<String>,
    /// Paths absent from current content.
    pub removed: Vec<String>,
    /// Paths whose bytes differ.
    pub modified: Vec<String>,
}

impl HashDiff {
    /// Compares content maps without trusting or modifying either one.
    pub fn between(stored: &FileHashes, current: &FileHashes) -> Self {
        let added: Vec<_> = current
            .keys()
            .filter(|p| !stored.contains_key(*p))
            .cloned()
            .collect();
        let removed: Vec<_> = stored
            .keys()
            .filter(|p| !current.contains_key(*p))
            .cloned()
            .collect();
        let modified: Vec<_> = current
            .iter()
            .filter(|(p, h)| stored.get(*p).is_some_and(|v| v != *h))
            .map(|(p, _)| p.clone())
            .collect();
        Self {
            matches: added.is_empty() && removed.is_empty() && modified.is_empty(),
            added,
            removed,
            modified,
        }
    }
}

/// Daemon-owned signing identity; secret material has no Debug or serialization implementation.
pub struct SigningIdentity(Ed25519KeyPair);

impl SigningIdentity {
    /// Fingerprint suitable for diagnostics and signed-record trust binding.
    pub fn fingerprint(&self) -> String {
        digest(self.0.public_key().as_ref())
    }

    /// Public key bytes; private material never crosses the service boundary.
    pub fn public_key(&self) -> &[u8] {
        self.0.public_key().as_ref()
    }

    /// Authenticates a validated domain record with the current system identity.
    ///
    /// # Errors
    /// Rejects invalid manifest invariants or failed canonical serialization.
    pub fn sign_manifest(&self, manifest: &mut Manifest) -> Result<(), GuardError> {
        manifest.validate()?;
        manifest.manifest_hash = manifest_digest(manifest)?;
        manifest.signature = Some(ManifestSignature {
            algorithm: "ed25519".into(),
            value: STANDARD.encode(self.0.sign(manifest.manifest_hash.as_bytes()).as_ref()),
            key_fingerprint: self.fingerprint(),
        });
        Ok(())
    }

    /// Verifies identity binding, metadata hash and signature before content is trusted.
    ///
    /// # Errors
    /// Rejects old keys, cross-Skill replay, malformed records and any signature/hash mismatch.
    pub fn verify_manifest(
        &self,
        manifest: &Manifest,
        expected: &SkillIdentity,
    ) -> Result<(), GuardError> {
        manifest.validate()?;
        let signature = manifest
            .signature
            .as_ref()
            .ok_or_else(|| GuardError::Integrity("manifest has no signature".into()))?;
        if &manifest.canonical_skill_dir != expected
            || signature.algorithm != "ed25519"
            || signature.key_fingerprint != self.fingerprint()
            || manifest.manifest_hash != manifest_digest(manifest)?
        {
            return Err(GuardError::Integrity(
                "manifest trust binding or hash does not match".into(),
            ));
        }
        let bytes = STANDARD
            .decode(&signature.value)
            .map_err(|_| GuardError::Integrity("signature is not base64".into()))?;
        UnparsedPublicKey::new(&ED25519, self.public_key())
            .verify(manifest.manifest_hash.as_bytes(), &bytes)
            .map_err(|_| GuardError::Integrity("signature does not match".into()))
    }
}

pub(crate) fn digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn manifest_digest(manifest: &Manifest) -> Result<String, GuardError> {
    let mut value = serde_json::to_value(manifest)?;
    let map = value
        .as_object_mut()
        .ok_or_else(|| GuardError::Integrity("manifest is not an object".into()))?;
    map.remove("manifestHash");
    map.remove("signature");
    // Recursive sorting must remain explicit even if another workspace crate enables preserve_order.
    value.sort_all_objects();
    Ok(digest(&serde_json::to_vec(&value)?))
}
