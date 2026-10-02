//! Kernel-backed identity for the writable Btrfs workspace generation.

use std::path::Path;

use anyhow::ensure;
use sha2::{Digest, Sha256};
use ws_ckpt_common::{validate_workspace_id_v2, WorkspaceGenerationTokenV2};

const LIVE_GENERATION_DOMAIN: &[u8] = b"ws-ckpt/live-workspace-generation/v2\0";
const BTRFS_FSID_SIZE: usize = 16;
const BTRFS_UUID_SIZE: usize = 16;

fn token_from_ids(
    fsid: [u8; BTRFS_FSID_SIZE],
    subvolume_uuid: [u8; BTRFS_UUID_SIZE],
) -> anyhow::Result<WorkspaceGenerationTokenV2> {
    ensure!(fsid.iter().any(|byte| *byte != 0), "btrfs FSID is zero");
    ensure!(
        subvolume_uuid.iter().any(|byte| *byte != 0),
        "btrfs subvolume UUID is zero"
    );

    let mut hasher = Sha256::new();
    hasher.update(LIVE_GENERATION_DOMAIN);
    hasher.update(fsid);
    hasher.update(subvolume_uuid);
    Ok(WorkspaceGenerationTokenV2::from_bytes(
        hasher.finalize().into(),
    ))
}

#[cfg(target_os = "linux")]
mod linux {
    use std::fs::{File, OpenOptions};
    use std::mem::{offset_of, size_of};
    use std::os::fd::AsRawFd;
    use std::os::linux::fs::MetadataExt;
    use std::os::unix::fs::OpenOptionsExt;

    use anyhow::Context;

    use super::*;

    const BTRFS_ROOT_INODE: u64 = 256;
    const BTRFS_IOCTL_MAGIC: u8 = 0x94;
    const BTRFS_IOC_FS_INFO_NR: u8 = 31;
    const BTRFS_IOC_GET_SUBVOL_INFO_NR: u8 = 60;
    const BTRFS_VOL_NAME_MAX: usize = 255;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct BtrfsIoctlFsInfoArgs {
        max_id: u64,
        num_devices: u64,
        fsid: [u8; BTRFS_FSID_SIZE],
        nodesize: u32,
        sectorsize: u32,
        clone_alignment: u32,
        csum_type: u16,
        csum_size: u16,
        flags: u64,
        generation: u64,
        metadata_uuid: [u8; BTRFS_FSID_SIZE],
        reserved: [u8; 944],
    }

