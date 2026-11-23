//! Machine-bound reversible encryption for dashboard-managed secrets.
//!
//! Secrets entered on the dashboard settings page (LLM API keys) must never
//! reach disk as plaintext — file permissions alone do not survive backups,
//! image snapshots, or root-level compromise. Values are sealed with
//! AES-256-GCM using a key derived from the host machine-id plus a
//! per-install random salt (stored next to the config with mode 0o400), so a
//! copied config file cannot be decrypted off-host.
//!
//! Linux-only by construction: the parent `server` module is cfg-gated in
//! lib.rs and `openssl` is a Linux-only dependency of this crate. The macOS
//! local viewer keeps its own plaintext config by design (per-user directory,
//! non-root) and must not reuse this module.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use openssl::rand::rand_bytes;
use openssl::symm::{Cipher, decrypt_aead, encrypt_aead};
use sha2::{Digest, Sha256};

/// Envelope prefix marking a sealed value; bump the version on format changes.
const SEALED_PREFIX: &str = "enc:v1:";
/// Per-install salt file name, created next to the config file.
const SALT_FILE_NAME: &str = ".secret_salt";
const SALT_LEN: usize = 32;
const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;
/// Domain separation for key derivation and GCM authentication.
const KEY_CONTEXT: &[u8] = b"agentsight.optimization_config.v1";

/// Returns whether `value` carries the sealed-envelope prefix.
pub fn is_sealed(value: &str) -> bool {
    value.starts_with(SEALED_PREFIX)
}

/// Encrypt `plaintext` into an `enc:v1:<b64 nonce>:<b64 ciphertext+tag>`
/// envelope bound to this host. Creates the salt file on first use.
///
/// # Errors
///
/// Fails when the salt file cannot be created or read, or when the OpenSSL
/// primitives reject the operation.
pub fn seal(plaintext: &str, base_dir: &Path) -> io::Result<String> {
    let key = derive_key(base_dir, true)?;
    let mut nonce = [0u8; NONCE_LEN];
    rand_bytes(&mut nonce).map_err(crypto_err)?;
    let mut tag = [0u8; TAG_LEN];
    let mut ciphertext = encrypt_aead(
        Cipher::aes_256_gcm(),
        &key,
        Some(&nonce),
        KEY_CONTEXT,
        plaintext.as_bytes(),
        &mut tag,
    )
    .map_err(crypto_err)?;
    ciphertext.extend_from_slice(&tag);
    Ok(format!(
        "{SEALED_PREFIX}{}:{}",
        BASE64.encode(nonce),
        BASE64.encode(&ciphertext)
    ))
}

/// Decrypt a sealed envelope produced by [`seal`].
///
/// Returns `None` when the envelope is malformed, the salt file is missing,
/// or the value was sealed on another host — callers should treat that as
/// "not configured" and ask the user to re-enter the secret.
pub fn unseal(value: &str, base_dir: &Path) -> Option<String> {
    let rest = value.strip_prefix(SEALED_PREFIX)?;
    let (nonce_b64, ct_b64) = rest.split_once(':')?;
    let nonce = BASE64.decode(nonce_b64).ok()?;
    let data = BASE64.decode(ct_b64).ok()?;
    if nonce.len() != NONCE_LEN || data.len() < TAG_LEN {
        return None;
    }
    let (ciphertext, tag) = data.split_at(data.len() - TAG_LEN);
    let key = derive_key(base_dir, false).ok()?;
    let plain = decrypt_aead(
        Cipher::aes_256_gcm(),
        &key,
        Some(&nonce),
        KEY_CONTEXT,
        ciphertext,
        tag,
    )
    .ok()?;
    String::from_utf8(plain).ok()
}

fn crypto_err(e: openssl::error::ErrorStack) -> io::Error {
    io::Error::other(e.to_string())
}

/// Derive the AES-256 key: SHA-256(context ‖ machine-id ‖ salt).
///
/// The machine-id binds ciphertexts to the host; the salt keeps the key
/// unpredictable even where /etc/machine-id is world-readable, and is the
/// sole key material in environments without a machine-id (containers).
fn derive_key(base_dir: &Path, create_salt: bool) -> io::Result<[u8; 32]> {
    let salt = load_or_create_salt(&base_dir.join(SALT_FILE_NAME), create_salt)?;
    let mut hasher = Sha256::new();
    hasher.update(KEY_CONTEXT);
    match machine_id() {
        Some(id) => hasher.update(id.as_bytes()),
        // Surface the degraded binding mode so operators can tell
        // machine-bound and salt-only installs apart when auditing.
        None => log::info!(
            "machine-id unavailable; optimization secret key derives from the per-install salt only"
        ),
    }
    hasher.update(&salt);
    Ok(hasher.finalize().into())
}

