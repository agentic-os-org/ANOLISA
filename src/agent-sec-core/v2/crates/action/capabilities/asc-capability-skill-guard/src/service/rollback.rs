//! A private rollback intent makes interrupted source replacement recoverable on restart.

use super::activation::decision_payload;
use super::{
    SkillGuardService, SkillRoot, canonicalize_entries, check_locked, merge, prepare,
    refresh_locked, required_ledger, validate_skill,
};
use crate::ledger::{
    Ledger,
    content::Content,
    storage::{Directory, MAX_RECORD_BYTES, missing},
};
use crate::scanner::ScanTree;
use crate::{
    DecisionAction, GuardError, Manifest, SigningIdentity, SkillIdentity, check_deadline, io_error,
};
use crate::{FileHashes, UserDecision};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RollbackIntent {
    identity: SkillIdentity,
    io_dir: PathBuf,
    backup_name: String,
    backup_hashes: FileHashes,
    backup_links: std::collections::BTreeMap<String, String>,
    version_id: String,
    manifest_hash: String,
    replace_started: bool,
}

impl SkillGuardService {
    /// Restores an explicit trusted version or the currently active version, with a recovery backup.
    ///
    /// Scanning uses the same captured snapshot that is restored. If replacement or commit fails
    /// before a matching signed version is published, the original regular-file tree is restored.
    ///
    /// # Errors
    /// Rejects untrusted targets, unsafe paths, changed source content, backup and commit failures.
    pub fn rollback(
        &self,
        root: &SkillRoot,
        selector: Option<&str>,
        reason: Option<&str>,
        deadline: Instant,
    ) -> Result<Value, GuardError> {
        self.with_locked_root(root, deadline, |directory, key| {
            self.recover_rollback(root, directory, key, deadline)?;
            validate_skill(directory)?;
            let ledger = required_ledger(directory)?;
            let id = rollback_target(&ledger, directory, key, root, selector, deadline)?;
            let selected = ledger.version(&id, key, &root.identity, deadline)?;
            let content = ledger.snapshot(&selected, deadline)?;
            let original = ScanTree::open(&root.io_dir, deadline)?;
            let backup = Content::capture_backup(directory, deadline)?;
            let links = source_links(directory, deadline)?;
            let (mut manifest, _, new_version) = prepare(&ledger, root, key, &content, deadline)?;
            let snapshot_path = ledger.versions.path.join(format!("{id}.snapshot"));
            let snapshot_tree = ScanTree::open(&snapshot_path, deadline)?;
            let (staging_dir, tree) =
                content.scan_tree(&self.config.state_dir, &snapshot_tree, deadline)?;
            let mut entries = self.registry.scan_tree(&tree, None, deadline)?;
            if entries.is_empty() {
                return Err(GuardError::Scanner(
                    "rollback requires built-in scan results".into(),
                ));
            }
            canonicalize_entries(&mut entries, staging_dir.path(), root)?;
            merge(&mut manifest, entries);
            manifest.user_decision = Some(UserDecision {
                action: DecisionAction::Rollback,
                target_version_id: Some(id),
                reason: reason.map(str::to_owned),
            });
            key.sign_manifest(&mut manifest)?;
            let mut intent = self.begin_rollback(root, &ledger, &backup, &manifest, deadline)?;

            if let Err(error) = backup.unchanged(directory, &original, deadline) {
                self.remove_intent(root, Instant::now() + Duration::from_secs(5))?;
                return Err(error);
            }
            if source_links(directory, deadline)? != links || intent.backup_links != links {
                self.remove_intent(root, Instant::now() + Duration::from_secs(5))?;
                return Err(GuardError::Integrity(
                    "source links changed before rollback".into(),
                ));
            }
            intent.replace_started = true;
            self.write_intent(root, &intent, true)?;
            let outcome = (|| {
                replace_root(directory, &content, deadline)?;
                let restored = ScanTree::open(&root.io_dir, deadline)?;
                ledger.commit(&manifest, &content, new_version, deadline, || {
                    content.unchanged(directory, &restored, deadline)
                })
            })();
            if let Err(error) = outcome {
                let recovery_deadline = Instant::now() + Duration::from_secs(60);
                // A signed version is the commit point, even if latest's atomic replacement failed.
                // Keep its intent for reconciliation instead of undoing a committed rollback.
                if !committed(&intent, &ledger, key, root, recovery_deadline)
                    && let Err(recovery) =
                        self.recover_rollback(root, directory, key, recovery_deadline)
                {
                    return Err(GuardError::Integrity(format!(
                        "{error}; rollback recovery required: {recovery}"
                    )));
                }
                return Err(error);
            }
            self.remove_intent(root, deadline)?;
            let activation = refresh_locked(&ledger, directory, key, root, deadline);
            let mut result = decision_payload(&manifest, &activation, true);
            result["rollbackBackup"] = json!(
                root.identity
                    .path()
                    .join(".skill-meta/backups")
                    .join(intent.backup_name)
            );
            Ok(result)
        })
    }

