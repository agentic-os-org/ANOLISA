//! Owned-file integrity checks.
//!
//! Single concern: given an [`OwnedFile`] from `installed.toml`, report
//! whether the on-disk file still exists and matches the recorded content,
//! permission, and Linux file-capability contract. Used by `anolisa status`
//! to surface tampering / drift without relying on a component-supplied
//! health probe.
//!
//! The check is intentionally minimal — it does not consult the catalog,
//! does not run manifest-declared health hooks, and does not touch any
//! file outside `OwnedFile.path`. Manifest health hooks are deliberately
//! out of scope and report as `skipped` at the call site.
//!
//! Path safety is layered in front of every IO:
//!
//!   * `OwnedFile.path` is re-validated against [`FsLayout`] via
//!     [`crate::path_safety::validate_owned_path`] **before** any `stat` or open.
//!     A forged `installed.toml` claiming `owner = anolisa` for
//!     `/etc/shadow` (or `<bin_dir>/escape -> /etc/shadow`) is therefore
//!     refused with `OutOfBounds` rather than read.
//!   * Managed symlinks (`kind == Symlink`) are verified via
//!     [`std::fs::read_link`] against the recorded referent path,
//!     bypassing content hashing entirely.
//!   * Legacy symlinks (`kind == File` but symlink on disk) are refused
//!     with `IntegrityStatus::Symlink`. Pre-v4 entries are migrated to
//!     `kind = Symlink` by `migrate_v3_symlinks` before the probe runs.
//!   * Regular files are opened with `O_NOFOLLOW` so a symlink planted
//!     at the destination cannot redirect the read to a third-party file.
//!   * Special files (directories, fifos, sockets, devices) are refused
//!     via the regular-file guard so `status` cannot hang on a fifo or
//!     mis-hash a directory.
//!
//! All guards report through dedicated [`IntegrityStatus`] variants
//! so the wire surface tells operators *why* the probe refused, not just
//! that it failed.

use std::fs;

use sha2::{Digest, Sha256};

use anolisa_platform::fs_layout::FsLayout;

use crate::capability::probe_file_capabilities;
use crate::path_safety::{PathBoundaryError, validate_owned_path};
use crate::state::{FileOwner, OwnedFile, OwnedFileKind};

/// Maximum bytes the integrity probe will read for one file, bounding the
/// wall-clock a single `status` / `doctor` run can spend hashing one path.
/// Hashing itself streams through a fixed 8 KiB buffer, so the ceiling
/// costs no memory — it only caps time and page-cache churn.
///
/// The bound is deliberately above the largest artifacts ANOLISA actually
/// ships: raw installs vendor native ML runtimes whose shared objects run
/// to hundreds of megabytes (`libtorch_cpu.so` is ~440 MB, CUDA builds
/// larger still). A ceiling below those would leave first-party components
/// permanently unverified, which is worse than the multi-second hash it
/// would save.
///
/// Exceeding it is **not** an integrity failure: the bytes were never
/// examined, so nothing was disproved. It reports
/// [`IntegrityStatus::ProbeLimitExceeded`], which degrades rather than
/// fails. Note this is a per-file bound and the probe applies it to every
/// owned file in turn — it caps the cost of one path, not of one run.
const MAX_PROBE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Result of a single integrity probe against one [`OwnedFile`].
///
/// Variants are ordered by severity so callers can fold via `max`:
/// `Ok < Skipped < Unverified < ProbeLimitExceeded < OutOfBounds < Symlink
/// < NotRegularFile < MissingFile < ReadError < ModeMismatch
/// < CapabilityMismatch < ShaMismatch`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum IntegrityStatus {
    /// File satisfies its contract; config content is intentionally mutable.
    Ok,
    /// Owner is not ANOLISA-managed — we deliberately don't probe.
    Skipped,
    /// File exists but no sha256 was ever recorded — drift cannot be
    /// proved either way, so we degrade rather than claim health.
    Unverified,
    /// File exists and passed every cheap check (regular file, mode,
    /// capabilities), but its size is above the probe's per-file read
    /// ceiling so the content hash was never computed. The ceiling is an
    /// internal constant; `limit` below reports the value that applied.
    ///
    /// This is a *budget* outcome, not a *read* outcome: the probe chose
    /// not to spend the time, as opposed to trying and failing. Nothing
    /// about the bytes was disproved, so — like [`Self::Unverified`] —
    /// it degrades instead of failing. Keeping it distinct from
    /// `Unverified` tells operators which of the two holds: no digest was
    /// ever recorded, or one was recorded but not checked this run.
    ProbeLimitExceeded {
        /// Size observed on disk, in bytes.
        size: u64,
        /// Per-file read ceiling that was exceeded, in bytes.
        limit: u64,
    },
    /// Path escapes the ANOLISA-owned roots in the active [`FsLayout`].
    /// Probe is refused without any filesystem touch; this strongly
    /// suggests a forged or corrupted `installed.toml`.
    OutOfBounds,
    /// `OwnedFile.path` is a symlink. The probe refuses to follow it so
    /// a planted symlink cannot redirect the read to a third-party file.
    Symlink,
    /// Path exists but is not a regular file (directory, fifo, socket,
    /// device, etc.). Refused so `status` cannot hang on a fifo or
    /// mis-hash a directory.
    NotRegularFile,
    /// File is gone from disk.
    MissingFile,
    /// File exists but cannot be read (permissions, broken symlink, etc).
    ReadError(String),
    /// Managed symlink referent mismatch: the link either does not exist
    /// as a symlink, or points at a different target than recorded.
    ReferentMismatch {
        /// Referent path recorded in `installed.toml`.
        expected: String,
        /// Actual `readlink` result, or `"(not a symlink)"` when the path
        /// is not a symlink at all.
        actual: String,
    },
    /// File content exists, but its Unix permission bits differ from the
    /// install contract.
    ModeMismatch {
        /// Four-digit octal mode recorded in installed state.
        expected: String,
        /// Four-digit octal mode observed on disk.
        actual: String,
    },
    /// Linux file capabilities differ from the set successfully applied
    /// during install.
    CapabilityMismatch {
        /// Normalized capability assignment expected by the record.
        expected: String,
        /// Capability assignment decoded from `security.capability`.
        actual: String,
    },
    /// File exists, sha256 was recorded, and bytes diverged.
    ShaMismatch {
        /// Lowercase sha256 recorded in `installed.toml`.
        expected: String,
        /// Lowercase sha256 computed from the current on-disk bytes.
        actual: String,
    },
}

