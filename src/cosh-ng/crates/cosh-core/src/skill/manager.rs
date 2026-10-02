use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::{broadcast, RwLock};

use super::loader;
use super::types::{SkillConfig, SkillLevel};
use super::{COPILOT_CONFIG_DIR, SKILLS_DIR};

/// Central manager for skill discovery, caching, hot-reload and priority
/// merging. Mirrors the role of copilot-shell's `SkillManager`.
pub struct SkillManager {
    cache: RwLock<HashMap<SkillLevel, Vec<SkillConfig>>>,
    project_root: PathBuf,
    custom_paths: Vec<PathBuf>,
    extension_paths: Vec<PathBuf>,
    change_tx: broadcast::Sender<()>,
    #[allow(dead_code)]
    watcher_handle: RwLock<Option<notify::RecommendedWatcher>>,
    user_paths: Vec<PathBuf>,
    system_paths: Vec<PathBuf>,
}

impl SkillManager {
    /// Create a new SkillManager.
    ///
    /// * `project_root` – the current project/workspace root (used for
    ///   project-level skills at `<project>/.copilot-shell/skills/`).
    /// * `custom_paths` – already-expanded custom skill directory paths from
    ///   config (`skills.custom_paths`).
    /// * `extension_paths` – skill directories contributed by loaded extensions.
    pub fn new(
        project_root: PathBuf,
        custom_paths: Vec<PathBuf>,
        extension_paths: Vec<PathBuf>,
    ) -> Arc<Self> {
        let (change_tx, _) = broadcast::channel(16);
        Arc::new(Self {
            cache: RwLock::new(HashMap::new()),
            project_root,
            custom_paths,
            extension_paths,
            change_tx,
            watcher_handle: RwLock::new(None),
            user_paths: user_skill_dirs(
                dirs::home_dir(),
                std::env::var_os("XDG_DATA_HOME").map(PathBuf::from),
            ),
            system_paths: crate::paths::system_data_dirs()
                .into_iter()
                .map(|dir| dir.join(SKILLS_DIR))
                .collect(),
        })
    }

    /// Test constructor that overrides user and system directories to avoid
    /// scanning the real home / system paths.
    #[cfg(test)]
    pub fn new_isolated(
        project_root: PathBuf,
        custom_paths: Vec<PathBuf>,
        user_dir: Option<PathBuf>,
        system_dir: Option<PathBuf>,
    ) -> Arc<Self> {
        let mut manager = Self::new(project_root, custom_paths, Vec::new());
        let inner = Arc::get_mut(&mut manager).unwrap();
        if let Some(dir) = user_dir {
            inner.user_paths = vec![dir];
        }
        if let Some(dir) = system_dir {
            inner.system_paths = vec![dir];
        }
        manager
    }

    /// Rescan all skill directories and update the internal cache.
    pub async fn refresh(&self) {
        let mut new_cache: HashMap<SkillLevel, Vec<SkillConfig>> = HashMap::new();

        for &level in SkillLevel::all() {
            let skills: Vec<_> = self
                .dirs_of(level)
                .iter()
                .flat_map(|dir| loader::load_skills_from_dir(dir, level))
                .collect();
            if !skills.is_empty() {
                new_cache.insert(level, skills);
            }
        }

        *self.cache.write().await = new_cache;
        let _ = self.change_tx.send(());
    }

    /// Return a deduplicated, priority-merged list of all available skills.
    /// Higher-priority levels (Project > Custom > User > Extension > System)
    /// shadow lower-priority skills with the same name.
    pub async fn list(&self) -> Vec<SkillConfig> {
        let cache = self.cache.read().await;
        let mut merged: HashMap<String, SkillConfig> = HashMap::new();

        // Insert in reverse priority order so that higher-priority entries
        // overwrite lower ones.
        for &level in SkillLevel::all().iter().rev() {
            if let Some(skills) = cache.get(&level) {
                // Earlier directories within a level also win, matching load().
                for skill in skills.iter().rev() {
                    merged.insert(skill.name.clone(), skill.clone());
                }
            }
        }

        let mut result: Vec<_> = merged.into_values().collect();
        result.sort_by(|a, b| a.name.cmp(&b.name));
        result
    }

    /// Look up a single skill by name, returning the highest-priority match.
    pub async fn load(&self, name: &str) -> Option<SkillConfig> {
        let cache = self.cache.read().await;
        for &level in SkillLevel::all() {
            if let Some(skills) = cache.get(&level) {
                if let Some(skill) = skills.iter().find(|s| s.name == name) {
                    return Some(skill.clone());
                }
            }
        }
        None
    }

