//! Shared orchestration and per-canonical-Skill serialization for every Ledger consumer.

use crate::ledger::Ledger;
use crate::ledger::content::Content;
use crate::ledger::storage::{Directory, MAX_RECORD_BYTES, missing, set_owner};
use crate::scanner::{ScanTree, ScannerRegistry, requested_names, scan_entry};
use crate::{
    DecisionAction, GuardConfig, GuardError, HashDiff, KeyStore, Manifest, ScanEntry, ScanStatus,
    SigningIdentity, SkillIdentity, check_deadline, io_error,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, RwLock, TryLockError, Weak};
use std::time::{Duration, Instant};

/// Trusted mapping produced by the daemon's configured resolver, never decoded from RPC parameters.
#[derive(Debug, Clone)]
pub struct SkillRoot {
    /// Stable source identity shared by source and live aliases.
    pub identity: SkillIdentity,
    /// Physical directory supplied by the verified resolver or equal to the direct source path.
    pub io_dir: PathBuf,
    file_identity: Option<(u64, u64)>,
    resolution_error: Option<String>,
}

impl SkillRoot {
    /// Uses the canonical path directly when no `SkillFS` mapping applies.
    ///
    /// # Errors
    /// Rejects relative, ambiguous or traversal-containing paths.
    pub fn direct(path: impl AsRef<Path>) -> Result<Self, GuardError> {
        let identity = SkillIdentity::new(path)?;
        Ok(Self {
            io_dir: identity.path().into(),
            identity,
            file_identity: None,
            resolution_error: None,
        })
    }

    /// Separates source identity from a physical path authenticated by the runtime resolver.
    ///
    /// # Errors
    /// Rejects an invalid physical path. The caller must authenticate the mapping before calling.
    pub fn resolved(identity: SkillIdentity, io_dir: PathBuf) -> Result<Self, GuardError> {
        SkillIdentity::new(&io_dir)?;
        Ok(Self {
            identity,
            io_dir,
            file_identity: None,
            resolution_error: None,
        })
    }
    /// Preserves a per-Skill resolver failure in aggregate results without reading the FUSE view.
    pub fn unavailable(identity: SkillIdentity, message: String) -> Self {
        Self {
            io_dir: identity.path().into(),
            identity,
            file_identity: None,
            resolution_error: Some(message),
        }
    }

    /// Pins the physical directory returned by an authenticated shared-path resolver.
    ///
    /// # Errors
    /// Rejects a replacement directory or unsafe traversal before accepting the mapping.
    pub fn with_file_identity(mut self, device: u64, inode: u64) -> Result<Self, GuardError> {
        self.file_identity = Some((device, inode));
        self.open_verified()?;
        Ok(self)
    }

    pub(crate) fn open_verified(&self) -> Result<Directory, GuardError> {
        if [self.identity.path(), self.io_dir.as_path()]
            .iter()
            .any(|path| {
                path.components()
                    .any(|part| part.as_os_str() == ".skill-meta")
            })
        {
            return Err(GuardError::Invalid(
                "Skill root must be outside reserved Ledger metadata".into(),
            ));
        }
        if let Some(message) = &self.resolution_error {
            return Err(GuardError::Integrity(message.clone()));
        }
        let directory = Directory::open(&self.io_dir)?;
        if let Some(expected) = self.file_identity {
            let meta = directory
                .file
                .metadata()
                .map_err(|e| io_error(&self.io_dir, e))?;
            if (meta.dev(), meta.ino()) != expected {
                return Err(GuardError::Integrity(
                    "resolved Skill directory identity changed".into(),
                ));
            }
        }
        Ok(directory)
    }

    pub(crate) fn verify_mapping(&self) -> Result<(), GuardError> {
        if self.file_identity.is_some() || self.resolution_error.is_some() {
            self.open_verified()?;
        }
        Ok(())
    }
}

