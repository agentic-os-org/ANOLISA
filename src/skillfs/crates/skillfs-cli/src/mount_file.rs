//! Explicit mount sources and deterministic, whole-skill precedence.

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
        if !errors.is_empty() {
            return Err(errors
                .iter()
                .map(|e| format!("{}: {}", e.path.display(), e.error))
                .collect::<Vec<_>>()
                .join("; ")
                .into());
        }
        for (name, entry) in store.iter() {
            if let skillfs_core::ParseStatus::Error(error) = &entry.parse_status {
                return Err(format!("{}: {error}", entry.source_path.display()).into());
            }
            if let Some(winner) = merged.get(name) {
                tracing::warn!(skill = %name, selected = %winner.source_path.display(),
                    shadowed = %entry.source_path.display(), "duplicate skill: earlier source wins");
            } else {
                if merged.len() >= config.max_skills {
                    return Err(format!(
                        "combined sources exceed max skills limit ({})",
                        config.max_skills
                    )
                    .into());
                }
                merged.upsert(entry.clone());
            }
        }
    }
    Ok(merged)
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