    /// Subscribe to change notifications (e.g. after hot-reload).
    #[allow(dead_code)]
    pub fn subscribe(&self) -> broadcast::Receiver<()> {
        self.change_tx.subscribe()
    }

    /// Start file-system watchers for all relevant skill directories.
    /// Changes are debounced (150 ms) then trigger `refresh()`.
    pub async fn start_watching(self: &Arc<Self>) {
        use notify::{RecursiveMode, Watcher};

        let (fs_tx, mut fs_rx) = tokio::sync::mpsc::channel::<()>(16);

        let watcher_result =
            notify::recommended_watcher(move |res: Result<notify::Event, notify::Error>| {
                if res.is_ok() {
                    let _ = fs_tx.try_send(());
                }
            });

        let mut watcher = match watcher_result {
            Ok(w) => w,
            Err(e) => {
                tracing::warn!(target: "skill_manager", "Failed to create file watcher: {e}");
                return;
            }
        };

        for dir in self.watch_dirs() {
            if dir.exists() {
                if let Err(e) = watcher.watch(&dir, RecursiveMode::Recursive) {
                    tracing::warn!(
                        target: "skill_manager",
                        "Failed to watch {}: {e}",
                        dir.display()
                    );
                }
            }
        }

        *self.watcher_handle.write().await = Some(watcher);

        // Spawn debounce task
        let manager = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                // Wait for the first change event
                if fs_rx.recv().await.is_none() {
                    break;
                }
                // Debounce: wait 150 ms, drain any further events
                tokio::time::sleep(tokio::time::Duration::from_millis(150)).await;
                while fs_rx.try_recv().is_ok() {}

                manager.refresh().await;
            }
        });
    }

    // ── private helpers ──────────────────────────────────────────────

    fn dirs_of(&self, level: SkillLevel) -> Vec<PathBuf> {
        match level {
            SkillLevel::Project => {
                // Skip if project_root is the same as home (avoids double-scan)
                let home = dirs::home_dir().and_then(|h| h.canonicalize().ok());
                let project = self.project_root.canonicalize().ok();
                if home.is_some() && home == project {
                    Vec::new()
                } else {
                    vec![self.project_root.join(COPILOT_CONFIG_DIR).join(SKILLS_DIR)]
                }
            }
            SkillLevel::Custom => self.custom_paths.clone(),
            SkillLevel::Extension => self.extension_paths.clone(),
            SkillLevel::User => self.user_paths.clone(),
            SkillLevel::System => self.system_paths.clone(),
        }
    }

    fn watch_dirs(&self) -> Vec<PathBuf> {
        SkillLevel::all()
            .iter()
            .flat_map(|&level| self.dirs_of(level))
            .collect()
    }
}

// ANOLISA uses the XDG layout on both Linux and macOS, rather than the
// platform-specific data directory returned by dirs::data_dir(). Match its
// install layout by ignoring relative roots and explicit dot segments.
fn user_skill_dirs(home: Option<PathBuf>, data_home: Option<PathBuf>) -> Vec<PathBuf> {
    let data_home = data_home
        .filter(|path| {
            path.is_absolute()
                && !path
                    .to_string_lossy()
                    .split(std::path::MAIN_SEPARATOR)
                    .any(|segment| segment == "." || segment == "..")
        })
        .or_else(|| home.as_ref().map(|home| home.join(".local/share")));
    home.into_iter()
        .map(|home| home.join(COPILOT_CONFIG_DIR).join(SKILLS_DIR))
        .chain(data_home.map(|data| data.join("anolisa/skills")))
        .collect()
}

/// Expand `~`, `${VAR}`, and `$VAR` in a path string.
pub fn expand_path(raw: &str) -> Option<PathBuf> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }

    let expanded = if raw == "~" {
        dirs::home_dir()?
    } else if let Some(rest) = raw.strip_prefix("~/") {
        dirs::home_dir()?.join(rest)
    } else {
        PathBuf::from(raw)
    };

    // Expand ${VAR} and $VAR in each component
    let s = expanded.to_string_lossy().to_string();
    Some(PathBuf::from(expand_env_vars(&s)))
}

