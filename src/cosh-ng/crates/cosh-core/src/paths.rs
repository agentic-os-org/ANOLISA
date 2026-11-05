//! Installed and XDG resource discovery shared by skills and extensions.

use std::path::{Path, PathBuf};

/// Ordered resource roots, keeping user contributions above system packages.
#[derive(Default)]
pub(crate) struct ResourceDirs {
    pub(crate) user_skills: Vec<PathBuf>,
    pub(crate) system_skills: Vec<PathBuf>,
    pub(crate) user_extensions: Vec<PathBuf>,
    pub(crate) system_extensions: Vec<PathBuf>,
}

/// Resolve installed paths without depending on a particular distributor.
pub(crate) fn resource_dirs() -> ResourceDirs {
    resource_dirs_for(
        std::env::current_exe().ok().as_deref(),
        dirs::home_dir().as_deref(),
        std::env::var_os("XDG_DATA_HOME").as_deref(),
        std::env::var_os("XDG_DATA_DIRS").as_deref(),
    )
}

fn valid_root(path: &Path) -> bool {
    path.is_absolute()
        && !path
            .as_os_str()
            .to_string_lossy()
            .split(std::path::MAIN_SEPARATOR)
            .any(|part| part == "." || part == "..")
}

fn push_unique(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !paths.contains(&path) {
        paths.push(path);
    }
}

fn installed_prefix(executable: &Path) -> Option<(&Path, Option<&std::ffi::OsStr>)> {
    let component = executable.parent()?;
    if component.file_name()? != "cosh-ng" {
        return None;
    }
    let parent = component.parent()?;
    if parent.file_name()? == "libexec" {
        return Some((parent.parent()?, None));
    }
    let libexec = parent.parent()?;
    (libexec.file_name()? == "libexec").then_some((libexec.parent()?, parent.file_name()))
}

fn default_system_bases(prefix: Option<&Path>) -> Vec<PathBuf> {
    let rebase = prefix.and_then(|prefix| {
        if prefix.ends_with("usr/local") {
            prefix.parent()?.parent()
        } else if prefix.ends_with("usr") {
            prefix.parent()
        } else {
            None
        }
    });
    let root = rebase.unwrap_or_else(|| Path::new("/"));
    vec![root.join("usr/local/share"), root.join("usr/share")]
}

/// Explicit discovery inputs let tests avoid the host installation and environment.
pub(crate) fn resource_dirs_for(
    executable: Option<&Path>,
    home: Option<&Path>,
    data_home: Option<&std::ffi::OsStr>,
    data_dirs: Option<&std::ffi::OsStr>,
) -> ResourceDirs {
    let data_home = data_home
        .map(Path::new)
        .filter(|path| valid_root(path))
        .map(Path::to_path_buf)
        .or_else(|| home.map(|home| home.join(".local/share")));
    let installed = executable.and_then(installed_prefix);
    let mut bases: Vec<PathBuf> = data_dirs
        .filter(|value| !value.is_empty())
        .map(|value| {
            std::env::split_paths(value)
                .filter(|path| valid_root(path))
                .collect()
        })
        .unwrap_or_else(|| default_system_bases(installed.map(|(prefix, _)| prefix)));
    if let Some((prefix, _)) = installed {
        let base = prefix.join("share");
        if !bases.contains(&base) {
            bases.insert(0, base);
        }
    }

    let mut result = ResourceDirs::default();
    if let Some(home) = home {
        result.user_skills.push(home.join(".copilot-shell/skills"));
    }
    if let Some(executable) = executable {
        result.read_installed(executable, home, data_home.as_deref());
    }
    let recorded_skills = std::mem::take(&mut result.system_skills);
    let recorded_extensions = std::mem::take(&mut result.system_extensions);
    // Keep established distribution roots as compatibility candidates.
    // Installed namespaces come from the layout, not a product-name test.
    let mut namespaces = vec![std::ffi::OsString::from("cosh-ng")];
    if let Some((_, Some(namespace))) = installed {
        if !namespaces.iter().any(|existing| existing == namespace) {
            namespaces.push(namespace.to_os_string());
        }
    }
    if !namespaces.iter().any(|namespace| namespace == "anolisa") {
        namespaces.push("anolisa".into());
    }
    for namespace in namespaces {
        if let Some(data) = &data_home {
            result.add_root(&data.join(&namespace), true);
        }
        for base in &bases {
            let user = data_home
                .as_ref()
                .is_some_and(|data| base.starts_with(data))
                || home.is_some_and(|home| base.starts_with(home.join(".local")));
            result.add_root(&base.join(&namespace), user);
        }
    }
    // Recording a standard root must not override its established precedence.
    for (paths, recorded) in [
        (&mut result.system_skills, recorded_skills),
        (&mut result.system_extensions, recorded_extensions),
    ] {
        for path in recorded.into_iter().rev() {
            if !paths.contains(&path) {
                paths.insert(0, path);
            }
        }
    }
    // A user install may share its XDG root with its inferred prefix.
    // Scan that directory once, at user priority, rather than inventing two sources.
    result
        .system_skills
        .retain(|path| !result.user_skills.contains(path));
    result
        .system_extensions
        .retain(|path| !result.user_extensions.contains(path));
    result
}