/// Scanner selection and explicit rescanning, independent of transport defaults.
#[derive(Debug, Clone, Default)]
pub struct ScanOptions {
    /// None or empty selects the two built-ins; custom scanners remain import-only.
    pub scanners: Option<Vec<String>>,
    /// Replaces existing results even when content is unchanged.
    pub force: bool,
}

/// One daemon-owned service, shared by CLI actions and background actors.
pub struct SkillGuardService {
    pub(crate) config: GuardConfig,
    pub(crate) registry: ScannerRegistry,
    pub(crate) generation: RwLock<()>,
    pub(crate) active_requests: std::sync::atomic::AtomicUsize,
    locks: Mutex<BTreeMap<SkillIdentity, Weak<Mutex<()>>>>,
    managed: Mutex<BTreeSet<SkillIdentity>>,
}

impl SkillGuardService {
    /// Opens explicitly provisioned private state without initializing or replacing a key.
    ///
    /// # Errors
    /// Rejects unsafe state paths, permissions and malformed persisted registration.
    pub fn new(config: GuardConfig, registry: ScannerRegistry) -> Result<Self, GuardError> {
        config.validate()?;
        KeyStore::open(&config.state_dir)?;
        let directory = Directory::open(&config.state_dir)?;
        let mut managed: BTreeSet<SkillIdentity> = match directory.read(
            "managed-skills.json",
            MAX_RECORD_BYTES,
            Instant::now() + Duration::from_secs(5),
        ) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(e) if missing(&e) => BTreeSet::new(),
            Err(e) => return Err(e),
        };
        managed.extend(config.managed_skill_dirs.iter().cloned());
        Ok(Self {
            config,
            registry,
            generation: RwLock::new(()),
            active_requests: std::sync::atomic::AtomicUsize::new(0),
            locks: Mutex::new(BTreeMap::new()),
            managed: Mutex::new(managed),
        })
    }

    /// Initializes the current system key without touching any Skill or scanning a baseline.
    ///
    /// # Errors
    /// Propagates unsafe existing key/state and persistence errors.
    pub fn initialize(&self) -> Result<Value, GuardError> {
        self.initialize_with_deadline(Instant::now() + Duration::from_secs(30))
    }

    /// Initializes keys within the caller deadline, without replacing current trust.
    ///
    /// # Errors
    /// Rejects unsafe keys, pending rotation and exhausted deadlines.
    pub fn initialize_with_deadline(&self, deadline: Instant) -> Result<Value, GuardError> {
        let _generation = self.generation_write(deadline)?;
        self.require_no_rotation(deadline)?;
        let store = KeyStore::open(&self.config.state_dir)?;
        let created = match store.load() {
            Ok(_) => false,
            Err(error) if missing(&error) => true,
            Err(error) => return Err(error),
        };
        let key = store.initialize()?;
        Ok(json!({"initialized":true,"keyFingerprint":key.fingerprint(),"keyCreated":created}))
    }

    /// Exact registered roots; registration never expands a parent into sibling Skills.
    ///
    /// # Errors
    /// Reports a poisoned service registry.
    pub fn managed_skills(&self) -> Result<Vec<SkillIdentity>, GuardError> {
        Ok(self
            .managed
            .lock()
            .map_err(|_| poisoned())?
            .iter()
            .cloned()
            .collect())
    }

    /// Configured scanner inventory for CLI discovery and status.
    pub fn scanners(&self) -> &ScannerRegistry {
        &self.registry
    }

    /// Scans staged bytes, rechecks live content and commits under the canonical Skill lock.
    ///
    /// # Errors
    /// Rejects missing keys, unsafe paths, insufficient scan coverage, content changes and deadlines.
    pub fn scan(
        &self,
        root: &SkillRoot,
        options: &ScanOptions,
        deadline: Instant,
    ) -> Result<Value, GuardError> {
        let requested = requested_names(options.scanners.as_deref())?;
        self.with_locked_root(root, deadline, |directory, key| {
            self.recover_rollback(root, directory, key, deadline)?;
            validate_skill(directory)?;
            let ledger = required_ledger(directory)?;
            let content = Content::capture(directory, false, deadline)?;
            let original = ScanTree::open(&root.io_dir, deadline)?;
            let (mut manifest, state, new_version) =
                prepare(&ledger, root, key, &content, deadline)?;
            let to_run: Vec<_> = requested
                .iter()
                .filter(|name| {
                    options.force
                        || new_version
                        || !manifest.scans.iter().any(|s| &s.scanner == *name)
                })
                .cloned()
                .collect();
            if to_run.is_empty() {
                content.unchanged(directory, &original, deadline)?;
                let mut result = self.noop(root, &manifest, &requested)?;
                result["activation"] = refresh_locked(&ledger, directory, key, root, deadline);
                return Ok(result);
            }
            let (staging_dir, tree) =
                content.scan_tree(&self.config.state_dir, &original, deadline)?;
            let mut entries = self.registry.scan_tree(&tree, Some(&to_run), deadline)?;
            if entries.is_empty() {
                if new_version {
                    return Err(GuardError::Scanner(
                        "cannot establish trust without scanner results".into(),
                    ));
                }
                content.unchanged(directory, &original, deadline)?;
                let mut result = self.noop(root, &manifest, &to_run)?;
                result["activation"] = refresh_locked(&ledger, directory, key, root, deadline);
                return Ok(result);
            }
            canonicalize_entries(&mut entries, staging_dir.path(), root)?;
            let scanners_run: Vec<_> = entries.iter().map(|s| s.scanner.clone()).collect();
            merge(&mut manifest, entries);
            key.sign_manifest(&mut manifest)?;
            ledger.commit(&manifest, &content, new_version, deadline, || {
                content.unchanged(directory, &original, deadline)
            })?;
            self.remember(&root.identity)?;
            let skipped: Vec<_> = requested
                .iter()
                .filter(|name| !to_run.contains(name))
                .cloned()
                .collect();
            let mut result = scan_payload(&manifest, new_version, &scanners_run, "scanned");
            result["skippedScanners"] = json!(skipped);
            recovery_event(&mut result, state, "scan", &manifest, &scanners_run);
            result["activation"] = refresh_locked(&ledger, directory, key, root, deadline);
            Ok(result)
        })
    }

    /// Certifies an imported findings value against captured current content.
    ///
    /// # Errors
    /// Rejects invalid findings, keys, paths, changed content and expired execution deadlines.
    pub fn certify(
        &self,
        root: &SkillRoot,
        scanner: &str,
        version: Option<&str>,
        findings: &Value,
        deadline: Instant,
    ) -> Result<Value, GuardError> {
        let parsed = self.registry.parse_external(scanner, findings)?;
        self.with_locked_root(root, deadline, |directory, key| {
            self.recover_rollback(root, directory, key, deadline)?;
            validate_skill(directory)?;
            let ledger = required_ledger(directory)?;
            let content = Content::capture(directory, false, deadline)?;
            let original = ScanTree::open(&root.io_dir, deadline)?;
            let (mut manifest, state, new_version) =
                prepare(&ledger, root, key, &content, deadline)?;
            let mut entries = vec![scan_entry(
                scanner.into(),
                version.unwrap_or("unknown").into(),
                parsed.findings,
            )];
            canonicalize_entries(&mut entries, &root.io_dir, root)?;
            merge(&mut manifest, entries);
            key.sign_manifest(&mut manifest)?;
            ledger.commit(&manifest, &content, new_version, deadline, || {
                content.unchanged(directory, &original, deadline)
            })?;
            self.remember(&root.identity)?;
            let scanners_run = vec![scanner.to_owned()];
            let mut result = scan_payload(&manifest, new_version, &scanners_run, "scanned");
            if !parsed.warnings.is_empty() {
                result["warnings"] = json!(parsed.warnings);
            }
            recovery_event(&mut result, state, "certify", &manifest, &scanners_run);
            result["activation"] = refresh_locked(&ledger, directory, key, root, deadline);
            Ok(result)
        })
    }

    /// Returns none/pass/warn/deny/drifted/tampered without requiring a snapshot.
    ///
    /// # Errors
    /// Reports unavailable keys for existing history, inaccessible source content and deadlines.
    pub fn check(&self, root: &SkillRoot, deadline: Instant) -> Result<Value, GuardError> {
        self.with_locked_directory(root, deadline, |directory| {
            validate_skill(directory)?;
            let empty = Ledger::open(directory, false).and_then(|ledger| match ledger {
                Some(ledger) => ledger_has_history(&ledger, deadline).map(|present| !present),
                None => Ok(true),
            });
            match empty {
                Ok(true) => {
                    let mut result = safe_metadata(root);
                    result["status"] = json!("none");
                    return Ok(result);
                }
                Err(GuardError::Timeout) => return Err(GuardError::Timeout),
                _ => {}
            }
            let key = KeyStore::open(&self.config.state_dir)?.load()?;
            check_locked(directory, &key, root, deadline)
        })
    }

    /// Audits every reserved version, parent signature and optionally snapshot content.
    ///
    /// # Errors
    /// Reports unavailable keys, unsafe directories or deadlines outside per-artifact findings.
    pub fn audit(
        &self,
        root: &SkillRoot,
        verify_snapshots: bool,
        deadline: Instant,
    ) -> Result<Value, GuardError> {
        self.with_locked_directory(root, deadline, |directory| {
            validate_skill(directory)?;
            match Ledger::open(directory, false)? {
                Some(ledger) if ledger_has_history(&ledger, deadline)? => {
                    let key = KeyStore::open(&self.config.state_dir)?.load()?;
                    ledger.audit(&key, &root.identity, verify_snapshots, deadline)
                }
                _ => Ok(json!({"canonicalSkillDir":root.identity,"skillName":root.identity.name(),"valid":true,"versions_checked":0,"errors":[],"message":"No versions found — nothing to audit"})),
            }
        })
    }

    /// Exports a trusted version into an existing, empty directory owned by the authenticated caller.
    ///
    /// The CLI creates the directory as its own user; the daemon never creates arbitrary parents.
    /// `caller_uid` must come from peer credentials, never request JSON.
    ///
    /// # Errors
    /// Rejects unsafe destinations, untrusted versions/snapshots, failed writes and expired deadlines.
    pub fn export(
        &self,
        root: &SkillRoot,
        selector: &str,
        output: &Path,
        caller_uid: u32,
        deadline: Instant,
    ) -> Result<Value, GuardError> {
        SkillIdentity::new(output)?;
        if output.starts_with(&root.io_dir) || output.starts_with(&self.config.state_dir) {
            return Err(GuardError::Invalid(
                "export must be outside Skill and daemon state".into(),
            ));
        }
        let destination = Directory::open(output)?;
        let meta = destination
            .file
            .metadata()
            .map_err(|e| io_error(output, e))?;
        if meta.uid() != caller_uid
            || meta.mode() & 0o022 != 0
            || !destination.names(deadline)?.is_empty()
        {
            return Err(GuardError::Invalid(
                "export directory must be empty, caller-owned and not group/world writable".into(),
            ));
        }
        self.with_skill(root, deadline, |directory, key| {
            let ledger = Ledger::open(directory, false)?.ok_or_else(|| GuardError::Integrity("Skill has no versions".into()))?;
            let selected;
            let selector = if selector == "active" {
                let status = check_locked(directory, key, root, deadline)?;
                let summary = crate::activation::summary(Some(&ledger), key, root, &status, deadline)?;
                selected = summary["activeVersionId"].as_str().ok_or_else(|| GuardError::Integrity("Skill has no active version".into()))?.to_owned();
                selected.as_str()
            } else { selector };
            let manifest = if selector == "latest" {
                ledger.latest(key, &root.identity, true, deadline)?.ok_or_else(|| GuardError::Integrity("Skill has no latest version".into()))?
            } else { ledger.version(selector, key, &root.identity, deadline)? };
            let content = ledger.snapshot(&manifest, deadline)?;
            let snapshot = destination.fresh_child("snapshot")?;
            content.write_owned(&snapshot, deadline, Some(caller_uid))?;
            if Content::capture(&snapshot, true, deadline)?.hashes() != manifest.file_hashes {
                return Err(GuardError::Integrity("export content changed during creation".into()));
            }
            let record = destination.write_atomic("manifest.json", &serde_json::to_vec(&manifest)?, false)?;
            set_owner(&record, caller_uid, &output.join("manifest.json"))?;
            let report = destination.write_atomic("findings.json", &serde_json::to_vec(&findings(&manifest))?, false)?;
            set_owner(&report, caller_uid, &output.join("findings.json"))?;
            destination.verify_path()?;
            Ok(json!({"canonicalSkillDir":root.identity,"skillName":root.identity.name(),"versionId":manifest.version_id,
                "output":output,"snapshot":output.join("snapshot"),"manifest":output.join("manifest.json"),"findings":output.join("findings.json")}))
        })
    }

    pub(crate) fn with_skill<T>(
        &self,
        root: &SkillRoot,
        deadline: Instant,
        operation: impl FnOnce(&Directory, &SigningIdentity) -> Result<T, GuardError>,
    ) -> Result<T, GuardError> {
        self.with_locked_root(root, deadline, |directory, key| {
            validate_skill(directory)?;
            operation(directory, key)
        })
    }

    fn with_locked_root<T>(
        &self,
        root: &SkillRoot,
        deadline: Instant,
        operation: impl FnOnce(&Directory, &SigningIdentity) -> Result<T, GuardError>,
    ) -> Result<T, GuardError> {
        self.with_locked_directory(root, deadline, |directory| {
            let key = KeyStore::open(&self.config.state_dir)?.load()?;
            operation(directory, &key)
        })
    }

    fn with_locked_directory<T>(
        &self,
        root: &SkillRoot,
        deadline: Instant,
        operation: impl FnOnce(&Directory) -> Result<T, GuardError>,
    ) -> Result<T, GuardError> {
        let generation = loop {
            check_deadline(deadline)?;
            match self.generation.try_read() {
                Ok(lock) => break lock,
                Err(TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(5)),
                Err(TryLockError::Poisoned(_)) => return Err(poisoned()),
            }
        };
        self.require_no_rotation(deadline)?;
        let lock = self.skill_lock(&root.identity)?;
        let _guard = timed_lock(&lock, deadline)?;
        let directory = root.open_verified()?;
        let result = operation(&directory);
        drop(generation);
        result
    }

    fn skill_lock(&self, identity: &SkillIdentity) -> Result<Arc<Mutex<()>>, GuardError> {
        let mut locks = self.locks.lock().map_err(|_| poisoned())?;
        locks.retain(|_, value| value.strong_count() != 0);
        if let Some(lock) = locks.get(identity).and_then(Weak::upgrade) {
            return Ok(lock);
        }
        let lock = Arc::new(Mutex::new(()));
        locks.insert(identity.clone(), Arc::downgrade(&lock));
        Ok(lock)
    }

    fn remember(&self, identity: &SkillIdentity) -> Result<(), GuardError> {
        let mut managed = self.managed.lock().map_err(|_| poisoned())?;
        if !managed.contains(identity) {
            let mut updated = managed.clone();
            updated.insert(identity.clone());
            Directory::open(&self.config.state_dir)?.write_atomic(
                "managed-skills.json",
                &serde_json::to_vec(&updated)?,
                true,
            )?;
            *managed = updated;
        }
        Ok(())
    }

    fn noop(
        &self,
        root: &SkillRoot,
        manifest: &Manifest,
        skipped: &[String],
    ) -> Result<Value, GuardError> {
        self.remember(&root.identity)?;
        let mut result = scan_payload(manifest, false, &[], "noop");
        result["skippedScanners"] = json!(skipped);
        Ok(result)
    }
}

