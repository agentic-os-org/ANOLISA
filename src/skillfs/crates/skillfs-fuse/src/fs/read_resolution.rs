//! Ledger-driven read resolution.
//!
//! Owns the [`ReadResolution`] enum and the SkillFs methods that
//! consume it. These are the read counterparts of the ledger active
//! mapping: `Source` covers the no-resolver default and the
//! `Current` decision, `Snapshot` switches reads to a trusted
//! snapshot directory, and `Hidden` instructs callers to surface
//! `ENOENT` (lookup/getattr) or to drop the entry (readdir).
//!
//! Write paths intentionally bypass this module — D1.1 is read-only by
//! design.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::SkillFs;
use crate::security::ActiveTarget;

/// Outcome of [`SkillFs::resolve_skill_read`].
#[derive(Debug, Clone)]
pub(super) enum ReadResolution {
    /// Read the live source directory. Returned outside demo mode and
    /// for [`ActiveTarget::Current`]; the actual directory is computed
    /// by [`SkillFs::skill_physical_dir`] at the call site so existing
    /// flat/categorized-layout handling stays in one place.
    Source,
    /// Read the snapshot directory. `dir` is already rewritten through
    /// [`SkillFs::source_base`] so in-place mounts bypass FUSE.
    /// `version` is the ledger-supplied label and is currently only
    /// used by the demo-event consumer.
    Snapshot {
        dir: PathBuf,
        #[allow(dead_code)]
        version: String,
    },
    /// Security mode: skill is hidden by the ledger or has no entry in the
    /// resolver.
    Hidden,
}

impl SkillFs {
    pub(super) fn capture_transformed(
        &self,
        skill_name: &str,
        physical: &Path,
        target: Option<&ActiveTarget>,
    ) -> std::io::Result<Arc<str>> {
        self.load_transformed(skill_name, physical, target)
            .map(|(content, _)| content)
    }

    fn load_transformed(
        &self,
        skill_name: &str,
        physical: &Path,
        target: Option<&ActiveTarget>,
    ) -> std::io::Result<(Arc<str>, std::fs::Metadata)> {
        if self.transform_pipeline.is_empty() {
            use std::io::Read;
            let mut file = std::fs::File::open(physical)?;
            let metadata = file.metadata()?;
            let mut raw = String::new();
            file.read_to_string(&mut raw)?;
            return Ok((raw.into(), metadata));
        }
        self.transform_cache.load(
            skill_name,
            physical,
            target,
            self.transform_pipeline.identity(),
            |raw| self.transform_pipeline.run(raw),
        )
    }

    // Select activation once for both the transformed size and physical attrs.
    pub(super) fn transformed_skill_attr(
        &self,
        skill_name: &str,
        category: Option<&str>,
    ) -> Option<fuser::FileAttr> {
        if category.is_none() && skill_name == "skill-discover" {
            return Some(self.virtual_file_attr(self.get_skill_discover_content().len() as u64));
        }
        let (id, target, resolution) = match category {
            Some(category) => {
                let (target, resolution) =
                    self.resolve_hermes_nested_read_pinned(category, skill_name);
                (
                    Self::hermes_skill_id(category, skill_name),
                    target,
                    resolution,
                )
            }
            None => {
                let (target, resolution) = self.resolve_skill_read_pinned(skill_name);
                (skill_name.to_owned(), target, resolution)
            }
        };
        let physical = match resolution {
            ReadResolution::Hidden => return None,
            ReadResolution::Snapshot { dir, .. } => dir.join("SKILL.md"),
            ReadResolution::Source if category.is_some() || self.in_place => {
                self.source_base().join(&id).join("SKILL.md")
            }
            ReadResolution::Source => self.skill_source_path(&id)?,
        };
        let (content, metadata) = self
            .load_transformed(&id, &physical, target.as_ref())
            .ok()?;
        let mut attr = crate::attr::file_attr_from_metadata(&metadata);
        attr.size = content.len() as u64;
        Some(attr)
    }

