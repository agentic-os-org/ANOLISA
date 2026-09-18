//! CLI self-update support for release-manifest checks and binary replacement.
//!
//! The update endpoint is a lightweight TOML file (`release-manifest.toml`)
//! that declares the latest version and per-platform download URLs with
//! SHA256 checksums. This is intentionally separate from the
//! [`DistributionIndex`](crate::DistributionIndex) used for component
//! artifacts: the CLI binary swap must never share a transaction with
//! component updates (spec §7.3, decision §11.2).

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use flate2::read::GzDecoder;
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tar::Archive;

use crate::lock::{InstallLock, LockError};

const DEFAULT_UPDATE_URL: &str = "https://anolisa.oss-cn-hangzhou.aliyuncs.com/anolisa-releases/anolisa/v1/cli/release-manifest.toml";
const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const HTTP_READ_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_DOWNLOAD_BYTES: u64 = 512 * 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;

/// Top-level release manifest fetched from the update endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseManifest {
    /// Wire-format version; clients reject unknown schemas before using any
    /// artifact metadata.
    pub schema_version: u32,

    /// Latest available CLI version, parsed as semver before comparison.
    pub version: String,

    /// Platform-specific binaries advertised by this release.
    ///
    /// Defaults to an empty list so older or check-only manifests still parse,
    /// while update execution fails later with [`SelfUpdateError::NoArtifact`].
    #[serde(default)]
    pub artifacts: Vec<ReleaseArtifact>,
}

/// One downloadable binary for a specific platform.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseArtifact {
    /// Operating-system selector matching [`current_os`].
    pub os: String,

    /// Architecture selector matching [`current_arch`].
    pub arch: String,

    /// Download URL for the gzipped tar archive containing the CLI binary.
    pub url: String,

    /// Expected SHA256 digest of the tar.gz archive.
    pub sha256: String,

    /// Size of the tar.gz archive; used as progress-total hint and download cap.
    pub size: Option<u64>,
}

/// Errors raised during self-update.
#[derive(Debug, thiserror::Error)]
pub enum SelfUpdateError {
    /// The manifest endpoint could not be fetched or read within limits.
    #[error("failed to fetch release manifest from {url}: {reason}")]
    FetchManifest {
        /// Endpoint URL used for the manifest request.
        url: String,

        /// Transport, UTF-8, or body-size failure detail.
        reason: String,
    },

    /// The manifest body is not valid TOML for the current schema.
    #[error("failed to parse release manifest: {0}")]
    ParseManifest(String),

    /// The local or remote version string is not semver-compatible.
    #[error("release manifest version '{0}' is not valid semver")]
    InvalidVersion(String),

    /// No artifact matches the current host selector tuple.
    #[error("no artifact found for {os}/{arch} in release manifest")]
    NoArtifact {
        /// Operating-system selector requested by this host.
        os: String,

        /// Architecture selector requested by this host.
        arch: String,
    },

    /// The binary artifact request failed before filesystem staging.
    #[error("download failed: {reason}")]
    Download {
        /// HTTP or transport failure detail.
        reason: String,
    },

    /// The running executable path could not be resolved safely.
    #[error("cannot determine current executable path: {0}")]
    CurrentExe(String),

    /// A filesystem operation failed.
    #[error("io error at {path}: {source}")]
    Io {
        /// Path involved in the failed operation.
        path: PathBuf,

        /// Underlying I/O failure.
        #[source]
        source: io::Error,
    },

    /// Downloaded bytes did not match the manifest digest.
    #[error("sha256 mismatch: expected {expected}, got {actual}")]
    ChecksumMismatch {
        /// Manifest digest normalized to lowercase hex.
        expected: String,

        /// Digest computed from the downloaded bytes.
        actual: String,
    },

    /// The previous binary could not be preserved before replacement.
    #[error("failed to backup current binary to {path}: {source}")]
    BackupFailed {
        /// Backup path derived from the executable path.
        path: PathBuf,

        /// Underlying I/O failure.
        #[source]
        source: io::Error,
    },

    /// Replacement failed, then restoration from backup failed too.
    #[error("update failed and rollback also failed at {path}: {source}")]
    RollbackFailed {
        /// Executable path that could not be restored.
        path: PathBuf,

        /// Underlying rollback failure.
        #[source]
        source: io::Error,
    },

    /// Another process is already updating the same executable.
    #[error("install lock at {path} is already held by another process")]
    LockHeld {
        /// Lock path derived from the executable path.
        path: PathBuf,
    },

    /// The artifact response exceeded its declared or default cap.
    #[error("download exceeded size limit: {received} bytes received, limit is {limit} bytes")]
    DownloadTooLarge {
        /// Maximum accepted byte count.
        limit: u64,

        /// Bytes observed before aborting the transfer.
        received: u64,
    },

    /// The manifest schema is newer or otherwise incompatible.
    #[error("unsupported release manifest schema version {version} (expected {expected})")]
    UnsupportedSchema {
        /// Schema version found in the manifest.
        version: u32,

        /// Schema version supported by this client.
        expected: u32,
    },