impl ResourceDirs {
    fn add_root(&mut self, root: &Path, user: bool) {
        self.add_path("skill_dir", root.join("skills"), user);
        self.add_path("extension_dir", root.join("extensions"), user);
    }

    fn add_path(&mut self, kind: &str, path: PathBuf, user: bool) {
        let paths = match (kind, user) {
            ("skill_dir", true) => &mut self.user_skills,
            ("skill_dir", false) => &mut self.system_skills,
            ("extension_dir", true) => &mut self.user_extensions,
            ("extension_dir", false) => &mut self.system_extensions,
            _ => return,
        };
        push_unique(paths, path);
    }

    fn read_installed(&mut self, executable: &Path, home: Option<&Path>, data: Option<&Path>) {
        let Some(directory) = executable.parent() else {
            return;
        };
        let file = directory.join("resources.conf");
        let text = match std::fs::read_to_string(&file) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(error) => {
                tracing::warn!(path = %file.display(), %error, "cannot read installed resource paths");
                return;
            }
        };
        match parse_resources(&text) {
            Ok((profile, entries)) => {
                for (kind, path) in entries {
                    let user = profile.unwrap_or_else(|| {
                        data.is_some_and(|data| path.starts_with(data))
                            || home.is_some_and(|home| path.starts_with(home))
                            || home.is_some_and(|home| executable.starts_with(home.join(".local")))
                    });
                    self.add_path(kind, path, user);
                }
            }
            Err(error) => {
                tracing::warn!(path = %file.display(), error, "invalid installed resource paths")
            }
        }
    }
}

type ResourceEntries<'a> = (Option<bool>, Vec<(&'a str, PathBuf)>);