    fn begin_rollback(
        &self,
        root: &SkillRoot,
        ledger: &Ledger,
        backup: &Content,
        manifest: &Manifest,
        deadline: Instant,
    ) -> Result<RollbackIntent, GuardError> {
        let backups = ledger.meta.child("backups", true)?;
        let name = crate::ledger::storage::nonce("rollback-")?;
        let destination = backups.fresh_child(&name)?;
        backup.write(&destination, deadline)?;
        let hashes = backup.hashes();
        if Content::capture_backup(&destination, deadline)?.hashes() != hashes {
            return Err(GuardError::Integrity(
                "rollback backup changed during creation".into(),
            ));
        }
        let links = source_links(&Directory::open(&root.io_dir)?, deadline)?;
        restore_links(&destination, &links, deadline)?;
        let intent = RollbackIntent {
            identity: root.identity.clone(),
            io_dir: root.io_dir.clone(),
            backup_name: name,
            backup_hashes: hashes,
            backup_links: links,
            version_id: manifest.version_id.clone(),
            manifest_hash: manifest.manifest_hash.clone(),
            replace_started: false,
        };
        self.write_intent(root, &intent, false)?;
        Ok(intent)
    }

    pub(super) fn recover_rollback(
        &self,
        root: &SkillRoot,
        directory: &Directory,
        key: &SigningIdentity,
        deadline: Instant,
    ) -> Result<bool, GuardError> {
        let state = Directory::open(&self.config.state_dir)?;
        let bytes = match state.read(&intent_name(root), MAX_RECORD_BYTES, deadline) {
            Ok(bytes) => bytes,
            Err(error) if missing(&error) => return Ok(false),
            Err(error) => return Err(error),
        };
        let intent: RollbackIntent = serde_json::from_slice(&bytes)?;
        if intent.identity != root.identity
            || intent.io_dir != root.io_dir
            || !valid_backup_name(&intent.backup_name)
        {
            return Err(GuardError::Integrity(
                "rollback intent does not match the resolved Skill".into(),
            ));
        }
        if !intent.replace_started {
            self.remove_intent(root, deadline)?;
            return Ok(true);
        }
        let ledger = required_ledger(directory)?;
        if committed(&intent, &ledger, key, root, deadline) {
            let manifest = ledger.version(&intent.version_id, key, &root.identity, deadline)?;
            ledger
                .meta
                .write_atomic("latest.json", &serde_json::to_vec(&manifest)?, true)?;
        } else {
            let backup = ledger
                .meta
                .child("backups", false)?
                .child(&intent.backup_name, false)?;
            let content = Content::capture_backup(&backup, deadline)?;
            if content.hashes() != intent.backup_hashes
                || source_links(&backup, deadline)? != intent.backup_links
            {
                return Err(GuardError::Integrity(
                    "rollback backup is damaged; source was not overwritten".into(),
                ));
            }
            replace_root(directory, &content, deadline)?;
            restore_links(directory, &intent.backup_links, deadline)?;
        }
        self.remove_intent(root, deadline)?;
        Ok(true)
    }

