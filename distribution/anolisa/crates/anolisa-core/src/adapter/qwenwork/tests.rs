//! Ownership and interrupted-copy tests independent of the installed desktop.

use super::super::driver::{AdapterOps, CliOutput};
use super::super::managed_files::{
    ManagedFile, ManagedInventory, ManagedInventoryKind, materialized_files, source_revision,
};
use super::*;
use anolisa_platform::fs_layout::FsLayout;
use std::cell::Cell;

struct CopyOps {
    copies: Cell<usize>,
    fail_at: Cell<Option<usize>>,
}

impl AdapterOps for CopyOps {
    fn run_framework_cli(&self, _: FrameworkCommand) -> Result<CliOutput, AdapterError> {
        panic!("must not spawn a process")
    }
    fn copy_tree(&self, _: &Path, _: &Path) -> Result<(), AdapterError> {
        panic!("must use managed file copies")
    }
    fn copy_file(&self, src: &Path, dst: &Path) -> Result<(), AdapterError> {
        let count = self.copies.get();
        self.copies.set(count + 1);
        if self.fail_at.get() == Some(count) {
            return Err(invalid(dst, "injected copy failure"));
        }
        std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
        std::fs::copy(src, dst).unwrap();
        Ok(())
    }
    fn remove_path(&self, path: &Path) -> Result<bool, AdapterError> {
        if !path.exists() {
            return Ok(false);
        }
        if path.is_dir() {
            std::fs::remove_dir(path).unwrap();
        } else {
            std::fs::remove_file(path).unwrap();
        }
        Ok(true)
    }
    fn remove_tree(&self, _: &Path) -> Result<bool, AdapterError> {
        panic!("must not remove whole trees")
    }
    fn write_file(&self, _: &Path, _: &[u8]) -> Result<(), AdapterError> {
        panic!("must not write settings or markers")
    }
    fn create_symlink(&self, _: &Path, _: &Path) -> Result<(), AdapterError> {
        panic!("must copy self-contained resources")
    }
    fn read_file(&self, path: &Path) -> Result<Option<Vec<u8>>, AdapterError> {
        Ok(std::fs::read(path).ok())
    }
}

struct Fixture {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    source: PathBuf,
    layout: FsLayout,
    ops: CopyOps,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().canonicalize().unwrap();
        let source = home.join("source");
        let layout = FsLayout::user(home.clone());
        let fixture = Self {
            _tmp: tmp,
            home,
            source,
            layout,
            ops: CopyOps {
                copies: Cell::new(0),
                fail_at: Cell::new(None),
            },
        };
        fixture.write_revision("old");
        fixture
    }

    fn write_revision(&self, value: &str) {
        for file in RUNTIME_FILES {
            let path = self.source.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, value).unwrap();
        }
    }

    fn ctx(&self) -> DriverCtx<'_> {
        DriverCtx {
            component: "tokenless".to_string(),
            framework: "qwenwork".to_string(),
            layout: &self.layout,
            resource_root: self.source.clone(),
            user_home: Some(self.home.clone()),
            declared_plugin_id: Some("tokenless".to_string()),
            requested_profiles: Vec::new(),
            adapter_type: Some("plugin".to_string()),
            declared_skills: Vec::new(),
            declared_config: Vec::new(),
            declared_bundle_entry: None,
            framework_version_req: None,
            allow_unsafe_plugin_install: false,
            no_accept_capabilities: false,
            dry_run: false,
            ops: &self.ops,
        }
    }

    fn claim(&self) -> AdapterClaim {
        let mappings = QwenWorkDriver.materialized_mappings(&self.source, Some("plugin"), &[]);
        let mut files = RUNTIME_FILES
            .iter()
            .map(|file| {
                let path = self.source.join(file);
                ManagedFile {
                    sha256: Some(format!(
                        "{:x}",
                        Sha256::digest(std::fs::read(&path).unwrap())
                    )),
                    path,
                    kind: ManagedInventoryKind::File,
                    symlink_target: None,
                }
            })
            .collect::<Vec<_>>();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        let inventory = ManagedInventory { files };
        AdapterClaim {
            claim_schema: CLAIM_SCHEMA_VERSION,
            component: "tokenless".to_string(),
            framework: "qwenwork".to_string(),
            plugin_id: Some("tokenless".to_string()),
            adapter_type: Some("plugin".to_string()),
            enabled_at: now_iso8601(),
            resource_root: self.source.clone(),
            bundle_digest: None,
            source_revision: Some(source_revision(&inventory, &self.source, &mappings).unwrap()),
            materialized_files: materialized_files(&inventory, &mappings).unwrap(),
            driver_schema: DRIVER_SCHEMA_VERSION,
            status: ClaimStatus::CleanupFailed,
            notices: Vec::new(),
            resources: vec![ClaimResource {
                id: RES_PLUGIN.to_string(),
                purpose: "qwenwork_plugin_dir".to_string(),
                kind: ClaimResourceKind::ExternalPath {
                    path: self.home.join(".qwenworkcn/plugins-custom/tokenless"),
                },
            }],
            driver_payload: DriverPayload::QwenWork(QwenWorkClaim {
                plugin_resource: RES_PLUGIN.to_string(),
                installation_started: false,
                previous_files: Vec::new(),
            }),
        }
    }

    fn apply(&self, claim: &mut AdapterClaim) -> Result<(), AdapterError> {
        QwenWorkDriver.apply_enable(
            claim,
            &PreparedEnable::QwenWork {
                warnings: Vec::new(),
            },
            &self.ctx(),
            &mut (),
        )
    }
}