impl IntegrityStatus {
    /// Wire-friendly snake_case label for JSON/log output.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Skipped => "skipped",
            Self::Unverified => "unverified",
            Self::ProbeLimitExceeded { .. } => "probe_limit_exceeded",
            Self::OutOfBounds => "out_of_bounds",
            Self::Symlink => "symlink_refused",
            Self::NotRegularFile => "not_regular_file",
            Self::MissingFile => "missing_file",
            Self::ReadError(_) => "read_error",
            Self::ReferentMismatch { .. } => "referent_mismatch",
            Self::ModeMismatch { .. } => "mode_mismatch",
            Self::CapabilityMismatch { .. } => "capability_mismatch",
            Self::ShaMismatch { .. } => "sha256_mismatch",
        }
    }

    /// `true` when the probe found a real integrity problem (vs. ok /
    /// skipped / unverified / probe-limit-exceeded). Drives status
    /// escalation in `status`.
    /// Out-of-bounds / symlink / not-regular-file all count as failures
    /// because they signal either tampering or a corrupted state file —
    /// neither is "merely degraded".
    ///
    /// [`Self::ProbeLimitExceeded`] is deliberately absent: an unread file
    /// is not a damaged one, and reporting the component `failed` for
    /// owning a large-but-intact artifact was the bug behind #2251.
    pub fn is_failure(&self) -> bool {
        matches!(
            self,
            Self::OutOfBounds
                | Self::Symlink
                | Self::NotRegularFile
                | Self::MissingFile
                | Self::ReadError(_)
                | Self::ReferentMismatch { .. }
                | Self::ModeMismatch { .. }
                | Self::CapabilityMismatch { .. }
                | Self::ShaMismatch { .. }
        )
    }
}