pub(crate) fn timed_lock<T>(
    lock: &Mutex<T>,
    deadline: Instant,
) -> Result<MutexGuard<'_, T>, GuardError> {
    loop {
        check_deadline(deadline)?;
        match lock.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(5)),
            Err(TryLockError::Poisoned(_)) => return Err(poisoned()),
        }
    }
}

pub(crate) fn poisoned() -> GuardError {
    GuardError::Integrity("service lock poisoned; restart and reconcile".into())
}

fn validate_skill(directory: &Directory) -> Result<(), GuardError> {
    let stat = rustix::fs::statat(
        &directory.file,
        "SKILL.md",
        rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
    )
    .map_err(|e| io_error(directory.path.join("SKILL.md"), e))?;
    if rustix::fs::FileType::from_raw_mode(stat.st_mode) != rustix::fs::FileType::RegularFile {
        return Err(GuardError::Invalid(
            "Skill requires a regular SKILL.md".into(),
        ));
    }
    Ok(())
}

fn required_ledger(directory: &Directory) -> Result<Ledger, GuardError> {
    Ledger::open(directory, true)?
        .ok_or_else(|| GuardError::Integrity("failed to create ledger directories".into()))
}

fn ledger_has_history(ledger: &Ledger, deadline: Instant) -> Result<bool, GuardError> {
    if !ledger.ids(deadline)?.is_empty() {
        return Ok(true);
    }
    match ledger.meta.read("latest.json", MAX_RECORD_BYTES, deadline) {
        Ok(_) => Ok(true),
        Err(error) if missing(&error) => Ok(false),
        Err(error) => Err(error),
    }
}

