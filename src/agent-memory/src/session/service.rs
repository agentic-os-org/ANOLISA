use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::Utc;
use serde::{Deserialize, Serialize};

use super::base::SessionBase;
use super::id::SessionId;
use crate::audit::AuditEntry;
use crate::error::{MemoryError, Result};
use crate::ns::MountPoint;

const SCRATCH_DIR: &str = "scratch";
const META_FILE: &str = "meta.toml";
const LOG_FILE: &str = "log.jsonl";

/// On-disk shape of `<session>/meta.toml`. Serialized through the toml
/// crate so user-supplied values (owner_user_id from env) cannot break
/// out of the string and inject keys.
#[derive(Serialize)]
struct SessionMeta {
    sid: String,
    owner_user_id: String,
    agent_id: String,
    created_at: String,
    mount_ns: String,
}

/// What to do with a session directory when the process exits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EndAction {
    /// Recursively delete the session directory (default).
    #[default]
    Discard,
    /// Leave the directory on disk — useful for post-mortem inspection.
    Keep,
}

/// Per-process session scratch + log.
pub struct SessionLogService {
    sid: SessionId,
    /// Keeps the base directory's descriptor open. `root`, `scratch` and
    /// `log_path` are all built on `/proc/self/fd/<n>`, which only resolves
    /// while this is alive — dropping it would dangle every path below and
    /// let the descriptor number be recycled to an unrelated file.
    base: SessionBase,
    /// `<anchored base>/<sid>` — the path every filesystem operation uses.
    root: PathBuf,
    /// `<display base>/<sid>` — the same directory by the name an operator
    /// recognises. Reporting only.
    display_root: PathBuf,
    scratch: PathBuf,
    log_path: PathBuf,
    /// Held file handle for jsonl appends — avoids repeated open/close.
    log_file: Mutex<File>,
    /// Persistent mirror of the session log, written to
    /// `<mount>/.anolisa/session-logs/<sid>.jsonl`. Survives SIGKILL and
    /// tmpfs loss. Best-effort — failures are logged but never propagate.
    mirror_file: Mutex<Option<File>>,
}

impl SessionLogService {
    /// Create a new session directory under `base_dir/<sid>/`. Writes
    /// `meta.toml` and ensures `scratch/` exists.
    ///
    /// `mirror_dir` is the persistent directory where session log entries
    /// are mirrored (typically `<mount>/.anolisa/session-logs/`). This
    /// ensures session data survives SIGKILL and tmpfs loss. Pass `None`
    /// to disable mirroring.
    pub fn start(
        base_dir: impl AsRef<Path>,
        sid: SessionId,
        owner_user_id: &str,
        agent_id: Option<&str>,
        mount_ns: &str,
        mirror_dir: Option<&Path>,
    ) -> Result<Self> {
        Self::start_in(
            SessionBase::open_creating(base_dir.as_ref())?,
            sid,
            owner_user_id,
            agent_id,
            mount_ns,
            mirror_dir,
        )
    }