/// Run the integrity probe on one [`OwnedFile`]. Side-effect free.
///
/// `layout` is the live [`FsLayout`] for the install mode the caller is
/// reporting on. It is consulted before any filesystem IO so a forged
/// `installed.toml` entry pointing outside ANOLISA-owned roots is
/// refused with [`IntegrityStatus::OutOfBounds`] — `status` does not
/// stat, follow, or read that path.
///
/// Returns [`IntegrityStatus::Skipped`] for non-ANOLISA-owned entries so
/// the caller never accidentally hashes a third-party config file.
/// Immutable ANOLISA-owned entries with no recorded sha256 return
/// [`IntegrityStatus::Unverified`] rather than `Ok` — the absence of a
/// recorded hash is a degradation signal, not a clean state.
/// Config entries skip hashing after path, type, mode, and capability checks.
pub fn check_owned_file(layout: &FsLayout, file: &OwnedFile) -> IntegrityStatus {
    if file.owner != FileOwner::Anolisa {
        return IntegrityStatus::Skipped;
    }

    // Path-boundary guard FIRST so a forged path never reaches stat.
    if let Err(err) = validate_owned_path(layout, &file.path) {
        // Traversal and External both surface as out_of_bounds — the
        // wire surface does not need to leak which sub-rule fired, and
        // either way the probe refuses to touch the path.
        let _: PathBoundaryError = err;
        return IntegrityStatus::OutOfBounds;
    }

    // Managed symlinks are verified against their recorded referent
    // instead of hashing content through the link.
    if file.kind == OwnedFileKind::Symlink {
        return check_owned_symlink(layout, file);
    }

    // symlink_metadata does NOT follow — required so a planted symlink
    // cannot redirect the read to a third-party file. `exists()` would
    // follow and lie about a broken symlink.
    let meta = match fs::symlink_metadata(&file.path) {
        Ok(m) => m,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return IntegrityStatus::MissingFile;
        }
        Err(err) => return IntegrityStatus::ReadError(err.to_string()),
    };
    if meta.file_type().is_symlink() {
        return IntegrityStatus::Symlink;
    }
    if !meta.is_file() {
        return IntegrityStatus::NotRegularFile;
    }
    if let Some(expected) = file.mode.as_deref() {
        let expected_mode = match parse_recorded_mode(expected) {
            Some(mode) => mode,
            None => {
                return IntegrityStatus::ReadError(format!(
                    "recorded mode '{expected}' is not valid octal notation"
                ));
            }
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let actual_mode = meta.permissions().mode() & 0o7777;
            if actual_mode != expected_mode {
                return IntegrityStatus::ModeMismatch {
                    expected: format!("{expected_mode:04o}"),
                    actual: format!("{actual_mode:04o}"),
                };
            }
        }
    }
    if !file.capabilities.is_empty() {
        let expected = normalized_capability_assignment(&file.capabilities);
        match probe_file_capabilities(&file.path) {
            Ok(actual) if !actual.matches_requested(&file.capabilities) => {
                return IntegrityStatus::CapabilityMismatch {
                    expected,
                    actual: actual.display(),
                };
            }
            Ok(_) => {}
            Err(err) => return IntegrityStatus::ReadError(err.to_string()),
        }
    }
    if file.kind == OwnedFileKind::Config {
        return IntegrityStatus::Ok;
    }

    // Digest gate BEFORE the size gate, so each label keeps the meaning its
    // documentation promises: `Unverified` means no digest was ever
    // recorded, `ProbeLimitExceeded` means one was recorded but this run
    // did not check it. Gating on size first would collapse both into the
    // latter and tell operators a baseline exists where none does.
    let Some(expected) = file.sha256.clone() else {
        return IntegrityStatus::Unverified;
    };

    if meta.len() > MAX_PROBE_BYTES {
        return IntegrityStatus::ProbeLimitExceeded {
            size: meta.len(),
            limit: MAX_PROBE_BYTES,
        };
    }

    // The size just observed is the read budget. Growth past it means the
    // file changed under the probe, which is a finding, not a budget stop.
    match hash_file_sha256(&file.path, meta.len()) {
        Err(err) => IntegrityStatus::ReadError(err.to_string()),
        Ok(actual) if actual != expected => IntegrityStatus::ShaMismatch { expected, actual },
        Ok(_) => IntegrityStatus::Ok,
    }
}

fn parse_recorded_mode(raw: &str) -> Option<u32> {
    let trimmed = raw.trim();
    let octal = trimmed.strip_prefix("0o").unwrap_or(trimmed);
    let mode = u32::from_str_radix(octal, 8).ok()?;
    (mode <= 0o7777).then_some(mode)
}

fn normalized_capability_assignment(caps: &[String]) -> String {
    let mut caps = caps
        .iter()
        .map(|cap| cap.trim().to_ascii_lowercase())
        .collect::<Vec<_>>();
    caps.sort();
    caps.dedup();
    format!("{}=ep", caps.join(","))
}

