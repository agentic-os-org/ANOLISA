//! Path-resolution and inode helpers for `SkillFs`.
//!
//! Covers the conversions between FUSE virtual paths, store keys, and
//! physical filesystem paths, plus the `*at`-syscall parent-fd opener
//! used to sidestep `PATH_MAX`. The base helper [`SkillFs::source_base`]
//! folds in the in-place mount's `/proc/self/fd/{n}` rewrite so every
//! downstream resolver naturally bypasses the FUSE over-mount.

use std::path::{Path, PathBuf};

use fuser::FUSE_ROOT_ID;

use super::SkillFs;
use crate::path::{PathType, is_skill_discover_path};
use crate::security::{
    inbox::{is_inbox_dir_name, is_valid_inbox_skill_name},
    lifecycle::is_reserved_lifecycle_name,
};
use crate::sys::{errno, open_dir_path};

/// Selects whether installer-authorized live I/O targets a loaded skill or a
/// source-root candidate that has not entered the store yet.
#[derive(Clone, Copy)]
pub(super) enum FlatLiveDirKind {
    /// Resolve an existing skill through its stored manifest path.
    ExistingSkill,
    /// Resolve staging and pending-install candidates directly under source.
    SourceRootCandidate,
}

impl SkillFs {
    /// Return the base path for physical file access.
    ///
    /// In in-place mode returns `/proc/self/fd/{n}` (the pre-opened dirfd)
    /// so that reads bypass the FUSE mount layer.  Otherwise returns the
    /// plain source directory path.
    pub(super) fn source_base(&self) -> PathBuf {
        if let Some(fd) = &self.source_dirfd {
            use std::os::unix::io::AsRawFd;
            PathBuf::from(format!("/proc/self/fd/{}", fd.as_raw_fd()))
        } else {
            self.source.clone()
        }
    }

    /// FUSE inode path prefix for a skill dir.
    ///
    /// In normal mode → `/skills/{name}`; in in-place mode → `/{name}`.
    pub(super) fn skill_inode_path(&self, skill_name: &str) -> String {
        if self.in_place {
            format!("/{}", skill_name)
        } else {
            format!("/skills/{}", skill_name)
        }
    }

    /// Inode for the skills directory (the parent of individual skill dirs).
    pub(super) fn skills_dir_ino(&self) -> u64 {
        if self.in_place {
            FUSE_ROOT_ID
        } else {
            self.inodes.lookup_by_path("/skills").unwrap_or(2)
        }
    }

    /// Resolve the physical directory containing a skill's files.
    ///
    /// In in-place mode uses `source_base()` (the pre-opened fd path) so
    /// reads bypass the FUSE mount layer.
    pub(super) fn skill_physical_dir(&self, skill_name: &str) -> PathBuf {
        if self.in_place {
            // Always go through the fd to bypass the FUSE mount.
            self.source_base().join(skill_name)
        } else {
            self.skill_source_path(skill_name)
                .and_then(|p| p.parent().map(|d| d.to_path_buf()))
                .unwrap_or_else(|| self.source.join(skill_name))
        }
    }

    /// Resolve the live directory used by flat installer-authorized I/O.
    pub(super) fn flat_live_dir(&self, skill_name: &str, kind: FlatLiveDirKind) -> PathBuf {
        match kind {
            FlatLiveDirKind::ExistingSkill => self.skill_physical_dir(skill_name),
            FlatLiveDirKind::SourceRootCandidate => self.source_base().join(skill_name),
        }
    }

    /// Resolve the physical directory for an inbox skill candidate
    /// regardless of [`crate::security::ActiveSkillResolver`] visibility.
    /// The inbox is the install / repair entrance for hidden skills; it
    /// must keep working even when the runtime view at `/skills/<skill>`
    /// is hidden or pointed at a snapshot.
    pub(super) fn inbox_skill_dir(&self, skill_name: &str) -> PathBuf {
        self.source_base().join(skill_name)
    }

