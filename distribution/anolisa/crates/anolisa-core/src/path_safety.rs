//! Path-boundary validation shared by install and uninstall.
//!
//! Both verbs need the same guarantee: a path the framework is about to
//! create or remove must live under an ANOLISA-owned root in the current
//! [`FsLayout`]. The check is intentionally double-layered because either
//! layer alone is bypassable:
//!
//!   * **Lexical** rejects `..` / `.` segments and requires `starts_with`
//!     one of the layout's owned roots. Defeats template outputs that
//!     resolve to `<root>/../etc/passwd`.
//!   * **Canonical** walks `path`'s deepest existing ancestor through
//!     `canonicalize` and re-checks containment under canonical roots.
//!     Defeats symlink-in-ancestor escapes (e.g. someone planting
//!     `<bin_dir>/escape -> /etc`).
//!
//! Install uses this before write; uninstall uses it before backup +
//! `remove_file`. Without this symmetry, a forged `installed.toml`
//! claiming `owner = anolisa` for `/etc/shadow` could turn `uninstall`
//! into an arbitrary-delete primitive — install rejects writes to that
//! path, but uninstall would happily walk the path-from-state through
//! to `fs::remove_file`. Sharing this module keeps the two surfaces in
//! lockstep when the rules tighten.

use std::path::{Component, Path, PathBuf};

use anolisa_platform::fs_layout::FsLayout;

/// Reasons a path may be rejected by [`validate_owned_path`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PathBoundaryError {
    /// Path is outside every owned root in the current layout.
    #[error("path '{path}' is not under an ANOLISA-owned root")]
    External {
        /// Rejected path.
        path: PathBuf,
    },

    /// Path contains traversal segments even if it would later canonicalize
    /// back under an owned root.
    #[error("path '{path}' contains a '.' or '..' segment — refusing to operate via traversal")]
    Traversal {
        /// Rejected path.
        path: PathBuf,
    },
}

/// The set of layout-owned roots. Anything else is "third-party" or
/// "filesystem at large" and must not be created or deleted by the
/// framework.
pub fn lexical_roots(layout: &FsLayout) -> Vec<&Path> {
    vec![
        layout.bin_dir.as_path(),
        layout.etc_dir.as_path(),
        layout.state_dir.as_path(),
        layout.lib_dir.as_path(),
        layout.libexec_dir.as_path(),
        layout.datadir.as_path(),
        layout.log_dir.as_path(),
        layout.cache_dir.as_path(),
        // systemd unit dirs are owned so contracts can place units via
        // `{unitdir}`/`{userunitdir}` and uninstall removes them again.
        // In system mode these sit outside the `/usr/local/share/anolisa`
        // tree (`/usr/local/lib/systemd/{system,user}`); in user mode the
        // user-unit dir (`~/.config/systemd/user`) is outside `etc_dir`
        // (`~/.config/anolisa`) — so both must be listed explicitly.
        layout.systemd_unit_dir.as_path(),
        layout.systemd_user_unit_dir.as_path(),
    ]
}

/// Reject `path` unless it lives under one of `layout`'s owned roots,
/// both lexically and after canonicalising the deepest existing
/// ancestor. See module-level docs for why both layers are needed.
pub fn validate_owned_path(layout: &FsLayout, path: &Path) -> Result<(), PathBoundaryError> {
    for component in path.components() {
        if matches!(component, Component::ParentDir | Component::CurDir) {
            return Err(PathBoundaryError::Traversal {
                path: path.to_path_buf(),
            });
        }
    }
    let lex_roots = lexical_roots(layout);
    if !lex_roots.iter().any(|root| path.starts_with(root)) {
        return Err(PathBoundaryError::External {
            path: path.to_path_buf(),
        });
    }
    if let Some(canonical_dest) = canonicalize_nearest_existing(path) {
        let canonical_roots: Vec<PathBuf> = lex_roots
            .iter()
            .filter_map(|r| canonicalize_nearest_existing(r))
            .collect();
        if !canonical_roots.is_empty()
            && !canonical_roots
                .iter()
                .any(|r| canonical_dest.starts_with(r))
        {
            return Err(PathBoundaryError::External {
                path: path.to_path_buf(),
            });
        }
    }
    Ok(())
}

/// Walk up `p`'s ancestors until one exists, canonicalize that, and
/// re-attach the missing tail. Returns `None` only if not even `/` (or
/// the platform equivalent) can be canonicalized — effectively never on
/// the platforms this CLI targets.
pub fn canonicalize_nearest_existing(p: &Path) -> Option<PathBuf> {
    let mut suffix: Vec<std::ffi::OsString> = Vec::new();
    let mut current = p.to_path_buf();
    loop {
        if let Ok(canonical) = current.canonicalize() {
            let mut out = canonical;
            for seg in suffix.iter().rev() {
                out.push(seg);
            }
            return Some(out);
        }
        let name = current.file_name()?.to_os_string();
        suffix.push(name);
        if !current.pop() {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn fixture(prefix: &Path) -> FsLayout {
        FsLayout::system(Some(prefix.to_path_buf()))
    }

    #[test]
    fn rejects_path_outside_all_roots() {
        let tmp = tempdir().unwrap();
        let layout = fixture(tmp.path());
        let escape = PathBuf::from("/etc/shadow");
        let err = validate_owned_path(&layout, &escape).expect_err("must reject");
        assert!(matches!(err, PathBoundaryError::External { .. }));
    }

    #[test]
    fn rejects_traversal_segment_even_under_a_root() {
        let tmp = tempdir().unwrap();
        let layout = fixture(tmp.path());
        let dest = layout.bin_dir.join("..").join("escape");
        let err = validate_owned_path(&layout, &dest).expect_err("must reject");
        assert!(matches!(err, PathBoundaryError::Traversal { .. }));
    }

    #[test]
    fn accepts_clean_path_under_bin_dir() {
        let tmp = tempdir().unwrap();
        let layout = fixture(tmp.path());
        std::fs::create_dir_all(&layout.bin_dir).unwrap();
        let dest = layout.bin_dir.join("agentsight");
        validate_owned_path(&layout, &dest).expect("clean path must accept");
    }

    #[test]
    fn accepts_unit_targets_under_systemd_dirs() {
        // `{unitdir}`/`{userunitdir}` targets must be owned so contracts can
        // place (and uninstall can remove) systemd units.
        let tmp = tempdir().unwrap();
        let layout = fixture(tmp.path());
        std::fs::create_dir_all(&layout.systemd_unit_dir).unwrap();
        std::fs::create_dir_all(&layout.systemd_user_unit_dir).unwrap();
        validate_owned_path(&layout, &layout.systemd_unit_dir.join("agentsight.service"))
            .expect("system unit target must accept");
        validate_owned_path(
            &layout,
            &layout.systemd_user_unit_dir.join("anolisa-memory@.service"),
        )
        .expect("user unit target must accept");
    }

    #[test]
    #[cfg(unix)]
    fn rejects_symlink_in_ancestor_pointing_outside() {
        // bin_dir/escape -> <outside>, dest = bin_dir/escape/x. Lexical
        // starts_with passes (literally under bin_dir), but canonical
        // resolution follows the symlink and the canonical dest escapes.
        let tmp = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let layout = fixture(tmp.path());
        std::fs::create_dir_all(&layout.bin_dir).unwrap();
        let escape_link = layout.bin_dir.join("escape");
        std::os::unix::fs::symlink(outside.path(), &escape_link).unwrap();
        let dest = escape_link.join("x");
        let err = validate_owned_path(&layout, &dest).expect_err("must reject");
        assert!(matches!(err, PathBoundaryError::External { .. }));
    }
}
