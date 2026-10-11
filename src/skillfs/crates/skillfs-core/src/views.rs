//! View configuration for SkillFS.
//!
//! `skillfs-views.toml` controls which skills are visible at mount time
//! (the default view) and which are accessible only via the `skill-discover`
//! passthrough table (secondary views).
//!
//! # Format
//!
//! ```toml
//! [[view]]
//! name = "major"
//! default = true
//! description = "Core productivity skills used daily"
//! skills = ["github", "notion", "slack"]
//!
//! [[view]]
//! name = "other"
//! default = false
//! description = "Remaining skills available on demand"
//! skills = ["apple-notes", "blogwatcher"]
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use tracing::warn;

// ---------------------------------------------------------------------------
// ViewConfig
// ---------------------------------------------------------------------------

/// Configuration for a single named view.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ViewConfig {
    /// Unique view name (e.g. "major", "ai-tools", "other").
    pub name: String,

    /// When `true`, this view's skills are shown directly in `/skills` at
    /// mount time. Exactly one view should have `default = true`.
    #[serde(default)]
    pub default: bool,

    /// Human-readable description. Shown in `skill-discover` frontmatter so
    /// the AI understands what each view contains.
    #[serde(default)]
    pub description: String,

    /// Skill names belonging to this view (must match source directory names).
    #[serde(default)]
    pub skills: Vec<String>,
}

// ---------------------------------------------------------------------------
// ViewsConfig
// ---------------------------------------------------------------------------

/// Full views configuration loaded from `skillfs-views.toml`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ViewsConfig {
    #[serde(rename = "view")]
    pub views: Vec<ViewConfig>,
}

/// Sequence that makes every staging path unique inside this process.
static STAGING_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// How many staging names one save may try before giving up. A name can only
/// be taken by an entry stranded by a crashed save under the same pid.
const MAX_STAGING_ATTEMPTS: usize = 16;