    /// Returns `true` when `name` is an acceptable inbox skill-name top
    /// segment.
    ///
    /// The inbox enumerates the source root and maps
    /// `/.skillfs-inbox/<name>` to `source/<name>`. To match the
    /// "inbox = skill install / repair entrance" contract — and to keep
    /// the namespace from leaking arbitrary source-root directories —
    /// we admit only names the canonical SkillFS validator
    /// (`skillfs-core::parser::validate_name`) would accept: kebab-case
    /// (`[a-z0-9-]+`), no leading/trailing hyphen, length ≤ 64. This
    /// rejects every dot-prefixed entry (`.git`, `.skill-meta`,
    /// `.skillfs-inbox`, lifecycle reserved roots, `.cache`, …) plus
    /// uppercase / underscored / dotted names that the loader would
    /// also refuse to surface as a skill. `skill-discover` happens to
    /// match the kebab-case shape, so it is rejected explicitly.
    pub(super) fn is_inbox_skill_name_allowed(name: &str) -> bool {
        if !is_valid_inbox_skill_name(name) {
            return false;
        }
        if is_skill_discover_path(name) {
            return false;
        }
        // Lifecycle reserved roots are dot-prefixed and already excluded
        // by `is_valid_inbox_skill_name`, but keep the explicit check so
        // a future relaxation cannot accidentally let `.staging` through.
        if is_reserved_lifecycle_name(name) {
            return false;
        }
        // Same defense in depth for the inbox name itself.
        if is_inbox_dir_name(name) {
            return false;
        }
        true
    }

    /// Resolve the physical SKILL.md path for a skill via the store.
    pub(super) fn skill_source_path(&self, skill_name: &str) -> Option<PathBuf> {
        let store = self.store.read();
        store.get(skill_name).map(|e| e.source_path.clone())
    }

    /// Return the list of skill names to show in /skills (default view).
    ///
    /// Include unassigned skills without persisting view changes; explicit
    /// secondary assignments stay hidden. Without a config, return all skills.
    pub(super) fn primary_skill_names(&self) -> Vec<String> {
        if let Some(cfg) = &self.views_config {
            cfg.effective_default_skills(&self.store.read())
        } else {
            let store = self.store.read();
            store.list().iter().map(|s| s.to_string()).collect()
        }
    }

    #[allow(dead_code)]
    pub(super) fn skill_physical_path(&self, skill_name: &str) -> Option<PathBuf> {
        let store = self.store.read();
        let entry = store.get(skill_name)?;
        Some(entry.source_path.parent()?.to_path_buf())
    }

    /// Build the canonical FUSE path from a parent inode and child name.
    pub(super) fn build_fuse_path(&self, parent: u64, name: &std::ffi::OsStr) -> Option<String> {
        let parent_path = self.inodes.get_path(parent)?;
        let name_str = name.to_string_lossy();
        if parent_path == "/" {
            Some(format!("/{}", name_str))
        } else {
            Some(format!("{}/{}", parent_path, name_str))
        }
    }

    /// Resolve a FUSE virtual path to the underlying physical path.
    ///
    /// Uses `source_base()` (which goes through `/proc/self/fd/{n}` in
    /// in-place mode) so that all I/O bypasses the FUSE layer.
    pub(super) fn resolve_physical_path(&self, fuse_path: &str) -> Option<PathBuf> {
        match self.parse_fuse_path(Path::new(fuse_path)) {
            PathType::SkillDir { skill_name } => Some(self.skill_physical_dir(&skill_name)),
            PathType::SkillMd { skill_name } => {
                Some(self.skill_physical_dir(&skill_name).join("SKILL.md"))
            }
            PathType::Passthrough {
                skill_name,
                relative_path,
            } => Some(self.skill_physical_dir(&skill_name).join(&relative_path)),
            // L1 inbox virtual mapping: `<inbox>/<skill>/<rel>` shares
            // the physical layout with the live source candidate
            // directory. The inbox root itself has no physical backing
            // (it is purely virtual).
            PathType::InboxSkillDir { skill_name } => Some(self.source_base().join(&skill_name)),
            PathType::InboxPassthrough {
                skill_name,
                relative_path,
            } => Some(self.source_base().join(&skill_name).join(&relative_path)),
            PathType::HermesMeta { name } => Some(self.source_base().join(&name)),
            PathType::HermesMetaChild {
                name,
                relative_path,
            }
            | PathType::CategoryPassthrough {
                name,
                relative_path,
            } => Some(self.source_base().join(&name).join(&relative_path)),
            PathType::CategoryDir { category } => Some(self.source_base().join(&category)),
            PathType::NestedSkillDir {
                category,
                skill_name,
            } => Some(self.source_base().join(&category).join(&skill_name)),
            PathType::NestedSkillMd {
                category,
                skill_name,
            } => Some(
                self.source_base()
                    .join(&category)
                    .join(&skill_name)
                    .join("SKILL.md"),
            ),
            PathType::NestedPassthrough {
                category,
                skill_name,
                relative_path,
            } => Some(
                self.source_base()
                    .join(&category)
                    .join(&skill_name)
                    .join(&relative_path),
            ),
            _ => None,
        }
    }

