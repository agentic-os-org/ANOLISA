use std::collections::HashMap;
use std::path::Path;

use tracing::{info, warn};

use crate::parser;
use crate::{CategoryMeta, ParseConfig, ParseStatus, SkillEntry};

// ---------------------------------------------------------------------------
// LoadError
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct LoadError {
    pub path: std::path::PathBuf,
    pub error: String,
}

/// Reported for a Skill or category directory whose name cannot be
/// represented as UTF-8.
///
/// Such a name is never a skill name — the canonical resolver rejects the
/// path component (`invalid_canonical_path`) — so the loaders report the
/// directory instead of surfacing it under a fallback name. Directories that
/// are neither a Skill nor a category stay ignored whatever their name.
const NON_UTF8_NAME_ERROR: &str = "directory name is not valid UTF-8";

// ---------------------------------------------------------------------------
// SkillStore
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct SkillStore {
    skills: HashMap<String, SkillEntry>,
    /// Category name → category metadata (from `_category.yaml`)
    pub categories: HashMap<String, CategoryMeta>,
    /// Skill name → category name (empty string = uncategorized)
    skill_categories: HashMap<String, String>,
}

impl SkillStore {
    /// Create a new empty store.
    pub fn new() -> Self {
        Self {
            skills: HashMap::new(),
            categories: HashMap::new(),
            skill_categories: HashMap::new(),
        }
    }

