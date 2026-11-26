//! Explicit mount sources and deterministic, whole-skill precedence.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use skillfs_core::{ParseConfig, store::SkillStore};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Ordered mount inputs, separate from the legacy security configuration.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MountFile {
    /// Dedicated output directory, outside all sources.
    pub(super) mountpoint: PathBuf,
    /// Highest-precedence source first.
    pub(super) sources: Vec<PathBuf>,
}

/// Borrowed observations of the existing validation and merge decisions.
pub(super) trait Observer {
    /// Record the normalized mountpoint without creating it.
    fn mountpoint(&mut self, _path: &Path) {}
    /// Record one validated, canonical, deduplicated source.
    fn source(&mut self, _path: &Path) {}
    /// Record an accepted whole-skill winner.
    fn selected(&mut self, _source: &Path, _name: &str, _entry: &skillfs_core::SkillEntry) {}
    /// Record a loaded copy hidden by an earlier source.
    fn shadowed(&mut self, _source: &Path, _name: &str, _entry: &skillfs_core::SkillEntry) {}
    /// Record a nonblocking failed copy hidden by an earlier source.
    fn shadowed_error(
        &mut self,
        _source: &Path,
        _name: &str,
        _error: &skillfs_core::store::LoadError,
    ) {
    }
    /// Preserve discovery of a failing skill when its identity is known.
    fn rejected(&mut self, _name: Option<&str>) {}
}

struct IgnoreObservations;

impl Observer for IgnoreObservations {}

/// Recognize mount keys while preserving legacy security configuration files.
pub(super) fn load(path: &Path) -> Result<Option<MountFile>> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read config '{}': {e}", path.display()))?;
    let value: toml::Value =
        toml::from_str(&text).map_err(|e| format!("invalid config '{}': {e}", path.display()))?;
    if value.get("sources").is_none() && value.get("mountpoint").is_none() {
        return Ok(None);
    }
    Ok(Some(value.try_into().map_err(|e| {
        format!("invalid mount config '{}': {e}", path.display())
    })?))
}

/// Reject flags whose single-root semantics have not been adapted.
pub(super) fn validate_options(args: &clap::ArgMatches) -> Result<()> {
    // Clap also reports the derive-generated Mount argument group.
    for id in args.ids() {
        if args.value_source(id.as_str()) == Some(clap::parser::ValueSource::CommandLine)
            && !matches!(
                id.as_str(),
                "Mount"
                    | "config"
                    | "foreground"
                    | "managed"
                    | "allow_other"
                    | "read_only"
                    | "verbose"
                    | "log_file"
            )
        {
            return Err(format!("mount configuration cannot be combined with '{}'", id).into());
        }
    }
    Ok(())
}

impl MountFile {
    /// Resolve and deduplicate roots before creating any mount resources.
    pub(super) fn validate(self) -> Result<Self> {
        self.validate_observed(&mut IgnoreObservations)
    }

    /// Validate every source before callers may start observing skill loads.
    pub(super) fn validate_observed(mut self, observer: &mut impl Observer) -> Result<Self> {
        if self.sources.is_empty() {
            return Err("mount config sources must not be empty".into());
        }
        if !self.mountpoint.is_absolute() || self.sources.iter().any(|p| !p.is_absolute()) {
            return Err("mount config sources and mountpoint must be absolute paths".into());
        }
        // Resolve existing ancestors even when the mount directory is new.
        let mut ancestor = self.mountpoint.as_path();
        let mut suffix = Vec::new();
        while !ancestor.exists() {
            suffix.push(ancestor.file_name().ok_or("invalid mountpoint")?);
            ancestor = ancestor.parent().ok_or("invalid mountpoint parent")?;
        }
        let mut mountpoint = ancestor.canonicalize()?;
        for part in suffix.into_iter().rev() {
            mountpoint.push(part);
        }
        self.mountpoint = super::lexical_absolute(&mountpoint)?;
        observer.mountpoint(&self.mountpoint);
        let mut roots = Vec::new();
        for source in self.sources {
            let root = source
                .canonicalize()
                .map_err(|e| format!("cannot resolve source '{}': {e}", source.display()))?;
            std::fs::read_dir(&root)
                .map_err(|e| format!("cannot read source '{}': {e}", root.display()))?;
            if root.starts_with(&self.mountpoint) || self.mountpoint.starts_with(&root) {
                return Err(format!(
                    "source '{}' and mountpoint must not overlap",
                    root.display()
                )
                .into());
            }
            if !roots.contains(&root) {
                observer.source(&root);
                roots.push(root);
            }
        }
        self.sources = roots;
        Ok(self)
    }
}