    /// Read and compile a skill's SKILL.md content.
    ///
    /// In in-place mode reads via `/proc/self/fd/{n}` to bypass FUSE.
    /// When the D1.1 active resolver maps the skill to
    /// [`ActiveTarget::Snapshot`], the snapshot's `SKILL.md` is read and
    /// compiled instead of the live source, preserving the compiled-read
    /// semantics from the SkillFS invariants. [`ActiveTarget::Hidden`]
    /// returns `None` so the caller surfaces `ENOENT`.
    pub(super) fn compiled_skill_md(&self, skill_name: &str) -> Option<String> {
        if skill_name == "skill-discover" {
            return Some(self.get_skill_discover_content());
        }
        let physical_path = match self.resolve_skill_read(skill_name) {
            ReadResolution::Hidden => return None,
            ReadResolution::Source => {
                if self.in_place {
                    // Bypass the FUSE layer via the pre-opened fd.
                    self.source_base().join(skill_name).join("SKILL.md")
                } else {
                    self.skill_source_path(skill_name)?
                }
            }
            ReadResolution::Snapshot { dir, .. } => dir.join("SKILL.md"),
        };
        let raw = std::fs::read_to_string(&physical_path).ok()?;
        Some(self.transform_pipeline.run(&raw))
    }

    /// Whether a virtual `SKILL.md` entry should be listed for
    /// `skill_name` in `readdir`/`opendir`.
    ///
    /// A freshly-created placeholder skill directory (via `mkdir`) has no
    /// physical `SKILL.md` yet, so synthesizing the virtual entry
    /// unconditionally produces a phantom listing whose `lookup`/`getattr`
    /// then fail with `ENOENT` (broken entry with unknown attrs). Gate the
    /// virtual entry on the manifest actually being readable through the
    /// current read semantics: `skill-discover` is always virtual, and any
    /// other skill lists `SKILL.md` only when the resolved read directory
    /// (live source or snapshot) physically contains it.
    pub(super) fn skill_md_listable(&self, skill_name: &str) -> bool {
        if skill_name == "skill-discover" {
            return true;
        }
        match self.skill_read_dir(skill_name) {
            Some(dir) => skillfs_core::store::has_regular_skill_md(&dir),
            None => false,
        }
    }

    /// Physical directory to read **content from** for `skill_name`.
    ///
    /// For an unattached resolver (default) and for
    /// [`ActiveTarget::Current`] this is the live skill directory
    /// returned by [`Self::skill_physical_dir`]. For
    /// [`ActiveTarget::Snapshot`] it is the snapshot directory rewritten
    /// through [`Self::source_base`] so in-place mounts continue to
    /// bypass the FUSE over-mount via `/proc/self/fd/{n}`. Returns
    /// `None` when the resolver marks the skill as hidden so the caller
    /// can surface `ENOENT` instead of leaking a path.
    ///
    /// Skill-discover bypasses ledger gating entirely and always reads
    /// from the virtual skill-discover dir.
    pub(super) fn skill_read_dir(&self, skill_name: &str) -> Option<PathBuf> {
        if skill_name == "skill-discover" {
            return Some(self.skill_physical_dir(skill_name));
        }
        match self.resolve_skill_read(skill_name) {
            ReadResolution::Hidden => None,
            ReadResolution::Source => Some(self.skill_physical_dir(skill_name)),
            ReadResolution::Snapshot { dir, .. } => Some(dir),
        }
    }

    /// Read and compile using a pinned target instead of the live resolver.
    pub(super) fn compiled_skill_md_pinned(
        &self,
        skill_name: &str,
        pinned: Option<&ActiveTarget>,
    ) -> Option<String> {
        if skill_name == "skill-discover" {
            return Some(self.get_skill_discover_content());
        }
        let resolution = match pinned {
            Some(target) => self.resolve_from_target(skill_name, target),
            None => self.resolve_skill_read(skill_name),
        };
        let physical_path = match resolution {
            ReadResolution::Hidden => return None,
            ReadResolution::Source => {
                if self.in_place {
                    self.source_base().join(skill_name).join("SKILL.md")
                } else {
                    self.skill_source_path(skill_name)?
                }
            }
            ReadResolution::Snapshot { dir, .. } => dir.join("SKILL.md"),
        };
        let raw = std::fs::read_to_string(&physical_path).ok()?;
        Some(self.transform_pipeline.run(&raw))
    }

