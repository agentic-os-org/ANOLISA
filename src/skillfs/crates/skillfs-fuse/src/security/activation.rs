//! A1: Activation File Consumer.
//!
//! Consumes `<skill_dir>/.skill-meta/activation.json` and translates the
//! payload into [`ActiveTarget::Snapshot`] or [`ActiveTarget::Hidden`].
//!
//! The activation file is the runtime contract between SkillFS and an
//! external security daemon: the daemon writes the file; SkillFS reads
//! it at startup (and, in later packages, on explicit refresh).
//!
//! Validation rules (intentionally strict):
//!
//! * `schemaVersion` must be exactly `1`.
//! * `target = null` maps to [`ActiveTarget::Hidden`].
//! * Non-null `target` must be a relative path under
//!   `.skill-meta/versions/<version>.snapshot`.
//! * Absolute paths, empty strings, `..` traversal, non-`.snapshot`
//!   suffixes, foreign roots, and malformed JSON are rejected.
//! * The resolved snapshot directory must exist and must stay within the
//!   owning `skill_dir`.
//! * Any validation failure maps to hidden with a diagnostic error.

use std::ffi::CString;
use std::path::{Component, Path, PathBuf};

use super::active::ActiveTarget;

/// The only `schemaVersion` accepted by A1.
pub const ACTIVATION_SCHEMA_VERSION: u64 = 1;

/// Relative path from `skill_dir` to the activation file.
pub const ACTIVATION_FILE: &str = ".skill-meta/activation.json";

/// Extended attribute name for the activation record on `skill_dir`.
///
/// A2: the external daemon writes this xattr on the skill directory with
/// the same JSON payload as `activation.json`. SkillFS prefers this
/// source when present; falls back to the file when the xattr is absent
/// or unsupported by the filesystem.
pub const ACTIVATION_XATTR: &str = "user.agent_sec.skill_ledger.activation";

/// Required prefix for a non-null activation target.
const SNAPSHOT_PREFIX_FIRST: &str = ".skill-meta";
const SNAPSHOT_PREFIX_SECOND: &str = "versions";

/// Required suffix for the snapshot directory component.
const SNAPSHOT_SUFFIX: &str = ".snapshot";

// ─────────────────────────────────────────────────────────────────────────────
// Error
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum ActivationError {
    Io(std::io::Error),
    InvalidJson {
        reason: String,
    },
    UnsupportedSchema {
        got: String,
    },
    InvalidTarget {
        reason: String,
    },
    SnapshotNotFound {
        path: PathBuf,
    },
    SnapshotNotDirectory {
        path: PathBuf,
    },
    SnapshotEscapesSkillDir {
        path: PathBuf,
    },
    /// A2: the xattr exists but contains invalid content (malformed JSON,
    /// unsupported schema, bad target). Fail-safe hidden; no fallback to
    /// `activation.json`.
    XattrInvalid {
        reason: String,
    },
    /// A2: both the xattr and `activation.json` exist and parse
    /// successfully, but their `target` fields disagree. Fail-safe hidden.
    XattrJsonMismatch {
        xattr_target: Option<PathBuf>,
        json_target: Option<PathBuf>,
    },
    /// A2: `lgetxattr` returned an unexpected error (not ENODATA /
    /// ENOTSUP / EOPNOTSUPP). The xattr subsystem is nominally present
    /// but broken — fail-safe hidden, do not fall back to
    /// `activation.json` (which may be stale).
    XattrReadError {
        errno: i32,
    },
}

impl std::fmt::Display for ActivationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ActivationError::Io(e) => write!(f, "activation I/O error: {e}"),
            ActivationError::InvalidJson { reason } => {
                write!(f, "activation JSON invalid: {reason}")
            }
            ActivationError::UnsupportedSchema { got } => {
                write!(
                    f,
                    "activation schemaVersion '{got}' not supported (expected {ACTIVATION_SCHEMA_VERSION})"
                )
            }
            ActivationError::InvalidTarget { reason } => {
                write!(f, "activation target invalid: {reason}")
            }
            ActivationError::SnapshotNotFound { path } => {
                write!(
                    f,
                    "activation snapshot does not exist: '{}'",
                    path.display()
                )
            }
            ActivationError::SnapshotNotDirectory { path } => {
                write!(
                    f,
                    "activation snapshot is not a directory: '{}'",
                    path.display()
                )
            }
            ActivationError::SnapshotEscapesSkillDir { path } => {
                write!(
                    f,
                    "activation snapshot escapes skill dir: '{}'",
                    path.display()
                )
            }
            ActivationError::XattrInvalid { reason } => {
                write!(f, "activation xattr invalid: {reason}")
            }
            ActivationError::XattrJsonMismatch {
                xattr_target,
                json_target,
            } => {
                write!(
                    f,
                    "activation xattr/json mismatch: xattr target={}, json target={}",
                    xattr_target
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "null".to_string()),
                    json_target
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "null".to_string()),
                )
            }
            ActivationError::XattrReadError { errno } => {
                write!(
                    f,
                    "activation xattr read failed with unexpected errno {errno}"
                )
            }
        }
    }
}

impl std::error::Error for ActivationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ActivationError::Io(e) => Some(e),
            _ => None,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// ActivationRecord
// ─────────────────────────────────────────────────────────────────────────────

/// Parsed, validated activation record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivationRecord {
    pub schema_version: u64,
    /// `None` means hidden; `Some(path)` is a validated relative snapshot
    /// path under `.skill-meta/versions/<version>.snapshot`.
    pub target: Option<PathBuf>,
}