    /// Load all skills from a source directory (initial scan).
    ///
    /// Supports both flat and categorized layouts:
    /// - **Flat**: `{source}/{skill_name}/SKILL.md`
    /// - **Categorized**: `{source}/{category}/{skill_name}/SKILL.md`
    ///
    /// A subdirectory is treated as a **category** when it contains no
    /// `SKILL.md` of its own but has sub-subdirectories that contain
    /// `SKILL.md` files.
    ///
    /// Skills are keyed by directory leaf name, so two discovered skills
    /// sharing a leaf name collide: one entry is kept deterministically
    /// and the collision is returned as a `LoadError` (reported by the
    /// internal insert_discovered helper used by both loaders).
    pub fn load_from_directory(&mut self, source: &Path, config: &ParseConfig) -> Vec<LoadError> {
        let mut errors = Vec::new();
        let mut loaded_count = 0usize;

        let entries = match std::fs::read_dir(source) {
            Ok(e) => e,
            Err(e) => {
                errors.push(LoadError {
                    path: source.to_path_buf(),
                    error: format!("cannot read directory: {e}"),
                });
                return errors;
            }
        };

        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    warn!("failed to read dir entry: {e}");
                    continue;
                }
            };

            let path = entry.path();

            // Skip non-directories. No-follow entry type so a symlink to a
            // directory is skipped: a symlinked Skill/category is not managed
            // (the resolver rejects symlinked components with O_NOFOLLOW).
            if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }

            // Skip hidden directories, whatever the encoding of the name.
            if is_hidden(&path) {
                continue;
            }
            // A name that is not valid UTF-8 is never a skill name: the
            // canonical resolver rejects such a path component
            // (`invalid_canonical_path`). It is only reported once the
            // directory turns out to be a Skill or a category.
            let name = path.file_name().and_then(|n| n.to_str());

            if is_category_dir(&path) {
                // ---- Categorized layout ----
                // The max_skills limit is enforced per nested skill by
                // `load_skills_from_category`, so the category itself is
                // never charged a slot or an error of its own.
                let Some(name) = name else {
                    errors.push(non_utf8_name_error(&path));
                    continue;
                };
                let cat_name = name.to_string();

                // Try to load _category.yaml
                let cat_meta = load_category_meta(&path, &cat_name);
                self.categories.insert(cat_name.clone(), cat_meta);

                // Load skills inside this category directory
                let cat_errors =
                    self.load_skills_from_category(&path, &cat_name, config, &mut loaded_count);
                errors.extend(cat_errors);
            } else {
                // ---- Flat layout ----
                // Classify before enforcing the limit: a directory that is
                // not a skill is skipped silently whether or not the limit
                // has been reached, exactly as below the limit.
                if !has_regular_skill_md(&path) {
                    continue;
                }
                if loaded_count >= config.max_skills {
                    errors.push(LoadError {
                        path: path.clone(),
                        error: format!("max skills limit reached ({})", config.max_skills),
                    });
                    continue;
                }
                let Some(name) = name else {
                    errors.push(non_utf8_name_error(&path));
                    continue;
                };
                let skill_md = path.join("SKILL.md");

                match parser::parse_skill_file_with_limit(&skill_md, config.max_skill_size) {
                    Ok(mut entry) => {
                        let dir_name = name.to_string();
                        adopt_directory_name(&mut entry, &dir_name);
                        info!(name = %dir_name, "loaded skill");
                        if let Some(error) = self.insert_discovered(entry, "") {
                            errors.push(error);
                        }
                        loaded_count += 1;
                    }
                    Err(e) => {
                        errors.push(LoadError {
                            path: skill_md,
                            error: e.to_string(),
                        });
                    }
                }
            }
        }

        info!(count = loaded_count, "finished loading skills");
        errors
    }

    /// Insert a discovered skill, reporting a same-key collision.
    ///
    /// The store keys skills by their directory leaf name — the key every
    /// later lookup uses (`get`, `/skills` listings, the sync worker's
    /// `Reparse` events) — so two valid skills that share a leaf name
    /// (`alpha/notes` + `beta/notes`, or a flat `demo` plus a categorized
    /// `catalog/demo`) collide on one entry. Instead of letting the later
    /// `read_dir` winner silently overwrite the other, keep one entry
    /// deterministically (the lexicographically smaller `source_path`
    /// wins, mirroring the earlier-source-wins rule of the multi-source
    /// loader) and return a `LoadError` describing the loser, so mount
    /// logs and `sls validate` surface the collision. Re-discovering the
    /// exact same path is an idempotent refresh and never collides.
    fn insert_discovered(&mut self, entry: SkillEntry, category: &str) -> Option<LoadError> {
        let name = entry.metadata.name.clone();
        match self.skills.get(&name) {
            Some(existing) if existing.source_path != entry.source_path => {
                let (keep_new, dropped, kept) = if entry.source_path < existing.source_path {
                    (
                        true,
                        existing.source_path.clone(),
                        entry.source_path.clone(),
                    )
                } else {
                    (
                        false,
                        entry.source_path.clone(),
                        existing.source_path.clone(),
                    )
                };
                if keep_new {
                    self.skills.insert(name.clone(), entry);
                    self.skill_categories
                        .insert(name.clone(), category.to_string());
                }
                Some(LoadError {
                    path: dropped.clone(),
                    error: format!(
                        "duplicate skill name '{name}': {} and {} share the same \
                         directory name; keeping {}",
                        dropped.display(),
                        kept.display(),
                        kept.display(),
                    ),
                })
            }
            _ => {
                self.skills.insert(name.clone(), entry);
                self.skill_categories.insert(name, category.to_string());
                None
            }
        }
    }

    /// Load skills from a single category directory.
    fn load_skills_from_category(
        &mut self,
        cat_path: &Path,
        cat_name: &str,
        config: &ParseConfig,
        loaded_count: &mut usize,
    ) -> Vec<LoadError> {
        let mut errors = Vec::new();

        let entries = match std::fs::read_dir(cat_path) {
            Ok(e) => e,
            Err(e) => {
                errors.push(LoadError {
                    path: cat_path.to_path_buf(),
                    error: format!("cannot read category directory: {e}"),
                });
                return errors;
            }
        };

        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    warn!("failed to read dir entry in category {cat_name}: {e}");
                    continue;
                }
            };

            let path = entry.path();
            // No-follow entry type: a symlinked child is never a managed
            // nested Skill (resolver rejects symlinked components).
            if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }

            if is_hidden(&path) {
                continue;
            }

            // Classify before enforcing the limit: a category child that is
            // not a skill is skipped silently whether or not the limit has
            // been reached, exactly as below the limit.
            if !has_regular_skill_md(&path) {
                continue;
            }
            if *loaded_count >= config.max_skills {
                errors.push(LoadError {
                    path: path.clone(),
                    error: format!("max skills limit reached ({})", config.max_skills),
                });
                continue;
            }
            // Same rule as the top-level loader: a Skill whose name is not
            // valid UTF-8 is reported, not loaded under a fallback name.
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                errors.push(non_utf8_name_error(&path));
                continue;
            };
            let skill_md = path.join("SKILL.md");

            match parser::parse_skill_file_with_limit(&skill_md, config.max_skill_size) {
                Ok(mut entry) => {
                    let dir_name = name.to_string();
                    adopt_directory_name(&mut entry, &dir_name);
                    info!(name = %dir_name, category = %cat_name, "loaded skill");
                    if let Some(error) = self.insert_discovered(entry, cat_name) {
                        errors.push(error);
                    }
                    *loaded_count += 1;
                }
                Err(e) => {
                    errors.push(LoadError {
                        path: skill_md,
                        error: e.to_string(),
                    });
                }
            }
        }

        errors
    }

    /// Insert or update a skill entry.
    pub fn upsert(&mut self, entry: SkillEntry) {
        self.skills.insert(entry.metadata.name.clone(), entry);
    }

    /// Remove a skill by name.
    pub fn remove(&mut self, name: &str) -> Option<SkillEntry> {
        self.skill_categories.remove(name);
        self.skills.remove(name)
    }

    /// Get a skill by name.
    pub fn get(&self, name: &str) -> Option<&SkillEntry> {
        self.skills.get(name)
    }

    /// Iterate over all skills.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &SkillEntry)> {
        self.skills.iter()
    }

    /// List all skill names (sorted alphabetically).
    pub fn list(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.skills.keys().map(|s| s.as_str()).collect();
        names.sort_unstable();
        names
    }

    /// Get the number of skills.
    pub fn len(&self) -> usize {
        self.skills.len()
    }

    /// Check if store is empty.
    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }

    /// Split store skills into (primary, secondary) based on a primary list.
    ///
    /// - `primary_list = None` -> (all_skills, empty), no filtering.
    /// - `primary_list = Some(list)` -> skills in list become primary (filtered
    ///   to those present in store); all others become secondary. Duplicate
    ///   names in the list collapse to the first occurrence, matching the set
    ///   semantics the secondary computation already applies — a view listing
    ///   the same skill twice must not list it twice in `/skills`.
    pub fn split_primary(&self, primary_list: Option<&[String]>) -> (Vec<String>, Vec<String>) {
        match primary_list {
            None => {
                let all = self.list().iter().map(|s| s.to_string()).collect();
                (all, Vec::new())
            }
            Some(list) => {
                let mut seen = std::collections::HashSet::new();
                let primary: Vec<String> = list
                    .iter()
                    .filter(|name| self.skills.contains_key(name.as_str()))
                    .filter(|name| seen.insert(name.as_str()))
                    .cloned()
                    .collect();
                let primary_set: std::collections::HashSet<&str> =
                    primary.iter().map(|s| s.as_str()).collect();
                let secondary: Vec<String> = self
                    .list()
                    .iter()
                    .filter(|name| !primary_set.contains(*name))
                    .map(|s| s.to_string())
                    .collect();
                (primary, secondary)
            }
        }
    }
}