    /// Resolve from an explicit `ActiveTarget` without consulting the resolver.
    fn resolve_from_target(&self, skill_name: &str, target: &ActiveTarget) -> ReadResolution {
        match target {
            ActiveTarget::Hidden { .. } => ReadResolution::Hidden,
            ActiveTarget::Current { .. } => ReadResolution::Source,
            ActiveTarget::Snapshot {
                snapshot_dir,
                version,
            } => {
                let dir = self.snapshot_read_dir(skill_name, snapshot_dir);
                ReadResolution::Snapshot {
                    dir,
                    version: version.clone(),
                }
            }
        }
    }

    /// Pure resolver consult. Returns `ReadResolution::Source` whenever
    /// no resolver is attached so the pre-security code paths behave
    /// exactly as before. Skill-discover is always `Source`.
    pub(super) fn resolve_skill_read(&self, skill_name: &str) -> ReadResolution {
        self.resolve_skill_read_pinned(skill_name).1
    }

    /// Snapshot the resolver **once** and return both the pinned
    /// [`ActiveTarget`] (for handle pinning) and the derived
    /// [`ReadResolution`] (for the open-time security decision).
    ///
    /// Serving both from a single `resolver.get` closes the TOCTOU window in
    /// `open`: reading the target for pinning and then re-resolving for the
    /// Hidden/Current/Snapshot decision could straddle a `Current -> Snapshot`
    /// activation change, letting an open be judged against one target while
    /// the handle pinned another. A snapshot decision must never be served from
    /// the live source, so both derive from the same observed target.
    ///
    /// Returns `(None, Source)` when no resolver is attached or for
    /// skill-discover, matching the no-resolver default (no pinning).
    pub(super) fn resolve_skill_read_pinned(
        &self,
        skill_name: &str,
    ) -> (Option<ActiveTarget>, ReadResolution) {
        if skill_name == "skill-discover" {
            return (None, ReadResolution::Source);
        }
        let resolver = match self.active_resolver.as_ref() {
            Some(r) => r,
            None => return (None, ReadResolution::Source),
        };
        let target = resolver.get(skill_name);
        // Default: skills the ledger has no opinion on are treated as
        // not-yet-certified and stay hidden until a future hook handler
        // installs a target for them.
        let resolution = match &target {
            None => ReadResolution::Hidden,
            Some(t) => self.resolve_from_target(skill_name, t),
        };
        (target, resolution)
    }

    /// Rewrite a `snapshot_dir` from the resolver so reads bypass the
    /// FUSE layer in in-place mode.
    ///
    /// The resolver constructs
    /// `snapshot_dir = source_root.join(skill).join(<rel>)` against the
    /// `source_root` it was built with. The CLI / tests build that
    /// resolver from the same `source` path the FUSE mount uses, so the
    /// prefix matches exactly and the relative segment after
    /// `<skill>/` can be safely rejoined against
    /// [`Self::source_base`]. In normal mode `source_base()` is the
    /// plain source path so the rewrite is a no-op; in in-place mode it
    /// becomes `/proc/self/fd/{n}`, which reads the underlying inode
    /// instead of re-entering the FUSE over-mount (which would deny
    /// `.skill-meta/**` mutations and would not even resolve through
    /// the virtual layer).
    ///
    /// If the prefix does not match (operator passed a canonicalized vs
    /// non-canonicalized source, or a future package starts handing the
    /// resolver an absolute path it did not build), the original
    /// `snapshot_dir` is returned verbatim. Either it resolves and the
    /// operator is happy, or the underlying syscall surfaces a real
    /// errno — no silent fallback to live source.
    pub(super) fn snapshot_read_dir(&self, skill_name: &str, snapshot_dir: &Path) -> PathBuf {
        let prefix = self.source.join(skill_name);
        match snapshot_dir.strip_prefix(&prefix) {
            Ok(rel) => self.source_base().join(skill_name).join(rel),
            Err(_) => snapshot_dir.to_path_buf(),
        }
    }