    fn write_intent(
        &self,
        root: &SkillRoot,
        intent: &RollbackIntent,
        replace: bool,
    ) -> Result<(), GuardError> {
        let bytes = serde_json::to_vec(intent)?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_RECORD_BYTES {
            return Err(GuardError::Invalid(
                "rollback recovery record exceeds 8 MiB".into(),
            ));
        }
        Directory::open(&self.config.state_dir)?.write_atomic(
            &intent_name(root),
            &bytes,
            replace,
        )?;
        Ok(())
    }

    fn remove_intent(&self, root: &SkillRoot, deadline: Instant) -> Result<(), GuardError> {
        Directory::open(&self.config.state_dir)?.remove_child(&intent_name(root), deadline)
    }
}

fn committed(
    intent: &RollbackIntent,
    ledger: &Ledger,
    key: &SigningIdentity,
    root: &SkillRoot,
    deadline: Instant,
) -> bool {
    ledger
        .version(&intent.version_id, key, &root.identity, deadline)
        .is_ok_and(|manifest| {
            manifest.manifest_hash == intent.manifest_hash
                && ledger.snapshot(&manifest, deadline).is_ok()
        })
}

fn intent_name(root: &SkillRoot) -> String {
    format!(
        ".rollback-{}.json",
        crate::integrity::digest(root.identity.path().to_string_lossy().as_bytes())
            .trim_start_matches("sha256:")
    )
}

fn valid_backup_name(name: &str) -> bool {
    name.strip_prefix("rollback-")
        .is_some_and(|suffix| suffix.len() == 64 && suffix.bytes().all(|c| c.is_ascii_hexdigit()))
}

fn replace_root(
    directory: &Directory,
    content: &Content,
    deadline: Instant,
) -> Result<(), GuardError> {
    directory.verify_path()?;
    for name in directory.names(deadline)? {
        if !matches!(name.as_str(), ".git" | ".skill-meta") {
            directory.remove_child(&name, deadline)?;
        }
    }
    content.write(directory, deadline)
}

fn rollback_target(
    ledger: &Ledger,
    directory: &Directory,
    key: &SigningIdentity,
    root: &SkillRoot,
    selector: Option<&str>,
    deadline: Instant,
) -> Result<String, GuardError> {
    if let Some(selector) = selector {
        return Ok(selector.into());
    }
    let status = check_locked(directory, key, root, deadline)?;
    let exposure = crate::activation::summary(Some(ledger), key, root, &status, deadline)?;
    exposure["activeVersionId"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| {
            GuardError::Integrity("cannot choose rollback target without an active version".into())
        })
}

// Backups retain link text without following targets. Special files are rejected before mutation.
fn source_links(
    root: &Directory,
    deadline: Instant,
) -> Result<std::collections::BTreeMap<String, String>, GuardError> {
    fn walk(
        root: &Directory,
        prefix: &str,
        depth: usize,
        remaining: &mut usize,
        links: &mut std::collections::BTreeMap<String, String>,
        deadline: Instant,
    ) -> Result<(), GuardError> {
        if depth > 32 {
            return Err(GuardError::Invalid(
                "rollback tree exceeds depth limit".into(),
            ));
        }
        for name in root.names(deadline)? {
            check_deadline(deadline)?;
            if depth == 0 && matches!(name.as_str(), ".git" | ".skill-meta") {
                continue;
            }
            *remaining = remaining
                .checked_sub(1)
                .ok_or_else(|| GuardError::Invalid("rollback tree exceeds entry limit".into()))?;
            let path = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let stat = rustix::fs::statat(
                &root.file,
                name.as_str(),
                rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
            )
            .map_err(|e| io_error(root.path.join(&name), e))?;
            match rustix::fs::FileType::from_raw_mode(stat.st_mode) {
                rustix::fs::FileType::Directory => walk(
                    &root.child(&name, false)?,
                    &path,
                    depth + 1,
                    remaining,
                    links,
                    deadline,
                )?,
                rustix::fs::FileType::RegularFile => {}
                rustix::fs::FileType::Symlink => {
                    let target = rustix::fs::readlinkat(&root.file, name.as_str(), Vec::new())
                        .map_err(|e| io_error(root.path.join(&name), e))?;
                    let target = target.to_str().map_err(|_| {
                        GuardError::Invalid("rollback link target is not UTF-8".into())
                    })?;
                    if links.len() >= 2000 || target.len() > 4096 {
                        return Err(GuardError::Invalid(
                            "rollback link backup exceeds limit".into(),
                        ));
                    }
                    links.insert(path, target.into());
                }
                _ => {
                    return Err(GuardError::Invalid(
                        "rollback source contains a special file".into(),
                    ));
                }
            }
        }
        root.verify_path()
    }
    let mut links = std::collections::BTreeMap::new();
    walk(root, "", 0, &mut 14000, &mut links, deadline)?;
    Ok(links)
}