fn prepare(
    ledger: &Ledger,
    root: &SkillRoot,
    key: &SigningIdentity,
    content: &Content,
    deadline: Instant,
) -> Result<(Manifest, &'static str, bool), GuardError> {
    let hashes = content.hashes();
    let state = match ledger.latest(key, &root.identity, true, deadline) {
        Ok(Some(manifest)) if manifest.file_hashes == hashes => {
            return Ok((manifest, "verified_signed", false));
        }
        Ok(Some(_)) => "drifted",
        Ok(None) => "missing",
        Err(GuardError::Timeout) => return Err(GuardError::Timeout),
        Err(_) => "tampered",
    };
    let previous = ledger.newest(key, &root.identity, true, deadline)?;
    let mut manifest = Manifest::initial(root.identity.clone(), hashes);
    manifest.version_id = ledger.next_id(previous.as_ref(), deadline)?;
    if let Some(previous) = previous {
        manifest.previous_version_id = Some(previous.version_id);
        manifest.previous_manifest_signature = previous.signature.map(|s| s.value);
        manifest.user_decision = previous
            .user_decision
            .filter(|d| d.action == DecisionAction::AlwaysAllow);
    }
    Ok((manifest, state, true))
}

pub(crate) fn merge(manifest: &mut Manifest, entries: Vec<ScanEntry>) {
    let incoming: BTreeSet<_> = entries.iter().map(|e| e.scanner.clone()).collect();
    let mut seen = BTreeSet::new();
    manifest
        .scans
        .retain(|e| !incoming.contains(&e.scanner) && seen.insert(e.scanner.clone()));
    for entry in entries {
        if seen.insert(entry.scanner.clone()) {
            manifest.scans.push(entry);
        }
    }
    manifest.scan_status = manifest
        .scans
        .iter()
        .map(|s| s.status)
        .max()
        .unwrap_or(ScanStatus::None);
    manifest.updated_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
}

