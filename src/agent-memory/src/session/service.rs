use std::fs::{File, OpenOptions};
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
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

/// Owner-only modes for the persistent mirror: the directory holding the
/// `<sid>.jsonl` files, and the files themselves. The mirror carries the same
/// per-tool-call history the session root is created 0700 for.
const MIRROR_DIR_MODE: u32 = 0o700;
const MIRROR_FILE_MODE: u32 = 0o600;

/// The bits that would let another local user read the mirror.
const GROUP_OR_OTHER_BITS: u32 = 0o077;

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

/// Create (or tighten) the mirror directory and open `mirror_path` to append.
///
/// Both end up owner-only. `create_dir_all` would leave the directory at
/// 0777 & ~umask — 0755 under the usual umask, so any local user could list
/// the mirrored session ids and read the logs — and `OpenOptions::mode` only
/// applies when the file is created, so an existing directory or a mirror an
/// older version left world-readable is chmod-ed as well. The file mode is
/// set through the descriptor the `O_NOFOLLOW` open returned, so tightening it
/// cannot be redirected to another path.
///
/// Both chmods are read back: a mount that accepts one without applying it
/// would otherwise report a private mirror and keep writing a readable one.
fn open_mirror(dir: &Path, mirror_path: &Path) -> std::io::Result<File> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(MIRROR_DIR_MODE)
        .create(dir)?;
    std::fs::set_permissions(dir, PermissionsExt::from_mode(MIRROR_DIR_MODE))?;
    check_owner_only(
        std::fs::metadata(dir)?.permissions().mode(),
        MIRROR_DIR_MODE,
        dir,
    )?;

    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(MIRROR_FILE_MODE)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
        .open(mirror_path)?;
    // `File::set_permissions` and `File::metadata` are `fchmod`/`fstat` on the
    // descriptor the open returned, so neither the tightening nor the
    // read-back can be redirected to another path.
    file.set_permissions(PermissionsExt::from_mode(MIRROR_FILE_MODE))?;
    check_owner_only(
        file.metadata()?.permissions().mode(),
        MIRROR_FILE_MODE,
        mirror_path,
    )?;
    Ok(file)
}

