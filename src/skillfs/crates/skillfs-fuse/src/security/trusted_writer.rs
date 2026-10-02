//! Trusted ledger writer process gate.
//!
//! Ordinary mount-path callers must remain unable to mutate
//! `.skill-meta/**`; that is the S1 default and the security
//! invariant. At the same time the configured ledger provider needs
//! a way to write manifests, scan results, and version snapshots into
//! `.skill-meta/**` from outside the agent sandbox. The two
//! requirements conflict unless SkillFS can identify the ledger writer
//! at the FUSE-callback boundary.
//!
//! ## Identity modes
//!
//! **Production (recommended): `--trusted-writer-exe <PATH>`**
//!
//! Matches the FUSE caller's executable via `/proc/<tgid>/exe`
//! readlink. The configured path is canonicalized at startup and its
//! on-disk file identity `(dev, ino)` is pinned. Each request
//! compares the resolved exe path and file identity against the
//! pinned values, plus starttime for PID reuse defense. This is
//! resistant to `prctl(PR_SET_NAME)` spoofing and same-basename
//! binary substitution.
//!
//! **Compatibility (deprecated): `--trusted-writer <NAME>`**
//!
//! Matches the FUSE caller's process `comm` (via
//! `/proc/<tgid>/comm`). Process `comm` is spoofable by any local
//! process via `prctl(PR_SET_NAME)` or by exec'ing a binary whose
//! basename matches. This mode is retained for backward
//! compatibility only and should not be used in production.
//!
//! When both are configured, exe identity is the sole authorization
//! basis; comm is logged for context but does not influence the
//! allow/deny decision.
//!
//! ## Scope
//!
//! This module ships an operator-configured identity gate with
//! starttime verification, scoped strictly to `.skill-meta/**`
//! mutation. It is intentionally narrow:
//!
//! * Default disabled. Without a configured trusted writer name the
//!   gate denies everyone, matching the pre-existing
//!   [`super::policy::SkillMetaProtectionPolicy`] behavior bit-for-bit.
//! * Linux-only identity resolution. The FUSE-supplied `pid` is
//!   actually a kernel-side TID; the resolver dereferences it through
//!   `/proc/<pid>/status` to its `Tgid`, then reads
//!   `/proc/<tgid>/comm`. Single-threaded ledger invocations have
//!   `TID == TGID` so the extra hop is a no-op for the common path; for
//!   multi-threaded callers it pins identity to the binary's comm
//!   rather than the per-thread name a worker might be carrying.
//!   Non-Linux targets always resolve to `None`, which the policy
//!   treats as deny-by-default.
//! * The bypass applies only to `.skill-meta/**` mutation that
//!   [`super::policy::SkillMetaProtectionPolicy`] would otherwise deny.
//!   It does **not** touch lifecycle reserved roots, the `skill-discover`
//!   virtual namespace, virtual paths, cross-skill policy, xattr policy,
//!   symlink/link/FIFO policy, or any non-`.skill-meta` write surface.
//! * The bypass is observable. SkillFS emits an audit
//!   [`super::event::SkillEventKind::PolicyDecision`] (allowed) record
//!   with `trusted_writer=<name>` folded into the existing `detail`
//!   string so the JSONL audit shape stays unchanged.
//!
//! Known limitations (deliberate, documented for the security review):
//!
//! * **Compatibility comm gate** (`--trusted-writer`): process `comm`
//!   can be spoofed via `prctl(PR_SET_NAME)` or same-basename exec.
//!   Not a production identity.
//! * **Exe identity** (`--trusted-writer-exe`): pins the on-disk
//!   binary via `(dev, ino)`. A replaced binary (different inode) is
//!   rejected, but an in-place overwrite to the same inode is not
//!   detected by the file identity alone. Still not remote
//!   attestation.
//! * Root / `sudo` invocation does **not** bypass the gate
//!   automatically. The configured identity still has to match.
//! * If the FUSE request's pid cannot be resolved (kernel races,
//!   `/proc` not mounted, pid namespace mismatch), the gate denies —
//!   there is no open-on-fail relaxation.
//! * Linux is the only supported `proc` substrate. Other Unix targets
//!   compile cleanly but always deny.
//!
//! Future hardening directions:
//!
//! * Move ledger writes off the FUSE mount path entirely and onto a
//!   dedicated daemon socket authenticated via `SO_PEERCRED` /
//!   `SCM_CREDENTIALS`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;