    impl Default for BtrfsIoctlFsInfoArgs {
        fn default() -> Self {
            Self {
                max_id: 0,
                num_devices: 0,
                fsid: [0; BTRFS_FSID_SIZE],
                nodesize: 0,
                sectorsize: 0,
                clone_alignment: 0,
                csum_type: 0,
                csum_size: 0,
                flags: 0,
                generation: 0,
                metadata_uuid: [0; BTRFS_FSID_SIZE],
                reserved: [0; 944],
            }
        }
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct BtrfsIoctlTimespec {
        sec: u64,
        nsec: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct BtrfsIoctlGetSubvolInfoArgs {
        treeid: u64,
        name: [u8; BTRFS_VOL_NAME_MAX + 1],
        parent_id: u64,
        dirid: u64,
        generation: u64,
        flags: u64,
        uuid: [u8; BTRFS_UUID_SIZE],
        parent_uuid: [u8; BTRFS_UUID_SIZE],
        received_uuid: [u8; BTRFS_UUID_SIZE],
        ctransid: u64,
        otransid: u64,
        stransid: u64,
        rtransid: u64,
        ctime: BtrfsIoctlTimespec,
        otime: BtrfsIoctlTimespec,
        stime: BtrfsIoctlTimespec,
        rtime: BtrfsIoctlTimespec,
        reserved: [u64; 8],
    }

    impl Default for BtrfsIoctlGetSubvolInfoArgs {
        fn default() -> Self {
            Self {
                treeid: 0,
                name: [0; BTRFS_VOL_NAME_MAX + 1],
                parent_id: 0,
                dirid: 0,
                generation: 0,
                flags: 0,
                uuid: [0; BTRFS_UUID_SIZE],
                parent_uuid: [0; BTRFS_UUID_SIZE],
                received_uuid: [0; BTRFS_UUID_SIZE],
                ctransid: 0,
                otransid: 0,
                stransid: 0,
                rtransid: 0,
                ctime: BtrfsIoctlTimespec::default(),
                otime: BtrfsIoctlTimespec::default(),
                stime: BtrfsIoctlTimespec::default(),
                rtime: BtrfsIoctlTimespec::default(),
                reserved: [0; 8],
            }
        }
    }

    nix::ioctl_read!(
        btrfs_ioc_fs_info,
        BTRFS_IOCTL_MAGIC,
        BTRFS_IOC_FS_INFO_NR,
        BtrfsIoctlFsInfoArgs
    );
    nix::ioctl_read!(
        btrfs_ioc_get_subvol_info,
        BTRFS_IOCTL_MAGIC,
        BTRFS_IOC_GET_SUBVOL_INFO_NR,
        BtrfsIoctlGetSubvolInfoArgs
    );

    const BTRFS_IOC_FS_INFO_REQUEST: libc::c_ulong = nix::request_code_read!(
        BTRFS_IOCTL_MAGIC,
        BTRFS_IOC_FS_INFO_NR,
        size_of::<BtrfsIoctlFsInfoArgs>()
    ) as libc::c_ulong;
    const BTRFS_IOC_GET_SUBVOL_INFO_REQUEST: libc::c_ulong = nix::request_code_read!(
        BTRFS_IOCTL_MAGIC,
        BTRFS_IOC_GET_SUBVOL_INFO_NR,
        size_of::<BtrfsIoctlGetSubvolInfoArgs>()
    ) as libc::c_ulong;

    const _: () = {
        assert!(size_of::<BtrfsIoctlFsInfoArgs>() == 1024);
        assert!(offset_of!(BtrfsIoctlFsInfoArgs, max_id) == 0);
        assert!(offset_of!(BtrfsIoctlFsInfoArgs, num_devices) == 8);
        assert!(offset_of!(BtrfsIoctlFsInfoArgs, fsid) == 16);
        assert!(offset_of!(BtrfsIoctlFsInfoArgs, nodesize) == 32);
        assert!(offset_of!(BtrfsIoctlFsInfoArgs, sectorsize) == 36);
        assert!(offset_of!(BtrfsIoctlFsInfoArgs, clone_alignment) == 40);
        assert!(offset_of!(BtrfsIoctlFsInfoArgs, csum_type) == 44);
        assert!(offset_of!(BtrfsIoctlFsInfoArgs, csum_size) == 46);
        assert!(offset_of!(BtrfsIoctlFsInfoArgs, flags) == 48);
        assert!(offset_of!(BtrfsIoctlFsInfoArgs, generation) == 56);
        assert!(offset_of!(BtrfsIoctlFsInfoArgs, metadata_uuid) == 64);
        assert!(offset_of!(BtrfsIoctlFsInfoArgs, reserved) == 80);
        assert!(BTRFS_IOC_FS_INFO_REQUEST == 0x8400_941f);

        assert!(offset_of!(BtrfsIoctlGetSubvolInfoArgs, treeid) == 0);
        assert!(offset_of!(BtrfsIoctlGetSubvolInfoArgs, name) == 8);
        assert!(offset_of!(BtrfsIoctlGetSubvolInfoArgs, parent_id) == 264);
        assert!(offset_of!(BtrfsIoctlGetSubvolInfoArgs, dirid) == 272);
        assert!(offset_of!(BtrfsIoctlGetSubvolInfoArgs, generation) == 280);
        assert!(offset_of!(BtrfsIoctlGetSubvolInfoArgs, flags) == 288);
        assert!(offset_of!(BtrfsIoctlGetSubvolInfoArgs, uuid) == 296);
        assert!(offset_of!(BtrfsIoctlGetSubvolInfoArgs, parent_uuid) == 312);
        assert!(offset_of!(BtrfsIoctlGetSubvolInfoArgs, received_uuid) == 328);
        assert!(offset_of!(BtrfsIoctlGetSubvolInfoArgs, ctransid) == 344);
        assert!(offset_of!(BtrfsIoctlGetSubvolInfoArgs, otransid) == 352);
        assert!(offset_of!(BtrfsIoctlGetSubvolInfoArgs, stransid) == 360);
        assert!(offset_of!(BtrfsIoctlGetSubvolInfoArgs, rtransid) == 368);
        assert!(offset_of!(BtrfsIoctlGetSubvolInfoArgs, ctime) == 376);
        assert!(offset_of!(BtrfsIoctlTimespec, sec) == 0);
        assert!(offset_of!(BtrfsIoctlTimespec, nsec) == 8);
        assert!(
            offset_of!(BtrfsIoctlGetSubvolInfoArgs, otime) == 376 + size_of::<BtrfsIoctlTimespec>()
        );
        assert!(
            offset_of!(BtrfsIoctlGetSubvolInfoArgs, stime)
                == 376 + 2 * size_of::<BtrfsIoctlTimespec>()
        );
        assert!(
            offset_of!(BtrfsIoctlGetSubvolInfoArgs, rtime)
                == 376 + 3 * size_of::<BtrfsIoctlTimespec>()
        );
        assert!(
            offset_of!(BtrfsIoctlGetSubvolInfoArgs, reserved)
                == 376 + 4 * size_of::<BtrfsIoctlTimespec>()
        );
        assert!(
            size_of::<BtrfsIoctlGetSubvolInfoArgs>()
                == offset_of!(BtrfsIoctlGetSubvolInfoArgs, reserved) + size_of::<[u64; 8]>()
        );
        assert!(
            (size_of::<BtrfsIoctlGetSubvolInfoArgs>() == 488
                && BTRFS_IOC_GET_SUBVOL_INFO_REQUEST == 0x81e8_943c)
                || (size_of::<BtrfsIoctlGetSubvolInfoArgs>() == 504
                    && BTRFS_IOC_GET_SUBVOL_INFO_REQUEST == 0x81f8_943c)
        );
    };

    fn open_live_subvolume(data_root: &Path, ws_id: &str) -> anyhow::Result<File> {
        let path = data_root.join(ws_id);
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&path)
            .with_context(|| format!("failed to open live workspace {}", path.display()))
    }