impl Default for SkillStore {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Return `true` when `dir` is itself a **real directory** (not a symlink)
/// and contains a `SKILL.md` that is a **regular file**, both classified
/// **without following symlinks**.
///
/// This is the single, shared definition of "this directory is a Skill"
/// used by store discovery, the FUSE readdir/read gating, Hermes activation
/// enumeration, and the control-socket resolver. Keeping one predicate
/// prevents the layers from disagreeing about what a Skill is:
///
/// * `dir` must be a real directory. A symlinked Skill directory
///   (`<root>/linked-skill -> /outside`) is **not** a managed Skill: the
///   resolver descends with `openat(O_NOFOLLOW)` and rejects any symlinked
///   component, so store/FUSE discovery must reject it too rather than load,
///   expose, and read a Skill the resolver refuses to resolve.
/// * A `SKILL.md` that is a symlink, directory, FIFO, or any other
///   non-regular object is **not** a valid marker — it is treated as absent
///   (the directory is not a Skill), never followed.
/// * A regular-file `SKILL.md` is a marker even when it is unreadable
///   (mode `000`): discovery must not depend on read permission.
///
/// This matches the resolver's `openat(O_NOFOLLOW)` + `fstatat`
/// classification (real directory with a regular-file marker → Skill;
/// anything else → not a Skill).
pub fn has_regular_skill_md(dir: &Path) -> bool {
    // The directory itself must be a real directory, checked no-follow so a
    // symlink is rejected even when its target is a directory.
    match std::fs::symlink_metadata(dir) {
        Ok(meta) if meta.file_type().is_dir() => {}
        _ => return false,
    }
    match std::fs::symlink_metadata(dir.join("SKILL.md")) {
        Ok(meta) => meta.file_type().is_file(),
        Err(_) => false,
    }
}

/// Returns `true` when the last component of `path` starts with `.`, checked
/// on the raw bytes so a hidden name that is not valid UTF-8 is hidden too.
fn is_hidden(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|n| n.as_encoded_bytes().starts_with(b"."))
}

fn non_utf8_name_error(path: &Path) -> LoadError {
    LoadError {
        path: path.to_path_buf(),
        error: NON_UTF8_NAME_ERROR.to_string(),
    }
}

/// Returns `true` when `dir` looks like a category container:
/// it has no `SKILL.md` of its own but contains at least one **real
/// sub-directory** (not a symlink) that does have a `SKILL.md`.
fn is_category_dir(dir: &Path) -> bool {
    if has_regular_skill_md(dir) {
        return false;
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            // No-follow entry type: a symlinked child is never a Skill
            // directory, so it never makes `dir` a category.
            let is_real_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if is_real_dir && has_regular_skill_md(&entry.path()) {
                return true;
            }
        }
    }
    false
}

/// The store exposes every skill under its directory name; that adopted
/// name must obey the same grammar the parser enforces for the
/// frontmatter `name` (kebab-case, max 64 chars). When it does not, the
/// entry is degraded — not skipped — so `skillfs list`/validate surfaces
/// the problem instead of presenting a non-conforming name as a cleanly
/// parsed skill.
fn degrade_on_invalid_dir_name(entry: &mut SkillEntry, dir_name: &str) {
    let mut issues = Vec::new();
    parser::validate_name(dir_name, &mut issues);
    if issues.is_empty() {
        return;
    }
    let issue = format!("skill directory name `{dir_name}`: {}", issues.join("; "));
    entry.parse_status = match entry.parse_status.clone() {
        ParseStatus::Ok => ParseStatus::Degraded(issue),
        ParseStatus::Degraded(existing) => ParseStatus::Degraded(format!("{existing}; {issue}")),
        error @ ParseStatus::Error(_) => error,
    };
}

/// Adopt a skill directory's name as the entry's authoritative identity
/// and merge the name-grammar validation state into the entry.
///
/// The directory name is the store key regardless of what the frontmatter
/// `name:` field says, so every producer that adopts a directory name —
/// the store loaders, the FUSE sync worker re-parsing after a write, and
/// the rename path re-parsing under a new directory — must go through
/// this single entry point. Applying the adoption anywhere else would let
/// a runtime re-parse overwrite a `Degraded` entry from the initial scan
/// with a clean one for the same non-conforming directory.
pub fn adopt_directory_name(entry: &mut SkillEntry, dir_name: &str) {
    entry.metadata.name = dir_name.to_string();
    degrade_on_invalid_dir_name(entry, dir_name);
}