/// Operator-supplied process-name gate for `.skill-meta/**` mutation.
///
/// Default constructed via [`TrustedWriterConfig::disabled`] (or
/// [`Default::default`]). [`TrustedWriterConfig::with_process_name`] is
/// the only way to enable it.
///
/// `expected_process_name` is compared byte-for-byte against the value
/// resolved by the configured [`ProcessIdentityResolver`]. The Linux
/// resolver follows the FUSE caller TID to its TGID, then returns
/// `/proc/<tgid>/comm` minus its trailing newline, which is at most
/// 15 bytes — operators should configure an identity within that limit.
///
/// When starttime is available, the gate caches `(pid, starttime)` on
/// first match and denies subsequent requests from the same pid if the
/// starttime changed (PID reuse defense).
#[derive(Debug, Default)]
pub struct TrustedWriterConfig {
    /// Compatibility / deprecated process-name identity.
    expected_process_name: Option<String>,
    /// Production executable identity: canonical path.
    expected_exe_path: Option<PathBuf>,
    /// Production executable identity: `(dev, ino)` from startup stat.
    expected_exe_file_id: Option<FileId>,
    pinned: Mutex<HashMap<u32, u64>>,
}

impl TrustedWriterConfig {
    pub fn disabled() -> Self {
        Self {
            expected_process_name: None,
            expected_exe_path: None,
            expected_exe_file_id: None,
            pinned: Mutex::new(HashMap::new()),
        }
    }

    /// Compatibility gate: match by process `comm` only.
    /// Empty strings are treated as disabled.
    pub fn with_process_name(name: impl Into<String>) -> Self {
        let name = name.into();
        if name.trim().is_empty() {
            return Self::disabled();
        }
        Self {
            expected_process_name: Some(name),
            expected_exe_path: None,
            expected_exe_file_id: None,
            pinned: Mutex::new(HashMap::new()),
        }
    }

    /// Production gate: match by executable file identity.
    pub fn with_executable(path: PathBuf, file_id: FileId) -> Self {
        Self {
            expected_process_name: None,
            expected_exe_path: Some(path),
            expected_exe_file_id: Some(file_id),
            pinned: Mutex::new(HashMap::new()),
        }
    }

    /// Production gate with compatibility comm context (for logging).
    /// The exe identity is the authorization basis; comm is log-only.
    pub fn with_executable_and_compat_name(
        path: PathBuf,
        file_id: FileId,
        name: impl Into<String>,
    ) -> Self {
        let name = name.into();
        Self {
            expected_process_name: if name.trim().is_empty() {
                None
            } else {
                Some(name)
            },
            expected_exe_path: Some(path),
            expected_exe_file_id: Some(file_id),
            pinned: Mutex::new(HashMap::new()),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.expected_process_name.is_some() || self.expected_exe_path.is_some()
    }

    pub fn is_exe_enabled(&self) -> bool {
        self.expected_exe_path.is_some()
    }

    pub fn expected_process_name(&self) -> Option<&str> {
        self.expected_process_name.as_deref()
    }

    pub fn expected_exe_path(&self) -> Option<&Path> {
        self.expected_exe_path.as_deref()
    }

    pub fn expected_exe_file_id(&self) -> Option<FileId> {
        self.expected_exe_file_id
    }
}

/// On-disk file identity: `(dev, ino)` from `stat`. Used to pin a
/// specific executable binary so the gate survives path renames.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileId {
    pub dev: u64,
    pub ino: u64,
}

impl std::fmt::Display for FileId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "dev={},ino={}", self.dev, self.ino)
    }
}

/// Resolved process identity for the trusted-writer gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessIdentity {
    pub comm: String,
    pub starttime: Option<u64>,
    pub exe_path: Option<PathBuf>,
    pub exe_file_id: Option<FileId>,
}

/// Indirection between the FUSE callback layer and the host's pid →
/// process identity lookup so the policy is unit-testable without
/// spawning real processes.
///
/// Implementations must be cheap, non-blocking, and side-effect free
/// — they run on the FUSE thread.
pub trait ProcessIdentityResolver: Send + Sync + 'static {
    /// Best-effort lookup. Returns `None` when the identity cannot be
    /// determined. The policy treats `None` as deny.
    fn resolve_identity(&self, pid: u32) -> Option<ProcessIdentity>;

    /// Convenience wrapper returning just the comm name.
    fn resolve_process_name(&self, pid: u32) -> Option<String> {
        self.resolve_identity(pid).map(|id| id.comm)
    }
}

/// Linux resolver based on `/proc/<tgid>/comm`.
///
/// FUSE callbacks pass the kernel-side `pid` of the requesting task,
/// which on Linux is a per-thread TID rather than a process group
/// identifier. The gate cares about *process* identity — a
/// per-thread comm can be set independently via
/// `pthread_setname_np`, so reading the TID's comm is both noisy
/// (cargo test renames worker threads, tokio worker threads carry
/// `tokio-runtime-w*`) and weaker as an identity signal. The
/// resolver therefore first dereferences `pid` to its `Tgid` via
/// `/proc/<pid>/status`, then reads `/proc/<tgid>/comm`. Single-
/// threaded ledger invocations have `TID == TGID == PID`, so the
/// extra step is a no-op for the common path.
///
/// The returned string is the comm file's contents with any trailing
/// newline stripped. On non-Linux targets the resolver compiles
/// cleanly but always returns `None`, which the gate treats as
/// deny-by-default.
#[derive(Debug, Default, Clone, Copy)]
pub struct LinuxProcCommResolver;

impl LinuxProcCommResolver {
    pub fn new() -> Self {
        Self
    }