fn expand_env_vars(s: &str) -> String {
    let mut result = s.to_string();

    // ${VAR}
    let mut search_from = 0;
    while let Some(pos) = result[search_from..].find("${") {
        let start = search_from + pos;
        if let Some(end) = result[start..].find('}') {
            let var_name = &result[start + 2..start + end];
            match std::env::var(var_name) {
                Ok(val) => {
                    result = format!("{}{}{}", &result[..start], val, &result[start + end + 1..]);
                    search_from = start + val.len();
                }
                Err(_) => {
                    search_from = start + end + 1;
                }
            }
        } else {
            break;
        }
    }

    // $VAR (only when not already handled as ${VAR})
    let mut out = String::new();
    let mut chars = result.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '$'
            && chars
                .peek()
                .is_some_and(|c| c.is_ascii_alphabetic() || *c == '_')
        {
            let mut var = String::new();
            while chars
                .peek()
                .is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_')
            {
                var.push(chars.next().unwrap());
            }
            out.push_str(&std::env::var(&var).unwrap_or_else(|_| format!("${}", var)));
        } else {
            out.push(c);
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn make_skill_file(dir: &Path, skill_name: &str, description: &str) {
        let skills_dir = dir.join(COPILOT_CONFIG_DIR).join(SKILLS_DIR);
        let skill_dir = skills_dir.join(skill_name);
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join(super::super::SKILL_MANIFEST),
            format!(
                "---\nname: {skill_name}\ndescription: {description}\n---\n\nBody of {skill_name}."
            ),
        )
        .unwrap();
    }

    /// Create an isolated manager that only scans project + custom dirs.
    fn isolated_manager(project_root: &Path, custom_paths: Vec<PathBuf>) -> Arc<SkillManager> {
        let empty = tempfile::tempdir().unwrap();
        SkillManager::new_isolated(
            project_root.to_path_buf(),
            custom_paths,
            Some(empty.path().join("nonexistent-user")),
            Some(empty.path().join("nonexistent-system")),
        )
    }

    fn make_flat_skill_file(dir: &Path, skill_name: &str) {
        let skills_dir = dir.join(COPILOT_CONFIG_DIR).join(SKILLS_DIR);
        std::fs::create_dir_all(&skills_dir).unwrap();
        std::fs::write(
            skills_dir.join(format!("{skill_name}.md")),
            format!("---\nname: {skill_name}\ndescription: flat desc\n---\n\nFlat body."),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn priority_override() {
        let project_dir = tempfile::tempdir().unwrap();
        let user_dir = tempfile::tempdir().unwrap();

        // Create a user-level skill
        let user_skills = user_dir.path().join(COPILOT_CONFIG_DIR).join(SKILLS_DIR);
        std::fs::create_dir_all(user_skills.join("shared")).unwrap();
        std::fs::write(
            user_skills.join("shared").join("SKILL.md"),
            "---\nname: shared\ndescription: user version\n---\n\nUser body.",
        )
        .unwrap();

        // Create a project-level skill with the same name
        make_skill_file(project_dir.path(), "shared", "project version");

        let mgr = SkillManager::new_isolated(
            project_dir.path().to_path_buf(),
            vec![],
            Some(user_skills.clone()),
            Some(PathBuf::from("/nonexistent-sys")),
        );
        mgr.refresh().await;

        let all = mgr.list().await;
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "shared");
        assert_eq!(all[0].level, SkillLevel::Project);
        assert_eq!(all[0].description, "project version");
    }

    #[tokio::test]
    async fn custom_paths_loading() {
        let custom_dir = tempfile::tempdir().unwrap();
        let skill_dir = custom_dir.path().join("my-custom-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: my-custom-skill\ndescription: custom\n---\n\nCustom body.",
        )
        .unwrap();

        let project_dir = tempfile::tempdir().unwrap();
        let mgr = isolated_manager(project_dir.path(), vec![custom_dir.path().to_path_buf()]);
        mgr.refresh().await;

        let all = mgr.list().await;
        assert!(all.iter().any(|s| s.name == "my-custom-skill"));
    }

    #[tokio::test]
    async fn system_dir_loading() {
        let sys_dir = tempfile::tempdir().unwrap();
        let skill_dir = sys_dir.path().join("sys-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: sys-skill\ndescription: system\n---\n\nSystem body.",
        )
        .unwrap();

        let project_dir = tempfile::tempdir().unwrap();
        let mgr = SkillManager::new_isolated(
            project_dir.path().to_path_buf(),
            vec![],
            Some(PathBuf::from("/nonexistent-user")),
            Some(sys_dir.path().to_path_buf()),
        );
        mgr.refresh().await;

        let skill = mgr.load("sys-skill").await.unwrap();
        assert_eq!(skill.level, SkillLevel::System);
    }

    #[tokio::test]
    async fn ordered_directories_agree_for_list_load_and_watch() {
        let root = tempfile::tempdir().unwrap();
        let mut mgr = SkillManager::new(
            root.path().join("project"),
            vec![root.path().join("custom-1"), root.path().join("custom-2")],
            vec![
                root.path().join("extension-1"),
                root.path().join("extension-2"),
            ],
        );
        let inner = Arc::get_mut(&mut mgr).unwrap();
        assert_eq!(
            inner.system_paths,
            vec![
                PathBuf::from("/usr/local/share/anolisa/skills"),
                PathBuf::from("/usr/share/anolisa/skills"),
            ]
        );
        inner.user_paths = user_skill_dirs(Some(root.path().join("home")), None);
        assert_eq!(
            inner.user_paths,
            vec![
                root.path().join("home/.copilot-shell/skills"),
                root.path().join("home/.local/share/anolisa/skills"),
            ]
        );
        inner.system_paths = inner
            .system_paths
            .iter()
            .map(|path| root.path().join(path.strip_prefix("/").unwrap()))
            .collect();
        let dirs = mgr.watch_dirs();
        assert_eq!(dirs.len(), 9);
        for (i, dir) in dirs.iter().enumerate() {
            std::fs::create_dir_all(dir).unwrap();
            for name in ["shared".to_string(), format!("only-{i}")] {
                std::fs::write(
                    dir.join(format!("{name}.md")),
                    format!("---\nname: {name}\ndescription: directory {i}\n---\nBody {i}"),
                )
                .unwrap();
            }
        }
        // Removing each winner exposes the next directory, including both
        // raw roots, without hiding skills unique to any lower-priority root.
        for dir in &dirs {
            mgr.refresh().await;
            let listed = mgr.list().await;
            assert_eq!(listed.len(), dirs.len() + 1);
            let listed = listed.iter().find(|skill| skill.name == "shared").unwrap();
            let loaded = mgr.load("shared").await.unwrap();
            assert_eq!(listed.file_path, dir.join("shared.md"));
            assert_eq!(loaded.file_path, listed.file_path);
            assert_eq!(loaded.body, listed.body);
            std::fs::remove_file(dir.join("shared.md")).unwrap();
        }
        // Missing roots are normal when only one install mode is in use.
        std::fs::remove_dir_all(&dirs[0]).unwrap();
        mgr.refresh().await;
        assert!(mgr.load("shared").await.is_none());
        assert_eq!(mgr.list().await.len(), dirs.len() - 1);
    }

    #[tokio::test]
    async fn watcher_triggers_refresh() {
        let project_dir = tempfile::tempdir().unwrap();
        let custom_dir = tempfile::tempdir().unwrap();

        let mut mgr = isolated_manager(project_dir.path(), vec![custom_dir.path().to_path_buf()]);
        let inner = Arc::get_mut(&mut mgr).unwrap();
        inner.user_paths = user_skill_dirs(Some(project_dir.path().join("home")), None);
        inner.system_paths = vec![project_dir.path().join("usr/local/share/anolisa/skills")];
        let dirs = mgr.watch_dirs();
        for dir in &dirs {
            std::fs::create_dir_all(dir).unwrap();
        }
        mgr.refresh().await;
        assert!(mgr.list().await.is_empty());

        mgr.start_watching().await;

        for (i, dir) in dirs.iter().enumerate() {
            let name = format!("new-skill-{i}");
            let new_skill_dir = dir.join(&name);
            std::fs::create_dir_all(&new_skill_dir).unwrap();
            std::fs::write(
                new_skill_dir.join("SKILL.md"),
                format!("---\nname: {name}\ndescription: dynamic\n---\n\nDynamic body."),
            )
            .unwrap();

            let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
            while mgr.load(&name).await.is_none() {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "watcher did not pick up {} within 5s",
                    new_skill_dir.display()
                );
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
            }
        }
    }

    #[test]
    fn expand_path_tilde() {
        let p = expand_path("~/foo/bar").unwrap();
        let home = dirs::home_dir().unwrap();
        assert_eq!(p, home.join("foo/bar"));
    }

    #[test]
    fn expand_path_envvar() {
        std::env::set_var("COSH_TEST_EXPAND", "/custom");
        let p = expand_path("${COSH_TEST_EXPAND}/skills").unwrap();
        assert_eq!(p, PathBuf::from("/custom/skills"));
        std::env::remove_var("COSH_TEST_EXPAND");
    }

    #[test]
    fn expand_path_bare_dollar_var() {
        std::env::set_var("COSH_TEST_BARE", "/bare");
        let p = expand_path("$COSH_TEST_BARE/dir").unwrap();
        assert_eq!(p, PathBuf::from("/bare/dir"));
        std::env::remove_var("COSH_TEST_BARE");
    }

    #[test]
    fn expand_path_empty_returns_none() {
        assert!(expand_path("").is_none());
        assert!(expand_path("  ").is_none());
    }
}