/// Verify a managed symlink entry: the path must be a symlink whose
/// `readlink` result matches the recorded referent, and the referent
/// must remain within ANOLISA-owned roots.
fn check_owned_symlink(layout: &FsLayout, file: &OwnedFile) -> IntegrityStatus {
    let expected_referent = match &file.referent {
        Some(r) => r,
        None => return IntegrityStatus::Unverified,
    };

    let meta = match fs::symlink_metadata(&file.path) {
        Ok(m) => m,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return IntegrityStatus::MissingFile;
        }
        Err(err) => return IntegrityStatus::ReadError(err.to_string()),
    };

    if !meta.file_type().is_symlink() {
        return IntegrityStatus::ReferentMismatch {
            expected: expected_referent.display().to_string(),
            actual: "(not a symlink)".to_string(),
        };
    }

    let actual_referent = match fs::read_link(&file.path) {
        Ok(r) => r,
        Err(err) => return IntegrityStatus::ReadError(err.to_string()),
    };

    if actual_referent != *expected_referent {
        return IntegrityStatus::ReferentMismatch {
            expected: expected_referent.display().to_string(),
            actual: actual_referent.display().to_string(),
        };
    }

    if validate_owned_path(layout, &actual_referent).is_err() {
        return IntegrityStatus::OutOfBounds;
    }

    // Verify the referent target actually exists and is a regular file.
    // A dangling symlink whose readlink matches is still broken.
    match fs::metadata(&file.path) {
        Ok(m) if m.is_file() => IntegrityStatus::Ok,
        Ok(_) => IntegrityStatus::NotRegularFile,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            IntegrityStatus::ReadError(format!(
                "dangling symlink: referent {} does not exist",
                expected_referent.display()
            ))
        }
        Err(err) => IntegrityStatus::ReadError(err.to_string()),
    }
}

#[cfg(unix)]
fn open_nofollow(path: &std::path::Path) -> std::io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    // O_NOFOLLOW: the open() syscall refuses to follow a terminal-segment
    // symlink. Combined with the symlink_metadata pre-check above this
    // closes the TOCTOU window where a symlink could be swapped in
    // between stat and open.
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(not(unix))]
fn open_nofollow(path: &std::path::Path) -> std::io::Result<fs::File> {
    fs::File::open(path)
}

