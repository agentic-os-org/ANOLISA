use sha2::{Digest, Sha256};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

const MODULE: &str = "/usr/libexec/anolisa/cosh-ng/cosh-integration.bash";
const MODULE_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../packaging/bash/cosh-integration.bash"
));

fn metadata_token(metadata: &std::fs::Metadata) -> [u64; 10] {
    [
        metadata.dev(),
        metadata.ino(),
        metadata.mode().into(),
        metadata.uid().into(),
        metadata.size(),
        metadata.mtime() as u64,
        metadata.mtime_nsec() as u64,
        metadata.ctime() as u64,
        metadata.ctime_nsec() as u64,
        metadata.nlink(),
    ]
}

fn owned(metadata: &std::fs::Metadata, uid: u32) -> bool {
    metadata.uid() == uid && metadata.mode() & 0o022 == 0 && !metadata.file_type().is_symlink()
}

fn trusted_parent(
    root: &Path,
    home: &Path,
    path: &Path,
    uid: u32,
    system_uid: u32,
) -> Result<(), ()> {
    let relative = path
        .parent()
        .ok_or(())?
        .strip_prefix(root)
        .map_err(|_| ())?;
    let mut directory = root.to_path_buf();
    for component in std::iter::once(None).chain(relative.components().map(Some)) {
        if let Some(component) = component {
            directory.push(component);
        }
        let metadata = match std::fs::symlink_metadata(&directory) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(_) => return Err(()),
        };
        let owner = if directory.starts_with(home) {
            uid
        } else {
            system_uid
        };
        if !metadata.is_dir() || !owned(&metadata, owner) {
            return Err(());
        }
    }
    Ok(())
}

fn file_bytes(path: &Path, uid: u32) -> Result<(Vec<u8>, std::fs::Metadata), ()> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK | nix::libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| ())?;
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        let mut filesystem = std::mem::MaybeUninit::<nix::libc::statfs>::uninit();
        if unsafe { nix::libc::fstatfs(file.as_raw_fd(), filesystem.as_mut_ptr()) } != 0 {
            return Err(());
        }
        let filesystem = unsafe { filesystem.assume_init() };
        if !matches!(
            filesystem.f_type as u64,
            0x01021994 | 0x858458f6 | 0xef53 | 0x58465342 | 0x9123683e | 0xf2f52010
        ) {
            return Err(());
        }
    }
    let before = file.metadata().map_err(|_| ())?;
    if !before.is_file() || !owned(&before, uid) || before.nlink() != 1 || before.len() > 128 * 1024
    {
        return Err(());
    }
    let mut bytes = Vec::new();
    (&file)
        .take(128 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ())?;
    let after = file.metadata().map_err(|_| ())?;
    let named = std::fs::symlink_metadata(path).map_err(|_| ())?;
    if bytes.len() > 128 * 1024
        || metadata_token(&before) != metadata_token(&after)
        || metadata_token(&after) != metadata_token(&named)
    {
        return Err(());
    }
    Ok((bytes, after))
}

fn verify_at(root: &Path, home: &Path, uid: u32, system_uid: u32) -> Result<String, ()> {
    trusted_parent(root, home, home, uid, system_uid)?;
    let home_info = std::fs::symlink_metadata(home).map_err(|_| ())?;
    if !home_info.is_dir() || !owned(&home_info, uid) {
        return Err(());
    }
    let module = root.join(MODULE.trim_start_matches('/'));
    trusted_parent(root, home, &module, uid, system_uid)?;
    let (bytes, metadata) = file_bytes(&module, system_uid)?;
    if bytes != MODULE_BYTES {
        return Err(());
    }
    trusted_parent(root, home, &module, uid, system_uid)?;
    let mut hash = Sha256::new();
    hash.update(super::VERSION.as_bytes());
    hash.update(uid.to_le_bytes());
    hash.update(home.as_os_str().as_encoded_bytes());
    hash.update(Sha256::digest(&bytes));
    hash.update(format!("{:?}", metadata_token(&metadata)));
    Ok(format!("{:x}", hash.finalize()))
}

pub(super) fn verify() -> Result<String, ()> {
    if !cfg!(target_os = "linux") {
        return Err(());
    }
    let uid = unsafe { nix::libc::geteuid() };
    if uid != unsafe { nix::libc::getuid() } {
        return Err(());
    }
    let mut record = std::mem::MaybeUninit::<nix::libc::passwd>::uninit();
    let mut buffer = vec![0u8; 65536];
    let mut result = std::ptr::null_mut();
    if unsafe {
        nix::libc::getpwuid_r(
            uid,
            record.as_mut_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut result,
        )
    } != 0
        || result.is_null()
    {
        return Err(());
    }
    let record = unsafe { record.assume_init() };
    if record.pw_dir.is_null() {
        return Err(());
    }
    let home = unsafe { std::ffi::CStr::from_ptr(record.pw_dir) }
        .to_str()
        .map_err(|_| ())?;
    if !home.starts_with('/') || std::env::var("HOME").as_deref() != Ok(home) {
        return Err(());
    }
    let home = Path::new(home);
    verify_at(Path::new("/"), home, uid, 0)
}

#[cfg(test)]
mod tests;
