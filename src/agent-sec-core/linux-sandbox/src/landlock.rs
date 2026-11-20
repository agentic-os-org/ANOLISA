//! Optional ABI 3 write ceiling installed inside the verified bubblewrap view.

use std::fs::File;
use std::os::unix::fs::MetadataExt;

use asc_sandbox_types::{LandlockMode, LandlockStatus};
use landlock::{
    ABI, AccessFs, CompatLevel, Compatible, PathBeneath, Ruleset, RulesetAttr, RulesetCreatedAttr,
    RulesetStatus,
};
use rustix::fs::{self, Mode, OFlags, ResolveFlags};
use serde::{Deserialize, Serialize};

use crate::error::{Result, SandboxError};

const LANDLOCK_CREATE_RULESET_VERSION: libc::c_uint = 1;

fn failure(message: impl std::fmt::Display) -> SandboxError {
    SandboxError::UnsupportedOperation(format!("Landlock write protection: {message}"))
}

#[derive(Deserialize, Serialize)]
pub(super) struct WriteGrant {
    pub path: String,
    pub device: u64,
    pub inode: u64,
}

#[derive(Deserialize, Serialize)]
pub(super) enum WriteLayer {
    Skipped(String),
    Install { abi: u32, grants: Vec<WriteGrant> },
}

fn kernel_abi() -> std::io::Result<u32> {
    // The crate's compatibility API coalesces probe failures. Preserve errno
    // so auto can distinguish an inactive LSM from an unexpected denial.
    let abi = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<u8>(),
            0usize,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    };
    if abi < 0 {
        return Err(std::io::Error::last_os_error());
    }
    u32::try_from(abi).map_err(std::io::Error::other)
}

pub(super) fn select(mode: LandlockMode) -> Result<Option<WriteLayer>> {
    select_with(mode, kernel_abi)
}

/// Probe in the short-lived helper so inherited layer limits and syscall
/// denials are visible before bubblewrap starts setting up mount namespaces.
pub(super) fn check(mode: LandlockMode) -> Result<()> {
    if let Some(layer) = select(mode)?
        && mode == LandlockMode::Required
    {
        layer.install()?;
    }
    Ok(())
}

fn select_with(
    mode: LandlockMode,
    probe: impl FnOnce() -> std::io::Result<u32>,
) -> Result<Option<WriteLayer>> {
    if mode.is_off() {
        return Ok(None);
    }
    let reason = match probe() {
        Ok(abi @ 3..) => {
            return Ok(Some(WriteLayer::Install {
                abi,
                grants: Vec::new(),
            }));
        }
        Ok(abi @ 1..=2) => format!("ABI {abi} is older than required ABI 3"),
        Ok(0) => return Err(failure("invalid ABI 0")),
        Err(e) if e.raw_os_error() == Some(libc::ENOSYS) => {
            format!("system call unavailable ({e})")
        }
        Err(e) if e.raw_os_error() == Some(libc::EOPNOTSUPP) => format!("LSM disabled ({e})"),
        Err(e) => return Err(failure(format!("ABI probe failed: {e}"))),
    };
    if mode == LandlockMode::Required {
        return Err(failure(reason));
    }
    Ok(Some(WriteLayer::Skipped(reason)))
}

impl WriteLayer {
    pub(super) fn install(&self) -> Result<LandlockStatus> {
        let (abi, grants) = match self {
            Self::Install { abi, grants } => (abi, grants),
            Self::Skipped(reason) => {
                return Ok(LandlockStatus::Skipped {
                    reason: reason.clone(),
                });
            }
        };
        let writes = AccessFs::from_write(ABI::V3);
        let mut rules = Ruleset::default()
            .set_compatibility(CompatLevel::HardRequirement)
            .handle_access(writes)
            .map_err(failure)?
            .create()
            .map_err(failure)?;
        for grant in grants {
            let file = open_path(&grant.path)?;
            let metadata = file.metadata()?;
            if metadata.dev() != grant.device || metadata.ino() != grant.inode {
                return Err(failure(format!("writable object changed: {}", grant.path)));
            }
            let access = if metadata.is_dir() {
                writes
            } else {
                AccessFs::WriteFile | AccessFs::Truncate
            };
            rules = rules
                .add_rule(PathBeneath::new(file, access))
                .map_err(failure)?;
        }
        // Bubblewrap creates this private device mount after all caller mounts.
        // Shell redirection to /dev/null and private PTYs need write access.
        rules = rules
            .add_rule(PathBeneath::new(open_path("/dev")?, writes))
            .map_err(failure)?;
        let status = rules.restrict_self().map_err(failure)?;
        if status.ruleset != RulesetStatus::FullyEnforced {
            return Err(failure(format!("incomplete ruleset: {:?}", status.ruleset)));
        }
        Ok(LandlockStatus::Enforced { abi: *abi })
    }
}