impl ActivationRecord {
    /// Parse and validate an activation record from JSON bytes.
    pub fn from_json_str(s: &str) -> Result<Self, ActivationError> {
        // Parse into a generic Value first so we can distinguish
        // `"target": null` (present, null) from a missing key.
        let obj: serde_json::Value =
            serde_json::from_str(s).map_err(|e| ActivationError::InvalidJson {
                reason: e.to_string(),
            })?;

        let map = obj
            .as_object()
            .ok_or_else(|| ActivationError::InvalidJson {
                reason: "expected a JSON object".to_string(),
            })?;

        let schema_version = match map.get("schemaVersion") {
            Some(serde_json::Value::Number(n)) => n
                .as_u64()
                .ok_or_else(|| ActivationError::UnsupportedSchema { got: n.to_string() })?,
            Some(other) => {
                return Err(ActivationError::UnsupportedSchema {
                    got: other.to_string(),
                });
            }
            None => {
                return Err(ActivationError::InvalidJson {
                    reason: "missing required field 'schemaVersion'".to_string(),
                });
            }
        };

        if schema_version != ACTIVATION_SCHEMA_VERSION {
            return Err(ActivationError::UnsupportedSchema {
                got: schema_version.to_string(),
            });
        }

        // Distinguish: key present with null value vs. key absent.
        let target = match map.get("target") {
            Some(serde_json::Value::Null) => None,
            Some(serde_json::Value::String(s)) => {
                let path = validate_activation_target(s)?;
                Some(path)
            }
            Some(other) => {
                return Err(ActivationError::InvalidTarget {
                    reason: format!("target must be a string or null, got {}", other),
                });
            }
            None => {
                return Err(ActivationError::InvalidJson {
                    reason: "missing required field 'target'".to_string(),
                });
            }
        };

        Ok(Self {
            schema_version,
            target,
        })
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Target path validation
// ─────────────────────────────────────────────────────────────────────────────

/// Validate the `target` string from `activation.json`.
///
/// Rules (all lexical, no I/O):
///
/// * Non-empty.
/// * Not absolute.
/// * No `.` / `..` / NUL components.
/// * Must start with `.skill-meta/versions/`.
/// * Must have exactly one component after the prefix (the snapshot dir).
/// * That component must end with `.snapshot`.
fn validate_activation_target(raw: &str) -> Result<PathBuf, ActivationError> {
    if raw.is_empty() {
        return Err(ActivationError::InvalidTarget {
            reason: "target must be non-empty".to_string(),
        });
    }
    if raw.contains('\0') {
        return Err(ActivationError::InvalidTarget {
            reason: "target must not contain NUL bytes".to_string(),
        });
    }

    let path = Path::new(raw);

    if path.is_absolute() {
        return Err(ActivationError::InvalidTarget {
            reason: format!("must be relative, got absolute '{raw}'"),
        });
    }

    for c in path.components() {
        match c {
            Component::Normal(_) => {}
            Component::CurDir | Component::ParentDir => {
                return Err(ActivationError::InvalidTarget {
                    reason: format!("must not contain '.' or '..' components: '{raw}'"),
                });
            }
            Component::Prefix(_) | Component::RootDir => {
                return Err(ActivationError::InvalidTarget {
                    reason: format!("must be relative, got rooted '{raw}'"),
                });
            }
        }
    }

    let mut comps = path.components();
    let first = comps.next().and_then(|c| match c {
        Component::Normal(s) => s.to_str(),
        _ => None,
    });
    let second = comps.next().and_then(|c| match c {
        Component::Normal(s) => s.to_str(),
        _ => None,
    });
    let third = comps.next().and_then(|c| match c {
        Component::Normal(s) => s.to_str(),
        _ => None,
    });
    let has_more = comps.next().is_some();

    let prefix_ok = matches!(
        (first, second),
        (Some(SNAPSHOT_PREFIX_FIRST), Some(SNAPSHOT_PREFIX_SECOND))
    );
    if !prefix_ok {
        return Err(ActivationError::InvalidTarget {
            reason: format!(
                "must be under '{SNAPSHOT_PREFIX_FIRST}/{SNAPSHOT_PREFIX_SECOND}/<version>.snapshot', got '{raw}'"
            ),
        });
    }

    let snapshot_name = match third {
        Some(name) => name,
        None => {
            return Err(ActivationError::InvalidTarget {
                reason: format!(
                    "must include a snapshot component after '{SNAPSHOT_PREFIX_FIRST}/{SNAPSHOT_PREFIX_SECOND}/', got '{raw}'"
                ),
            });
        }
    };

    if has_more {
        return Err(ActivationError::InvalidTarget {
            reason: format!(
                "must be exactly '{SNAPSHOT_PREFIX_FIRST}/{SNAPSHOT_PREFIX_SECOND}/<version>.snapshot', got '{raw}' with extra components"
            ),
        });
    }

    if !snapshot_name.ends_with(SNAPSHOT_SUFFIX) {
        return Err(ActivationError::InvalidTarget {
            reason: format!(
                "snapshot component must end with '{SNAPSHOT_SUFFIX}', got '{snapshot_name}'"
            ),
        });
    }

    Ok(path.to_path_buf())
}

// ─────────────────────────────────────────────────────────────────────────────
// Conversion to ActiveTarget
// ─────────────────────────────────────────────────────────────────────────────

/// Read and parse `<skill_dir>/.skill-meta/activation.json`, validate
/// the target path against the real filesystem, and return the
/// corresponding [`ActiveTarget`].
///
/// On any error the caller should map the result to
/// [`ActiveTarget::Hidden`] — this function does **not** do that
/// automatically so the caller can log the diagnostic reason.
pub fn load_activation(skill_dir: &Path) -> Result<ActiveTarget, ActivationError> {
    let activation_path = skill_dir.join(ACTIVATION_FILE);
    let content = std::fs::read_to_string(&activation_path).map_err(ActivationError::Io)?;
    let record = ActivationRecord::from_json_str(&content)?;
    resolve_activation(skill_dir, &record)
}

/// Convert a parsed [`ActivationRecord`] into an [`ActiveTarget`],
/// validating the snapshot path against the filesystem.
pub fn resolve_activation(
    skill_dir: &Path,
    record: &ActivationRecord,
) -> Result<ActiveTarget, ActivationError> {
    match &record.target {
        None => Ok(ActiveTarget::Hidden {
            reason: "activation target is null".to_string(),
        }),
        Some(rel_target) => {
            let snapshot_dir = skill_dir.join(rel_target);

            if !snapshot_dir.exists() {
                return Err(ActivationError::SnapshotNotFound { path: snapshot_dir });
            }
            if !snapshot_dir.is_dir() {
                return Err(ActivationError::SnapshotNotDirectory { path: snapshot_dir });
            }

            // Canonicalize both to check containment.
            let skill_canon = skill_dir.canonicalize().map_err(ActivationError::Io)?;
            let snapshot_canon = snapshot_dir.canonicalize().map_err(ActivationError::Io)?;

            if !snapshot_canon.starts_with(&skill_canon) {
                return Err(ActivationError::SnapshotEscapesSkillDir {
                    path: snapshot_canon,
                });
            }

            let version = rel_target
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| rel_target.to_string_lossy().into_owned());

            Ok(ActiveTarget::Snapshot {
                snapshot_dir,
                version,
            })
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// A2: Prefer-xattr activation loading
// ─────────────────────────────────────────────────────────────────────────────

/// A2 entry point: load activation state for a single skill, preferring
/// the xattr source with `activation.json` as a fallback.
///
/// Strategy:
///
/// 1. Try reading `ACTIVATION_XATTR` from `skill_dir` (no-follow).
/// 2. If xattr is absent (`ENODATA`) or unsupported (`ENOTSUP` /
///    `EOPNOTSUPP`), fall back to `activation.json`.
/// 3. If `lgetxattr` returned an unexpected error (e.g. `EACCES`,
///    `EIO`, `ERANGE`), fail-safe hidden — do **not** fall back to
///    `activation.json`, which may be stale.
/// 4. If xattr is present but invalid (bad JSON, bad schema, bad target),
///    return `XattrInvalid` immediately — **no** fallback, fail-safe hidden.
/// 5. If xattr is present and valid, and `activation.json` also exists and
///    is valid, check that both agree on the `target` field. Disagreement
///    returns `XattrJsonMismatch` — fail-safe hidden.
/// 6. If xattr is present and valid, and `activation.json` is missing or
///    unreadable, use the xattr record.
pub fn load_activation_prefer_xattr(skill_dir: &Path) -> Result<ActiveTarget, ActivationError> {
    match read_activation_xattr(skill_dir) {
        XattrReadOutcome::Present(xattr_str) => {
            let xattr_record = ActivationRecord::from_json_str(&xattr_str).map_err(|e| {
                ActivationError::XattrInvalid {
                    reason: e.to_string(),
                }
            })?;

            // Check whether activation.json also exists and agrees.
            let activation_path = skill_dir.join(ACTIVATION_FILE);
            if let Ok(json_content) = std::fs::read_to_string(&activation_path) {
                if let Ok(json_record) = ActivationRecord::from_json_str(&json_content) {
                    if xattr_record.target != json_record.target {
                        return Err(ActivationError::XattrJsonMismatch {
                            xattr_target: xattr_record.target,
                            json_target: json_record.target,
                        });
                    }
                }
                // json invalid or missing is fine — xattr is authoritative.
            }

            resolve_activation(skill_dir, &xattr_record)
        }
        XattrReadOutcome::Absent => load_activation(skill_dir),
        XattrReadOutcome::OsError(errno) => Err(ActivationError::XattrReadError { errno }),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// A2: Physical xattr reader (no-follow, bypasses FUSE)
// ─────────────────────────────────────────────────────────────────────────────

/// Outcome of trying to read the activation xattr from `skill_dir`.
#[derive(Debug)]
pub enum XattrReadOutcome {
    /// Xattr present and successfully read as UTF-8.
    Present(String),
    /// Xattr absent (`ENODATA`) or filesystem does not support user
    /// xattrs (`ENOTSUP` / `EOPNOTSUPP`). Caller should fall back to
    /// `activation.json`.
    Absent,
    /// Xattr system call failed with an unexpected errno (e.g. `EACCES`,
    /// `EIO`, `ERANGE`). Caller must fail-safe hidden — do NOT fall back
    /// to `activation.json`.
    OsError(i32),
}

/// Classify an `lgetxattr` errno into `Absent` (safe to fallback) or
/// `OsError` (fail-safe hidden).
///
/// Only `ENODATA` (xattr does not exist) and `ENOTSUP` / `EOPNOTSUPP`
/// (filesystem does not support user xattrs) are treated as "absent".
/// Everything else — `EACCES`, `EIO`, `ERANGE`, etc. — indicates the
/// xattr subsystem is nominally present but broken, so the caller must
/// not silently fall through to a potentially stale `activation.json`.
pub fn classify_xattr_errno(errno: i32) -> XattrReadOutcome {
    if errno == libc::ENODATA || errno == libc::ENOTSUP || errno == libc::EOPNOTSUPP {
        XattrReadOutcome::Absent
    } else {
        XattrReadOutcome::OsError(errno)
    }
}

/// Read the `user.agent_sec.skill_ledger.activation` xattr from the
/// physical `skill_dir` directory using `lgetxattr` (no symlink follow).
///
/// This is intentionally a direct libc call against the *physical* source
/// directory — it does NOT go through the FUSE xattr callback path, so it
/// cannot create a loop and does not affect the T3 `user.*` passthrough
/// semantics.
pub fn read_activation_xattr(skill_dir: &Path) -> XattrReadOutcome {
    use std::os::unix::ffi::OsStrExt;

    let c_path = match CString::new(skill_dir.as_os_str().as_bytes()) {
        Ok(c) => c,
        Err(_) => return XattrReadOutcome::OsError(libc::EINVAL),
    };
    let c_name = match CString::new(ACTIVATION_XATTR) {
        Ok(c) => c,
        Err(_) => return XattrReadOutcome::OsError(libc::EINVAL),
    };

    let needed =
        unsafe { libc::lgetxattr(c_path.as_ptr(), c_name.as_ptr(), std::ptr::null_mut(), 0) };
    if needed < 0 {
        let e = std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(libc::EIO);
        return classify_xattr_errno(e);
    }
    let needed = needed as usize;
    if needed == 0 {
        return XattrReadOutcome::Present(String::new());
    }
    let mut buf = vec![0u8; needed];
    let got = unsafe {
        libc::lgetxattr(
            c_path.as_ptr(),
            c_name.as_ptr(),
            buf.as_mut_ptr() as *mut libc::c_void,
            buf.len(),
        )
    };
    if got < 0 {
        let e = std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(libc::EIO);
        return classify_xattr_errno(e);
    }
    buf.truncate(got as usize);
    match String::from_utf8(buf) {
        Ok(s) => XattrReadOutcome::Present(s),
        Err(_) => XattrReadOutcome::Present(String::new()),
    }
}

/// Fail-safe helper: convert any activation error into a hidden target
/// with a diagnostic reason string.
pub fn fail_safe_hidden(err: &ActivationError) -> ActiveTarget {
    ActiveTarget::Hidden {
        reason: format!("activation fail-safe: {err}"),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// ActivationMode
// ─────────────────────────────────────────────────────────────────────────────

/// Runtime activation mode. Controls whether SkillFS consumes
/// `activation.json` at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ActivationMode {
    /// Activation file consumption is disabled. The existing
    /// `--decision-command` path (if any) is the only way to populate the
    /// active-skill resolver. This is the default.
    #[default]
    Off,
    /// Consume `<skill_dir>/.skill-meta/activation.json` at startup.
    File,
}

impl ActivationMode {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "off" => Some(Self::Off),
            "file" => Some(Self::File),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::File => "file",
        }
    }
}

impl std::fmt::Display for ActivationMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Startup bootstrap
// ─────────────────────────────────────────────────────────────────────────────

/// Enumerate Hermes nested skill leaves under `source_root`.
///
/// Walks the source root physically: first-level dirs are categories
/// (skipping management paths), second-level dirs with `SKILL.md` are
/// skill leaves. Returns skill ids as `"category/skill"`.
pub fn enumerate_hermes_skill_leaves(source_root: &Path) -> Vec<String> {
    use crate::path::is_hermes_management_path;

    let mut leaves = Vec::new();
    let entries = match std::fs::read_dir(source_root) {
        Ok(e) => e,
        Err(_) => return leaves,
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if is_hermes_management_path(&name) || name.starts_with('.') {
            continue;
        }
        // No-follow type: a symlinked category is not a managed category
        // (the resolver rejects symlinked components with O_NOFOLLOW), and
        // descending into it would register skills that live outside the
        // managed root. Skip it so enumeration matches the resolver.
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        // A first-level directory that carries its own SKILL.md is a
        // top-level skill, not a category. The mount serves it (and its
        // whole subtree) as a flat skill, so descending into it here would
        // register `skill/subdir` as a phantom nested skill and diverge
        // from mount discovery. Uses the shared no-follow regular-file
        // marker so store, mount, and resolver agree on what a Skill is.
        if skillfs_core::store::has_regular_skill_md(&entry.path()) {
            continue;
        }
        let cat_entries = match std::fs::read_dir(entry.path()) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for child in cat_entries.flatten() {
            let child_name = child.file_name().to_string_lossy().to_string();
            // No-follow: a symlinked nested entry is never a managed skill.
            if !child.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            if skillfs_core::store::has_regular_skill_md(&child.path()) {
                leaves.push(format!("{}/{}", name, child_name));
            }
        }
    }
    leaves
}

/// Enumerate top-level Hermes skills under `source_root`.
///
/// A top-level skill is a non-management directory directly under the
/// source root that contains its own `SKILL.md`. Real Hermes workspaces
/// mix these flat skills with categorized nested skills, so activation
/// discovery must cover both.
pub fn enumerate_hermes_top_level_skills(source_root: &Path) -> Vec<String> {
    use crate::path::is_hermes_management_path;

    let mut skills = Vec::new();
    let entries = match std::fs::read_dir(source_root) {
        Ok(e) => e,
        Err(_) => return skills,
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if is_hermes_management_path(&name) || name.starts_with('.') {
            continue;
        }
        // No-follow: a symlinked top-level entry is not a managed skill.
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        if skillfs_core::store::has_regular_skill_md(&entry.path()) {
            skills.push(name);
        }
    }
    skills
}

/// Enumerate every activatable Hermes skill id under `source_root`:
/// top-level skills (`skill`) and categorized nested skills
/// (`category/skill`).
///
/// This mirrors exactly what the FUSE mount treats as a skill — top-level
/// directories with `SKILL.md` become flat skills, and second-level
/// directories with `SKILL.md` become nested skills — so activation
/// bootstrap consumes the same set the mount exposes and cannot diverge.
pub fn enumerate_hermes_skill_ids(source_root: &Path) -> Vec<String> {
    let mut ids = enumerate_hermes_top_level_skills(source_root);
    ids.extend(enumerate_hermes_skill_leaves(source_root));
    ids
}

/// Load activation state for every skill in `skill_names` and install
/// into the given resolver. Errors are non-fatal: each failing skill is
/// mapped to hidden with a diagnostic log line.
///
/// This is the startup entry point. It does NOT re-read on every FUSE
/// read — the resolver is populated once and updated only by explicit
/// refresh (a later package).
pub fn bootstrap_activation(
    source_root: &Path,
    skill_names: &[String],
    resolver: &super::active::ActiveSkillResolver,
) -> Vec<(String, Result<ActiveTarget, ActivationError>)> {
    let mut results = Vec::with_capacity(skill_names.len());
    for name in skill_names {
        let skill_dir = source_root.join(name);
        let outcome = load_activation_prefer_xattr(&skill_dir);
        let target = match &outcome {
            Ok(t) => t.clone(),
            Err(e) => fail_safe_hidden(e),
        };
        resolver.set(name.clone(), target);
        results.push((name.clone(), outcome));
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─────────────────────────────────────────────────────────────────────
    // ActivationRecord parsing
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn parses_valid_snapshot_target() {
        let json = r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000001.snapshot"}"#;
        let r = ActivationRecord::from_json_str(json).unwrap();
        assert_eq!(r.schema_version, 1);
        assert_eq!(
            r.target,
            Some(PathBuf::from(".skill-meta/versions/v000001.snapshot"))
        );
    }

    #[test]
    fn parses_null_target_as_hidden() {
        let json = r#"{"schemaVersion": 1, "target": null}"#;
        let r = ActivationRecord::from_json_str(json).unwrap();
        assert!(r.target.is_none());
    }

    #[test]
    fn rejects_absent_target_field() {
        let json = r#"{"schemaVersion": 1}"#;
        let err = ActivationRecord::from_json_str(json).unwrap_err();
        assert!(matches!(err, ActivationError::InvalidJson { .. }));
        assert!(
            err.to_string().contains("target"),
            "error should mention 'target': {err}"
        );
    }

    #[test]
    fn rejects_unsupported_schema_version() {
        let json = r#"{"schemaVersion": 2, "target": null}"#;
        let err = ActivationRecord::from_json_str(json).unwrap_err();
        assert!(matches!(err, ActivationError::UnsupportedSchema { .. }));
    }

    #[test]
    fn rejects_string_schema_version() {
        let json = r#"{"schemaVersion": "1", "target": null}"#;
        let err = ActivationRecord::from_json_str(json).unwrap_err();
        assert!(matches!(err, ActivationError::UnsupportedSchema { .. }));
    }

    #[test]
    fn rejects_missing_schema_version() {
        let json = r#"{"target": null}"#;
        let err = ActivationRecord::from_json_str(json).unwrap_err();
        assert!(matches!(err, ActivationError::InvalidJson { .. }));
    }

    #[test]
    fn rejects_malformed_json() {
        let err = ActivationRecord::from_json_str("not json").unwrap_err();
        assert!(matches!(err, ActivationError::InvalidJson { .. }));
    }

    #[test]
    fn rejects_non_string_target() {
        let json = r#"{"schemaVersion": 1, "target": 42}"#;
        let err = ActivationRecord::from_json_str(json).unwrap_err();
        assert!(matches!(err, ActivationError::InvalidTarget { .. }));
    }

    // ─────────────────────────────────────────────────────────────────────
    // Target path validation
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn rejects_empty_target() {
        let json = r#"{"schemaVersion": 1, "target": ""}"#;
        let err = ActivationRecord::from_json_str(json).unwrap_err();
        assert!(matches!(err, ActivationError::InvalidTarget { .. }));
    }

    #[test]
    fn rejects_absolute_path() {
        let json = r#"{"schemaVersion": 1, "target": "/etc/passwd"}"#;
        let err = ActivationRecord::from_json_str(json).unwrap_err();
        assert!(matches!(err, ActivationError::InvalidTarget { .. }));
    }

    #[test]
    fn rejects_dotdot_traversal() {
        let json = r#"{"schemaVersion": 1, "target": ".skill-meta/versions/../../etc/passwd"}"#;
        let err = ActivationRecord::from_json_str(json).unwrap_err();
        assert!(matches!(err, ActivationError::InvalidTarget { .. }));
    }

    #[test]
    fn rejects_wrong_prefix() {
        let json = r#"{"schemaVersion": 1, "target": "scripts/v000001.snapshot"}"#;
        let err = ActivationRecord::from_json_str(json).unwrap_err();
        assert!(matches!(err, ActivationError::InvalidTarget { .. }));
    }

    #[test]
    fn rejects_bare_prefix() {
        let json = r#"{"schemaVersion": 1, "target": ".skill-meta/versions"}"#;
        let err = ActivationRecord::from_json_str(json).unwrap_err();
        assert!(matches!(err, ActivationError::InvalidTarget { .. }));
    }

    #[test]
    fn rejects_non_snapshot_suffix() {
        let json = r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000001.tar"}"#;
        let err = ActivationRecord::from_json_str(json).unwrap_err();
        assert!(matches!(err, ActivationError::InvalidTarget { .. }));
    }

    #[test]
    fn rejects_extra_components_after_snapshot() {
        let json =
            r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000001.snapshot/extra"}"#;
        let err = ActivationRecord::from_json_str(json).unwrap_err();
        assert!(matches!(err, ActivationError::InvalidTarget { .. }));
    }

    #[test]
    fn rejects_nul_in_target() {
        let target_with_nul = ".skill-meta/versions/v\0.snapshot";
        let json = serde_json::json!({"schemaVersion": 1, "target": target_with_nul});
        let err = ActivationRecord::from_json_str(&json.to_string()).unwrap_err();
        assert!(matches!(err, ActivationError::InvalidTarget { .. }));
    }

    // ─────────────────────────────────────────────────────────────────────
    // resolve_activation (needs filesystem)
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn resolve_null_target_yields_hidden() {
        let dir = tempfile::tempdir().unwrap();
        let record = ActivationRecord {
            schema_version: 1,
            target: None,
        };
        let target = resolve_activation(dir.path(), &record).unwrap();
        assert!(matches!(target, ActiveTarget::Hidden { .. }));
    }

    #[test]
    fn resolve_valid_snapshot_yields_snapshot_target() {
        let dir = tempfile::tempdir().unwrap();
        let snap = dir.path().join(".skill-meta/versions/v000001.snapshot");
        std::fs::create_dir_all(&snap).unwrap();

        let record = ActivationRecord {
            schema_version: 1,
            target: Some(PathBuf::from(".skill-meta/versions/v000001.snapshot")),
        };
        let target = resolve_activation(dir.path(), &record).unwrap();
        match target {
            ActiveTarget::Snapshot {
                snapshot_dir,
                version,
            } => {
                assert_eq!(snapshot_dir, snap);
                assert_eq!(version, "v000001.snapshot");
            }
            other => panic!("expected Snapshot, got {other:?}"),
        }
    }

    #[test]
    fn resolve_missing_snapshot_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let record = ActivationRecord {
            schema_version: 1,
            target: Some(PathBuf::from(".skill-meta/versions/v000001.snapshot")),
        };
        let err = resolve_activation(dir.path(), &record).unwrap_err();
        assert!(matches!(err, ActivationError::SnapshotNotFound { .. }));
    }

    #[test]
    fn resolve_snapshot_file_not_dir_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let versions = dir.path().join(".skill-meta/versions");
        std::fs::create_dir_all(&versions).unwrap();
        std::fs::write(versions.join("v000001.snapshot"), "not a directory").unwrap();

        let record = ActivationRecord {
            schema_version: 1,
            target: Some(PathBuf::from(".skill-meta/versions/v000001.snapshot")),
        };
        let err = resolve_activation(dir.path(), &record).unwrap_err();
        assert!(
            matches!(err, ActivationError::SnapshotNotDirectory { .. }),
            "expected SnapshotNotDirectory, got {err:?}"
        );
    }

    // ─────────────────────────────────────────────────────────────────────
    // load_activation (end-to-end)
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn load_missing_activation_file_returns_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = load_activation(dir.path()).unwrap_err();
        assert!(matches!(err, ActivationError::Io(_)));
    }

    #[test]
    fn load_valid_hidden_activation() {
        let dir = tempfile::tempdir().unwrap();
        let meta = dir.path().join(".skill-meta");
        std::fs::create_dir_all(&meta).unwrap();
        std::fs::write(
            meta.join("activation.json"),
            r#"{"schemaVersion": 1, "target": null}"#,
        )
        .unwrap();

        let target = load_activation(dir.path()).unwrap();
        assert!(matches!(target, ActiveTarget::Hidden { .. }));
    }

    #[test]
    fn load_valid_snapshot_activation() {
        let dir = tempfile::tempdir().unwrap();
        let snap = dir.path().join(".skill-meta/versions/v000001.snapshot");
        std::fs::create_dir_all(&snap).unwrap();
        std::fs::write(
            dir.path().join(".skill-meta/activation.json"),
            r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000001.snapshot"}"#,
        )
        .unwrap();

        let target = load_activation(dir.path()).unwrap();
        match target {
            ActiveTarget::Snapshot { version, .. } => {
                assert_eq!(version, "v000001.snapshot");
            }
            other => panic!("expected Snapshot, got {other:?}"),
        }
    }

    // ─────────────────────────────────────────────────────────────────────
    // fail_safe_hidden
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn fail_safe_hidden_produces_hidden_target() {
        let err = ActivationError::InvalidTarget {
            reason: "test".to_string(),
        };
        let target = fail_safe_hidden(&err);
        match target {
            ActiveTarget::Hidden { reason } => {
                assert!(reason.contains("fail-safe"));
                assert!(reason.contains("test"));
            }
            other => panic!("expected Hidden, got {other:?}"),
        }
    }

    // ─────────────────────────────────────────────────────────────────────
    // ActivationMode
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn activation_mode_round_trip() {
        assert_eq!(ActivationMode::parse("off"), Some(ActivationMode::Off));
        assert_eq!(ActivationMode::parse("file"), Some(ActivationMode::File));
        assert_eq!(ActivationMode::parse("bogus"), None);
        assert_eq!(ActivationMode::Off.as_str(), "off");
        assert_eq!(ActivationMode::File.as_str(), "file");
    }

    #[test]
    fn activation_mode_default_is_off() {
        assert_eq!(ActivationMode::default(), ActivationMode::Off);
    }

    // ─────────────────────────────────────────────────────────────────────
    // bootstrap_activation
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn bootstrap_installs_valid_and_hides_invalid() {
        use super::super::active::ActiveSkillResolver;

        let dir = tempfile::tempdir().unwrap();

        // Skill with a valid snapshot activation.
        let alpha = dir.path().join("alpha");
        std::fs::create_dir_all(alpha.join(".skill-meta/versions/v000001.snapshot")).unwrap();
        std::fs::write(
            alpha.join(".skill-meta/activation.json"),
            r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000001.snapshot"}"#,
        )
        .unwrap();

        // Skill with invalid activation (bad JSON).
        let beta = dir.path().join("beta");
        std::fs::create_dir_all(beta.join(".skill-meta")).unwrap();
        std::fs::write(beta.join(".skill-meta/activation.json"), "not json").unwrap();

        // Skill with no activation file.
        let gamma = dir.path().join("gamma");
        std::fs::create_dir_all(&gamma).unwrap();

        let resolver = ActiveSkillResolver::new(dir.path());
        let names: Vec<String> = vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()];
        let results = bootstrap_activation(dir.path(), &names, &resolver);

        assert!(results[0].1.is_ok());
        assert!(results[1].1.is_err());
        assert!(results[2].1.is_err());

        assert!(matches!(
            resolver.get("alpha"),
            Some(ActiveTarget::Snapshot { .. })
        ));
        assert!(matches!(
            resolver.get("beta"),
            Some(ActiveTarget::Hidden { .. })
        ));
        assert!(matches!(
            resolver.get("gamma"),
            Some(ActiveTarget::Hidden { .. })
        ));
    }

    // ─────────────────────────────────────────────────────────────────────
    // enumerate_hermes_skill_leaves + bootstrap_activation with nested ids
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn enumerate_hermes_skill_leaves_finds_nested_skills() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        // Management paths — should be skipped.
        std::fs::create_dir_all(root.join(".hub")).unwrap();
        std::fs::write(root.join(".bundled_manifest"), "").unwrap();

        // Category with two skill leaves.
        let notes = root.join("apple/apple-notes");
        std::fs::create_dir_all(&notes).unwrap();
        std::fs::write(notes.join("SKILL.md"), "---\nname: n\n---\n").unwrap();

        let music = root.join("apple/apple-music");
        std::fs::create_dir_all(&music).unwrap();
        std::fs::write(music.join("SKILL.md"), "---\nname: m\n---\n").unwrap();

        // Non-skill subdir (no SKILL.md) — should be skipped.
        std::fs::create_dir_all(root.join("apple/docs")).unwrap();

        // Dot-prefixed dir — should be skipped.
        std::fs::create_dir_all(root.join(".git/objects")).unwrap();

        let mut leaves = enumerate_hermes_skill_leaves(root);
        leaves.sort();
        assert_eq!(
            leaves,
            vec!["apple/apple-music", "apple/apple-notes"],
            "must enumerate only dirs with SKILL.md, got: {:?}",
            leaves
        );
    }

    #[test]
    fn enumerate_hermes_rejects_symlink_category_and_nested_and_top_level() {
        // Symlinked categories, symlinked nested skills, and symlinked
        // top-level skills all live outside the managed root once resolved,
        // and the resolver rejects symlinked components with O_NOFOLLOW.
        // Enumeration must skip them so it never registers an activatable id
        // the resolver would refuse to resolve.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let outside = tempfile::tempdir().unwrap();

        // Real category + nested skill (must be enumerated).
        let real_nested = root.join("cat/realnested");
        std::fs::create_dir_all(&real_nested).unwrap();
        std::fs::write(real_nested.join("SKILL.md"), "---\nname: rn\n---\n").unwrap();

        // Symlinked nested skill under the real category (must be skipped).
        let out_skill = outside.path().join("out-skill");
        std::fs::create_dir(&out_skill).unwrap();
        std::fs::write(out_skill.join("SKILL.md"), "---\nname: os\n---\n").unwrap();
        std::os::unix::fs::symlink(&out_skill, root.join("cat/linknested")).unwrap();

        // Symlinked category (must not be descended).
        let out_cat = outside.path().join("out-cat");
        let out_cat_child = out_cat.join("child");
        std::fs::create_dir_all(&out_cat_child).unwrap();
        std::fs::write(out_cat_child.join("SKILL.md"), "---\nname: oc\n---\n").unwrap();
        std::os::unix::fs::symlink(&out_cat, root.join("linkcat")).unwrap();

        // Symlinked top-level skill (must be skipped).
        let out_top = outside.path().join("out-top");
        std::fs::create_dir(&out_top).unwrap();
        std::fs::write(out_top.join("SKILL.md"), "---\nname: ot\n---\n").unwrap();
        std::os::unix::fs::symlink(&out_top, root.join("linktop")).unwrap();

        let mut leaves = enumerate_hermes_skill_leaves(root);
        leaves.sort();
        assert_eq!(
            leaves,
            vec!["cat/realnested"],
            "only the real nested skill may be enumerated, got: {leaves:?}"
        );

        let top = enumerate_hermes_top_level_skills(root);
        assert!(
            !top.iter().any(|s| s == "linktop"),
            "a symlinked top-level skill must not be enumerated, got: {top:?}"
        );

        let ids = enumerate_hermes_skill_ids(root);
        assert!(
            !ids.iter()
                .any(|i| i == "linktop" || i == "linkcat/child" || i == "cat/linknested"),
            "no symlinked category/nested/top-level id may appear, got: {ids:?}"
        );
    }

    #[test]
    fn bootstrap_activation_with_hermes_nested_id() {
        use super::super::active::ActiveSkillResolver;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        // Create a nested skill with a valid activation file.
        let skill = root.join("apple/apple-notes");
        std::fs::create_dir_all(skill.join(".skill-meta/versions/v000001.snapshot")).unwrap();
        std::fs::write(
            skill.join(".skill-meta/activation.json"),
            r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000001.snapshot"}"#,
        )
        .unwrap();

        // Create another nested skill with hidden activation.
        let hidden = root.join("apple/apple-music");
        std::fs::create_dir_all(hidden.join(".skill-meta")).unwrap();
        std::fs::write(
            hidden.join(".skill-meta/activation.json"),
            r#"{"schemaVersion": 1, "target": null}"#,
        )
        .unwrap();

        let resolver = ActiveSkillResolver::new(root);
        let names = vec![
            "apple/apple-notes".to_string(),
            "apple/apple-music".to_string(),
        ];
        let results = bootstrap_activation(root, &names, &resolver);

        assert!(results[0].1.is_ok(), "apple/apple-notes must load");
        assert!(results[1].1.is_ok(), "apple/apple-music must load");

        assert!(
            matches!(
                resolver.get("apple/apple-notes"),
                Some(ActiveTarget::Snapshot { .. })
            ),
            "apple/apple-notes must be snapshot"
        );
        assert!(
            matches!(
                resolver.get("apple/apple-music"),
                Some(ActiveTarget::Hidden { .. })
            ),
            "apple/apple-music must be hidden"
        );
    }

    // ─────────────────────────────────────────────────────────────────────
    // A2: load_activation_prefer_xattr
    // ─────────────────────────────────────────────────────────────────────

    /// Helper: probe whether user xattrs work on the given directory.
    fn user_xattr_supported(dir: &Path) -> bool {
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

    /// Helper: set the activation xattr on a directory.
    fn set_activation_xattr(dir: &Path, value: &str) {
        use std::os::unix::ffi::OsStrExt;
        let c_path = CString::new(dir.as_os_str().as_bytes()).unwrap();
        let c_name = CString::new(ACTIVATION_XATTR).unwrap();
        let rc = unsafe {
            libc::lsetxattr(
                c_path.as_ptr(),
                c_name.as_ptr(),
                value.as_ptr() as *const libc::c_void,
                value.len(),
                0,
            )
        };
        assert_eq!(
            rc,
            0,
            "lsetxattr failed: {}",
            std::io::Error::last_os_error()
        );
    }

    /// Helper: find an xattr-capable temp root (same strategy as
    /// `posix_xattr_tests`).
    fn xattr_capable_tempdir() -> Option<tempfile::TempDir> {
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Ok(env_path) = std::env::var("SKILLFS_XATTR_TEST_ROOT") {
            if !env_path.is_empty() {
                candidates.push(PathBuf::from(env_path));
            }
        }
        // Walk up from CARGO_MANIFEST_DIR to find the workspace root.
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
                .prefix("a2-unit-")
                .tempdir_in(&cand)
            {
                Ok(d) => d,
                Err(_) => continue,
            };
            if user_xattr_supported(td.path()) {
                return Some(td);
            }
        }
        None
    }

    #[test]
    fn prefer_xattr_json_only_still_works() {
        let dir = tempfile::tempdir().unwrap();
        let snap = dir.path().join(".skill-meta/versions/v000001.snapshot");
        std::fs::create_dir_all(&snap).unwrap();
        std::fs::write(
            dir.path().join(ACTIVATION_FILE),
            r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000001.snapshot"}"#,
        )
        .unwrap();

        let target = load_activation_prefer_xattr(dir.path()).unwrap();
        match target {
            ActiveTarget::Snapshot { version, .. } => assert_eq!(version, "v000001.snapshot"),
            other => panic!("expected Snapshot, got {other:?}"),
        }
    }

    #[test]
    fn prefer_xattr_json_only_hidden() {
        let dir = tempfile::tempdir().unwrap();
        let meta = dir.path().join(".skill-meta");
        std::fs::create_dir_all(&meta).unwrap();
        std::fs::write(
            meta.join("activation.json"),
            r#"{"schemaVersion": 1, "target": null}"#,
        )
        .unwrap();

        let target = load_activation_prefer_xattr(dir.path()).unwrap();
        assert!(matches!(target, ActiveTarget::Hidden { .. }));
    }

    #[test]
    fn prefer_xattr_missing_everything_returns_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = load_activation_prefer_xattr(dir.path()).unwrap_err();
        assert!(matches!(err, ActivationError::Io(_)));
    }