    pub(super) fn live_generation(
        data_root: &Path,
        ws_id: &str,
    ) -> anyhow::Result<WorkspaceGenerationTokenV2> {
        validate_workspace_id_v2(ws_id).map_err(anyhow::Error::msg)?;
        let live = open_live_subvolume(data_root, ws_id)?;
        let metadata = live
            .metadata()
            .context("failed to inspect open live workspace")?;
        ensure!(
            metadata.st_ino() == BTRFS_ROOT_INODE,
            "live workspace root inode is {}, expected btrfs subvolume root inode {}",
            metadata.st_ino(),
            BTRFS_ROOT_INODE
        );

        let fd = live.as_raw_fd();
        let mut fs_info = BtrfsIoctlFsInfoArgs::default();
        // SAFETY: `live` keeps `fd` valid and `fs_info` is the exact writable Linux UAPI layout.
        unsafe { btrfs_ioc_fs_info(fd, &mut fs_info) }
            .context("BTRFS_IOC_FS_INFO failed for live workspace")?;

        let mut subvol_info = BtrfsIoctlGetSubvolInfoArgs::default();
        // SAFETY: The same open directory fd remains valid and `subvol_info` matches the UAPI.
        unsafe { btrfs_ioc_get_subvol_info(fd, &mut subvol_info) }
            .context("BTRFS_IOC_GET_SUBVOL_INFO failed for live workspace")?;

        token_from_ids(fs_info.fsid, subvol_info.uuid)
    }

    #[cfg(test)]
    mod tests {
        use std::os::unix::fs::symlink;

        use super::*;

