//! Shared ANOLISA data roots for installed skills and extensions.

use std::path::{Path, PathBuf};

/// Returns raw and package-managed data roots for the running installation.
/// Installed binaries follow their FHS libexec prefix; development binaries
/// and user installations use the host system roots.
pub(crate) fn system_data_dirs() -> Vec<PathBuf> {
    system_data_dirs_for_executable(std::env::current_exe().ok().as_deref())
}

/// Returns component-owned resource roots for standalone file installations.
pub(crate) fn component_data_dirs() -> Vec<PathBuf> {
    component_data_dirs_for_executable(std::env::current_exe().ok().as_deref())
}

fn component_data_dirs_for_executable(executable: Option<&Path>) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(directory) = executable.and_then(Path::parent) {
        if directory.file_name().is_some_and(|name| name == "cosh-ng") {
            if let Some(libexec) = directory
                .parent()
                .filter(|path| path.file_name().is_some_and(|name| name == "libexec"))
            {
                if let Some(prefix) = libexec.parent() {
                    roots.push(prefix.join("share/cosh-ng"));
                }
            }
        }
    }
    for root in [
        PathBuf::from("/usr/local/share/cosh-ng"),
        PathBuf::from("/usr/share/cosh-ng"),
    ] {
        if !roots.contains(&root) {
            roots.push(root);
        }
    }
    roots
}

fn system_data_dirs_for_executable(executable: Option<&Path>) -> Vec<PathBuf> {
    let prefix = executable
        .and_then(anolisa_prefix_from_executable)
        .unwrap_or_else(|| Path::new("/"));
    vec![
        prefix.join("usr/local/share/anolisa"),
        prefix.join("usr/share/anolisa"),
    ]
}

fn anolisa_prefix_from_executable(executable: &Path) -> Option<&Path> {
    let cosh_ng_dir = executable.parent()?;
    let anolisa_dir = cosh_ng_dir.parent()?;
    let libexec_dir = anolisa_dir.parent()?;
    let name = executable.file_name()?;
    // Linux marks an unlinked executable this way even when launched through its pinned descriptor.
    let core = name == "cosh-core" || (cfg!(target_os = "linux") && name == "cosh-core (deleted)");
    if !core
        || cosh_ng_dir.file_name()? != "cosh-ng"
        || anolisa_dir.file_name()? != "anolisa"
        || libexec_dir.file_name()? != "libexec"
    {
        return None;
    }

    let fhs_tree = libexec_dir.parent()?;
    if fhs_tree.ends_with("usr/local") {
        fhs_tree.parent()?.parent()
    } else if fhs_tree.ends_with("usr") {
        fhs_tree.parent()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standalone_resources_follow_the_installed_libexec_prefix() {
        assert_eq!(
            component_data_dirs_for_executable(Some(Path::new(
                "/opt/owner/libexec/cosh-ng/cosh-core"
            )))[0],
            PathBuf::from("/opt/owner/share/cosh-ng")
        );
        assert_eq!(
            component_data_dirs_for_executable(Some(Path::new(
                "/usr/local/libexec/cosh-ng/cosh-core"
            )))
            .len(),
            2
        );
    }

    #[test]
    fn system_data_roots_follow_installation_prefix() {
        for executable in [
            "/opt/anolisa/usr/local/libexec/anolisa/cosh-ng/cosh-core",
            "/opt/anolisa/usr/libexec/anolisa/cosh-ng/cosh-core",
        ] {
            assert_eq!(
                system_data_dirs_for_executable(Some(Path::new(executable))),
                vec![
                    PathBuf::from("/opt/anolisa/usr/local/share/anolisa"),
                    PathBuf::from("/opt/anolisa/usr/share/anolisa"),
                ]
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn system_data_roots_follow_unlinked_installation() {
        for executable in [
            "/opt/anolisa/usr/local/libexec/anolisa/cosh-ng/cosh-core (deleted)",
            "/opt/anolisa/usr/libexec/anolisa/cosh-ng/cosh-core (deleted)",
        ] {
            assert_eq!(
                system_data_dirs_for_executable(Some(Path::new(executable))),
                vec![
                    PathBuf::from("/opt/anolisa/usr/local/share/anolisa"),
                    PathBuf::from("/opt/anolisa/usr/share/anolisa"),
                ],
            );
        }
    }

    #[test]
    fn system_data_roots_default_to_host() {
        for executable in [
            None,
            Some("/usr/local/libexec/anolisa/cosh-ng/cosh-core"),
            Some("/usr/libexec/anolisa/cosh-ng/cosh-core"),
            Some("/home/user/.local/lib/anolisa/libexec/cosh-ng/cosh-core"),
            Some("/work/target/debug/cosh-core"),
            Some("/opt/anolisa/usr/local/libexec/other/cosh-ng/cosh-core"),
            Some("/opt/anolisa/usr/libexec/anolisa/cosh-ng/not-core (deleted)"),
        ] {
            assert_eq!(
                system_data_dirs_for_executable(executable.map(Path::new)),
                vec![
                    PathBuf::from("/usr/local/share/anolisa"),
                    PathBuf::from("/usr/share/anolisa"),
                ]
            );
        }
    }
}
