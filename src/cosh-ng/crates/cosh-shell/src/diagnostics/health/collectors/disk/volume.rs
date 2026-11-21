//! Volume-level statfs facts for declared local mounts, bound without plain open fallback.

use super::super::super::builder::HealthReportBuilder;
use super::super::super::model::{
    HealthFactCategory, HealthFactScope, HealthFactSource, HealthFactValue,
};
use crate::config::HealthConfig;

pub(crate) const MAX_DECLARED_TARGETS: usize = 4;
const MOUNTINFO_MAX_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VolumeUnknown {
    NotMountPoint,
    Autofs,
    UnsupportedFs,
    SubtreeBind,
    ResolveFailed,
    PermissionDenied,
    MountChanged,
    FsMismatch,
    InvalidStats,
    MountinfoUnavailable,
    UnsupportedPlatform,
}

impl VolumeUnknown {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::NotMountPoint => "not_mount_point",
            Self::Autofs => "autofs",
            Self::UnsupportedFs => "unsupported_fs",
            Self::SubtreeBind => "subtree_bind",
            Self::ResolveFailed => "resolve_failed",
            Self::PermissionDenied => "permission_denied",
            Self::MountChanged => "mount_changed",
            Self::FsMismatch => "fs_mismatch",
            Self::InvalidStats => "invalid_stats",
            Self::MountinfoUnavailable => "mountinfo_unavailable",
            Self::UnsupportedPlatform => "unsupported_platform",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ByteStats {
    pub bavail_bytes: u64,
    pub bfree_bytes: u64,
    pub used_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InodeStats {
    pub files: u64,
    pub favail: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VolumeFacts {
    pub mount: String,
    pub fs_type: Option<String>,
    pub device: Option<String>,
    pub fsid: Option<String>,
    pub bytes: Option<ByteStats>,
    pub inodes: Option<InodeStats>,
    pub inode_applicable: bool,
    pub readonly: Option<bool>,
    pub expected_writable: bool,
    pub project_quota: bool,
    pub unknown: Option<VolumeUnknown>,
}

impl VolumeFacts {
    pub(crate) fn proven(&self) -> bool {
        self.unknown.is_none() && self.bytes.is_some()
    }

    fn unknown(mount: &str, reason: VolumeUnknown) -> Self {
        Self {
            mount: mount.to_string(),
            fs_type: None,
            device: None,
            fsid: None,
            bytes: None,
            inodes: None,
            inode_applicable: false,
            readonly: None,
            expected_writable: false,
            project_quota: false,
            unknown: Some(reason),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VolumeSample {
    pub volumes: Vec<VolumeFacts>,
    pub over_limit: usize,
    pub mnt_ns: Option<String>,
    pub supported: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MountEntry {
    pub mnt_id: u64,
    pub dev: String,
    pub root: String,
    pub mount_point: String,
    pub fs_type: String,
    pub project_quota: bool,
}

pub(super) fn decode_mountinfo_field(value: &str) -> String {
    value
        .replace("\\040", " ")
        .replace("\\011", "\t")
        .replace("\\012", "\n")
        .replace("\\134", "\\")
}

pub(super) fn parse_mountinfo(content: &str) -> Vec<MountEntry> {
    content
        .lines()
        .filter_map(|line| {
            let (left, right) = line.split_once(" - ")?;
            let mut left_fields = left.split_whitespace();
            let mnt_id = left_fields.next()?.parse().ok()?;
            let _parent = left_fields.next()?;
            let dev = left_fields.next()?.to_string();
            let root = decode_mountinfo_field(left_fields.next()?);
            let mount_point = decode_mountinfo_field(left_fields.next()?);
            let mount_options = left_fields.next().unwrap_or_default();
            let mut right_fields = right.split_whitespace();
            let fs_type = right_fields.next()?.to_string();
            let super_options = right_fields.nth(1).unwrap_or_default();
            // XFS accounting-only pqnoenforce does not narrow statfs.
            let project_quota = mount_options
                .split(',')
                .chain(super_options.split(','))
                .any(|option| matches!(option, "prjquota" | "pquota"));
            Some(MountEntry {
                mnt_id,
                dev,
                root,
                mount_point,
                fs_type,
                project_quota,
            })
        })
        .collect()
}

pub(super) fn parse_fdinfo_mnt_id(content: &str) -> Option<u64> {
    content.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        (key.trim() == "mnt_id").then(|| value.trim().parse().ok())?
    })
}

/// Raw statfs scalars before validation; kept separate so malformed input has an
/// explicit unknown landing instead of a zero-filled fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RawStatfs {
    pub frsize: u64,
    pub blocks: u64,
    pub bfree: u64,
    pub bavail: u64,
    pub files: u64,
    pub favail: u64,
}

pub(super) fn validate_bytes(raw: &RawStatfs) -> Option<ByteStats> {
    if raw.frsize == 0 || raw.bfree > raw.blocks || raw.bavail > raw.bfree {
        return None;
    }
    if raw.blocks == 0 && raw.bfree == 0 && raw.bavail == 0 {
        return None;
    }
    let bavail_bytes = raw.bavail.checked_mul(raw.frsize)?;
    let bfree_bytes = raw.bfree.checked_mul(raw.frsize)?;
    let used_bytes = (raw.blocks - raw.bfree).checked_mul(raw.frsize)?;
    Some(ByteStats {
        bavail_bytes,
        bfree_bytes,
        used_bytes,
    })
}

pub(super) fn validate_inodes(raw: &RawStatfs) -> Option<InodeStats> {
    if raw.files == 0 || raw.favail > raw.files {
        return None;
    }
    Some(InodeStats {
        files: raw.files,
        favail: raw.favail,
    })
}

pub(super) fn byte_volume_fs(fs_type: &str) -> bool {
    matches!(fs_type, "ext2" | "ext3" | "ext4" | "xfs")
}

pub(super) fn inode_volume_fs(fs_type: &str) -> bool {
    fs_type == "ext4"
}

/// Targets sampled for the enabled disk checks only: `/` belongs to
/// `filesystem.root`, declared mounts to `filesystem.mounts`. Sampling a
/// disabled check's targets would statfs paths the scan no longer reports.
pub(super) fn declared_targets(
    config: &HealthConfig,
    root: bool,
    mounts: bool,
) -> (Vec<String>, usize) {
    let mut targets = Vec::new();
    if root {
        targets.push("/".to_string());
    }
    if mounts {
        for path in config.scan_mounts.as_deref().unwrap_or_default() {
            if path == "/" || targets.iter().any(|existing| existing == path) {
                continue;
            }
            targets.push(path.clone());
        }
    }
    let over_limit = targets.len().saturating_sub(MAX_DECLARED_TARGETS);
    targets.truncate(MAX_DECLARED_TARGETS);
    (targets, over_limit)
}

pub(crate) fn read_volume_sample(config: &HealthConfig, root: bool, mounts: bool) -> VolumeSample {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (config, root, mounts);
        VolumeSample {
            volumes: Vec::new(),
            over_limit: 0,
            mnt_ns: None,
            supported: false,
        }
    }
    #[cfg(target_os = "linux")]
    linux::read(config, root, mounts)
}

fn volume_selected(volume: &VolumeFacts, check_id: Option<&str>) -> bool {
    match check_id {
        Some("filesystem.root") => volume.mount == "/",
        Some("filesystem.mounts") => volume.mount != "/",
        _ => true,
    }
}

/// Whether this check's own targets produced a proven volume; a df failure for
/// one check must not be masked by another check's proven volumes.
pub(crate) fn volume_proven_for_check(sample: &VolumeSample, check_id: &str) -> bool {
    sample
        .volumes
        .iter()
        .any(|volume| volume_selected(volume, Some(check_id)) && volume.proven())
}

pub(crate) fn record_volumes(
    builder: &mut HealthReportBuilder,
    sample: &VolumeSample,
    check_id: Option<&str>,
    elapsed_ms: u128,
) {
    if !sample.supported {
        return;
    }
    let rows = sample
        .volumes
        .iter()
        .filter(|volume| volume_selected(volume, check_id))
        .collect::<Vec<_>>();
    for volume in rows {
        builder.set_scope(HealthFactScope::Path(volume.mount.clone()));
        let prefix = format!("filesystem.mount.{}.", volume.mount);
        let mut add = |name: &str, value: HealthFactValue| {
            builder.add_fact(
                HealthFactCategory::Disk,
                format!("{prefix}{name}"),
                value,
                None,
                HealthFactSource::Statfs,
                elapsed_ms,
            );
        };
        add("volume_proven", HealthFactValue::Bool(volume.proven()));
        if let Some(reason) = volume.unknown {
            add(
                "volume_unknown_reason",
                HealthFactValue::String(reason.label().to_string()),
            );
        }
        if let Some(fs_type) = &volume.fs_type {
            add("volume_fs_type", HealthFactValue::String(fs_type.clone()));
        }
        if let Some(device) = &volume.device {
            add("volume_device", HealthFactValue::String(device.clone()));
        }
        if let Some(fsid) = &volume.fsid {
            add("volume_fsid", HealthFactValue::String(fsid.clone()));
        }
        if let Some(bytes) = &volume.bytes {
            add(
                "volume_bavail_bytes",
                HealthFactValue::Unsigned(bytes.bavail_bytes),
            );
            add(
                "volume_bfree_bytes",
                HealthFactValue::Unsigned(bytes.bfree_bytes),
            );
            add(
                "volume_used_bytes",
                HealthFactValue::Unsigned(bytes.used_bytes),
            );
        }
        if let Some(inodes) = &volume.inodes {
            add("volume_files", HealthFactValue::Unsigned(inodes.files));
            add("volume_favail", HealthFactValue::Unsigned(inodes.favail));
        }
        add(
            "volume_inode_applicable",
            HealthFactValue::Bool(volume.inode_applicable),
        );
        if let Some(readonly) = volume.readonly {
            add("volume_readonly", HealthFactValue::Bool(readonly));
        }
        add(
            "volume_expected_writable",
            HealthFactValue::Bool(volume.expected_writable),
        );
        if volume.project_quota {
            builder.add_fact(
                HealthFactCategory::Disk,
                format!("{prefix}volume_project_quota"),
                HealthFactValue::Bool(true),
                None,
                HealthFactSource::Derived,
                elapsed_ms,
            );
        }
    }
    let summary_check = match check_id {
        Some("filesystem.mounts") => true,
        Some("filesystem.root") => !sample.volumes.iter().any(|v| v.mount != "/"),
        _ => true,
    };
    if summary_check {
        builder.set_scope(HealthFactScope::Host);
        if sample.over_limit > 0 {
            builder.add_fact(
                HealthFactCategory::Disk,
                "filesystem.volume_over_limit",
                HealthFactValue::Unsigned(sample.over_limit as u64),
                None,
                HealthFactSource::Statfs,
                elapsed_ms,
            );
        }
        if let Some(mnt_ns) = &sample.mnt_ns {
            builder.add_fact(
                HealthFactCategory::Disk,
                "filesystem.volume_mnt_ns",
                HealthFactValue::String(mnt_ns.clone()),
                None,
                HealthFactSource::Statfs,
                elapsed_ms,
            );
        }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::fs;
    use std::io::Read;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::io::RawFd;

    const EXT4_SUPER_MAGIC: u64 = 0xEF53;
    const XFS_SB_MAGIC: u64 = 0x5846_5342;

    struct OwnedFd(RawFd);
    impl Drop for OwnedFd {
        fn drop(&mut self) {
            unsafe { nix::libc::close(self.0) };
        }
    }

    fn open_root() -> Option<OwnedFd> {
        let fd = unsafe {
            nix::libc::open(
                c"/".as_ptr(),
                nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC,
            )
        };
        (fd >= 0).then(|| OwnedFd(fd))
    }

    fn openat2_resolve(root: &OwnedFd, relative: &str) -> Result<OwnedFd, VolumeUnknown> {
        #[repr(C)]
        struct OpenHow {
            flags: u64,
            mode: u64,
            resolve: u64,
        }
        let how = OpenHow {
            flags: (nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC) as u64,
            mode: 0,
            resolve: (nix::libc::RESOLVE_IN_ROOT
                | nix::libc::RESOLVE_NO_SYMLINKS
                | nix::libc::RESOLVE_NO_MAGICLINKS),
        };
        let path = std::ffi::CString::new(relative).map_err(|_| VolumeUnknown::ResolveFailed)?;
        let fd = unsafe {
            nix::libc::syscall(
                nix::libc::SYS_openat2,
                root.0,
                path.as_ptr(),
                &how as *const OpenHow,
                std::mem::size_of::<OpenHow>(),
            )
        };
        if fd >= 0 {
            return Ok(OwnedFd(fd as RawFd));
        }
        let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        Err(match errno {
            nix::libc::EACCES | nix::libc::EPERM => VolumeUnknown::PermissionDenied,
            _ => VolumeUnknown::ResolveFailed,
        })
    }

    fn fd_mount_id(fd: &OwnedFd) -> Option<u64> {
        let content = fs::read_to_string(format!("/proc/self/fdinfo/{}", fd.0)).ok()?;
        parse_fdinfo_mnt_id(&content)
    }

    fn statfs_raw(fd: &OwnedFd) -> Option<(RawStatfs, u64, String, u64)> {
        let mut stat: nix::libc::statfs64 = unsafe { std::mem::zeroed() };
        if unsafe { nix::libc::fstatfs64(fd.0, &mut stat) } != 0 {
            return None;
        }
        // statfs64 lacks f_favail; statvfs carries the unprivileged inode count.
        let mut vfs: nix::libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { nix::libc::fstatvfs(fd.0, &mut vfs) } != 0 {
            return None;
        }
        let raw = RawStatfs {
            frsize: vfs.f_frsize as u64,
            blocks: vfs.f_blocks as u64,
            bfree: vfs.f_bfree as u64,
            bavail: vfs.f_bavail as u64,
            files: vfs.f_files as u64,
            favail: vfs.f_favail as u64,
        };
        let identity = nix::sys::stat::fstat(fd.0)
            .ok()
            .map(|st| format!("{}:{}", st.st_dev, st.st_ino))?;
        Some((raw, stat.f_type as u64, identity, stat.f_flags as u64))
    }

    fn expected_magic(fs_type: &str) -> Option<u64> {
        match fs_type {
            "ext2" | "ext3" | "ext4" => Some(EXT4_SUPER_MAGIC),
            "xfs" => Some(XFS_SB_MAGIC),
            _ => None,
        }
    }

    fn read_mountinfo() -> Option<Vec<MountEntry>> {
        let file = fs::File::open("/proc/self/mountinfo").ok()?;
        let mut content = String::new();
        file.take(MOUNTINFO_MAX_BYTES as u64 + 1)
            .read_to_string(&mut content)
            .ok()?;
        if content.len() > MOUNTINFO_MAX_BYTES {
            return None;
        }
        Some(parse_mountinfo(&content))
    }

    fn bind_volume(root: &OwnedFd, entry: &MountEntry, expected_writable: bool) -> VolumeFacts {
        let mount = entry.mount_point.as_str();
        if entry.fs_type == "autofs" {
            return VolumeFacts::unknown(mount, VolumeUnknown::Autofs);
        }
        if !byte_volume_fs(&entry.fs_type) {
            return VolumeFacts::unknown(mount, VolumeUnknown::UnsupportedFs);
        }
        if entry.root != "/" {
            return VolumeFacts::unknown(mount, VolumeUnknown::SubtreeBind);
        }
        let trimmed = mount.trim_start_matches('/');
        let relative = if trimmed.is_empty() { "." } else { trimmed };
        let fd = match openat2_resolve(root, relative) {
            Ok(fd) => fd,
            Err(reason) => return VolumeFacts::unknown(mount, reason),
        };
        if fd_mount_id(&fd) != Some(entry.mnt_id) {
            return VolumeFacts::unknown(mount, VolumeUnknown::MountChanged);
        }
        let Some((raw, f_type, fsid, f_flags)) = statfs_raw(&fd) else {
            return VolumeFacts::unknown(mount, VolumeUnknown::InvalidStats);
        };
        if expected_magic(&entry.fs_type) != Some(f_type) {
            return VolumeFacts::unknown(mount, VolumeUnknown::FsMismatch);
        }
        let readonly = Some(f_flags & nix::libc::ST_RDONLY != 0);
        let bytes = validate_bytes(&raw);
        let inode_applicable = inode_volume_fs(&entry.fs_type);
        let inodes = if inode_applicable {
            validate_inodes(&raw)
        } else {
            None
        };
        if bytes.is_none() {
            return VolumeFacts {
                mount: mount.to_string(),
                fs_type: Some(entry.fs_type.clone()),
                device: Some(entry.dev.clone()),
                fsid: Some(fsid),
                bytes: None,
                inodes: None,
                inode_applicable: false,
                readonly,
                expected_writable,
                project_quota: entry.project_quota,
                unknown: Some(VolumeUnknown::InvalidStats),
            };
        }
        let inode_ok = inode_applicable && inodes.is_some();
        VolumeFacts {
            mount: mount.to_string(),
            fs_type: Some(entry.fs_type.clone()),
            device: Some(entry.dev.clone()),
            fsid: Some(fsid),
            bytes,
            inodes,
            inode_applicable: inode_ok,
            readonly,
            expected_writable,
            project_quota: entry.project_quota,
            unknown: None,
        }
    }

    pub(super) fn read(config: &HealthConfig, root: bool, mounts: bool) -> VolumeSample {
        let (targets, over_limit) = declared_targets(config, root, mounts);
        let Some(entries) = read_mountinfo() else {
            return VolumeSample {
                volumes: targets
                    .iter()
                    .map(|target| VolumeFacts::unknown(target, VolumeUnknown::MountinfoUnavailable))
                    .collect(),
                over_limit,
                mnt_ns: None,
                supported: true,
            };
        };
        let Some(root) = open_root() else {
            return VolumeSample {
                volumes: targets
                    .iter()
                    .map(|target| VolumeFacts::unknown(target, VolumeUnknown::PermissionDenied))
                    .collect(),
                over_limit,
                mnt_ns: None,
                supported: true,
            };
        };
        let volumes = targets
            .iter()
            .map(|target| {
                let Some(entry) = entries.iter().find(|entry| &entry.mount_point == target) else {
                    return VolumeFacts::unknown(target, VolumeUnknown::NotMountPoint);
                };
                let expected_writable = config
                    .expected_writable_mounts
                    .iter()
                    .any(|mount| mount == target);
                bind_volume(&root, entry, expected_writable)
            })
            .collect();
        let mnt_ns = fs::read_link("/proc/self/ns/mnt")
            .ok()
            .map(|link| link.as_os_str().as_bytes().to_vec())
            .and_then(|bytes| String::from_utf8(bytes).ok());
        VolumeSample {
            volumes,
            over_limit,
            mnt_ns,
            supported: true,
        }
    }
}

#[cfg(test)]
mod tests;