#[test]
fn interrupted_first_install_can_retry_or_disable_without_source() {
    for retry in [false, true] {
        let f = Fixture::new();
        let mut claim = f.claim();
        f.ops.fail_at.set(Some(2));
        assert!(f.apply(&mut claim).is_err());
        assert_eq!(claim.status, ClaimStatus::CleanupFailed);
        assert!(receipt(&claim).unwrap().1.installation_started);
        let encoded = toml::to_string(&claim).unwrap();
        let restored: AdapterClaim = toml::from_str(&encoded).unwrap();
        assert_eq!(restored, claim);
        restored
            .validate(&f.layout, &[f.home.join(".qwenworkcn")])
            .unwrap();
        if retry {
            let mut next = f.claim();
            QwenWorkDriver
                .preserve_reenable_facts(&claim, &mut next)
                .unwrap();
            f.ops.fail_at.set(None);
            f.apply(&mut next).unwrap();
            assert_eq!(next.status, ClaimStatus::Enabled);
            claim = next;
        }
        std::fs::remove_dir_all(&f.source).unwrap();
        let report = QwenWorkDriver.disable(&claim, &f.ctx()).unwrap();
        assert!(report.cleanup_complete);
        assert!(!receipt(&claim).unwrap().0.exists());
    }
}

#[test]
fn interrupted_upgrade_preserves_both_revisions_across_repeated_failure() {
    let f = Fixture::new();
    let mut old = f.claim();
    f.apply(&mut old).unwrap();
    f.write_revision("new");
    let mut next = f.claim();
    QwenWorkDriver
        .preserve_reenable_facts(&old, &mut next)
        .unwrap();
    f.ops.fail_at.set(Some(f.ops.copies.get() + 2));
    assert!(f.apply(&mut next).is_err());
    verify_owned(&next, true).unwrap();
    let root = receipt(&next).unwrap().0;
    assert_eq!(
        std::fs::read_to_string(root.join(".qoder-plugin/plugin.json")).unwrap(),
        "new"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("hooks/shared/hook_utils.py")).unwrap(),
        "old"
    );
    f.write_revision("newer");
    let mut retry = f.claim();
    QwenWorkDriver
        .preserve_reenable_facts(&next, &mut retry)
        .unwrap();
    f.ops.fail_at.set(Some(f.ops.copies.get() + 1));
    assert!(f.apply(&mut retry).is_err());
    verify_owned(&retry, true).unwrap();
    let restored: AdapterClaim = toml::from_str(&toml::to_string(&retry).unwrap()).unwrap();
    std::fs::remove_dir_all(&f.source).unwrap();
    assert!(
        QwenWorkDriver
            .disable(&restored, &f.ctx())
            .unwrap()
            .cleanup_complete
    );
    assert!(!receipt(&restored).unwrap().0.exists());
}