/// Fail unless `mode` — what the inode actually has, read back after a chmod —
/// leaves nothing for group or other.
///
/// A successful `chmod` is not proof the mode changed: some FUSE and network
/// mounts accept the call without applying it. `probe_session_base` reads a
/// session base's mode back for the same reason.
fn check_owner_only(mode: u32, want: u32, what: &Path) -> std::io::Result<()> {
    if mode & GROUP_OR_OTHER_BITS == 0 {
        return Ok(());
    }
    Err(std::io::Error::other(format!(
        "{} is still mode {:o} after chmod to {want:o}; group and other can read it",
        what.display(),
        mode & 0o777
    )))
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
        std::fs::set_permissions(&root, PermissionsExt::from_mode(0o700))?;

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

        // Open persistent mirror (best-effort — failure doesn't block session
        // start, but a mirror we cannot keep owner-only is not opened at all).
        let mirror_file = mirror_dir.and_then(|dir| {
            let mirror_path = dir.join(format!("{}.jsonl", sid.as_str()));
            match open_mirror(dir, &mirror_path) {
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
    /// what it has done most recently. A most-recent entry that alone
    /// exceeds the cap degrades to its final `cap` bytes (a partial
    /// line the jsonl parser tolerates), never to an empty string.
    pub fn read_log(&self) -> Result<String> {
        // Hold the write-side lock while reading so we get a consistent
        // snapshot (no half-written lines) — not to prevent concurrent
        // corruption, which O_APPEND already guarantees. The held log_file
        // handle is opened append/write-only, so the actual reads go through
        // a fresh read-only view of the same anchored path.
        let _g = self.log_file.lock().unwrap_or_else(|e| e.into_inner());
        let cap = Self::MAX_SESSION_LOG_BYTES;
        let mut f = std::fs::File::open(&self.log_path)?;
        let len = f.metadata()?.len();

        if len <= cap {
            // Whole log: already bounded by the cap, nothing to truncate.
            // `take` keeps the allocation bounded even if the file grows
            // between the metadata call and this read.
            let mut raw = Vec::new();
            (&mut f).take(cap).read_to_end(&mut raw)?;
            return String::from_utf8(raw).map_err(|e| MemoryError::Other(e.to_string()));
        }

        // Read only the last `cap` bytes instead of the whole file first:
        // the cap exists to bound memory for long-running sessions, and a
        // full `fs::read` would transiently allocate the entire log before
        // the tail is cut out of it. `Start` + `take` (rather than
        // `End(-cap)` + `read_exact`) also stays correct when the file
        // shrank between the metadata call and the read: the seek cannot
        // produce a negative offset and a short read is not an error.
        f.seek(SeekFrom::Start(len.saturating_sub(cap)))?;
        let mut window = Vec::with_capacity(cap as usize);
        (&mut f).take(cap).read_to_end(&mut window)?;

        // Return whole entries: skip to the first line boundary inside the
        // window. Two degenerate shapes fall back to the windowed bytes:
        // a most-recent entry larger than the cap leaves no boundary at
        // all, and a *complete* oversized entry puts its sole `\n` at the
        // window's last byte — accepting that boundary would yield the
        // empty string and silently disable consolidation. In both cases
        // skip only the leading bytes of any UTF-8 sequence the window
        // start may have split mid-character, which would otherwise make
        // the tail fail `from_utf8` instead of degrading to a shorter
        // (partial-line) string.
        let start = window
            .iter()
            .position(|&b| b == b'\n')
            .map(|p| p + 1)
            .filter(|&s| s < window.len())
            .unwrap_or_else(|| {
                let mut s = 0;
                while s < window.len() && (window[s] & 0xC0) == 0x80 {
                    s += 1;
                }
                s
            });
        String::from_utf8(window[start..].to_vec()).map_err(|e| MemoryError::Other(e.to_string()))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionId;
    use tempfile::tempdir;

    fn setup() -> (tempfile::TempDir, SessionLogService) {
        let tmp = tempdir().unwrap();
        let svc = SessionLogService::start(
            tmp.path(),
            SessionId::from_string("ses_test").unwrap(),
            "alice",
            Some("test"),
            "user-alice",
            None,
        )
        .unwrap();
        (tmp, svc)
    }

    #[test]
    fn read_log_returns_whole_small_log() {
        let (_t, svc) = setup();
        svc.append_log(crate::audit::AuditEntry::new("mem_read").path("a.md"))
            .unwrap();
        svc.append_log(crate::audit::AuditEntry::new("mem_write").path("b.md"))
            .unwrap();

        let text = svc.read_log().unwrap();
        // The ≤-cap path must return the entire log — including the oldest
        // entry — not just a tail window.
        assert_eq!(text.lines().count(), 2);
        assert!(text.contains("mem_read"));
        assert!(text.contains("mem_write"));
    }

    #[test]
    fn read_log_tail_keeps_whole_recent_lines() {
        let (_t, svc) = setup();
        let cap = SessionLogService::MAX_SESSION_LOG_BYTES as usize;
        // ~1093-byte lines; 1100 of them exceed the cap. The returned tail
        // must start at a line boundary and contain the newest entries.
        let mut body = String::new();
        for i in 0..1100 {
            body.push_str(&format!("entry-{i:05} {}\n", "x".repeat(1080)));
        }
        assert!(body.len() > cap);
        std::fs::write(svc.log_path(), body.as_bytes()).unwrap();

        let tail = svc.read_log().unwrap();
        assert!(tail.len() <= cap);
        assert!(
            tail.starts_with("entry-"),
            "tail must start at a whole line, got: {}",
            &tail[..tail.len().min(40)]
        );
        // Most recent entry survived truncation.
        assert!(tail.contains("entry-01099"));
    }

    #[test]
    fn read_log_tail_never_splits_a_utf8_sequence() {
        let (_t, svc) = setup();
        // Regression: the tail window used to be returned verbatim from
        // byte `len - cap` even when no line boundary existed inside it.
        // With one giant entry built from 2-byte UTF-8 chars the window
        // provably opens mid-sequence (any odd offset into an `é` run is a
        // continuation byte), so `read_log` failed with an invalid-UTF-8
        // error instead of degrading to a shorter string.
        let cap = SessionLogService::MAX_SESSION_LOG_BYTES as usize;
        let giant = format!("x\n{}z", "é".repeat((cap - 2) / 2 + 8));
        std::fs::write(svc.log_path(), giant.as_bytes()).unwrap();

        let tail = svc
            .read_log()
            .expect("oversized single entry must degrade to a shorter tail, not error");
        assert!(tail.len() <= cap);
        assert!(tail.ends_with('z'), "tail must keep the newest bytes");
        // The split `é` was skipped; every remaining char is whole.
        assert!(tail.chars().all(|c| c == 'é' || c == 'z'));
    }

    #[test]
    fn read_log_returns_partial_tail_for_a_complete_oversized_entry() {
        let (_t, svc) = setup();
        // Regression: a *complete* oversized entry ends with the very `\n`
        // `append_log` writes, so the window's only boundary sits at its
        // last byte and accepting it returned `Ok("")` — consolidation was
        // silently skipped and `mem_session_log` told the model its log
        // was empty. The tail must instead degrade to the entry's final
        // bytes (a partial line the jsonl parser tolerates). Built through
        // `append_log` so the real line shape — serde-escaped newlines,
        // trailing `\n` — is what the window sees.
        let giant_error = "e".repeat(SessionLogService::MAX_SESSION_LOG_BYTES as usize + 4096);
        for i in 0..5 {
            let entry = crate::audit::AuditEntry::new("mem_read")
                .path(format!("notes/small-{i}.md"))
                .ok(true);
            svc.append_log(entry).unwrap();
        }
        let giant = crate::audit::AuditEntry::new("mem_write")
            .path("notes/giant.md")
            .error(giant_error);
        svc.append_log(giant).unwrap();

        let tail = svc
            .read_log()
            .expect("a complete oversized entry must yield a partial tail, not an error");
        assert!(
            !tail.is_empty(),
            "the oversized entry must not collapse to an empty read"
        );
        assert!(tail.len() <= SessionLogService::MAX_SESSION_LOG_BYTES as usize);
        assert!(tail.ends_with('\n'), "append_log lines end with a newline");
        // The partial line is valid UTF-8 by construction here; the small
        // entries before it are gone (they are outside the window).
        assert!(!tail.contains("notes/small-4.md"));
    }

    #[test]
    fn check_owner_only_accepts_owner_only_modes() {
        for mode in [0o400, 0o500, 0o600, 0o700] {
            let what = Path::new("/m/.anolisa/session-logs/ses_x.jsonl");
            assert!(
                check_owner_only(mode, mode, what).is_ok(),
                "{mode:o} is owner-only"
            );
        }
    }

    #[test]
    fn check_owner_only_rejects_a_chmod_that_did_not_apply() {
        // A mount that accepted the chmod without applying it reports the old
        // mode back. That has to fail instead of looking fixed.
        let what = Path::new("/m/.anolisa/session-logs/ses_x.jsonl");
        for mode in [0o604, 0o640, 0o644, 0o660, 0o755] {
            let err = check_owner_only(mode, 0o600, what)
                .err()
                .unwrap_or_else(|| panic!("{mode:o} is readable beyond its owner"));
            let msg = err.to_string();
            assert!(msg.contains("ses_x.jsonl"), "{msg}");
            assert!(msg.contains("600"), "{msg}");
        }

        // The message names the mode that was read back, in octal.
        let msg = check_owner_only(0o644, 0o600, what)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_else(|| panic!("{} is readable beyond its owner", what.display()));
        assert!(msg.contains("644"), "{msg}");
    }
}