/// Create the exclusive staging file for one save.
///
/// The name carries the pid (separating processes) and a fresh sequence
/// number (separating saves inside one process, threads included), so no two
/// saves ever share a path and no call can unlink or publish another call's
/// staging file. `create_new` also refuses an entry stranded at that exact
/// name by a crashed save instead of writing through it: the next sequence
/// number is used instead, and the stranded entry is left alone — only the
/// writer that created a staging file may remove it.
fn create_staging_file(source_dir: &Path) -> std::io::Result<(PathBuf, std::fs::File)> {
    for _ in 0..MAX_STAGING_ATTEMPTS {
        let path = source_dir.join(format!(
            ".skillfs-views.toml.{}.{}.tmp",
            std::process::id(),
            STAGING_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "every candidate staging path for skillfs-views.toml is taken",
    ))
}

impl ViewsConfig {
    /// Load from `<source_dir>/skillfs-views.toml`.
    ///
    /// Returns `None` if the file does not exist or fails to parse.
    pub fn load(source_dir: &Path) -> Option<Self> {
        let path = source_dir.join("skillfs-views.toml");
        let content = std::fs::read_to_string(&path).ok()?;
        match toml::from_str::<ViewsConfig>(&content) {
            Ok(cfg) => {
                let default_views = cfg.views.iter().filter(|view| view.default).count();
                if default_views > 1 {
                    warn!(
                        default_views,
                        "skillfs-views.toml marks several views as default; only the \
                         first drives /skills, the others stay discoverable as \
                         secondary views"
                    );
                }
                Some(cfg)
            }
            Err(e) => {
                warn!("failed to parse skillfs-views.toml: {e}");
                None
            }
        }
    }

    /// Return the default view (first one with `default = true`).
    pub fn default_view(&self) -> Option<&ViewConfig> {
        self.default_view_index().map(|index| &self.views[index])
    }

    /// Return every view other than the default view.
    ///
    /// `default = true` marks the view shown directly in `/skills`; every
    /// other view is a secondary view whose skills `skill-discover` lists.
    /// Filtering on the flag alone dropped views after the first default one
    /// from both lists, so a skill assigned only to a second
    /// `default = true` view disappeared from the mounted view entirely.
    pub fn secondary_views(&self) -> Vec<&ViewConfig> {
        let default = self.default_view_index();
        self.views
            .iter()
            .enumerate()
            .filter(|(index, _)| Some(*index) != default)
            .map(|(_, view)| view)
            .collect()
    }

    /// Index of the default view (first one with `default = true`).
    fn default_view_index(&self) -> Option<usize> {
        self.views.iter().position(|view| view.default)
    }

    /// Return the skill names in the default view.
    pub fn default_skills(&self) -> Vec<String> {
        self.default_view()
            .map(|v| v.skills.clone())
            .unwrap_or_default()
    }

    /// Return existing default-view skills plus skills not assigned to any view.
    ///
    /// Resolve automatic membership in memory so read-only mounts need not
    /// rewrite the source configuration. Explicit secondary assignments remain
    /// excluded, including when the file has no default view.
    pub fn effective_default_skills(&self, store: &crate::store::SkillStore) -> Vec<String> {
        let (mut primary, _) = store.split_primary(Some(&self.default_skills()));
        let assigned = self.all_assigned_skills();
        primary.extend(
            store
                .list()
                .into_iter()
                .filter(|name| !assigned.contains(*name))
                .map(str::to_string),
        );
        primary
    }

    /// Return all skill names assigned to any view.
    pub fn all_assigned_skills(&self) -> HashSet<String> {
        self.views
            .iter()
            .flat_map(|v| v.skills.iter().cloned())
            .collect()
    }

    /// Append `new_skills` to the default view's skills list and save.
    ///
    /// Used for auto-assigning newly installed skills that are not yet in
    /// any view.
    pub fn assign_to_default(
        &mut self,
        source_dir: &Path,
        new_skills: &[String],
    ) -> std::io::Result<()> {
        if let Some(view) = self.views.iter_mut().find(|v| v.default) {
            for skill in new_skills {
                if !view.skills.contains(skill) {
                    view.skills.push(skill.clone());
                }
            }
        }
        self.save(source_dir)
    }

    /// Serialize and write to `<source_dir>/skillfs-views.toml`.
    ///
    /// Uses write-to-tmp + rename for atomicity: if the process crashes
    /// mid-write the target file is never left in a truncated state. Every
    /// save stages on its own exclusive path and removes only the file it
    /// created, so concurrent saves — threads in one process included — never
    /// unlink or publish another call's staging file.
    pub fn save(&self, source_dir: &Path) -> std::io::Result<()> {
        let path = source_dir.join("skillfs-views.toml");
        let content = toml::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
        let (tmp_path, mut file) = create_staging_file(source_dir)?;
        let result = (|| {
            use std::io::Write;
            file.write_all(content.as_bytes())?;
            std::fs::rename(&tmp_path, &path)
        })();
        if result.is_err() {
            // No other save ever stages at this path, so this removes only the
            // file this call created.
            let _ = std::fs::remove_file(&tmp_path);
        }
        result
    }

    /// Publish a config without replacing an existing one.
    ///
    /// Same staging sequence as [`Self::save`], but the publication step is
    /// no-replace: the target only ever appears as a fully written file, and
    /// if it appeared since the caller's absence check — another process, or
    /// an editor save — the call fails with `AlreadyExists` instead of
    /// renaming over it. Use this where "create only when absent" is the
    /// contract; [`Self::save`] remains the replace-in-place path for
    /// updating an existing config.
    pub fn save_new(&self, source_dir: &Path) -> std::io::Result<()> {
        let path = source_dir.join("skillfs-views.toml");
        let content = toml::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
        let (tmp_path, mut file) = create_staging_file(source_dir)?;
        let result = (|| {
            use std::io::Write;
            file.write_all(content.as_bytes())?;
            // `hard_link` is the no-replace publication primitive: it fails
            // with `AlreadyExists` when the name is taken (a symlink counts)
            // and never follows or overwrites the target. The staging file is
            // on the same directory, so the link cannot cross filesystems.
            std::fs::hard_link(&tmp_path, &path)
        })();
        drop(file);
        // Only this call stages at this path; dropping our own staging name is
        // correct on success (the target is its second link) and on failure.
        let _ = std::fs::remove_file(&tmp_path);
        result
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_config() -> ViewsConfig {
        ViewsConfig {
            views: vec![
                ViewConfig {
                    name: "major".to_string(),
                    default: true,
                    description: "Core skills".to_string(),
                    skills: vec!["github".to_string(), "notion".to_string()],
                },
                ViewConfig {
                    name: "other".to_string(),
                    default: false,
                    description: "Remaining skills".to_string(),
                    skills: vec!["apple-notes".to_string(), "blogwatcher".to_string()],
                },
            ],
        }
    }

    #[test]
    fn test_default_view() {
        let cfg = make_config();
        let dv = cfg.default_view().unwrap();
        assert_eq!(dv.name, "major");
        assert!(dv.default);
    }

    #[test]
    fn test_secondary_views() {
        let cfg = make_config();
        let sv = cfg.secondary_views();
        assert_eq!(sv.len(), 1);
        assert_eq!(sv[0].name, "other");
    }

    #[test]
    fn test_default_skills() {
        let cfg = make_config();
        let skills = cfg.default_skills();
        assert_eq!(skills, vec!["github", "notion"]);
    }

    #[test]
    fn test_all_assigned_skills() {
        let cfg = make_config();
        let all = cfg.all_assigned_skills();
        assert!(all.contains("github"));
        assert!(all.contains("apple-notes"));
    }

    #[test]
    fn test_save_and_load() {
        let dir = TempDir::new().unwrap();
        let cfg = make_config();
        cfg.save(dir.path()).unwrap();

        let loaded = ViewsConfig::load(dir.path()).unwrap();
        assert_eq!(loaded.views.len(), 2);
        assert_eq!(loaded.default_skills(), vec!["github", "notion"]);
    }

    #[cfg(unix)]
    #[test]
    fn test_save_does_not_publish_through_a_planted_staging_entry() {
        // A fixed staging path makes any entry already sitting there the
        // publication vehicle: writing through a symlink would clobber its
        // target and rename the link itself onto the config path.
        let dir = TempDir::new().unwrap();
        let victim = dir.path().join("victim.toml");
        std::fs::write(&victim, "untouched").unwrap();
        std::os::unix::fs::symlink(&victim, dir.path().join(".skillfs-views.toml.tmp")).unwrap();

        make_config().save(dir.path()).unwrap();

        let saved = dir.path().join("skillfs-views.toml");
        assert!(
            !std::fs::symlink_metadata(&saved)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the published config must be a regular file"
        );
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "untouched");
        assert_eq!(
            ViewsConfig::load(dir.path()).unwrap().default_skills(),
            vec!["github", "notion"]
        );
    }

    #[test]
    fn concurrent_saves_in_one_process_all_publish() {
        // Every save must own its staging path. With one path shared by all
        // saves in the process, concurrent callers unlink each other's
        // half-written file and the rename fails: an 8-thread run of 40
        // saves each published only 86 of the 320 calls.
        let dir = TempDir::new().unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|worker| {
                let directory = dir.path().to_path_buf();
                let barrier = std::sync::Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let mut config = make_config();
                    config.views[0].skills = vec![format!("skill-{worker}")];
                    barrier.wait();
                    (0..40).filter(|_| config.save(&directory).is_ok()).count()
                })
            })
            .collect();
        let published: usize = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .sum();
        assert_eq!(published, 320, "every concurrent save must publish");
        // Whichever save won, the target is a complete config, never a torn one.
        assert_eq!(
            ViewsConfig::load(dir.path())
                .expect("complete config")
                .views
                .len(),
            2
        );
    }

    #[test]
    fn concurrent_save_new_publishes_exactly_once() {
        // Create-only publication is the atomic gate: every thread passes the
        // same absence state before publishing, and the no-replace link lets
        // exactly one win while the others report `AlreadyExists`.
        let dir = TempDir::new().unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|worker| {
                let directory = dir.path().to_path_buf();
                let barrier = std::sync::Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let mut config = make_config();
                    config.views[0].skills = vec![format!("skill-{worker}")];
                    barrier.wait();
                    config.save_new(&directory)
                })
            })
            .collect();
        let outcomes: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(
            outcomes.iter().filter(|r| r.is_ok()).count(),
            1,
            "exactly one create-only save may publish: {outcomes:?}"
        );
        assert!(
            outcomes.iter().filter(|r| r.is_err()).all(|r| r
                .as_ref()
                .err()
                .map(std::io::Error::kind)
                == Some(std::io::ErrorKind::AlreadyExists)),
            "losers must report AlreadyExists: {outcomes:?}"
        );
        assert!(
            ViewsConfig::load(dir.path()).is_some(),
            "one complete config"
        );
        assert!(
            staging_names(dir.path()).is_empty(),
            "no staging file may be left behind: {:?}",
            staging_names(dir.path())
        );
    }

    #[test]
    fn save_new_refuses_to_replace_an_existing_config() {
        // A target that exists — whether hand-written or created after the
        // caller's absence check — must survive byte for byte.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("skillfs-views.toml");
        std::fs::write(&path, "user-authored config\n").unwrap();

        let error = make_config()
            .save_new(dir.path())
            .expect_err("an existing config must not be replaced");
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "user-authored config\n"
        );
        assert!(
            staging_names(dir.path()).is_empty(),
            "the loser must clean up its staging file: {:?}",
            staging_names(dir.path())
        );

        // Publishing into a config-less directory succeeds and is loadable.
        let fresh = TempDir::new().unwrap();
        make_config().save_new(fresh.path()).expect("publish");
        assert!(ViewsConfig::load(fresh.path()).is_some());
        assert!(
            staging_names(fresh.path()).is_empty(),
            "the winner must drop its staging name: {:?}",
            staging_names(fresh.path())
        );
    }

    fn staging_names(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|name| name.contains(".tmp"))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn save_leaves_a_foreign_staging_entry_alone() {
        // Only the call that created a staging file may remove it: an entry
        // left by a crashed save (or a concurrent writer) must survive.
        let dir = TempDir::new().unwrap();
        let legacy = dir
            .path()
            .join(format!(".skillfs-views.toml.{}.tmp", std::process::id()));
        std::fs::write(&legacy, "legacy leftover").unwrap();
        let current = dir.path().join(format!(
            ".skillfs-views.toml.{}.{}.tmp",
            std::process::id(),
            STAGING_SEQUENCE.load(Ordering::Relaxed)
        ));
        std::fs::write(&current, "stranded leftover").unwrap();

        make_config().save(dir.path()).unwrap();

        assert_eq!(
            std::fs::read_to_string(&legacy).unwrap(),
            "legacy leftover",
            "a foreign staging entry must be left alone"
        );
        assert_eq!(
            std::fs::read_to_string(&current).unwrap(),
            "stranded leftover",
            "a stranded entry at a staging name must be left alone"
        );
        assert_eq!(
            ViewsConfig::load(dir.path()).unwrap().default_skills(),
            vec!["github", "notion"]
        );
    }

    #[test]
    fn test_assign_to_default() {
        let dir = TempDir::new().unwrap();
        let mut cfg = make_config();
        cfg.save(dir.path()).unwrap();

        cfg.assign_to_default(dir.path(), &["new-skill".to_string()])
            .unwrap();

        let loaded = ViewsConfig::load(dir.path()).unwrap();
        assert!(loaded.default_skills().contains(&"new-skill".to_string()));
    }

    #[test]
    fn effective_default_includes_unassigned_without_changing_views() {
        let mut cfg = make_config();
        let original = cfg.default_skills();
        let mut store = crate::store::SkillStore::new();
        for name in ["github", "apple-notes", "new-skill"] {
            store.upsert(crate::parser::parse_skill_md("# Fixture", name));
        }
        assert_eq!(
            cfg.effective_default_skills(&store),
            vec!["github", "new-skill"]
        );
        assert_eq!(cfg.default_skills(), original);
        cfg.views[0].default = false;
        assert_eq!(cfg.effective_default_skills(&store), vec!["new-skill"]);
    }

    #[test]
    fn test_load_missing_file() {
        let dir = TempDir::new().unwrap();
        assert!(ViewsConfig::load(dir.path()).is_none());
    }

    #[test]
    fn extra_default_views_stay_discoverable() {
        // The documented contract is one `default = true` view, but a file
        // that marks several must not hide skills: every view other than the
        // selected default stays a secondary view, so its skills are listed
        // by skill-discover instead of disappearing from the mounted view.
        let mut cfg = make_config();
        cfg.views[1].default = true; // second default view
        cfg.views.push(ViewConfig {
            name: "extra".to_string(),
            default: false,
            description: String::new(),
            skills: vec!["extra-skill".to_string()],
        });
        let mut store = crate::store::SkillStore::new();
        for name in [
            "github",
            "notion",
            "apple-notes",
            "blogwatcher",
            "extra-skill",
        ] {
            store.upsert(crate::parser::parse_skill_md("# Fixture", name));
        }

        // The first default view still drives /skills.
        assert_eq!(cfg.default_view().unwrap().name, "major");
        assert_eq!(
            cfg.secondary_views()
                .iter()
                .map(|view| view.name.as_str())
                .collect::<Vec<_>>(),
            ["other", "extra"],
            "views after the selected default stay secondary views"
        );

        // No skill in the store may become unreachable: it is either in the
        // default view or listed by a secondary view.
        let mut visible: HashSet<String> =
            cfg.effective_default_skills(&store).into_iter().collect();
        for view in cfg.secondary_views() {
            visible.extend(view.skills.iter().cloned());
        }
        for name in store.list() {
            assert!(
                visible.contains(name),
                "skill `{name}` is in no view at all"
            );
        }
    }

    #[test]
    fn effective_default_skills_lists_duplicate_view_entries_once() {
        let mut cfg = make_config();
        cfg.views[0].skills = vec![
            "github".to_string(),
            "github".to_string(),
            "notion".to_string(),
        ];
        let mut store = crate::store::SkillStore::new();
        for name in ["github", "notion", "new-skill"] {
            store.upsert(crate::parser::parse_skill_md("# Fixture", name));
        }

        assert_eq!(
            cfg.effective_default_skills(&store),
            vec![
                "github".to_string(),
                "notion".to_string(),
                "new-skill".to_string()
            ]
        );
    }
}
