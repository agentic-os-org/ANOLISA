//! Authenticated version artifacts and snapshots; all mutations require the Service's Skill lock.

pub(crate) mod content;
pub(crate) mod storage;

use crate::models::valid_version;
use crate::{Manifest, SigningIdentity, SkillIdentity, SkillSecError, check_deadline};
use content::Content;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;
use storage::{Directory, MAX_RECORD_BYTES, missing, nonce};

pub(crate) struct Ledger {
    pub meta: Directory,
    pub versions: Directory,
}

impl Ledger {
    pub fn open(root: &Directory, create: bool) -> Result<Option<Self>, SkillSecError> {
        let meta = match root.child(".skill-meta", create) {
            Ok(value) => value,
            Err(e) if !create && missing(&e) => return Ok(None),
            Err(e) => return Err(e),
        };
        let versions = match meta.child("versions", create) {
            Ok(value) => value,
            Err(e) if !create && missing(&e) => {
                return Err(SkillSecError::Integrity(
                    "ledger versions directory is missing".into(),
                ));
            }
            Err(e) => return Err(e),
        };
        Ok(Some(Self { meta, versions }))
    }

    pub fn ids(&self, deadline: Instant) -> Result<BTreeSet<String>, SkillSecError> {
        Ok(self
            .versions
            .names(deadline)?
            .into_iter()
            .filter_map(|name| {
                name.strip_suffix(".json")
                    .or_else(|| name.strip_suffix(".snapshot"))
                    .filter(|id| valid_version(id))
                    .map(str::to_owned)
            })
            .collect())
    }

    pub fn version(
        &self,
        id: &str,
        key: &SigningIdentity,
        identity: &SkillIdentity,
        deadline: Instant,
    ) -> Result<Manifest, SkillSecError> {
        if !valid_version(id) {
            return Err(SkillSecError::Invalid("invalid version selector".into()));
        }
        let manifest: Manifest = serde_json::from_slice(&self.versions.read(
            &format!("{id}.json"),
            MAX_RECORD_BYTES,
            deadline,
        )?)?;
        key.verify_manifest(&manifest, identity)?;
        if manifest.version_id != id {
            return Err(SkillSecError::Integrity(
                "version filename disagrees with signed identity".into(),
            ));
        }
        Ok(manifest)
    }

    pub fn snapshot(
        &self,
        manifest: &Manifest,
        deadline: Instant,
    ) -> Result<Content, SkillSecError> {
        let directory = self
            .versions
            .child(&format!("{}.snapshot", manifest.version_id), false)?;
        let content = Content::capture(&directory, true, deadline)?;
        if content.hashes() != manifest.file_hashes {
            return Err(SkillSecError::Integrity(
                "snapshot does not match manifest".into(),
            ));
        }
        Ok(content)
    }

    pub fn newest(
        &self,
        key: &SigningIdentity,
        identity: &SkillIdentity,
        snapshots: bool,
        deadline: Instant,
    ) -> Result<Option<Manifest>, SkillSecError> {
        for id in self.ids(deadline)?.iter().rev() {
            check_deadline(deadline)?;
            if let Ok(manifest) = self.version(id, key, identity, deadline)
                && (!snapshots || self.snapshot(&manifest, deadline).is_ok())
            {
                return Ok(Some(manifest));
            }
        }
        check_deadline(deadline)?;
        Ok(None)
    }

    pub fn latest(
        &self,
        key: &SigningIdentity,
        identity: &SkillIdentity,
        snapshots: bool,
        deadline: Instant,
    ) -> Result<Option<Manifest>, SkillSecError> {
        let bytes = match self.meta.read("latest.json", MAX_RECORD_BYTES, deadline) {
            Ok(bytes) => bytes,
            Err(e) if missing(&e) && self.ids(deadline)?.is_empty() => return Ok(None),
            Err(e) => return Err(e),
        };
        let manifest: Manifest = serde_json::from_slice(&bytes)?;
        key.verify_manifest(&manifest, identity)?;
        if self.newest(key, identity, false, deadline)?.as_ref() != Some(&manifest) {
            return Err(SkillSecError::Integrity(
                "latest.json does not match the newest authenticated version".into(),
            ));
        }
        if snapshots {
            self.snapshot(&manifest, deadline)?;
        }
        Ok(Some(manifest))
    }

    pub fn next_id(
        &self,
        previous: Option<&Manifest>,
        deadline: Instant,
    ) -> Result<String, SkillSecError> {
        let start = match previous {
            Some(m) => {
                m.version_id[1..]
                    .parse::<u32>()
                    .map_err(|_| SkillSecError::Integrity("invalid predecessor ID".into()))?
                    + 1
            }
            None => 1,
        };
        let reserved = self.ids(deadline)?;
        for number in start..1_000_000 {
            check_deadline(deadline)?;
            let candidate = format!("v{number:06}");
            if !reserved.contains(&candidate) {
                return Ok(candidate);
            }
        }
        Err(SkillSecError::Invalid("version ID space exhausted".into()))
    }