    /// Open the physical parent directory of `fuse_path` and return both the
    /// open fd and the leaf name suitable for an `*at` syscall. Used to
    /// sidestep `PATH_MAX` on absolute physical paths whose total length
    /// exceeds the kernel's userspace path-name limit: the parent itself
    /// stays within `PATH_MAX` for any reachable leaf (because the leaf
    /// component is at least one byte), so opening the parent succeeds and
    /// the `*at` syscall only needs the short leaf component.
    ///
    /// Returns the FUSE-side errno on failure (parent unresolvable, parent
    /// open failed, or leaf missing).
    pub(super) fn open_parent_dir_for(
        &self,
        fuse_path: &str,
    ) -> Result<(std::fs::File, std::ffi::OsString), i32> {
        let path = Path::new(fuse_path);
        let leaf = path
            .file_name()
            .map(|n| n.to_os_string())
            .ok_or(libc::EINVAL)?;
        let parent_fuse = path.parent().ok_or(libc::EINVAL)?;
        let parent_physical = match self.parse_fuse_path(parent_fuse) {
            PathType::SkillDir { skill_name } => self.skill_physical_dir(&skill_name),
            PathType::InboxSkillDir { skill_name } => self.inbox_skill_dir(&skill_name),
            PathType::SkillMd { skill_name } => {
                self.skill_physical_dir(&skill_name).join("SKILL.md")
            }
            PathType::Passthrough {
                skill_name,
                relative_path,
            } => self.skill_physical_dir(&skill_name).join(&relative_path),
            PathType::InboxPassthrough {
                skill_name,
                relative_path,
            } => self.inbox_skill_dir(&skill_name).join(&relative_path),
            PathType::HermesMeta { name } => self.source_base().join(&name),
            PathType::HermesMetaChild {
                name,
                relative_path,
            }
            | PathType::CategoryPassthrough {
                name,
                relative_path,
            } => self.source_base().join(&name).join(&relative_path),
            PathType::CategoryDir { category } => self.source_base().join(&category),
            PathType::NestedSkillDir {
                category,
                skill_name,
            } => self.source_base().join(&category).join(&skill_name),
            PathType::NestedSkillMd {
                category,
                skill_name,
            } => self
                .source_base()
                .join(&category)
                .join(&skill_name)
                .join("SKILL.md"),
            PathType::NestedPassthrough {
                category,
                skill_name,
                relative_path,
            } => self
                .source_base()
                .join(&category)
                .join(&skill_name)
                .join(&relative_path),
            PathType::SkillsDir | PathType::Root | PathType::InboxDir => self.source_base(),
            PathType::Invalid => return Err(libc::ENOTDIR),
        };
        open_dir_path(&parent_physical)
            .map(|f| (f, leaf))
            .map_err(|e| errno(&e))
    }

    pub(super) fn is_staging_skill_root(&self, skill_name: &str) -> bool {
        self.staging_matcher.as_ref().is_some_and(|m| {
            if m.is_staging_root(skill_name) {
                return true;
            }
            // H3: Hermes nested ID — check the leaf component.
            if let Some(leaf) = skill_name.split('/').next_back() {
                if leaf != skill_name {
                    return m.is_staging_root(leaf);
                }
            }
            false
        })
    }