    /// The downloaded archive could not be extracted.
    #[error("failed to extract binary from archive: {reason}")]
    ExtractFailed { reason: String },
}

// -- Public helpers ---------------------------------------------------

/// Resolves the update endpoint URL.
///
/// Honors `ANOLISA_UPDATE_URL` for internal mirrors and tests, then falls
/// back to the compiled-in default.
pub fn update_url() -> String {
    std::env::var("ANOLISA_UPDATE_URL").unwrap_or_else(|_| DEFAULT_UPDATE_URL.to_string())
}

/// Returns the current OS string used in release manifests.
pub fn current_os() -> &'static str {
    std::env::consts::OS
}

/// Returns the current architecture string used in release manifests.
pub fn current_arch() -> &'static str {
    std::env::consts::ARCH
}

/// Resolve the canonical path to the currently running executable.
///
/// # Errors
///
/// Returns [`SelfUpdateError::CurrentExe`] when the process executable path
/// cannot be read or canonicalized.
pub fn resolve_current_exe() -> Result<PathBuf, SelfUpdateError> {
    std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .map_err(|e| SelfUpdateError::CurrentExe(e.to_string()))
}

// -- Release manifest -------------------------------------------------

/// Fetch and parse the release manifest from `endpoint_url`.
///
/// The response body is capped at 1 MiB to prevent an anomalous
/// endpoint from exhausting memory.
///
/// # Errors
///
/// Returns [`SelfUpdateError::FetchManifest`] for HTTP, UTF-8, or body-size
/// failures, and manifest parse/schema errors from
/// [`ReleaseManifest::from_toml_str`].
pub fn fetch_manifest(endpoint_url: &str) -> Result<ReleaseManifest, SelfUpdateError> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(HTTP_CONNECT_TIMEOUT)
        .timeout_read(HTTP_READ_TIMEOUT)
        .build();

    let response =
        agent
            .get(endpoint_url)
            .call()
            .map_err(|err| SelfUpdateError::FetchManifest {
                url: endpoint_url.to_string(),
                reason: err.to_string(),
            })?;

    let mut buf = Vec::new();
    response
        .into_reader()
        .take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut buf)
        .map_err(|err| SelfUpdateError::FetchManifest {
            url: endpoint_url.to_string(),
            reason: err.to_string(),
        })?;

    if buf.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(SelfUpdateError::FetchManifest {
            url: endpoint_url.to_string(),
            reason: format!(
                "response body exceeds {MAX_MANIFEST_BYTES} byte limit for release manifest"
            ),
        });
    }

    let body = String::from_utf8(buf).map_err(|err| SelfUpdateError::FetchManifest {
        url: endpoint_url.to_string(),
        reason: err.to_string(),
    })?;

    ReleaseManifest::from_toml_str(&body)
}

const MANIFEST_SCHEMA_VERSION: u32 = 1;

impl ReleaseManifest {
    /// Parse from a TOML string, rejecting unsupported schema versions.
    ///
    /// # Errors
    ///
    /// Returns [`SelfUpdateError::ParseManifest`] for invalid TOML and
    /// [`SelfUpdateError::UnsupportedSchema`] for unknown schema versions.
    pub fn from_toml_str(s: &str) -> Result<Self, SelfUpdateError> {
        let manifest: Self =
            toml::from_str(s).map_err(|e| SelfUpdateError::ParseManifest(e.to_string()))?;
        if manifest.schema_version != MANIFEST_SCHEMA_VERSION {
            return Err(SelfUpdateError::UnsupportedSchema {
                version: manifest.schema_version,
                expected: MANIFEST_SCHEMA_VERSION,
            });
        }
        Ok(manifest)
    }

    /// Parse [`Self::version`] as semver.
    ///
    /// # Errors
    ///
    /// Returns [`SelfUpdateError::InvalidVersion`] when the manifest version
    /// is not valid semver.
    pub fn version(&self) -> Result<Version, SelfUpdateError> {
        Version::parse(&self.version)
            .map_err(|_| SelfUpdateError::InvalidVersion(self.version.clone()))
    }

    /// Find the artifact matching the given platform tuple.
    pub fn artifact_for(&self, os: &str, arch: &str) -> Option<&ReleaseArtifact> {
        self.artifacts.iter().find(|a| a.os == os && a.arch == arch)
    }
}

// -- Version check ----------------------------------------------------

/// Check whether a newer version is available.
///
/// Returns `Some(manifest)` when the remote version is strictly newer
/// than `current_version`, `None` when already up to date.
///
/// # Errors
///
/// Returns manifest fetch/parse errors or [`SelfUpdateError::InvalidVersion`]
/// if either version string is not semver-compatible.
pub fn check_update(
    endpoint_url: &str,
    current_version: &str,
) -> Result<Option<ReleaseManifest>, SelfUpdateError> {
    let manifest = fetch_manifest(endpoint_url)?;
    let remote = manifest.version()?;
    let local = Version::parse(current_version)
        .map_err(|_| SelfUpdateError::InvalidVersion(current_version.to_string()))?;

    if remote > local {
        Ok(Some(manifest))
    } else {
        Ok(None)
    }
}