/// Load `_category.yaml` from `dir` if present; fall back to a default meta
/// with `name = cat_name`.
fn load_category_meta(dir: &Path, cat_name: &str) -> CategoryMeta {
    let yaml_path = dir.join("_category.yaml");
    if yaml_path.exists() {
        if let Ok(content) = std::fs::read_to_string(&yaml_path) {
            if let Ok(meta) = serde_yaml::from_str::<CategoryMeta>(&content) {
                return meta;
            }
        }
    }
    CategoryMeta {
        name: cat_name.to_string(),
        description: String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ParseStatus, SkillMetadata};
    use std::time::SystemTime;

    // Helper to create a test skill entry
    fn create_test_entry(name: &str, description: &str, tags: Vec<&str>) -> SkillEntry {
        SkillEntry {
            metadata: SkillMetadata {
                name: name.to_string(),
                description: description.to_string(),
                version: "1.0.0".to_string(),
                tags: tags.into_iter().map(|s| s.to_string()).collect(),
                enabled: true,
                requires: None,
            },
            parameters: Vec::new(),
            returns: Vec::new(),
            body: String::new(),
            parse_status: ParseStatus::Ok,
            source_path: std::path::PathBuf::new(),
            last_modified: SystemTime::UNIX_EPOCH,
        }
    }

    // -----------------------------------------------------------------------
    // Basic CRUD Tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_new_store_is_empty() {
        let store = SkillStore::new();
        assert!(store.is_empty());
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn test_upsert_new() {
        let mut store = SkillStore::new();
        let entry = create_test_entry("test-skill", "A test skill", vec!["test"]);

        store.upsert(entry);

        assert_eq!(store.len(), 1);
        assert!(!store.is_empty());
    }

    #[test]
    fn test_upsert_existing() {
        let mut store = SkillStore::new();
        let entry1 = create_test_entry("test-skill", "Original description", vec!["test"]);
        store.upsert(entry1);

        let entry2 =
            create_test_entry("test-skill", "Updated description", vec!["test", "updated"]);
        store.upsert(entry2);

        assert_eq!(store.len(), 1);
        let retrieved = store.get("test-skill").unwrap();
        assert_eq!(retrieved.metadata.description, "Updated description");
    }

    #[test]
    fn test_remove_existing() {
        let mut store = SkillStore::new();
        let entry = create_test_entry("test-skill", "A test skill", vec![]);
        store.upsert(entry);

        let removed = store.remove("test-skill");

        assert!(removed.is_some());
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn test_remove_nonexistent() {
        let mut store = SkillStore::new();

        let removed = store.remove("nonexistent");

        assert!(removed.is_none());
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn test_get_existing() {
        let mut store = SkillStore::new();
        let entry = create_test_entry("test-skill", "A test skill", vec![]);
        store.upsert(entry);

        let retrieved = store.get("test-skill");

        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().metadata.name, "test-skill");
    }

    #[test]
    fn test_get_nonexistent() {
        let store = SkillStore::new();

        let retrieved = store.get("nonexistent");

        assert!(retrieved.is_none());
    }

    #[test]
    fn test_list_names() {
        let mut store = SkillStore::new();
        store.upsert(create_test_entry("zebra", "Zebra skill", vec![]));
        store.upsert(create_test_entry("alpha", "Alpha skill", vec![]));
        store.upsert(create_test_entry("beta", "Beta skill", vec![]));

        let names = store.list();

        assert_eq!(names.len(), 3);
        assert_eq!(names[0], "alpha");
        assert_eq!(names[1], "beta");
        assert_eq!(names[2], "zebra");
    }

    // -----------------------------------------------------------------------
    // Manifest Tests
    // -----------------------------------------------------------------------

    // -----------------------------------------------------------------------
    // Load from Directory Tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_load_from_directory_empty() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let mut store = SkillStore::new();
        let config = ParseConfig {
            strict: false,
            max_skill_size: 1_048_576,
            max_skills: 1000,
        };

        let errors = store.load_from_directory(temp_dir.path(), &config);

        assert!(errors.is_empty());
        assert!(store.is_empty());
    }

    #[test]
    fn test_load_from_directory_ignores_hidden() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let hidden_dir = temp_dir.path().join(".hidden");
        std::fs::create_dir(&hidden_dir).unwrap();
        std::fs::write(hidden_dir.join("SKILL.md"), "---\nname: hidden\n---\n").unwrap();

        let mut store = SkillStore::new();
        let config = ParseConfig {
            strict: false,
            max_skill_size: 1_048_576,
            max_skills: 1000,
        };

        let errors = store.load_from_directory(temp_dir.path(), &config);

        assert!(errors.is_empty());
        assert!(store.is_empty()); // hidden dir should be ignored
    }

    #[test]
    fn test_load_from_directory_ignores_files() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join("not-a-dir.txt"), "not a skill").unwrap();

        let mut store = SkillStore::new();
        let config = ParseConfig {
            strict: false,
            max_skill_size: 1_048_576,
            max_skills: 1000,
        };

        let errors = store.load_from_directory(temp_dir.path(), &config);

        assert!(errors.is_empty());
        assert!(store.is_empty());
    }

    /// Build a valid skill directory under `parent` whose directory name is
    /// not valid UTF-8, e.g. a Latin-1 name coming out of an archive.
    #[cfg(unix)]
    fn create_non_utf8_skill_dir(parent: &Path) -> std::path::PathBuf {
        use std::os::unix::ffi::OsStringExt;

        let dir = parent.join(std::ffi::OsString::from_vec(b"caf\xe9".to_vec()));
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), "---\nname: caf\n---\nbody\n").unwrap();
        dir
    }

    /// A directory whose name cannot be represented as UTF-8 must not be
    /// surfaced as a skill: the canonical resolver rejects such a path
    /// component (`invalid_canonical_path`), so the store must not invent a
    /// skill named `unknown` for it.
    #[test]
    #[cfg(unix)]
    fn test_load_from_directory_skips_non_utf8_skill_dir() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        create_non_utf8_skill_dir(temp_dir.path());

        let mut store = SkillStore::new();
        let config = ParseConfig {
            strict: false,
            max_skill_size: 1_048_576,
            max_skills: 1000,
        };

        let errors = store.load_from_directory(temp_dir.path(), &config);

        assert!(
            store.is_empty(),
            "non-UTF-8 directory names must not surface as skills, got {:?}",
            store.list()
        );
        assert!(
            errors.iter().any(|e| e.error.contains("UTF-8")),
            "the skipped directory should be reported, got {errors:?}"
        );
    }

    /// Two non-UTF-8 skill directories must not collapse into one skill entry
    /// sharing the fallback name.
    #[test]
    #[cfg(unix)]
    fn test_load_from_directory_does_not_merge_non_utf8_skill_dirs() {
        use std::os::unix::ffi::OsStringExt;

        let temp_dir = tempfile::TempDir::new().unwrap();
        for name in [b"caf\xe9".to_vec(), b"na\xefve".to_vec()] {
            let dir = temp_dir.path().join(std::ffi::OsString::from_vec(name));
            std::fs::create_dir(&dir).unwrap();
            std::fs::write(dir.join("SKILL.md"), "---\nname: x\n---\nbody\n").unwrap();
        }

        let mut store = SkillStore::new();
        let config = ParseConfig {
            strict: false,
            max_skill_size: 1_048_576,
            max_skills: 1000,
        };

        store.load_from_directory(temp_dir.path(), &config);

        assert_eq!(
            store.len(),
            0,
            "neither of the two non-UTF-8 directories is a loadable skill"
        );
    }

    /// A non-UTF-8 child directory inside a category layout is skipped the
    /// same way, and a non-UTF-8 category name must not attribute its skills
    /// to a fallback category.
    #[test]
    #[cfg(unix)]
    fn test_load_skills_from_category_skips_non_utf8_names() {
        use std::os::unix::ffi::OsStringExt;

        let temp_dir = tempfile::TempDir::new().unwrap();
        let category = temp_dir.path().join("tools");
        std::fs::create_dir(&category).unwrap();

        // A well-formed skill and a non-UTF-8 sibling in the same category.
        let good = category.join("alpha");
        std::fs::create_dir(&good).unwrap();
        std::fs::write(good.join("SKILL.md"), "---\nname: alpha\n---\nbody\n").unwrap();
        create_non_utf8_skill_dir(&category);

        // A second category whose own name is not UTF-8.
        let weird_category = temp_dir
            .path()
            .join(std::ffi::OsString::from_vec(b"cat\xe9".to_vec()));
        std::fs::create_dir(&weird_category).unwrap();
        let nested = weird_category.join("beta");
        std::fs::create_dir(&nested).unwrap();
        std::fs::write(nested.join("SKILL.md"), "---\nname: beta\n---\nbody\n").unwrap();

        let mut store = SkillStore::new();
        let config = ParseConfig {
            strict: false,
            max_skill_size: 1_048_576,
            max_skills: 1000,
        };

        let errors = store.load_from_directory(temp_dir.path(), &config);

        assert_eq!(
            store.list(),
            vec!["alpha"],
            "only the UTF-8 named skill may load"
        );
        assert!(
            errors.iter().any(|e| e.error.contains("UTF-8")),
            "both skipped directories should be reported, got {errors:?}"
        );
    }

    /// Only a Skill or category directory is reported for its name: a
    /// non-UTF-8 directory without `SKILL.md`, or a hidden one, stays ignored
    /// like any other unrelated or hidden directory, at the top level and
    /// inside a category.
    #[test]
    #[cfg(unix)]
    fn test_load_from_directory_ignores_non_utf8_non_skill_dirs() {
        use std::os::unix::ffi::OsStringExt;

        let raw = |bytes: &[u8]| std::ffi::OsString::from_vec(bytes.to_vec());
        let temp_dir = tempfile::TempDir::new().unwrap();
        let root = temp_dir.path();

        let good = root.join("good");
        std::fs::create_dir(&good).unwrap();
        std::fs::write(good.join("SKILL.md"), "---\nname: good\n---\nbody\n").unwrap();
        std::fs::create_dir(root.join(raw(b"unrelated-\xff"))).unwrap();
        let hidden = root.join(raw(b".hidden-\xff"));
        std::fs::create_dir(&hidden).unwrap();
        std::fs::write(hidden.join("SKILL.md"), "---\nname: h\n---\nbody\n").unwrap();

        let category = root.join("tools");
        let alpha = category.join("alpha");
        std::fs::create_dir_all(&alpha).unwrap();
        std::fs::write(alpha.join("SKILL.md"), "---\nname: alpha\n---\nbody\n").unwrap();
        std::fs::create_dir(category.join(raw(b"empty-\xff"))).unwrap();
        let hidden_child = category.join(raw(b".\xff"));
        std::fs::create_dir(&hidden_child).unwrap();
        std::fs::write(hidden_child.join("SKILL.md"), "---\nname: h\n---\nbody\n").unwrap();

        let mut store = SkillStore::new();
        let config = ParseConfig {
            strict: false,
            max_skill_size: 1_048_576,
            max_skills: 1000,
        };

        let errors = store.load_from_directory(root, &config);

        assert!(errors.is_empty(), "nothing to report, got {errors:?}");
        assert_eq!(store.list(), vec!["alpha", "good"]);
    }

    #[test]
    fn test_load_from_directory_no_skill_md() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let skill_dir = temp_dir.path().join("empty-skill");
        std::fs::create_dir(&skill_dir).unwrap();
        // No SKILL.md file

        let mut store = SkillStore::new();
        let config = ParseConfig {
            strict: false,
            max_skill_size: 1_048_576,
            max_skills: 1000,
        };

        let errors = store.load_from_directory(temp_dir.path(), &config);

        assert!(errors.is_empty());
        assert!(store.is_empty());
    }

    // -----------------------------------------------------------------------
    // has_regular_skill_md predicate boundaries (shared Skill-marker rule)
    // -----------------------------------------------------------------------

    #[test]
    fn has_regular_skill_md_true_for_regular_file() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("SKILL.md"), "---\nname: x\n---\n").unwrap();
        assert!(has_regular_skill_md(dir.path()));
    }

    #[test]
    fn has_regular_skill_md_true_for_unreadable_mode_000_regular_file() {
        // Discovery must not depend on read permission: a mode-000 regular
        // SKILL.md is still a marker (stat succeeds without read access).
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().unwrap();
        let md = dir.path().join("SKILL.md");
        std::fs::write(&md, "---\nname: x\n---\n").unwrap();
        std::fs::set_permissions(&md, std::fs::Permissions::from_mode(0o000)).unwrap();
        let present = has_regular_skill_md(dir.path());
        // Restore perms so the tempdir cleans up.
        std::fs::set_permissions(&md, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(present, "mode-000 regular SKILL.md must be seen as present");
    }

    #[test]
    fn has_regular_skill_md_false_for_symlink() {
        // A symlink named SKILL.md is not a marker and is never followed.
        let dir = tempfile::TempDir::new().unwrap();
        let target = dir.path().join("real.md");
        std::fs::write(&target, "---\nname: x\n---\n").unwrap();
        std::os::unix::fs::symlink(&target, dir.path().join("SKILL.md")).unwrap();
        assert!(!has_regular_skill_md(dir.path()));
    }

    #[test]
    fn has_regular_skill_md_false_for_directory() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join("SKILL.md")).unwrap();
        assert!(!has_regular_skill_md(dir.path()));
    }

    #[test]
    fn has_regular_skill_md_false_when_absent() {
        let dir = tempfile::TempDir::new().unwrap();
        assert!(!has_regular_skill_md(dir.path()));
    }

    #[test]
    fn store_hermes_symlinked_top_level_skill_md_is_category_not_skill() {
        // A top-level dir whose only SKILL.md is a symlink is a category:
        // store must NOT load it as a flat skill, and its real nested child
        // (regular SKILL.md) IS loaded as `category/child`. This is the same
        // classification the resolver and FUSE layer now apply.
        let temp_dir = tempfile::TempDir::new().unwrap();
        let top = temp_dir.path().join("top");
        std::fs::create_dir(&top).unwrap();
        let real = temp_dir.path().join("real.md");
        std::fs::write(&real, "---\nname: r\n---\n").unwrap();
        std::os::unix::fs::symlink(&real, top.join("SKILL.md")).unwrap();
        let child = top.join("child");
        std::fs::create_dir(&child).unwrap();
        std::fs::write(child.join("SKILL.md"), "---\nname: child\n---\n").unwrap();

        let mut store = SkillStore::new();
        let config = ParseConfig {
            strict: false,
            max_skill_size: 1_048_576,
            max_skills: 1000,
        };
        let errors = store.load_from_directory(temp_dir.path(), &config);
        assert!(errors.is_empty(), "unexpected load errors: {errors:?}");

        // `top` is a category, not a flat skill; `child` is the nested skill.
        assert!(
            store.get("top").is_none(),
            "symlink-marker top must not load"
        );
        assert!(store.get("child").is_some(), "nested child must load");
    }

    #[test]
    fn has_regular_skill_md_false_for_symlinked_directory() {
        // `dir` itself is a symlink pointing at a real Skill directory. It is
        // NOT a managed Skill: the resolver's O_NOFOLLOW descent rejects a
        // symlinked component, so the predicate must reject it too.
        let root = tempfile::TempDir::new().unwrap();
        let real = root.path().join("real-skill");
        std::fs::create_dir(&real).unwrap();
        std::fs::write(real.join("SKILL.md"), "---\nname: x\n---\n").unwrap();
        let link = root.path().join("linked-skill");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(has_regular_skill_md(&real), "the real dir is a Skill");
        assert!(
            !has_regular_skill_md(&link),
            "a symlinked Skill directory must not be classified as a Skill"
        );
    }

    #[test]
    fn store_flat_symlinked_skill_dir_not_loaded() {
        // Flat layout: `<root>/linked-skill -> <outside>/real-skill`. The
        // store must NOT load the symlinked directory as a Skill (the
        // resolver rejects it), while a sibling real Skill loads normally.
        let temp_dir = tempfile::TempDir::new().unwrap();
        let outside = tempfile::TempDir::new().unwrap();
        let real = outside.path().join("real-skill");
        std::fs::create_dir(&real).unwrap();
        std::fs::write(real.join("SKILL.md"), "---\nname: real\n---\n").unwrap();
        std::os::unix::fs::symlink(&real, temp_dir.path().join("linked-skill")).unwrap();
        // A genuine in-root Skill to prove loading still works.
        let inroot = temp_dir.path().join("inroot");
        std::fs::create_dir(&inroot).unwrap();
        std::fs::write(inroot.join("SKILL.md"), "---\nname: inroot\n---\n").unwrap();

        let mut store = SkillStore::new();
        let config = ParseConfig {
            strict: false,
            max_skill_size: 1_048_576,
            max_skills: 1000,
        };
        let errors = store.load_from_directory(temp_dir.path(), &config);
        assert!(errors.is_empty(), "unexpected load errors: {errors:?}");
        assert!(
            store.get("linked-skill").is_none(),
            "symlinked Skill directory must not be loaded"
        );
        assert!(store.get("inroot").is_some(), "real Skill must load");
    }

    #[test]
    fn store_hermes_symlink_category_not_descended() {
        // A symlinked category (`<root>/linkcat -> <outside>/cat`) must not be
        // descended: its nested skills live outside the managed root and the
        // resolver rejects the symlinked component. Skills under it must NOT
        // load.
        let temp_dir = tempfile::TempDir::new().unwrap();
        let outside = tempfile::TempDir::new().unwrap();
        let cat = outside.path().join("cat");
        let nested = cat.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("SKILL.md"), "---\nname: nested\n---\n").unwrap();
        std::os::unix::fs::symlink(&cat, temp_dir.path().join("linkcat")).unwrap();

        let mut store = SkillStore::new();
        let config = ParseConfig {
            strict: false,
            max_skill_size: 1_048_576,
            max_skills: 1000,
        };
        let errors = store.load_from_directory(temp_dir.path(), &config);
        assert!(errors.is_empty(), "unexpected load errors: {errors:?}");
        assert!(
            store.get("nested").is_none(),
            "skill under a symlinked category must not load"
        );
    }

    #[test]
    fn store_hermes_symlink_nested_skill_not_loaded() {
        // Real category, but the nested skill entry is a symlink to a real
        // Skill directory. The symlinked nested skill must NOT load.
        let temp_dir = tempfile::TempDir::new().unwrap();
        let outside = tempfile::TempDir::new().unwrap();
        let real = outside.path().join("real-nested");
        std::fs::create_dir(&real).unwrap();
        std::fs::write(real.join("SKILL.md"), "---\nname: linked\n---\n").unwrap();

        let cat = temp_dir.path().join("cat");
        std::fs::create_dir(&cat).unwrap();
        // A genuine nested skill so `cat` is recognized as a category.
        let realnested = cat.join("realnested");
        std::fs::create_dir(&realnested).unwrap();
        std::fs::write(realnested.join("SKILL.md"), "---\nname: realnested\n---\n").unwrap();
        // The symlinked nested skill.
        std::os::unix::fs::symlink(&real, cat.join("linknested")).unwrap();

        let mut store = SkillStore::new();
        let config = ParseConfig {
            strict: false,
            max_skill_size: 1_048_576,
            max_skills: 1000,
        };
        let errors = store.load_from_directory(temp_dir.path(), &config);
        assert!(errors.is_empty(), "unexpected load errors: {errors:?}");
        assert!(
            store.get("realnested").is_some(),
            "real nested skill must load"
        );
        assert!(
            store.get("linknested").is_none(),
            "symlinked nested skill must not load"
        );
    }

    // -----------------------------------------------------------------------
    // Same-leaf-name collision tests
    // -----------------------------------------------------------------------

    #[test]
    fn store_load_reports_cross_category_leaf_name_collision() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        for cat in ["alpha", "beta"] {
            let dir = temp_dir.path().join(cat).join("notes");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("SKILL.md"),
                format!("---\nname: notes\ndescription: {cat}\n---\n"),
            )
            .unwrap();
        }

        let mut store = SkillStore::new();
        let config = ParseConfig {
            strict: false,
            max_skill_size: 1_048_576,
            max_skills: 1000,
        };
        let errors = store.load_from_directory(temp_dir.path(), &config);

        // Exactly one entry survives the shared key, and the collision is
        // reported instead of silently dropping the loser.
        assert_eq!(store.len(), 1);
        assert_eq!(errors.len(), 1, "collision must surface, got {errors:?}");
        // The deterministic winner is the lexicographically smaller path,
        // independent of read_dir order.
        let winner = temp_dir.path().join("alpha").join("notes").join("SKILL.md");
        let loser = temp_dir.path().join("beta").join("notes").join("SKILL.md");
        assert_eq!(store.get("notes").unwrap().source_path, winner);
        assert_eq!(errors[0].path, loser);
        assert!(errors[0].error.contains("notes"), "{}", errors[0].error);

        // A rescan onto the same store keeps the same deterministic winner
        // (never flip-flopping with read_dir order): re-discovering the
        // winner's path is a silent refresh, and the still-existing loser
        // is reported again on every scan while the collision persists.
        let errors_again = store.load_from_directory(temp_dir.path(), &config);
        assert_eq!(errors_again.len(), 1, "reload: {errors_again:?}");
        assert_eq!(errors_again[0].path, loser);
        assert_eq!(store.len(), 1);
        assert_eq!(store.get("notes").unwrap().source_path, winner);
    }

    #[test]
    fn store_load_reports_flat_and_categorized_leaf_name_collision() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let flat = temp_dir.path().join("demo");
        std::fs::create_dir(&flat).unwrap();
        std::fs::write(
            flat.join("SKILL.md"),
            "---\nname: demo\ndescription: flat\n---\n",
        )
        .unwrap();
        let nested = temp_dir.path().join("catalog").join("demo");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(
            nested.join("SKILL.md"),
            "---\nname: demo\ndescription: categorized\n---\n",
        )
        .unwrap();

        let mut store = SkillStore::new();
        let config = ParseConfig {
            strict: false,
            max_skill_size: 1_048_576,
            max_skills: 1000,
        };
        let errors = store.load_from_directory(temp_dir.path(), &config);

        assert_eq!(store.len(), 1);
        assert_eq!(errors.len(), 1, "collision must surface, got {errors:?}");
        // `catalog/demo/SKILL.md` sorts before `demo/SKILL.md`.
        assert_eq!(
            store.get("demo").unwrap().source_path,
            nested.join("SKILL.md")
        );
        assert_eq!(errors[0].path, flat.join("SKILL.md"));
    }

    #[test]
    fn store_load_distinct_leaf_names_across_categories_both_load() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        for (cat, skill) in [("alpha", "notes"), ("beta", "journal")] {
            let dir = temp_dir.path().join(cat).join(skill);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("SKILL.md"),
                format!("---\nname: {skill}\ndescription: {cat}\n---\n"),
            )
            .unwrap();
        }

        let mut store = SkillStore::new();
        let config = ParseConfig {
            strict: false,
            max_skill_size: 1_048_576,
            max_skills: 1000,
        };
        let errors = store.load_from_directory(temp_dir.path(), &config);

        assert!(errors.is_empty(), "unexpected load errors: {errors:?}");
        assert_eq!(store.len(), 2);
        assert!(store.get("notes").is_some());
        assert!(store.get("journal").is_some());
    }

    // -----------------------------------------------------------------------
    // split_primary duplicate-list tests
    // -----------------------------------------------------------------------

    #[test]
    fn split_primary_dedups_primary_list_preserving_order() {
        let mut store = SkillStore::new();
        for name in ["github", "notion", "other"] {
            store.upsert(create_test_entry(name, "skill", vec![]));
        }

        let list = vec![
            "github".to_string(),
            "github".to_string(),
            "other".to_string(),
        ];
        let (primary, secondary) = store.split_primary(Some(&list));

        assert_eq!(primary, vec!["github".to_string(), "other".to_string()]);
        assert_eq!(secondary, vec!["notion".to_string()]);
    }

    // -----------------------------------------------------------------------
    // max_skills limit classification tests
    // -----------------------------------------------------------------------

    fn limit_config(max_skills: usize) -> ParseConfig {
        ParseConfig {
            strict: false,
            max_skill_size: 1_048_576,
            max_skills,
        }
    }

    #[test]
    fn store_max_skills_skips_non_skill_dirs_silently() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let skill = temp_dir.path().join("alpha");
        std::fs::create_dir(&skill).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            "---\nname: alpha\ndescription: a\n---\n",
        )
        .unwrap();
        let plain = temp_dir.path().join("not-a-skill");
        std::fs::create_dir(&plain).unwrap();

        let mut store = SkillStore::new();
        let errors = store.load_from_directory(temp_dir.path(), &limit_config(0));

        // The real skill still reports the limit as before...
        assert_eq!(errors.len(), 1, "only the skill may error, got {errors:?}");
        assert_eq!(errors[0].path, skill);
        assert!(
            errors[0].error.contains("max skills"),
            "{}",
            errors[0].error
        );
        // ...but the non-skill directory is skipped silently, exactly as
        // it is below the limit.
        assert!(store.is_empty());
    }

    #[test]
    fn store_max_skills_charges_category_skills_once() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let nested = temp_dir.path().join("cat").join("inner");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(
            nested.join("SKILL.md"),
            "---\nname: inner\ndescription: i\n---\n",
        )
        .unwrap();
        // A non-skill sibling inside the category must stay silent too.
        std::fs::create_dir_all(temp_dir.path().join("cat").join("docs")).unwrap();

        let mut store = SkillStore::new();
        let errors = store.load_from_directory(temp_dir.path(), &limit_config(0));

        assert_eq!(
            errors.len(),
            1,
            "the category must be charged once per skill, got {errors:?}"
        );
        assert_eq!(errors[0].path, nested);
        assert!(store.is_empty());
    }

    #[test]
    fn store_max_skills_limit_still_enforced_for_real_skills() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        for name in ["first", "second"] {
            let dir = temp_dir.path().join(name);
            std::fs::create_dir(&dir).unwrap();
            std::fs::write(
                dir.join("SKILL.md"),
                format!("---\nname: {name}\ndescription: d\n---\n"),
            )
            .unwrap();
        }

        let mut store = SkillStore::new();
        let errors = store.load_from_directory(temp_dir.path(), &limit_config(1));

        assert_eq!(store.len(), 1, "the first skill still loads");
        assert_eq!(errors.len(), 1, "the second skill still errors");
        assert!(errors[0].error.contains("max skills"));
    }
}