    pub fn commit(
        &self,
        manifest: &Manifest,
        content: &Content,
        new_version: bool,
        deadline: Instant,
        before_publish: impl FnOnce() -> Result<(), SkillSecError>,
    ) -> Result<(), SkillSecError> {
        let bytes = serde_json::to_vec(manifest)?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_RECORD_BYTES {
            return Err(SkillSecError::Invalid("signed record exceeds 8 MiB".into()));
        }
        let temporary = if new_version {
            Some(nonce(".snapshot-")?)
        } else {
            None
        };
        let result = (|| {
            check_deadline(deadline)?;
            self.meta.verify_path()?;
            self.versions.verify_path()?;
            let staging = if let Some(name) = &temporary {
                let directory = self.versions.fresh_child(name)?;
                content.write(&directory, deadline)?;
                if Content::capture(&directory, true, deadline)?.hashes() != manifest.file_hashes {
                    return Err(SkillSecError::Integrity(
                        "staged snapshot changed before publication".into(),
                    ));
                }
                Some(directory)
            } else {
                self.snapshot(manifest, deadline)?;
                None
            };
            before_publish()?;
            check_deadline(deadline)?;
            self.meta.verify_path()?;
            self.versions.verify_path()?;
            if let (Some(name), Some(directory)) = (&temporary, staging) {
                directory.verify_path()?;
                self.versions
                    .rename_child(name, &format!("{}.snapshot", manifest.version_id))?;
            }
            // Complete both atomic records after publication starts. A crash between them remains
            // detectable; startup reconciliation verifies the snapshot before repairing latest.
            self.versions.write_atomic(
                &format!("{}.json", manifest.version_id),
                &bytes,
                !new_version,
            )?;
            self.meta.write_atomic("latest.json", &bytes, true)?;
            self.meta.verify_path()?;
            self.versions.verify_path()
        })();
        if let (Err(original), Some(name)) = (&result, &temporary) {
            // Clean only this operation's unpublished temporary snapshot. Published slots remain
            // reserved as recovery evidence, even if a later record write failed.
            if let Err(cleanup) = self
                .versions
                .remove_child(name, Instant::now() + std::time::Duration::from_secs(5))
            {
                return Err(SkillSecError::Integrity(format!(
                    "{original}; temporary snapshot cleanup failed: {cleanup}"
                )));
            }
        }
        result
    }

    pub fn audit(
        &self,
        key: &SigningIdentity,
        identity: &SkillIdentity,
        snapshots: bool,
        deadline: Instant,
    ) -> Result<Value, SkillSecError> {
        let ids = self.ids(deadline)?;
        let mut authenticated = BTreeMap::new();
        let mut errors = Vec::new();
        for id in &ids {
            check_deadline(deadline)?;
            match self.version(id, key, identity, deadline) {
                Ok(manifest) => {
                    if snapshots && self.snapshot(&manifest, deadline).is_err() {
                        errors.push(json!({"versionId":id,"error":"Snapshot missing or invalid"}));
                    }
                    // Parent authentication needs only chain links; drop unbounded findings now.
                    authenticated.insert(
                        id.clone(),
                        (
                            manifest.signature.map(|signature| signature.value),
                            manifest.previous_version_id,
                            manifest.previous_manifest_signature,
                        ),
                    );
                }
                Err(_) => errors.push(
                    json!({"versionId":id,"error":"Version manifest missing or unauthenticated"}),
                ),
            }
        }
        for (id, (_, parent_id, previous_signature)) in &authenticated {
            check_deadline(deadline)?;
            if let Some(parent_id) = parent_id {
                let parent_signature = authenticated
                    .get(parent_id)
                    .and_then(|(signature, _, _)| signature.as_ref());
                if parent_signature.is_none() || parent_signature != previous_signature.as_ref() {
                    errors.push(json!({"versionId":id,"error":"Referenced parent signature missing or invalid"}));
                }
            }
        }
        if self.latest(key, identity, false, deadline).is_err() {
            errors.push(
                json!({"versionId":"latest.json","error":"Latest record missing or inconsistent"}),
            );
        }
        check_deadline(deadline)?;
        let mut result = json!({"canonicalSkillDir":identity,"skillName":identity.name(),
            "valid":errors.is_empty(),"versions_checked":ids.len(),"errors":errors});
        if ids.is_empty() && result["valid"] == true {
            result["message"] = json!("No versions found — nothing to audit");
        }
        Ok(result)
    }
}