// -- Download with progress ------------------------------------------

/// Receives `(bytes_downloaded, total_bytes_or_none)` during artifact download.
pub type ProgressFn = Box<dyn Fn(u64, Option<u64>) + Send>;

/// Download `url` to `dest`, streaming SHA256 and invoking `on_progress`.
///
/// `expected_size` is used for progress reporting (preferred over HTTP
/// Content-Length). `max_bytes` enforces an upper bound — the download is
/// aborted with [`SelfUpdateError::DownloadTooLarge`] if exceeded.
///
/// # Errors
///
/// Returns download, filesystem, checksum, or size-limit errors. A partially
/// written destination is removed on read/write/checksum/size failures.
pub fn download_with_progress(
    url: &str,
    dest: &Path,
    expected_sha256: &str,
    expected_size: Option<u64>,
    max_bytes: Option<u64>,
    on_progress: Option<&ProgressFn>,
) -> Result<(), SelfUpdateError> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(HTTP_CONNECT_TIMEOUT)
        .timeout_read(HTTP_READ_TIMEOUT)
        .build();

    let response = agent
        .get(url)
        .call()
        .map_err(|err| SelfUpdateError::Download {
            reason: err.to_string(),
        })?;

    let content_length: Option<u64> = response
        .header("Content-Length")
        .and_then(|v| v.parse().ok());
    let progress_total = expected_size.or(content_length);

    let mut reader = response.into_reader();

    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|source| SelfUpdateError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(nix::libc::O_NOFOLLOW);
    }
    let mut out = opts.open(dest).map_err(|source| SelfUpdateError::Io {
        path: dest.to_path_buf(),
        source,
    })?;

    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    let mut downloaded: u64 = 0;

    loop {
        let n = reader.read(&mut buf).map_err(|source| {
            let _ = fs::remove_file(dest);
            SelfUpdateError::Io {
                path: dest.to_path_buf(),
                source,
            }
        })?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        out.write_all(&buf[..n]).map_err(|source| {
            let _ = fs::remove_file(dest);
            SelfUpdateError::Io {
                path: dest.to_path_buf(),
                source,
            }
        })?;
        downloaded += n as u64;
        if let Some(limit) = max_bytes
            && downloaded > limit
        {
            let _ = fs::remove_file(dest);
            return Err(SelfUpdateError::DownloadTooLarge {
                limit,
                received: downloaded,
            });
        }
        if let Some(cb) = on_progress {
            cb(downloaded, progress_total);
        }
    }
    out.flush().map_err(|source| SelfUpdateError::Io {
        path: dest.to_path_buf(),
        source,
    })?;

    let actual = hex_lower(&hasher.finalize());
    let expected_norm = expected_sha256.to_ascii_lowercase();
    if actual != expected_norm {
        let _ = fs::remove_file(dest);
        return Err(SelfUpdateError::ChecksumMismatch {
            expected: expected_norm,
            actual,
        });
    }

    Ok(())
}

// -- Binary replacement -----------------------------------------------

/// Download the new binary and atomically replace the current executable.
///
/// An exclusive lock derived from `current_exe` (`{exe}.update-lock`) is
/// held for the duration, preventing concurrent self-updates targeting
/// the same binary regardless of `--install-mode`.
///
/// # Errors
///
/// Returns download, lock, permission, backup, replacement, or rollback
/// failures. When replacement fails after a backup is created, the function
/// attempts to restore the previous binary before returning.
pub fn perform_update(
    artifact: &ReleaseArtifact,
    current_exe: &Path,
    on_progress: Option<&ProgressFn>,
) -> Result<(), SelfUpdateError> {
    let lock_path = self_update_lock_path(current_exe);
    let _lock = acquire_lock(&lock_path)?;

    let staging = staging_path(current_exe);
    let archive = archive_path(current_exe);
    let backup = backup_path(current_exe);

    // Clean up leftovers from a previous failed attempt.
    clean_staging(&staging)?;
    let _ = fs::remove_file(&archive);
    let _ = fs::remove_file(&backup);

    // Download the tar.gz archive with SHA256 verification.
    let max_bytes = artifact
        .size
        .map_or(MAX_DOWNLOAD_BYTES, |s| s.min(MAX_DOWNLOAD_BYTES));
    download_with_progress(
        &artifact.url,
        &archive,
        &artifact.sha256,
        artifact.size,
        Some(max_bytes),
        on_progress,
    )?;

    // Extract the binary from the verified archive.
    let extract_result = extract_binary_from_archive(&archive, &staging);
    let _ = fs::remove_file(&archive);
    extract_result?;

    let orig_meta = fs::metadata(current_exe).map_err(|source| SelfUpdateError::Io {
        path: current_exe.to_path_buf(),
        source,
    })?;

    // Apply ownership first, then permissions. chown(2) clears setuid/
    // setgid bits, so chmod must come after to restore them.
    #[cfg(unix)]
    {
        apply_original_ownership(&staging, &orig_meta);
        apply_original_permissions(&staging, &orig_meta)?;
    }
    #[cfg(not(unix))]
    {
        let perms = orig_meta.permissions();
        fs::set_permissions(&staging, perms).map_err(|source| SelfUpdateError::Io {
            path: staging.clone(),
            source,
        })?;
    }

    // Backup via hard link keeps the executable path present throughout.
    // Falls back to copy when hard_link fails (cross-filesystem, etc.).
    if fs::hard_link(current_exe, &backup).is_err() {
        fs::copy(current_exe, &backup).map_err(|source| {
            let _ = fs::remove_file(&staging);
            SelfUpdateError::BackupFailed {
                path: backup.clone(),
                source,
            }
        })?;
    }

    // The executable path never disappears on same-filesystem rename.
    if let Err(err) = rename_or_copy(&staging, current_exe) {
        if let Err(rb_err) = rename_or_copy(&backup, current_exe) {
            let _ = fs::remove_file(&staging);
            return Err(SelfUpdateError::RollbackFailed {
                path: current_exe.to_path_buf(),
                source: match rb_err {
                    SelfUpdateError::Io { source, .. } => source,
                    _ => io::Error::other(rb_err.to_string()),
                },
            });
        }
        let _ = fs::remove_file(&staging);
        return Err(err);
    }

    let _ = fs::remove_file(&backup);

    Ok(())
}

