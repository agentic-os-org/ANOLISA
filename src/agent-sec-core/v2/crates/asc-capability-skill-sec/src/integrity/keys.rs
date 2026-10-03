//! Atomic system-key initialization in an explicitly provisioned private state directory.

use super::{SigningIdentity, digest};
use crate::filesystem::{READ_FLAGS, open_directory};
use crate::{SkillSecError, check_deadline, io_error};
use ring::{
    rand::{SecureRandom as _, SystemRandom},
    signature::Ed25519KeyPair,
};
use rustix::fs::{AtFlags, Mode, OFlags, RenameFlags, openat, renameat_with, statat, unlinkat};
use std::fs::File;
use std::io::{Read as _, Write as _};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const KEY_FILE: &str = "signing-key.pk8";
const MAX_KEY_BYTES: u64 = 4096;

/// Age after which an unreferenced `.key-` temporary is treated as a crashed
/// leftover rather than an in-flight write by a concurrent initializer.
///
/// Key writes finish in milliseconds, so an hour-old temporary can only be
/// an orphan: either `initialize` or `replace` was interrupted between the
/// temp write and the rename, and its unlinkat cleanup never ran. The margin
/// exists so a racing initializer's live temporary is never swept out from
/// under it (its rename would then fail spuriously with ENOENT).
const STALE_KEY_TEMP_AGE: Duration = Duration::from_secs(3600);

/// Mirrors the two nonce shapes produced by this module: `replace` writes
/// `.key-<64hex>` and `initialize` writes `.key-<64hex>.tmp`. Anything else
/// in the state directory (`signing-key.pk8`, `key-rotation.json`,
/// `managed-skills.json`, `.rollback-*.json`) must never match; if a nonce
/// format above changes, this predicate must change with it.
fn is_key_temp(name: &str) -> bool {
    name.strip_prefix(".key-").is_some_and(|suffix| {
        let hex = suffix.strip_suffix(".tmp").unwrap_or(suffix);
        hex.len() == 64 && hex.bytes().all(|c| c.is_ascii_hexdigit())
    })
}

/// Pinned service-owned key directory; only initialization is exposed to business setup.
pub struct KeyStore {
    directory: File,
    path: PathBuf,
}