    /// Resolve read for a Hermes nested skill leaf.
    ///
    /// Consults the active_resolver with `"category/skill"`.
    pub(super) fn resolve_hermes_nested_read(
        &self,
        category: &str,
        skill_name: &str,
    ) -> ReadResolution {
        self.resolve_hermes_nested_read_pinned(category, skill_name)
            .1
    }

    /// Single-read counterpart of [`Self::resolve_hermes_nested_read`], mirroring
    /// [`Self::resolve_skill_read_pinned`] for the Hermes layout: the nested
    /// skill's `ActiveTarget` is read once and used for both the open-time
    /// decision and handle pinning, closing the same TOCTOU window.
    pub(super) fn resolve_hermes_nested_read_pinned(
        &self,
        category: &str,
        skill_name: &str,
    ) -> (Option<ActiveTarget>, ReadResolution) {
        // Non-skill children of a category (a directory without `SKILL.md`,
        // e.g. `apple/docs`) are plain passthrough — never gated by
        // activation. Without this, an attached resolver has no entry for
        // `category/child` and would (incorrectly) map it to `Hidden`,
        // hiding files like `apple/docs/readme.txt`. Category-child *files*
        // are reclassified to `CategoryPassthrough` earlier and never reach
        // here; this guard covers the directory case (including brand-new
        // install/staging dirs that have no `SKILL.md` yet).
        if !self.hermes_nested_is_skill(category, skill_name) {
            return (None, ReadResolution::Source);
        }
        let nested_id = Self::hermes_skill_id(category, skill_name);
        let resolver = match self.active_resolver.as_ref() {
            Some(r) => r,
            None => return (None, ReadResolution::Source),
        };
        let target = resolver.get(&nested_id);
        let resolution = match &target {
            None => ReadResolution::Hidden,
            Some(t) => self.resolve_from_target(&nested_id, t),
        };
        (target, resolution)
    }

    /// Compiled nested `SKILL.md` honoring a pinned activation target.
    ///
    /// Mirrors [`Self::compiled_skill_md_pinned`] for the Hermes layout: when a
    /// handle carries a pinned [`ActiveTarget`], the read resolves against that
    /// target instead of re-consulting the live resolver, so a handle opened on
    /// a snapshot never switches to the live source and a handle opened on the
    /// live source stays readable after the skill is later hidden.
    pub(super) fn compiled_hermes_nested_skill_md_pinned(
        &self,
        category: &str,
        skill_name: &str,
        pinned: Option<&ActiveTarget>,
    ) -> Option<String> {
        let resolution = match pinned {
            // Non-skill category children (e.g. `apple/docs`) are plain
            // passthrough and never gated; keep them on the source path.
            Some(_) if !self.hermes_nested_is_skill(category, skill_name) => ReadResolution::Source,
            Some(target) => {
                let nested_id = Self::hermes_skill_id(category, skill_name);
                self.resolve_from_target(&nested_id, target)
            }
            None => self.resolve_hermes_nested_read(category, skill_name),
        };
        let physical_path = match resolution {
            ReadResolution::Hidden => return None,
            ReadResolution::Source => self
                .source_base()
                .join(category)
                .join(skill_name)
                .join("SKILL.md"),
            ReadResolution::Snapshot { dir, .. } => dir.join("SKILL.md"),
        };
        let raw = std::fs::read_to_string(&physical_path).ok()?;
        Some(self.transform_pipeline.run(&raw))
    }