/// Stream `path` through sha256 with a fixed 8 KiB buffer, so peak memory
/// is constant regardless of file size.
///
/// `expected_len` is the length `stat` reported moments earlier, and it is
/// the read budget: reading more than that means the file grew *while the
/// probe was streaming it*. That is a concurrent modification, not a budget
/// stop, and it fails hard. Treating it as a budget stop would hand a
/// writer an evasion — append past the ceiling mid-read and the digest
/// comparison never completes, so tampering reports as merely unverified.
///
/// Anchoring on the observed length rather than the ceiling also catches
/// growth that stays under the ceiling, which a ceiling test would miss.
fn hash_file_sha256(path: &std::path::Path, expected_len: u64) -> std::io::Result<String> {
    use std::io::Read;
    let mut f = open_nofollow(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8 * 1024];
    let mut total: u64 = 0;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > expected_len {
            return Err(std::io::Error::other(format!(
                "file grew past its observed size of {expected_len} bytes during the \
                 integrity read"
            )));
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex_lower(&hasher.finalize()))
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use tempfile::tempdir;

    fn layout_under(prefix: &Path) -> FsLayout {
        let layout = FsLayout::system(Some(prefix.to_path_buf()));
        // Pre-create the bin dir so the canonical-roots check in
        // path_safety has something to canonicalise. Other tests can
        // create extra subdirs as needed.
        fs::create_dir_all(&layout.bin_dir).expect("mkdir bin_dir");
        layout
    }

    fn anolisa_owned(path: PathBuf, sha256: Option<String>) -> OwnedFile {
        OwnedFile {
            path,
            owner: FileOwner::Anolisa,
            sha256,
            kind: OwnedFileKind::File,
            referent: None,
            mode: None,
            capabilities: Vec::new(),
        }
    }

    #[test]
    fn external_owned_file_is_skipped_without_hashing() {
        // Non-Anolisa owners must short-circuit before any filesystem
        // touch so we never accidentally hash third-party files. The
        // path-safety guard never even runs.
        let tmp = tempdir().expect("tempdir");
        let layout = layout_under(tmp.path());
        let owned = OwnedFile {
            path: PathBuf::from("/definitely/not/here"),
            owner: FileOwner::External,
            sha256: Some("deadbeef".to_string()),
            kind: OwnedFileKind::File,
            referent: None,
            mode: None,
            capabilities: Vec::new(),
        };
        assert_eq!(check_owned_file(&layout, &owned), IntegrityStatus::Skipped);
    }

    #[test]
    fn path_outside_owned_roots_is_refused_without_stat() {
        // The path-boundary guard must fire BEFORE any filesystem touch.
        // We pick `/etc/shadow` which does exist on most Linux dev hosts
        // — if integrity were to stat it the test would still pass on
        // status grounds, but on macOS the file does not exist and a
        // missing-file fallback would mask the bug. Asserting
        // OutOfBounds proves we did not reach stat.
        let tmp = tempdir().expect("tempdir");
        let layout = layout_under(tmp.path());
        let owned = anolisa_owned(PathBuf::from("/etc/shadow"), Some("deadbeef".to_string()));
        assert_eq!(
            check_owned_file(&layout, &owned),
            IntegrityStatus::OutOfBounds,
        );
    }

    #[test]
    fn traversal_segment_under_a_root_is_refused() {
        // A forged path that lexically starts under bin_dir but contains
        // a `..` must be refused as out_of_bounds — same wire surface as
        // a fully-external path so a forged state file cannot signal
        // anything more specific than "we refused".
        let tmp = tempdir().expect("tempdir");
        let layout = layout_under(tmp.path());
        let path = layout.bin_dir.join("..").join("escape");
        let owned = anolisa_owned(path, Some("deadbeef".to_string()));
        assert_eq!(
            check_owned_file(&layout, &owned),
            IntegrityStatus::OutOfBounds,
        );
    }

    #[test]
    fn missing_file_reports_missing_status() {
        let tmp = tempdir().expect("tempdir");
        let layout = layout_under(tmp.path());
        let owned = anolisa_owned(layout.bin_dir.join("absent"), Some("deadbeef".to_string()));
        assert_eq!(
            check_owned_file(&layout, &owned),
            IntegrityStatus::MissingFile,
        );
    }

    #[test]
    fn file_present_without_recorded_sha_is_unverified() {
        let tmp = tempdir().expect("tempdir");
        let layout = layout_under(tmp.path());
        let path = layout.bin_dir.join("foo");
        fs::write(&path, b"payload").expect("write");
        let owned = anolisa_owned(path, None);
        assert_eq!(
            check_owned_file(&layout, &owned),
            IntegrityStatus::Unverified,
        );
    }

    #[test]
    #[cfg(unix)]
    fn config_skips_only_content_checks() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let tmp = tempdir().expect("tempdir");
        let layout = layout_under(tmp.path());
        let path = layout.bin_dir.join("settings.toml");
        fs::write(&path, b"operator edit").expect("write");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("chmod");
        let mut owned = anolisa_owned(path.clone(), Some("old digest".into()));
        owned.kind = OwnedFileKind::Config;
        owned.mode = Some("0644".into());
        assert_eq!(check_owned_file(&layout, &owned), IntegrityStatus::Ok);
        owned.sha256 = None;
        assert_eq!(check_owned_file(&layout, &owned), IntegrityStatus::Ok);

        #[cfg(target_os = "linux")]
        {
            owned.capabilities.push("CAP_NET_BIND_SERVICE".into());
            assert!(matches!(
                check_owned_file(&layout, &owned),
                IntegrityStatus::CapabilityMismatch { .. }
            ));
            owned.capabilities.clear();
        }

        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("chmod");
        assert!(matches!(
            check_owned_file(&layout, &owned),
            IntegrityStatus::ModeMismatch { .. }
        ));
        fs::remove_file(&path).expect("remove");
        assert_eq!(
            check_owned_file(&layout, &owned),
            IntegrityStatus::MissingFile
        );
        fs::create_dir(&path).expect("directory");
        assert_eq!(
            check_owned_file(&layout, &owned),
            IntegrityStatus::NotRegularFile
        );
        fs::remove_dir(&path).expect("remove directory");
        let target = layout.bin_dir.join("target");
        fs::write(&target, b"target").expect("target");
        symlink(&target, &path).expect("symlink");
        assert_eq!(check_owned_file(&layout, &owned), IntegrityStatus::Symlink);
        owned.path = tmp.path().join("outside-owned-roots");
        assert_eq!(
            check_owned_file(&layout, &owned),
            IntegrityStatus::OutOfBounds
        );
    }

    #[test]
    fn matching_sha_reports_ok() {
        let tmp = tempdir().expect("tempdir");
        let layout = layout_under(tmp.path());
        let path = layout.bin_dir.join("foo");
        fs::write(&path, b"payload").expect("write");
        let owned = anolisa_owned(
            path,
            Some("239f59ed55e737c77147cf55ad0c1b030b6d7ee748a7426952f9b852d5a935e5".to_string()),
        );
        assert_eq!(check_owned_file(&layout, &owned), IntegrityStatus::Ok);
    }

    /// Regression for #2251: `sec-core` installed via the raw backend owns
    /// `libtorch_cpu.so` (~440 MB). Exceeding the probe budget must not be
    /// reported as `read_error`, because `read_error` is a failure and the
    /// component was marked `failed` for owning a large, intact file.
    ///
    /// The fixture is sparse — `set_len` sets the inode size without
    /// allocating blocks — and the size gate fires before any read, so the
    /// test neither allocates nor hashes the nominal byte count.
    #[test]
    fn file_over_probe_ceiling_reports_budget_not_read_error() {
        let tmp = tempdir().expect("tempdir");
        let layout = layout_under(tmp.path());
        let path = layout.bin_dir.join("libtorch_cpu.so");
        let f = fs::File::create(&path).expect("create");
        let size = MAX_PROBE_BYTES + 1;
        f.set_len(size).expect("set_len");
        drop(f);

        // A digest IS recorded — install hashed this file with no ceiling.
        // The probe declining to re-read it must not read as damage.
        let owned = anolisa_owned(path, Some("deadbeef".to_string()));
        let status = check_owned_file(&layout, &owned);

        assert_eq!(
            status,
            IntegrityStatus::ProbeLimitExceeded {
                size,
                limit: MAX_PROBE_BYTES,
            }
        );
        assert!(!status.is_failure(), "must degrade, not fail");
        assert_eq!(status.label(), "probe_limit_exceeded");
    }

    /// An oversized file with no recorded digest must report `Unverified`,
    /// not `ProbeLimitExceeded`: the latter's contract is "a digest exists
    /// but this run skipped it", and reporting it here would tell operators
    /// an integrity baseline exists where none was ever recorded.
    #[test]
    fn oversized_file_without_recorded_digest_is_unverified() {
        let tmp = tempdir().expect("tempdir");
        let layout = layout_under(tmp.path());
        let path = layout.bin_dir.join("huge-legacy-blob");
        let f = fs::File::create(&path).expect("create");
        f.set_len(MAX_PROBE_BYTES + 1).expect("set_len");
        drop(f);

        let owned = anolisa_owned(path, None);
        assert_eq!(
            check_owned_file(&layout, &owned),
            IntegrityStatus::Unverified,
        );
    }

    /// A file that grows while the probe streams it must fail hard, not
    /// degrade. Otherwise a process with write access could dodge the digest
    /// comparison by appending mid-read: the hash never completes and the
    /// tampered file would report as merely unverified.
    ///
    /// The guard is anchored on the length `stat` observed, so this is
    /// exercised by handing `hash_file_sha256` a budget smaller than the
    /// file — deterministic, and equivalent to the file having grown by that
    /// difference between the stat and the read.
    #[test]
    fn growth_during_read_is_a_hard_failure_not_a_budget_stop() {
        let tmp = tempdir().expect("tempdir");
        let layout = layout_under(tmp.path());
        let path = layout.bin_dir.join("grows");
        fs::write(&path, vec![b'x'; 4096]).expect("write");

        // stat said 1024; the read finds 4096.
        let err = hash_file_sha256(&path, 1024).expect_err("must refuse to hash");
        assert!(err.to_string().contains("grew past"), "msg: {err}");

        // And that error class is decisive, not a degradation.
        assert!(IntegrityStatus::ReadError(err.to_string()).is_failure());
    }

    /// Growth that stays under the ceiling is caught too — the budget is the
    /// observed length, not the 2 GiB ceiling, so a small file cannot be
    /// padded to hide a content change.
    #[test]
    fn growth_below_the_ceiling_is_still_caught() {
        let tmp = tempdir().expect("tempdir");
        let layout = layout_under(tmp.path());
        let path = layout.bin_dir.join("small-but-grown");
        fs::write(&path, b"payload-plus-appended-junk").expect("write");

        assert!(hash_file_sha256(&path, 7).is_err());
        // Same file hashed within its observed length succeeds.
        assert!(hash_file_sha256(&path, 26).is_ok());
    }

    /// The budget outcome sorts with the other "not proved either way"
    /// states, below every real finding — callers fold via `max`, so a
    /// misplaced variant would let an oversized file mask a sha mismatch.
    #[test]
    fn probe_limit_sorts_above_unverified_and_below_failures() {
        let limited = IntegrityStatus::ProbeLimitExceeded {
            size: MAX_PROBE_BYTES + 1,
            limit: MAX_PROBE_BYTES,
        };
        assert!(limited > IntegrityStatus::Unverified);
        assert!(limited < IntegrityStatus::OutOfBounds);
        assert!(
            limited
                < IntegrityStatus::ShaMismatch {
                    expected: "a".into(),
                    actual: "b".into(),
                }
        );
    }

    #[test]
    #[cfg(unix)]
    fn matching_content_with_wrong_mode_reports_mismatch() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempdir().expect("tempdir");
        let layout = layout_under(tmp.path());
        let path = layout.bin_dir.join("foo");
        fs::write(&path, b"payload").expect("write");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("chmod");
        let mut owned = anolisa_owned(
            path,
            Some("239f59ed55e737c77147cf55ad0c1b030b6d7ee748a7426952f9b852d5a935e5".to_string()),
        );
        owned.mode = Some("0755".to_string());

        assert_eq!(
            check_owned_file(&layout, &owned),
            IntegrityStatus::ModeMismatch {
                expected: "0755".to_string(),
                actual: "0644".to_string(),
            }
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn missing_recorded_file_capability_reports_mismatch() {
        let tmp = tempdir().expect("tempdir");
        let layout = layout_under(tmp.path());
        let path = layout.bin_dir.join("foo");
        fs::write(&path, b"payload").expect("write");
        let mut owned = anolisa_owned(
            path,
            Some("239f59ed55e737c77147cf55ad0c1b030b6d7ee748a7426952f9b852d5a935e5".to_string()),
        );
        owned.capabilities = vec!["CAP_BPF".to_string()];

        assert_eq!(
            check_owned_file(&layout, &owned),
            IntegrityStatus::CapabilityMismatch {
                expected: "cap_bpf=ep".to_string(),
                actual: "none".to_string(),
            }
        );
    }

    #[test]
    fn diverged_sha_reports_mismatch_with_both_values() {
        let tmp = tempdir().expect("tempdir");
        let layout = layout_under(tmp.path());
        let path = layout.bin_dir.join("foo");
        fs::write(&path, b"payload").expect("write");
        let expected = "0000000000000000000000000000000000000000000000000000000000000000";
        let owned = anolisa_owned(path, Some(expected.to_string()));

        match check_owned_file(&layout, &owned) {
            IntegrityStatus::ShaMismatch {
                expected: e,
                actual,
            } => {
                assert_eq!(e, expected);
                assert_eq!(
                    actual,
                    "239f59ed55e737c77147cf55ad0c1b030b6d7ee748a7426952f9b852d5a935e5"
                );
            }
            other => panic!("expected ShaMismatch, got {other:?}"),
        }
    }

    #[test]
    #[cfg(unix)]
    fn old_state_symlink_under_kind_file_is_refused() {
        // kind=File (pre-v4 default) + symlink on disk → Symlink refused,
        // regardless of whether the hash matches. The probe does NOT
        // follow the symlink; migration to kind=Symlink happens upstream.
        let tmp = tempdir().expect("tempdir");
        let layout = layout_under(tmp.path());
        fs::create_dir_all(&layout.datadir).expect("mkdir datadir");
        let target = layout.datadir.join("decoy");
        fs::write(&target, b"decoy-payload").expect("write decoy");
        let link = layout.bin_dir.join("hello");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        let owned = anolisa_owned(link, Some("deadbeef".to_string()));
        assert_eq!(check_owned_file(&layout, &owned), IntegrityStatus::Symlink,);
    }

    #[test]
    fn directory_at_owned_path_is_refused() {
        // A directory exists at the expected file path — `status` must
        // refuse rather than try to hash a directory entry.
        let tmp = tempdir().expect("tempdir");
        let layout = layout_under(tmp.path());
        let path = layout.bin_dir.join("hello");
        fs::create_dir_all(&path).expect("mkdir");
        let owned = anolisa_owned(path, Some("deadbeef".to_string()));
        assert_eq!(
            check_owned_file(&layout, &owned),
            IntegrityStatus::NotRegularFile,
        );
    }

    #[test]
    fn label_and_is_failure_match_severity_intent() {
        // Ok / Skipped / Unverified / ProbeLimitExceeded are NOT failures —
        // they don't escalate status past Installed/Degraded respectively.
        assert!(!IntegrityStatus::Ok.is_failure());
        assert!(!IntegrityStatus::Skipped.is_failure());
        assert!(!IntegrityStatus::Unverified.is_failure());
        assert!(
            !IntegrityStatus::ProbeLimitExceeded {
                size: MAX_PROBE_BYTES + 1,
                limit: MAX_PROBE_BYTES,
            }
            .is_failure()
        );
        // All of the path-safety / IO refusals ARE failures.
        assert!(IntegrityStatus::OutOfBounds.is_failure());
        assert!(IntegrityStatus::Symlink.is_failure());
        assert!(IntegrityStatus::NotRegularFile.is_failure());
        assert!(IntegrityStatus::MissingFile.is_failure());
        assert!(IntegrityStatus::ReadError("permission denied".into()).is_failure());
        assert!(
            IntegrityStatus::ReferentMismatch {
                expected: "a".into(),
                actual: "b".into()
            }
            .is_failure()
        );
        assert!(
            IntegrityStatus::ModeMismatch {
                expected: "0755".into(),
                actual: "0644".into()
            }
            .is_failure()
        );
        assert!(
            IntegrityStatus::CapabilityMismatch {
                expected: "cap_bpf=ep".into(),
                actual: "none".into()
            }
            .is_failure()
        );
        assert!(
            IntegrityStatus::ShaMismatch {
                expected: "a".into(),
                actual: "b".into()
            }
            .is_failure()
        );
        // Wire labels are stable snake_case.
        assert_eq!(IntegrityStatus::Ok.label(), "ok");
        assert_eq!(IntegrityStatus::Skipped.label(), "skipped");
        assert_eq!(IntegrityStatus::Unverified.label(), "unverified");
        assert_eq!(
            IntegrityStatus::ProbeLimitExceeded { size: 1, limit: 0 }.label(),
            "probe_limit_exceeded"
        );
        assert_eq!(IntegrityStatus::OutOfBounds.label(), "out_of_bounds");
        assert_eq!(IntegrityStatus::Symlink.label(), "symlink_refused");
        assert_eq!(IntegrityStatus::NotRegularFile.label(), "not_regular_file");
        assert_eq!(IntegrityStatus::MissingFile.label(), "missing_file");
        assert_eq!(IntegrityStatus::ReadError("x".into()).label(), "read_error");
        assert_eq!(
            IntegrityStatus::ReferentMismatch {
                expected: "a".into(),
                actual: "b".into()
            }
            .label(),
            "referent_mismatch"
        );
        assert_eq!(
            IntegrityStatus::ModeMismatch {
                expected: "0755".into(),
                actual: "0644".into()
            }
            .label(),
            "mode_mismatch"
        );
        assert_eq!(
            IntegrityStatus::CapabilityMismatch {
                expected: "cap_bpf=ep".into(),
                actual: "none".into()
            }
            .label(),
            "capability_mismatch"
        );
        assert_eq!(
            IntegrityStatus::ShaMismatch {
                expected: "a".into(),
                actual: "b".into()
            }
            .label(),
            "sha256_mismatch"
        );
    }

    #[test]
    #[cfg(unix)]
    fn managed_symlink_ok_reports_ok() {
        let tmp = tempdir().expect("tempdir");
        let layout = layout_under(tmp.path());
        let target = layout.bin_dir.join("real-bin");
        fs::write(&target, b"binary-content").expect("write target");
        let link = layout.bin_dir.join("alias");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        let owned = OwnedFile {
            path: link,
            owner: FileOwner::Anolisa,
            sha256: None,
            kind: OwnedFileKind::Symlink,
            referent: Some(target),
            mode: None,
            capabilities: Vec::new(),
        };
        assert_eq!(check_owned_file(&layout, &owned), IntegrityStatus::Ok);
    }

    #[test]
    #[cfg(unix)]
    fn managed_symlink_referent_mismatch() {
        let tmp = tempdir().expect("tempdir");
        let layout = layout_under(tmp.path());
        let real_target = layout.bin_dir.join("real-bin");
        fs::write(&real_target, b"real").expect("write real");
        let recorded_target = layout.bin_dir.join("expected-bin");
        fs::write(&recorded_target, b"expected").expect("write expected");
        let link = layout.bin_dir.join("alias");
        std::os::unix::fs::symlink(&real_target, &link).expect("symlink");
        let owned = OwnedFile {
            path: link,
            owner: FileOwner::Anolisa,
            sha256: None,
            kind: OwnedFileKind::Symlink,
            referent: Some(recorded_target.clone()),
            mode: None,
            capabilities: Vec::new(),
        };
        match check_owned_file(&layout, &owned) {
            IntegrityStatus::ReferentMismatch { expected, actual } => {
                assert_eq!(expected, recorded_target.display().to_string());
                assert_eq!(actual, real_target.display().to_string());
            }
            other => panic!("expected ReferentMismatch, got {other:?}"),
        }
    }

    #[test]
    #[cfg(unix)]
    fn managed_symlink_dangling_referent_reports_error() {
        let tmp = tempdir().expect("tempdir");
        let layout = layout_under(tmp.path());
        let target = layout.bin_dir.join("gone");
        let link = layout.bin_dir.join("alias");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        let owned = OwnedFile {
            path: link,
            owner: FileOwner::Anolisa,
            sha256: None,
            kind: OwnedFileKind::Symlink,
            referent: Some(target),
            mode: None,
            capabilities: Vec::new(),
        };
        match check_owned_file(&layout, &owned) {
            IntegrityStatus::ReadError(msg) => {
                assert!(msg.contains("dangling"), "msg: {msg}");
            }
            other => panic!("expected ReadError(dangling), got {other:?}"),
        }
    }
}