pub(crate) fn findings(manifest: &Manifest) -> Value {
    json!(
        manifest
            .scans
            .iter()
            .flat_map(|s| &s.findings)
            .collect::<Vec<_>>()
    )
}

pub(crate) fn manifest_metadata(manifest: &Manifest) -> Value {
    json!({"canonicalSkillDir":manifest.canonical_skill_dir,"skillName":manifest.skill_name,
        "versionId":manifest.version_id,"createdAt":manifest.created_at,"updatedAt":manifest.updated_at,
        "fileCount":manifest.file_hashes.len(),"manifestHash":manifest.manifest_hash,"userDecision":crate::activation::decision_value(manifest.user_decision.as_ref())})
}

fn safe_metadata(root: &SkillRoot) -> Value {
    json!({"canonicalSkillDir":root.identity,"skillName":root.identity.name(),"versionId":null,
        "createdAt":null,"updatedAt":null,"fileCount":null,"manifestHash":null,"userDecision":null})
}

fn scan_payload(
    manifest: &Manifest,
    new_version: bool,
    scanners: &[String],
    status: &str,
) -> Value {
    let mut value = manifest_metadata(manifest);
    if let Some(object) = value.as_object_mut() {
        object.remove("userDecision");
    }
    value["status"] = json!(status);
    value["scanStatus"] = json!(manifest.scan_status);
    value["newVersion"] = json!(new_version);
    value["scannersRun"] = json!(scanners);
    value
}