fn self_update_lock_path(current_exe: &Path) -> PathBuf {
    let mut s = current_exe.as_os_str().to_os_string();
    s.push(".update-lock");
    PathBuf::from(s)
}

fn acquire_lock(lock_path: &Path) -> Result<InstallLock, SelfUpdateError> {
    InstallLock::acquire(lock_path).map_err(|e| match e {
        LockError::Held { path } => SelfUpdateError::LockHeld { path },
        LockError::Io { path, source } => SelfUpdateError::Io { path, source },
    })
}

/// `rename(2)` with `EXDEV` fallback to copy + remove.
fn rename_or_copy(src: &Path, dst: &Path) -> Result<(), SelfUpdateError> {
    match fs::rename(src, dst) {
        Ok(()) => Ok(()),
        Err(ref e) if is_cross_device(e) => {
            fs::copy(src, dst).map_err(|source| SelfUpdateError::Io {
                path: dst.to_path_buf(),
                source,
            })?;
            let _ = fs::remove_file(src);
            Ok(())
        }
        Err(source) => Err(SelfUpdateError::Io {
            path: dst.to_path_buf(),
            source,
        }),
    }
}

#[cfg(unix)]
fn is_cross_device(err: &io::Error) -> bool {
    err.raw_os_error() == Some(nix::libc::EXDEV)
}

#[cfg(not(unix))]
fn is_cross_device(_err: &io::Error) -> bool {
    false
}