fn open_path(path: &str) -> Result<File> {
    fs::openat2(
        fs::CWD,
        path,
        OFlags::PATH | OFlags::CLOEXEC,
        Mode::empty(),
        ResolveFlags::NO_MAGICLINKS,
    )
    .map(File::from)
    .map_err(|e| failure(format!("opening {path}: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernel_write_ceiling_is_inherited_without_mount_isolation() {
        if std::env::var_os("ASC_LANDLOCK_INTEGRATION").is_none() {
            return;
        }
        if let Some(root) = std::env::var_os("ASC_LANDLOCK_TEST_CHILD") {
            let root = std::path::Path::new(&root);
            let allowed = root.join("allowed");
            let metadata = allowed.metadata().unwrap();
            let layer = WriteLayer::Install {
                abi: kernel_abi().unwrap(),
                grants: vec![WriteGrant {
                    path: allowed.to_str().unwrap().into(),
                    device: metadata.dev(),
                    inode: metadata.ino(),
                }],
            };
            assert!(matches!(
                layer.install().unwrap(),
                LandlockStatus::Enforced { .. }
            ));
            std::fs::write(allowed.join("ok"), b"child").unwrap();
            assert_eq!(
                std::fs::write(root.join("blocked"), b"no")
                    .unwrap_err()
                    .kind(),
                std::io::ErrorKind::PermissionDenied
            );
            let output = std::process::Command::new("/bin/sh")
                .args(["-c", "echo ok > allowed/grandchild && ! echo no > existing"])
                .current_dir(root)
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            return;
        }
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("allowed")).unwrap();
        std::fs::write(root.path().join("existing"), b"original").unwrap();
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "landlock::tests::kernel_write_ceiling_is_inherited_without_mount_isolation",
            ])
            .env("ASC_LANDLOCK_TEST_CHILD", root.path())
            .output()
            .unwrap();
        assert!(child.status.success(), "{child:?}");
        assert_eq!(
            std::fs::read(root.path().join("allowed/grandchild")).unwrap(),
            b"ok\n"
        );
        assert_eq!(
            std::fs::read(root.path().join("existing")).unwrap(),
            b"original"
        );
    }

    #[test]
    fn auto_skips_only_recognized_abi_limitations() {
        assert!(
            select_with(LandlockMode::Off, || panic!("off probed"))
                .unwrap()
                .is_none()
        );
        for errno in [libc::ENOSYS, libc::EOPNOTSUPP] {
            let probe = || Err(std::io::Error::from_raw_os_error(errno));
            assert!(matches!(
                select_with(LandlockMode::Auto, probe).unwrap(),
                Some(WriteLayer::Skipped(_))
            ));
            assert!(select_with(LandlockMode::Required, probe).is_err());
        }
        for abi in [1, 2] {
            assert!(matches!(
                select_with(LandlockMode::Auto, || Ok(abi)).unwrap(),
                Some(WriteLayer::Skipped(_))
            ));
            assert!(select_with(LandlockMode::Required, || Ok(abi)).is_err());
        }
        for mode in [LandlockMode::Auto, LandlockMode::Required] {
            assert!(
                select_with(mode, || Err(std::io::Error::from_raw_os_error(libc::EPERM))).is_err()
            );
            assert!(matches!(
                select_with(mode, || Ok(3)).unwrap(),
                Some(WriteLayer::Install { abi: 3, .. })
            ));
        }
    }
}