fn recovery_event(
    result: &mut Value,
    state: &str,
    operation: &str,
    manifest: &Manifest,
    scanners: &[String],
) {
    if state == "tampered" {
        result["auditEvents"] = json!([{"type":"tampered_recovered","operation":operation,
            "fromStatus":"tampered","toStatus":manifest.scan_status,"versionId":manifest.version_id,
            "manifestHash":manifest.manifest_hash,"scannersRun":scanners}]);
    }
}

fn canonicalize_entries(
    entries: &mut Vec<ScanEntry>,
    stage: &Path,
    root: &SkillRoot,
) -> Result<(), GuardError> {
    let mut value = serde_json::to_value(&*entries)?;
    canonicalize(
        &mut value,
        &stage.to_string_lossy(),
        &root.identity.path().to_string_lossy(),
    );
    canonicalize(
        &mut value,
        &root.io_dir.to_string_lossy(),
        &root.identity.path().to_string_lossy(),
    );
    *entries = serde_json::from_value(value)?;
    Ok(())
}

fn canonicalize(value: &mut Value, physical: &str, canonical: &str) {
    match value {
        Value::String(text) => *text = text.replace(physical, canonical),
        Value::Array(items) => items
            .iter_mut()
            .for_each(|item| canonicalize(item, physical, canonical)),
        Value::Object(items) => items
            .values_mut()
            .for_each(|item| canonicalize(item, physical, canonical)),
        _ => {}
    }
}

