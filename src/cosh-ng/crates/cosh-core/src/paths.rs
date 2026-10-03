//! Shared ANOLISA data roots for installed skills and extensions.

use std::path::{Path, PathBuf};

/// Returns raw and package-managed data roots for the running installation.
/// Installed binaries follow their FHS libexec prefix; development binaries
/// and user installations use the host system roots.
pub(crate) fn system_data_dirs() -> Vec<PathBuf> {
    system_data_dirs_for_executable(std::env::current_exe().ok().as_deref())
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
    if executable.file_name()? != "cosh-core"
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

    #[test]
    fn system_data_roots_default_to_host() {
        for executable in [
            None,
            Some("/usr/local/libexec/anolisa/cosh-ng/cosh-core"),
            Some("/usr/libexec/anolisa/cosh-ng/cosh-core"),
            Some("/home/user/.local/lib/anolisa/libexec/cosh-ng/cosh-core"),
            Some("/work/target/debug/cosh-core"),
            Some("/opt/anolisa/usr/local/libexec/other/cosh-ng/cosh-core"),
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