    /// Physical read dir for a Hermes nested skill.
    pub(super) fn hermes_nested_skill_read_dir(
        &self,
        category: &str,
        skill_name: &str,
    ) -> Option<PathBuf> {
        match self.resolve_hermes_nested_read(category, skill_name) {
            ReadResolution::Hidden => None,
            ReadResolution::Source => Some(self.source_base().join(category).join(skill_name)),
            ReadResolution::Snapshot { dir, .. } => Some(dir),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::{ActiveSkillResolver, ActiveTarget};
    use crate::{MountConfig, MountOptions, mount_background_configured};
    use parking_lot::RwLock;
    use skillfs_core::transform::TransformPipeline;
    use skillfs_core::{ParseConfig, SharedSkillStore, store::SkillStore};
    use std::io::Read;
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn attrs_and_opens_share_results_but_mutable_reads_and_raw_do_not() {
        for nested in [false, true] {
            let tmp = tempfile::tempdir().unwrap();
            let id = if nested { "cloud/web" } else { "web" };
            let physical = tmp.path().join(id).join("SKILL.md");
            std::fs::create_dir_all(physical.parent().unwrap()).unwrap();
            std::fs::write(
                &physical,
                "<!-- @if os == plan9 -->\nhidden\n<!-- @endif -->\nvisible\n",
            )
            .unwrap();
            let mut store = SkillStore::new();
            store.load_from_directory(tmp.path(), &ParseConfig::default());
            let mut fs = SkillFs::new(
                tmp.path().into(),
                tmp.path().into(),
                Arc::new(RwLock::new(store)),
                false,
            );
            let category = nested.then_some("cloud");
            let attr = fs.transformed_skill_attr("web", category).unwrap();
            assert_eq!(fs.transform_cache.events(), [0, 1, 0, 0]);
            let content = fs.capture_transformed(id, &physical, None).unwrap();
            assert_eq!(content.len() as u64, attr.size);
            let again = fs.capture_transformed(id, &physical, None).unwrap();
            assert!(Arc::ptr_eq(&content, &again));
            assert_eq!(fs.transform_cache.events(), [2, 1, 0, 0]);
            // These are the existing read helpers used by mutable handles.
            let mutable = if nested {
                fs.compiled_hermes_nested_skill_md_pinned("cloud", "web", None)
            } else {
                fs.compiled_skill_md_pinned("web", None)
            }
            .unwrap();
            assert_eq!(mutable, &*content);
            assert_eq!(fs.transform_cache.events(), [2, 1, 0, 0]);
            fs.transform_pipeline.set_directive(None);
            let raw_attr = fs.transformed_skill_attr("web", category).unwrap();
            assert_eq!(raw_attr.size, std::fs::metadata(&physical).unwrap().len());
            assert_eq!(fs.transform_cache.events(), [2, 1, 0, 0]);
        }
    }

    /// Minimal FUSE-availability probe (mirrors the integration harness) so
    /// mount-based unit tests skip gracefully where `/dev/fuse` is unusable.
    fn fuse_available() -> bool {
        if !std::path::Path::new("/dev/fuse").exists() {
            return false;
        }
        let dev = std::ffi::CString::new("/dev/fuse").expect("cstring");
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

    /// Classify a resolution into a stable tag for cross-checking against the
    /// pinned target without requiring `PartialEq` on `ReadResolution`.
    fn tag(res: &ReadResolution) -> &'static str {
        match res {
            ReadResolution::Source => "source",
            ReadResolution::Snapshot { .. } => "snapshot",
            ReadResolution::Hidden => "hidden",
        }
    }

    fn skillfs_with_resolver(source: &Path, resolver: ActiveSkillResolver) -> SkillFs {
        let store: SharedSkillStore = Arc::new(RwLock::new(SkillStore::new()));
        // Empty pipeline: this test exercises resolver logic only and must not
        // pay for environment detection.
        SkillFs::new_with_pipeline(
            source.to_path_buf(),
            source.to_path_buf(),
            store,
            false,
            TransformPipeline::empty(),
        )
        .with_active_resolver(Arc::new(resolver))
    }

    /// The single-read `resolve_skill_read_pinned` must return a target and a
    /// resolution that agree, and its resolution must match the legacy
    /// `resolve_skill_read` entry point. This guards against re-introducing a
    /// second, independent resolver read in `open` (the TOCTOU window).
    #[test]
    fn pinned_read_target_and_resolution_are_consistent() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path();
        let resolver = ActiveSkillResolver::new(src.to_path_buf());
        resolver.set(
            "cur",
            ActiveTarget::Current {
                source_dir: src.join("cur"),
            },
        );
        resolver.set(
            "snap",
            ActiveTarget::Snapshot {
                snapshot_dir: src.join("snap/.skill-meta/versions/v1"),
                version: "v1".to_string(),
            },
        );
        resolver.set(
            "hid",
            ActiveTarget::Hidden {
                reason: "risk".to_string(),
            },
        );
        let fs = skillfs_with_resolver(src, resolver);

        for (name, want_target, want_tag) in [
            ("cur", "current", "source"),
            ("snap", "snapshot", "snapshot"),
            ("hid", "hidden", "hidden"),
            ("absent", "none", "hidden"),
        ] {
            let (target, resolution) = fs.resolve_skill_read_pinned(name);
            // Resolution must agree with the single-decision entry point.
            assert_eq!(
                tag(&resolution),
                tag(&fs.resolve_skill_read(name)),
                "resolution disagreement for {name}"
            );
            // The pinned target and the derived resolution must describe the
            // same decision — never Current-target with a Snapshot resolution.
            let target_kind = match &target {
                None => "none",
                Some(ActiveTarget::Current { .. }) => "current",
                Some(ActiveTarget::Snapshot { .. }) => "snapshot",
                Some(ActiveTarget::Hidden { .. }) => "hidden",
            };
            assert_eq!(target_kind, want_target, "target kind for {name}");
            assert_eq!(tag(&resolution), want_tag, "resolution tag for {name}");
        }
    }

    /// The public `SkillFs::new` must keep the directive stage enabled by
    /// default so embedders that construct it directly still get compiled
    /// `SKILL.md` (byte-compatible with pre-pipeline SkillFS), not raw content.
    /// Managed mounts opt out via `new_with_pipeline`.
    #[test]
    fn public_new_defaults_to_compiled_skill_md() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path();
        // A directive that must be stripped (evaluates false on any host) plus
        // a line that must survive. Raw output would keep the directive markers.
        std::fs::create_dir_all(src.join("foo")).unwrap();
        std::fs::write(
            src.join("foo/SKILL.md"),
            "<!-- @if os == plan9 -->\nHIDDEN-BRANCH\n<!-- @endif -->\nVISIBLE-LINE\n",
        )
        .unwrap();

        let mut store = SkillStore::new();
        store.load_from_directory(src, &ParseConfig::default());
        let store: SharedSkillStore = Arc::new(RwLock::new(store));

        // Default public constructor — no `with_directive_enabled` call.
        let fs = SkillFs::new(src.to_path_buf(), src.to_path_buf(), store, false);
        assert_eq!(
            fs.transform_stage_names(),
            vec!["directive"],
            "public new must enable the directive stage by default"
        );

        let out = fs
            .compiled_skill_md("foo")
            .expect("skill foo should be readable");
        assert!(out.contains("VISIBLE-LINE"), "compiled output: {out}");
        assert!(
            !out.contains("HIDDEN-BRANCH"),
            "false directive branch must be stripped: {out}"
        );
        assert!(
            !out.contains("@if"),
            "directive markers must be stripped: {out}"
        );
    }