        #[test]
        fn uapi_layouts_and_requests_match_linux() {
            assert_eq!(size_of::<BtrfsIoctlFsInfoArgs>(), 1024);
            assert_eq!(
                std::mem::align_of::<BtrfsIoctlFsInfoArgs>(),
                std::mem::align_of::<u64>()
            );
            assert_eq!(offset_of!(BtrfsIoctlFsInfoArgs, generation), 56);
            assert_eq!(BTRFS_IOC_FS_INFO_REQUEST, 0x8400_941f);

            assert!(matches!(size_of::<BtrfsIoctlTimespec>(), 12 | 16));
            assert_eq!(
                std::mem::align_of::<BtrfsIoctlGetSubvolInfoArgs>(),
                std::mem::align_of::<u64>()
            );
            assert_eq!(
                offset_of!(BtrfsIoctlGetSubvolInfoArgs, rtime),
                376 + 3 * size_of::<BtrfsIoctlTimespec>()
            );
            match size_of::<BtrfsIoctlGetSubvolInfoArgs>() {
                488 => assert_eq!(BTRFS_IOC_GET_SUBVOL_INFO_REQUEST, 0x81e8_943c),
                504 => assert_eq!(BTRFS_IOC_GET_SUBVOL_INFO_REQUEST, 0x81f8_943c),
                size => panic!("unexpected BtrfsIoctlGetSubvolInfoArgs size: {size}"),
            }
        }

        #[test]
        fn ordinary_directory_fails_closed() {
            let root = tempfile::tempdir().unwrap();
            std::fs::create_dir(root.path().join("ws-abcdef")).unwrap();

            assert!(live_generation(root.path(), "ws-abcdef").is_err());
        }

        #[test]
        fn symlink_fails_closed() {
            let root = tempfile::tempdir().unwrap();
            std::fs::create_dir(root.path().join("target")).unwrap();
            symlink(root.path().join("target"), root.path().join("ws-abcdef")).unwrap();

            assert!(live_generation(root.path(), "ws-abcdef").is_err());
        }

        #[test]
        fn invalid_workspace_id_never_reaches_the_filesystem() {
            let root = tempfile::tempdir().unwrap();
            std::fs::create_dir(root.path().join("escape")).unwrap();

            assert!(live_generation(root.path(), "../escape").is_err());
        }
    }
}

#[cfg(target_os = "linux")]
pub(super) fn live_generation(
    data_root: &Path,
    ws_id: &str,
) -> anyhow::Result<WorkspaceGenerationTokenV2> {
    linux::live_generation(data_root, ws_id)
}

#[cfg(not(target_os = "linux"))]
pub(super) fn live_generation(
    _data_root: &Path,
    _ws_id: &str,
) -> anyhow::Result<WorkspaceGenerationTokenV2> {
    anyhow::bail!("secure btrfs live-generation identity is only available on Linux")
}

#[cfg(test)]
mod tests {
    use super::*;

    const FSID: [u8; BTRFS_FSID_SIZE] = [
        0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
        0xff,
    ];
    const SUBVOL_UUID: [u8; BTRFS_UUID_SIZE] = [
        0xff, 0xee, 0xdd, 0xcc, 0xbb, 0xaa, 0x99, 0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11,
        0x00,
    ];

    #[test]
    fn token_matches_fixed_vector() {
        let token = token_from_ids(FSID, SUBVOL_UUID).unwrap();

        assert_eq!(
            hex::encode(token.as_bytes()),
            "57f4a595e2283b4fda2f746d09b21911b6965f72e33441bb78ee7d1f78fcddff"
        );
    }

    #[test]
    fn token_is_sensitive_to_fsid_and_subvolume_uuid() {
        let base = token_from_ids(FSID, SUBVOL_UUID).unwrap();
        let mut other_fsid = FSID;
        other_fsid[0] ^= 1;
        let mut other_uuid = SUBVOL_UUID;
        other_uuid[0] ^= 1;

        assert_ne!(base, token_from_ids(other_fsid, SUBVOL_UUID).unwrap());
        assert_ne!(base, token_from_ids(FSID, other_uuid).unwrap());
    }

    #[test]
    fn zero_fsid_and_uuid_are_rejected() {
        assert!(token_from_ids([0; BTRFS_FSID_SIZE], SUBVOL_UUID).is_err());
        assert!(token_from_ids(FSID, [0; BTRFS_UUID_SIZE]).is_err());
    }
}