    pub(super) fn is_pending_install(&self, skill_name: &str) -> bool {
        let ctrl = match self.pending_install_controller.as_ref() {
            Some(c) => c,
            None => return false,
        };
        if !ctrl.is_pending(skill_name) {
            return false;
        }
        if let Some(ref resolver) = self.active_resolver {
            if resolver.get(skill_name).is_some() {
                ctrl.clear_if_activated(skill_name);
                return false;
            }
        }
        true
    }

    pub(super) fn should_reject_hidden_write(
        &self,
        skill_name: &str,
        relative_path: Option<&std::path::Path>,
    ) -> bool {
        use crate::fs::read_resolution::ReadResolution;
        if !matches!(self.resolve_skill_read(skill_name), ReadResolution::Hidden) {
            return false;
        }
        if self.is_staging_skill_root(skill_name) || self.is_pending_install(skill_name) {
            return false;
        }
        !self.is_post_publish_grace_allowed(skill_name, relative_path)
    }

    /// Hidden-write gate for a Hermes nested path (`category/skill/...`).
    ///
    /// Mirrors [`Self::should_reject_hidden_write`] but resolves through
    /// [`Self::resolve_hermes_nested_read`] so it honors nested-skill
    /// activation. Crucially, non-skill category children (a directory
    /// without `SKILL.md`, e.g. `apple/docs`) are plain passthrough and are
    /// never rejected — otherwise the flat resolver would treat the synthetic
    /// id `apple/docs` as an unknown (hidden) skill and block creating new
    /// files like `apple/docs/new.txt` while still allowing reads/appends.
    pub(super) fn should_reject_hermes_nested_hidden_write(
        &self,
        category: &str,
        skill_name: &str,
        relative_path: Option<&std::path::Path>,
    ) -> bool {
        use crate::fs::read_resolution::ReadResolution;
        if !self.hermes_nested_is_skill(category, skill_name) {
            return false;
        }
        if !matches!(
            self.resolve_hermes_nested_read(category, skill_name),
            ReadResolution::Hidden
        ) {
            return false;
        }
        let nid = Self::hermes_skill_id(category, skill_name);
        if self.is_staging_skill_root(&nid) || self.is_pending_install(&nid) {
            return false;
        }
        !self.is_post_publish_grace_allowed(&nid, relative_path)
    }

    pub(super) fn is_post_publish_grace_allowed(
        &self,
        skill_name: &str,
        relative_path: Option<&std::path::Path>,
    ) -> bool {
        let ctrl = match self.post_publish_controller.as_ref() {
            Some(c) => c,
            None => return false,
        };
        match relative_path {
            Some(rel) => ctrl.is_grace_allowed(skill_name, rel),
            None => ctrl.has_active_session(skill_name),
        }
    }

    /// Canonical Hermes skill id: `"category/skill"`.
    pub(super) fn hermes_skill_id(category: &str, skill_name: &str) -> String {
        format!("{}/{}", category, skill_name)
    }

    /// Physical source dir for a Hermes nested skill leaf.
    pub(super) fn hermes_skill_physical_dir(&self, category: &str, skill_name: &str) -> PathBuf {
        self.source_base().join(category).join(skill_name)
    }

    /// Whether `<category>/<skill_name>` is a real Hermes nested skill
    /// leaf — i.e. a directory that physically contains `SKILL.md`.
    ///
    /// The Hermes parser is purely lexical and classifies *every*
    /// second-level entry as a nested skill, but only directories with a
    /// `SKILL.md` are actual skills. Plain files/dirs under a category
    /// (e.g. `apple/docs/readme.txt`) are category passthrough and must
    /// never enter activation gating. The probe hits the physical source
    /// via `source_base()` so it bypasses the FUSE over-mount in in-place
    /// mode.
    ///
    /// Uses the shared no-follow regular-file `SKILL.md` predicate so path
    /// classification, store discovery, activation enumeration, and the
    /// resolver all agree on what a Skill is (a symlinked or non-regular
    /// `SKILL.md` is not a marker).
    pub(super) fn hermes_nested_is_skill(&self, category: &str, skill_name: &str) -> bool {
        skillfs_core::store::has_regular_skill_md(
            &self.hermes_skill_physical_dir(category, skill_name),
        )
    }