    /// `/proc/<tgid>/comm` path on Linux. Exposed as a helper so
    /// tests can pin the path shape without depending on a live
    /// `/proc`.
    #[cfg(target_os = "linux")]
    pub fn comm_path(tgid: u32) -> PathBuf {
        PathBuf::from(format!("/proc/{tgid}/comm"))
    }

    /// `/proc/<pid>/status` path on Linux. Exposed as a helper so
    /// tests can pin the path shape and the parser can be unit-tested
    /// without `/proc`.
    #[cfg(target_os = "linux")]
    pub fn status_path(pid: u32) -> PathBuf {
        PathBuf::from(format!("/proc/{pid}/status"))
    }
}

impl LinuxProcCommResolver {
    #[cfg(target_os = "linux")]
    pub fn stat_path(tgid: u32) -> PathBuf {
        PathBuf::from(format!("/proc/{tgid}/stat"))
    }

    #[cfg(target_os = "linux")]
    pub fn exe_symlink_path(tgid: u32) -> PathBuf {
        PathBuf::from(format!("/proc/{tgid}/exe"))
    }
}

impl ProcessIdentityResolver for LinuxProcCommResolver {
    #[cfg(target_os = "linux")]
    fn resolve_identity(&self, pid: u32) -> Option<ProcessIdentity> {
        use std::os::unix::fs::MetadataExt;
        let tgid = read_tgid_from_status(&Self::status_path(pid))?;
        let comm = read_comm_file(&Self::comm_path(tgid))?;
        let starttime = read_starttime_from_stat(&Self::stat_path(tgid));
        let exe_path = std::fs::read_link(Self::exe_symlink_path(tgid)).ok();
        let exe_file_id = exe_path.as_ref().and_then(|p| {
            let meta = std::fs::metadata(p).ok()?;
            Some(FileId {
                dev: meta.dev(),
                ino: meta.ino(),
            })
        });
        Some(ProcessIdentity {
            comm,
            starttime,
            exe_path,
            exe_file_id,
        })
    }

    #[cfg(not(target_os = "linux"))]
    fn resolve_identity(&self, _pid: u32) -> Option<ProcessIdentity> {
        None
    }
}

/// Parse a `Tgid:` line out of a `/proc/<pid>/status`-shaped file and
/// return the TGID it advertises. `None` on missing file, missing
/// line, malformed integer, or empty file.
///
/// Public so tests can drive the parser against a temporary file
/// without involving `/proc`.
pub fn read_tgid_from_status(path: &Path) -> Option<u32> {
    let text = std::fs::read_to_string(path).ok()?;
    parse_tgid_from_status_text(&text)
}

/// Pure parser for a `/proc/<pid>/status`-shaped string.
///
/// Splits the input by newlines, finds the line starting with
/// `Tgid:`, and parses the second whitespace-separated token as a
/// `u32`.
fn parse_tgid_from_status_text(text: &str) -> Option<u32> {
    let line = text.lines().find(|l| l.starts_with("Tgid:"))?;
    let token = line.split_whitespace().nth(1)?;
    token.parse::<u32>().ok()
}

/// Parse starttime (field 22, 0-indexed) from `/proc/<tgid>/stat`.
///
/// The `stat` file is a single line with space-separated fields. Field 2
/// is the comm wrapped in parentheses (which may contain spaces), so we
/// skip past the closing `)` and then count fields from there.
/// Field 22 (1-indexed) is starttime in clock ticks since boot.
/// Returns `None` on missing file, parse error, or zombie state.
pub fn read_starttime_from_stat(path: &Path) -> Option<u64> {
    let text = std::fs::read_to_string(path).ok()?;
    parse_starttime_from_stat_text(&text)
}

fn parse_starttime_from_stat_text(text: &str) -> Option<u64> {
    // Find the closing ')' of the comm field (field 2).
    let after_comm = text.rfind(')')? + 1;
    let rest = text.get(after_comm..)?;
    // Fields after comm: state(3), ppid(4), ... starttime is field 22.
    // After the `)`, fields 3..N are space-separated. starttime is
    // the 20th token after `)` (field 22 - 2 = 20, but 0-indexed = 19).
    let token = rest.split_whitespace().nth(19)?;
    token.parse::<u64>().ok()
}

/// Read a `/proc/<tgid>/comm`-style file and normalize its contents.
///
/// Public so tests can drive the parser against a temporary file
/// without involving `/proc`. Returns `None` on missing file, I/O
/// error, non-UTF-8 contents, or empty result after newline stripping.
pub fn read_comm_file(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let mut s = String::from_utf8(bytes).ok()?;
    if s.ends_with('\n') {
        s.pop();
    }
    if s.is_empty() {
        return None;
    }
    Some(s)
}