/// Merge complete skills by directory name without changing source files.
pub(super) fn load_sources(roots: &[PathBuf], config: &ParseConfig) -> Result<SkillStore> {
    load_sources_observed(roots, config, &mut IgnoreObservations)
}

/// Observe merge decisions without allocating diagnostic records for mounts.
pub(super) fn load_sources_observed(
    roots: &[PathBuf],
    config: &ParseConfig,
    observer: &mut impl Observer,
) -> Result<SkillStore> {
    let mut merged = SkillStore::new();
    for root in roots {
        let mut store = SkillStore::new();
        let errors = store.load_from_directory(root, config);
        // Skills an earlier source already provides, sampled before this root
        // is merged in. The entry loop below can consult `merged` directly
        // because a store holds each name once; the error loop cannot, since
        // by then this root's own entries are in `merged` and a collision
        // inside this root would read as a copy shadowed by itself.
        let winners: HashSet<String> = merged.iter().map(|(name, _)| name.clone()).collect();
        for (name, entry) in store.iter() {
            if let Some(winner) = merged.get(name) {
                observer.shadowed(root, name, entry);
                // The earlier source already provides this skill; a broken
                // copy in a lower-precedence source (invalid YAML, oversized
                // SKILL.md, ...) is irrelevant to what gets served, so warn
                // and skip it instead of aborting the whole mount.
                tracing::warn!(skill = %name, selected = %winner.source_path.display(),
                    shadowed = %entry.source_path.display(), "duplicate skill: earlier source wins");
                continue;
            }
            if let skillfs_core::ParseStatus::Error(error) = &entry.parse_status {
                observer.rejected(Some(name));
                return Err(format!("{}: {error}", entry.source_path.display()).into());
            }
            if merged.len() >= config.max_skills {
                observer.rejected(Some(name));
                return Err(format!(
                    "combined sources exceed max skills limit ({})",
                    config.max_skills
                )
                .into());
            }
            observer.selected(root, name, entry);
            merged.upsert(entry.clone());
        }
        // The loader reports a failed SKILL.md (e.g. oversized) without
        // inserting an entry, so the precedence decision above cannot see
        // that skill. Such a per-skill error is shadowed only when an earlier
        // source really provides the name; a structural error the current
        // root reports (two of its directories colliding on one leaf name)
        // names a skill no winner provides and stays fatal.
        for error in &errors {
            let skill = error_skill_name(root, error);
            if let Some(skill) = skill {
                if winners.contains(skill) {
                    observer.shadowed_error(root, skill, error);
                    tracing::warn!(skill = %skill, shadowed = %error.path.display(),
                        error = %error.error,
                        "duplicate skill: earlier source wins over a broken copy");
                    continue;
                }
            }
            // Directory-inspection errors (unreadable candidate skill or
            // category directories under a readable source root) warn and
            // continue, aligning mount-file mounts with single-source
            // `skillfs mount` so unrelated inaccessible auxiliary directories
            // do not abort the mount. Unreadable source roots (`error.path ==
            // root`) and confirmed manifest/structural errors remain fatal.
            if error.is_directory_inspection_error() && error.path != *root {
                tracing::warn!(path = %error.path.display(), error = %error.error,
                    "skipping unreadable directory during mount");
                continue;
            }
            observer.rejected(skill);
            return Err(format!("{}: {}", error.path.display(), error.error).into());
        }
    }
    Ok(merged)
}

