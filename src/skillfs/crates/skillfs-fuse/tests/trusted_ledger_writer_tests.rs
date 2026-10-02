//! Integration tests for the trusted writer process gate.
//!
//! Coverage:
//!
//! * Helper-level decision tests pin every branch of
//!   [`evaluate_trusted_writer`] using the public API. No FUSE round
//!   trip is required so these always run, even when `/dev/fuse` is
//!   unavailable.
//! * FUSE-level tests verify that [`mount_background_configured`]
//!   correctly threads the configuration into the `.skill-meta`
//!   enforcement gate. The default-disabled path is exercised so the
//!   pre-existing `EACCES` deny is preserved bit-for-bit; the
//!   matching path uses the test process's own
//!   `/proc/<self>/comm` value as the configured trusted name so the
//!   bypass is reachable without a separate process. A mismatched
//!   configured name still denies.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use skillfs_core::{ParseConfig, SharedSkillStore, store::SkillStore};
use skillfs_fuse::security::{
    FileId, LinuxProcCommResolver, ProcessIdentity, ProcessIdentityResolver, TrustedWriterConfig,
    TrustedWriterDecision, evaluate_trusted_writer,
};
use skillfs_fuse::{MountConfig, MountHandle, MountOptions, mount_background_configured};

mod common;

// ─────────────────────────────────────────────────────────────────────────────
// Helper-level decision tests (no FUSE)
// ─────────────────────────────────────────────────────────────────────────────

/// Test resolver that always returns the same name, regardless of pid.
/// Lets the helper-level tests pin allow/deny without depending on a
/// real /proc.
struct FixedNameResolver(String);

impl ProcessIdentityResolver for FixedNameResolver {
    fn resolve_identity(&self, _pid: u32) -> Option<ProcessIdentity> {
        Some(ProcessIdentity {
            comm: self.0.clone(),
            starttime: None,
            exe_path: None,
            exe_file_id: None,
        })
    }
}

/// Test resolver that always fails to resolve. Used to exercise the
/// `DeniedIdentityUnresolved` branch.
struct FailingResolver;

impl ProcessIdentityResolver for FailingResolver {
    fn resolve_identity(&self, _pid: u32) -> Option<ProcessIdentity> {
        None
    }
}

#[test]
fn default_config_decision_is_disabled() {
    let cfg = TrustedWriterConfig::default();
    let resolver = FixedNameResolver("agent-sec-cli".into());
    let d = evaluate_trusted_writer(&cfg, 1234, &resolver);
    assert_eq!(d, TrustedWriterDecision::Disabled);
    assert!(!d.is_allowed());
}

#[test]
fn matching_resolved_name_is_allowed() {
    let cfg = TrustedWriterConfig::with_process_name("agent-sec-cli");
    let resolver = FixedNameResolver("agent-sec-cli".into());
    let d = evaluate_trusted_writer(&cfg, 1234, &resolver);
    match d {
        TrustedWriterDecision::AllowedByName { name } => assert_eq!(name, "agent-sec-cli"),
        other => panic!("expected AllowedByName, got {other:?}"),
    }
}

#[test]
fn mismatched_resolved_name_is_denied() {
    let cfg = TrustedWriterConfig::with_process_name("agent-sec-cli");
    let resolver = FixedNameResolver("bash".into());
    let d = evaluate_trusted_writer(&cfg, 1234, &resolver);
    assert_eq!(d.audit_label(), "trusted_writer_name_mismatch");
    match d {
        TrustedWriterDecision::DeniedNameMismatch { actual, expected } => {
            assert_eq!(actual, "bash");
            assert_eq!(expected, "agent-sec-cli");
        }
        other => panic!("expected DeniedNameMismatch, got {other:?}"),
    }
}

#[test]
fn unresolved_pid_is_denied() {
    let cfg = TrustedWriterConfig::with_process_name("agent-sec-cli");
    let resolver = FailingResolver;
    let d = evaluate_trusted_writer(&cfg, 1234, &resolver);
    assert_eq!(d, TrustedWriterDecision::DeniedIdentityUnresolved);
    assert_eq!(d.audit_label(), "trusted_writer_identity_unresolved");
}