/// Outcome of a trusted-writer gate evaluation.
///
/// The gate returns the same enum for every code path so the policy
/// site can render a deterministic audit `detail` string and tests can
/// pin the failure mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustedWriterDecision {
    Disabled,
    // --- Production: executable identity ---
    AllowedByExecutable { path: PathBuf, file_id: FileId },
    DeniedExecutableUnresolved,
    DeniedExecutableMismatch { expected: PathBuf, actual: PathBuf },
    DeniedExecutableFileIdMismatch { expected: FileId, actual: FileId },
    // --- Compatibility: process comm ---
    AllowedByName { name: String },
    DeniedIdentityUnresolved,
    DeniedNameMismatch { actual: String, expected: String },
    DeniedStarttimeMismatch { pid: u32, pinned: u64, actual: u64 },
}

impl TrustedWriterDecision {
    pub fn is_allowed(&self) -> bool {
        matches!(
            self,
            Self::AllowedByExecutable { .. } | Self::AllowedByName { .. }
        )
    }

    pub fn audit_label(&self) -> &'static str {
        match self {
            Self::Disabled => "trusted_writer_disabled",
            Self::AllowedByExecutable { .. } => "trusted_writer_exe_match",
            Self::DeniedExecutableUnresolved => "trusted_writer_exe_unresolved",
            Self::DeniedExecutableMismatch { .. } => "trusted_writer_exe_mismatch",
            Self::DeniedExecutableFileIdMismatch { .. } => "trusted_writer_exe_file_id_mismatch",
            Self::AllowedByName { .. } => "trusted_writer_name_match_compat",
            Self::DeniedIdentityUnresolved => "trusted_writer_identity_unresolved",
            Self::DeniedNameMismatch { .. } => "trusted_writer_name_mismatch",
            Self::DeniedStarttimeMismatch { .. } => "trusted_writer_starttime_mismatch",
        }
    }
}

/// Evaluate the trusted-writer gate for a single FUSE request.
///
/// When executable identity is configured, the gate matches the
/// caller's `/proc/<tgid>/exe` readlink against the configured
/// canonical path and `(dev, ino)` file identity. Process `comm` is
/// NOT consulted for authorization in exe mode. When only the
/// compatibility comm name is configured, the legacy flow applies.
///
/// On first match for a given `pid`, if the resolver provides a
/// `starttime`, the gate caches `(pid, starttime)` in
/// `config.pinned`. On subsequent calls from the same `pid`, if the
/// starttime changed the gate denies — the PID was reused by a
/// different process.
pub fn evaluate_trusted_writer(
    config: &TrustedWriterConfig,
    pid: u32,
    resolver: &dyn ProcessIdentityResolver,
) -> TrustedWriterDecision {
    if !config.is_enabled() {
        return TrustedWriterDecision::Disabled;
    }

    let identity = match resolver.resolve_identity(pid) {
        Some(id) => id,
        None => {
            return if config.is_exe_enabled() {
                TrustedWriterDecision::DeniedExecutableUnresolved
            } else {
                TrustedWriterDecision::DeniedIdentityUnresolved
            };
        }
    };

    if config.is_exe_enabled() {
        return evaluate_exe_identity(config, pid, &identity);
    }

    evaluate_comm_identity(config, pid, &identity)
}

fn evaluate_exe_identity(
    config: &TrustedWriterConfig,
    pid: u32,
    identity: &ProcessIdentity,
) -> TrustedWriterDecision {
    let expected_path = config.expected_exe_path().unwrap();
    let expected_fid = config.expected_exe_file_id().unwrap();

    let actual_path = match identity.exe_path.as_ref() {
        Some(p) => p,
        None => return TrustedWriterDecision::DeniedExecutableUnresolved,
    };
    let actual_fid = match identity.exe_file_id {
        Some(fid) => fid,
        None => return TrustedWriterDecision::DeniedExecutableUnresolved,
    };

    let actual_canon = std::fs::canonicalize(actual_path)
        .ok()
        .unwrap_or_else(|| actual_path.clone());

    if actual_canon != expected_path {
        return TrustedWriterDecision::DeniedExecutableMismatch {
            expected: expected_path.to_path_buf(),
            actual: actual_canon,
        };
    }

    if actual_fid != expected_fid {
        return TrustedWriterDecision::DeniedExecutableFileIdMismatch {
            expected: expected_fid,
            actual: actual_fid,
        };
    }

    if let Err(d) = check_starttime_pin(config, pid, identity) {
        return d;
    }

    TrustedWriterDecision::AllowedByExecutable {
        path: actual_canon,
        file_id: actual_fid,
    }
}

fn evaluate_comm_identity(
    config: &TrustedWriterConfig,
    pid: u32,
    identity: &ProcessIdentity,
) -> TrustedWriterDecision {
    let expected = config.expected_process_name().unwrap();

    if identity.comm != expected {
        return TrustedWriterDecision::DeniedNameMismatch {
            actual: identity.comm.clone(),
            expected: expected.to_string(),
        };
    }

    if let Err(d) = check_starttime_pin(config, pid, identity) {
        return d;
    }

    TrustedWriterDecision::AllowedByName {
        name: identity.comm.clone(),
    }
}