    /// Same as [`Self::start`], but on a base that has already been opened
    /// and validated. The session tree is built through that one
    /// descriptor, so nothing that happens to the base's *pathname* between
    /// validation and here can redirect it.
    pub fn start_in(
        base: SessionBase,
        sid: SessionId,
        owner_user_id: &str,
        agent_id: Option<&str>,
        mount_ns: &str,
        mirror_dir: Option<&Path>,
    ) -> Result<Self> {
        let root = base.path().join(sid.as_str());
        let display_root = base.display_path().join(sid.as_str());
        let scratch = root.join(SCRATCH_DIR);
        let log_path = root.join(LOG_FILE);

        // `<sid>` is created with mkdirat against the base descriptor: the
        // one step where a swapped pathname would matter most, and the one
        // that can be done without touching the pathname at all.
        base.mkdir_child(sid.as_str(), 0o700)?;
        std::fs::create_dir_all(&scratch)?;
        // Enforce 0700 on session root so only the owner can read
        // meta.toml (owner_user_id, agent_id, mount_ns) and log.jsonl
        // (per-tool-call path/bytes/errors). ULID session ids provide
        // entropy but are not a substitute for filesystem ACLs.
        std::fs::set_permissions(&root, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;

        // Serialize meta through the toml crate so quote / newline / TOML
        // special characters in owner_user_id (env-derived, user-supplied)
        // can't break the file syntax or inject extra keys.
        let meta_struct = SessionMeta {
            sid: sid.to_string(),
            owner_user_id: owner_user_id.to_string(),
            agent_id: agent_id.unwrap_or("unknown").to_string(),
            created_at: Utc::now().to_rfc3339(),
            mount_ns: mount_ns.to_string(),
        };
        let meta = toml::to_string(&meta_struct)
            .map_err(|e| MemoryError::Other(format!("serialize session meta: {e}")))?;
        std::fs::write(root.join(META_FILE), meta)?;

        // Pre-touch the log so reading it during an empty session yields "" not NotFound.
        // O_NOFOLLOW refuses to reopen if log.jsonl has been swapped for a
        // symlink (defense-in-depth — /run/anolisa/sessions is meant to be
        // owner-only but the trust chain shouldn't rely on filesystem ACLs alone).
        let log_file = OpenOptions::new()
            .create(true)
            .append(true)
            .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
            .open(&log_path)?;

        // Open persistent mirror (best-effort — failure doesn't block session start).
        let mirror_file = mirror_dir.and_then(|dir| {
            if std::fs::create_dir_all(dir).is_err() {
                tracing::warn!("failed to create session log mirror dir: {}", dir.display());
                return None;
            }
            let mirror_path = dir.join(format!("{}.jsonl", sid.as_str()));
            match OpenOptions::new()
                .create(true)
                .append(true)
                .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
                .open(&mirror_path)
            {
                Ok(f) => {
                    tracing::debug!("session log mirror: {}", mirror_path.display());
                    Some(f)
                }
                Err(e) => {
                    tracing::warn!(
                        "session log mirror unavailable at {}: {e}",
                        mirror_path.display()
                    );
                    None
                }
            }
        });

        Ok(Self {
            sid,
            base,
            root,
            display_root,
            scratch,
            log_path,
            log_file: Mutex::new(log_file),
            mirror_file: Mutex::new(mirror_file),
        })
    }

    pub fn sid(&self) -> &SessionId {
        &self.sid
    }

    /// The session directory as paths see it: anchored to the descriptor the
    /// base was validated through, so nothing that happens to the base's
    /// pathname can redirect what is read or written here.
    ///
    /// Valid only while this service is alive — the descriptor backing it is
    /// dropped with the service. Use [`Self::display_root`] for anything
    /// that has to outlive it (logs, a path shown to an operator).
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The same directory by the pathname an operator configured or the
    /// fallback message reported. Use this in logs; use [`Self::root`] for
    /// anything that touches the filesystem.
    pub fn display_root(&self) -> &Path {
        &self.display_root
    }

    /// The base directory this session lives under.
    ///
    /// Mostly here to be explicit about why the field exists at all: holding
    /// the [`SessionBase`] is what keeps the descriptor open, and therefore
    /// what keeps every path above resolving to the directory that was
    /// validated rather than to whatever the pathname points at now.
    pub fn base(&self) -> &SessionBase {
        &self.base
    }

    pub fn scratch_root(&self) -> &Path {
        &self.scratch
    }

    pub fn log_path(&self) -> &Path {
        &self.log_path
    }

    /// Append one jsonl line to the session log AND the persistent mirror.
    /// The mirror write is best-effort — failures are logged but never
    /// propagate, so the foreground tool call is never blocked.
    pub fn append_log(&self, entry: AuditEntry) -> Result<()> {
        let line = serde_json::to_string(&entry)? + "\n";
        let mut f = self.log_file.lock().unwrap_or_else(|e| e.into_inner());
        f.write_all(line.as_bytes())?;
        f.flush()?;

        // Mirror to persistent storage (best-effort).
        if let Ok(mut guard) = self.mirror_file.lock() {
            if let Some(ref mut mf) = *guard {
                let _ = mf.write_all(line.as_bytes());
                let _ = mf.flush();
            }
        }

        Ok(())
    }

    /// Maximum session log size returned by `read_log`. Prevents unbounded
    /// memory allocation for long-running sessions.
    const MAX_SESSION_LOG_BYTES: u64 = 1_048_576; // 1 MiB

    /// Read the session jsonl log as a string (UTF-8), capped to
    /// `MAX_SESSION_LOG_BYTES`. When the log exceeds the cap, the most
    /// recent entries are returned (tail truncation) so the model can see
    /// what it has done most recently.
    pub fn read_log(&self) -> Result<String> {
        // Hold the write-side lock while reading so we get a consistent
        // snapshot (no half-written lines) — not to prevent concurrent
        // corruption, which O_APPEND already guarantees.
        let _g = self.log_file.lock().unwrap_or_else(|e| e.into_inner());
        let raw = std::fs::read(&self.log_path)?;
        if raw.len() <= Self::MAX_SESSION_LOG_BYTES as usize {
            return String::from_utf8(raw).map_err(|e| MemoryError::Other(e.to_string()));
        }
        // Return the tail: find a line boundary near the end of the cap.
        let cap = Self::MAX_SESSION_LOG_BYTES as usize;
        let start = raw[raw.len() - cap..]
            .iter()
            .position(|&b| b == b'\n')
            .map(|p| raw.len() - cap + p + 1)
            .unwrap_or(raw.len() - cap);
        String::from_utf8(raw[start..].to_vec()).map_err(|e| MemoryError::Other(e.to_string()))
    }

    /// Copy a file from `<scratch>/<src_in_scratch>` to `<mount.root>/<dst_in_store>`.
    /// Both paths are sandbox-checked; destination must not already exist.
    /// `max_bytes` caps the source file size to prevent unbounded memory allocation.
    /// Returns the number of bytes copied.
    pub fn promote(
        &self,
        src_in_scratch: &str,
        dst_in_store: &str,
        mount: &MountPoint,
        max_bytes: u64,
    ) -> Result<u64> {
        use std::os::fd::AsFd;
        use std::path::Path;

        let src = super::paths::resolve_in_scratch(self, src_in_scratch)?;
        if !src.exists() {
            return Err(MemoryError::NotFound(src_in_scratch.into()));
        }
        if !src.is_file() {
            return Err(MemoryError::InvalidArgument(format!(
                "scratch path '{src_in_scratch}' is not a file"
            )));
        }
        let src_size = std::fs::metadata(&src)?.len();
        if src_size > max_bytes {
            return Err(MemoryError::InvalidArgument(format!(
                "scratch file '{src_in_scratch}' is {} bytes, exceeds promote limit of {} bytes",
                src_size, max_bytes
            )));
        }

        let dst = crate::ns::paths::resolve_for_create(mount, dst_in_store)?;
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Route the destination through safe_fs: openat2(O_CREAT|O_EXCL)
        // gives us atomic "create new under sandbox" semantics, closing
        // both the symlink-TOCTOU and the exists()-then-write race.
        let rel = crate::ns::paths::relative_to_mount(mount, &dst);
        let content = std::fs::read(&src)?;
        // Defense-in-depth: re-check size after read. The file may have grown
        // between the metadata check and this read (TOCTOU window).
        if content.len() as u64 > max_bytes {
            return Err(MemoryError::InvalidArgument(format!(
                "scratch file '{src_in_scratch}' grew to {} bytes during read, exceeds promote limit of {} bytes",
                content.len(),
                max_bytes
            )));
        }
        let bytes =
            crate::safe_fs::write_create_new(mount.root_fd.as_fd(), Path::new(&rel), &content)?;
        Ok(bytes)
    }

    /// Tear down the session directory.
    pub fn end(self, action: EndAction) -> Result<()> {
        match action {
            EndAction::Keep => Ok(()),
            EndAction::Discard => {
                if self.root.exists() {
                    std::fs::remove_dir_all(&self.root)?;
                }
                Ok(())
            }
        }
    }
}