/// Skill identity carried by a per-skill load error: skills are keyed by
/// directory leaf name. A failed `SKILL.md` reports its file path (so its
/// parent directory leaf names the skill), and an unreadable candidate skill
/// directory probe under `root` reports the directory itself (so its leaf
/// names the candidate skill for shadowing). Errors for anything else (the
/// source root itself, category listing failures, the max-skills guard,
/// non-UTF-8 names) carry no skill identity.
fn error_skill_name<'a>(root: &Path, error: &'a skillfs_core::store::LoadError) -> Option<&'a str> {
    if error.path.file_name() == Some(std::ffi::OsStr::new("SKILL.md")) {
        return error
            .path
            .parent()?
            .file_name()
            .and_then(|name| name.to_str());
    }
    if error.is_manifest_probe_error() && error.path != root {
        return error.path.file_name().and_then(|name| name.to_str());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_precedence_limits_and_invalid_skills() {
        let temp = tempfile::tempdir().unwrap();
        let roots: Vec<_> = ["high", "low"].map(|s| temp.path().join(s)).into();
        for root in &roots {
            std::fs::create_dir_all(root.join("demo")).unwrap();
            std::fs::write(
                root.join("demo/SKILL.md"),
                "---\nname: demo\ndescription: test\n---\n",
            )
            .unwrap();
        }
        let store = load_sources(&roots, &ParseConfig::default()).unwrap();
        assert_eq!(store.len(), 1);
        assert_eq!(
            store.get("demo").unwrap().source_path,
            roots[0].join("demo/SKILL.md")
        );
        std::fs::rename(roots[1].join("demo"), roots[1].join("other")).unwrap();
        let config = ParseConfig {
            max_skills: 1,
            ..ParseConfig::default()
        };
        assert!(
            load_sources(&roots, &config)
                .unwrap_err()
                .to_string()
                .contains("combined sources")
        );
        std::fs::write(
            roots[0].join("demo/SKILL.md"),
            "---\ndescription: [broken\n---\n",
        )
        .unwrap();
        assert!(load_sources(&roots, &ParseConfig::default()).is_err());
    }

    #[test]
    fn shadowed_source_parse_error_does_not_abort_mount() {
        let temp = tempfile::tempdir().unwrap();
        let roots: Vec<_> = ["high", "low"].map(|s| temp.path().join(s)).into();
        for root in &roots {
            std::fs::create_dir_all(root.join("demo")).unwrap();
        }
        std::fs::write(
            roots[0].join("demo/SKILL.md"),
            "---\nname: demo\ndescription: good\n---\n",
        )
        .unwrap();
        // Lower-precedence duplicate is unparseable. The documented rule is
        // "earlier source wins", so the mount must serve the good copy
        // instead of failing on the shadowed one.
        std::fs::write(
            roots[1].join("demo/SKILL.md"),
            "---\ndescription: [broken\n---\n",
        )
        .unwrap();
        let store = load_sources(&roots, &ParseConfig::default()).unwrap();
        assert_eq!(store.len(), 1);
        assert_eq!(
            store.get("demo").unwrap().source_path,
            roots[0].join("demo/SKILL.md")
        );

        // The same parse error in the only source still aborts the mount.
        std::fs::remove_file(roots[0].join("demo/SKILL.md")).unwrap();
        assert!(load_sources(&roots, &ParseConfig::default()).is_err());
    }

    /// An oversized SKILL.md is reported as a load error and never inserted,
    /// so only the post-precedence error handling can shadow it: a valid
    /// higher-precedence copy must win without failing the mount.
    #[test]
    fn shadowed_oversized_duplicate_does_not_abort_mount() {
        let temp = tempfile::tempdir().unwrap();
        let roots: Vec<_> = ["high", "low"].map(|s| temp.path().join(s)).into();
        for root in &roots {
            std::fs::create_dir_all(root.join("demo")).unwrap();
        }
        std::fs::write(
            roots[0].join("demo/SKILL.md"),
            "---\nname: demo\ndescription: good\n---\n",
        )
        .unwrap();
        // Lower-precedence copy exceeds the max SKILL.md size: the loader
        // reports a LoadError and inserts no entry for it.
        let config = ParseConfig {
            max_skill_size: 64,
            ..ParseConfig::default()
        };
        std::fs::write(
            roots[1].join("demo/SKILL.md"),
            format!("---\nname: demo\ndescription: {}\n---\n", "x".repeat(64)),
        )
        .unwrap();

        let store = load_sources(&roots, &config).unwrap();
        assert_eq!(store.len(), 1);
        assert_eq!(
            store.get("demo").unwrap().source_path,
            roots[0].join("demo/SKILL.md")
        );
    }

    /// An oversized SKILL.md that no valid copy shadows still aborts the
    /// mount: the error must stay visible, not silently drop the skill.
    #[test]
    fn unshadowed_oversized_skill_aborts_mount() {
        let temp = tempfile::tempdir().unwrap();
        let roots: Vec<_> = ["high", "low"].map(|s| temp.path().join(s)).into();
        std::fs::create_dir_all(roots[0].join("demo")).unwrap();
        std::fs::write(
            roots[0].join("demo/SKILL.md"),
            "---\nname: demo\ndescription: good\n---\n",
        )
        .unwrap();
        // A different, unique skill in the lower-precedence source is
        // oversized and shadowed by nobody.
        std::fs::create_dir_all(roots[1].join("huge")).unwrap();
        let config = ParseConfig {
            max_skill_size: 64,
            ..ParseConfig::default()
        };
        std::fs::write(
            roots[1].join("huge/SKILL.md"),
            format!("---\nname: huge\ndescription: {}\n---\n", "x".repeat(64)),
        )
        .unwrap();
        let err = load_sources(&roots, &config).unwrap_err().to_string();
        assert!(
            err.contains("huge/SKILL.md"),
            "the oversized unique skill must abort the mount, got: {err}"
        );
    }

    /// Two categories in one source may hold skills with the same leaf name;
    /// the loader reports that collision as a structural error. No earlier
    /// source provides the name, so it must abort the mount instead of being
    /// downgraded to a warning about a "shadowed" copy of itself.
    #[test]
    fn same_name_inside_one_source_aborts_mount() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("source");
        for category in ["a", "b"] {
            let dir = root.join(category).join("demo");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("SKILL.md"),
                "---\nname: demo\ndescription: good\n---\n",
            )
            .unwrap();
        }
        let err = load_sources(&[root], &ParseConfig::default())
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("duplicate skill name 'demo'"),
            "a same-source name collision must abort the mount, got: {err}"
        );
    }

    #[test]
    fn canonical_roots_and_mount_overlap() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("source");
        std::fs::create_dir(&root).unwrap();
        let alias = temp.path().join("alias");
        std::os::unix::fs::symlink(&root, &alias).unwrap();
        let file = MountFile {
            mountpoint: temp.path().join("new/mount"),
            sources: vec![root.clone(), alias.clone()],
        }
        .validate()
        .unwrap();
        assert_eq!(file.sources, vec![root.clone()]);
        for mountpoint in [root.clone(), alias.join("mount"), temp.path().to_path_buf()] {
            assert!(
                MountFile {
                    mountpoint,
                    sources: vec![root.clone()]
                }
                .validate()
                .is_err()
            );
        }
    }

    #[cfg(unix)]
    struct RestorePermissions<'a>(&'a Path, u32);

    #[cfg(unix)]
    impl Drop for RestorePermissions<'_> {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(self.0, std::fs::Permissions::from_mode(self.1));
        }
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_auxiliary_directories_do_not_abort_mount() {
        use std::os::unix::fs::PermissionsExt;

        // Root bypasses directory permission checks.
        // SAFETY: geteuid takes no pointers and reads the effective uid.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("source");
        let demo = root.join("demo");
        let aux = root.join("aux");
        let cat_aux = root.join("cat-aux");
        std::fs::create_dir_all(&demo).unwrap();
        std::fs::create_dir_all(&aux).unwrap();
        std::fs::create_dir_all(&cat_aux).unwrap();
        std::fs::write(
            demo.join("SKILL.md"),
            "---\nname: demo\ndescription: good\n---\n",
        )
        .unwrap();

        let _restore_aux = RestorePermissions(&aux, 0o755);
        let _restore_cat_aux = RestorePermissions(&cat_aux, 0o755);
        std::fs::set_permissions(&aux, std::fs::Permissions::from_mode(0o000)).unwrap();
        std::fs::set_permissions(&cat_aux, std::fs::Permissions::from_mode(0o111)).unwrap();

        let store = load_sources(&[root.clone()], &ParseConfig::default()).unwrap();
        assert_eq!(store.len(), 1);
        assert_eq!(
            store.get("demo").unwrap().source_path,
            demo.join("SKILL.md")
        );
    }

    #[cfg(unix)]
    #[test]
    fn shadowed_unreadable_duplicate_is_observed_without_aborting_mount() {
        use std::os::unix::fs::PermissionsExt;

        // Root bypasses directory permission checks.
        // SAFETY: geteuid takes no pointers and reads the effective uid.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        #[derive(Default)]
        struct RecordingObserver {
            shadowed_errors: Vec<(PathBuf, String, PathBuf)>,
            rejected: Vec<Option<String>>,
        }
        impl Observer for RecordingObserver {
            fn shadowed_error(
                &mut self,
                source: &Path,
                name: &str,
                error: &skillfs_core::store::LoadError,
            ) {
                self.shadowed_errors.push((
                    source.to_path_buf(),
                    name.to_owned(),
                    error.path.clone(),
                ));
            }
            fn rejected(&mut self, name: Option<&str>) {
                self.rejected.push(name.map(str::to_owned));
            }
        }

        let temp = tempfile::tempdir().unwrap();
        let roots: Vec<_> = ["high", "low-flat", "low-cat"]
            .map(|s| temp.path().join(s))
            .into();
        let winner = roots[0].join("demo");
        let flat_dup = roots[1].join("demo");
        let nested_dup = roots[2].join("ops").join("demo");
        for dir in [&winner, &flat_dup, &nested_dup] {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(
                dir.join("SKILL.md"),
                "---\nname: demo\ndescription: good\n---\n",
            )
            .unwrap();
        }

        let _restore_flat = RestorePermissions(&flat_dup, 0o755);
        let _restore_nested = RestorePermissions(&nested_dup, 0o755);
        std::fs::set_permissions(&flat_dup, std::fs::Permissions::from_mode(0o000)).unwrap();
        std::fs::set_permissions(&nested_dup, std::fs::Permissions::from_mode(0o000)).unwrap();

        let mut observer = RecordingObserver::default();
        let store = load_sources_observed(&roots, &ParseConfig::default(), &mut observer).unwrap();
        assert_eq!(store.len(), 1);
        assert_eq!(
            store.get("demo").unwrap().source_path,
            winner.join("SKILL.md")
        );
        assert!(observer.rejected.is_empty(), "{:?}", observer.rejected);
        assert_eq!(
            observer.shadowed_errors,
            vec![
                (roots[1].clone(), "demo".to_owned(), flat_dup.clone()),
                (roots[2].clone(), "demo".to_owned(), nested_dup.clone()),
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_source_root_and_unreadable_manifest_still_abort_mount() {
        use std::os::unix::fs::PermissionsExt;

        // Root bypasses file and directory permission checks.
        // SAFETY: geteuid takes no pointers and reads the effective uid.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let temp = tempfile::tempdir().unwrap();

        let blocked_root = temp.path().join("blocked-root");
        std::fs::create_dir(&blocked_root).unwrap();
        {
            let _restore = RestorePermissions(&blocked_root, 0o755);
            std::fs::set_permissions(&blocked_root, std::fs::Permissions::from_mode(0o000))
                .unwrap();
            let err = load_sources(&[blocked_root.clone()], &ParseConfig::default())
                .unwrap_err()
                .to_string();
            assert!(
                err.contains("cannot read directory"),
                "unreadable source root must abort mount, got: {err}"
            );
        }

        let root = temp.path().join("source");
        let demo = root.join("demo");
        std::fs::create_dir_all(&demo).unwrap();
        let manifest = demo.join("SKILL.md");
        std::fs::write(&manifest, "---\nname: demo\ndescription: good\n---\n").unwrap();
        let _restore = RestorePermissions(&manifest, 0o644);
        std::fs::set_permissions(&manifest, std::fs::Permissions::from_mode(0o000)).unwrap();
        let err = load_sources(&[root], &ParseConfig::default())
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("demo/SKILL.md"),
            "confirmed unreadable SKILL.md must still abort mount, got: {err}"
        );
    }
}
