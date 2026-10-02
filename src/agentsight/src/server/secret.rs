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

use std::io;
use std::path::{Path, PathBuf};

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

/// Read the per-install salt, generating it with mode 0o400 when absent and
/// `create` is set. Refusing to create during decryption ensures a config
/// copied without its salt file never silently "decrypts" to garbage keys.
fn load_or_create_salt(path: &PathBuf, create: bool) -> io::Result<Vec<u8>> {
    match std::fs::read(path) {
        Ok(salt) if salt.len() == SALT_LEN => Ok(salt),
        Ok(_) => Err(io::Error::other(format!(
            "corrupt salt file {} (expected {SALT_LEN} bytes)",
            path.display()
        ))),
        Err(e) if e.kind() == io::ErrorKind::NotFound && create => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut salt = vec![0u8; SALT_LEN];
            rand_bytes(&mut salt).map_err(crypto_err)?;
            std::fs::write(path, &salt)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o400))?;
            }
            Ok(salt)
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
}