#[test]
fn empty_string_config_normalizes_to_disabled() {
    let cfg = TrustedWriterConfig::with_process_name("");
    let resolver = FixedNameResolver("anything".into());
    let d = evaluate_trusted_writer(&cfg, 1234, &resolver);
    assert_eq!(d, TrustedWriterDecision::Disabled);
}

#[test]
fn whitespace_only_config_normalizes_to_disabled() {
    for n in ["   ", "\t", "\n", "  \t\n"] {
        let cfg = TrustedWriterConfig::with_process_name(n);
        assert!(
            !cfg.is_enabled(),
            "whitespace-only name {n:?} must not enable the gate"
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn linux_proc_comm_resolver_self_lookup_is_some() {
    // Sanity check: the live resolver must produce a non-empty name
    // for the test process itself. This is what the FUSE-level tests
    // below depend on for the matching-bypass path.
    let pid = std::process::id();
    let resolver = LinuxProcCommResolver::new();
    let name = resolver.resolve_process_name(pid);
    assert!(
        name.is_some(),
        "self-pid resolution must succeed on Linux; got {name:?}"
    );
    assert!(!name.unwrap().is_empty());
}

// ─────────────────────────────────────────────────────────────────────────────
// FUSE-level tests
// ─────────────────────────────────────────────────────────────────────────────

/// Read the test process's own `/proc/<self>/comm`, with the trailing
/// newline stripped. Linux truncates `comm` to 15 bytes; the
/// trusted-writer gate compares the same string the kernel exposes,
/// so the test does not need to know the binary's full name.
#[cfg(target_os = "linux")]
fn self_comm() -> String {
    let bytes =
        std::fs::read(format!("/proc/{}/comm", std::process::id())).expect("/proc/<self>/comm");
    let mut s = String::from_utf8(bytes).expect("comm utf-8");
    if s.ends_with('\n') {
        s.pop();
    }
    assert!(!s.is_empty(), "self comm must not be empty");
    s
}

/// Seed a normal skill plus its `.skill-meta` directory so the FUSE
/// callback for a `.skill-meta` write has a real physical target to
/// hit. The default policy denies the mutation before reaching the
/// physical write, so the file does not need to exist; we still
/// pre-create the directory so the matching-bypass path can succeed.
fn seed_skill_with_meta(source: &Path, skill: &str) {
    common::create_skill_dir(source, skill);
    let meta = source.join(skill).join(".skill-meta");
    std::fs::create_dir_all(&meta).expect("create .skill-meta dir");
}

fn fixture_store(source: &Path) -> SharedSkillStore {
    let mut store = SkillStore::new();
    let _ = store.load_from_directory(source, &ParseConfig::default());
    Arc::new(RwLock::new(store))
}

/// RAII fixture that mounts SkillFS in normal mode with an
/// operator-supplied trusted-writer config. Drop unmounts via
/// `fusermount3 -u` and lets `tempfile` clean the source directory.
struct TrustedWriterFixture {
    source: tempfile::TempDir,
    mountpoint: tempfile::TempDir,
    handle: Option<MountHandle>,
}

impl TrustedWriterFixture {
    fn mount_with_config(skill: &str, trusted_writer: Option<TrustedWriterConfig>) -> Self {
        let source = tempfile::tempdir().expect("source tempdir");
        seed_skill_with_meta(source.path(), skill);
        let mountpoint = tempfile::tempdir().expect("mount tempdir");

        let store = fixture_store(source.path());
        let handle = mount_background_configured(
            mountpoint.path(),
            source.path(),
            store,
            MountOptions::default(),
            false,
            MountConfig {
                trusted_writer,
                ..MountConfig::default()
            },
        )
        .expect("mount_background_configured");
        std::thread::sleep(Duration::from_millis(300));
        Self {
            source,
            mountpoint,
            handle: Some(handle),
        }
    }

    fn skill_meta(&self, skill: &str) -> PathBuf {
        self.mountpoint
            .path()
            .join("skills")
            .join(skill)
            .join(".skill-meta")
    }

    fn source_skill_meta(&self, skill: &str) -> PathBuf {
        self.source.path().join(skill).join(".skill-meta")
    }
}

impl Drop for TrustedWriterFixture {
    fn drop(&mut self) {
        if let Some(h) = self.handle.take() {
            drop(h);
        }
        std::thread::sleep(Duration::from_millis(150));
        let _ = std::process::Command::new("fusermount3")
            .args(["-u", &self.mountpoint.path().to_string_lossy()])
            .output();
    }
}

/// Default-disabled config: `.skill-meta` access through the FUSE
/// mount is hidden (ENOENT). Pre-existing deny is preserved; the
/// view gate now hides `.skill-meta` entirely for untrusted callers.
#[test]
fn default_disabled_config_keeps_skill_meta_denied() {
    if !common::fuse_available() {
        eprintln!("SKIP default_disabled_config_keeps_skill_meta_denied: FUSE not available");
        return;
    }
    let skill = "alpha";
    let fx = TrustedWriterFixture::mount_with_config(skill, None);
    let target = fx.skill_meta(skill).join("manifest.json");
    let err =
        std::fs::write(&target, b"{}\n").expect_err("write must fail with default-disabled gate");
    assert_eq!(
        err.raw_os_error(),
        Some(libc::ENOENT),
        "default-disabled gate must surface ENOENT, got {err:?}"
    );
    // The on-disk source must remain untouched.
    assert!(
        !fx.source_skill_meta(skill).join("manifest.json").exists(),
        "denied write must not have created the source-side file"
    );
}

/// Configured trusted-writer name that does NOT match the test
/// process's comm: `.skill-meta` access through the mount returns
/// `ENOENT` (hidden from untrusted callers).
#[cfg(target_os = "linux")]
#[test]
fn mismatched_trusted_writer_name_still_denies_skill_meta() {
    if !common::fuse_available() {
        eprintln!(
            "SKIP mismatched_trusted_writer_name_still_denies_skill_meta: FUSE not available"
        );
        return;
    }
    let skill = "alpha";
    let mismatched = "skillfs-non-match";
    assert_ne!(
        mismatched,
        self_comm(),
        "test fixture name must not coincidentally equal /proc/self/comm"
    );
    let cfg = TrustedWriterConfig::with_process_name(mismatched);
    let fx = TrustedWriterFixture::mount_with_config(skill, Some(cfg));
    let target = fx.skill_meta(skill).join("manifest.json");
    let err = std::fs::write(&target, b"{}\n").expect_err(
        "write must fail when the configured trusted writer does not match the test process",
    );
    assert_eq!(err.raw_os_error(), Some(libc::ENOENT));
    assert!(
        !fx.source_skill_meta(skill).join("manifest.json").exists(),
        "mismatched-gate write must not have hit disk"
    );
}

/// Configured trusted-writer name equal to the test process's own
/// `/proc/<self>/comm`: `.skill-meta` mutation through the mount
/// bypasses the deny and the physical write lands on the source
/// directory.
#[cfg(target_os = "linux")]
#[test]
fn matching_trusted_writer_name_allows_skill_meta_write() {
    if !common::fuse_available() {
        eprintln!("SKIP matching_trusted_writer_name_allows_skill_meta_write: FUSE not available");
        return;
    }
    let skill = "alpha";
    let comm = self_comm();
    let cfg = TrustedWriterConfig::with_process_name(comm);
    let fx = TrustedWriterFixture::mount_with_config(skill, Some(cfg));
    let target = fx.skill_meta(skill).join("manifest.json");
    let body = b"{\"trusted\":true}\n";
    std::fs::write(&target, body)
        .expect("write must succeed when the configured trusted writer matches the test process");
    // Verify the physical source side actually changed.
    let on_disk = fx.source_skill_meta(skill).join("manifest.json");
    let actual = std::fs::read(&on_disk).expect("source-side file must exist after bypass");
    assert_eq!(
        actual, body,
        "bypass write must reach the source filesystem"
    );
    // Read-back through the FUSE mount works too. `.skill-meta` reads
    // are allowed by S1 by default; this asserts the bypass did not
    // change the read-side semantics.
    let via_mount = std::fs::read(&target).expect("read-back through mount");
    assert_eq!(via_mount, body);
}

/// The trusted-writer bypass is scoped to ordinary `.skill-meta` file and
/// directory mutations. It must not allow creating symlinks inside the
/// protected metadata namespace, even when the caller identity matches.
#[cfg(target_os = "linux")]
#[test]
fn matching_trusted_writer_does_not_relax_symlink_policy() {
    if !common::fuse_available() {
        eprintln!("SKIP matching_trusted_writer_does_not_relax_symlink_policy: FUSE not available");
        return;
    }
    let skill = "alpha";
    let comm = self_comm();
    let cfg = TrustedWriterConfig::with_process_name(comm);
    let fx = TrustedWriterFixture::mount_with_config(skill, Some(cfg));
    let normal = fx
        .mountpoint
        .path()
        .join("skills")
        .join(skill)
        .join("regular.txt");
    std::fs::write(&normal, b"normal\n").expect("seed regular file through mount");

    let link_path = fx.skill_meta(skill).join("link-to-regular");
    let err = std::os::unix::fs::symlink("../regular.txt", &link_path)
        .expect_err("trusted writer must not create symlink inside .skill-meta");
    assert_eq!(err.raw_os_error(), Some(libc::EACCES));
    assert!(
        !fx.source_skill_meta(skill).join("link-to-regular").exists(),
        "denied symlink must not reach source .skill-meta"
    );
}

/// Hardlinking a protected metadata file out to an ordinary skill path would
/// leak the same inode under an unprotected name. The trusted-writer bypass
/// must not relax the hardlink source-side `.skill-meta` gate.
#[cfg(target_os = "linux")]
#[test]
fn matching_trusted_writer_does_not_relax_hardlink_policy() {
    if !common::fuse_available() {
        eprintln!(
            "SKIP matching_trusted_writer_does_not_relax_hardlink_policy: FUSE not available"
        );
        return;
    }
    let skill = "alpha";
    let comm = self_comm();
    let cfg = TrustedWriterConfig::with_process_name(comm);
    let fx = TrustedWriterFixture::mount_with_config(skill, Some(cfg));
    let meta_source = fx.source_skill_meta(skill).join("manifest.json");
    std::fs::write(&meta_source, b"{}\n").expect("seed source-side metadata file");

    let src = fx.skill_meta(skill).join("manifest.json");
    let dst = fx
        .mountpoint
        .path()
        .join("skills")
        .join(skill)
        .join("manifest-copy.json");
    let err = std::fs::hard_link(&src, &dst)
        .expect_err("trusted writer must not hardlink .skill-meta file out");
    assert_eq!(err.raw_os_error(), Some(libc::EACCES));
    assert!(
        !fx.source
            .path()
            .join(skill)
            .join("manifest-copy.json")
            .exists(),
        "denied hardlink must not create an unprotected alias"
    );
}

#[cfg(target_os = "linux")]
fn setxattr_path(path: &Path, name: &str, value: &[u8]) -> std::io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c_path = CString::new(path.as_os_str().as_bytes()).expect("path without NUL");
    let c_name = CString::new(name).expect("xattr name without NUL");
    let rc = unsafe {
        libc::setxattr(
            c_path.as_ptr(),
            c_name.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// The trusted writer can bypass the `.skill-meta` mutation gate, but xattr
/// namespace policy still applies afterwards. Non-`user.*` names must stay
/// rejected instead of becoming a generic metadata escape hatch.
#[cfg(target_os = "linux")]
#[test]
fn matching_trusted_writer_does_not_relax_xattr_namespace_policy() {
    if !common::fuse_available() {
        eprintln!(
            "SKIP matching_trusted_writer_does_not_relax_xattr_namespace_policy: FUSE not available"
        );
        return;
    }
    let skill = "alpha";
    let comm = self_comm();
    let cfg = TrustedWriterConfig::with_process_name(comm);
    let fx = TrustedWriterFixture::mount_with_config(skill, Some(cfg));
    let target = fx.skill_meta(skill).join("manifest.json");
    std::fs::write(&target, b"{}\n").expect("trusted writer creates metadata file");

    let err = setxattr_path(&target, "skillfs.no_namespace", b"blocked")
        .expect_err("unsupported xattr namespace must remain rejected");
    assert_eq!(err.raw_os_error(), Some(libc::EOPNOTSUPP));
}

/// `skill-discover` is a virtual read-only namespace; the trusted
/// writer bypass must not relax its `EROFS` rejection. We do not
/// expect any sane operator to point the bypass at it, but a
/// defensive test pins the boundary so a future refactor cannot
/// silently widen the bypass.
#[cfg(target_os = "linux")]
#[test]
fn matching_trusted_writer_does_not_relax_skill_discover() {
    if !common::fuse_available() {
        eprintln!("SKIP matching_trusted_writer_does_not_relax_skill_discover: FUSE not available");
        return;
    }
    let skill = "alpha";
    let comm = self_comm();
    let cfg = TrustedWriterConfig::with_process_name(comm);
    let fx = TrustedWriterFixture::mount_with_config(skill, Some(cfg));
    // skill-discover is virtual; mutations there return EROFS.
    let discover = fx
        .mountpoint
        .path()
        .join("skills")
        .join("skill-discover")
        .join(".skill-meta");
    let res = std::fs::create_dir_all(&discover);
    assert!(res.is_err(), "skill-discover must remain read-only");
    let err = res.unwrap_err();
    assert!(
        matches!(err.raw_os_error(), Some(libc::EROFS) | Some(libc::EACCES))
            || err.kind() == std::io::ErrorKind::NotFound,
        "skill-discover mutation must not be relaxed by the bypass; got {err:?}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Executable identity tests (FUSE-level)
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
fn self_exe_identity() -> (PathBuf, FileId) {
    use std::os::unix::fs::MetadataExt;
    let exe = std::fs::read_link(format!("/proc/{}/exe", std::process::id()))
        .expect("/proc/self/exe readlink");
    let canonical = std::fs::canonicalize(&exe).unwrap_or(exe);
    let meta = std::fs::metadata(&canonical).expect("stat self exe");
    let fid = FileId {
        dev: meta.dev(),
        ino: meta.ino(),
    };
    (canonical, fid)
}

#[cfg(target_os = "linux")]
#[test]
fn matching_exe_identity_allows_skill_meta_write() {
    if !common::fuse_available() {
        eprintln!("SKIP matching_exe_identity_allows_skill_meta_write: FUSE not available");
        return;
    }
    let skill = "alpha";
    let (exe_path, file_id) = self_exe_identity();
    let cfg = TrustedWriterConfig::with_executable(exe_path, file_id);
    let fx = TrustedWriterFixture::mount_with_config(skill, Some(cfg));
    let target = fx.skill_meta(skill).join("manifest.json");
    let body = b"{\"exe_identity\":true}\n";
    std::fs::write(&target, body).expect("write must succeed when exe identity matches");
    let on_disk = fx.source_skill_meta(skill).join("manifest.json");
    let actual = std::fs::read(&on_disk).expect("source-side file must exist");
    assert_eq!(actual, body);
}

#[cfg(target_os = "linux")]
#[test]
fn mismatched_exe_identity_denies_skill_meta_write() {
    if !common::fuse_available() {
        eprintln!("SKIP mismatched_exe_identity_denies_skill_meta_write: FUSE not available");
        return;
    }
    let skill = "alpha";
    let cfg = TrustedWriterConfig::with_executable(
        PathBuf::from("/usr/bin/this-does-not-match"),
        FileId { dev: 0, ino: 0 },
    );
    let fx = TrustedWriterFixture::mount_with_config(skill, Some(cfg));
    let target = fx.skill_meta(skill).join("manifest.json");
    let err = std::fs::write(&target, b"{}\n")
        .expect_err("write must fail when exe identity does not match");
    assert_eq!(err.raw_os_error(), Some(libc::ENOENT));
}

#[cfg(target_os = "linux")]
#[test]
fn exe_identity_does_not_relax_symlink_policy() {
    if !common::fuse_available() {
        eprintln!("SKIP exe_identity_does_not_relax_symlink_policy: FUSE not available");
        return;
    }
    let skill = "alpha";
    let (exe_path, file_id) = self_exe_identity();
    let cfg = TrustedWriterConfig::with_executable(exe_path, file_id);
    let fx = TrustedWriterFixture::mount_with_config(skill, Some(cfg));
    let normal = fx
        .mountpoint
        .path()
        .join("skills")
        .join(skill)
        .join("regular.txt");
    std::fs::write(&normal, b"normal\n").expect("seed regular file");
    let link_path = fx.skill_meta(skill).join("link-to-regular");
    let err = std::os::unix::fs::symlink("../regular.txt", &link_path)
        .expect_err("exe identity must not create symlink inside .skill-meta");
    assert_eq!(err.raw_os_error(), Some(libc::EACCES));
}
