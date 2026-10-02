//! Trusted peer control socket.
//!
//! Provides a Unix domain socket control plane authenticated either by
//! the legacy executable identity contract or an explicitly configured
//! shared-key challenge-response contract for isolated containers.
//!
//! ## Protocol
//!
//! JSONL over the Unix socket: one JSON object per line.
//!
//! Request:
//! ```json
//! {"schemaVersion":"1","method":"ping"}
//! {"schemaVersion":"1","method":"status"}
//! {"schemaVersion":"1","method":"meta.writeActivation","skillName":"demo-weather","activation":{"schemaVersion":1,"target":null}}
//! {"schemaVersion":"1","method":"meta.setActivationXattr","skillName":"demo-weather","activation":{"schemaVersion":1,"target":null}}
//! {"schemaVersion":"1","method":"skill.resolveLiveSource","canonicalSkillDir":"/canonical/skills/apple/apple-notes"}
//! ```
//!
//! Response:
//! ```json
//! {"schemaVersion":"1","ok":true,"result":{"pong":true}}
//! {"schemaVersion":"1","ok":true,"result":{"status":"ready"}}
//! {"schemaVersion":"1","ok":true,"result":{"outcome":"updated"}}
//! {"schemaVersion":"1","ok":true,"result":{"managed":true,"canonicalSkillDir":"...","skillId":"apple/apple-notes","relativeSkillDir":"apple/apple-notes","liveSkillDir":"...","identity":{"device":42,"inode":1001},"transport":"shared_path"}}
//! {"schemaVersion":"1","ok":true,"result":{"managed":false,"canonicalSkillDir":"...","reason":"not_managed"}}
//! {"schemaVersion":"1","ok":false,"error":{"code":"permission_denied","message":"..."}}
//! ```
//!
//! ## `skill.resolveLiveSource`
//!
//! A read-only query that maps a caller-supplied canonical Skill directory
//! to its physical live/backing source. It is O(path depth): it opens the
//! live Skill directory one path component at a time and never scans the
//! whole Skill root. It has no side effects — no scan, manifest, policy
//! decision, or activation write. See `dispatch_resolve_live_source`.
//!
//! ## Security
//!
//! - Socket parent directory permissions: `0o700` (owner-only).
//! - Socket file permissions: `0o600` (owner-only).
//! - Peer credentials obtained via `SO_PEERCRED` (`getsockopt`).
//! - Peer executable resolved via `/proc/<pid>/exe` readlink + stat.
//! - Peer starttime read from `/proc/<pid>/stat` field 22 for PID
//!   reuse protection.
//! - Credential, executable identity, and starttime must all match.
//! - Failed verification returns an error and closes the connection.
//! - Linux-only; non-Linux targets fail at startup when configured.

use std::io::{BufRead, BufReader, Read as IoRead, Write};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::path::SkillLayout;
use std::sync::atomic::{AtomicBool, Ordering};

use tracing::{debug, info, warn};

use super::activation::{ACTIVATION_XATTR, ActivationRecord};
use super::activation_reload::ReloadOutcome;
use super::active::{ActiveSkillResolver, ActiveTarget};
#[cfg(test)]
use super::auth::authenticate_client;
use super::auth::{
    AuthenticatedSession, CONTROL_CLIENT_DOMAIN, CONTROL_SERVER_DOMAIN, FrameSender, SharedSecret,
    authenticate_server,
};
use super::ledger::validate_skill_name_component;
use super::protocol_events::{ProtocolEvent, ProtocolEventWriter};
use super::skill_dir::{SkillDirError, VerifiedSkillDir, open_verified_skill_dir};
use super::trusted_writer::FileId;

// ─────────────────────────────────────────────────────────────────────────────
// Protocol constants
// ─────────────────────────────────────────────────────────────────────────────

pub const CONTROL_SCHEMA_VERSION: &str = "1";

// ─────────────────────────────────────────────────────────────────────────────
// Configuration
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ControlSocketConfig {
    pub socket_path: PathBuf,
    pub trusted_peer: TrustedPeerConfig,
}

#[derive(Debug, Clone)]
pub struct TrustedPeerConfig {
    pub exe_path: PathBuf,
    pub exe_file_id: FileId,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
}

/// Runtime context for methods that need filesystem access
/// (e.g. `meta.writeActivation`, `meta.setActivationXattr`,
/// `skill.resolveLiveSource`).
///
/// Passed through `ControlSocketServer::new()` and threaded into
/// `handle_connection()` so methods can access the canonical/live roots,
/// active resolver, and protocol event writer. Read-only methods
/// (`ping`, `status`) ignore the context.
///
/// This context keeps the two skill roots explicit:
///
/// * `canonical_root` — the absolute, lexically normalized user-visible
///   Skill identity the external ledger addresses. It does not follow a
///   source-root symlink. Incoming `canonicalSkillDir` paths are checked for
///   lexical containment against this root, and the relative skill id is
///   derived from it.
/// * `source_root` — the live / backing root whose physical content stays
///   accessible after the FUSE over-mount. Write methods `openat` against
///   it, and `skill.resolveLiveSource` opens the live Skill directory
///   under it to report the physical backing path and its identity.
///
/// In the common single-source in-place mount the two roots resolve to
/// the same tree (or a bind-mount alias of it), but they are stored
/// separately so a query never crosses the canonical / FUSE / live
/// boundary implicitly.
#[derive(Clone)]
pub struct ControlSocketContext {
    /// User-visible canonical Skill root used for containment checks and
    /// relative skill-id derivation.
    pub canonical_root: PathBuf,
    /// Live / backing root whose physical content is opened for reads and
    /// activation writes.
    pub source_root: PathBuf,
    /// Skill layout of the mount. `skill.resolveLiveSource` uses it to
    /// enforce the same Flat / Hermes Skill boundaries as the FUSE layer.
    pub layout: SkillLayout,
    pub resolver: Option<Arc<ActiveSkillResolver>>,
    pub protocol_event_writer: Option<Arc<dyn ProtocolEventWriter>>,
}