#[cfg(unix)]
fn apply_original_permissions(
    path: &Path,
    orig_meta: &fs::Metadata,
) -> Result<(), SelfUpdateError> {
    use std::os::unix::fs::PermissionsExt;
    let mode = orig_meta.permissions().mode();
    let perms = fs::Permissions::from_mode(mode);
    fs::set_permissions(path, perms).map_err(|source| SelfUpdateError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(unix)]
fn apply_original_ownership(path: &Path, orig_meta: &fs::Metadata) {
    use std::os::unix::fs::MetadataExt;
    let uid = orig_meta.uid();
    let gid = orig_meta.gid();
    // Non-root updates cannot chown; mode restoration still preserves executability.
    let _ = nix::unistd::chown(
        path,
        Some(nix::unistd::Uid::from_raw(uid)),
        Some(nix::unistd::Gid::from_raw(gid)),
    );
}

fn clean_staging(staging: &Path) -> Result<(), SelfUpdateError> {
    match fs::symlink_metadata(staging) {
        Ok(meta) if meta.file_type().is_symlink() => Err(SelfUpdateError::Io {
            path: staging.to_path_buf(),
            source: io::Error::new(
                io::ErrorKind::AlreadyExists,
                "staging path is a symlink — refusing to proceed",
            ),
        }),
        Ok(_) => {
            let _ = fs::remove_file(staging);
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(SelfUpdateError::Io {
            path: staging.to_path_buf(),
            source,
        }),
    }
}

fn staging_path(current_exe: &Path) -> PathBuf {
    let mut s = current_exe.as_os_str().to_os_string();
    s.push(".new.tmp");
    PathBuf::from(s)
}

fn archive_path(current_exe: &Path) -> PathBuf {
    let mut s = current_exe.as_os_str().to_os_string();
    s.push(".archive.tmp");
    PathBuf::from(s)
}

fn backup_path(current_exe: &Path) -> PathBuf {
    let mut s = current_exe.as_os_str().to_os_string();
    s.push(".old");
    PathBuf::from(s)
}

/// Extract the `anolisa` binary from a gzipped tar archive.
///
/// Searches for an entry whose file name is exactly `anolisa` (at any
/// nesting depth). The first match is written to `dest`.
fn extract_binary_from_archive(archive_file: &Path, dest: &Path) -> Result<(), SelfUpdateError> {
    let file = File::open(archive_file).map_err(|source| SelfUpdateError::Io {
        path: archive_file.to_path_buf(),
        source,
    })?;
    let decoder = GzDecoder::new(BufReader::new(file));
    let mut tar = Archive::new(decoder);

    for entry in tar.entries().map_err(|e| SelfUpdateError::ExtractFailed {
        reason: format!("cannot read tar entries: {e}"),
    })? {
        let entry = entry.map_err(|e| SelfUpdateError::ExtractFailed {
            reason: format!("corrupt tar entry: {e}"),
        })?;

        if !entry.header().entry_type().is_file() {
            continue;
        }

        let path = entry.path().map_err(|e| SelfUpdateError::ExtractFailed {
            reason: format!("invalid entry path: {e}"),
        })?;
        let file_name = path.file_name().unwrap_or_default();
        if file_name != "anolisa" {
            continue;
        }

        let declared_size = entry.header().size().unwrap_or(0);
        if declared_size > MAX_DOWNLOAD_BYTES {
            return Err(SelfUpdateError::ExtractFailed {
                reason: format!("entry size {declared_size} exceeds limit {MAX_DOWNLOAD_BYTES}"),
            });
        }

        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.custom_flags(nix::libc::O_NOFOLLOW);
        }
        let mut out = opts.open(dest).map_err(|source| SelfUpdateError::Io {
            path: dest.to_path_buf(),
            source,
        })?;

        let copied =
            io::copy(&mut entry.take(MAX_DOWNLOAD_BYTES + 1), &mut out).map_err(|source| {
                let _ = fs::remove_file(dest);
                SelfUpdateError::Io {
                    path: dest.to_path_buf(),
                    source,
                }
            })?;

        if copied > MAX_DOWNLOAD_BYTES {
            let _ = fs::remove_file(dest);
            return Err(SelfUpdateError::ExtractFailed {
                reason: format!("extracted size {copied} exceeds limit {MAX_DOWNLOAD_BYTES}"),
            });
        }

        return Ok(());
    }

    Err(SelfUpdateError::ExtractFailed {
        reason: "archive does not contain a regular file named 'anolisa'".to_string(),
    })
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
    use std::net::TcpListener;
    use std::thread;
    use tempfile::tempdir;

    fn sha256_of(bytes: &[u8]) -> String {
        let mut h = Sha256::new();
        h.update(bytes);
        hex_lower(&h.finalize())
    }

    /// Build a tar.gz archive containing a single `anolisa` entry with `content`.
    fn make_tar_gz(content: &[u8]) -> Vec<u8> {
        use flate2::Compression;
        use flate2::write::GzEncoder;

        let mut enc = GzEncoder::new(Vec::new(), Compression::fast());
        {
            let mut builder = tar::Builder::new(&mut enc);
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder
                .append_data(&mut header, "anolisa", content)
                .expect("append");
            builder.finish().expect("finish");
        }
        enc.finish().expect("gz finish")
    }

    fn serve_once(body: &'static [u8]) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            std::io::Write::write_all(&mut stream, response.as_bytes()).expect("write head");
            std::io::Write::write_all(&mut stream, body).expect("write body");
        });
        format!("http://{addr}/release-manifest.toml")
    }

    #[test]
    fn rejects_unsupported_schema_version() {
        let toml = r#"
            schema_version = 99
            version = "0.2.0"
        "#;
        let err = ReleaseManifest::from_toml_str(toml).expect_err("must reject");
        assert!(matches!(
            err,
            SelfUpdateError::UnsupportedSchema {
                version: 99,
                expected: 1
            }
        ));
    }

    #[test]
    fn parse_release_manifest() {
        let toml = r#"
            schema_version = 1
            version = "0.2.0"

            [[artifacts]]
            os = "linux"
            arch = "x86_64"
            url = "https://example.invalid/anolisa-linux-x86_64"
            sha256 = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"
            size = 12345
        "#;
        let m = ReleaseManifest::from_toml_str(toml).expect("parse");
        assert_eq!(m.schema_version, 1);
        assert_eq!(m.version, "0.2.0");
        assert_eq!(m.artifacts.len(), 1);
        assert_eq!(m.artifacts[0].os, "linux");
        assert_eq!(m.artifacts[0].arch, "x86_64");
    }

    #[test]
    fn check_update_detects_newer_version() {
        let manifest = r#"
            schema_version = 1
            version = "0.3.0"
            [[artifacts]]
            os = "linux"
            arch = "x86_64"
            url = "https://example.invalid/bin"
            sha256 = "0000000000000000000000000000000000000000000000000000000000000000"
        "#;
        let url = serve_once(manifest.as_bytes());
        let result = check_update(&url, "0.1.0").expect("check");
        assert!(result.is_some());
        assert_eq!(result.unwrap().version, "0.3.0");
    }

    #[test]
    fn check_update_returns_none_when_up_to_date() {
        let manifest = r#"
            schema_version = 1
            version = "0.1.0"
            [[artifacts]]
            os = "linux"
            arch = "x86_64"
            url = "https://example.invalid/bin"
            sha256 = "0000000000000000000000000000000000000000000000000000000000000000"
        "#;
        let url = serve_once(manifest.as_bytes());
        let result = check_update(&url, "0.1.0").expect("check");
        assert!(result.is_none());
    }

    #[test]
    fn check_update_returns_none_when_local_is_newer() {
        let manifest = r#"
            schema_version = 1
            version = "0.1.0"
            [[artifacts]]
            os = "linux"
            arch = "x86_64"
            url = "https://example.invalid/bin"
            sha256 = "0000000000000000000000000000000000000000000000000000000000000000"
        "#;
        let url = serve_once(manifest.as_bytes());
        let result = check_update(&url, "0.5.0").expect("check");
        assert!(result.is_none());
    }

    #[test]
    fn artifact_for_finds_matching_platform() {
        let m = ReleaseManifest {
            schema_version: 1,
            version: "0.2.0".into(),
            artifacts: vec![
                ReleaseArtifact {
                    os: "linux".into(),
                    arch: "x86_64".into(),
                    url: "https://example.invalid/linux-x86_64".into(),
                    sha256: "a".repeat(64),
                    size: None,
                },
                ReleaseArtifact {
                    os: "linux".into(),
                    arch: "aarch64".into(),
                    url: "https://example.invalid/linux-aarch64".into(),
                    sha256: "b".repeat(64),
                    size: None,
                },
            ],
        };
        let a = m.artifact_for("linux", "aarch64").expect("found");
        assert!(a.url.contains("aarch64"));
        assert!(m.artifact_for("darwin", "x86_64").is_none());
    }

    #[test]
    fn perform_update_replaces_binary_and_cleans_up() {
        let dir = tempdir().unwrap();
        let binary_content = b"new-binary-bytes";
        let archive = make_tar_gz(binary_content);
        let sha = sha256_of(&archive);

        let url = {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            let addr = listener.local_addr().expect("addr");
            let archive_clone = archive.clone();
            thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    archive_clone.len()
                );
                std::io::Write::write_all(&mut stream, response.as_bytes()).expect("write head");
                std::io::Write::write_all(&mut stream, &archive_clone).expect("write body");
            });
            format!("http://{addr}/anolisa.tar.gz")
        };

        let current = dir.path().join("anolisa");
        fs::write(&current, b"old-binary-bytes").unwrap();

        let artifact = ReleaseArtifact {
            os: "linux".into(),
            arch: "x86_64".into(),
            url,
            sha256: sha,
            size: Some(archive.len() as u64),
        };

        perform_update(&artifact, &current, None).expect("update ok");

        assert_eq!(fs::read(&current).unwrap(), binary_content);
        assert!(!staging_path(&current).exists(), "staging cleaned up");
        assert!(!archive_path(&current).exists(), "archive cleaned up");
        assert!(!backup_path(&current).exists(), "backup cleaned up");
    }

    #[test]
    fn perform_update_rejects_checksum_mismatch_before_replacement() {
        let dir = tempdir().unwrap();
        let payload = b"bad-binary";

        let url = {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            let addr = listener.local_addr().expect("addr");
            thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    payload.len()
                );
                std::io::Write::write_all(&mut stream, response.as_bytes()).expect("write head");
                std::io::Write::write_all(&mut stream, payload).expect("write body");
            });
            format!("http://{addr}/anolisa-bin")
        };

        let current = dir.path().join("anolisa");
        fs::write(&current, b"old-binary").unwrap();

        let artifact = ReleaseArtifact {
            os: "linux".into(),
            arch: "x86_64".into(),
            url,
            sha256: "0".repeat(64),
            size: None,
        };

        let err = perform_update(&artifact, &current, None).expect_err("must fail");
        assert!(matches!(err, SelfUpdateError::ChecksumMismatch { .. }));
        // Original file should still be intact (download failed before backup).
        assert_eq!(fs::read(&current).unwrap(), b"old-binary");
    }

    #[cfg(unix)]
    #[test]
    fn perform_update_preserves_original_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let binary_content = b"new-binary";
        let archive = make_tar_gz(binary_content);
        let sha = sha256_of(&archive);

        let url = {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            let addr = listener.local_addr().expect("addr");
            let archive_clone = archive.clone();
            thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    archive_clone.len()
                );
                std::io::Write::write_all(&mut stream, response.as_bytes()).expect("write head");
                std::io::Write::write_all(&mut stream, &archive_clone).expect("write body");
            });
            format!("http://{addr}/anolisa.tar.gz")
        };

        let current = dir.path().join("anolisa");
        fs::write(&current, b"old").unwrap();
        fs::set_permissions(&current, fs::Permissions::from_mode(0o700)).unwrap();

        let artifact = ReleaseArtifact {
            os: "linux".into(),
            arch: "x86_64".into(),
            url,
            sha256: sha,
            size: Some(archive.len() as u64),
        };

        perform_update(&artifact, &current, None).expect("update ok");

        let mode = fs::metadata(&current).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[test]
    fn progress_callback_is_invoked() {
        let dir = tempdir().unwrap();
        let payload = b"progress-test-payload-bytes!!";
        let sha = sha256_of(payload);

        let url = {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            let addr = listener.local_addr().expect("addr");
            thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    payload.len()
                );
                std::io::Write::write_all(&mut stream, response.as_bytes()).expect("write head");
                std::io::Write::write_all(&mut stream, payload).expect("write body");
            });
            format!("http://{addr}/anolisa-bin")
        };

        let dest = dir.path().join("downloaded");
        let call_count = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let counter = call_count.clone();
        let cb: ProgressFn = Box::new(move |downloaded, total| {
            counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            assert!(downloaded > 0);
            assert_eq!(total, Some(payload.len() as u64));
        });

        download_with_progress(&url, &dest, &sha, None, None, Some(&cb)).expect("download ok");
        assert!(call_count.load(std::sync::atomic::Ordering::Relaxed) > 0);
        assert_eq!(fs::read(&dest).unwrap(), payload);
    }

    #[cfg(unix)]
    #[test]
    fn clean_staging_refuses_symlink() {
        let dir = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let victim = outside.path().join("victim");
        fs::write(&victim, b"untouched").unwrap();

        let staging = dir.path().join("anolisa.new.tmp");
        std::os::unix::fs::symlink(&victim, &staging).unwrap();

        let err = clean_staging(&staging).expect_err("must refuse");
        assert!(matches!(err, SelfUpdateError::Io { .. }));
        assert_eq!(fs::read(&victim).unwrap(), b"untouched");
    }

    #[test]
    fn staging_path_appends_suffix() {
        let p = PathBuf::from("/usr/local/bin/anolisa");
        assert_eq!(
            staging_path(&p),
            PathBuf::from("/usr/local/bin/anolisa.new.tmp")
        );
    }

    #[test]
    fn backup_path_appends_old_suffix() {
        let p = PathBuf::from("/usr/local/bin/anolisa");
        assert_eq!(backup_path(&p), PathBuf::from("/usr/local/bin/anolisa.old"));
    }

    #[test]
    fn perform_update_acquires_lock_automatically() {
        let dir = tempdir().unwrap();
        let binary_content = b"locked-binary";
        let archive = make_tar_gz(binary_content);
        let sha = sha256_of(&archive);

        let url = {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            let addr = listener.local_addr().expect("addr");
            let archive_clone = archive.clone();
            thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    archive_clone.len()
                );
                std::io::Write::write_all(&mut stream, response.as_bytes()).expect("write head");
                std::io::Write::write_all(&mut stream, &archive_clone).expect("write body");
            });
            format!("http://{addr}/anolisa.tar.gz")
        };

        let current = dir.path().join("anolisa");
        fs::write(&current, b"old").unwrap();

        let artifact = ReleaseArtifact {
            os: "linux".into(),
            arch: "x86_64".into(),
            url,
            sha256: sha,
            size: Some(archive.len() as u64),
        };

        perform_update(&artifact, &current, None).expect("update ok");
        assert_eq!(fs::read(&current).unwrap(), binary_content);
        assert!(self_update_lock_path(&current).exists());
    }

    #[test]
    fn perform_update_rejects_when_lock_held() {
        let dir = tempdir().unwrap();
        let current = dir.path().join("anolisa");
        fs::write(&current, b"old").unwrap();

        // Pre-acquire the same lock that perform_update would derive.
        let lock_path = self_update_lock_path(&current);
        let _held = InstallLock::acquire(&lock_path).expect("acquire");

        let artifact = ReleaseArtifact {
            os: "linux".into(),
            arch: "x86_64".into(),
            url: "http://127.0.0.1:1/unused".into(),
            sha256: "0".repeat(64),
            size: None,
        };

        let err = perform_update(&artifact, &current, None).expect_err("must fail");
        assert!(matches!(err, SelfUpdateError::LockHeld { .. }));
    }

    #[test]
    fn download_rejects_oversized_response() {
        let dir = tempdir().unwrap();
        let payload = b"this-payload-exceeds-the-limit";
        let sha = sha256_of(payload);

        let url = {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            let addr = listener.local_addr().expect("addr");
            thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    payload.len()
                );
                std::io::Write::write_all(&mut stream, response.as_bytes()).expect("write head");
                std::io::Write::write_all(&mut stream, payload).expect("write body");
            });
            format!("http://{addr}/oversized")
        };

        let dest = dir.path().join("downloaded");
        let err =
            download_with_progress(&url, &dest, &sha, None, Some(10), None).expect_err("must fail");
        assert!(matches!(err, SelfUpdateError::DownloadTooLarge { .. }));
        assert!(!dest.exists(), "partial file cleaned up");
    }

    #[test]
    fn fetch_manifest_rejects_oversized_response() {
        let body = vec![b'#'; (MAX_MANIFEST_BYTES + 512) as usize];
        let url = {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            let addr = listener.local_addr().expect("addr");
            thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut req = [0u8; 1024];
                let _ = stream.read(&mut req);
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                std::io::Write::write_all(&mut stream, header.as_bytes()).expect("write head");
                std::io::Write::write_all(&mut stream, &body).expect("write body");
            });
            format!("http://{addr}/release-manifest.toml")
        };

        let err = fetch_manifest(&url).expect_err("must reject oversized manifest");
        match &err {
            SelfUpdateError::FetchManifest { reason, .. } => {
                assert!(
                    reason.contains("limit"),
                    "error should mention size limit, got: {reason}"
                );
            }
            other => panic!("expected FetchManifest, got {other:?}"),
        }
    }

    #[test]
    fn extract_skips_directory_entry_named_anolisa() {
        use flate2::Compression;
        use flate2::write::GzEncoder;

        let binary_content = b"real-binary";

        // Build archive: directory "anolisa/" followed by file "anolisa/anolisa".
        let archive_bytes = {
            let mut enc = GzEncoder::new(Vec::new(), Compression::fast());
            {
                let mut builder = tar::Builder::new(&mut enc);

                // Directory entry with basename "anolisa".
                let mut dir_header = tar::Header::new_gnu();
                dir_header.set_entry_type(tar::EntryType::Directory);
                dir_header.set_size(0);
                dir_header.set_mode(0o755);
                dir_header.set_cksum();
                builder
                    .append_data(&mut dir_header, "anolisa/", &[] as &[u8])
                    .expect("append dir");

                // Regular file "anolisa/anolisa".
                let mut file_header = tar::Header::new_gnu();
                file_header.set_size(binary_content.len() as u64);
                file_header.set_mode(0o755);
                file_header.set_cksum();
                builder
                    .append_data(&mut file_header, "anolisa/anolisa", &binary_content[..])
                    .expect("append file");

                builder.finish().expect("finish");
            }
            enc.finish().expect("gz finish")
        };

        let dir = tempdir().unwrap();
        let archive_path = dir.path().join("test.tar.gz");
        fs::write(&archive_path, &archive_bytes).unwrap();

        let dest = dir.path().join("extracted");
        extract_binary_from_archive(&archive_path, &dest).expect("extract ok");

        assert_eq!(fs::read(&dest).unwrap(), binary_content);
    }

    #[test]
    fn extract_rejects_archive_without_regular_file() {
        use flate2::Compression;
        use flate2::write::GzEncoder;

        // Archive with only a directory named "anolisa".
        let archive_bytes = {
            let mut enc = GzEncoder::new(Vec::new(), Compression::fast());
            {
                let mut builder = tar::Builder::new(&mut enc);
                let mut dir_header = tar::Header::new_gnu();
                dir_header.set_entry_type(tar::EntryType::Directory);
                dir_header.set_size(0);
                dir_header.set_mode(0o755);
                dir_header.set_cksum();
                builder
                    .append_data(&mut dir_header, "anolisa", &[] as &[u8])
                    .expect("append dir");
                builder.finish().expect("finish");
            }
            enc.finish().expect("gz finish")
        };

        let dir = tempdir().unwrap();
        let archive_path = dir.path().join("test.tar.gz");
        fs::write(&archive_path, &archive_bytes).unwrap();

        let dest = dir.path().join("extracted");
        let err = extract_binary_from_archive(&archive_path, &dest).expect_err("must fail");
        assert!(matches!(err, SelfUpdateError::ExtractFailed { .. }));
        assert!(!dest.exists());
    }

    #[test]
    fn perform_update_cleans_archive_on_extract_failure() {
        let dir = tempdir().unwrap();

        // Serve a valid gzip but with no `anolisa` regular file inside.
        let bad_archive = {
            use flate2::Compression;
            use flate2::write::GzEncoder;

            let mut enc = GzEncoder::new(Vec::new(), Compression::fast());
            {
                let mut builder = tar::Builder::new(&mut enc);
                let mut header = tar::Header::new_gnu();
                header.set_size(5);
                header.set_mode(0o644);
                header.set_cksum();
                builder
                    .append_data(&mut header, "not-anolisa", b"hello" as &[u8])
                    .expect("append");
                builder.finish().expect("finish");
            }
            enc.finish().expect("gz finish")
        };
        let sha = sha256_of(&bad_archive);

        let url = {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            let addr = listener.local_addr().expect("addr");
            let data = bad_archive.clone();
            thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    data.len()
                );
                std::io::Write::write_all(&mut stream, response.as_bytes()).expect("write head");
                std::io::Write::write_all(&mut stream, &data).expect("write body");
            });
            format!("http://{addr}/bad.tar.gz")
        };

        let current = dir.path().join("anolisa");
        fs::write(&current, b"original").unwrap();

        let artifact = ReleaseArtifact {
            os: "linux".into(),
            arch: "x86_64".into(),
            url,
            sha256: sha,
            size: Some(bad_archive.len() as u64),
        };

        let err = perform_update(&artifact, &current, None).expect_err("must fail");
        assert!(matches!(err, SelfUpdateError::ExtractFailed { .. }));
        // Archive temp file must be cleaned up even on extract failure.
        assert!(!archive_path(&current).exists(), "archive.tmp not cleaned");
        // Original binary must be intact.
        assert_eq!(fs::read(&current).unwrap(), b"original");
    }
}