#[cfg(test)]
pub(crate) mod tests;

fn check_locked(
    directory: &Directory,
    key: &SigningIdentity,
    root: &SkillRoot,
    deadline: Instant,
) -> Result<Value, GuardError> {
    let mut result = safe_metadata(root);
    let loaded = Ledger::open(directory, false).and_then(|ledger| match ledger {
        Some(ledger) => ledger.latest(key, &root.identity, false, deadline),
        None => Ok(None),
    });
    let manifest = match loaded {
        Ok(Some(manifest)) => manifest,
        Ok(None) => {
            result["status"] = json!("none");
            return Ok(result);
        }
        Err(GuardError::Timeout) => return Err(GuardError::Timeout),
        Err(_) => {
            check_deadline(deadline)?;
            result["status"] = json!("tampered");
            result["reason"] = json!("manifest missing, unauthenticated or inconsistent");
            return Ok(result);
        }
    };
    result = manifest_metadata(&manifest);
    let diff = HashDiff::between(
        &manifest.file_hashes,
        &Content::capture(directory, false, deadline)?.hashes(),
    );
    if diff.matches {
        result["status"] = json!(manifest.scan_status);
        if matches!(manifest.scan_status, ScanStatus::Warn | ScanStatus::Deny) {
            result["findings"] = findings(&manifest);
        }
    } else {
        result["status"] = json!("drifted");
        result["added"] = json!(diff.added);
        result["removed"] = json!(diff.removed);
        result["modified"] = json!(diff.modified);
    }
    Ok(result)
}

mod activation;
mod administration;
mod display;
mod rollback;
use activation::refresh_locked;