impl std::fmt::Debug for ControlSocketContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ControlSocketContext")
            .field("canonical_root", &self.canonical_root)
            .field("source_root", &self.source_root)
            .field("layout", &self.layout)
            .field("resolver", &self.resolver.is_some())
            .field(
                "protocol_event_writer",
                &self.protocol_event_writer.is_some(),
            )
            .finish()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Peer identity types
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerCredentials {
    pub pid: u32,
    pub uid: u32,
    pub gid: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerIdentity {
    pub credentials: PeerCredentials,
    pub exe_path: Option<PathBuf>,
    pub exe_file_id: Option<FileId>,
    /// Starttime read *before* exe resolution — used together with
    /// `starttime_after` to bracket the `/proc/<pid>/exe` read and
    /// detect PID reuse during identity collection.
    pub starttime_before: Option<u64>,
    /// Starttime read *after* exe resolution.
    pub starttime_after: Option<u64>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Protocol types
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlRequest {
    pub schema_version: String,
    pub method: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlResponse {
    pub schema_version: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ControlError>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ControlError {
    pub code: String,
    pub message: String,
}

impl ControlResponse {
    pub fn ok(result: serde_json::Value) -> Self {
        Self {
            schema_version: CONTROL_SCHEMA_VERSION.to_string(),
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    pub fn err(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            schema_version: CONTROL_SCHEMA_VERSION.to_string(),
            ok: false,
            result: None,
            error: Some(ControlError {
                code: code.into(),
                message: message.into(),
            }),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Peer credential resolution
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
pub fn get_peer_credentials(stream: &UnixStream) -> std::io::Result<PeerCredentials> {
    use std::os::unix::io::AsRawFd;

    let fd = stream.as_raw_fd();
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;

    let ret = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut libc::ucred as *mut libc::c_void,
            &mut len,
        )
    };

    if ret != 0 {
        return Err(std::io::Error::last_os_error());
    }

    Ok(PeerCredentials {
        pid: cred.pid as u32,
        uid: cred.uid,
        gid: cred.gid,
    })
}

#[cfg(not(target_os = "linux"))]
pub fn get_peer_credentials(_stream: &UnixStream) -> std::io::Result<PeerCredentials> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "SO_PEERCRED is only available on Linux",
    ))
}

/// Resolve peer executable identity from `/proc/<pid>/exe`.
///
/// The display path comes from `readlink` (human-readable), but the
/// file identity `(dev, ino)` is obtained by statting the proc symlink
/// itself (with follow), NOT the resolved path string. This avoids a
/// TOCTOU where the path is replaced between readlink and stat.
#[cfg(target_os = "linux")]
pub fn resolve_peer_exe(pid: u32) -> Option<(PathBuf, FileId)> {
    use std::os::unix::fs::MetadataExt;

    let exe_link = PathBuf::from(format!("/proc/{pid}/exe"));
    let exe_path = std::fs::read_link(&exe_link).ok()?;
    // stat the proc symlink (follows to the running exe inode), not
    // the resolved path string which could race with replacement.
    let meta = std::fs::metadata(&exe_link).ok()?;
    Some((
        exe_path,
        FileId {
            dev: meta.dev(),
            ino: meta.ino(),
        },
    ))
}

#[cfg(not(target_os = "linux"))]
pub fn resolve_peer_exe(_pid: u32) -> Option<(PathBuf, FileId)> {
    None
}

/// Read starttime (field 22) from `/proc/<pid>/stat` for PID reuse defense.
#[cfg(target_os = "linux")]
pub fn resolve_peer_starttime(pid: u32) -> Option<u64> {
    let stat_path = PathBuf::from(format!("/proc/{pid}/stat"));
    super::trusted_writer::read_starttime_from_stat(&stat_path)
}

#[cfg(not(target_os = "linux"))]
pub fn resolve_peer_starttime(_pid: u32) -> Option<u64> {
    None
}

/// Build a full [`PeerIdentity`] from a connected stream.
///
/// Reads starttime *before* and *after* the `/proc/<pid>/exe`
/// resolution to bracket the identity collection window. If the
/// PID is reused between the two reads, the starttimes will differ
/// and `verify_peer` will reject the connection.
pub fn identify_peer(stream: &UnixStream) -> std::io::Result<PeerIdentity> {
    let creds = get_peer_credentials(stream)?;
    let starttime_before = resolve_peer_starttime(creds.pid);
    let (exe_path, exe_file_id) = match resolve_peer_exe(creds.pid) {
        Some((p, fid)) => (Some(p), Some(fid)),
        None => (None, None),
    };
    let starttime_after = resolve_peer_starttime(creds.pid);
    Ok(PeerIdentity {
        credentials: creds,
        exe_path,
        exe_file_id,
        starttime_before,
        starttime_after,
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Peer verification
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerVerifyResult {
    Accepted,
    DeniedUidMismatch { expected: u32, actual: u32 },
    DeniedGidMismatch { expected: u32, actual: u32 },
    DeniedExeUnresolved,
    DeniedExePathMismatch { expected: PathBuf, actual: PathBuf },
    DeniedExeFileIdMismatch { expected: FileId, actual: FileId },
    DeniedStarttimeUnresolved,
    DeniedStarttimeMismatch { pid: u32, pinned: u64, actual: u64 },
}

impl PeerVerifyResult {
    pub fn is_accepted(&self) -> bool {
        matches!(self, Self::Accepted)
    }

    pub fn denial_message(&self) -> Option<String> {
        match self {
            Self::Accepted => None,
            Self::DeniedUidMismatch { expected, actual } => {
                Some(format!("uid mismatch: expected {expected}, got {actual}"))
            }
            Self::DeniedGidMismatch { expected, actual } => {
                Some(format!("gid mismatch: expected {expected}, got {actual}"))
            }
            Self::DeniedExeUnresolved => Some("peer executable could not be resolved".to_string()),
            Self::DeniedExePathMismatch { expected, actual } => Some(format!(
                "exe path mismatch: expected {}, got {}",
                expected.display(),
                actual.display()
            )),
            Self::DeniedExeFileIdMismatch { expected, actual } => Some(format!(
                "exe file id mismatch: expected {expected}, got {actual}"
            )),
            Self::DeniedStarttimeUnresolved => {
                Some("peer starttime could not be resolved".to_string())
            }
            Self::DeniedStarttimeMismatch {
                pid,
                pinned,
                actual,
            } => Some(format!(
                "starttime mismatch for pid {pid}: pinned {pinned}, got {actual}"
            )),
        }
    }
}

pub fn verify_peer(config: &TrustedPeerConfig, identity: &PeerIdentity) -> PeerVerifyResult {
    if let Some(expected_uid) = config.uid {
        if identity.credentials.uid != expected_uid {
            return PeerVerifyResult::DeniedUidMismatch {
                expected: expected_uid,
                actual: identity.credentials.uid,
            };
        }
    }

    if let Some(expected_gid) = config.gid {
        if identity.credentials.gid != expected_gid {
            return PeerVerifyResult::DeniedGidMismatch {
                expected: expected_gid,
                actual: identity.credentials.gid,
            };
        }
    }

    // Starttime bracketing: both before and after exe resolution must
    // be present and equal, otherwise the PID was reused mid-collection.
    #[cfg(target_os = "linux")]
    {
        let before = match identity.starttime_before {
            Some(st) => st,
            None => return PeerVerifyResult::DeniedStarttimeUnresolved,
        };
        let after = match identity.starttime_after {
            Some(st) => st,
            None => return PeerVerifyResult::DeniedStarttimeUnresolved,
        };
        if before != after {
            return PeerVerifyResult::DeniedStarttimeMismatch {
                pid: identity.credentials.pid,
                pinned: before,
                actual: after,
            };
        }
    }

    let actual_path = match identity.exe_path.as_ref() {
        Some(p) => p,
        None => return PeerVerifyResult::DeniedExeUnresolved,
    };
    let actual_fid = match identity.exe_file_id {
        Some(fid) => fid,
        None => return PeerVerifyResult::DeniedExeUnresolved,
    };

    let actual_canon = std::fs::canonicalize(actual_path)
        .ok()
        .unwrap_or_else(|| actual_path.clone());

    if actual_canon != config.exe_path {
        return PeerVerifyResult::DeniedExePathMismatch {
            expected: config.exe_path.clone(),
            actual: actual_canon,
        };
    }

    if actual_fid != config.exe_file_id {
        return PeerVerifyResult::DeniedExeFileIdMismatch {
            expected: config.exe_file_id,
            actual: actual_fid,
        };
    }

    PeerVerifyResult::Accepted
}

// ─────────────────────────────────────────────────────────────────────────────
// Request dispatch
// ─────────────────────────────────────────────────────────────────────────────

pub fn parse_request(line: &str) -> Result<ControlRequest, ControlResponse> {
    let (req, _raw) = parse_request_with_raw(line)?;
    Ok(req)
}

pub fn parse_request_with_raw(
    line: &str,
) -> Result<(ControlRequest, serde_json::Value), ControlResponse> {
    let raw: serde_json::Value = serde_json::from_str(line)
        .map_err(|e| ControlResponse::err("invalid_request", format!("JSON parse error: {e}")))?;
    let req: ControlRequest = serde_json::from_value(raw.clone())
        .map_err(|e| ControlResponse::err("invalid_request", format!("JSON parse error: {e}")))?;
    if req.schema_version != CONTROL_SCHEMA_VERSION {
        return Err(ControlResponse::err(
            "unsupported_schema_version",
            format!(
                "unsupported schemaVersion '{}'; expected '{CONTROL_SCHEMA_VERSION}'",
                req.schema_version
            ),
        ));
    }
    Ok((req, raw))
}

pub fn dispatch_request(
    req: &ControlRequest,
    raw: &serde_json::Value,
    ctx: Option<&ControlSocketContext>,
) -> ControlResponse {
    match req.method.as_str() {
        "ping" => ControlResponse::ok(serde_json::json!({ "pong": true })),
        "status" => ControlResponse::ok(serde_json::json!({ "status": "ready" })),
        "meta.writeActivation" => dispatch_meta_write_activation(raw, ctx),
        "meta.setActivationXattr" => dispatch_meta_set_activation_xattr(raw, ctx),
        "skill.resolveLiveSource" => dispatch_resolve_live_source(raw, ctx),
        other => ControlResponse::err("unknown_method", format!("unknown method '{other}'")),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Meta write: shared validation
// ─────────────────────────────────────────────────────────────────────────────

fn extract_and_validate_meta_request<'a>(
    raw: &'a serde_json::Value,
    ctx: Option<&ControlSocketContext>,
) -> Result<(&'a str, String, PathBuf), ControlResponse> {
    let ctx = ctx.ok_or_else(|| {
        ControlResponse::err(
            "not_configured",
            "meta write methods require a configured source root",
        )
    })?;

    if ctx.resolver.is_none() {
        return Err(ControlResponse::err(
            "not_configured",
            "meta write methods require an active resolver (--security --activation-mode file)",
        ));
    }

    let skill_name = raw
        .get("skillName")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            ControlResponse::err("invalid_request", "missing or non-string 'skillName' field")
        })?;

    validate_skill_id(skill_name, ctx.layout)?;

    let activation_value = raw
        .get("activation")
        .ok_or_else(|| ControlResponse::err("invalid_request", "missing 'activation' field"))?;

    let activation_json = serde_json::to_string(activation_value).map_err(|e| {
        ControlResponse::err(
            "invalid_activation",
            format!("cannot serialize activation: {e}"),
        )
    })?;

    ActivationRecord::from_json_str(&activation_json)
        .map_err(|e| ControlResponse::err("invalid_activation", e.to_string()))?;

    let skill_dir = ctx.source_root.join(skill_name);

    Ok((skill_name, activation_json, skill_dir))
}

/// Validate a relative Skill id without weakening the ledger's
/// single-component name contract.
fn validate_skill_id(skill_id: &str, layout: SkillLayout) -> Result<(), ControlResponse> {
    let components: Vec<&str> = skill_id.split('/').collect();
    let valid_depth = match layout {
        SkillLayout::Flat => components.len() == 1,
        SkillLayout::Hermes => matches!(components.len(), 1 | 2),
    };
    if !valid_depth {
        return Err(ControlResponse::err(
            "invalid_skill_name",
            format!("skill id '{skill_id}' is incompatible with {layout:?} layout"),
        ));
    }

    if components
        .iter()
        .any(|component| component.starts_with('.'))
        || components.first() == Some(&"skill-discover")
    {
        return Err(ControlResponse::err(
            "invalid_skill_name",
            format!("skill id '{skill_id}' refers to a managed/reserved path"),
        ));
    }

    for component in components {
        validate_skill_name_component(component)
            .map_err(|e| ControlResponse::err("invalid_skill_name", e.to_string()))?;
    }
    Ok(())
}

fn reload_and_emit(
    ctx: Option<&ControlSocketContext>,
    skill_name: &str,
    skill_dir: &Path,
    write_kind: &str,
) -> serde_json::Value {
    let mut outcome_label = "no_reload";

    if let Some(ctx) = ctx {
        if let Some(ref resolver) = ctx.resolver {
            let reload_outcome = reload_skill_once_into(resolver, &ctx.source_root, skill_name);
            outcome_label = match &reload_outcome {
                ReloadOutcome::Updated(_) => "updated",
                ReloadOutcome::Unchanged => "unchanged",
                ReloadOutcome::Timeout => "timeout",
                ReloadOutcome::FailSafeHidden { .. } => "fail_safe_hidden",
            };
        }

        if let Some(ref writer) = ctx.protocol_event_writer {
            let event = ProtocolEvent::new(
                skill_dir.to_string_lossy().to_string(),
                skill_name,
                write_kind,
                Vec::new(),
            );
            writer.emit(&event);

            let reload_event = ProtocolEvent::with_reload_outcome(
                skill_dir.to_string_lossy().to_string(),
                skill_name,
                &format!("activation_{outcome_label}"),
            );
            writer.emit(&reload_event);
        }
    }

    serde_json::json!({ "outcome": outcome_label })
}

fn reload_skill_once_into(
    resolver: &ActiveSkillResolver,
    source_root: &Path,
    skill_name: &str,
) -> ReloadOutcome {
    use super::activation::{fail_safe_hidden, load_activation_prefer_xattr};

    let skill_dir = source_root.join(skill_name);
    match load_activation_prefer_xattr(&skill_dir) {
        Ok(target) => {
            let prev = resolver.get(skill_name);
            let changed = match (&prev, &target) {
                (None, _) => true,
                (Some(ActiveTarget::Hidden { .. }), ActiveTarget::Hidden { .. }) => false,
                (
                    Some(ActiveTarget::Snapshot {
                        snapshot_dir: a, ..
                    }),
                    ActiveTarget::Snapshot {
                        snapshot_dir: b, ..
                    },
                ) => a != b,
                (
                    Some(ActiveTarget::Current { source_dir: a }),
                    ActiveTarget::Current { source_dir: b },
                ) => a != b,
                _ => true,
            };
            resolver.set(skill_name.to_string(), target.clone());
            if changed {
                ReloadOutcome::Updated(target)
            } else {
                ReloadOutcome::Unchanged
            }
        }
        Err(e) => {
            let hidden = fail_safe_hidden(&e);
            resolver.set(skill_name.to_string(), hidden);
            ReloadOutcome::FailSafeHidden {
                reason: e.to_string(),
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// meta.writeActivation
// ─────────────────────────────────────────────────────────────────────────────

fn dispatch_meta_write_activation(
    raw: &serde_json::Value,
    ctx: Option<&ControlSocketContext>,
) -> ControlResponse {
    let (skill_name, activation_json, skill_dir) = match extract_and_validate_meta_request(raw, ctx)
    {
        Ok(v) => v,
        Err(resp) => return resp,
    };

    let context = ctx.as_ref().unwrap();
    if let Err(resp) = atomic_write_activation_fd(
        &context.source_root,
        skill_name,
        context.layout,
        activation_json.as_bytes(),
    ) {
        return resp;
    }

    let result = reload_and_emit(
        ctx,
        skill_name,
        &skill_dir,
        "control_plane_write_activation",
    );
    ControlResponse::ok(result)
}

/// Fully fd-anchored atomic activation write.
///
/// Opens `source_root` → each Skill-id component with
/// `openat(O_NOFOLLOW|O_DIRECTORY)` →
/// `mkdirat(.skill-meta)` → `openat(.skill-meta, O_NOFOLLOW|O_DIRECTORY)` →
/// `openat(tmp, O_CREAT|O_EXCL)` → write+fsync → `renameat(tmp, activation.json)` →
/// fsync dir.
///
/// Every path segment is opened with `O_NOFOLLOW` so a symlink at any
/// level (skill dir or `.skill-meta`) causes `ELOOP` rather than
/// following the link outside the source tree.
fn atomic_write_activation_fd(
    source_root: &Path,
    skill_name: &str,
    layout: SkillLayout,
    json_bytes: &[u8],
) -> Result<(), ControlResponse> {
    use std::ffi::CString;
    use std::os::unix::io::FromRawFd;

    let skill_guard = open_skill_dir_for_write(source_root, skill_name, layout)?;
    let skill_fd = skill_guard.as_raw_fd();

    // 3. Ensure .skill-meta exists via mkdirat. EEXIST is fine.
    let c_meta = CString::new(".skill-meta").unwrap();
    let rc = unsafe { libc::mkdirat(skill_fd, c_meta.as_ptr(), 0o700) };
    if rc != 0 {
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() != Some(libc::EEXIST) {
            return Err(ControlResponse::err(
                "write_failed",
                format!("failed to create .skill-meta: {e}"),
            ));
        }
    }

    // 4. Open .skill-meta relative to skill_fd with O_NOFOLLOW.
    let meta_fd = unsafe {
        libc::openat(
            skill_fd,
            c_meta.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if meta_fd < 0 {
        let e = std::io::Error::last_os_error();
        return Err(ControlResponse::err(
            "write_failed",
            format!("failed to open .skill-meta (O_NOFOLLOW): {e}"),
        ));
    }
    let meta_dir_file = unsafe { std::fs::File::from_raw_fd(meta_fd) };
    let rc = unsafe { libc::fchmod(meta_fd, 0o700) };
    if rc != 0 {
        let e = std::io::Error::last_os_error();
        return Err(ControlResponse::err(
            "write_failed",
            format!("failed to secure .skill-meta permissions: {e}"),
        ));
    }

    // 5. Create temp file via openat on meta_fd.
    let tmp_name = format!(
        "activation.tmp.{}.{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let c_tmp = CString::new(tmp_name.as_bytes())
        .map_err(|_| ControlResponse::err("write_failed", "temp name contains NUL"))?;
    let c_target = CString::new("activation.json").unwrap();

    let tmp_fd = unsafe {
        libc::openat(
            meta_fd,
            c_tmp.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC,
            0o600,
        )
    };
    if tmp_fd < 0 {
        let e = std::io::Error::last_os_error();
        return Err(ControlResponse::err(
            "write_failed",
            format!("failed to create temp file: {e}"),
        ));
    }

    // 6. Write, fsync, close temp file.
    let write_result = {
        let mut f = unsafe { std::fs::File::from_raw_fd(tmp_fd) };
        f.write_all(json_bytes)
            .and_then(|()| f.sync_all())
            .map_err(|e| format!("{e}"))
    };
    if let Err(msg) = write_result {
        unsafe { libc::unlinkat(meta_fd, c_tmp.as_ptr(), 0) };
        return Err(ControlResponse::err(
            "write_failed",
            format!("failed to write/fsync temp file: {msg}"),
        ));
    }

    // 7. Atomic rename via renameat on meta_fd.
    let rc = unsafe { libc::renameat(meta_fd, c_tmp.as_ptr(), meta_fd, c_target.as_ptr()) };
    if rc != 0 {
        let e = std::io::Error::last_os_error();
        unsafe { libc::unlinkat(meta_fd, c_tmp.as_ptr(), 0) };
        return Err(ControlResponse::err(
            "write_failed",
            format!("failed to rename temp to activation.json: {e}"),
        ));
    }

    // 8. Best-effort fsync the directory.
    let _ = meta_dir_file.sync_all();

    Ok(())
}

/// Resolve the write target through the shared fd-anchored Skill boundary.
fn open_skill_dir_for_write(
    source_root: &Path,
    skill_name: &str,
    layout: SkillLayout,
) -> Result<VerifiedSkillDir, ControlResponse> {
    let components: Vec<_> = skill_name.split('/').collect();
    open_verified_skill_dir(source_root, layout, &components)
        .map_err(|error| map_skill_dir_error_for_write(error, skill_name, layout))
}

fn map_skill_dir_error_for_write(
    error: SkillDirError,
    skill_name: &str,
    layout: SkillLayout,
) -> ControlResponse {
    match error {
        SkillDirError::InvalidDepth => ControlResponse::err(
            "invalid_skill_name",
            format!("skill id '{skill_name}' is incompatible with {layout:?} layout"),
        ),
        SkillDirError::RootPathContainsNul => {
            ControlResponse::err("write_failed", "source root path contains NUL")
        }
        SkillDirError::RootOpen(error) => ControlResponse::err(
            "write_failed",
            format!("failed to open source root: {error}"),
        ),
        SkillDirError::ComponentContainsNul => {
            ControlResponse::err("write_failed", "skill name contains NUL")
        }
        SkillDirError::ComponentSymlink { component } => ControlResponse::err(
            "invalid_skill_name",
            format!(
                "skill path component '{component}' in '{skill_name}' is a symlink; refusing to follow"
            ),
        ),
        SkillDirError::ComponentMissing { component } => ControlResponse::err(
            "skill_not_found",
            format!("skill path component '{component}' in '{skill_name}' does not exist"),
        ),
        SkillDirError::ComponentNotDirectory { component } => ControlResponse::err(
            "skill_not_found",
            format!("skill path component '{component}' in '{skill_name}' is not a directory"),
        ),
        SkillDirError::ComponentOpen { component, source } => ControlResponse::err(
            "write_failed",
            format!(
                "failed to open skill path component '{component}' in '{skill_name}': {source}"
            ),
        ),
        SkillDirError::NestedBelowTopLevelSkill { parent, child } => ControlResponse::err(
            "invalid_skill_layout",
            format!(
                "'{parent}' is a top-level skill, not a category; '{child}' is a subdirectory, not a nested Skill"
            ),
        ),
        SkillDirError::MarkerInspect(error) => ControlResponse::err(
            "write_failed",
            format!("failed to inspect SKILL.md: {error}"),
        ),
        SkillDirError::MissingMarker => ControlResponse::err(
            "invalid_skill_layout",
            format!("'{skill_name}' has no regular SKILL.md and is not a Skill"),
        ),
        SkillDirError::Identity(error) => ControlResponse::err(
            "write_failed",
            format!("failed to stat skill directory: {error}"),
        ),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// meta.setActivationXattr
// ─────────────────────────────────────────────────────────────────────────────

fn dispatch_meta_set_activation_xattr(
    raw: &serde_json::Value,
    ctx: Option<&ControlSocketContext>,
) -> ControlResponse {
    let (skill_name, activation_json, skill_dir) = match extract_and_validate_meta_request(raw, ctx)
    {
        Ok(v) => v,
        Err(resp) => return resp,
    };

    let context = ctx.as_ref().unwrap();
    if let Err(resp) = set_activation_xattr_fd(
        &context.source_root,
        skill_name,
        context.layout,
        &activation_json,
    ) {
        return resp;
    }

    let result = reload_and_emit(ctx, skill_name, &skill_dir, "control_plane_write_xattr");
    ControlResponse::ok(result)
}

/// Fd-anchored xattr write: open source_root → each Skill-id component
/// with `openat(O_NOFOLLOW|O_DIRECTORY)` → fsetxattr on the verified fd.
fn set_activation_xattr_fd(
    source_root: &Path,
    skill_name: &str,
    layout: SkillLayout,
    json_str: &str,
) -> Result<(), ControlResponse> {
    use std::ffi::CString;

    let skill_guard = open_skill_dir_for_write(source_root, skill_name, layout)?;
    let skill_fd = skill_guard.as_raw_fd();

    let c_name = CString::new(ACTIVATION_XATTR)
        .map_err(|_| ControlResponse::err("write_failed", "xattr name contains NUL"))?;

    let rc = unsafe {
        libc::fsetxattr(
            skill_fd,
            c_name.as_ptr(),
            json_str.as_ptr() as *const libc::c_void,
            json_str.len(),
            0,
        )
    };

    if rc != 0 {
        let err = std::io::Error::last_os_error();
        let errno = err.raw_os_error().unwrap_or(0);
        if errno == libc::ENOTSUP || errno == libc::EOPNOTSUPP {
            return Err(ControlResponse::err(
                "xattr_not_supported",
                format!("filesystem does not support user xattrs on '{skill_name}'"),
            ));
        }
        return Err(ControlResponse::err(
            "write_failed",
            format!("fsetxattr failed: {err}"),
        ));
    }

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// skill.resolveLiveSource (read-only)
// ─────────────────────────────────────────────────────────────────────────────

/// Dispatch `skill.resolveLiveSource`.
///
/// A read-only query mapping a caller-supplied canonical Skill directory to
/// its physical live/backing source. The path/layout resolution and
/// response construction live in [`super::resolver`]; this dispatcher only
/// validates the request envelope, forwards the canonical/live roots and
/// layout from the context, and maps the resolver outcome onto a control
/// response. Three distinct outcomes:
///
/// * `managed = true` — the path resolves to a valid live Skill directory
///   under the managed canonical root.
/// * `managed = false` — a valid absolute path outside the managed root; a
///   normal success the caller may fall back on.
/// * structured error — never disguised as `managed = false`.
fn dispatch_resolve_live_source(
    raw: &serde_json::Value,
    ctx: Option<&ControlSocketContext>,
) -> ControlResponse {
    let ctx = match ctx {
        Some(c) => c,
        None => {
            return ControlResponse::err(
                "not_configured",
                "skill.resolveLiveSource requires a configured canonical and live root",
            );
        }
    };

    let canonical_skill_dir = match raw.get("canonicalSkillDir").and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => s,
        _ => {
            return ControlResponse::err(
                "invalid_request",
                "missing or non-string 'canonicalSkillDir' field",
            );
        }
    };

    match super::resolver::resolve_live_source(
        &ctx.canonical_root,
        &ctx.source_root,
        ctx.layout,
        canonical_skill_dir,
    ) {
        Ok(result) => ControlResponse::ok(result),
        Err(e) => ControlResponse::err(e.code, e.message),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Socket path preflight
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum SocketPreflightError {
    ParentDoesNotExist(PathBuf),
    /// The existing path is a symlink, regular file, directory, or other
    /// non-socket object. It is never deleted.
    ExistingPathNotSocket(PathBuf),
    /// The existing socket is owned by a different uid. It is never
    /// deleted.
    ExistingSocketWrongOwner {
        path: PathBuf,
        uid: u32,
    },
    /// A live listener is accepting connections on the existing socket.
    /// A second instance must not unlink an active endpoint.
    ActiveListener(PathBuf),
    /// The liveness probe could not prove the socket is stale (e.g.
    /// `EACCES`, `EINTR`, resource exhaustion). Only a definitive
    /// `ECONNREFUSED` is treated as stale; everything else fails closed.
    ProbeInconclusive(PathBuf, std::io::Error),
    UnlinkFailed(PathBuf, std::io::Error),
    Stat(PathBuf, std::io::Error),
}

impl std::fmt::Display for SocketPreflightError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ParentDoesNotExist(p) => {
                write!(f, "socket parent directory does not exist: {}", p.display())
            }
            Self::ExistingPathNotSocket(p) => write!(
                f,
                "path '{}' exists but is not a socket; refusing to overwrite",
                p.display()
            ),
            Self::ExistingSocketWrongOwner { path, uid } => write!(
                f,
                "socket '{}' is owned by uid {uid}; refusing to unlink a socket \
                 we do not own",
                path.display()
            ),
            Self::ActiveListener(p) => write!(
                f,
                "socket '{}' has an active listener; another instance owns this \
                 endpoint",
                p.display()
            ),
            Self::ProbeInconclusive(p, e) => write!(
                f,
                "cannot determine liveness of socket '{}' ({e}); refusing to \
                 unlink",
                p.display()
            ),
            Self::UnlinkFailed(p, e) => {
                write!(f, "failed to unlink stale socket '{}': {e}", p.display())
            }
            Self::Stat(p, e) => write!(f, "failed to stat '{}': {e}", p.display()),
        }
    }
}

impl std::error::Error for SocketPreflightError {}

/// Preflight the socket path, safely reclaiming only a confirmed-stale
/// socket that we own.
///
/// Called while the lifecycle lock is held, so no other lock-aware
/// instance can be serving this endpoint. The checks are still defensive:
///
/// * A missing path is fine — the caller will bind.
/// * A non-socket object (symlink, regular file, directory) is never
///   deleted; startup fails closed.
/// * A socket owned by a different uid is never deleted.
/// * A socket is reclaimed only when a non-blocking connect probe returns
///   a definitive `ECONNREFUSED` (no listener). A successful connect means
///   a live listener (a non-lock-aware instance may still be serving it),
///   and any other probe error (`EACCES`, `EINTR`, resource exhaustion, …)
///   is inconclusive — both fail closed rather than unlink.
pub fn preflight_socket_path(path: &Path) -> Result<(), SocketPreflightError> {
    use std::os::unix::fs::MetadataExt;

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            return Err(SocketPreflightError::ParentDoesNotExist(
                parent.to_path_buf(),
            ));
        }
    }

    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(SocketPreflightError::Stat(path.to_path_buf(), e)),
    };

    // `symlink_metadata` does not follow links, so a symlink reports its
    // own type here. Classify the object; only an owned socket is a
    // reclamation candidate.
    let our_uid = unsafe { libc::geteuid() };
    match classify_socket_object(meta.file_type().is_socket(), meta.uid(), our_uid) {
        SocketObjectClass::NotSocket => {
            return Err(SocketPreflightError::ExistingPathNotSocket(
                path.to_path_buf(),
            ));
        }
        SocketObjectClass::WrongOwner(uid) => {
            return Err(SocketPreflightError::ExistingSocketWrongOwner {
                path: path.to_path_buf(),
                uid,
            });
        }
        SocketObjectClass::OwnedSocket => {}
    }

    // Probe liveness while holding the lifecycle lock. Only a definitive
    // ECONNREFUSED proves the socket is stale and safe to reclaim; a live
    // listener or any inconclusive error fails closed. The probe retries a
    // bounded number of times so a socket whose previous owner's listener is
    // still finishing teardown (transiently EAGAIN/EINPROGRESS) settles to
    // ECONNREFUSED rather than being misread as a live listener — which
    // otherwise makes a fast restart fail to reclaim its own endpoint.
    match probe_socket_liveness(path) {
        SocketLiveness::Stale => {
            std::fs::remove_file(path)
                .map_err(|e| SocketPreflightError::UnlinkFailed(path.to_path_buf(), e))?;
            Ok(())
        }
        SocketLiveness::Live => Err(SocketPreflightError::ActiveListener(path.to_path_buf())),
        SocketLiveness::Inconclusive(e) => Err(SocketPreflightError::ProbeInconclusive(
            path.to_path_buf(),
            e,
        )),
    }
}

/// Result of probing whether a socket path has a live listener.
enum SocketLiveness {
    /// A listener accepted, or was persistently unconnectable-but-present
    /// (backlog full) across every retry — the endpoint is live and must
    /// never be reclaimed.
    Live,
    /// The kernel returned `ECONNREFUSED` — no listener, the socket file is
    /// stale and safe to reclaim.
    Stale,
    /// The probe never reached a definitive result; liveness is unknown.
    Inconclusive(std::io::Error),
}

/// Outcome of a single non-blocking `connect(2)` attempt.
enum ProbeOnce {
    /// `connect` returned 0: a listener accepted — definitively live.
    Connected,
    /// `ECONNREFUSED`: no listener — definitively stale.
    Refused,
    /// A non-definitive result (`EAGAIN`/`EINPROGRESS` — backlog full or
    /// pending — or a resource/other error). Ambiguous on a single attempt;
    /// the caller retries before drawing a conclusion.
    Ambiguous(std::io::Error),
}

/// Non-blocking `connect(2)` probe against an `AF_UNIX` socket path. Bounded
/// (never blocks) and precise: only a definitive `ECONNREFUSED` is treated
/// as stale.
///
/// A single connect is ambiguous while a previous owner's listener is
/// mid-teardown: for a short window after the listener fd is closed, a stale
/// socket transiently reports `EAGAIN`/`EINPROGRESS` before the kernel
/// settles the endpoint to `ECONNREFUSED`. Classifying that transient state
/// as a live listener would refuse to reclaim a genuinely stale socket and
/// make a fast restart fail. The lifecycle flock guarantees mutual exclusion
/// but not that teardown has completed, so the probe retries on any
/// non-definitive result:
///
/// * a genuinely live listener connects (or is persistently backlog-full)
///   across every retry, so it is reported `Live` and never unlinked;
/// * a stale socket settles to `ECONNREFUSED` within the retry window and is
///   reported `Stale`.
///
/// The common cases (immediate connect or immediate refusal) return on the
/// first attempt with no delay; the retry budget is capped so the probe
/// always terminates quickly.
fn probe_socket_liveness(path: &Path) -> SocketLiveness {
    const MAX_ATTEMPTS: u32 = 40;
    const RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(5);

    resolve_socket_liveness(
        MAX_ATTEMPTS,
        || probe_socket_liveness_once(path),
        || std::thread::sleep(RETRY_DELAY),
    )
}

/// Retry state machine over single-probe outcomes, factored out of the
/// `connect(2)` syscall so it can be covered deterministically with injected
/// outcomes (the real socket teardown almost always yields `ECONNREFUSED`
/// immediately, so a live-socket test cannot reliably reach the retry path).
///
/// Returns `Stale` on the first `Refused` and `Live` on the first
/// `Connected`; otherwise retries up to `max_attempts` on `Ambiguous`,
/// invoking `wait` between attempts (a real sleep in production, a no-op in
/// tests). If no definitive result ever appears, a persistent
/// backlog-full/pending signal (`EAGAIN`/`EINPROGRESS`) is reported `Live`
/// (never unlinked) and any other persistent error is `Inconclusive` (fail
/// closed).
fn resolve_socket_liveness(
    max_attempts: u32,
    mut probe: impl FnMut() -> ProbeOnce,
    mut wait: impl FnMut(),
) -> SocketLiveness {
    let mut last_err: Option<std::io::Error> = None;
    for attempt in 0..max_attempts {
        match probe() {
            ProbeOnce::Connected => return SocketLiveness::Live,
            ProbeOnce::Refused => return SocketLiveness::Stale,
            ProbeOnce::Ambiguous(e) => last_err = Some(e),
        }
        if attempt + 1 < max_attempts {
            wait();
        }
    }

    // Never observed a definitive result. A persistently backlog-full live
    // listener lands here (EAGAIN/EINPROGRESS every attempt): keep it Live so
    // it is never unlinked. Any other persistent error is inconclusive and
    // fails closed.
    match last_err {
        Some(e) => match e.raw_os_error() {
            Some(libc::EAGAIN) | Some(libc::EINPROGRESS) => SocketLiveness::Live,
            _ => SocketLiveness::Inconclusive(e),
        },
        None => SocketLiveness::Inconclusive(std::io::Error::other("probe produced no result")),
    }
}

/// One non-blocking `connect(2)` attempt; see [`probe_socket_liveness`].
fn probe_socket_liveness_once(path: &Path) -> ProbeOnce {
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;

    let bytes = path.as_os_str().as_bytes();
    // sun_path must be NUL-terminated and fit the sockaddr_un buffer.
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    if bytes.len() >= std::mem::size_of_val(&addr.sun_path) {
        return ProbeOnce::Ambiguous(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "socket path too long for sockaddr_un",
        ));
    }
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (i, b) in bytes.iter().enumerate() {
        addr.sun_path[i] = *b as libc::c_char;
    }

    let fd = unsafe {
        libc::socket(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            0,
        )
    };
    if fd < 0 {
        return ProbeOnce::Ambiguous(std::io::Error::last_os_error());
    }
    // SAFETY: `socket` returned a new fd and ownership is transferred once.
    let _guard = unsafe { OwnedFd::from_raw_fd(fd) };

    let rc = unsafe {
        libc::connect(
            fd,
            &addr as *const libc::sockaddr_un as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t,
        )
    };
    if rc == 0 {
        return ProbeOnce::Connected;
    }
    let err = std::io::Error::last_os_error();
    match err.raw_os_error() {
        // No listener bound to the path: stale.
        Some(libc::ECONNREFUSED) => ProbeOnce::Refused,
        // Backlog full / connection pending / resource or other error:
        // ambiguous on a single attempt — let the caller retry.
        _ => ProbeOnce::Ambiguous(err),
    }
}

/// Classification of an existing object at the socket path, excluding the
/// live-listener probe (which needs a connect attempt).
#[derive(Debug, PartialEq, Eq)]
enum SocketObjectClass {
    /// Not a socket (symlink, regular file, directory, …): never delete.
    NotSocket,
    /// A socket owned by another uid: never delete.
    WrongOwner(u32),
    /// A socket owned by the current uid: a stale-reclamation candidate.
    OwnedSocket,
}

/// Pure classification used by [`preflight_socket_path`]. Kept separate so
/// the wrong-owner and non-socket branches are unit-testable without a
/// second uid.
fn classify_socket_object(is_socket: bool, owner_uid: u32, our_uid: u32) -> SocketObjectClass {
    if !is_socket {
        SocketObjectClass::NotSocket
    } else if owner_uid != our_uid {
        SocketObjectClass::WrongOwner(owner_uid)
    } else {
        SocketObjectClass::OwnedSocket
    }
}

/// Ensure the socket parent directory exists, is a directory (not a
/// symlink), is owned by the current uid, and has permissions `0o700`.
///
/// - If the parent does not exist, creates it (and ancestors) and sets
///   permissions to `0o700`.
/// - If the parent exists, verifies it is a directory, owned by the
///   current euid, and tightens permissions to `0o700`.
/// - Fails closed if the parent is not a directory, is owned by another
///   uid, or permissions cannot be set.
pub fn secure_socket_parent(socket_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let parent = match socket_path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => return Ok(()),
    };

    if !parent.exists() {
        std::fs::create_dir_all(parent)?;
        let perms = std::fs::Permissions::from_mode(0o700);
        std::fs::set_permissions(parent, perms)?;
        return Ok(());
    }

    let meta = std::fs::symlink_metadata(parent)?;
    if !meta.file_type().is_dir() {
        return Err(format!(
            "socket parent '{}' exists but is not a directory",
            parent.display()
        )
        .into());
    }

    let our_uid = unsafe { libc::geteuid() };
    if meta.uid() != our_uid {
        return Err(format!(
            "socket parent '{}' is owned by uid {}; expected the current uid {our_uid}",
            parent.display(),
            meta.uid()
        )
        .into());
    }

    let perms = std::fs::Permissions::from_mode(0o700);
    std::fs::set_permissions(parent, perms)?;

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Default endpoint
// ─────────────────────────────────────────────────────────────────────────────

/// Error resolving the default control socket endpoint.
#[derive(Debug)]
pub enum DefaultEndpointError {
    /// The per-user runtime directory `/run/user/<uid>` does not exist.
    RuntimeDirMissing(PathBuf),
    /// `/run/user/<uid>` exists but is not a directory.
    RuntimeDirNotDir(PathBuf),
}

impl std::fmt::Display for DefaultEndpointError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RuntimeDirMissing(p) => write!(
                f,
                "default control socket endpoint unavailable: '{}' does not exist; \
                 pass --control-socket <PATH> explicitly (the default never falls \
                 back to /tmp or /var/tmp)",
                p.display()
            ),
            Self::RuntimeDirNotDir(p) => write!(
                f,
                "default control socket endpoint unavailable: '{}' is not a directory; \
                 pass --control-socket <PATH> explicitly",
                p.display()
            ),
        }
    }
}

impl std::error::Error for DefaultEndpointError {}

/// The default control socket endpoint path for `uid`:
/// `/run/user/<uid>/skillfs/control.sock`.
///
/// Pure: does not touch the filesystem. Deliberately anchored on the
/// per-user runtime directory and never on `/tmp` or `/var/tmp`.
pub fn default_control_socket_path(uid: u32) -> PathBuf {
    PathBuf::from(format!("/run/user/{uid}/skillfs/control.sock"))
}

/// Resolution of the effective control socket endpoint from the merged
/// explicit path (CLI value already merged over the config value) and
/// whether a trusted peer is configured.
///
/// Priority: an explicit path always wins; otherwise a trusted peer
/// selects the default per-user endpoint; otherwise the control plane is
/// disabled. An explicit path without a trusted peer is a configuration
/// error — the control plane is always authenticated.
#[derive(Debug, PartialEq, Eq)]
pub enum EndpointResolution {
    /// Neither an explicit path nor a trusted peer: control plane off.
    Disabled,
    /// Explicit path with a trusted peer: use the path verbatim.
    Explicit(PathBuf),
    /// Trusted peer with no explicit path: bind the default endpoint.
    UseDefault,
    /// Explicit path without a trusted peer: configuration error.
    MissingTrustedPeer(PathBuf),
}

/// Classify the control socket endpoint from the merged explicit path and
/// whether a trusted peer is configured. Pure: performs no filesystem
/// access.
pub fn classify_control_socket_endpoint(
    explicit_path: Option<&Path>,
    has_trusted_peer: bool,
) -> EndpointResolution {
    match (explicit_path, has_trusted_peer) {
        (Some(p), true) => EndpointResolution::Explicit(p.to_path_buf()),
        (Some(p), false) => EndpointResolution::MissingTrustedPeer(p.to_path_buf()),
        (None, true) => EndpointResolution::UseDefault,
        (None, false) => EndpointResolution::Disabled,
    }
}

/// Resolve the default control socket endpoint for the current user,
/// validating that the per-user runtime directory `/run/user/<uid>` exists
/// and is a directory.
///
/// The `skillfs/` leaf under it is created (0700) at bind time by
/// [`secure_socket_parent`]; this function refuses to invent a
/// `/run/user/<uid>` that the system did not provide, and never falls back
/// to a public temporary directory.
pub fn resolve_default_control_socket_endpoint() -> Result<PathBuf, DefaultEndpointError> {
    let uid = unsafe { libc::geteuid() };
    let runtime_dir = PathBuf::from(format!("/run/user/{uid}"));
    match std::fs::metadata(&runtime_dir) {
        Ok(meta) if meta.is_dir() => Ok(default_control_socket_path(uid)),
        Ok(_) => Err(DefaultEndpointError::RuntimeDirNotDir(runtime_dir)),
        Err(_) => Err(DefaultEndpointError::RuntimeDirMissing(runtime_dir)),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Lifecycle lock
// ─────────────────────────────────────────────────────────────────────────────

/// Path of the lifecycle lock file guarding a socket endpoint:
/// the socket path with a `.lock` suffix appended.
fn lifecycle_lock_path(socket_path: &Path) -> PathBuf {
    let mut os = socket_path.as_os_str().to_owned();
    os.push(".lock");
    PathBuf::from(os)
}

/// A non-blocking advisory lock (`flock(LOCK_EX | LOCK_NB)`) guarding a
/// single control socket endpoint for the lifetime of the server.
///
/// The kernel releases the lock automatically when the process exits, so a
/// crashed instance never wedges the endpoint. The lock file itself is
/// intentionally left in place on clean shutdown to avoid a delete/reopen
/// race between a departing and an arriving instance.
struct LifecycleLock {
    // Held for RAII: dropping closes the fd and releases the flock.
    _file: std::fs::File,
}

/// Acquire the lifecycle lock for `socket_path` without blocking.
///
/// Returns an error if another instance already holds the lock (the
/// endpoint is live) or the lock file cannot be created.
fn acquire_lifecycle_lock(socket_path: &Path) -> Result<LifecycleLock, Box<dyn std::error::Error>> {
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::io::AsRawFd;

    let lock_path = lifecycle_lock_path(socket_path);
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&lock_path)
        .map_err(|e| {
            format!(
                "failed to open lifecycle lock '{}': {e}",
                lock_path.display()
            )
        })?;

    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() == Some(libc::EWOULDBLOCK) {
            return Err(format!(
                "another skillfs instance already owns the control socket endpoint '{}'",
                socket_path.display()
            )
            .into());
        }
        return Err(format!(
            "failed to acquire lifecycle lock '{}': {e}",
            lock_path.display()
        )
        .into());
    }

    Ok(LifecycleLock { _file: file })
}

/// Read the `(dev, ino)` identity of the object at `path` without
/// following symlinks. Returns `None` if it does not exist or is not a
/// socket.
fn socket_identity(path: &Path) -> Option<FileId> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.file_type().is_socket() {
        return None;
    }
    Some(FileId {
        dev: meta.dev(),
        ino: meta.ino(),
    })
}

fn remove_socket_if_identity_matches(path: &Path, identity: Option<FileId>) {
    if identity.is_some() && identity == socket_identity(path) {
        let _ = std::fs::remove_file(path);
    }
}

/// Removes a newly bound socket if startup exits before ownership transfers
/// to [`ControlSocketHandle`]. Identity matching protects replacements.
struct BoundSocketGuard {
    socket_path: PathBuf,
    bound_identity: Option<FileId>,
    armed: bool,
}

impl BoundSocketGuard {
    fn new(socket_path: PathBuf) -> Self {
        let bound_identity = socket_identity(&socket_path);
        Self {
            socket_path,
            bound_identity,
            armed: true,
        }
    }

    fn disarm(mut self) -> Option<FileId> {
        self.armed = false;
        self.bound_identity
    }
}

impl Drop for BoundSocketGuard {
    fn drop(&mut self) {
        if self.armed {
            remove_socket_if_identity_matches(&self.socket_path, self.bound_identity);
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Server
// ─────────────────────────────────────────────────────────────────────────────

pub struct ControlSocketServer {
    socket_path: PathBuf,
    authentication: ControlSocketAuthentication,
    context: Option<Arc<ControlSocketContext>>,
    shutdown: Arc<AtomicBool>,
}

#[derive(Clone)]
enum ControlSocketAuthentication {
    Executable(TrustedPeerConfig),
    Hmac {
        secret: SharedSecret,
        uid: Option<u32>,
        gid: Option<u32>,
    },
}

/// Handle returned to the caller for shutdown coordination.
pub struct ControlSocketHandle {
    socket_path: PathBuf,
    shutdown: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    /// `(dev, ino)` of the socket this instance bound. On shutdown the
    /// path is unlinked only if it still resolves to this identity, so a
    /// path replaced by another object after bind is never deleted.
    bound_identity: Option<FileId>,
    /// Held for the server's lifetime; released on shutdown/drop.
    _lifecycle_lock: LifecycleLock,
}

impl ControlSocketHandle {
    pub fn shutdown(mut self) {
        self.stop_and_cleanup();
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Stop the accept loop, join the thread, and remove the socket file
    /// only if it still resolves to the identity this instance bound.
    fn stop_and_cleanup(&mut self) {
        // Signal the non-blocking accept loop to exit. It polls the flag,
        // so shutdown is bounded and does not depend on connecting to the
        // socket path (which may have been replaced).
        self.shutdown.store(true, Ordering::SeqCst);

        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }

        // Only remove the socket if it is still the exact object we bound.
        // If the path was replaced by another socket or object, leave it.
        remove_socket_if_identity_matches(&self.socket_path, self.bound_identity);
    }
}

impl Drop for ControlSocketHandle {
    fn drop(&mut self) {
        // `shutdown()` consumes self and would have taken the thread; if
        // the handle is dropped without an explicit shutdown, clean up.
        if self.thread.is_some() {
            self.stop_and_cleanup();
        }
    }
}

impl ControlSocketServer {
    pub fn new(config: ControlSocketConfig) -> Self {
        Self {
            socket_path: config.socket_path,
            authentication: ControlSocketAuthentication::Executable(config.trusted_peer),
            context: None,
            shutdown: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Builds a server using shared-key authentication without consulting
    /// the peer executable across process or mount namespaces.
    pub fn new_hmac(
        socket_path: PathBuf,
        key_file: &Path,
        uid: Option<u32>,
        gid: Option<u32>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let secret = SharedSecret::load(key_file)?;
        Ok(Self {
            socket_path,
            authentication: ControlSocketAuthentication::Hmac { secret, uid, gid },
            context: None,
            shutdown: Arc::new(AtomicBool::new(false)),
        })
    }

    pub fn with_context(mut self, context: ControlSocketContext) -> Self {
        self.context = Some(Arc::new(context));
        self
    }

    /// Start the server on a dedicated thread. Returns a handle for
    /// shutdown coordination.
    pub fn start(self) -> Result<ControlSocketHandle, Box<dyn std::error::Error>> {
        self.start_with_listener_setup(|listener| listener.set_nonblocking(true))
    }

    fn start_with_listener_setup<F>(
        self,
        setup_listener: F,
    ) -> Result<ControlSocketHandle, Box<dyn std::error::Error>>
    where
        F: FnOnce(&UnixListener) -> std::io::Result<()>,
    {
        // Secure socket parent directory to 0o700 before bind to
        // eliminate the bind-to-chmod permission window. Runs before
        // the lifecycle lock so that create_dir_all provides the parent
        // the lock file lives in.
        secure_socket_parent(&self.socket_path)?;

        // Acquire the non-blocking lifecycle lock before touching the
        // socket path. A second instance targeting the same endpoint
        // fails here instead of unlinking a live socket.
        let lifecycle_lock = acquire_lifecycle_lock(&self.socket_path)?;

        // Reclaim only a confirmed-stale, owned socket (checked while the
        // lock is held). Fails closed on non-sockets, wrong owner, or a
        // live listener.
        preflight_socket_path(&self.socket_path)?;

        let listener = UnixListener::bind(&self.socket_path)?;
        let bound_socket_guard = BoundSocketGuard::new(self.socket_path.clone());
        setup_listener(&listener)?;

        // Set socket file permissions to 0o600.
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o600);
            std::fs::set_permissions(&self.socket_path, perms)?;
        }

        let shutdown = self.shutdown.clone();
        let authentication = self.authentication.clone();
        let context = self.context.clone();
        let socket_path = self.socket_path.clone();

        match &authentication {
            ControlSocketAuthentication::Executable(peer) => info!(
                socket = %socket_path.display(),
                trusted_peer_exe = %peer.exe_path.display(),
                trusted_peer_file_id = %peer.exe_file_id,
                auth_mode = "executable",
                "control socket server starting"
            ),
            ControlSocketAuthentication::Hmac { .. } => info!(
                socket = %socket_path.display(),
                auth_mode = "hmac-sha256",
                "control socket server starting"
            ),
        }

        let shutdown_for_thread = shutdown.clone();
        let thread = std::thread::Builder::new()
            .name("skillfs-control-socket".to_string())
            .spawn(move || {
                run_server_loop(
                    &listener,
                    &authentication,
                    context.as_deref(),
                    &shutdown_for_thread,
                );
            })?;

        // Ownership transfers to the handle only after every fallible
        // startup step has completed.
        let bound_identity = bound_socket_guard.disarm();

        Ok(ControlSocketHandle {
            socket_path,
            shutdown,
            thread: Some(thread),
            bound_identity,
            _lifecycle_lock: lifecycle_lock,
        })
    }
}

/// Poll interval for the non-blocking accept loop. Bounds shutdown
/// latency without a per-request thread or a self-pipe.
const ACCEPT_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

fn run_server_loop(
    listener: &UnixListener,
    authentication: &ControlSocketAuthentication,
    ctx: Option<&ControlSocketContext>,
    shutdown: &AtomicBool,
) {
    // Non-blocking accept + shutdown-flag poll. This makes shutdown
    // reliable and bounded even if the socket path is later replaced by
    // another object (connecting to the path to unblock a blocking
    // accept would not work in that case). Connections are still handled
    // one at a time on this single thread — no thread-per-request.
    while !shutdown.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _addr)) => {
                if shutdown.load(Ordering::SeqCst) {
                    break;
                }
                handle_connection(stream, authentication, ctx);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(ACCEPT_POLL_INTERVAL);
            }
            Err(e) => {
                if shutdown.load(Ordering::SeqCst) {
                    break;
                }
                warn!("control socket accept error: {e}");
                std::thread::sleep(ACCEPT_POLL_INTERVAL);
            }
        }
    }

    debug!("control socket server loop exited");
}

/// Per-connection read timeout. The server processes exactly one
/// request per connection, so this bounds the total hold time.
const CONNECTION_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Maximum request body size (bytes) accepted from a peer.
const MAX_CONTROL_REQUEST_BYTES: u64 = 64 * 1024;

fn handle_connection(
    mut stream: UnixStream,
    authentication: &ControlSocketAuthentication,
    ctx: Option<&ControlSocketContext>,
) {
    // The listener is non-blocking; ensure the accepted stream is blocking
    // so the read timeout governs the per-connection hold time.
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(CONNECTION_READ_TIMEOUT));
    let _ = stream.set_write_timeout(Some(CONNECTION_READ_TIMEOUT));

    let (credentials, authenticated_session) = match authentication {
        ControlSocketAuthentication::Executable(config) => {
            let peer_identity = match identify_peer(&stream) {
                Ok(id) => id,
                Err(e) => {
                    warn!("failed to identify peer executable: {e}");
                    let resp = ControlResponse::err("peer_identification_failed", e.to_string());
                    let _ = send_response(&mut stream, &resp, None);
                    return;
                }
            };
            let verify = verify_peer(config, &peer_identity);
            if !verify.is_accepted() {
                let msg = verify
                    .denial_message()
                    .unwrap_or_else(|| "peer verification failed".to_string());
                warn!(pid = peer_identity.credentials.pid, reason = %msg, "control socket peer rejected");
                let resp = ControlResponse::err("permission_denied", msg);
                let _ = send_response(&mut stream, &resp, None);
                return;
            }
            (peer_identity.credentials, None)
        }
        ControlSocketAuthentication::Hmac { secret, uid, gid } => {
            let credentials = match get_peer_credentials(&stream) {
                Ok(credentials) => credentials,
                Err(e) => {
                    warn!("failed to identify HMAC peer credentials: {e}");
                    return;
                }
            };
            if uid.is_some_and(|expected| credentials.uid != expected)
                || gid.is_some_and(|expected| credentials.gid != expected)
            {
                warn!(
                    pid = credentials.pid,
                    "control socket peer credentials rejected"
                );
                return;
            }
            let session = match authenticate_server(
                &mut stream,
                secret,
                CONTROL_CLIENT_DOMAIN,
                CONTROL_SERVER_DOMAIN,
            ) {
                Ok(session) => session,
                Err(error) => {
                    warn!(pid = credentials.pid, reason = %error, "control socket HMAC peer rejected");
                    return;
                }
            };
            (credentials, Some(session))
        }
    };

    debug!(pid = credentials.pid, "control socket peer accepted");

    let line = if let Some(session) = &authenticated_session {
        match session.read_frame(
            &mut stream,
            FrameSender::Client,
            MAX_CONTROL_REQUEST_BYTES as usize,
        ) {
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(line) => line,
                Err(error) => {
                    debug!("control socket request is not UTF-8: {error}");
                    return;
                }
            },
            Err(error) => {
                warn!(pid = credentials.pid, reason = %error, "control socket business frame rejected");
                return;
            }
        }
    } else {
        let reader = BufReader::new(&stream);
        let mut limited = reader.take(MAX_CONTROL_REQUEST_BYTES + 1);
        let mut line = String::new();
        match limited.read_line(&mut line) {
            Ok(0) => return,
            Ok(n) if n as u64 > MAX_CONTROL_REQUEST_BYTES => {
                warn!(
                    pid = credentials.pid,
                    "control socket request exceeds {MAX_CONTROL_REQUEST_BYTES} byte limit"
                );
                let resp = ControlResponse::err(
                    "invalid_request",
                    format!("request exceeds {MAX_CONTROL_REQUEST_BYTES} byte limit"),
                );
                let _ = send_response(&mut stream, &resp, None);
                return;
            }
            Ok(_) => {}
            Err(e) => {
                debug!("control socket read error: {e}");
                return;
            }
        }
        line
    };

    if line.trim().is_empty() {
        return;
    }

    let resp = match parse_request_with_raw(&line) {
        Ok((req, raw)) => dispatch_request(&req, &raw, ctx),
        Err(err_resp) => err_resp,
    };

    let _ = send_response(&mut stream, &resp, authenticated_session.as_ref());
}

fn send_response(
    stream: &mut UnixStream,
    resp: &ControlResponse,
    session: Option<&AuthenticatedSession>,
) -> Result<(), Box<dyn std::error::Error>> {
    let json = serde_json::to_vec(resp)?;
    if let Some(session) = session {
        session.write_frame(stream, FrameSender::Server, &json)?;
    } else {
        stream.write_all(&json)?;
        stream.write_all(b"\n")?;
        stream.flush()?;
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Liveness retry state machine (deterministic, injected probes) ────
    //
    // The real `connect(2)` teardown almost always yields ECONNREFUSED on the
    // first attempt, so the live-socket integration test cannot reliably
    // reach the Ambiguous retry branch. These drive `resolve_socket_liveness`
    // with scripted single-probe outcomes to pin every branch.

    fn eagain() -> std::io::Error {
        std::io::Error::from_raw_os_error(libc::EAGAIN)
    }

    /// Build a probe closure that returns the given outcomes in order, then
    /// repeats the final outcome for any further attempts.
    fn scripted(outcomes: Vec<ProbeOnce>) -> impl FnMut() -> ProbeOnce {
        let mut i = 0usize;
        move || {
            // Reconstruct each outcome fresh (io::Error is not Clone).
            let idx = i.min(outcomes.len().saturating_sub(1));
            i += 1;
            match &outcomes[idx] {
                ProbeOnce::Connected => ProbeOnce::Connected,
                ProbeOnce::Refused => ProbeOnce::Refused,
                ProbeOnce::Ambiguous(e) => ProbeOnce::Ambiguous(std::io::Error::from_raw_os_error(
                    e.raw_os_error().unwrap_or(libc::EIO),
                )),
            }
        }
    }

    #[test]
    fn liveness_ambiguous_then_refused_is_stale() {
        // EAGAIN/EINPROGRESS that later settles to ECONNREFUSED → Stale.
        let probe = scripted(vec![
            ProbeOnce::Ambiguous(eagain()),
            ProbeOnce::Ambiguous(std::io::Error::from_raw_os_error(libc::EINPROGRESS)),
            ProbeOnce::Refused,
        ]);
        let r = resolve_socket_liveness(40, probe, || {});
        assert!(matches!(r, SocketLiveness::Stale));
    }

    #[test]
    fn liveness_persistent_ambiguous_backlog_is_live() {
        // Persistent EAGAIN (backlog-full live listener) → Live, never unlink.
        let probe = || ProbeOnce::Ambiguous(eagain());
        let r = resolve_socket_liveness(5, probe, || {});
        assert!(matches!(r, SocketLiveness::Live));
    }

    #[test]
    fn liveness_persistent_other_error_is_inconclusive() {
        // Persistent non-backlog error (e.g. EACCES) → fail closed.
        let probe = || ProbeOnce::Ambiguous(std::io::Error::from_raw_os_error(libc::EACCES));
        let r = resolve_socket_liveness(5, probe, || {});
        match r {
            SocketLiveness::Inconclusive(e) => {
                assert_eq!(e.raw_os_error(), Some(libc::EACCES));
            }
            _ => panic!("expected Inconclusive"),
        }
    }

    #[test]
    fn liveness_connected_is_immediately_live() {
        // A definitive connect on the first attempt → Live, no retries.
        let mut attempts = 0u32;
        let probe = || {
            attempts += 1;
            ProbeOnce::Connected
        };
        let r = resolve_socket_liveness(40, probe, || panic!("must not wait after Connected"));
        assert!(matches!(r, SocketLiveness::Live));
        assert_eq!(attempts, 1, "Connected must return on the first attempt");
    }

    #[test]
    fn liveness_refused_is_immediately_stale() {
        // A definitive refusal on the first attempt → Stale, no retries.
        let mut attempts = 0u32;
        let probe = || {
            attempts += 1;
            ProbeOnce::Refused
        };
        let r = resolve_socket_liveness(40, probe, || panic!("must not wait after Refused"));
        assert!(matches!(r, SocketLiveness::Stale));
        assert_eq!(attempts, 1, "Refused must return on the first attempt");
    }

    // ── Protocol parse / serialize ───────────────────────────────────────

    #[test]
    fn parse_ping_request() {
        let line = r#"{"schemaVersion":"1","method":"ping"}"#;
        let req = parse_request(line).unwrap();
        assert_eq!(req.method, "ping");
        assert_eq!(req.schema_version, "1");
    }

    #[test]
    fn parse_status_request() {
        let line = r#"{"schemaVersion":"1","method":"status"}"#;
        let req = parse_request(line).unwrap();
        assert_eq!(req.method, "status");
    }

    #[test]
    fn parse_request_missing_schema_version_is_error() {
        let line = r#"{"method":"ping"}"#;
        let result = parse_request(line);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(!err.ok);
        assert_eq!(err.error.as_ref().unwrap().code, "invalid_request");
    }

    #[test]
    fn parse_request_wrong_schema_version_is_error() {
        let line = r#"{"schemaVersion":"99","method":"ping"}"#;
        let result = parse_request(line);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(
            err.error.as_ref().unwrap().code,
            "unsupported_schema_version"
        );
    }

    #[test]
    fn parse_request_invalid_json_is_error() {
        let line = "not json at all";
        let result = parse_request(line);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.error.as_ref().unwrap().code, "invalid_request");
    }

    #[test]
    fn dispatch_ping_returns_pong() {
        let req = ControlRequest {
            schema_version: "1".to_string(),
            method: "ping".to_string(),
        };
        let raw = serde_json::json!({"schemaVersion": "1", "method": "ping"});
        let resp = dispatch_request(&req, &raw, None);
        assert!(resp.ok);
        let result = resp.result.unwrap();
        assert_eq!(result["pong"], true);
    }

    #[test]
    fn dispatch_status_returns_ready() {
        let req = ControlRequest {
            schema_version: "1".to_string(),
            method: "status".to_string(),
        };
        let raw = serde_json::json!({"schemaVersion": "1", "method": "status"});
        let resp = dispatch_request(&req, &raw, None);
        assert!(resp.ok);
        let result = resp.result.unwrap();
        assert_eq!(result["status"], "ready");
    }

    #[test]
    fn dispatch_unknown_method_returns_error() {
        let req = ControlRequest {
            schema_version: "1".to_string(),
            method: "write_meta".to_string(),
        };
        let raw = serde_json::json!({"schemaVersion": "1", "method": "write_meta"});
        let resp = dispatch_request(&req, &raw, None);
        assert!(!resp.ok);
        assert_eq!(resp.error.as_ref().unwrap().code, "unknown_method");
        assert!(resp.error.as_ref().unwrap().message.contains("write_meta"));
    }

    // ── Meta write dispatch (unit) ─────────────────────────────────────

    fn test_ctx(source_root: &Path) -> ControlSocketContext {
        test_ctx_with_layout(source_root, SkillLayout::Flat)
    }

    fn test_ctx_with_layout(source_root: &Path, layout: SkillLayout) -> ControlSocketContext {
        ControlSocketContext {
            canonical_root: source_root.to_path_buf(),
            source_root: source_root.to_path_buf(),
            layout,
            resolver: Some(Arc::new(ActiveSkillResolver::new(source_root))),
            protocol_event_writer: None,
        }
    }

    fn seed_activation_skill(source_root: &Path, skill_id: &str) -> PathBuf {
        let skill_dir = source_root.join(skill_id);
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "# Skill\n").unwrap();
        skill_dir
    }

    fn assert_flat_write_resolve_reject(source_root: &Path, skill_name: &str) {
        let skill_dir = source_root.join(skill_name);
        let raw = serde_json::json!({
            "schemaVersion": "1",
            "method": "meta.writeActivation",
            "skillName": skill_name,
            "activation": {"schemaVersion": 1, "target": null}
        });
        let req = ControlRequest {
            schema_version: "1".to_string(),
            method: "meta.writeActivation".to_string(),
        };
        let ctx = test_ctx(source_root);

        let write_resp = dispatch_request(&req, &raw, Some(&ctx));
        let resolve_error = super::super::resolver::resolve_live_source(
            source_root,
            source_root,
            SkillLayout::Flat,
            skill_dir.to_str().unwrap(),
        )
        .unwrap_err();

        assert!(!write_resp.ok);
        assert_eq!(write_resp.error.as_ref().unwrap().code, resolve_error.code);
        assert_eq!(resolve_error.code, "invalid_skill_layout");
        assert!(!skill_dir.join(".skill-meta").exists());
    }

    #[test]
    fn meta_write_activation_missing_skill_name() {
        let raw = serde_json::json!({
            "schemaVersion": "1",
            "method": "meta.writeActivation",
            "activation": {"schemaVersion": 1, "target": null}
        });
        let req = ControlRequest {
            schema_version: "1".to_string(),
            method: "meta.writeActivation".to_string(),
        };
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        let resp = dispatch_request(&req, &raw, Some(&ctx));
        assert!(!resp.ok);
        assert_eq!(resp.error.as_ref().unwrap().code, "invalid_request");
    }

    #[test]
    fn meta_write_activation_missing_activation() {
        let raw = serde_json::json!({
            "schemaVersion": "1",
            "method": "meta.writeActivation",
            "skillName": "alpha"
        });
        let req = ControlRequest {
            schema_version: "1".to_string(),
            method: "meta.writeActivation".to_string(),
        };
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("alpha")).unwrap();
        let ctx = test_ctx(dir.path());
        let resp = dispatch_request(&req, &raw, Some(&ctx));
        assert!(!resp.ok);
        assert_eq!(resp.error.as_ref().unwrap().code, "invalid_request");
    }

    #[test]
    fn meta_write_activation_invalid_skill_name_dot() {
        let raw = serde_json::json!({
            "schemaVersion": "1",
            "method": "meta.writeActivation",
            "skillName": "..",
            "activation": {"schemaVersion": 1, "target": null}
        });
        let req = ControlRequest {
            schema_version: "1".to_string(),
            method: "meta.writeActivation".to_string(),
        };
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        let resp = dispatch_request(&req, &raw, Some(&ctx));
        assert!(!resp.ok);
        assert_eq!(resp.error.as_ref().unwrap().code, "invalid_skill_name");
    }

    #[test]
    fn meta_write_activation_invalid_skill_name_slash() {
        let raw = serde_json::json!({
            "schemaVersion": "1",
            "method": "meta.writeActivation",
            "skillName": "a/b",
            "activation": {"schemaVersion": 1, "target": null}
        });
        let req = ControlRequest {
            schema_version: "1".to_string(),
            method: "meta.writeActivation".to_string(),
        };
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        let resp = dispatch_request(&req, &raw, Some(&ctx));
        assert!(!resp.ok);
        assert_eq!(resp.error.as_ref().unwrap().code, "invalid_skill_name");
    }

    #[test]
    fn meta_write_activation_hermes_nested_skill_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("category/skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "# Skill\n").unwrap();

        let raw = serde_json::json!({
            "schemaVersion": "1",
            "method": "meta.writeActivation",
            "skillName": "category/skill",
            "activation": {"schemaVersion": 1, "target": null}
        });
        let req = ControlRequest {
            schema_version: "1".to_string(),
            method: "meta.writeActivation".to_string(),
        };
        let ctx = test_ctx_with_layout(dir.path(), SkillLayout::Hermes);

        let resp = dispatch_request(&req, &raw, Some(&ctx));

        assert!(
            resp.ok,
            "expected nested activation write to succeed: {resp:?}"
        );
        assert_eq!(
            resp.result
                .as_ref()
                .and_then(|result| result["outcome"].as_str()),
            Some("updated")
        );
        assert!(skill_dir.join(".skill-meta/activation.json").is_file());
        assert!(matches!(
            ctx.resolver
                .as_ref()
                .and_then(|resolver| resolver.get("category/skill")),
            Some(ActiveTarget::Hidden { .. })
        ));
    }

    #[test]
    fn meta_write_activation_hermes_third_level_rejected() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("a/b/c")).unwrap();
        let raw = serde_json::json!({
            "schemaVersion": "1",
            "method": "meta.writeActivation",
            "skillName": "a/b/c",
            "activation": {"schemaVersion": 1, "target": null}
        });
        let req = ControlRequest {
            schema_version: "1".to_string(),
            method: "meta.writeActivation".to_string(),
        };
        let ctx = test_ctx_with_layout(dir.path(), SkillLayout::Hermes);

        let resp = dispatch_request(&req, &raw, Some(&ctx));

        assert!(!resp.ok);
        assert_eq!(resp.error.as_ref().unwrap().code, "invalid_skill_name");
        assert!(!dir.path().join("a/b/c/.skill-meta").exists());
    }

    #[test]
    fn meta_write_activation_hermes_category_symlink_rejected() {
        let source = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let outside_skill = outside.path().join("skill");
        std::fs::create_dir(&outside_skill).unwrap();
        std::os::unix::fs::symlink(outside.path(), source.path().join("category")).unwrap();

        let raw = serde_json::json!({
            "schemaVersion": "1",
            "method": "meta.writeActivation",
            "skillName": "category/skill",
            "activation": {"schemaVersion": 1, "target": null}
        });
        let req = ControlRequest {
            schema_version: "1".to_string(),
            method: "meta.writeActivation".to_string(),
        };
        let ctx = test_ctx_with_layout(source.path(), SkillLayout::Hermes);

        let resp = dispatch_request(&req, &raw, Some(&ctx));

        assert!(!resp.ok);
        assert_eq!(resp.error.as_ref().unwrap().code, "invalid_skill_name");
        assert!(!outside_skill.join(".skill-meta").exists());
    }

    #[test]
    fn meta_set_xattr_hermes_nested_skill_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("category/skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "# Skill\n").unwrap();
        let raw = serde_json::json!({
            "schemaVersion": "1",
            "method": "meta.setActivationXattr",
            "skillName": "category/skill",
            "activation": {"schemaVersion": 1, "target": null}
        });
        let req = ControlRequest {
            schema_version: "1".to_string(),
            method: "meta.setActivationXattr".to_string(),
        };
        let ctx = test_ctx_with_layout(dir.path(), SkillLayout::Hermes);

        let resp = dispatch_request(&req, &raw, Some(&ctx));
        if resp.error.as_ref().map(|error| error.code.as_str()) == Some("xattr_not_supported") {
            eprintln!("SKIP: temp filesystem does not support user xattrs");
            return;
        }

        assert!(resp.ok, "expected nested xattr write to succeed: {resp:?}");
        match super::super::activation::read_activation_xattr(&skill_dir) {
            super::super::activation::XattrReadOutcome::Present(value) => {
                let parsed: serde_json::Value = serde_json::from_str(&value).unwrap();
                assert_eq!(parsed["schemaVersion"], 1);
                assert!(parsed["target"].is_null());
            }
            other => panic!("expected activation xattr, got {other:?}"),
        }
    }

    #[test]
    fn meta_write_activation_hermes_missing_marker_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("category/ordinary");
        std::fs::create_dir_all(&skill_dir).unwrap();
        let raw = serde_json::json!({
            "schemaVersion": "1",
            "method": "meta.writeActivation",
            "skillName": "category/ordinary",
            "activation": {"schemaVersion": 1, "target": null}
        });
        let req = ControlRequest {
            schema_version: "1".to_string(),
            method: "meta.writeActivation".to_string(),
        };
        let ctx = test_ctx_with_layout(dir.path(), SkillLayout::Hermes);

        let resp = dispatch_request(&req, &raw, Some(&ctx));

        assert!(!resp.ok);
        assert_eq!(resp.error.as_ref().unwrap().code, "invalid_skill_layout");
        assert!(!skill_dir.join(".skill-meta").exists());
    }

    #[test]
    fn meta_write_activation_hermes_top_skill_subdir_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let top_skill = dir.path().join("top");
        let nested_dir = top_skill.join("subdir");
        std::fs::create_dir_all(&nested_dir).unwrap();
        std::fs::write(top_skill.join("SKILL.md"), "# Top Skill\n").unwrap();
        std::fs::write(nested_dir.join("SKILL.md"), "# Nested marker\n").unwrap();
        let raw = serde_json::json!({
            "schemaVersion": "1",
            "method": "meta.writeActivation",
            "skillName": "top/subdir",
            "activation": {"schemaVersion": 1, "target": null}
        });
        let req = ControlRequest {
            schema_version: "1".to_string(),
            method: "meta.writeActivation".to_string(),
        };
        let ctx = test_ctx_with_layout(dir.path(), SkillLayout::Hermes);

        let resp = dispatch_request(&req, &raw, Some(&ctx));

        assert!(!resp.ok);
        assert_eq!(resp.error.as_ref().unwrap().code, "invalid_skill_layout");
        assert!(!nested_dir.join(".skill-meta").exists());
    }

    #[test]
    fn meta_write_activation_hermes_symlink_marker_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let external = tempfile::NamedTempFile::new().unwrap();
        let skill_dir = dir.path().join("category/skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::os::unix::fs::symlink(external.path(), skill_dir.join("SKILL.md")).unwrap();
        let raw = serde_json::json!({
            "schemaVersion": "1",
            "method": "meta.writeActivation",
            "skillName": "category/skill",
            "activation": {"schemaVersion": 1, "target": null}
        });
        let req = ControlRequest {
            schema_version: "1".to_string(),
            method: "meta.writeActivation".to_string(),
        };
        let ctx = test_ctx_with_layout(dir.path(), SkillLayout::Hermes);

        let resp = dispatch_request(&req, &raw, Some(&ctx));

        assert!(!resp.ok);
        assert_eq!(resp.error.as_ref().unwrap().code, "invalid_skill_layout");
        assert!(!skill_dir.join(".skill-meta").exists());
    }

    #[test]
    fn meta_set_xattr_hermes_missing_marker_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("category/ordinary");
        std::fs::create_dir_all(&skill_dir).unwrap();
        let raw = serde_json::json!({
            "schemaVersion": "1",
            "method": "meta.setActivationXattr",
            "skillName": "category/ordinary",
            "activation": {"schemaVersion": 1, "target": null}
        });
        let req = ControlRequest {
            schema_version: "1".to_string(),
            method: "meta.setActivationXattr".to_string(),
        };
        let ctx = test_ctx_with_layout(dir.path(), SkillLayout::Hermes);

        let resp = dispatch_request(&req, &raw, Some(&ctx));

        assert!(!resp.ok);
        assert_eq!(resp.error.as_ref().unwrap().code, "invalid_skill_layout");
    }

    #[test]
    fn meta_write_activation_hermes_reserved_path_rejected() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".hub/skill")).unwrap();
        let raw = serde_json::json!({
            "schemaVersion": "1",
            "method": "meta.writeActivation",
            "skillName": ".hub/skill",
            "activation": {"schemaVersion": 1, "target": null}
        });
        let req = ControlRequest {
            schema_version: "1".to_string(),
            method: "meta.writeActivation".to_string(),
        };
        let ctx = test_ctx_with_layout(dir.path(), SkillLayout::Hermes);

        let resp = dispatch_request(&req, &raw, Some(&ctx));

        assert!(!resp.ok);
        assert_eq!(resp.error.as_ref().unwrap().code, "invalid_skill_name");
        assert!(!dir.path().join(".hub/skill/.skill-meta").exists());
    }

    #[test]
    fn meta_write_activation_flat_reserved_paths_rejected() {
        let dir = tempfile::tempdir().unwrap();
        for skill_name in [".hub", "skill-discover"] {
            let skill_dir = seed_activation_skill(dir.path(), skill_name);
            let raw = serde_json::json!({
                "schemaVersion": "1",
                "method": "meta.writeActivation",
                "skillName": skill_name,
                "activation": {"schemaVersion": 1, "target": null}
            });
            let req = ControlRequest {
                schema_version: "1".to_string(),
                method: "meta.writeActivation".to_string(),
            };
            let ctx = test_ctx(dir.path());

            let resp = dispatch_request(&req, &raw, Some(&ctx));

            assert!(!resp.ok, "{skill_name} must remain reserved");
            assert_eq!(resp.error.as_ref().unwrap().code, "invalid_skill_name");
            assert!(!skill_dir.join(".skill-meta").exists());
        }
    }

    #[test]
    fn meta_write_activation_flat_missing_marker_matches_resolver() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("ordinary-directory");
        std::fs::create_dir(&skill_dir).unwrap();

        assert_flat_write_resolve_reject(dir.path(), "ordinary-directory");
    }

    #[test]
    fn meta_write_activation_flat_symlink_marker_matches_resolver() {
        let dir = tempfile::tempdir().unwrap();
        let external = tempfile::NamedTempFile::new().unwrap();
        let skill_dir = dir.path().join("symlink-marker");
        std::fs::create_dir(&skill_dir).unwrap();
        std::os::unix::fs::symlink(external.path(), skill_dir.join("SKILL.md")).unwrap();

        assert_flat_write_resolve_reject(dir.path(), "symlink-marker");
    }

    #[test]
    fn shared_fd_resolution_preserves_protocol_error_semantics() {
        let dir = tempfile::tempdir().unwrap();
        let skill_path = dir.path().join("not-a-directory");
        std::fs::write(&skill_path, "ordinary file").unwrap();
        let raw = serde_json::json!({
            "schemaVersion": "1",
            "method": "meta.writeActivation",
            "skillName": "not-a-directory",
            "activation": {"schemaVersion": 1, "target": null}
        });
        let req = ControlRequest {
            schema_version: "1".to_string(),
            method: "meta.writeActivation".to_string(),
        };
        let ctx = test_ctx(dir.path());

        let write_error = dispatch_request(&req, &raw, Some(&ctx))
            .error
            .expect("write must reject a non-directory Skill path");
        let resolve_error = super::super::resolver::resolve_live_source(
            dir.path(),
            dir.path(),
            SkillLayout::Flat,
            skill_path.to_str().unwrap(),
        )
        .unwrap_err();

        assert_eq!(write_error.code, "skill_not_found");
        assert_eq!(
            write_error.message,
            "skill path component 'not-a-directory' in 'not-a-directory' is not a directory"
        );
        assert_eq!(resolve_error.code, "invalid_skill_layout");
        assert_eq!(
            resolve_error.message,
            "skill path component 'not-a-directory' is not a directory"
        );
    }

    #[test]
    fn meta_write_activation_invalid_skill_name_nul() {
        let raw = serde_json::json!({
            "schemaVersion": "1",
            "method": "meta.writeActivation",
            "skillName": "a\0b",
            "activation": {"schemaVersion": 1, "target": null}
        });
        let req = ControlRequest {
            schema_version: "1".to_string(),
            method: "meta.writeActivation".to_string(),
        };
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        let resp = dispatch_request(&req, &raw, Some(&ctx));
        assert!(!resp.ok);
        assert_eq!(resp.error.as_ref().unwrap().code, "invalid_skill_name");
    }

    #[test]
    fn meta_write_activation_invalid_skill_name_empty() {
        let raw = serde_json::json!({
            "schemaVersion": "1",
            "method": "meta.writeActivation",
            "skillName": "",
            "activation": {"schemaVersion": 1, "target": null}
        });
        let req = ControlRequest {
            schema_version: "1".to_string(),
            method: "meta.writeActivation".to_string(),
        };
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        let resp = dispatch_request(&req, &raw, Some(&ctx));
        assert!(!resp.ok);
        assert_eq!(resp.error.as_ref().unwrap().code, "invalid_skill_name");
    }

    #[test]
    fn meta_write_activation_skill_not_found() {
        let raw = serde_json::json!({
            "schemaVersion": "1",
            "method": "meta.writeActivation",
            "skillName": "nonexistent",
            "activation": {"schemaVersion": 1, "target": null}
        });
        let req = ControlRequest {
            schema_version: "1".to_string(),
            method: "meta.writeActivation".to_string(),
        };
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        let resp = dispatch_request(&req, &raw, Some(&ctx));
        assert!(!resp.ok);
        assert_eq!(resp.error.as_ref().unwrap().code, "skill_not_found");
    }

    #[test]
    fn meta_write_activation_symlink_skill_dir_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let real_dir = dir.path().join("real-skill");
        std::fs::create_dir(&real_dir).unwrap();
        std::os::unix::fs::symlink(&real_dir, dir.path().join("link-skill")).unwrap();

        let raw = serde_json::json!({
            "schemaVersion": "1",
            "method": "meta.writeActivation",
            "skillName": "link-skill",
            "activation": {"schemaVersion": 1, "target": null}
        });
        let req = ControlRequest {
            schema_version: "1".to_string(),
            method: "meta.writeActivation".to_string(),
        };
        let ctx = test_ctx(dir.path());
        let resp = dispatch_request(&req, &raw, Some(&ctx));
        assert!(!resp.ok);
        assert_eq!(resp.error.as_ref().unwrap().code, "invalid_skill_name");
        assert!(
            resp.error.as_ref().unwrap().message.contains("symlink"),
            "error should mention symlink: {}",
            resp.error.as_ref().unwrap().message
        );
    }

    #[test]
    fn meta_set_xattr_symlink_skill_dir_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let real_dir = dir.path().join("real-skill");
        std::fs::create_dir(&real_dir).unwrap();
        std::os::unix::fs::symlink(&real_dir, dir.path().join("link-skill")).unwrap();

        let raw = serde_json::json!({
            "schemaVersion": "1",
            "method": "meta.setActivationXattr",
            "skillName": "link-skill",
            "activation": {"schemaVersion": 1, "target": null}
        });
        let req = ControlRequest {
            schema_version: "1".to_string(),
            method: "meta.setActivationXattr".to_string(),
        };
        let ctx = test_ctx(dir.path());
        let resp = dispatch_request(&req, &raw, Some(&ctx));
        assert!(!resp.ok);
        assert_eq!(resp.error.as_ref().unwrap().code, "invalid_skill_name");
    }

    #[test]
    fn meta_write_activation_no_resolver_returns_not_configured() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("alpha")).unwrap();
        let ctx = ControlSocketContext {
            canonical_root: dir.path().to_path_buf(),
            source_root: dir.path().to_path_buf(),
            layout: SkillLayout::Flat,
            resolver: None,
            protocol_event_writer: None,
        };
        let raw = serde_json::json!({
            "schemaVersion": "1",
            "method": "meta.writeActivation",
            "skillName": "alpha",
            "activation": {"schemaVersion": 1, "target": null}
        });
        let req = ControlRequest {
            schema_version: "1".to_string(),
            method: "meta.writeActivation".to_string(),
        };
        let resp = dispatch_request(&req, &raw, Some(&ctx));
        assert!(!resp.ok);
        assert_eq!(resp.error.as_ref().unwrap().code, "not_configured");
        // Verify no file was written to disk.
        assert!(
            !dir.path()
                .join("alpha/.skill-meta/activation.json")
                .exists(),
            "no-resolver request must not write to disk"
        );
    }

    #[test]
    fn meta_set_xattr_no_resolver_returns_not_configured() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("alpha")).unwrap();
        let ctx = ControlSocketContext {
            canonical_root: dir.path().to_path_buf(),
            source_root: dir.path().to_path_buf(),
            layout: SkillLayout::Flat,
            resolver: None,
            protocol_event_writer: None,
        };
        let raw = serde_json::json!({
            "schemaVersion": "1",
            "method": "meta.setActivationXattr",
            "skillName": "alpha",
            "activation": {"schemaVersion": 1, "target": null}
        });
        let req = ControlRequest {
            schema_version: "1".to_string(),
            method: "meta.setActivationXattr".to_string(),
        };
        let resp = dispatch_request(&req, &raw, Some(&ctx));
        assert!(!resp.ok);
        assert_eq!(resp.error.as_ref().unwrap().code, "not_configured");
    }

    #[test]
    fn meta_write_activation_invalid_activation_bad_schema() {
        let raw = serde_json::json!({
            "schemaVersion": "1",
            "method": "meta.writeActivation",
            "skillName": "alpha",
            "activation": {"schemaVersion": 99, "target": null}
        });
        let req = ControlRequest {
            schema_version: "1".to_string(),
            method: "meta.writeActivation".to_string(),
        };
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("alpha")).unwrap();
        let ctx = test_ctx(dir.path());
        let resp = dispatch_request(&req, &raw, Some(&ctx));
        assert!(!resp.ok);
        assert_eq!(resp.error.as_ref().unwrap().code, "invalid_activation");
    }

    #[test]
    fn meta_write_activation_invalid_activation_bad_target() {
        let raw = serde_json::json!({
            "schemaVersion": "1",
            "method": "meta.writeActivation",
            "skillName": "alpha",
            "activation": {"schemaVersion": 1, "target": "/etc/passwd"}
        });
        let req = ControlRequest {
            schema_version: "1".to_string(),
            method: "meta.writeActivation".to_string(),
        };
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("alpha")).unwrap();
        let ctx = test_ctx(dir.path());
        let resp = dispatch_request(&req, &raw, Some(&ctx));
        assert!(!resp.ok);
        assert_eq!(resp.error.as_ref().unwrap().code, "invalid_activation");
    }

    #[test]
    fn meta_write_activation_no_context_returns_not_configured() {
        let raw = serde_json::json!({
            "schemaVersion": "1",
            "method": "meta.writeActivation",
            "skillName": "alpha",
            "activation": {"schemaVersion": 1, "target": null}
        });
        let req = ControlRequest {
            schema_version: "1".to_string(),
            method: "meta.writeActivation".to_string(),
        };
        let resp = dispatch_request(&req, &raw, None);
        assert!(!resp.ok);
        assert_eq!(resp.error.as_ref().unwrap().code, "not_configured");
    }

    #[test]
    fn meta_write_activation_success_writes_file() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("alpha");
        let meta_dir = skill_dir.join(".skill-meta");
        let activation_path = meta_dir.join("activation.json");
        std::fs::create_dir_all(&meta_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "# Skill\n").unwrap();
        std::fs::write(&activation_path, "old").unwrap();
        std::fs::set_permissions(&meta_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&activation_path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let raw = serde_json::json!({
            "schemaVersion": "1",
            "method": "meta.writeActivation",
            "skillName": "alpha",
            "activation": {"schemaVersion": 1, "target": null}
        });
        let req = ControlRequest {
            schema_version: "1".to_string(),
            method: "meta.writeActivation".to_string(),
        };
        let ctx = test_ctx(dir.path());
        let resp = dispatch_request(&req, &raw, Some(&ctx));
        assert!(resp.ok, "expected ok, got: {resp:?}");

        let written = std::fs::read_to_string(&activation_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&written).unwrap();
        assert_eq!(parsed["schemaVersion"], 1);
        assert!(parsed["target"].is_null());
        let meta_mode = std::fs::metadata(&meta_dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(meta_mode, 0o700, ".skill-meta must be owner-only");
        let mode = std::fs::metadata(&activation_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "activation.json must be owner-only");
    }

    #[test]
    fn meta_write_activation_success_updates_resolver() {
        let dir = tempfile::tempdir().unwrap();
        seed_activation_skill(dir.path(), "alpha");

        let resolver = Arc::new(ActiveSkillResolver::new(dir.path()));
        let ctx = ControlSocketContext {
            canonical_root: dir.path().to_path_buf(),
            source_root: dir.path().to_path_buf(),
            layout: SkillLayout::Flat,
            resolver: Some(resolver.clone()),
            protocol_event_writer: None,
        };

        let raw = serde_json::json!({
            "schemaVersion": "1",
            "method": "meta.writeActivation",
            "skillName": "alpha",
            "activation": {"schemaVersion": 1, "target": null}
        });
        let req = ControlRequest {
            schema_version: "1".to_string(),
            method: "meta.writeActivation".to_string(),
        };
        let resp = dispatch_request(&req, &raw, Some(&ctx));
        assert!(resp.ok);

        assert!(
            matches!(resolver.get("alpha"), Some(ActiveTarget::Hidden { .. })),
            "resolver should have hidden target after null activation write"
        );
    }

    #[test]
    fn meta_write_activation_snapshot_target_updates_resolver() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("alpha");
        std::fs::create_dir_all(skill_dir.join(".skill-meta/versions/v000001.snapshot")).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "# Skill\n").unwrap();

        let resolver = Arc::new(ActiveSkillResolver::new(dir.path()));
        let ctx = ControlSocketContext {
            canonical_root: dir.path().to_path_buf(),
            source_root: dir.path().to_path_buf(),
            layout: SkillLayout::Flat,
            resolver: Some(resolver.clone()),
            protocol_event_writer: None,
        };

        let raw = serde_json::json!({
            "schemaVersion": "1",
            "method": "meta.writeActivation",
            "skillName": "alpha",
            "activation": {"schemaVersion": 1, "target": ".skill-meta/versions/v000001.snapshot"}
        });
        let req = ControlRequest {
            schema_version: "1".to_string(),
            method: "meta.writeActivation".to_string(),
        };
        let resp = dispatch_request(&req, &raw, Some(&ctx));
        assert!(resp.ok, "expected ok, got: {resp:?}");

        assert!(
            matches!(resolver.get("alpha"), Some(ActiveTarget::Snapshot { .. })),
            "resolver should have snapshot target"
        );
    }

    #[test]
    fn meta_set_xattr_missing_skill_name() {
        let raw = serde_json::json!({
            "schemaVersion": "1",
            "method": "meta.setActivationXattr",
            "activation": {"schemaVersion": 1, "target": null}
        });
        let req = ControlRequest {
            schema_version: "1".to_string(),
            method: "meta.setActivationXattr".to_string(),
        };
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        let resp = dispatch_request(&req, &raw, Some(&ctx));
        assert!(!resp.ok);
        assert_eq!(resp.error.as_ref().unwrap().code, "invalid_request");
    }

    #[test]
    fn meta_set_xattr_invalid_skill_name() {
        let raw = serde_json::json!({
            "schemaVersion": "1",
            "method": "meta.setActivationXattr",
            "skillName": "../escape",
            "activation": {"schemaVersion": 1, "target": null}
        });
        let req = ControlRequest {
            schema_version: "1".to_string(),
            method: "meta.setActivationXattr".to_string(),
        };
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        let resp = dispatch_request(&req, &raw, Some(&ctx));
        assert!(!resp.ok);
        assert_eq!(resp.error.as_ref().unwrap().code, "invalid_skill_name");
    }

    #[test]
    fn meta_set_xattr_skill_not_found() {
        let raw = serde_json::json!({
            "schemaVersion": "1",
            "method": "meta.setActivationXattr",
            "skillName": "missing",
            "activation": {"schemaVersion": 1, "target": null}
        });
        let req = ControlRequest {
            schema_version: "1".to_string(),
            method: "meta.setActivationXattr".to_string(),
        };
        let dir = tempfile::tempdir().unwrap();
        let ctx = test_ctx(dir.path());
        let resp = dispatch_request(&req, &raw, Some(&ctx));
        assert!(!resp.ok);
        assert_eq!(resp.error.as_ref().unwrap().code, "skill_not_found");
    }

    #[test]
    fn meta_set_xattr_invalid_activation() {
        let raw = serde_json::json!({
            "schemaVersion": "1",
            "method": "meta.setActivationXattr",
            "skillName": "alpha",
            "activation": {"schemaVersion": 99, "target": null}
        });
        let req = ControlRequest {
            schema_version: "1".to_string(),
            method: "meta.setActivationXattr".to_string(),
        };
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("alpha")).unwrap();
        let ctx = test_ctx(dir.path());
        let resp = dispatch_request(&req, &raw, Some(&ctx));
        assert!(!resp.ok);
        assert_eq!(resp.error.as_ref().unwrap().code, "invalid_activation");
    }

    #[test]
    fn parse_request_with_raw_preserves_extra_fields() {
        let line = r#"{"schemaVersion":"1","method":"meta.writeActivation","skillName":"demo","activation":{"schemaVersion":1,"target":null}}"#;
        let (req, raw) = parse_request_with_raw(line).unwrap();
        assert_eq!(req.method, "meta.writeActivation");
        assert_eq!(raw["skillName"], "demo");
        assert!(raw.get("activation").is_some());
    }

    #[test]
    fn response_ok_serializes() {
        let resp = ControlResponse::ok(serde_json::json!({"pong": true}));
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"ok\":true"));
        assert!(json.contains("\"pong\":true"));
        assert!(!json.contains("\"error\""));
    }

    #[test]
    fn response_err_serializes() {
        let resp = ControlResponse::err("test_code", "test message");
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"ok\":false"));
        assert!(json.contains("\"test_code\""));
        assert!(json.contains("test message"));
        assert!(!json.contains("\"result\""));
    }

    // ── Socket path preflight ────────────────────────────────────────────

    #[test]
    fn preflight_nonexistent_parent_fails() {
        let path = PathBuf::from("/nonexistent/parent/dir/socket.sock");
        let result = preflight_socket_path(&path);
        assert!(result.is_err());
        match result.unwrap_err() {
            SocketPreflightError::ParentDoesNotExist(_) => {}
            other => panic!("expected ParentDoesNotExist, got {other:?}"),
        }
    }

    #[test]
    fn preflight_existing_regular_file_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("not-a-socket");
        std::fs::write(&path, "data").unwrap();
        let result = preflight_socket_path(&path);
        assert!(result.is_err());
        match result.unwrap_err() {
            SocketPreflightError::ExistingPathNotSocket(_) => {}
            other => panic!("expected ExistingPathNotSocket, got {other:?}"),
        }
    }

    #[test]
    fn preflight_existing_directory_fails() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("subdir");
        std::fs::create_dir(&sub).unwrap();
        let result = preflight_socket_path(&sub);
        assert!(result.is_err());
        match result.unwrap_err() {
            SocketPreflightError::ExistingPathNotSocket(_) => {}
            other => panic!("expected ExistingPathNotSocket, got {other:?}"),
        }
    }

    #[test]
    fn preflight_existing_socket_is_unlinked() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stale.sock");
        // Create a real socket to simulate stale leftover.
        let _listener = UnixListener::bind(&path).unwrap();
        drop(_listener);
        assert!(path.exists());
        let result = preflight_socket_path(&path);
        assert!(result.is_ok());
        assert!(!path.exists(), "stale socket should have been unlinked");
    }

    #[test]
    fn preflight_nonexistent_path_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("new.sock");
        let result = preflight_socket_path(&path);
        assert!(result.is_ok());
    }

    #[test]
    fn preflight_symlink_not_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        std::fs::write(&target, "x").unwrap();
        let link = dir.path().join("link.sock");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let result = preflight_socket_path(&link);
        assert!(matches!(
            result,
            Err(SocketPreflightError::ExistingPathNotSocket(_))
        ));
        // The symlink must not have been deleted.
        assert!(
            std::fs::symlink_metadata(&link).is_ok(),
            "symlink must not be deleted"
        );
    }

    // ── Socket-object classification ─────────────────────────────────────

    #[test]
    fn classify_socket_object_non_socket_never_deleted() {
        assert_eq!(
            classify_socket_object(false, 1000, 1000),
            SocketObjectClass::NotSocket
        );
    }

    #[test]
    fn classify_socket_object_wrong_owner_never_deleted() {
        // A socket owned by a different uid must never be reclaimed.
        assert_eq!(
            classify_socket_object(true, 999, 1000),
            SocketObjectClass::WrongOwner(999)
        );
    }

    #[test]
    fn classify_socket_object_owned_socket_is_reclaim_candidate() {
        assert_eq!(
            classify_socket_object(true, 1000, 1000),
            SocketObjectClass::OwnedSocket
        );
    }

    // ── Default endpoint + priority ──────────────────────────────────────

    #[test]
    fn default_endpoint_path_is_under_run_user_never_tmp() {
        let p = default_control_socket_path(1000);
        assert_eq!(p, PathBuf::from("/run/user/1000/skillfs/control.sock"));
        let s = p.to_string_lossy();
        assert!(
            !s.contains("/tmp"),
            "default endpoint must not use /tmp: {s}"
        );
        assert!(
            !s.contains("/var/tmp"),
            "default endpoint must not use /var/tmp: {s}"
        );
    }

    #[test]
    fn resolve_default_endpoint_is_run_user_or_actionable_error() {
        let uid = unsafe { libc::geteuid() };
        match resolve_default_control_socket_endpoint() {
            Ok(p) => {
                // When /run/user/<uid> exists, the resolved path is the
                // per-user endpoint and never a public temp directory.
                assert_eq!(p, default_control_socket_path(uid));
                let s = p.to_string_lossy();
                assert!(!s.contains("/tmp") && !s.contains("/var/tmp"));
            }
            Err(e) => {
                // When it is unavailable, the error is clear and actionable.
                let msg = e.to_string();
                assert!(
                    msg.contains("--control-socket"),
                    "error must be actionable: {msg}"
                );
                assert!(
                    msg.contains(&format!("/run/user/{uid}")),
                    "error must name the runtime dir: {msg}"
                );
            }
        }
    }

    #[test]
    fn endpoint_priority_cli_over_config_over_default() {
        // CLI value wins over config (the `or_else` merge in cmd_mount is
        // applied first, then classified here).
        let cli = Some(PathBuf::from("/cli.sock"));
        let config = Some(PathBuf::from("/config.sock"));
        let merged = cli.clone().or(config.clone());
        assert_eq!(
            classify_control_socket_endpoint(merged.as_deref(), true),
            EndpointResolution::Explicit(PathBuf::from("/cli.sock"))
        );

        // Config value wins over the default when there is no CLI value.
        let merged2: Option<PathBuf> = None.or(config);
        assert_eq!(
            classify_control_socket_endpoint(merged2.as_deref(), true),
            EndpointResolution::Explicit(PathBuf::from("/config.sock"))
        );

        // Trusted peer, no explicit path → default endpoint.
        assert_eq!(
            classify_control_socket_endpoint(None, true),
            EndpointResolution::UseDefault
        );
    }

    #[test]
    fn endpoint_explicit_path_without_trusted_peer_is_error() {
        assert_eq!(
            classify_control_socket_endpoint(Some(Path::new("/x.sock")), false),
            EndpointResolution::MissingTrustedPeer(PathBuf::from("/x.sock"))
        );
    }

    #[test]
    fn endpoint_neither_disables_control_plane() {
        assert_eq!(
            classify_control_socket_endpoint(None, false),
            EndpointResolution::Disabled
        );
    }

    // ── skill.resolveLiveSource dispatch plumbing ────────────────────────
    //
    // The resolver behavior suite (managed/not-managed/errors, layout
    // boundaries, identity, no-side-effects) lives in `super::resolver`'s
    // unit tests against the pure `resolve_live_source`. These tests cover
    // only the control-socket dispatch layer: context threading and
    // envelope validation.

    fn resolver_ctx(canonical_root: &Path, live_root: &Path) -> ControlSocketContext {
        ControlSocketContext {
            canonical_root: canonical_root.to_path_buf(),
            source_root: live_root.to_path_buf(),
            layout: SkillLayout::Flat,
            // The read-only resolver must work without an active resolver.
            resolver: None,
            protocol_event_writer: None,
        }
    }

    fn seed_skill(root: &Path, rel: &str) {
        let dir = root.join(rel);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            "---\nname: x\ndescription: y\n---\nbody\n",
        )
        .unwrap();
    }

    fn resolve_req(canonical_skill_dir: &str) -> serde_json::Value {
        serde_json::json!({
            "schemaVersion": "1",
            "method": "skill.resolveLiveSource",
            "canonicalSkillDir": canonical_skill_dir,
        })
    }

    #[test]
    fn resolve_missing_canonical_field_is_invalid_request() {
        let root = tempfile::tempdir().unwrap();
        let ctx = resolver_ctx(root.path(), root.path());
        let raw = serde_json::json!({
            "schemaVersion": "1",
            "method": "skill.resolveLiveSource",
        });
        let resp = dispatch_resolve_live_source(&raw, Some(&ctx));
        assert!(!resp.ok);
        assert_eq!(resp.error.unwrap().code, "invalid_request");
    }

    #[test]
    fn resolve_without_context_is_not_configured() {
        let raw = resolve_req("/x/y");
        let resp = dispatch_resolve_live_source(&raw, None);
        assert!(!resp.ok);
        assert_eq!(resp.error.unwrap().code, "not_configured");
    }

    #[test]
    fn resolve_dispatches_via_dispatch_request() {
        // The method is reachable through the top-level dispatcher and
        // threads the context roots + layout into the resolver.
        let root = tempfile::tempdir().unwrap();
        seed_skill(root.path(), "my-skill");
        let ctx = resolver_ctx(root.path(), root.path());
        let raw = resolve_req(root.path().join("my-skill").to_str().unwrap());
        let req = ControlRequest {
            schema_version: "1".to_string(),
            method: "skill.resolveLiveSource".to_string(),
        };
        let resp = dispatch_request(&req, &raw, Some(&ctx));
        assert!(resp.ok);
        assert_eq!(resp.result.unwrap()["managed"], true);
    }

    // ── Peer verification ────────────────────────────────────────────────

    fn test_peer_config() -> TrustedPeerConfig {
        TrustedPeerConfig {
            exe_path: PathBuf::from("/usr/local/bin/agent-sec-cli"),
            exe_file_id: FileId { dev: 10, ino: 20 },
            uid: None,
            gid: None,
        }
    }

    fn test_peer_identity() -> PeerIdentity {
        PeerIdentity {
            credentials: PeerCredentials {
                pid: 1234,
                uid: 1000,
                gid: 1000,
            },
            exe_path: Some(PathBuf::from("/usr/local/bin/agent-sec-cli")),
            exe_file_id: Some(FileId { dev: 10, ino: 20 }),
            starttime_before: Some(9876543),
            starttime_after: Some(9876543),
        }
    }

    #[test]
    fn verify_matching_peer_accepted() {
        let config = test_peer_config();
        let identity = test_peer_identity();
        let result = verify_peer(&config, &identity);
        assert!(result.is_accepted());
    }

    #[test]
    fn verify_uid_mismatch_denied() {
        let config = TrustedPeerConfig {
            uid: Some(0),
            ..test_peer_config()
        };
        let identity = test_peer_identity();
        let result = verify_peer(&config, &identity);
        assert!(!result.is_accepted());
        match result {
            PeerVerifyResult::DeniedUidMismatch {
                expected, actual, ..
            } => {
                assert_eq!(expected, 0);
                assert_eq!(actual, 1000);
            }
            other => panic!("expected DeniedUidMismatch, got {other:?}"),
        }
    }

    #[test]
    fn verify_gid_mismatch_denied() {
        let config = TrustedPeerConfig {
            gid: Some(0),
            ..test_peer_config()
        };
        let identity = test_peer_identity();
        let result = verify_peer(&config, &identity);
        assert!(!result.is_accepted());
        match result {
            PeerVerifyResult::DeniedGidMismatch {
                expected, actual, ..
            } => {
                assert_eq!(expected, 0);
                assert_eq!(actual, 1000);
            }
            other => panic!("expected DeniedGidMismatch, got {other:?}"),
        }
    }

    #[test]
    fn verify_uid_match_accepted() {
        let config = TrustedPeerConfig {
            uid: Some(1000),
            ..test_peer_config()
        };
        let identity = test_peer_identity();
        let result = verify_peer(&config, &identity);
        assert!(result.is_accepted());
    }

    #[test]
    fn verify_gid_match_accepted() {
        let config = TrustedPeerConfig {
            gid: Some(1000),
            ..test_peer_config()
        };
        let identity = test_peer_identity();
        let result = verify_peer(&config, &identity);
        assert!(result.is_accepted());
    }

    #[test]
    fn verify_exe_unresolved_denied() {
        let config = test_peer_config();
        let identity = PeerIdentity {
            credentials: PeerCredentials {
                pid: 1234,
                uid: 1000,
                gid: 1000,
            },
            exe_path: None,
            exe_file_id: None,
            starttime_before: Some(100),
            starttime_after: Some(100),
        };
        let result = verify_peer(&config, &identity);
        assert!(!result.is_accepted());
        assert!(matches!(result, PeerVerifyResult::DeniedExeUnresolved));
    }

    #[test]
    fn verify_exe_path_mismatch_denied() {
        let config = test_peer_config();
        let identity = PeerIdentity {
            credentials: PeerCredentials {
                pid: 1234,
                uid: 1000,
                gid: 1000,
            },
            exe_path: Some(PathBuf::from("/usr/bin/imposter")),
            exe_file_id: Some(FileId { dev: 99, ino: 99 }),
            starttime_before: Some(100),
            starttime_after: Some(100),
        };
        let result = verify_peer(&config, &identity);
        assert!(!result.is_accepted());
        match result {
            PeerVerifyResult::DeniedExePathMismatch { .. } => {}
            other => panic!("expected DeniedExePathMismatch, got {other:?}"),
        }
    }

    #[test]
    fn verify_exe_file_id_mismatch_denied() {
        let config = test_peer_config();
        let identity = PeerIdentity {
            credentials: PeerCredentials {
                pid: 1234,
                uid: 1000,
                gid: 1000,
            },
            exe_path: Some(PathBuf::from("/usr/local/bin/agent-sec-cli")),
            exe_file_id: Some(FileId { dev: 10, ino: 999 }),
            starttime_before: Some(100),
            starttime_after: Some(100),
        };
        let result = verify_peer(&config, &identity);
        assert!(!result.is_accepted());
        match result {
            PeerVerifyResult::DeniedExeFileIdMismatch {
                expected, actual, ..
            } => {
                assert_eq!(expected, FileId { dev: 10, ino: 20 });
                assert_eq!(actual, FileId { dev: 10, ino: 999 });
            }
            other => panic!("expected DeniedExeFileIdMismatch, got {other:?}"),
        }
    }

    #[test]
    fn verify_uid_checked_before_exe() {
        let config = TrustedPeerConfig {
            uid: Some(0),
            ..test_peer_config()
        };
        let identity = PeerIdentity {
            credentials: PeerCredentials {
                pid: 1234,
                uid: 1000,
                gid: 1000,
            },
            exe_path: None,
            exe_file_id: None,
            starttime_before: None,
            starttime_after: None,
        };
        let result = verify_peer(&config, &identity);
        assert!(
            matches!(result, PeerVerifyResult::DeniedUidMismatch { .. }),
            "uid check should fire before exe check"
        );
    }

    #[test]
    fn denial_message_variants() {
        assert!(PeerVerifyResult::Accepted.denial_message().is_none());
        assert!(
            PeerVerifyResult::DeniedUidMismatch {
                expected: 0,
                actual: 1000
            }
            .denial_message()
            .unwrap()
            .contains("uid")
        );
        assert!(
            PeerVerifyResult::DeniedGidMismatch {
                expected: 0,
                actual: 1000
            }
            .denial_message()
            .unwrap()
            .contains("gid")
        );
        assert!(
            PeerVerifyResult::DeniedExeUnresolved
                .denial_message()
                .unwrap()
                .contains("resolved")
        );
        assert!(
            PeerVerifyResult::DeniedExePathMismatch {
                expected: PathBuf::from("/a"),
                actual: PathBuf::from("/b"),
            }
            .denial_message()
            .unwrap()
            .contains("path")
        );
        assert!(
            PeerVerifyResult::DeniedExeFileIdMismatch {
                expected: FileId { dev: 1, ino: 2 },
                actual: FileId { dev: 3, ino: 4 },
            }
            .denial_message()
            .unwrap()
            .contains("file id")
        );
        assert!(
            PeerVerifyResult::DeniedStarttimeUnresolved
                .denial_message()
                .unwrap()
                .contains("starttime")
        );
        assert!(
            PeerVerifyResult::DeniedStarttimeMismatch {
                pid: 42,
                pinned: 100,
                actual: 200,
            }
            .denial_message()
            .unwrap()
            .contains("starttime")
        );
    }

    // ── Starttime bracket verification ─────────────────────────────────

    #[cfg(target_os = "linux")]
    #[test]
    fn verify_missing_starttime_before_denied() {
        let config = test_peer_config();
        let identity = PeerIdentity {
            starttime_before: None,
            starttime_after: Some(100),
            ..test_peer_identity()
        };
        let result = verify_peer(&config, &identity);
        assert!(!result.is_accepted());
        assert!(
            matches!(result, PeerVerifyResult::DeniedStarttimeUnresolved),
            "missing starttime_before must deny on Linux, got {result:?}"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn verify_missing_starttime_after_denied() {
        let config = test_peer_config();
        let identity = PeerIdentity {
            starttime_before: Some(100),
            starttime_after: None,
            ..test_peer_identity()
        };
        let result = verify_peer(&config, &identity);
        assert!(!result.is_accepted());
        assert!(
            matches!(result, PeerVerifyResult::DeniedStarttimeUnresolved),
            "missing starttime_after must deny on Linux, got {result:?}"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn verify_starttime_before_after_mismatch_denied() {
        let config = test_peer_config();
        let identity = PeerIdentity {
            starttime_before: Some(100),
            starttime_after: Some(200),
            ..test_peer_identity()
        };
        let result = verify_peer(&config, &identity);
        assert!(!result.is_accepted());
        match result {
            PeerVerifyResult::DeniedStarttimeMismatch {
                pid,
                pinned,
                actual,
            } => {
                assert_eq!(pid, 1234);
                assert_eq!(pinned, 100);
                assert_eq!(actual, 200);
            }
            other => panic!("expected DeniedStarttimeMismatch, got {other:?}"),
        }
    }

    #[test]
    fn verify_with_matching_starttime_accepted() {
        let config = test_peer_config();
        let identity = PeerIdentity {
            starttime_before: Some(42),
            starttime_after: Some(42),
            ..test_peer_identity()
        };
        let result = verify_peer(&config, &identity);
        assert!(result.is_accepted());
    }

    // ── Server integration (Linux only) ──────────────────────────────────

    #[cfg(target_os = "linux")]
    mod integration {
        use super::*;
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::fs::MetadataExt;

        fn self_exe_config() -> TrustedPeerConfig {
            let exe = std::env::current_exe().unwrap();
            let canon = std::fs::canonicalize(&exe).unwrap();
            let meta = std::fs::metadata(&canon).unwrap();
            TrustedPeerConfig {
                exe_path: canon,
                exe_file_id: FileId {
                    dev: meta.dev(),
                    ino: meta.ino(),
                },
                uid: None,
                gid: None,
            }
        }

        fn connect_and_send(socket_path: &Path, request: &str) -> String {
            let mut stream = UnixStream::connect(socket_path).unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            writeln!(stream, "{request}").unwrap();
            stream.flush().unwrap();
            let mut reader = BufReader::new(&stream);
            let mut response = String::new();
            reader.read_line(&mut response).unwrap();
            response
        }

        fn write_key(path: &Path, byte: u8) {
            use std::os::unix::fs::PermissionsExt;
            std::fs::write(path, [byte; 32]).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }

        fn hmac_connect_and_send(socket_path: &Path, key_path: &Path, request: &str) -> String {
            let secret = SharedSecret::load(key_path).unwrap();
            let mut stream = UnixStream::connect(socket_path).unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let session = authenticate_client(
                &mut stream,
                &secret,
                CONTROL_CLIENT_DOMAIN,
                CONTROL_SERVER_DOMAIN,
            )
            .unwrap();
            session
                .write_frame(&mut stream, FrameSender::Client, request.as_bytes())
                .unwrap();
            let response = session
                .read_frame(
                    &mut stream,
                    FrameSender::Server,
                    MAX_CONTROL_REQUEST_BYTES as usize,
                )
                .unwrap();
            String::from_utf8(response).unwrap()
        }

        #[test]
        fn hmac_server_authenticates_before_dispatch() {
            let dir = tempfile::tempdir().unwrap();
            let socket_path = dir.path().join("test.sock");
            let key_path = dir.path().join("key");
            write_key(&key_path, 7);
            let handle = ControlSocketServer::new_hmac(
                socket_path.clone(),
                &key_path,
                Some(unsafe { libc::geteuid() }),
                None,
            )
            .unwrap()
            .start()
            .unwrap();

            let response = hmac_connect_and_send(
                &socket_path,
                &key_path,
                r#"{"schemaVersion":"1","method":"ping"}"#,
            );
            let response: ControlResponse = serde_json::from_str(&response).unwrap();
            assert!(response.ok);
            assert_eq!(response.result.unwrap()["pong"], true);
            handle.shutdown();
        }

        #[test]
        fn hmac_server_rejects_plain_or_wrong_key_without_dispatch() {
            let dir = tempfile::tempdir().unwrap();
            let socket_path = dir.path().join("test.sock");
            let key_path = dir.path().join("key");
            let wrong_path = dir.path().join("wrong-key");
            write_key(&key_path, 7);
            write_key(&wrong_path, 8);
            let handle = ControlSocketServer::new_hmac(socket_path.clone(), &key_path, None, None)
                .unwrap()
                .start()
                .unwrap();

            let mut plain = UnixStream::connect(&socket_path).unwrap();
            plain
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            plain
                .write_all(b"{\"schemaVersion\":\"1\",\"method\":\"ping\"}\n")
                .unwrap();
            let mut response = String::new();
            let read = BufReader::new(&plain).read_line(&mut response).unwrap();
            assert_eq!(read, 0);
            assert!(response.is_empty());

            let wrong = SharedSecret::load(&wrong_path).unwrap();
            let mut stream = UnixStream::connect(&socket_path).unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            assert!(
                authenticate_client(
                    &mut stream,
                    &wrong,
                    CONTROL_CLIENT_DOMAIN,
                    CONTROL_SERVER_DOMAIN,
                )
                .is_err()
            );
            handle.shutdown();
        }

        #[test]
        fn server_ping_returns_pong() {
            let dir = tempfile::tempdir().unwrap();
            let socket_path = dir.path().join("test.sock");
            let config = ControlSocketConfig {
                socket_path: socket_path.clone(),
                trusted_peer: self_exe_config(),
            };
            let server = ControlSocketServer::new(config);
            let handle = server.start().unwrap();

            let resp_str =
                connect_and_send(&socket_path, r#"{"schemaVersion":"1","method":"ping"}"#);
            let resp: ControlResponse = serde_json::from_str(&resp_str).unwrap();
            assert!(resp.ok);
            assert_eq!(resp.result.unwrap()["pong"], true);

            handle.shutdown();
            assert!(
                !socket_path.exists(),
                "socket file should be cleaned up after shutdown"
            );
        }

        #[test]
        fn server_status_returns_ready() {
            let dir = tempfile::tempdir().unwrap();
            let socket_path = dir.path().join("test.sock");
            let config = ControlSocketConfig {
                socket_path: socket_path.clone(),
                trusted_peer: self_exe_config(),
            };
            let server = ControlSocketServer::new(config);
            let handle = server.start().unwrap();

            let resp_str =
                connect_and_send(&socket_path, r#"{"schemaVersion":"1","method":"status"}"#);
            let resp: ControlResponse = serde_json::from_str(&resp_str).unwrap();
            assert!(resp.ok);
            assert_eq!(resp.result.unwrap()["status"], "ready");

            handle.shutdown();
        }

        #[test]
        fn server_unknown_method_returns_error() {
            let dir = tempfile::tempdir().unwrap();
            let socket_path = dir.path().join("test.sock");
            let config = ControlSocketConfig {
                socket_path: socket_path.clone(),
                trusted_peer: self_exe_config(),
            };
            let server = ControlSocketServer::new(config);
            let handle = server.start().unwrap();

            let resp_str = connect_and_send(
                &socket_path,
                r#"{"schemaVersion":"1","method":"write_meta"}"#,
            );
            let resp: ControlResponse = serde_json::from_str(&resp_str).unwrap();
            assert!(!resp.ok);
            assert_eq!(resp.error.as_ref().unwrap().code, "unknown_method");

            handle.shutdown();
        }

        #[test]
        fn server_invalid_schema_returns_error() {
            let dir = tempfile::tempdir().unwrap();
            let socket_path = dir.path().join("test.sock");
            let config = ControlSocketConfig {
                socket_path: socket_path.clone(),
                trusted_peer: self_exe_config(),
            };
            let server = ControlSocketServer::new(config);
            let handle = server.start().unwrap();

            let resp_str =
                connect_and_send(&socket_path, r#"{"schemaVersion":"99","method":"ping"}"#);
            let resp: ControlResponse = serde_json::from_str(&resp_str).unwrap();
            assert!(!resp.ok);
            assert_eq!(
                resp.error.as_ref().unwrap().code,
                "unsupported_schema_version"
            );

            handle.shutdown();
        }

        #[test]
        fn server_invalid_json_returns_error() {
            let dir = tempfile::tempdir().unwrap();
            let socket_path = dir.path().join("test.sock");
            let config = ControlSocketConfig {
                socket_path: socket_path.clone(),
                trusted_peer: self_exe_config(),
            };
            let server = ControlSocketServer::new(config);
            let handle = server.start().unwrap();

            let resp_str = connect_and_send(&socket_path, "not json");
            let resp: ControlResponse = serde_json::from_str(&resp_str).unwrap();
            assert!(!resp.ok);
            assert_eq!(resp.error.as_ref().unwrap().code, "invalid_request");

            handle.shutdown();
        }

        #[test]
        fn server_untrusted_peer_rejected() {
            let dir = tempfile::tempdir().unwrap();
            let socket_path = dir.path().join("test.sock");
            let config = ControlSocketConfig {
                socket_path: socket_path.clone(),
                trusted_peer: TrustedPeerConfig {
                    exe_path: PathBuf::from("/nonexistent/binary"),
                    exe_file_id: FileId { dev: 0, ino: 0 },
                    uid: None,
                    gid: None,
                },
            };
            let server = ControlSocketServer::new(config);
            let handle = server.start().unwrap();

            let mut stream = UnixStream::connect(&socket_path).unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            writeln!(stream, r#"{{"schemaVersion":"1","method":"ping"}}"#).unwrap();
            stream.flush().unwrap();

            let mut reader = BufReader::new(&stream);
            let mut response = String::new();
            reader.read_line(&mut response).unwrap();

            let resp: ControlResponse = serde_json::from_str(&response).unwrap();
            assert!(!resp.ok);
            assert_eq!(resp.error.as_ref().unwrap().code, "permission_denied");

            handle.shutdown();
        }

        #[test]
        fn server_untrusted_uid_rejected() {
            let dir = tempfile::tempdir().unwrap();
            let socket_path = dir.path().join("test.sock");
            // Use the real exe but require uid=99999, which won't match.
            let mut peer_config = self_exe_config();
            peer_config.uid = Some(99999);
            let config = ControlSocketConfig {
                socket_path: socket_path.clone(),
                trusted_peer: peer_config,
            };
            let server = ControlSocketServer::new(config);
            let handle = server.start().unwrap();

            let mut stream = UnixStream::connect(&socket_path).unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            writeln!(stream, r#"{{"schemaVersion":"1","method":"ping"}}"#).unwrap();
            stream.flush().unwrap();

            let mut reader = BufReader::new(&stream);
            let mut response = String::new();
            reader.read_line(&mut response).unwrap();

            let resp: ControlResponse = serde_json::from_str(&response).unwrap();
            assert!(!resp.ok);
            assert_eq!(resp.error.as_ref().unwrap().code, "permission_denied");

            handle.shutdown();
        }

        #[test]
        fn server_handles_sequential_connections() {
            let dir = tempfile::tempdir().unwrap();
            let socket_path = dir.path().join("test.sock");
            let config = ControlSocketConfig {
                socket_path: socket_path.clone(),
                trusted_peer: self_exe_config(),
            };
            let server = ControlSocketServer::new(config);
            let handle = server.start().unwrap();

            // Each request uses its own connection (one-request-per-connection).
            let resp_str =
                connect_and_send(&socket_path, r#"{"schemaVersion":"1","method":"ping"}"#);
            let resp: ControlResponse = serde_json::from_str(&resp_str).unwrap();
            assert!(resp.ok);
            assert_eq!(resp.result.unwrap()["pong"], true);

            let resp_str =
                connect_and_send(&socket_path, r#"{"schemaVersion":"1","method":"status"}"#);
            let resp2: ControlResponse = serde_json::from_str(&resp_str).unwrap();
            assert!(resp2.ok);
            assert_eq!(resp2.result.unwrap()["status"], "ready");

            handle.shutdown();
        }

        #[test]
        fn shutdown_removes_socket_file() {
            let dir = tempfile::tempdir().unwrap();
            let socket_path = dir.path().join("test.sock");
            let config = ControlSocketConfig {
                socket_path: socket_path.clone(),
                trusted_peer: self_exe_config(),
            };
            let server = ControlSocketServer::new(config);
            let handle = server.start().unwrap();
            assert!(socket_path.exists(), "socket file must exist while running");
            handle.shutdown();
            assert!(
                !socket_path.exists(),
                "socket file must be removed after shutdown"
            );
        }

        #[test]
        fn drop_removes_socket_file() {
            let dir = tempfile::tempdir().unwrap();
            let socket_path = dir.path().join("test.sock");
            let config = ControlSocketConfig {
                socket_path: socket_path.clone(),
                trusted_peer: self_exe_config(),
            };
            let server = ControlSocketServer::new(config);
            let handle = server.start().unwrap();
            assert!(socket_path.exists());
            drop(handle);
            assert!(
                !socket_path.exists(),
                "socket file must be removed after drop"
            );
        }

        #[test]
        fn socket_permissions_are_0600() {
            let dir = tempfile::tempdir().unwrap();
            let socket_path = dir.path().join("test.sock");
            let config = ControlSocketConfig {
                socket_path: socket_path.clone(),
                trusted_peer: self_exe_config(),
            };
            let server = ControlSocketServer::new(config);
            let handle = server.start().unwrap();

            let meta = std::fs::metadata(&socket_path).unwrap();
            use std::os::unix::fs::PermissionsExt;
            let mode = meta.permissions().mode() & 0o777;
            assert_eq!(
                mode, 0o600,
                "socket file permissions must be 0600, got {mode:o}"
            );

            handle.shutdown();
        }

        #[test]
        fn socket_parent_permissions_are_0700() {
            let dir = tempfile::tempdir().unwrap();
            let subdir = dir.path().join("sock-parent");
            let socket_path = subdir.join("test.sock");
            let config = ControlSocketConfig {
                socket_path: socket_path.clone(),
                trusted_peer: self_exe_config(),
            };
            let server = ControlSocketServer::new(config);
            let handle = server.start().unwrap();

            use std::os::unix::fs::PermissionsExt;
            let parent_meta = std::fs::metadata(&subdir).unwrap();
            let parent_mode = parent_meta.permissions().mode() & 0o777;
            assert_eq!(
                parent_mode, 0o700,
                "socket parent directory must be 0700, got {parent_mode:o}"
            );

            let sock_meta = std::fs::metadata(&socket_path).unwrap();
            let sock_mode = sock_meta.permissions().mode() & 0o777;
            assert_eq!(
                sock_mode, 0o600,
                "socket file must still be 0600, got {sock_mode:o}"
            );

            handle.shutdown();
        }

        #[test]
        fn socket_parent_existing_gets_tightened_to_0700() {
            let dir = tempfile::tempdir().unwrap();
            let subdir = dir.path().join("loose-parent");
            std::fs::create_dir(&subdir).unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&subdir, std::fs::Permissions::from_mode(0o755)).unwrap();

            let socket_path = subdir.join("test.sock");
            let config = ControlSocketConfig {
                socket_path: socket_path.clone(),
                trusted_peer: self_exe_config(),
            };
            let server = ControlSocketServer::new(config);
            let handle = server.start().unwrap();

            let parent_meta = std::fs::metadata(&subdir).unwrap();
            let parent_mode = parent_meta.permissions().mode() & 0o777;
            assert_eq!(
                parent_mode, 0o700,
                "existing parent must be tightened to 0700, got {parent_mode:o}"
            );

            handle.shutdown();
        }

        // ── Request size limit ────────────────────────────────────────────

        #[test]
        fn normal_request_accepted() {
            let dir = tempfile::tempdir().unwrap();
            let socket_path = dir.path().join("test.sock");
            let config = ControlSocketConfig {
                socket_path: socket_path.clone(),
                trusted_peer: self_exe_config(),
            };
            let handle = ControlSocketServer::new(config).start().unwrap();
            std::thread::sleep(std::time::Duration::from_millis(50));

            let stream = UnixStream::connect(&socket_path).unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let req = r#"{"schemaVersion":"1","method":"ping"}"#;
            writeln!(&stream, "{req}").unwrap();
            (&stream).flush().unwrap();

            let mut reader = BufReader::new(&stream);
            let mut response = String::new();
            reader.read_line(&mut response).unwrap();
            assert!(
                response.contains("\"ok\":true"),
                "normal request must be accepted: {response}"
            );

            handle.shutdown();
        }

        #[test]
        fn oversized_request_rejected() {
            let dir = tempfile::tempdir().unwrap();
            let socket_path = dir.path().join("test.sock");
            let config = ControlSocketConfig {
                socket_path: socket_path.clone(),
                trusted_peer: self_exe_config(),
            };
            let handle = ControlSocketServer::new(config).start().unwrap();
            std::thread::sleep(std::time::Duration::from_millis(50));

            let stream = UnixStream::connect(&socket_path).unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            // Write >64KB without a newline — should be rejected.
            let payload = vec![b'A'; (MAX_CONTROL_REQUEST_BYTES as usize) + 100];
            (&stream).write_all(&payload).unwrap();
            (&stream).write_all(b"\n").unwrap();
            (&stream).flush().unwrap();

            let mut reader = BufReader::new(&stream);
            let mut response = String::new();
            reader.read_line(&mut response).unwrap();
            assert!(
                response.contains("request exceeds") || response.contains("invalid_request"),
                "oversized request must be rejected: {response}"
            );

            handle.shutdown();
        }

        // ── Meta write integration (through socket) ─────────────────────

        fn start_server_with_context(
            dir: &Path,
            source_root: &Path,
        ) -> (PathBuf, ControlSocketHandle) {
            start_server_with_context_and_layout(dir, source_root, SkillLayout::Flat)
        }

        fn start_server_with_context_and_layout(
            dir: &Path,
            source_root: &Path,
            layout: SkillLayout,
        ) -> (PathBuf, ControlSocketHandle) {
            let socket_path = dir.join("test.sock");
            let resolver = Arc::new(ActiveSkillResolver::new(source_root));
            let writer =
                Arc::new(super::super::super::protocol_events::InMemoryProtocolEventWriter::new());
            let ctx = ControlSocketContext {
                canonical_root: source_root.to_path_buf(),
                source_root: source_root.to_path_buf(),
                layout,
                resolver: Some(resolver),
                protocol_event_writer: Some(writer),
            };
            let config = ControlSocketConfig {
                socket_path: socket_path.clone(),
                trusted_peer: self_exe_config(),
            };
            let server = ControlSocketServer::new(config).with_context(ctx);
            let handle = server.start().unwrap();
            (socket_path, handle)
        }

        #[test]
        fn server_writes_hermes_nested_activation() {
            let dir = tempfile::tempdir().unwrap();
            let source = tempfile::tempdir().unwrap();
            let skill_dir = source.path().join("apple/apple-notes");
            std::fs::create_dir_all(&skill_dir).unwrap();
            std::fs::write(skill_dir.join("SKILL.md"), "# Apple Notes\n").unwrap();
            let (socket_path, handle) = start_server_with_context_and_layout(
                dir.path(),
                source.path(),
                SkillLayout::Hermes,
            );
            let req = serde_json::json!({
                "schemaVersion": "1",
                "method": "meta.writeActivation",
                "skillName": "apple/apple-notes",
                "activation": {"schemaVersion": 1, "target": null}
            });

            let response = connect_and_send(&socket_path, &req.to_string());
            let response: ControlResponse = serde_json::from_str(&response).unwrap();

            assert!(response.ok, "expected ok, got: {response:?}");
            assert_eq!(
                response
                    .result
                    .as_ref()
                    .and_then(|result| result["outcome"].as_str()),
                Some("updated")
            );
            assert!(skill_dir.join(".skill-meta/activation.json").is_file());
            handle.shutdown();
        }

        #[test]
        fn server_meta_write_activation_writes_file() {
            let dir = tempfile::tempdir().unwrap();
            let source = tempfile::tempdir().unwrap();
            seed_skill_dir(source.path(), "demo-weather");
            let skill_dir = source.path().join("demo-weather");

            let (socket_path, handle) = start_server_with_context(dir.path(), source.path());

            let req = serde_json::json!({
                "schemaVersion": "1",
                "method": "meta.writeActivation",
                "skillName": "demo-weather",
                "activation": {"schemaVersion": 1, "target": null}
            });
            let resp_str = connect_and_send(&socket_path, &req.to_string());
            let resp: ControlResponse = serde_json::from_str(&resp_str).unwrap();
            assert!(resp.ok, "expected ok, got: {resp:?}");

            let written = std::fs::read_to_string(skill_dir.join(".skill-meta/activation.json"))
                .expect("activation.json should exist");
            let parsed: serde_json::Value = serde_json::from_str(&written).unwrap();
            assert_eq!(parsed["schemaVersion"], 1);
            assert!(parsed["target"].is_null());

            handle.shutdown();
        }

        #[test]
        fn server_meta_write_activation_snapshot_target() {
            let dir = tempfile::tempdir().unwrap();
            let source = tempfile::tempdir().unwrap();
            let skill_dir = source.path().join("demo-weather");
            std::fs::create_dir_all(skill_dir.join(".skill-meta/versions/v000001.snapshot"))
                .unwrap();
            std::fs::write(skill_dir.join("SKILL.md"), "# Skill\n").unwrap();

            let (socket_path, handle) = start_server_with_context(dir.path(), source.path());

            let req = serde_json::json!({
                "schemaVersion": "1",
                "method": "meta.writeActivation",
                "skillName": "demo-weather",
                "activation": {
                    "schemaVersion": 1,
                    "target": ".skill-meta/versions/v000001.snapshot"
                }
            });
            let resp_str = connect_and_send(&socket_path, &req.to_string());
            let resp: ControlResponse = serde_json::from_str(&resp_str).unwrap();
            assert!(resp.ok, "expected ok, got: {resp:?}");

            let result = resp.result.unwrap();
            let outcome = result["outcome"].as_str().unwrap();
            assert!(
                outcome == "updated" || outcome == "unchanged",
                "expected updated or unchanged, got {outcome}"
            );

            handle.shutdown();
        }

        #[test]
        fn server_meta_write_activation_untrusted_peer_rejected() {
            let dir = tempfile::tempdir().unwrap();
            let source = tempfile::tempdir().unwrap();
            let skill_dir = source.path().join("demo-weather");
            std::fs::create_dir(&skill_dir).unwrap();

            let socket_path = dir.path().join("test.sock");
            let ctx = ControlSocketContext {
                canonical_root: source.path().to_path_buf(),
                source_root: source.path().to_path_buf(),
                layout: SkillLayout::Flat,
                resolver: None,
                protocol_event_writer: None,
            };
            let config = ControlSocketConfig {
                socket_path: socket_path.clone(),
                trusted_peer: TrustedPeerConfig {
                    exe_path: PathBuf::from("/nonexistent/binary"),
                    exe_file_id: FileId { dev: 0, ino: 0 },
                    uid: None,
                    gid: None,
                },
            };
            let server = ControlSocketServer::new(config).with_context(ctx);
            let handle = server.start().unwrap();

            let req = serde_json::json!({
                "schemaVersion": "1",
                "method": "meta.writeActivation",
                "skillName": "demo-weather",
                "activation": {"schemaVersion": 1, "target": null}
            });

            let mut stream = UnixStream::connect(&socket_path).unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            // The server authenticates the peer before reading the
            // request. An untrusted peer may therefore receive the
            // `permission_denied` response and close before this write
            // completes, so BrokenPipe is an acceptable race here.
            let _ = writeln!(stream, "{req}");
            let _ = stream.flush();

            let mut reader = BufReader::new(&stream);
            let mut response = String::new();
            reader.read_line(&mut response).unwrap();

            let resp: ControlResponse = serde_json::from_str(&response).unwrap();
            assert!(!resp.ok);
            assert_eq!(resp.error.as_ref().unwrap().code, "permission_denied");

            // Verify no file was written.
            assert!(
                !skill_dir.join(".skill-meta/activation.json").exists(),
                "rejected peer must not write activation.json"
            );

            handle.shutdown();
        }

        #[test]
        fn server_meta_write_activation_invalid_skill_name_rejected() {
            let dir = tempfile::tempdir().unwrap();
            let source = tempfile::tempdir().unwrap();

            let (socket_path, handle) = start_server_with_context(dir.path(), source.path());

            let req = serde_json::json!({
                "schemaVersion": "1",
                "method": "meta.writeActivation",
                "skillName": "../escape",
                "activation": {"schemaVersion": 1, "target": null}
            });
            let resp_str = connect_and_send(&socket_path, &req.to_string());
            let resp: ControlResponse = serde_json::from_str(&resp_str).unwrap();
            assert!(!resp.ok);
            assert_eq!(resp.error.as_ref().unwrap().code, "invalid_skill_name");

            handle.shutdown();
        }

        #[test]
        fn server_meta_write_activation_nonexistent_skill_rejected() {
            let dir = tempfile::tempdir().unwrap();
            let source = tempfile::tempdir().unwrap();

            let (socket_path, handle) = start_server_with_context(dir.path(), source.path());

            let req = serde_json::json!({
                "schemaVersion": "1",
                "method": "meta.writeActivation",
                "skillName": "nonexistent",
                "activation": {"schemaVersion": 1, "target": null}
            });
            let resp_str = connect_and_send(&socket_path, &req.to_string());
            let resp: ControlResponse = serde_json::from_str(&resp_str).unwrap();
            assert!(!resp.ok);
            assert_eq!(resp.error.as_ref().unwrap().code, "skill_not_found");

            handle.shutdown();
        }

        #[test]
        fn server_meta_write_activation_malformed_activation_rejected() {
            let dir = tempfile::tempdir().unwrap();
            let source = tempfile::tempdir().unwrap();
            let skill_dir = source.path().join("alpha");
            std::fs::create_dir(&skill_dir).unwrap();

            let (socket_path, handle) = start_server_with_context(dir.path(), source.path());

            let req = serde_json::json!({
                "schemaVersion": "1",
                "method": "meta.writeActivation",
                "skillName": "alpha",
                "activation": {"schemaVersion": 99, "target": null}
            });
            let resp_str = connect_and_send(&socket_path, &req.to_string());
            let resp: ControlResponse = serde_json::from_str(&resp_str).unwrap();
            assert!(!resp.ok);
            assert_eq!(resp.error.as_ref().unwrap().code, "invalid_activation");

            // Verify no file was written.
            assert!(
                !skill_dir.join(".skill-meta/activation.json").exists(),
                "rejected activation must not write to disk"
            );

            handle.shutdown();
        }

        #[test]
        fn server_meta_write_activation_no_partial_json() {
            let dir = tempfile::tempdir().unwrap();
            let source = tempfile::tempdir().unwrap();
            seed_skill_dir(source.path(), "alpha");
            let skill_dir = source.path().join("alpha");

            let (socket_path, handle) = start_server_with_context(dir.path(), source.path());

            let req = serde_json::json!({
                "schemaVersion": "1",
                "method": "meta.writeActivation",
                "skillName": "alpha",
                "activation": {"schemaVersion": 1, "target": null}
            });
            let resp_str = connect_and_send(&socket_path, &req.to_string());
            let resp: ControlResponse = serde_json::from_str(&resp_str).unwrap();
            assert!(resp.ok);

            // Read the file and verify it's valid JSON.
            let content =
                std::fs::read_to_string(skill_dir.join(".skill-meta/activation.json")).unwrap();
            let parsed: serde_json::Value =
                serde_json::from_str(&content).expect("activation.json must be valid JSON");
            assert_eq!(parsed["schemaVersion"], 1);

            // No temp files should remain.
            let meta_dir = skill_dir.join(".skill-meta");
            for entry in std::fs::read_dir(&meta_dir).unwrap() {
                let entry = entry.unwrap();
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                assert!(
                    !name_str.starts_with("activation.tmp."),
                    "temp file should have been cleaned up: {name_str}"
                );
            }

            handle.shutdown();
        }

        #[test]
        fn server_meta_write_without_trusted_writer_exe_works() {
            let dir = tempfile::tempdir().unwrap();
            let source = tempfile::tempdir().unwrap();
            seed_skill_dir(source.path(), "alpha");

            let (socket_path, handle) = start_server_with_context(dir.path(), source.path());

            let req = serde_json::json!({
                "schemaVersion": "1",
                "method": "meta.writeActivation",
                "skillName": "alpha",
                "activation": {"schemaVersion": 1, "target": null}
            });
            let resp_str = connect_and_send(&socket_path, &req.to_string());
            let resp: ControlResponse = serde_json::from_str(&resp_str).unwrap();
            assert!(
                resp.ok,
                "control socket write should work without --trusted-writer-exe"
            );

            handle.shutdown();
        }

        #[test]
        fn server_meta_set_xattr_writes_xattr() {
            let dir = tempfile::tempdir().unwrap();

            // Find an xattr-capable tempdir for the source.
            let source = match xattr_capable_tempdir_for_meta() {
                Some(d) => d,
                None => {
                    eprintln!("SKIP: no xattr-capable filesystem for meta.setActivationXattr test");
                    return;
                }
            };
            let skill_dir = source.path().join("alpha");
            std::fs::create_dir(&skill_dir).unwrap();
            std::fs::write(skill_dir.join("SKILL.md"), "# Skill\n").unwrap();

            let (socket_path, handle) = start_server_with_context(dir.path(), source.path());

            let req = serde_json::json!({
                "schemaVersion": "1",
                "method": "meta.setActivationXattr",
                "skillName": "alpha",
                "activation": {"schemaVersion": 1, "target": null}
            });
            let resp_str = connect_and_send(&socket_path, &req.to_string());
            let resp: ControlResponse = serde_json::from_str(&resp_str).unwrap();
            assert!(resp.ok, "expected ok, got: {resp:?}");

            // Verify the xattr was set.
            let xattr_outcome = super::super::super::activation::read_activation_xattr(&skill_dir);
            match xattr_outcome {
                super::super::super::activation::XattrReadOutcome::Present(s) => {
                    let parsed: serde_json::Value = serde_json::from_str(&s).unwrap();
                    assert_eq!(parsed["schemaVersion"], 1);
                    assert!(parsed["target"].is_null());
                }
                other => panic!("expected xattr Present, got {other:?}"),
            }

            handle.shutdown();
        }

        #[test]
        fn server_meta_set_xattr_untrusted_peer_no_disk_change() {
            let dir = tempfile::tempdir().unwrap();
            let source = match xattr_capable_tempdir_for_meta() {
                Some(d) => d,
                None => {
                    eprintln!("SKIP: no xattr-capable filesystem for untrusted xattr test");
                    return;
                }
            };
            let skill_dir = source.path().join("alpha");
            std::fs::create_dir(&skill_dir).unwrap();

            let socket_path = dir.path().join("test.sock");
            let ctx = ControlSocketContext {
                canonical_root: source.path().to_path_buf(),
                source_root: source.path().to_path_buf(),
                layout: SkillLayout::Flat,
                resolver: None,
                protocol_event_writer: None,
            };
            let config = ControlSocketConfig {
                socket_path: socket_path.clone(),
                trusted_peer: TrustedPeerConfig {
                    exe_path: PathBuf::from("/nonexistent/binary"),
                    exe_file_id: FileId { dev: 0, ino: 0 },
                    uid: None,
                    gid: None,
                },
            };
            let server = ControlSocketServer::new(config).with_context(ctx);
            let handle = server.start().unwrap();

            let req = serde_json::json!({
                "schemaVersion": "1",
                "method": "meta.setActivationXattr",
                "skillName": "alpha",
                "activation": {"schemaVersion": 1, "target": null}
            });

            let mut stream = UnixStream::connect(&socket_path).unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            // The server authenticates the peer before reading the
            // request. An untrusted peer may therefore receive the
            // `permission_denied` response and close before this write
            // completes, so BrokenPipe is an acceptable race here.
            let _ = writeln!(stream, "{req}");
            let _ = stream.flush();

            let mut reader = BufReader::new(&stream);
            let mut response = String::new();
            reader.read_line(&mut response).unwrap();

            let resp: ControlResponse = serde_json::from_str(&response).unwrap();
            assert!(!resp.ok);
            assert_eq!(resp.error.as_ref().unwrap().code, "permission_denied");

            // Verify xattr was not set.
            let xattr_outcome = super::super::super::activation::read_activation_xattr(&skill_dir);
            assert!(
                !matches!(
                    xattr_outcome,
                    super::super::super::activation::XattrReadOutcome::Present(_)
                ),
                "rejected peer must not write xattr"
            );

            handle.shutdown();
        }

        fn xattr_capable_tempdir_for_meta() -> Option<tempfile::TempDir> {
            let mut candidates: Vec<PathBuf> = Vec::new();
            if let Ok(env_path) = std::env::var("SKILLFS_XATTR_TEST_ROOT") {
                if !env_path.is_empty() {
                    candidates.push(PathBuf::from(env_path));
                }
            }
            let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            for ancestor in manifest_dir.ancestors() {
                if ancestor.join("Cargo.lock").exists() {
                    candidates.push(ancestor.join("target").join("xattr-tests"));
                    break;
                }
            }
            if let Some(home) = std::env::var_os("HOME") {
                let mut path = PathBuf::from(home);
                path.push(".cache");
                path.push("skillfs-xattr-tests");
                candidates.push(path);
            }

            for cand in candidates {
                if std::fs::create_dir_all(&cand).is_err() {
                    continue;
                }
                let td = match tempfile::Builder::new()
                    .prefix("c1-meta-")
                    .tempdir_in(&cand)
                {
                    Ok(d) => d,
                    Err(_) => continue,
                };
                if user_xattr_supported_meta(td.path()) {
                    return Some(td);
                }
            }
            None
        }

        fn user_xattr_supported_meta(dir: &Path) -> bool {
            use std::ffi::CString;
            use std::os::unix::ffi::OsStrExt;
            let c_path = match CString::new(dir.as_os_str().as_bytes()) {
                Ok(c) => c,
                Err(_) => return false,
            };
            let c_name = match CString::new("user.skillfs.probe") {
                Ok(c) => c,
                Err(_) => return false,
            };
            let rc = unsafe {
                libc::lsetxattr(
                    c_path.as_ptr(),
                    c_name.as_ptr(),
                    b"1".as_ptr() as *const libc::c_void,
                    1,
                    0,
                )
            };
            if rc != 0 {
                return false;
            }
            unsafe {
                libc::lremovexattr(c_path.as_ptr(), c_name.as_ptr());
            }
            true
        }

        // ── skill.resolveLiveSource over the socket ──────────────────

        fn seed_skill_dir(root: &Path, rel: &str) {
            let dir = root.join(rel);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("SKILL.md"), "---\nname: x\ndescription: y\n---\n").unwrap();
        }

        #[test]
        fn server_resolve_live_source_managed() {
            // start_server_with_context uses the default (Flat) layout, so
            // seed a flat skill. Nested/Hermes boundary behavior is covered
            // by the resolver module's own unit tests.
            let dir = tempfile::tempdir().unwrap();
            let source = tempfile::tempdir().unwrap();
            seed_skill_dir(source.path(), "my-skill");
            let (socket_path, handle) = start_server_with_context(dir.path(), source.path());

            let canonical = source.path().join("my-skill");
            let req = serde_json::json!({
                "schemaVersion": "1",
                "method": "skill.resolveLiveSource",
                "canonicalSkillDir": canonical.to_string_lossy(),
            });
            let resp_str = connect_and_send(&socket_path, &req.to_string());
            let resp: ControlResponse = serde_json::from_str(&resp_str).unwrap();
            assert!(resp.ok, "expected ok, got {resp:?}");
            let r = resp.result.unwrap();
            assert_eq!(r["managed"], true);
            assert_eq!(r["skillId"], "my-skill");
            assert_eq!(r["transport"], "shared_path");

            handle.shutdown();
        }

        #[test]
        fn server_resolve_live_source_not_managed() {
            let dir = tempfile::tempdir().unwrap();
            let source = tempfile::tempdir().unwrap();
            let other = tempfile::tempdir().unwrap();
            let (socket_path, handle) = start_server_with_context(dir.path(), source.path());

            let req = serde_json::json!({
                "schemaVersion": "1",
                "method": "skill.resolveLiveSource",
                "canonicalSkillDir": other.path().join("x").to_string_lossy(),
            });
            let resp_str = connect_and_send(&socket_path, &req.to_string());
            let resp: ControlResponse = serde_json::from_str(&resp_str).unwrap();
            assert!(resp.ok);
            let r = resp.result.unwrap();
            assert_eq!(r["managed"], false);
            assert_eq!(r["reason"], "not_managed");

            handle.shutdown();
        }

        #[test]
        fn server_resolve_live_source_relative_path_error() {
            let dir = tempfile::tempdir().unwrap();
            let source = tempfile::tempdir().unwrap();
            let (socket_path, handle) = start_server_with_context(dir.path(), source.path());

            let req = serde_json::json!({
                "schemaVersion": "1",
                "method": "skill.resolveLiveSource",
                "canonicalSkillDir": "relative/path",
            });
            let resp_str = connect_and_send(&socket_path, &req.to_string());
            let resp: ControlResponse = serde_json::from_str(&resp_str).unwrap();
            assert!(!resp.ok);
            assert_eq!(resp.error.unwrap().code, "invalid_canonical_path");

            handle.shutdown();
        }

        #[test]
        fn server_resolve_live_source_non_normalized_path_error() {
            let dir = tempfile::tempdir().unwrap();
            let source = tempfile::tempdir().unwrap();
            let (socket_path, handle) = start_server_with_context(dir.path(), source.path());

            for canonical_skill_dir in [
                format!("{}//my-skill", source.path().display()),
                format!("//{}/my-skill", source.path().display()),
                format!("{}/my-skill/", source.path().display()),
            ] {
                let req = serde_json::json!({
                    "schemaVersion": "1",
                    "method": "skill.resolveLiveSource",
                    "canonicalSkillDir": canonical_skill_dir,
                });
                let resp_str = connect_and_send(&socket_path, &req.to_string());
                let resp: ControlResponse = serde_json::from_str(&resp_str).unwrap();
                assert!(!resp.ok);
                assert_eq!(resp.error.unwrap().code, "invalid_canonical_path");
            }

            handle.shutdown();
        }

        #[test]
        fn server_resolve_live_source_untrusted_peer_rejected() {
            let dir = tempfile::tempdir().unwrap();
            let source = tempfile::tempdir().unwrap();
            seed_skill_dir(source.path(), "my-skill");

            let socket_path = dir.path().join("test.sock");
            let ctx = ControlSocketContext {
                canonical_root: source.path().to_path_buf(),
                source_root: source.path().to_path_buf(),
                layout: SkillLayout::Flat,
                resolver: None,
                protocol_event_writer: None,
            };
            let config = ControlSocketConfig {
                socket_path: socket_path.clone(),
                trusted_peer: TrustedPeerConfig {
                    exe_path: PathBuf::from("/nonexistent/binary"),
                    exe_file_id: FileId { dev: 0, ino: 0 },
                    uid: None,
                    gid: None,
                },
            };
            let handle = ControlSocketServer::new(config)
                .with_context(ctx)
                .start()
                .unwrap();

            let req = serde_json::json!({
                "schemaVersion": "1",
                "method": "skill.resolveLiveSource",
                "canonicalSkillDir": source.path().join("my-skill").to_string_lossy(),
            });
            let mut stream = UnixStream::connect(&socket_path).unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let _ = writeln!(stream, "{req}");
            let _ = stream.flush();
            let mut reader = BufReader::new(&stream);
            let mut response = String::new();
            reader.read_line(&mut response).unwrap();
            let resp: ControlResponse = serde_json::from_str(&response).unwrap();
            assert!(!resp.ok);
            assert_eq!(resp.error.unwrap().code, "permission_denied");

            handle.shutdown();
        }

        #[test]
        fn burst_mixed_queries_independent_and_deadlock_free() {
            let dir = tempfile::tempdir().unwrap();
            let source = tempfile::tempdir().unwrap();
            seed_skill_dir(source.path(), "my-skill");
            let (socket_path, handle) = start_server_with_context(dir.path(), source.path());

            let managed_path = source.path().join("my-skill");
            let outside = tempfile::tempdir().unwrap();
            let not_managed_path = outside.path().join("x");

            let mut managed_ok = 0;
            let mut not_managed_ok = 0;
            let mut invalid_ok = 0;

            const ITERATIONS: usize = 120;
            for i in 0..ITERATIONS {
                let (req, expect): (serde_json::Value, &str) = match i % 3 {
                    0 => (
                        serde_json::json!({
                            "schemaVersion": "1",
                            "method": "skill.resolveLiveSource",
                            "canonicalSkillDir": managed_path.to_string_lossy(),
                        }),
                        "managed",
                    ),
                    1 => (
                        serde_json::json!({
                            "schemaVersion": "1",
                            "method": "skill.resolveLiveSource",
                            "canonicalSkillDir": not_managed_path.to_string_lossy(),
                        }),
                        "not_managed",
                    ),
                    _ => (
                        // Malformed: relative canonicalSkillDir.
                        serde_json::json!({
                            "schemaVersion": "1",
                            "method": "skill.resolveLiveSource",
                            "canonicalSkillDir": "relative/path",
                        }),
                        "invalid",
                    ),
                };

                let resp_str = connect_and_send(&socket_path, &req.to_string());
                // Each response is a single, complete, independent JSON line.
                let resp: ControlResponse = serde_json::from_str(&resp_str)
                    .unwrap_or_else(|e| panic!("iteration {i}: bad response '{resp_str}': {e}"));

                match expect {
                    "managed" => {
                        assert!(resp.ok, "iteration {i}: {resp:?}");
                        let r = resp.result.unwrap();
                        assert_eq!(r["managed"], true);
                        assert_eq!(r["skillId"], "my-skill");
                        managed_ok += 1;
                    }
                    "not_managed" => {
                        assert!(resp.ok);
                        assert_eq!(resp.result.unwrap()["managed"], false);
                        not_managed_ok += 1;
                    }
                    _ => {
                        assert!(!resp.ok);
                        assert_eq!(resp.error.unwrap().code, "invalid_canonical_path");
                        invalid_ok += 1;
                    }
                }
            }

            assert_eq!(managed_ok + not_managed_ok + invalid_ok, ITERATIONS);
            assert!(managed_ok >= 40 && not_managed_ok >= 40 && invalid_ok >= 40);

            handle.shutdown();
        }

        // ── Socket lifecycle ─────────────────────────────────────────

        #[test]
        fn second_instance_fails_while_socket_active() {
            let dir = tempfile::tempdir().unwrap();
            let socket_path = dir.path().join("test.sock");
            let handle1 = ControlSocketServer::new(ControlSocketConfig {
                socket_path: socket_path.clone(),
                trusted_peer: self_exe_config(),
            })
            .start()
            .unwrap();

            // A second instance must fail (lifecycle lock held) and must
            // NOT unlink the active socket.
            let result2 = ControlSocketServer::new(ControlSocketConfig {
                socket_path: socket_path.clone(),
                trusted_peer: self_exe_config(),
            })
            .start();
            assert!(result2.is_err(), "second instance must fail to start");
            assert!(socket_path.exists(), "active socket must not be removed");

            // The first instance is still serving.
            let resp_str =
                connect_and_send(&socket_path, r#"{"schemaVersion":"1","method":"ping"}"#);
            assert!(resp_str.contains("\"ok\":true"));

            handle1.shutdown();
        }

        #[test]
        fn lifecycle_lock_does_not_block_unbounded() {
            let dir = tempfile::tempdir().unwrap();
            let socket_path = dir.path().join("test.sock");
            let handle1 = ControlSocketServer::new(ControlSocketConfig {
                socket_path: socket_path.clone(),
                trusted_peer: self_exe_config(),
            })
            .start()
            .unwrap();

            let start = std::time::Instant::now();
            let result2 = ControlSocketServer::new(ControlSocketConfig {
                socket_path: socket_path.clone(),
                trusted_peer: self_exe_config(),
            })
            .start();
            let elapsed = start.elapsed();
            assert!(result2.is_err());
            assert!(
                elapsed < std::time::Duration::from_secs(1),
                "non-blocking lock must fail fast, took {elapsed:?}"
            );

            handle1.shutdown();
        }

        #[test]
        fn stale_socket_is_recovered() {
            let dir = tempfile::tempdir().unwrap();
            let socket_path = dir.path().join("test.sock");
            // Leave a stale socket file behind (std does not unlink on drop)
            // and recover it immediately — no pre-settling. This exercises
            // the real "listener just closed, restart now" path, where the
            // bounded-retry preflight probe must let the endpoint settle to
            // ECONNREFUSED and reclaim it rather than misreading a teardown
            // transient as a live listener.
            {
                let _l = UnixListener::bind(&socket_path).unwrap();
            }
            assert!(socket_path.exists());

            let handle = ControlSocketServer::new(ControlSocketConfig {
                socket_path: socket_path.clone(),
                trusted_peer: self_exe_config(),
            })
            .start()
            .expect("stale socket must be recoverable");

            let resp_str =
                connect_and_send(&socket_path, r#"{"schemaVersion":"1","method":"ping"}"#);
            assert!(resp_str.contains("\"ok\":true"));

            handle.shutdown();
        }

        #[test]
        fn listener_setup_failure_cleans_socket_and_releases_lock() {
            let dir = tempfile::tempdir().unwrap();
            let socket_path = dir.path().join("test.sock");
            let result = ControlSocketServer::new(ControlSocketConfig {
                socket_path: socket_path.clone(),
                trusted_peer: self_exe_config(),
            })
            .start_with_listener_setup(|_| {
                Err(std::io::Error::other("injected listener setup failure"))
            });

            assert!(result.is_err());
            assert!(
                !socket_path.exists(),
                "failed startup must remove its socket"
            );

            let handle = ControlSocketServer::new(ControlSocketConfig {
                socket_path: socket_path.clone(),
                trusted_peer: self_exe_config(),
            })
            .start()
            .expect("failed startup must release the lifecycle lock");
            handle.shutdown();
        }

        #[test]
        fn start_refuses_symlink_at_socket_path() {
            let dir = tempfile::tempdir().unwrap();
            let target = dir.path().join("target");
            std::fs::write(&target, "x").unwrap();
            let socket_path = dir.path().join("test.sock");
            std::os::unix::fs::symlink(&target, &socket_path).unwrap();

            let result = ControlSocketServer::new(ControlSocketConfig {
                socket_path: socket_path.clone(),
                trusted_peer: self_exe_config(),
            })
            .start();
            assert!(result.is_err(), "symlink at socket path must fail closed");
            assert!(
                std::fs::symlink_metadata(&socket_path)
                    .unwrap()
                    .file_type()
                    .is_symlink(),
                "symlink must not be deleted"
            );
        }

        #[test]
        fn start_refuses_regular_file_at_socket_path() {
            let dir = tempfile::tempdir().unwrap();
            let socket_path = dir.path().join("test.sock");
            std::fs::write(&socket_path, "not a socket").unwrap();

            let result = ControlSocketServer::new(ControlSocketConfig {
                socket_path: socket_path.clone(),
                trusted_peer: self_exe_config(),
            })
            .start();
            assert!(result.is_err(), "regular file at socket path must fail");
            assert!(socket_path.exists(), "regular file must not be deleted");
            assert_eq!(
                std::fs::read_to_string(&socket_path).unwrap(),
                "not a socket"
            );
        }

        #[test]
        fn shutdown_does_not_delete_replaced_path() {
            let dir = tempfile::tempdir().unwrap();
            let socket_path = dir.path().join("test.sock");
            let handle = ControlSocketServer::new(ControlSocketConfig {
                socket_path: socket_path.clone(),
                trusted_peer: self_exe_config(),
            })
            .start()
            .unwrap();

            // Replace the socket with a different object after bind.
            std::fs::remove_file(&socket_path).unwrap();
            std::fs::write(&socket_path, "replacement").unwrap();

            handle.shutdown();

            // The replacement object must survive: shutdown only removes the
            // exact socket identity this instance bound.
            assert!(
                socket_path.exists(),
                "replacement object must not be deleted"
            );
            assert_eq!(
                std::fs::read_to_string(&socket_path).unwrap(),
                "replacement"
            );
        }

        #[test]
        fn socket_and_parent_permissions() {
            use std::os::unix::fs::PermissionsExt;
            let dir = tempfile::tempdir().unwrap();
            let parent = dir.path().join("sock-parent");
            let socket_path = parent.join("test.sock");
            let handle = ControlSocketServer::new(ControlSocketConfig {
                socket_path: socket_path.clone(),
                trusted_peer: self_exe_config(),
            })
            .start()
            .unwrap();

            let parent_mode = std::fs::metadata(&parent).unwrap().permissions().mode() & 0o777;
            assert_eq!(parent_mode, 0o700, "parent must be 0700");
            let sock_mode = std::fs::metadata(&socket_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(sock_mode, 0o600, "socket must be 0600");

            handle.shutdown();
        }
    }
}