fn check_starttime_pin(
    config: &TrustedWriterConfig,
    pid: u32,
    identity: &ProcessIdentity,
) -> Result<(), TrustedWriterDecision> {
    if let Some(actual_st) = identity.starttime {
        let mut pinned = config.pinned.lock();
        match pinned.get(&pid) {
            Some(&cached_st) if cached_st != actual_st => {
                return Err(TrustedWriterDecision::DeniedStarttimeMismatch {
                    pid,
                    pinned: cached_st,
                    actual: actual_st,
                });
            }
            Some(_) => {}
            None => {
                pinned.insert(pid, actual_st);
            }
        }
    }
    Ok(())
}

/// Default identity resolver shared across the SkillFs runtime when
/// the operator has not provided one. Boxed `Arc` so SkillFs can hold
/// it next to the existing event sink and policy.
pub fn default_identity_resolver() -> Arc<dyn ProcessIdentityResolver> {
    Arc::new(LinuxProcCommResolver::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Test resolver that returns whatever the test stuffed into it.
    #[derive(Default)]
    struct StaticResolver {
        table: Mutex<std::collections::HashMap<u32, ProcessIdentity>>,
    }

    impl StaticResolver {
        fn insert(&self, pid: u32, name: impl Into<String>) {
            self.table.lock().unwrap().insert(
                pid,
                ProcessIdentity {
                    comm: name.into(),
                    starttime: None,
                    exe_path: None,
                    exe_file_id: None,
                },
            );
        }

        fn insert_with_starttime(&self, pid: u32, name: impl Into<String>, starttime: u64) {
            self.table.lock().unwrap().insert(
                pid,
                ProcessIdentity {
                    comm: name.into(),
                    starttime: Some(starttime),
                    exe_path: None,
                    exe_file_id: None,
                },
            );
        }

        fn insert_with_exe(
            &self,
            pid: u32,
            name: impl Into<String>,
            starttime: Option<u64>,
            exe_path: PathBuf,
            file_id: FileId,
        ) {
            self.table.lock().unwrap().insert(
                pid,
                ProcessIdentity {
                    comm: name.into(),
                    starttime,
                    exe_path: Some(exe_path),
                    exe_file_id: Some(file_id),
                },
            );
        }
    }

    impl ProcessIdentityResolver for StaticResolver {
        fn resolve_identity(&self, pid: u32) -> Option<ProcessIdentity> {
            self.table.lock().unwrap().get(&pid).cloned()
        }
    }

    #[test]
    fn default_config_is_disabled() {
        let cfg = TrustedWriterConfig::default();
        assert!(!cfg.is_enabled());
        assert!(cfg.expected_process_name().is_none());
    }

    #[test]
    fn empty_or_whitespace_process_name_is_normalized_to_disabled() {
        for name in ["", "   ", "\t", "\n"] {
            let cfg = TrustedWriterConfig::with_process_name(name);
            assert!(!cfg.is_enabled(), "name {name:?} must not enable the gate");
            assert!(cfg.expected_process_name().is_none());
        }
    }

    #[test]
    fn configured_process_name_is_round_trippable() {
        let cfg = TrustedWriterConfig::with_process_name("agent-sec-cli");
        assert!(cfg.is_enabled());
        assert_eq!(cfg.expected_process_name(), Some("agent-sec-cli"));
    }

    #[test]
    fn disabled_config_decides_disabled_regardless_of_resolver() {
        let resolver = StaticResolver::default();
        resolver.insert(42, "agent-sec-cli");
        let cfg = TrustedWriterConfig::disabled();
        let d = evaluate_trusted_writer(&cfg, 42, &resolver);
        assert_eq!(d, TrustedWriterDecision::Disabled);
        assert!(!d.is_allowed());
        assert_eq!(d.audit_label(), "trusted_writer_disabled");
    }

    #[test]
    fn matching_name_allows() {
        let resolver = StaticResolver::default();
        resolver.insert(42, "agent-sec-cli");
        let cfg = TrustedWriterConfig::with_process_name("agent-sec-cli");
        let d = evaluate_trusted_writer(&cfg, 42, &resolver);
        assert!(d.is_allowed());
        match d {
            TrustedWriterDecision::AllowedByName { name } => {
                assert_eq!(name, "agent-sec-cli");
            }
            other => panic!("expected AllowedByName, got {other:?}"),
        }
    }

    #[test]
    fn mismatching_name_denies() {
        let resolver = StaticResolver::default();
        resolver.insert(42, "bash");
        let cfg = TrustedWriterConfig::with_process_name("agent-sec-cli");
        let d = evaluate_trusted_writer(&cfg, 42, &resolver);
        assert!(!d.is_allowed());
        match d {
            TrustedWriterDecision::DeniedNameMismatch { actual, expected } => {
                assert_eq!(actual, "bash");
                assert_eq!(expected, "agent-sec-cli");
            }
            other => panic!("expected DeniedNameMismatch, got {other:?}"),
        }
    }

    #[test]
    fn unresolved_pid_denies() {
        let resolver = StaticResolver::default();
        // Pid 99 is not in the table.
        let cfg = TrustedWriterConfig::with_process_name("agent-sec-cli");
        let d = evaluate_trusted_writer(&cfg, 99, &resolver);
        assert_eq!(d, TrustedWriterDecision::DeniedIdentityUnresolved);
        assert!(!d.is_allowed());
        assert_eq!(d.audit_label(), "trusted_writer_identity_unresolved");
    }

    #[test]
    fn read_comm_file_strips_trailing_newline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("comm");
        std::fs::write(&path, "agent-sec-cli\n").unwrap();
        assert_eq!(
            read_comm_file(&path).as_deref(),
            Some("agent-sec-cli"),
            "newline must be stripped"
        );
    }

    #[test]
    fn read_comm_file_handles_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist");
        assert!(read_comm_file(&path).is_none());
    }

    #[test]
    fn read_comm_file_rejects_empty_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("comm");
        std::fs::write(&path, "").unwrap();
        assert!(read_comm_file(&path).is_none());
        std::fs::write(&path, "\n").unwrap();
        assert!(
            read_comm_file(&path).is_none(),
            "newline-only content must be rejected"
        );
    }

    #[test]
    fn read_comm_file_rejects_non_utf8() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("comm");
        std::fs::write(&path, [0xff, 0xfe, b'\n']).unwrap();
        assert!(read_comm_file(&path).is_none());
    }

    /// Sanity check that the `/proc/<tgid>/comm` path shape is what we
    /// document. The path is hard-coded by Linux; this test pins
    /// the formatter so a refactor cannot accidentally change it
    /// without tripping a unit test.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_proc_comm_path_format_is_pinned() {
        assert_eq!(
            LinuxProcCommResolver::comm_path(1234),
            std::path::PathBuf::from("/proc/1234/comm")
        );
    }

    /// Same pinning for the status path the resolver consults to
    /// follow a TID back to its TGID.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_proc_status_path_format_is_pinned() {
        assert_eq!(
            LinuxProcCommResolver::status_path(5678),
            std::path::PathBuf::from("/proc/5678/status")
        );
    }

    #[test]
    fn parse_tgid_from_status_text_extracts_tgid() {
        let text = "\
Name:\twriter
Umask:\t0022
State:\tR (running)
Tgid:\t4242
Ngid:\t0
Pid:\t4243
PPid:\t1
";
        assert_eq!(super::parse_tgid_from_status_text(text), Some(4242));
    }

    #[test]
    fn parse_tgid_from_status_text_handles_missing_line() {
        let text = "Name:\twriter\nPid:\t1\n";
        assert!(super::parse_tgid_from_status_text(text).is_none());
    }

    #[test]
    fn parse_tgid_from_status_text_rejects_malformed_value() {
        let text = "Tgid:\tnot-an-integer\n";
        assert!(super::parse_tgid_from_status_text(text).is_none());
    }

    #[test]
    fn read_tgid_from_status_handles_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("status");
        assert!(read_tgid_from_status(&path).is_none());
    }

    #[test]
    fn read_tgid_from_status_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("status");
        std::fs::write(&path, "Name:\twriter\nTgid:\t101\nPid:\t102\n").unwrap();
        assert_eq!(read_tgid_from_status(&path), Some(101));
    }

    /// The current process's own pid is always resolvable on Linux,
    /// so the live resolver should at least produce *some* name. We do
    /// not pin the value — different test runners (`cargo test`,
    /// custom harnesses) yield different `comm` strings — but a
    /// `Some(_)` result is enough to prove the resolver wires through.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_proc_comm_resolver_finds_self() {
        let resolver = LinuxProcCommResolver::new();
        let pid = std::process::id();
        let identity = resolver.resolve_identity(pid);
        assert!(
            identity.is_some(),
            "self-pid resolution must succeed on Linux; got {identity:?}"
        );
        let id = identity.unwrap();
        assert!(!id.comm.is_empty());
        assert!(
            id.starttime.is_some(),
            "starttime should be available for self"
        );
    }

    #[test]
    fn parse_starttime_from_stat_text_extracts_field_22() {
        // Real /proc/<pid>/stat line (comm may contain spaces/parens).
        let text = "4242 (agent-sec-cli) S 1 4242 4242 0 -1 4194304 100 0 0 0 10 5 0 0 20 0 1 0 123456789 12345678 100 18446744073709551615 0 0 0 0 0 0 0 0 0 0 0 0 17 0 0 0 0 0 0";
        assert_eq!(super::parse_starttime_from_stat_text(text), Some(123456789));
    }

    #[test]
    fn parse_starttime_from_stat_text_handles_comm_with_parens() {
        let text = "1234 (my (weird) proc) S 1 1234 1234 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 999 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0";
        assert_eq!(super::parse_starttime_from_stat_text(text), Some(999));
    }

    #[test]
    fn parse_starttime_from_stat_text_returns_none_for_truncated() {
        let text = "4242 (short) S 1";
        assert!(super::parse_starttime_from_stat_text(text).is_none());
    }

    #[test]
    fn read_starttime_from_stat_handles_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stat");
        assert!(read_starttime_from_stat(&path).is_none());
    }

    #[test]
    fn read_starttime_from_stat_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stat");
        std::fs::write(&path, "100 (test) S 1 100 100 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 42 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n").unwrap();
        assert_eq!(read_starttime_from_stat(&path), Some(42));
    }

    #[test]
    fn identity_with_starttime_resolves() {
        let resolver = StaticResolver::default();
        resolver.insert_with_starttime(42, "agent-sec-cli", 123456);
        let cfg = TrustedWriterConfig::with_process_name("agent-sec-cli");
        let d = evaluate_trusted_writer(&cfg, 42, &resolver);
        assert!(d.is_allowed());
    }

    #[test]
    fn starttime_pins_on_first_match_and_allows_repeat() {
        let resolver = StaticResolver::default();
        resolver.insert_with_starttime(42, "agent-sec-cli", 100);
        let cfg = TrustedWriterConfig::with_process_name("agent-sec-cli");
        assert!(evaluate_trusted_writer(&cfg, 42, &resolver).is_allowed());
        assert!(evaluate_trusted_writer(&cfg, 42, &resolver).is_allowed());
    }

    #[test]
    fn starttime_mismatch_denies_pid_reuse() {
        let resolver = StaticResolver::default();
        resolver.insert_with_starttime(42, "agent-sec-cli", 100);
        let cfg = TrustedWriterConfig::with_process_name("agent-sec-cli");
        assert!(evaluate_trusted_writer(&cfg, 42, &resolver).is_allowed());

        // PID 42 reused by a different process with the same comm but different starttime
        resolver.insert_with_starttime(42, "agent-sec-cli", 200);
        let d = evaluate_trusted_writer(&cfg, 42, &resolver);
        assert!(!d.is_allowed());
        assert_eq!(d.audit_label(), "trusted_writer_starttime_mismatch");
        match d {
            TrustedWriterDecision::DeniedStarttimeMismatch {
                pid,
                pinned,
                actual,
            } => {
                assert_eq!(pid, 42);
                assert_eq!(pinned, 100);
                assert_eq!(actual, 200);
            }
            other => panic!("expected DeniedStarttimeMismatch, got {other:?}"),
        }
    }

    #[test]
    fn no_starttime_skips_pin_verification() {
        let resolver = StaticResolver::default();
        resolver.insert(42, "agent-sec-cli");
        let cfg = TrustedWriterConfig::with_process_name("agent-sec-cli");
        assert!(evaluate_trusted_writer(&cfg, 42, &resolver).is_allowed());
        assert!(evaluate_trusted_writer(&cfg, 42, &resolver).is_allowed());
    }

    #[test]
    fn different_pids_pinned_independently() {
        let resolver = StaticResolver::default();
        resolver.insert_with_starttime(42, "agent-sec-cli", 100);
        resolver.insert_with_starttime(43, "agent-sec-cli", 200);
        let cfg = TrustedWriterConfig::with_process_name("agent-sec-cli");
        assert!(evaluate_trusted_writer(&cfg, 42, &resolver).is_allowed());
        assert!(evaluate_trusted_writer(&cfg, 43, &resolver).is_allowed());
    }

    // -----------------------------------------------------------------------
    // Executable identity tests
    // -----------------------------------------------------------------------

    fn test_exe_path() -> PathBuf {
        PathBuf::from("/usr/bin/agent-sec-cli")
    }

    fn test_file_id() -> FileId {
        FileId { dev: 100, ino: 200 }
    }

    #[test]
    fn exe_match_allows() {
        let resolver = StaticResolver::default();
        let path = test_exe_path();
        let fid = test_file_id();
        resolver.insert_with_exe(42, "agent-sec-cli", None, path.clone(), fid);
        let cfg = TrustedWriterConfig::with_executable(path.clone(), fid);
        let d = evaluate_trusted_writer(&cfg, 42, &resolver);
        assert!(d.is_allowed());
        match d {
            TrustedWriterDecision::AllowedByExecutable { file_id, .. } => {
                assert_eq!(file_id, fid);
            }
            other => panic!("expected AllowedByExecutable, got {other:?}"),
        }
        assert_eq!(d.audit_label(), "trusted_writer_exe_match");
    }

    #[test]
    fn exe_path_mismatch_denies() {
        let resolver = StaticResolver::default();
        let fid = test_file_id();
        resolver.insert_with_exe(
            42,
            "other",
            None,
            PathBuf::from("/usr/bin/other"),
            FileId { dev: 999, ino: 999 },
        );
        let cfg = TrustedWriterConfig::with_executable(test_exe_path(), fid);
        let d = evaluate_trusted_writer(&cfg, 42, &resolver);
        assert!(!d.is_allowed());
        assert_eq!(d.audit_label(), "trusted_writer_exe_mismatch");
    }

    #[test]
    fn exe_file_id_mismatch_denies() {
        let resolver = StaticResolver::default();
        let path = test_exe_path();
        let expected_fid = test_file_id();
        let actual_fid = FileId { dev: 100, ino: 999 };
        resolver.insert_with_exe(42, "agent-sec-cli", None, path.clone(), actual_fid);
        let cfg = TrustedWriterConfig::with_executable(path, expected_fid);
        let d = evaluate_trusted_writer(&cfg, 42, &resolver);
        assert!(!d.is_allowed());
        match d {
            TrustedWriterDecision::DeniedExecutableFileIdMismatch { expected, actual } => {
                assert_eq!(expected, expected_fid);
                assert_eq!(actual, actual_fid);
            }
            other => panic!("expected DeniedExecutableFileIdMismatch, got {other:?}"),
        }
    }

    #[test]
    fn exe_unresolved_denies() {
        let resolver = StaticResolver::default();
        resolver.insert(42, "agent-sec-cli");
        let cfg = TrustedWriterConfig::with_executable(test_exe_path(), test_file_id());
        let d = evaluate_trusted_writer(&cfg, 42, &resolver);
        assert!(!d.is_allowed());
        assert_eq!(d.audit_label(), "trusted_writer_exe_unresolved");
    }

    #[test]
    fn exe_identity_unresolved_pid_denies() {
        let resolver = StaticResolver::default();
        let cfg = TrustedWriterConfig::with_executable(test_exe_path(), test_file_id());
        let d = evaluate_trusted_writer(&cfg, 99, &resolver);
        assert_eq!(d, TrustedWriterDecision::DeniedExecutableUnresolved);
    }

    #[test]
    fn exe_configured_comm_match_but_exe_mismatch_denies() {
        let resolver = StaticResolver::default();
        let fid = test_file_id();
        resolver.insert_with_exe(
            42,
            "agent-sec-cli",
            None,
            PathBuf::from("/usr/bin/imposter"),
            FileId { dev: 1, ino: 1 },
        );
        let cfg = TrustedWriterConfig::with_executable_and_compat_name(
            test_exe_path(),
            fid,
            "agent-sec-cli",
        );
        let d = evaluate_trusted_writer(&cfg, 42, &resolver);
        assert!(
            !d.is_allowed(),
            "exe mismatch must deny even when comm matches"
        );
    }

    #[test]
    fn exe_and_comm_both_match_uses_exe_decision() {
        let resolver = StaticResolver::default();
        let path = test_exe_path();
        let fid = test_file_id();
        resolver.insert_with_exe(42, "agent-sec-cli", None, path.clone(), fid);
        let cfg = TrustedWriterConfig::with_executable_and_compat_name(path, fid, "agent-sec-cli");
        let d = evaluate_trusted_writer(&cfg, 42, &resolver);
        assert!(d.is_allowed());
        assert!(
            matches!(d, TrustedWriterDecision::AllowedByExecutable { .. }),
            "must be AllowedByExecutable, not AllowedByName"
        );
    }

    #[test]
    fn comm_only_still_works_as_compat() {
        let resolver = StaticResolver::default();
        resolver.insert(42, "agent-sec-cli");
        let cfg = TrustedWriterConfig::with_process_name("agent-sec-cli");
        let d = evaluate_trusted_writer(&cfg, 42, &resolver);
        assert!(d.is_allowed());
        assert_eq!(d.audit_label(), "trusted_writer_name_match_compat");
    }

    #[test]
    fn exe_with_starttime_mismatch_denies() {
        let resolver = StaticResolver::default();
        let path = test_exe_path();
        let fid = test_file_id();
        resolver.insert_with_exe(42, "agent-sec-cli", Some(100), path.clone(), fid);
        let cfg = TrustedWriterConfig::with_executable(path.clone(), fid);
        assert!(evaluate_trusted_writer(&cfg, 42, &resolver).is_allowed());

        resolver.insert_with_exe(42, "agent-sec-cli", Some(200), path, fid);
        let d = evaluate_trusted_writer(&cfg, 42, &resolver);
        assert!(!d.is_allowed());
        assert_eq!(d.audit_label(), "trusted_writer_starttime_mismatch");
    }

    #[test]
    fn exe_config_accessors() {
        let path = test_exe_path();
        let fid = test_file_id();
        let cfg = TrustedWriterConfig::with_executable(path.clone(), fid);
        assert!(cfg.is_enabled());
        assert!(cfg.is_exe_enabled());
        assert_eq!(cfg.expected_exe_path(), Some(path.as_path()));
        assert_eq!(cfg.expected_exe_file_id(), Some(fid));
        assert!(cfg.expected_process_name().is_none());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_resolver_populates_exe_fields() {
        let resolver = LinuxProcCommResolver::new();
        let pid = std::process::id();
        let identity = resolver.resolve_identity(pid).expect("self-resolution");
        assert!(identity.exe_path.is_some(), "exe_path must be populated");
        assert!(
            identity.exe_file_id.is_some(),
            "exe_file_id must be populated"
        );
    }
}
