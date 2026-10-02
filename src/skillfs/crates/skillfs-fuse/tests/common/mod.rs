//! Shared FUSE test harness for SkillFS Phase 1 acceptance tests.
//!
//! `MountFixture` owns the temp source dir, the optional separate mount-point,
//! and the `MountHandle`. Drop verifies the FUSE mount is gone before `tempfile`
//! cleans up the underlying directories.
//!
//! Each integration test file in `crates/skillfs-fuse/tests/*.rs` is compiled
//! as its own crate, so individual files only consume a subset of the items
//! exposed here. Callers should silence unused warnings via
//! `#[allow(dead_code)]` (or use the items directly).

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use skillfs_core::os_adapter::OsAdapterStage;
use skillfs_core::{ParseConfig, SharedSkillStore, store::SkillStore};
use skillfs_fuse::security::SkillEventSink;
use skillfs_fuse::{MountConfig, MountHandle, MountOptions, mount_background_configured};

// ─────────────────────────────────────────────────────────────────────────────
// FUSE availability detection
// ─────────────────────────────────────────────────────────────────────────────

/// Returns `true` when FUSE is likely usable in this environment.
///
/// Requires:
///   * `/dev/fuse` to be present **and** open()-able (covers the
///     "device exists but caller lacks permission" case that some
///     containers/CI environments expose);
///   * `fusermount3` to exist on `$PATH` and respond to `--version`.
///
/// We open `/dev/fuse` with `O_RDWR | O_CLOEXEC | O_NONBLOCK` and immediately
/// close it — that's what `fuser` does internally to acquire the channel, so
/// success here is a strong signal a real mount can succeed. Failure modes we
/// want to skip on:
///   * `ENOENT` — kernel module not present;
///   * `EACCES` / `EPERM` — caller cannot open the device (rootless container
///     without `cap_sys_admin`, restricted seccomp, etc.);
///   * `ENXIO` / `ENODEV` — device exists but no driver bound.
///
/// FUSE-dependent tests must call this and skip gracefully when it is `false`.
pub fn fuse_available() -> bool {
    if !Path::new("/dev/fuse").exists() {
        return false;
    }

    // Probe permission to actually open the FUSE device. `fusermount3
    // --version` succeeds even when the calling user can't open
    // `/dev/fuse`, so this extra probe avoids false positives.
    let dev = std::ffi::CString::new("/dev/fuse").expect("CString /dev/fuse");
    let fd = unsafe {
        libc::open(
            dev.as_ptr(),
            libc::O_RDWR | libc::O_CLOEXEC | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        return false;
    }
    unsafe { libc::close(fd) };

    std::process::Command::new("fusermount3")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Returns `true` when SLS telemetry is enabled on the host, i.e. the real
/// `/etc/anolisa/.telemetry_disabled` sentinel is absent.
///
/// Tests that mount with a `RuntimeMetricsSink` and assert metric records must
/// skip when this is `false`: the production writer pins the real sentinel (no
/// override exists), so a disabled host writes zero records and the assertions
/// cannot hold. The enabled write path stays covered by the writer unit tests,
/// which inject a temp sentinel.
pub fn telemetry_enabled() -> bool {
    skillfs_fuse::security::telemetry_allowed()
}

/// Skip the current test (early `return`) with a deterministic stderr line
/// when FUSE is unavailable. Use at the top of every FUSE-dependent test.
#[macro_export]
macro_rules! skip_if_no_fuse {
    () => {
        if !$crate::common::fuse_available() {
            eprintln!(
                "SKIP {}: FUSE not available (no /dev/fuse or fusermount3)",
                ::std::module_path!()
            );
            return;
        }
    };
    ($name:expr) => {
        if !$crate::common::fuse_available() {
            eprintln!("SKIP {}: FUSE not available", $name);
            return;
        }
    };
}

// ─────────────────────────────────────────────────────────────────────────────
// Mount mode
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MountMode {
    /// Source directory and mountpoint are distinct. Skills appear under
    /// `<mount>/skills/<skill>/...`.
    Normal,
    /// FUSE over-mounts the source directory. Skills appear at
    /// `<mount>/<skill>/...` (no `/skills/` prefix).
    InPlace,
}

// ─────────────────────────────────────────────────────────────────────────────
// MountFixture
// ─────────────────────────────────────────────────────────────────────────────

/// RAII fixture that mounts SkillFS over a temp source directory and tears the
/// mount down on `Drop`. A `seed` closure is invoked on the source directory
/// before mounting, so the initial `SkillStore` snapshot reflects the desired
/// pre-existing skills.
///
/// Always pair with `skip_if_no_fuse!()` — constructing a fixture without FUSE
/// will hang waiting for the mount thread.
pub struct MountFixture {
    mode: MountMode,
    source: tempfile::TempDir,
    /// `Some` only in normal mode.
    mountpoint: Option<tempfile::TempDir>,
    handle: Option<MountHandle>,
}

impl MountFixture {
    /// Mount in normal mode. `seed` is invoked with the source directory path
    /// before the store is loaded and FUSE is started.
    pub fn normal<F: FnOnce(&Path)>(seed: F) -> Self {
        let source = tempfile::tempdir().expect("source tempdir");
        seed(source.path());
        let mountpoint = tempfile::tempdir().expect("mount tempdir");
        Self::mount_now(MountMode::Normal, source, Some(mountpoint))
    }

    /// Like [`MountFixture::normal`] but builds the source tempdir under the
    /// caller-supplied `parent` directory instead of `$TMPDIR`. Tests that
    /// need a specific substrate capability (T3 `user.*` xattr passthrough
    /// is the motivating case — many tmpfs mounts disable `user.*`) use this
    /// to anchor the source where the substrate is known to cooperate. The
    /// mountpoint stays under `$TMPDIR` because only the source-side fd is
    /// the one that reaches the underlying filesystem.
    pub fn normal_in<F: FnOnce(&Path)>(parent: &Path, seed: F) -> Self {
        std::fs::create_dir_all(parent).expect("ensure source parent dir");
        let source = tempfile::Builder::new()
            .prefix("skillfs-src-")
            .tempdir_in(parent)
            .expect("source tempdir in parent");
        seed(source.path());
        let mountpoint = tempfile::tempdir().expect("mount tempdir");
        Self::mount_now(MountMode::Normal, source, Some(mountpoint))
    }

    /// Mount in in-place mode (`source == mountpoint`). `seed` is invoked
    /// before the store load.
    pub fn in_place<F: FnOnce(&Path)>(seed: F) -> Self {
        let source = tempfile::tempdir().expect("source tempdir");
        seed(source.path());
        Self::mount_now(MountMode::InPlace, source, None)
    }

    /// Mount in in-place mode with Hermes layout.
    pub fn in_place_hermes<F: FnOnce(&Path)>(seed: F) -> Self {
        let source = tempfile::tempdir().expect("source tempdir");
        seed(source.path());
        Self::mount_now_with_layout(
            MountMode::InPlace,
            source,
            None,
            Some(skillfs_fuse::SkillLayout::Hermes),
            None,
            None,
            None,
        )
    }

    /// Mount in normal mode with an opt-in OS adapter transform stage.
    pub fn normal_with_os_adapter<F: FnOnce(&Path)>(seed: F, stage: OsAdapterStage) -> Self {
        let source = tempfile::tempdir().expect("source tempdir");
        seed(source.path());
        let mountpoint = tempfile::tempdir().expect("mount tempdir");
        Self::mount_now_with_layout(
            MountMode::Normal,
            source,
            Some(mountpoint),
            None,
            Some(stage),
            None,
            None,
        )
    }

    /// Mount in normal mode with optional adapter and an audit event sink.
    pub fn normal_with_transform_audit<F: FnOnce(&Path)>(
        seed: F,
        os_adapter: Option<OsAdapterStage>,
        event_sink: Arc<dyn SkillEventSink>,
    ) -> Self {
        let source = tempfile::tempdir().expect("source tempdir");
        seed(source.path());
        let mountpoint = tempfile::tempdir().expect("mount tempdir");
        Self::mount_now_with_layout(
            MountMode::Normal,
            source,
            Some(mountpoint),
            None,
            os_adapter,
            None,
            Some(event_sink),
        )
    }

    /// Mount in normal mode with explicit directive-stage and OS-adapter
    /// configuration. `directive_enabled = Some(false)` disables the compiler
    /// stage; `os_adapter = None` leaves only whatever the directive setting
    /// selects (raw content when both are off).
    pub fn normal_with_transforms<F: FnOnce(&Path)>(
        seed: F,
        directive_enabled: Option<bool>,
        os_adapter: Option<OsAdapterStage>,
    ) -> Self {
        let source = tempfile::tempdir().expect("source tempdir");
        seed(source.path());
        let mountpoint = tempfile::tempdir().expect("mount tempdir");
        Self::mount_now_with_layout(
            MountMode::Normal,
            source,
            Some(mountpoint),
            None,
            os_adapter,
            directive_enabled,
            None,
        )
    }

    /// Mount in in-place Hermes mode with an opt-in OS adapter transform stage.
    pub fn in_place_hermes_with_os_adapter<F: FnOnce(&Path)>(
        seed: F,
        stage: OsAdapterStage,
    ) -> Self {
        let source = tempfile::tempdir().expect("source tempdir");
        seed(source.path());
        Self::mount_now_with_layout(
            MountMode::InPlace,
            source,
            None,
            Some(skillfs_fuse::SkillLayout::Hermes),
            Some(stage),
            None,
            None,
        )
    }

    /// Mount in in-place Hermes mode with an adapter and audit event sink.
    pub fn in_place_hermes_with_transform_audit<F: FnOnce(&Path)>(
        seed: F,
        stage: OsAdapterStage,
        event_sink: Arc<dyn SkillEventSink>,
    ) -> Self {
        let source = tempfile::tempdir().expect("source tempdir");
        seed(source.path());
        Self::mount_now_with_layout(
            MountMode::InPlace,
            source,
            None,
            Some(skillfs_fuse::SkillLayout::Hermes),
            Some(stage),
            None,
            Some(event_sink),
        )
    }

    fn mount_now(
        mode: MountMode,
        source: tempfile::TempDir,
        mountpoint: Option<tempfile::TempDir>,
    ) -> Self {
        Self::mount_now_with_layout(mode, source, mountpoint, None, None, None, None)
    }

    fn mount_now_with_layout(
        mode: MountMode,
        source: tempfile::TempDir,
        mountpoint: Option<tempfile::TempDir>,
        skill_layout: Option<skillfs_fuse::SkillLayout>,
        os_adapter: Option<OsAdapterStage>,
        directive_enabled: Option<bool>,
        event_sink: Option<Arc<dyn SkillEventSink>>,
    ) -> Self {
        let mut store = SkillStore::new();
        store.load_from_directory(source.path(), &ParseConfig::default());
        let shared: SharedSkillStore = Arc::new(RwLock::new(store));

        let mp_path: PathBuf = match (&mountpoint, mode) {
            (Some(mp), MountMode::Normal) => mp.path().to_path_buf(),
            (None, MountMode::InPlace) => source.path().to_path_buf(),
            _ => unreachable!("mountpoint/mode mismatch"),
        };
        let in_place = matches!(mode, MountMode::InPlace);

        let config = MountConfig {
            event_sink,
            skill_layout,
            os_adapter,
            directive_enabled,
            ..MountConfig::default()
        };

        let handle = mount_background_configured(
            &mp_path,
            source.path(),
            shared,
            MountOptions::default(),
            in_place,
            config,
        )
        .expect("mount_background_configured");

        // Give the FUSE daemon time to start serving requests.
        std::thread::sleep(Duration::from_millis(300));

        Self {
            mode,
            source,
            mountpoint,
            handle: Some(handle),
        }
    }

    pub fn mode(&self) -> MountMode {
        self.mode
    }

    /// The physical source directory backing the mount.
    pub fn source(&self) -> &Path {
        self.source.path()
    }

    /// The directory userspace tools should access. In normal mode this is
    /// the dedicated mount point; in in-place mode this is the source dir.
    pub fn mountpoint(&self) -> &Path {
        match self.mode {
            MountMode::Normal => self.mountpoint.as_ref().unwrap().path(),
            MountMode::InPlace => self.source.path(),
        }
    }

    /// Path to a skill directory through the mount, accounting for the mode's
    /// path layout (`<mount>/skills/<name>` vs `<mount>/<name>`).
    pub fn skill_path(&self, skill_name: &str) -> PathBuf {
        match self.mode {
            MountMode::Normal => self.mountpoint().join("skills").join(skill_name),
            MountMode::InPlace => self.mountpoint().join(skill_name),
        }
    }

    /// Path to the skills root through the mount: `<mount>/skills` in normal
    /// mode, `<mount>` itself in in-place mode.
    pub fn skills_root(&self) -> PathBuf {
        match self.mode {
            MountMode::Normal => self.mountpoint().join("skills"),
            MountMode::InPlace => self.mountpoint().to_path_buf(),
        }
    }

    /// Path to a passthrough file under a skill, through the mount.
    pub fn passthrough_path(&self, skill_name: &str, rel: &str) -> PathBuf {
        self.skill_path(skill_name).join(rel)
    }

    /// Physical path to a skill directory on the source filesystem.
    pub fn source_skill_path(&self, skill_name: &str) -> PathBuf {
        self.source.path().join(skill_name)
    }
}

fn is_mounted(path: &Path) -> bool {
    let Ok(mounts) = std::fs::read_to_string("/proc/mounts") else {
        return false;
    };
    let target = path.to_string_lossy();
    mounts
        .lines()
        .any(|line| line.split_whitespace().nth(1) == Some(&*target))
}

fn force_unmount(path: &Path) {
    for _ in 0..50 {
        if !is_mounted(path) {
            return;
        }

        let mountpoint = path.to_string_lossy().to_string();
        let _ = std::process::Command::new("fusermount3")
            .args(["-u", mountpoint.as_str()])
            .output();
        let _ = std::process::Command::new("fusermount3")
            .args(["-u", "-z", mountpoint.as_str()])
            .output();
        let _ = std::process::Command::new("umount")
            .args(["-l", mountpoint.as_str()])
            .output();
        std::thread::sleep(Duration::from_millis(100));
    }

    if is_mounted(path) {
        eprintln!("WARN: leaked SkillFS FUSE mount at {}", path.display());
    }
}

impl Drop for MountFixture {
    fn drop(&mut self) {
        let mp = self.mountpoint().to_path_buf();

        if let Some(handle) = self.handle.take() {
            if let Err(e) = handle.unmount() {
                eprintln!(
                    "WARN: SkillFS test unmount failed for {}: {e}",
                    mp.display()
                );
            }
        }
        force_unmount(&mp);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Seeding helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Create `<dir>/<name>/SKILL.md` with minimal Phase 1 frontmatter.
pub fn create_skill_dir(dir: &Path, name: &str) {
    let skill_dir = dir.join(name);
    std::fs::create_dir_all(&skill_dir).expect("create skill dir");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: test skill\n---\n"),
    )
    .expect("write SKILL.md");
}

/// Read the names of entries in `dir` (minus `.`/`..`), sorted for stable
/// assertions.
pub fn list_dir_names(dir: &Path) -> Vec<String> {
    let mut entries: Vec<String> = std::fs::read_dir(dir)
        .expect("read_dir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    entries.sort();
    entries
}
