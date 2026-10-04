//! Explicit mount sources and deterministic, whole-skill precedence.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use skillfs_core::{ParseConfig, store::SkillStore};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Ordered mount inputs, separate from the legacy security configuration.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MountFile {
    /// Dedicated output directory, outside all sources.
    pub(super) mountpoint: PathBuf,
    /// Highest-precedence source first.
    pub(super) sources: Vec<PathBuf>,
}

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
    pub(super) fn validate(mut self) -> Result<Self> {
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
                roots.push(root);
            }
        }
        self.sources = roots;
        Ok(self)
    }
}

/// Merge complete skills by directory name without changing source files.
pub(super) fn load_sources(roots: &[PathBuf], config: &ParseConfig) -> Result<SkillStore> {
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
                // The earlier source already provides this skill; a broken
                // copy in a lower-precedence source (invalid YAML, oversized
                // SKILL.md, ...) is irrelevant to what gets served, so warn
                // and skip it instead of aborting the whole mount.
                tracing::warn!(skill = %name, selected = %winner.source_path.display(),
                    shadowed = %entry.source_path.display(), "duplicate skill: earlier source wins");
                continue;
            }
            if let skillfs_core::ParseStatus::Error(error) = &entry.parse_status {
                return Err(format!("{}: {error}", entry.source_path.display()).into());
            }
            if merged.len() >= config.max_skills {
                return Err(format!(
                    "combined sources exceed max skills limit ({})",
                    config.max_skills
                )
                .into());
            }
            merged.upsert(entry.clone());
        }
        // The loader reports a failed SKILL.md (e.g. oversized) without
        // inserting an entry, so the precedence decision above cannot see
        // that skill. Such a per-skill error is shadowed only when an earlier
        // source really provides the name; a structural error the current
        // root reports (two of its directories colliding on one leaf name)
        // names a skill no winner provides and stays fatal.
        for error in &errors {
            if let Some(skill) = error_skill_name(error) {
                if winners.contains(&skill) {
                    tracing::warn!(skill = %skill, shadowed = %error.path.display(),
                        error = %error.error,
                        "duplicate skill: earlier source wins over a broken copy");
                    continue;
                }
            }
            return Err(format!("{}: {}", error.path.display(), error.error).into());
        }
    }
    Ok(merged)
}

/// Skill identity carried by a per-file load error: skills are keyed by
/// directory leaf name and a failed `SKILL.md` is reported by its file
/// path, so the parent directory's leaf name names the skill at fault.
/// Errors for anything else (the source root itself, the max-skills
/// guard, non-UTF-8 names) carry no skill identity.
fn error_skill_name(error: &skillfs_core::store::LoadError) -> Option<String> {
    if error.path.file_name()? != "SKILL.md" {
        return None;
    }
    error
        .path
        .parent()?
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_string)
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
}