static TEMP_SALT_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Open an existing regular salt file without blocking or following symlinks, and
/// bound the read to `SALT_LEN + 1` bytes before validating its exact length.
fn read_salt_nofollow(path: &Path) -> io::Result<Vec<u8>> {
    let mut opts = OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    }
    let mut file = opts.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("salt path {} is not a regular file", path.display()),
        ));
    }
    let mut salt = Vec::with_capacity(SALT_LEN + 1);
    (&mut file)
        .take((SALT_LEN + 1) as u64)
        .read_to_end(&mut salt)?;
    if salt.len() == SALT_LEN {
        Ok(salt)
    } else {
        Err(io::Error::other(format!(
            "corrupt salt file {} (expected {SALT_LEN} bytes)",
            path.display()
        )))
    }
}

/// Exclusively create `target` with mode `0o400` without following symlinks, write `salt`,
/// and flush it to disk before returning.
fn create_exclusively_file(target: &Path, salt: &[u8]) -> io::Result<()> {
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o400)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let mut file: File = opts.open(target)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o400))?;
    }
    file.write_all(salt)?;
    file.sync_all()?;
    Ok(())
}

fn remove_staging_file(tmp_path: &Path) {
    if let Err(err) = std::fs::remove_file(tmp_path) {
        if err.kind() != io::ErrorKind::NotFound {
            log::warn!(
                "failed to remove staging salt file {}: {err}",
                tmp_path.display()
            );
        }
    }
}

fn sync_parent_dir(parent: &Path) {
    match File::open(parent).and_then(|dir| dir.sync_all()) {
        Ok(()) => {}
        Err(err) => {
            log::debug!(
                "parent directory fsync skipped for {}: {err}",
                parent.display()
            );
        }
    }
}

/// Fallback exclusive creation when `hard_link` is unsupported on the target filesystem
/// (e.g. `EOPNOTSUPP` / `ENOSYS` or Linux VFS `EPERM` for non-hardlink filesystems).
fn write_salt_direct_fallback(path: &Path, salt: &[u8]) -> io::Result<Option<Vec<u8>>> {
    match create_exclusively_file(path, salt) {
        Ok(()) => {
            if let Some(parent) = path.parent() {
                sync_parent_dir(parent);
            }
            Ok(Some(salt.to_vec()))
        }
        Err(direct_err) if direct_err.kind() == io::ErrorKind::AlreadyExists => Ok(None),
        Err(direct_err) => Err(direct_err),
    }
}

/// Write `salt` into a unique `0o400` staging file and publish it atomically via `hard_link`
/// so concurrent writers cannot overwrite an earlier salt and other users can never read it
/// even transiently. Falls back to direct `O_CREAT | O_EXCL | O_NOFOLLOW` when the
/// underlying filesystem rejects hard links (`Unsupported` or `PermissionDenied`).
fn write_salt_exclusively(path: &Path, salt: &[u8]) -> io::Result<Option<Vec<u8>>> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let seq = TEMP_SALT_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut rand_tag = [0u8; 4];
    rand_bytes(&mut rand_tag).map_err(crypto_err)?;
    let tag = u32::from_le_bytes(rand_tag);
    let tmp_path = parent.join(format!(
        "{SALT_FILE_NAME}.{}.{seq}.{tag:08x}.tmp",
        std::process::id()
    ));

    if let Err(e) = create_exclusively_file(&tmp_path, salt) {
        remove_staging_file(&tmp_path);
        return Err(e);
    }

    let link_res = std::fs::hard_link(&tmp_path, path);
    remove_staging_file(&tmp_path);

    match link_res {
        Ok(()) => {
            sync_parent_dir(parent);
            Ok(Some(salt.to_vec()))
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(None),
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::Unsupported | io::ErrorKind::PermissionDenied
            ) =>
        {
            log::warn!(
                "atomic hard_link for {} unavailable ({e}); falling back to exclusive direct create",
                path.display()
            );
            write_salt_direct_fallback(path, salt)
        }
        Err(e) => Err(e),
    }
}