fn restore_links(
    root: &Directory,
    links: &std::collections::BTreeMap<String, String>,
    deadline: Instant,
) -> Result<(), GuardError> {
    for (path, target) in links {
        check_deadline(deadline)?;
        let mut pieces = path.split('/').peekable();
        let mut directory = Directory::open(&root.path)?;
        while let Some(name) = pieces.next() {
            if name.is_empty()
                || matches!(name, "." | "..")
                || (directory.path == root.path && matches!(name, ".git" | ".skill-meta"))
            {
                return Err(GuardError::Integrity("invalid rollback link path".into()));
            }
            if pieces.peek().is_some() {
                directory = directory.child(name, false)?;
            } else {
                rustix::fs::symlinkat(target.as_str(), &directory.file, name)
                    .map_err(|e| io_error(directory.path.join(name), e))?;
                directory.sync()?;
            }
        }
    }
    root.verify_path()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::KeyStore;
    use crate::service::tests::{deadline, fixture};
    use std::fs;
    use std::os::unix::fs::symlink;

    #[test]
    fn reconcile_restores_interrupted_root_even_without_skill_md() {
        let (_temp, service, root) = fixture();
        service
            .certify(&root, "fixture", None, &json!([]), deadline())
            .unwrap();
        let directory = Directory::open(&root.io_dir).unwrap();
        let ledger = required_ledger(&directory).unwrap();
        let key = KeyStore::open(&service.config.state_dir)
            .unwrap()
            .load()
            .unwrap();
        fs::create_dir_all(root.io_dir.join("nested/.git")).unwrap();
        fs::write(root.io_dir.join("nested/.git/config"), "nested metadata").unwrap();
        symlink("config", root.io_dir.join("nested/.git/link")).unwrap();
        let backup = Content::capture_backup(&directory, deadline()).unwrap();
        let mut manifest = ledger
            .latest(&key, &root.identity, true, deadline())
            .unwrap()
            .unwrap();
        manifest.version_id = "v000002".into();
        key.sign_manifest(&mut manifest).unwrap();
        symlink("run.sh", root.io_dir.join("shortcut")).unwrap();
        let mut intent = service
            .begin_rollback(&root, &ledger, &backup, &manifest, deadline())
            .unwrap();
        intent.backup_links = source_links(&directory, deadline()).unwrap();
        intent.replace_started = true;
        Directory::open(&service.config.state_dir)
            .unwrap()
            .write_atomic(
                &intent_name(&root),
                &serde_json::to_vec(&intent).unwrap(),
                true,
            )
            .unwrap();
        fs::remove_file(root.io_dir.join("SKILL.md")).unwrap();
        fs::remove_dir_all(root.io_dir.join("nested")).unwrap();
        fs::remove_file(root.io_dir.join("shortcut")).unwrap();
        fs::write(root.io_dir.join("run.sh"), "partial replacement").unwrap();
        let recovered = service.reconcile(&root, deadline()).unwrap();
        assert_eq!(recovered["rollbackRecovered"], true);
        assert_eq!(
            fs::read_to_string(root.io_dir.join("run.sh")).unwrap(),
            "echo safe\n"
        );
        assert_eq!(
            fs::read_link(root.io_dir.join("shortcut")).unwrap(),
            PathBuf::from("run.sh")
        );
        assert!(root.io_dir.join("SKILL.md").exists());
        assert_eq!(
            fs::read_to_string(root.io_dir.join("nested/.git/config")).unwrap(),
            "nested metadata"
        );
        assert_eq!(
            fs::read_link(root.io_dir.join("nested/.git/link")).unwrap(),
            PathBuf::from("config")
        );
        assert!(!service.config.state_dir.join(intent_name(&root)).exists());
    }

    #[test]
    fn prepared_intent_does_not_undo_later_edits_and_corrupt_backup_is_refused() {
        let (_temp, service, root) = fixture();
        service
            .certify(&root, "fixture", None, &json!([]), deadline())
            .unwrap();
        let directory = Directory::open(&root.io_dir).unwrap();
        let ledger = required_ledger(&directory).unwrap();
        let key = KeyStore::open(&service.config.state_dir)
            .unwrap()
            .load()
            .unwrap();
        let backup = Content::capture_backup(&directory, deadline()).unwrap();
        let mut manifest = ledger
            .latest(&key, &root.identity, true, deadline())
            .unwrap()
            .unwrap();
        manifest.version_id = "v000002".into();
        key.sign_manifest(&mut manifest).unwrap();
        service
            .begin_rollback(&root, &ledger, &backup, &manifest, deadline())
            .unwrap();
        fs::write(root.io_dir.join("run.sh"), "later edits").unwrap();
        service.reconcile(&root, deadline()).unwrap();
        assert_eq!(
            fs::read_to_string(root.io_dir.join("run.sh")).unwrap(),
            "later edits"
        );
        let mut intent = service
            .begin_rollback(&root, &ledger, &backup, &manifest, deadline())
            .unwrap();
        intent.replace_started = true;
        Directory::open(&service.config.state_dir)
            .unwrap()
            .write_atomic(
                &intent_name(&root),
                &serde_json::to_vec(&intent).unwrap(),
                true,
            )
            .unwrap();
        fs::write(
            ledger
                .meta
                .path
                .join("backups")
                .join(&intent.backup_name)
                .join("run.sh"),
            "damaged",
        )
        .unwrap();
        assert!(service.reconcile(&root, deadline()).is_err());
        assert_eq!(
            fs::read_to_string(root.io_dir.join("run.sh")).unwrap(),
            "later edits"
        );
        assert!(service.config.state_dir.join(intent_name(&root)).exists());
    }

    #[test]
    fn committed_rollback_repairs_latest_without_restoring_backup() {
        let (_temp, service, root) = fixture();
        service
            .certify(&root, "fixture", None, &json!([]), deadline())
            .unwrap();
        let directory = Directory::open(&root.io_dir).unwrap();
        let ledger = required_ledger(&directory).unwrap();
        let key = KeyStore::open(&service.config.state_dir)
            .unwrap()
            .load()
            .unwrap();
        let backup = Content::capture_backup(&directory, deadline()).unwrap();
        let manifest = ledger
            .latest(&key, &root.identity, true, deadline())
            .unwrap()
            .unwrap();
        let mut intent = service
            .begin_rollback(&root, &ledger, &backup, &manifest, deadline())
            .unwrap();
        intent.replace_started = true;
        Directory::open(&service.config.state_dir)
            .unwrap()
            .write_atomic(
                &intent_name(&root),
                &serde_json::to_vec(&intent).unwrap(),
                true,
            )
            .unwrap();
        fs::remove_file(ledger.meta.path.join("latest.json")).unwrap();
        fs::write(root.io_dir.join("run.sh"), "after commit edit").unwrap();
        service.reconcile(&root, deadline()).unwrap();
        assert_eq!(
            fs::read_to_string(root.io_dir.join("run.sh")).unwrap(),
            "after commit edit"
        );
        assert_eq!(
            ledger
                .latest(&key, &root.identity, true, deadline())
                .unwrap()
                .unwrap(),
            manifest
        );
    }
}