fn parse_resources(text: &str) -> Result<ResourceEntries<'_>, &'static str> {
    let mut profile = None;
    let mut entries = Vec::new();
    for line in text
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
    {
        let (key, value) = line.split_once('=').ok_or("expected key=value")?;
        match key {
            "profile" => {
                profile = Some(match value {
                    "system" => false,
                    "user" => true,
                    _ => return Err("profile must be system or user"),
                })
            }
            "skill_dir" | "extension_dir" => {
                let path = PathBuf::from(value);
                if !valid_root(&path) {
                    return Err("resource directories must be absolute without dot segments");
                }
                entries.push((key, path));
            }
            _ => return Err("unknown resource path key"),
        }
    }
    Ok((profile, entries))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespaces_and_prefixes_are_not_product_specific() {
        for namespace in ["anolisa", "other-vendor"] {
            let executable =
                PathBuf::from(format!("/opt/owner/libexec/{namespace}/cosh-ng/cosh-core"));
            let roots = resource_dirs_for(Some(&executable), None, None, None);
            assert!(roots.system_skills.contains(&PathBuf::from(format!(
                "/opt/owner/share/{namespace}/skills"
            ))));
        }
        let roots = resource_dirs_for(
            Some(Path::new("/opt/owner/libexec/cosh-ng/cosh-core")),
            None,
            None,
            None,
        );
        assert_eq!(
            roots.system_skills[0],
            Path::new("/opt/owner/share/cosh-ng/skills")
        );
    }

    #[test]
    fn raw_and_rpm_roots_follow_a_rebased_installation() {
        for suffix in ["cosh-core", "cosh-core (deleted)"] {
            let executable = PathBuf::from(format!(
                "/opt/root/usr/local/libexec/anolisa/cosh-ng/{suffix}"
            ));
            let roots = resource_dirs_for(Some(&executable), None, None, None);
            assert!(roots
                .system_skills
                .contains(&PathBuf::from("/opt/root/usr/share/anolisa/skills")));
            assert!(!roots
                .system_skills
                .contains(&PathBuf::from("/usr/share/anolisa/skills")));
        }
    }

    #[test]
    fn recorded_standard_system_roots_keep_their_discovery_order() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("usr/libexec/anolisa/cosh-ng");
        std::fs::create_dir_all(&directory).unwrap();
        let executable = directory.join("cosh-core");
        let expected = resource_dirs_for(Some(&executable), None, None, None);
        std::fs::write(
            directory.join("resources.conf"),
            format!(
                "profile=system\nskill_dir={}\nextension_dir={}\n",
                root.path().join("usr/share/anolisa/skills").display(),
                root.path().join("usr/share/anolisa/extensions").display(),
            ),
        )
        .unwrap();
        let actual = resource_dirs_for(Some(&executable), None, None, None);
        assert_eq!(actual.system_skills, expected.system_skills);
        assert_eq!(actual.system_extensions, expected.system_extensions);
    }

    #[test]
    fn xdg_roots_are_ordered_and_invalid_entries_are_ignored() {
        let roots = resource_dirs_for(
            None,
            Some(Path::new("/home/owner")),
            Some("/data/user".as_ref()),
            Some("/data/first:relative:/data/second:/data/first".as_ref()),
        );
        assert_eq!(
            roots.system_skills,
            vec![
                PathBuf::from("/data/first/cosh-ng/skills"),
                PathBuf::from("/data/second/cosh-ng/skills"),
                PathBuf::from("/data/first/anolisa/skills"),
                PathBuf::from("/data/second/anolisa/skills"),
            ]
        );
        assert!(roots
            .user_skills
            .contains(&PathBuf::from("/data/user/cosh-ng/skills")));
        assert!(!roots
            .user_skills
            .contains(&PathBuf::from("/home/owner/.local/share/cosh-ng/skills")));
        let fallback = resource_dirs_for(
            None,
            Some(Path::new("/home/owner")),
            Some("relative".as_ref()),
            Some("".as_ref()),
        );
        assert!(fallback
            .user_skills
            .contains(&PathBuf::from("/home/owner/.local/share/cosh-ng/skills")));
        assert!(fallback
            .system_skills
            .contains(&PathBuf::from("/usr/share/cosh-ng/skills")));
    }

    #[test]
    fn explicit_install_paths_preserve_scope_and_literal_characters() {
        let root = tempfile::tempdir().unwrap();
        let executable = root.path().join("cosh-core");
        let config = root.path().join("resources.conf");
        std::fs::write(&config, "profile=user\nskill_dir=/custom/space and 'quote'=value\nextension_dir=/custom/extensions\n").unwrap();
        let roots = resource_dirs_for(Some(&executable), None, None, None);
        assert_eq!(
            roots.user_skills,
            vec![PathBuf::from("/custom/space and 'quote'=value")]
        );
        assert_eq!(
            roots.user_extensions,
            vec![PathBuf::from("/custom/extensions")]
        );
        assert!(!roots
            .system_extensions
            .contains(&PathBuf::from("/custom/extensions")));
        std::fs::write(&config, "profile=system\nskill_dir=/custom/system\n").unwrap();
        let roots = resource_dirs_for(Some(&executable), None, None, None);
        assert_eq!(roots.system_skills[0], Path::new("/custom/system"));
    }

    #[test]
    fn raw_layout_uses_xdg_scope_and_rejects_invalid_records() {
        let root = tempfile::tempdir().unwrap();
        let executable = root.path().join("cosh-core");
        std::fs::write(
            root.path().join("resources.conf"),
            "skill_dir=/xdg/anolisa/skills\nextension_dir=/xdg/anolisa/extensions\n",
        )
        .unwrap();
        let paths = resource_dirs_for(
            Some(&executable),
            Some(Path::new("/home/owner")),
            Some("/xdg".as_ref()),
            None,
        );
        assert!(paths
            .user_extensions
            .contains(&PathBuf::from("/xdg/anolisa/extensions")));
        assert!(!paths
            .system_extensions
            .contains(&PathBuf::from("/xdg/anolisa/extensions")));
        for text in [
            "skill_dir=relative",
            "skill_dir=/opt/../other",
            "profile=other",
            "command=/bin/sh",
        ] {
            assert!(parse_resources(text).is_err(), "{text}");
        }
    }

    #[test]
    fn user_install_is_not_also_a_system_extension_source() {
        let home = Path::new("/home/owner");
        let executable = home.join(".local/libexec/cosh-ng/cosh-core");
        let paths = resource_dirs_for(Some(&executable), Some(home), None, None);
        let extensions = home.join(".local/share/cosh-ng/extensions");
        assert_eq!(
            paths
                .user_extensions
                .iter()
                .filter(|path| *path == &extensions)
                .count(),
            1
        );
        assert!(!paths.system_extensions.contains(&extensions));
    }
}