    #[test]
    fn prefer_xattr_xattr_only_works() {
        let td = match xattr_capable_tempdir() {
            Some(d) => d,
            None => {
                eprintln!("SKIP: no xattr-capable filesystem for A2 xattr-only test");
                return;
            }
        };
        let dir = td.path();
        std::fs::create_dir_all(dir.join(".skill-meta/versions/v000001.snapshot")).unwrap();
        set_activation_xattr(
            dir,
            r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000001.snapshot"}"#,
        );

        let target = load_activation_prefer_xattr(dir).unwrap();
        match target {
            ActiveTarget::Snapshot { version, .. } => assert_eq!(version, "v000001.snapshot"),
            other => panic!("expected Snapshot from xattr-only, got {other:?}"),
        }
    }

    #[test]
    fn prefer_xattr_xattr_missing_falls_back_to_json() {
        let td = match xattr_capable_tempdir() {
            Some(d) => d,
            None => {
                eprintln!("SKIP: no xattr-capable filesystem for A2 fallback test");
                return;
            }
        };
        let dir = td.path();
        std::fs::create_dir_all(dir.join(".skill-meta/versions/v000001.snapshot")).unwrap();
        // No xattr set, only activation.json.
        std::fs::write(
            dir.join(ACTIVATION_FILE),
            r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000001.snapshot"}"#,
        )
        .unwrap();

        let target = load_activation_prefer_xattr(dir).unwrap();
        match target {
            ActiveTarget::Snapshot { version, .. } => assert_eq!(version, "v000001.snapshot"),
            other => panic!("expected Snapshot from json fallback, got {other:?}"),
        }
    }