impl KeyStore {
    /// Opens an existing private directory without following symlink components.
    ///
    /// # Errors
    /// Rejects a directory not owned by the effective service UID or accessible to other users.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, SkillSecError> {
        let path = path.as_ref();
        let directory = open_directory(path)?;
        let meta = directory.metadata().map_err(|e| io_error(path, e))?;
        if meta.uid() != rustix::process::geteuid().as_raw() || meta.mode() & 0o077 != 0 {
            return Err(SkillSecError::Invalid(
                "signing directory must be service-owned and private".into(),
            ));
        }
        Ok(Self {
            directory,
            path: path.to_path_buf(),
        })
    }

    /// Loads only the current key, without V1 imports or historical keyring fallback.
    ///
    /// # Errors
    /// Rejects missing, malformed, hard-linked, symlinked, oversized or permissively owned keys.
    pub fn load(&self) -> Result<SigningIdentity, SkillSecError> {
        let path = self.path.join(KEY_FILE);
        let file = File::from(
            openat(&self.directory, KEY_FILE, READ_FLAGS, Mode::empty())
                .map_err(|e| io_error(&path, e))?,
        );
        let meta = file.metadata().map_err(|e| io_error(&path, e))?;
        if !meta.is_file()
            || meta.uid() != rustix::process::geteuid().as_raw()
            || meta.mode() & 0o077 != 0
            || meta.nlink() != 1
            || meta.len() > MAX_KEY_BYTES
        {
            return Err(SkillSecError::Invalid(
                "signing key must be a private service-owned regular file".into(),
            ));
        }
        let mut bytes = Vec::new();
        file.take(MAX_KEY_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| io_error(&path, e))?;
        let result = Ed25519KeyPair::from_pkcs8(&bytes)
            .map(SigningIdentity)
            .map_err(|_| SkillSecError::Key);
        // PKCS8 is transient; the key pair owns the only retained signing representation.
        bytes.fill(0);
        result
    }

    /// Replaces current trust after the service has withdrawn all managed activation.
    /// No previous public key or private key is retained.
    pub(crate) fn replace(&self) -> Result<SigningIdentity, SkillSecError> {
        self.load()?;
        let key =
            Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).map_err(|_| SkillSecError::Key)?;
        let temp = crate::ledger::storage::nonce(".key-")?;
        let result = (|| {
            let directory = crate::ledger::storage::Directory::open(&self.path)?;
            directory.write_new(&temp, key.as_ref(), 0o600)?;
            renameat_with(
                &self.directory,
                temp.as_str(),
                &self.directory,
                KEY_FILE,
                RenameFlags::empty(),
            )
            .map_err(|e| io_error(&self.path, e))?;
            self.directory
                .sync_all()
                .map_err(|e| io_error(&self.path, e))?;
            self.load()
        })();
        let _ = unlinkat(&self.directory, temp.as_str(), AtFlags::empty());
        result
    }

    /// Creates the first key atomically, or loads the existing identity unchanged.
    ///
    /// # Errors
    /// Propagates unsafe existing keys, entropy failures and persistence errors. Never repairs
    /// or replaces an invalid key automatically, since doing so would reset the trust domain.
    pub fn initialize(&self) -> Result<SigningIdentity, SkillSecError> {
        match self.load() {
            Ok(key) => return Ok(key),
            Err(SkillSecError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let rng = SystemRandom::new();
        let key = Ed25519KeyPair::generate_pkcs8(&rng).map_err(|_| SkillSecError::Key)?;
        let mut nonce = [0_u8; 32];
        rng.fill(&mut nonce).map_err(|_| SkillSecError::Key)?;
        let temp = format!(".key-{}.tmp", digest(&nonce).trim_start_matches("sha256:"));
        let mut file = File::from(
            openat(
                &self.directory,
                temp.as_str(),
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                Mode::RUSR | Mode::WUSR,
            )
            .map_err(|e| io_error(&self.path, e))?,
        );
        let outcome = (|| {
            file.write_all(key.as_ref())
                .map_err(|e| io_error(&self.path, e))?;
            file.sync_all().map_err(|e| io_error(&self.path, e))?;
            match renameat_with(
                &self.directory,
                temp.as_str(),
                &self.directory,
                KEY_FILE,
                RenameFlags::NOREPLACE,
            ) {
                Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                Err(error) => return Err(io_error(&self.path, error)),
            }
            self.directory
                .sync_all()
                .map_err(|e| io_error(&self.path, e))?;
            self.load()
        })();
        // No recursive cleanup and no user-supplied path: this removes only our exclusive temporary file.
        let _ = unlinkat(&self.directory, temp.as_str(), AtFlags::empty());
        outcome
    }

    /// Removes `.key-` temporaries left by an interrupted `initialize` or `replace`.
    ///
    /// Both writers only unlink their temporary when the process survives the
    /// function; a crash between the temp write and the rename strands a full
    /// Ed25519 PKCS#8 private key in the state directory forever, because
    /// nothing else in the recovery story reclaims this class of temporary
    /// (the ledger sweep only covers `.record-`/`.snapshot-`/`.pending-`).
    /// Only well-shaped names whose mtime is PROVABLY at least
    /// [`STALE_KEY_TEMP_AGE`] old are removed, so a concurrent initializer's
    /// in-flight temporary is never swept — including one carrying a
    /// future-dated mtime from clock skew, which cannot be proven stale and
    /// fails safe to "keep". An entry that vanishes between the enumeration
    /// and its stat is already reclaimed and is skipped, never propagated.
    ///
    /// # Errors
    /// Propagates directory and deadline failures; a missing entry is skipped.
    pub fn reclaim_temporaries(&self, deadline: Instant) -> Result<usize, SkillSecError> {
        let directory = crate::ledger::storage::Directory::open(&self.path)?;
        let mut removed = 0;
        for name in directory.names(deadline)? {
            check_deadline(deadline)?;
            if self.reclaim_one(&directory, &name, deadline)? {
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// The per-entry half of [`Self::reclaim_temporaries`], split out so the
    /// vanished-entry and clock-skew edges can be pinned directly by tests:
    /// reclaim `name` from `directory` when it is a well-shaped `.key-`
    /// temporary whose age is provably at least [`STALE_KEY_TEMP_AGE`].
    ///
    /// Returns whether an entry was removed. An entry that vanished since the
    /// enumeration — a concurrent cleanup, or the writer's own trailing
    /// unlink after a successful rename — is already reclaimed and reports
    /// `false` instead of failing service construction or key rotation with
    /// ENOENT; every other stat failure stays visible. An mtime in the future
    /// (clock skew, or a rollback across the sweep) makes the age unknowable
    /// and likewise fails safe to "keep", exactly like a young in-flight
    /// temporary.
    fn reclaim_one(
        &self,
        directory: &crate::ledger::storage::Directory,
        name: &str,
        deadline: Instant,
    ) -> Result<bool, SkillSecError> {
        if !is_key_temp(name) {
            return Ok(false);
        }
        let path = self.path.join(name);
        let stat = match statat(&directory.file, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => stat,
            Err(rustix::io::Errno::NOENT) => return Ok(false),
            Err(error) => return Err(io_error(&path, error)),
        };
        // Layout-independent mtime recovery: seconds since the epoch plus
        // nanoseconds, both clamped against a clock set behind the file
        // (Duration::new also rejects nanosecond overflow, hence the cap).
        let modified = std::time::SystemTime::UNIX_EPOCH
            + Duration::new(
                u64::try_from(stat.st_mtime).unwrap_or(0),
                u32::try_from(stat.st_mtime_nsec)
                    .unwrap_or(0)
                    .min(999_999_999),
            );
        // Fail-safe age guard: only an mtime provably at least
        // STALE_KEY_TEMP_AGE old selects the entry. A future-dated mtime
        // makes `elapsed()` Err and the age unknowable — keep the entry
        // rather than treat "not provably young" as stale.
        let stale = match modified.elapsed() {
            Ok(age) => age >= STALE_KEY_TEMP_AGE,
            Err(_) => false,
        };
        if !stale {
            return Ok(false);
        }
        directory.remove_child(name, deadline)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    /// Sets a file's mtime into the past so the sweep's age guard sees it as
    /// a crashed leftover.
    fn backdate(path: &Path, age: Duration) {
        let file = File::options().write(true).open(path).unwrap();
        file.set_times(
            std::fs::FileTimes::new().set_modified(
                std::time::SystemTime::now()
                    .checked_sub(age)
                    .expect("test age within clock range"),
            ),
        )
        .unwrap();
    }

    /// Sets a file's mtime into the future — the clock-skew shape, where the
    /// sweep cannot establish the entry's age at all.
    fn futuredate(path: &Path, skew: Duration) {
        let file = File::options().write(true).open(path).unwrap();
        file.set_times(std::fs::FileTimes::new().set_modified(std::time::SystemTime::now() + skew))
            .unwrap();
    }

    /// Plants a private-mode file with marker content, as a crashed writer would leave.
    fn plant(path: &Path, bytes: &[u8]) {
        std::fs::write(path, bytes).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    fn state_dir() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = dir.path().canonicalize().unwrap();
        (dir, path)
    }

    fn hex64(c: char) -> String {
        c.to_string().repeat(64)
    }

    #[test]
    fn key_temp_shape_matches_both_nonce_formats_only() {
        assert!(is_key_temp(&format!(".key-{}", hex64('a'))));
        assert!(is_key_temp(&format!(".key-{}.tmp", hex64('0'))));
        for name in [
            "signing-key.pk8",
            "key-rotation.json",
            "managed-skills.json",
            ".rollback.json",
            &format!(".key-{}.tmp2", hex64('a')),
            &format!(".key-{}", "a".repeat(63)),
            // Hex digits are case-insensitive here, mirroring the ledger's
            // `.record-` sweep; 'g' is genuinely outside the alphabet.
            &format!(".key-{}.tmp", "g".repeat(64)),
            ".key-short.tmp",
            &format!(".record-{}", hex64('a')),
            &format!(".snapshot-{}", hex64('a')),
            &format!(".pending-{}", hex64('a')),
            &format!(".key-{}.json", hex64('a')),
        ] {
            assert!(!is_key_temp(name), "{name} must never be reclaimed");
        }
    }

    #[test]
    fn reclaim_removes_only_stale_well_shaped_temps() {
        let (_dir, path) = state_dir();
        let stale_replace = format!(".key-{}", hex64('a'));
        let stale_initialize = format!(".key-{}.tmp", hex64('b'));
        let fresh = format!(".key-{}.tmp", hex64('c'));
        let others = [
            ("signing-key.pk8", b"active-key".as_slice()),
            ("key-rotation.json", b"{}"),
            ("managed-skills.json", b"[]"),
            (&format!(".rollback-{}.json", hex64('d')), b"{}"),
            (&format!(".key-{}.tmp2", hex64('e')), b"near-miss"),
            (".key-short.tmp", b"near-miss"),
        ];
        for (name, bytes) in &others {
            plant(&path.join(name), bytes);
        }
        plant(&path.join(&stale_replace), b"orphaned-private-key");
        plant(&path.join(&stale_initialize), b"orphaned-private-key");
        plant(&path.join(&fresh), b"in-flight-private-key");
        backdate(&path.join(&stale_replace), Duration::from_secs(7200));
        backdate(&path.join(&stale_initialize), Duration::from_secs(7200));

        let store = KeyStore::open(&path).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        assert_eq!(store.reclaim_temporaries(deadline).unwrap(), 2);

        assert!(!path.join(&stale_replace).exists());
        assert!(!path.join(&stale_initialize).exists());
        // A concurrent initializer's in-flight temporary survives the sweep.
        assert_eq!(
            std::fs::read(path.join(&fresh)).unwrap(),
            b"in-flight-private-key"
        );
        for (name, bytes) in &others {
            assert_eq!(&std::fs::read(path.join(name)).unwrap(), bytes, "{name}");
        }
    }

    #[test]
    fn reclaim_preserves_future_dated_temps_and_still_removes_stale_ones() {
        // Clock skew (or a rollback across the sweep) can leave an mtime in
        // the future; the age is then unknowable. The guard must fail safe —
        // keep the future-dated temp exactly like a concurrent initializer's
        // in-flight one — while still reclaiming provably stale siblings in
        // the same sweep: "not provably young" must never mean "stale".
        let (_dir, path) = state_dir();
        let skewed = format!(".key-{}", hex64('1'));
        let stale = format!(".key-{}.tmp", hex64('2'));
        plant(&path.join(&skewed), b"skewed-in-flight-key");
        plant(&path.join(&stale), b"orphaned-private-key");
        futuredate(&path.join(&skewed), Duration::from_secs(3600));
        backdate(&path.join(&stale), Duration::from_secs(7200));

        let store = KeyStore::open(&path).unwrap();
        assert_eq!(
            store
                .reclaim_temporaries(Instant::now() + Duration::from_secs(10))
                .unwrap(),
            1
        );
        assert_eq!(
            std::fs::read(path.join(&skewed)).unwrap(),
            b"skewed-in-flight-key",
            "a future-dated mtime cannot be proven stale, so it is kept"
        );
        assert!(!path.join(&stale).exists());
    }

    #[test]
    fn reclaim_skips_a_temp_that_vanished_mid_sweep() {
        // A temp that disappears between the readdir and its stat — a
        // concurrent cleanup, or the writer's own trailing unlink after a
        // successful rename — must be treated as already reclaimed, never
        // propagated as ENOENT to fail service construction or rotation.
        let (_dir, path) = state_dir();
        let vanished = format!(".key-{}", hex64('3'));
        plant(&path.join(&vanished), b"concurrently-removed");
        let directory = crate::ledger::storage::Directory::open(&path).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        // The entry was enumerated while it still existed...
        assert!(directory.names(deadline).unwrap().contains(&vanished));
        // ...and is gone before the sweep reaches its statat.
        std::fs::remove_file(path.join(&vanished)).unwrap();

        let store = KeyStore::open(&path).unwrap();
        // The exact mid-sweep position: the per-entry step reports "not
        // removed" for the vanished entry instead of an error.
        assert!(!store.reclaim_one(&directory, &vanished, deadline).unwrap());
        // A full sweep over the same directory also stays Ok.
        assert_eq!(store.reclaim_temporaries(deadline).unwrap(), 0);
    }

    #[test]
    fn reclaim_never_touches_the_active_key() {
        let (_dir, path) = state_dir();
        let store = KeyStore::open(&path).unwrap();
        let fingerprint = store.initialize().unwrap().fingerprint();
        // An ancient active key proves the sweep is name-shape based, not age
        // based: old age alone must never select `signing-key.pk8`.
        backdate(&path.join(KEY_FILE), Duration::from_secs(3600 * 24 * 365));
        assert_eq!(
            store
                .reclaim_temporaries(Instant::now() + Duration::from_secs(10))
                .unwrap(),
            0
        );
        assert_eq!(store.load().unwrap().fingerprint(), fingerprint);
    }

    #[test]
    fn reclaim_reports_zero_on_clean_state_directories() {
        let (_dir, path) = state_dir();
        let store = KeyStore::open(&path).unwrap();
        assert_eq!(
            store
                .reclaim_temporaries(Instant::now() + Duration::from_secs(10))
                .unwrap(),
            0
        );
        store.initialize().unwrap();
        assert_eq!(
            store
                .reclaim_temporaries(Instant::now() + Duration::from_secs(10))
                .unwrap(),
            0
        );
        assert_eq!(std::fs::read_dir(&path).unwrap().count(), 1);
    }

    #[test]
    fn crashed_initialize_temporaries_do_not_block_reinitialization() {
        let (_dir, path) = state_dir();
        let store = KeyStore::open(&path).unwrap();
        let stranded = format!(".key-{}.tmp", hex64('f'));
        plant(&path.join(&stranded), b"stranded-by-crash");
        backdate(&path.join(&stranded), Duration::from_secs(7200));
        // The fresh sweep at service construction would remove it; a plain
        // initialize must also succeed next to it, and a later sweep reclaims.
        let key = store.initialize().unwrap();
        assert!(path.join(&stranded).exists());
        assert_eq!(
            store
                .reclaim_temporaries(Instant::now() + Duration::from_secs(10))
                .unwrap(),
            1
        );
        assert_eq!(store.load().unwrap().fingerprint(), key.fingerprint());
        assert_eq!(std::fs::read_dir(&path).unwrap().count(), 1);
    }
}