#[test]
fn forged_previous_relative_path_is_rejected_before_cleanup() {
    let f = Fixture::new();
    let mut claim = f.claim();
    let mut file = claim.materialized_files[0].clone();
    file.relative_path = PathBuf::from("../../outside");
    if let DriverPayload::QwenWork(payload) = &mut claim.driver_payload {
        payload.previous_files.push(file);
    }
    assert!(
        claim
            .validate(&f.layout, &[f.home.join(".qwenworkcn")])
            .is_err()
    );
    assert!(owned_versions(&claim).is_err());
}

#[test]
fn successful_install_does_not_silently_restore_deleted_managed_files() {
    let f = Fixture::new();
    let mut claim = f.claim();
    f.apply(&mut claim).unwrap();
    std::fs::remove_file(receipt(&claim).unwrap().0.join("hooks/hooks.json")).unwrap();
    assert!(
        QwenWorkDriver
            .preserve_reenable_facts(&claim, &mut f.claim())
            .is_err()
    );
    assert!(
        QwenWorkDriver
            .disable(&claim, &f.ctx())
            .unwrap()
            .cleanup_complete
    );
}

#[test]
fn application_metadata_accepts_xml_and_binary_and_rejects_bad_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("Info.plist");
    let info = plist::Value::Dictionary(plist::Dictionary::from_iter([
        (
            "CFBundleIdentifier".to_string(),
            plist::Value::String("cn.qwenwork.desktop.mac".to_string()),
        ),
        (
            "CFBundleShortVersionString".to_string(),
            plist::Value::String("1.2.5".to_string()),
        ),
    ]));
    info.to_file_xml(&path).unwrap();
    assert_eq!(application_version(&path).unwrap(), "1.2.5");
    info.to_file_binary(&path).unwrap();
    assert_eq!(application_version(&path).unwrap(), "1.2.5");
    plist::Value::Dictionary(plist::Dictionary::new())
        .to_file_xml(&path)
        .unwrap();
    assert!(
        application_version(&path)
            .unwrap_err()
            .contains("bundle identifier")
    );
    std::fs::write(&path, "not a plist").unwrap();
    assert!(application_version(&path).is_err());
}

#[cfg(not(target_os = "macos"))]
#[test]
fn non_macos_detection_rejects_sideloading() {
    assert!(!QwenWorkDriver.detect(&HostEnv { user_home: None }).detected);
}

#[test]
fn preexisting_directory_after_preparation_is_not_claimed_or_removed() {
    let f = Fixture::new();
    let mut claim = f.claim();
    let root = receipt(&claim).unwrap().0.to_path_buf();
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("foreign"), "user").unwrap();
    assert!(f.apply(&mut claim).is_err());
    assert!(!receipt(&claim).unwrap().1.installation_started);
    assert_eq!(f.ops.copies.get(), 0);
    assert!(
        QwenWorkDriver
            .disable(&claim, &f.ctx())
            .unwrap()
            .cleanup_complete
    );
    assert_eq!(
        std::fs::read_to_string(root.join("foreign")).unwrap(),
        "user"
    );
}

#[test]
fn untracked_required_resource_is_rejected_before_writing() {
    let f = Fixture::new();
    let mut claim = f.claim();
    claim
        .materialized_files
        .retain(|file| file.relative_path != Path::new("hooks/shared/hook_utils.py"));
    assert!(QwenWorkDriver.validate_prepared_enable(&claim).is_err());
    assert_eq!(f.ops.copies.get(), 0);
    assert!(!receipt(&claim).unwrap().0.exists());
}