    #[test]
    fn prefer_xattr_invalid_xattr_hides_even_if_json_valid() {
        let td = match xattr_capable_tempdir() {
            Some(d) => d,
            None => {
                eprintln!("SKIP: no xattr-capable filesystem for A2 invalid-xattr test");
                return;
            }
        };
        let dir = td.path();
        std::fs::create_dir_all(dir.join(".skill-meta/versions/v000001.snapshot")).unwrap();
        // Valid activation.json.
        std::fs::write(
            dir.join(ACTIVATION_FILE),
            r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000001.snapshot"}"#,
        )
        .unwrap();
        // Invalid xattr (not JSON).
        set_activation_xattr(dir, "not valid json");

        let err = load_activation_prefer_xattr(dir).unwrap_err();
        assert!(
            matches!(err, ActivationError::XattrInvalid { .. }),
            "expected XattrInvalid, got {err:?}"
        );
        let target = fail_safe_hidden(&err);
        assert!(matches!(target, ActiveTarget::Hidden { .. }));
    }

    #[test]
    fn prefer_xattr_mismatch_hides() {
        let td = match xattr_capable_tempdir() {
            Some(d) => d,
            None => {
                eprintln!("SKIP: no xattr-capable filesystem for A2 mismatch test");
                return;
            }
        };
        let dir = td.path();
        std::fs::create_dir_all(dir.join(".skill-meta/versions/v000001.snapshot")).unwrap();
        std::fs::create_dir_all(dir.join(".skill-meta/versions/v000002.snapshot")).unwrap();

        // xattr points to v000001, json points to v000002.
        set_activation_xattr(
            dir,
            r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000001.snapshot"}"#,
        );
        std::fs::write(
            dir.join(ACTIVATION_FILE),
            r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000002.snapshot"}"#,
        )
        .unwrap();

        let err = load_activation_prefer_xattr(dir).unwrap_err();
        assert!(
            matches!(err, ActivationError::XattrJsonMismatch { .. }),
            "expected XattrJsonMismatch, got {err:?}"
        );
    }