    /// Building block: the flat pinned-resolution helper consults the resolver
    /// exactly once per call, returning both the pinned target and the decision
    /// from a single read. (The end-to-end guard that `open` actually uses this
    /// single read is `open_reads_resolver_once_end_to_end`.)
    #[test]
    fn resolve_skill_read_pinned_reads_resolver_once() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path();
        let resolver = Arc::new(ActiveSkillResolver::new(src.to_path_buf()));
        resolver.set(
            "web",
            ActiveTarget::Current {
                source_dir: src.join("web"),
            },
        );
        let store: SharedSkillStore = Arc::new(RwLock::new(SkillStore::new()));
        let fs = SkillFs::new_with_pipeline(
            src.to_path_buf(),
            src.to_path_buf(),
            store,
            false,
            TransformPipeline::empty(),
        )
        .with_active_resolver(resolver.clone());

        let before = resolver.get_call_count();
        let _ = fs.resolve_skill_read_pinned("web");
        assert_eq!(
            resolver.get_call_count() - before,
            1,
            "one pinned resolution must be a single resolver read"
        );
        // A second resolution is a second single read — never batched or doubled.
        let _ = fs.resolve_skill_read_pinned("web");
        assert_eq!(resolver.get_call_count() - before, 2);
    }

    /// Same single-read guarantee for the Hermes nested pinned-resolution
    /// boundary.
    #[test]
    fn resolve_hermes_nested_read_pinned_reads_resolver_once() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path();
        // hermes_nested_is_skill checks for a physical SKILL.md.
        std::fs::create_dir_all(src.join("cloud/deploy")).unwrap();
        std::fs::write(src.join("cloud/deploy/SKILL.md"), "x\n").unwrap();

        let resolver = Arc::new(ActiveSkillResolver::new(src.to_path_buf()));
        resolver.set(
            "cloud/deploy",
            ActiveTarget::Current {
                source_dir: src.join("cloud/deploy"),
            },
        );
        let store: SharedSkillStore = Arc::new(RwLock::new(SkillStore::new()));
        let fs = SkillFs::new_with_pipeline(
            src.to_path_buf(),
            src.to_path_buf(),
            store,
            false,
            TransformPipeline::empty(),
        )
        .with_active_resolver(resolver.clone());

        let before = resolver.get_call_count();
        let _ = fs.resolve_hermes_nested_read_pinned("cloud", "deploy");
        assert_eq!(
            resolver.get_call_count() - before,
            1,
            "one nested pinned resolution must be a single resolver read"
        );
    }

    /// End-to-end TOCTOU guard at the real `open` boundary, timing-independent.
    ///
    /// An `open`-decision scope (thread-local, see [`open_decision_scope`])
    /// tallies only the resolver reads made inside the `open` callback, so
    /// `lookup`/`getattr`/`read` reads never count and the assertion has no
    /// dependence on kernel entry/attr cache timing. A single agent `open` must
    /// read the activation target exactly once, so the decision and the
    /// handle's pinned target come from one snapshot. The pre-fix `open` read
    /// the resolver twice (pin + decide), which this counts as 2 and fails on.
    #[test]
    fn open_reads_activation_target_once_end_to_end() {
        if !fuse_available() {
            eprintln!("SKIP open_reads_activation_target_once_end_to_end: FUSE unavailable");
            return;
        }
        let src_dir = tempfile::tempdir().unwrap();
        let mnt_dir = tempfile::tempdir().unwrap();
        let src = src_dir.path();
        std::fs::create_dir_all(src.join("web")).unwrap();
        std::fs::write(src.join("web/SKILL.md"), "# Current\n").unwrap();

        let resolver = Arc::new(ActiveSkillResolver::new(src.to_path_buf()));
        resolver.set(
            "web",
            ActiveTarget::Current {
                source_dir: src.join("web"),
            },
        );

        let mut store = SkillStore::new();
        store.load_from_directory(src, &ParseConfig::default());
        let store: SharedSkillStore = Arc::new(RwLock::new(store));

        // `_handle` unmounts on drop (best-effort, never panics), so cleanup is
        // automatic even if an assertion below panics.
        let _handle = mount_background_configured(
            mnt_dir.path(),
            src,
            store,
            MountOptions::default(),
            false,
            MountConfig {
                active_resolver: Some(resolver.clone()),
                ..MountConfig::default()
            },
        )
        .expect("mount");
        std::thread::sleep(Duration::from_millis(300));

        let path = mnt_dir.path().join("skills/web/SKILL.md");
        let before = resolver.open_decision_reads();
        let mut f = std::fs::File::open(&path).expect("open");
        let mut buf = String::new();
        f.read_to_string(&mut buf).expect("read");
        let open_reads = resolver.open_decision_reads() - before;

        assert_eq!(buf, "# Current\n", "should serve Current live content");
        assert_eq!(
            open_reads, 1,
            "open must read the activation target exactly once (2 = pre-fix TOCTOU double read); got {open_reads}"
        );
    }
}
