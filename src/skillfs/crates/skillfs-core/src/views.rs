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
use std::path::Path;
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
        let Some(view) = self.views.iter_mut().find(|v| v.default) else {
            // Nothing to assign to. Saving an unchanged config and reporting
            // Ok made the caller log an auto-assignment that never happened.
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no default view configured; new skills were not assigned",
            ));
        };
        for skill in new_skills {
            if !view.skills.contains(skill) {
                view.skills.push(skill.clone());
            }
        }
        self.save(source_dir)
    }

    /// Serialize and write to `<source_dir>/skillfs-views.toml`.
    ///
    /// Uses write-to-tmp + rename for atomicity: if the process crashes
    /// mid-write the target file is never left in a truncated state.
    pub fn save(&self, source_dir: &Path) -> std::io::Result<()> {
        let path = source_dir.join("skillfs-views.toml");
        let tmp_path = source_dir.join(".skillfs-views.toml.tmp");
        let content = toml::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
        std::fs::write(&tmp_path, &content)?;
        std::fs::rename(&tmp_path, &path)?;
        Ok(())
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
    fn test_assign_to_default_without_a_default_view_errors() {
        let dir = TempDir::new().unwrap();
        let mut cfg = make_config();
        for view in &mut cfg.views {
            view.default = false;
        }
        cfg.save(dir.path()).unwrap();
        let path = dir.path().join("skillfs-views.toml");
        let before = std::fs::read_to_string(&path).unwrap();

        let err = cfg
            .assign_to_default(dir.path(), &["new-skill".to_string()])
            .expect_err("a config without a default view cannot assign");
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
        assert!(!cfg.all_assigned_skills().contains("new-skill"));
        // The config file is left untouched.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
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
}