    #[test]
    fn prefer_xattr_agreement_uses_xattr() {
        let td = match xattr_capable_tempdir() {
            Some(d) => d,
            None => {
                eprintln!("SKIP: no xattr-capable filesystem for A2 agreement test");
                return;
            }
        };
        let dir = td.path();
        std::fs::create_dir_all(dir.join(".skill-meta/versions/v000001.snapshot")).unwrap();

        let payload = r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000001.snapshot"}"#;
        set_activation_xattr(dir, payload);
        std::fs::write(dir.join(ACTIVATION_FILE), payload).unwrap();

        let target = load_activation_prefer_xattr(dir).unwrap();
        match target {
            ActiveTarget::Snapshot { version, .. } => assert_eq!(version, "v000001.snapshot"),
            other => panic!("expected Snapshot, got {other:?}"),
        }
    }

    #[test]
    fn prefer_xattr_xattr_null_json_missing_yields_hidden() {
        let td = match xattr_capable_tempdir() {
            Some(d) => d,
            None => {
                eprintln!("SKIP: no xattr-capable filesystem for A2 xattr-null test");
                return;
            }
        };
        let dir = td.path();
        set_activation_xattr(dir, r#"{"schemaVersion": 1, "target": null}"#);

        let target = load_activation_prefer_xattr(dir).unwrap();
        assert!(matches!(target, ActiveTarget::Hidden { .. }));
    }

    #[test]
    fn prefer_xattr_xattr_valid_json_invalid_uses_xattr() {
        let td = match xattr_capable_tempdir() {
            Some(d) => d,
            None => {
                eprintln!("SKIP: no xattr-capable filesystem for A2 xattr-valid-json-invalid test");
                return;
            }
        };
        let dir = td.path();
        std::fs::create_dir_all(dir.join(".skill-meta/versions/v000001.snapshot")).unwrap();

        set_activation_xattr(
            dir,
            r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000001.snapshot"}"#,
        );
        // Write invalid JSON to activation.json — xattr is valid, so
        // the json parse failure is ignored and xattr wins.
        let meta = dir.join(".skill-meta");
        std::fs::create_dir_all(&meta).unwrap();
        std::fs::write(meta.join("activation.json"), "INVALID").unwrap();

        let target = load_activation_prefer_xattr(dir).unwrap();
        match target {
            ActiveTarget::Snapshot { version, .. } => assert_eq!(version, "v000001.snapshot"),
            other => panic!("expected Snapshot from xattr (json invalid), got {other:?}"),
        }
    }

    #[test]
    fn activation_xattr_constant_value() {
        assert_eq!(ACTIVATION_XATTR, "user.agent_sec.skill_ledger.activation");
    }

    #[test]
    fn read_activation_xattr_absent_on_bare_dir() {
        let dir = tempfile::tempdir().unwrap();
        let outcome = read_activation_xattr(dir.path());
        assert!(
            matches!(
                outcome,
                XattrReadOutcome::Absent | XattrReadOutcome::OsError(_)
            ),
            "expected Absent or OsError on bare dir, got {outcome:?}"
        );
    }

    #[test]
    fn xattr_invalid_error_display() {
        let err = ActivationError::XattrInvalid {
            reason: "bad payload".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("xattr invalid"), "got: {msg}");
        assert!(msg.contains("bad payload"), "got: {msg}");
    }

    #[test]
    fn xattr_json_mismatch_error_display() {
        let err = ActivationError::XattrJsonMismatch {
            xattr_target: Some(PathBuf::from(".skill-meta/versions/v000001.snapshot")),
            json_target: Some(PathBuf::from(".skill-meta/versions/v000002.snapshot")),
        };
        let msg = err.to_string();
        assert!(msg.contains("mismatch"), "got: {msg}");
        assert!(msg.contains("v000001"), "got: {msg}");
        assert!(msg.contains("v000002"), "got: {msg}");
    }

    #[test]
    fn xattr_read_error_display() {
        let err = ActivationError::XattrReadError {
            errno: libc::EACCES,
        };
        let msg = err.to_string();
        assert!(msg.contains("xattr read failed"), "got: {msg}");
        assert!(msg.contains(&libc::EACCES.to_string()), "got: {msg}");
    }

    #[test]
    fn xattr_read_error_does_not_fallback() {
        let td = match xattr_capable_tempdir() {
            Some(d) => d,
            None => {
                eprintln!("SKIP: no xattr-capable filesystem for xattr-read-error test");
                return;
            }
        };
        let dir = td.path();
        std::fs::create_dir_all(dir.join(".skill-meta/versions/v000001.snapshot")).unwrap();
        // Valid activation.json exists — but if xattr read returns an
        // unexpected OsError, we must NOT fall back to it.
        std::fs::write(
            dir.join(ACTIVATION_FILE),
            r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000001.snapshot"}"#,
        )
        .unwrap();

        // We can't easily induce EACCES on lgetxattr in a unit test
        // without privilege manipulation, but we can verify the
        // XattrReadError variant round-trips through fail_safe_hidden.
        let err = ActivationError::XattrReadError { errno: libc::EIO };
        let target = fail_safe_hidden(&err);
        assert!(
            matches!(target, ActiveTarget::Hidden { .. }),
            "XattrReadError must produce hidden, got {target:?}"
        );
    }

    #[test]
    fn classify_xattr_errno_absent_vs_unexpected() {
        // ENODATA, ENOTSUP, EOPNOTSUPP => Absent (fallback ok)
        assert!(matches!(
            classify_xattr_errno(libc::ENODATA),
            XattrReadOutcome::Absent
        ));
        assert!(matches!(
            classify_xattr_errno(libc::ENOTSUP),
            XattrReadOutcome::Absent
        ));
        assert!(matches!(
            classify_xattr_errno(libc::EOPNOTSUPP),
            XattrReadOutcome::Absent
        ));

        // Everything else => OsError (fail-safe hidden)
        assert!(matches!(
            classify_xattr_errno(libc::EACCES),
            XattrReadOutcome::OsError(libc::EACCES)
        ));
        assert!(matches!(
            classify_xattr_errno(libc::EIO),
            XattrReadOutcome::OsError(libc::EIO)
        ));
        assert!(matches!(
            classify_xattr_errno(libc::ERANGE),
            XattrReadOutcome::OsError(libc::ERANGE)
        ));
        assert!(matches!(
            classify_xattr_errno(libc::EPERM),
            XattrReadOutcome::OsError(libc::EPERM)
        ));
    }
}