/// Read the per-install salt without following symlinks, creating it atomically with
/// mode 0o400 when absent and `create` is set. Refusing to create during decryption
/// ensures a config copied without its salt file never silently "decrypts" to garbage keys.
fn load_or_create_salt(path: &Path, create: bool) -> io::Result<Vec<u8>> {
    match read_salt_nofollow(path) {
        Ok(salt) => Ok(salt),
        Err(e) if e.kind() == io::ErrorKind::NotFound && create => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut salt = vec![0u8; SALT_LEN];
            rand_bytes(&mut salt).map_err(crypto_err)?;
            match write_salt_exclusively(path, &salt)? {
                Some(created) => Ok(created),
                None => read_salt_nofollow(path),
            }
        }
        Err(e) => Err(e),
    }
}

/// Host identity per machine-id(5); `None` in containers or minimal images,
/// where the per-install salt alone still keeps keys out of plaintext.
fn machine_id() -> Option<String> {
    for path in ["/etc/machine-id", "/var/lib/dbus/machine-id"] {
        if let Ok(content) = std::fs::read_to_string(path) {
            let id = content.trim();
            if !id.is_empty() {
                return Some(id.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn tmp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("opt-secret-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn seal_unseal_roundtrip() {
        let dir = tmp_dir("roundtrip");
        let sealed = seal("sk-test-1234567890", &dir).unwrap();
        assert!(is_sealed(&sealed));
        assert!(!sealed.contains("sk-test"));
        assert_eq!(unseal(&sealed, &dir).as_deref(), Some("sk-test-1234567890"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unseal_fails_with_foreign_salt() {
        let dir_a = tmp_dir("salt-a");
        let dir_b = tmp_dir("salt-b");
        let sealed = seal("sk-secret", &dir_a).unwrap();
        // Simulate a config copied to another install: same envelope, other salt.
        seal("warm-up", &dir_b).unwrap();
        assert_eq!(unseal(&sealed, &dir_b), None);
        let _ = std::fs::remove_dir_all(&dir_a);
        let _ = std::fs::remove_dir_all(&dir_b);
    }

    #[test]
    fn unseal_fails_when_salt_missing_or_tampered() {
        let dir = tmp_dir("tamper");
        let sealed = seal("sk-secret", &dir).unwrap();

        // Flip one ciphertext character — GCM authentication must reject it.
        let mut chars: Vec<char> = sealed.chars().collect();
        let last = chars.len() - 1;
        chars[last] = if chars[last] == 'A' { 'B' } else { 'A' };
        let tampered: String = chars.into_iter().collect();
        assert_eq!(unseal(&tampered, &dir), None);

        // Losing the salt must fail decryption, not regenerate a new salt.
        std::fs::remove_file(dir.join(SALT_FILE_NAME)).unwrap();
        assert_eq!(unseal(&sealed, &dir), None);
        assert!(!dir.join(SALT_FILE_NAME).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_malformed_envelopes() {
        let dir = tmp_dir("malformed");
        for bad in ["plain-key", "enc:v1:", "enc:v1:notb64", "enc:v1:AAAA:!!"] {
            assert_eq!(unseal(bad, &dir), None, "must reject {bad:?}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn salt_file_is_owner_read_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tmp_dir("perm");
        seal("sk-secret", &dir).unwrap();
        let mode = std::fs::metadata(dir.join(SALT_FILE_NAME))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o400);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn dangling_symlink_at_salt_path_is_rejected_without_creating_target() {
        let dir = tmp_dir("symlink");
        let outside = dir.join("outside_target");
        let salt_dir = dir.join("store");
        std::fs::create_dir_all(&salt_dir).unwrap();
        std::os::unix::fs::symlink(&outside, salt_dir.join(SALT_FILE_NAME)).unwrap();

        let err = seal("sk-secret", &salt_dir).expect_err("must reject symlink salt target");
        assert!(
            !outside.exists(),
            "salt creation must never follow a symlink and create {outside:?}: {err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn concurrent_first_seal_keeps_every_ciphertext_decryptable() {
        use std::sync::{Arc, Barrier};
        for round in 0..25 {
            let dir = tmp_dir(&format!("race-{round}"));
            let workers = 8;
            let barrier = Arc::new(Barrier::new(workers));
            let handles: Vec<_> = (0..workers)
                .map(|idx| {
                    let dir = dir.clone();
                    let barrier = Arc::clone(&barrier);
                    std::thread::spawn(move || {
                        barrier.wait();
                        let plain = format!("sk-secret-{idx}");
                        let sealed = seal(&plain, &dir).unwrap();
                        (plain, sealed)
                    })
                })
                .collect();
            let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
            for (plain, sealed) in results {
                assert_eq!(
                    unseal(&sealed, &dir).as_deref(),
                    Some(plain.as_str()),
                    "concurrent initialization in round {round} overwrote an earlier salt"
                );
            }
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[cfg(unix)]
    #[test]
    fn unseal_and_seal_reject_symlink_pointing_to_valid_salt() {
        let dir = tmp_dir("symlink-valid");
        let real_dir = dir.join("real");
        let link_dir = dir.join("link");
        std::fs::create_dir_all(&link_dir).unwrap();

        let sealed = seal("sk-secret", &real_dir).unwrap();
        std::os::unix::fs::symlink(real_dir.join(SALT_FILE_NAME), link_dir.join(SALT_FILE_NAME))
            .unwrap();

        assert!(
            unseal(&sealed, &link_dir).is_none(),
            "unseal must refuse a symlinked salt file even when target is valid"
        );
        assert!(
            seal("sk-other", &link_dir).is_err(),
            "seal must refuse overwriting or reading a symlinked salt file"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_seal_leaves_only_owner_read_only_salt_file() {
        use std::os::unix::fs::PermissionsExt;
        use std::sync::{Arc, Barrier};

        let dir = tmp_dir("cleanup");
        let workers = 8;
        let barrier = Arc::new(Barrier::new(workers));
        let handles: Vec<_> = (0..workers)
            .map(|idx| {
                let dir = dir.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    seal(&format!("sk-{idx}"), &dir).unwrap()
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }

        let entries: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(entries, vec![SALT_FILE_NAME.to_string()]);
        let mode = std::fs::metadata(dir.join(SALT_FILE_NAME))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o400);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_corrupt_salt_without_overwriting() {
        let dir = tmp_dir("corrupt");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(SALT_FILE_NAME), b"short").unwrap();

        let err = seal("sk-secret", &dir).expect_err("short salt must fail");
        assert!(err.to_string().contains("corrupt salt file"), "{err}");
        assert_eq!(std::fs::read(dir.join(SALT_FILE_NAME)).unwrap(), b"short");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn fifo_at_salt_path_is_rejected_without_blocking() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let dir = tmp_dir("fifo");
        let fifo_path = dir.join(SALT_FILE_NAME);
        let c_path = CString::new(fifo_path.as_os_str().as_bytes()).unwrap();
        let rc = unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };
        assert_eq!(rc, 0, "mkfifo failed: {}", io::Error::last_os_error());

        let sample_dir = tmp_dir("fifo-sample");
        let sealed = seal("sk-secret", &sample_dir).unwrap();
        assert!(unseal(&sealed, &dir).is_none());
        let err = seal("sk-secret", &dir).expect_err("FIFO salt path must fail cleanly");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        let _ = std::fs::remove_dir_all(&sample_dir);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_oversized_salt_without_overwriting() {
        let dir = tmp_dir("oversized");
        let big = vec![b'x'; 4096];
        std::fs::write(dir.join(SALT_FILE_NAME), &big).unwrap();

        let err = seal("sk-secret", &dir).expect_err("oversized salt must fail");
        assert!(err.to_string().contains("corrupt salt file"), "{err}");
        assert_eq!(std::fs::read(dir.join(SALT_FILE_NAME)).unwrap().len(), 4096);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn direct_create_fallback_sets_mode_400_and_respects_existing_salt() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tmp_dir("direct-fallback");
        let salt_path = dir.join(SALT_FILE_NAME);
        let first_salt = [0xABu8; SALT_LEN];
        let created = write_salt_direct_fallback(&salt_path, &first_salt).unwrap();
        assert_eq!(created.as_deref(), Some(&first_salt[..]));
        let mode = std::fs::metadata(&salt_path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o400);

        let second_salt = [0xCDu8; SALT_LEN];
        let raced = write_salt_direct_fallback(&salt_path, &second_salt).unwrap();
        assert!(raced.is_none(), "fallback must not overwrite existing salt");
        assert_eq!(read_salt_nofollow(&salt_path).unwrap(), first_salt);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
