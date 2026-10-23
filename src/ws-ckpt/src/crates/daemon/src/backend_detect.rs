use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use tracing::{info, warn};

use ws_ckpt_common::backend::{BackendType, StorageBackend};
use ws_ckpt_common::DaemonConfig;

use crate::backends::btrfs_base::{BtrfsBaseBackend, BtrfsBaseScenario};
use crate::backends::btrfs_common;
use crate::backends::btrfs_loop::BtrfsLoopBackend;

/// Result of backend detection, including the backend instance and how it was chosen.
pub struct DetectResult {
    pub backend: Arc<dyn StorageBackend>,
    pub method: String, // "config" or "auto-detect"
}

/// Detect and create the appropriate storage backend based on configuration.
///
/// An explicit type selects that backend. On a fresh auto install, a mount pin
/// selects BtrfsBase; otherwise the first writable physical btrfs mount wins,
/// with BtrfsLoop as the fallback. Restores use create_backend_from_persisted.
pub(crate) async fn detect_and_create_backend(
    config: &DaemonConfig,
) -> anyhow::Result<DetectResult> {
    match config.parse_backend_type() {
        Some(backend_type) => {
            info!(
                "Backend explicitly configured as '{}', skipping auto-detection",
                config.backend_type
            );
            let backend = create_backend(backend_type, config).await?;
            Ok(DetectResult {
                backend,
                method: "config".to_string(),
            })
        }
        None if config.btrfs_mount.is_some() => Ok(DetectResult {
            backend: create_backend(BackendType::BtrfsBase, config).await?,
            method: "config".to_string(),
        }),
        None => {
            // Choose only once on a fresh install; later starts use state.json.
            let backend = match btrfs_common::find_available_btrfs_partition().await {
                Ok(mount) => {
                    let path = Path::new(&mount.mount_point);
                    btrfs_common::validate_btrfs_mount(path).await?;
                    create_btrfs_base(config, path)
                }
                Err(error) => {
                    info!(
                        "No writable btrfs partition detected ({:#}); using BtrfsLoop",
                        error
                    );
                    create_backend(BackendType::BtrfsLoop, config).await?
                }
            };
            Ok(DetectResult {
                backend,
                method: "auto-detect".to_string(),
            })
        }
    }
}

/// Create a backend instance for the given type.
pub(crate) async fn create_backend(
    backend_type: BackendType,
    config: &DaemonConfig,
) -> anyhow::Result<Arc<dyn StorageBackend>> {
    create_backend_from_persisted(backend_type, config, None).await
}

/// Construct a backend using a prior data root only for a same-type restore.
pub(crate) async fn create_backend_from_persisted(
    backend_type: BackendType,
    config: &DaemonConfig,
    persisted_data_root: Option<&Path>,
) -> anyhow::Result<Arc<dyn StorageBackend>> {
    match backend_type {
        BackendType::BtrfsLoop => {
            // Decide effective image path before constructing the backend; this
            // also performs the one-shot legacy → target migration on upgrade.
            // On migration failure we transparently fall back to legacy so the
            // daemon keeps serving — see decide_effective_img_path for the tree.
            let target = PathBuf::from(ws_ckpt_common::BTRFS_IMG_PATH);
            let legacy = PathBuf::from(ws_ckpt_common::LEGACY_BTRFS_IMG_PATH);
            let effective = crate::backends::btrfs_loop::decide_effective_img_path(
                &config.mount_path,
                &target,
                &legacy,
            )
            .await
            .context("Failed to resolve effective btrfs image path")?;
            let backend = BtrfsLoopBackend::new(config.mount_path.clone(), effective);
            Ok(Arc::new(backend))
        }
        BackendType::BtrfsBase => {
            let preferred =
                btrfs_mount_preference(config.btrfs_mount.as_deref(), persisted_data_root)?;
            let mount = match preferred {
                Some(path) => btrfs_common::validate_btrfs_mount(&path).await?,
                None => {
                    let detected = btrfs_common::find_available_btrfs_partition().await
                        .context("BtrfsBase requires a writable btrfs mount; set [backend.btrfs-base] btrfs_mount")?;
                    btrfs_common::validate_btrfs_mount(Path::new(&detected.mount_point)).await?
                }
            };
            Ok(create_btrfs_base(config, Path::new(&mount.mount_point)))
        }
    }
}

fn btrfs_mount_preference(
    configured: Option<&Path>,
    persisted_data_root: Option<&Path>,
) -> anyhow::Result<Option<PathBuf>> {
    if let Some(path) = configured {
        return Ok(Some(path.to_path_buf()));
    }
    let Some(root) = persisted_data_root else {
        return Ok(None);
    };
    // Old manifests put the data root in both mount_path and data_root.
    // Recover the mount from the authoritative data_root without re-detection.
    if !root.is_absolute() || root.file_name() != Some(std::ffi::OsStr::new("ws-ckpt-data")) {
        anyhow::bail!("Persisted BtrfsBase data_root {:?} must be <absolute mount>/ws-ckpt-data; refusing automatic storage selection", root);
    }
    root.parent()
        .map(|path| Some(path.to_path_buf()))
        .context("Persisted BtrfsBase data root has no mount point")
}

fn create_btrfs_base(config: &DaemonConfig, mount: &Path) -> Arc<dyn StorageBackend> {
    if config.mount_path.as_path() != Path::new(ws_ckpt_common::DEFAULT_MOUNT_PATH) {
        warn!("--mount-path is only used by BtrfsLoop; BtrfsBase uses [backend.btrfs-base] btrfs_mount instead");
    }
    info!(
        "Creating BtrfsBase backend at {:?} (CrossDisk default)",
        mount
    );
    Arc::new(BtrfsBaseBackend::new(
        mount.to_path_buf(),
        BtrfsBaseScenario::CrossDisk,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_pin_overrides_persisted_root_and_absence_allows_first_install_detection() {
        assert_eq!(
            btrfs_mount_preference(
                Some(Path::new("/chosen")),
                Some(Path::new("/old/ws-ckpt-data"))
            )
            .unwrap(),
            Some(PathBuf::from("/chosen"))
        );
        assert_eq!(
            btrfs_mount_preference(None, Some(Path::new("/old mount/ws-ckpt-data"))).unwrap(),
            Some(PathBuf::from("/old mount"))
        );
        assert_eq!(btrfs_mount_preference(None, None).unwrap(), None);
    }

    #[test]
    fn invalid_persisted_root_is_not_a_first_install_but_can_be_explicitly_overridden() {
        for root in ["relative/ws-ckpt-data", "/old/wrong-data", "/"] {
            assert!(btrfs_mount_preference(None, Some(Path::new(root))).is_err());
            assert_eq!(
                btrfs_mount_preference(Some(Path::new("/chosen")), Some(Path::new(root))).unwrap(),
                Some(PathBuf::from("/chosen"))
            );
        }
    }

    #[tokio::test]
    async fn configured_or_persisted_unmounted_root_never_falls_back() {
        let temp = tempfile::tempdir().unwrap();
        for explicit in [false, true] {
            let config = DaemonConfig {
                btrfs_mount: explicit.then(|| temp.path().to_path_buf()),
                ..Default::default()
            };
            let root = temp.path().join("ws-ckpt-data");
            let result = create_backend_from_persisted(
                BackendType::BtrfsBase,
                &config,
                (!explicit).then_some(root.as_path()),
            )
            .await;
            assert!(result.is_err());
            assert!(format!("{:#}", result.err().unwrap()).contains("not mounted"));
        }
    }
}