    /// Whether the direct category child `<category>/<name>` physically
    /// exists and is NOT a directory (a plain file or symlink).
    ///
    /// A skill is always a directory, so a non-directory child under a
    /// category (e.g. `apple/README.md`) can never be a nested skill and
    /// must be served as ordinary passthrough. A missing child returns
    /// `false` so it stays on the nested-skill path (surfacing `ENOENT`
    /// via the normal directory checks rather than being mis-served).
    pub(super) fn hermes_category_child_is_file(&self, category: &str, name: &str) -> bool {
        std::fs::symlink_metadata(self.source_base().join(category).join(name))
            .map(|m| !m.is_dir())
            .unwrap_or(false)
    }

    /// Whether `<name>` is a real top-level Hermes skill — i.e. a
    /// directory directly under the source root that physically contains
    /// `SKILL.md`. Real Hermes workspaces mix top-level skills
    /// (`skill/SKILL.md`) with categorized nested skills
    /// (`category/skill/SKILL.md`); the lexical parser classifies every
    /// top-level entry as a category container, so callers reclassify a
    /// top-level skill back into the flat skill path types.
    ///
    /// Uses the shared no-follow regular-file `SKILL.md` predicate: a
    /// directory whose only `SKILL.md` is a symlink or other non-regular
    /// object is a category (its real nested Skills resolve), consistent
    /// with store discovery and the resolver — never a phantom top-level
    /// Skill that has no activation key and would fail closed to hidden.
    pub(super) fn hermes_is_top_level_skill(&self, name: &str) -> bool {
        skillfs_core::store::has_regular_skill_md(&self.source_base().join(name))
    }
}

#[cfg(test)]
mod tests {
    use std::os::fd::AsRawFd;
    use std::sync::Arc;

    use parking_lot::RwLock;
    use skillfs_core::{ParseConfig, store::SkillStore};

    use super::*;

    #[test]
    fn flat_paths_and_grace_use_the_stored_categorized_source_dir() {
        let source = tempfile::tempdir().expect("source tempdir");
        let skill_dir = source.path().join("catalog/demo");
        std::fs::create_dir_all(&skill_dir).expect("categorized skill dir");
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: demo\ndescription: categorized\n---\nbody\n",
        )
        .expect("categorized SKILL.md");
        std::fs::write(skill_dir.join("notes.txt"), "notes").expect("passthrough file");

        let mut store = SkillStore::new();
        store.load_from_directory(source.path(), &ParseConfig::default());
        let fs = SkillFs::new(
            source.path().join("mount"),
            source.path().to_path_buf(),
            Arc::new(RwLock::new(store)),
            false,
        );

        assert_eq!(
            fs.resolve_physical_path("/skills/demo"),
            Some(skill_dir.clone())
        );
        assert_eq!(
            fs.resolve_physical_path("/skills/demo/SKILL.md"),
            Some(skill_dir.join("SKILL.md"))
        );
        assert_eq!(
            fs.resolve_physical_path("/skills/demo/notes.txt"),
            Some(skill_dir.join("notes.txt"))
        );
        assert_eq!(
            fs.resolve_physical_path("/skills/new-skill"),
            Some(source.path().join("new-skill"))
        );
        assert_eq!(
            fs.flat_live_dir("demo", FlatLiveDirKind::ExistingSkill),
            skill_dir
        );
        // Grace targets loaded skills, while staging/pending candidates retain
        // their direct source-root mapping.
        assert_eq!(
            fs.flat_live_dir("demo", FlatLiveDirKind::SourceRootCandidate),
            source.path().join("demo")
        );

        let (parent, leaf) = fs
            .open_parent_dir_for("/skills/demo/notes.txt")
            .expect("open categorized parent");
        assert_eq!(leaf, "notes.txt");
        let fd_path = PathBuf::from(format!("/proc/self/fd/{}", parent.as_raw_fd()));
        assert_eq!(
            std::fs::canonicalize(fd_path).expect("canonical parent fd"),
            std::fs::canonicalize(&skill_dir).expect("canonical skill dir")
        );
    }
}
