//! Shared "where does the packaged datadir live?" helper.
//!
//! `install-anolisa.sh` (P1-A) lays down the packaged tree at
//! `${ANOLISA_PREFIX}/share/anolisa/`. The CLI needs to find that tree
//! at runtime so commands like `enable agent-observability --dry-run`
//! work without a source tree, an overlay, or matching `--install-mode`
//! to the install prefix.
//!
//! Lookup order (first existing directory wins):
//!
//!   1. `$ANOLISA_DATA_DIR` — explicit caller override. Set by smoke
//!      harnesses that stage anolisa under a tmpdir and need the binary
//!      to ignore the FHS default.
//!   2. `<exe-parent>/../share/anolisa/` — FHS sibling of the binary's
//!      bin/ directory. This is the canonical location after
//!      `install-anolisa.sh`: a binary at `/usr/local/bin/anolisa`
//!      finds its datadir at `/usr/local/share/anolisa/`, regardless of
//!      `--install-mode`.
//!   3. The install-mode default `layout.datadir` — what the
//!      [`FsLayout`] resolution returns for the current
//!      `--install-mode` (system: `/usr/local/share/anolisa`; user:
//!      `~/.local/share/anolisa`). Kept as the final fallback so
//!      pre-P1-A installs (where the datadir matches the install-mode
//!      root directly) still resolve.
//!
//! `cargo run` from the source tree falls through every probe (the
//! debug binary lives under `target/debug/` which has no sibling
//! `share/anolisa/`), at which point the dev-tree fallback in
//! [`crate::commands::common`] takes
//! over. That dev-tree fallback is the reason this helper returns
//! `Option<PathBuf>` rather than panicking.

use std::path::{Path, PathBuf};

use anolisa_platform::fs_layout::FsLayout;

/// Name of the env var that overrides the packaged datadir lookup.
pub const DATA_DIR_ENV: &str = "ANOLISA_DATA_DIR";

/// Process inputs used to locate the packaged `share/anolisa/` tree.
///
/// Capture these once at the CLI boundary so command execution and tests do
/// not observe process-global environment changes partway through a run.
#[derive(Debug, Clone, Default)]
pub(crate) struct PackagedDataProbe {
    env_override: Option<PathBuf>,
    executable: Option<PathBuf>,
}

impl PackagedDataProbe {
    /// Capture the packaged-data inputs from the current process.
    pub(crate) fn detect() -> Self {
        Self {
            env_override: std::env::var_os(DATA_DIR_ENV).map(PathBuf::from),
            executable: std::env::current_exe().ok(),
        }
    }

    /// Build a probe from explicit inputs.
    #[cfg(test)]
    pub(crate) fn from_inputs(env_override: Option<PathBuf>, executable: Option<PathBuf>) -> Self {
        Self {
            env_override,
            executable,
        }
    }

    /// Resolve the first existing packaged-data candidate for `layout`.
    pub(crate) fn resolve(&self, layout: &FsLayout) -> Option<PathBuf> {
        if let Some(candidate) = self.env_override.as_deref()
            && candidate.is_dir()
        {
            return Some(candidate.to_path_buf());
        }
        if let Some(prefix) = self
            .executable
            .as_deref()
            .and_then(Path::parent)
            .and_then(Path::parent)
        {
            let candidate = prefix.join("share").join("anolisa");
            if candidate.is_dir() {
                return Some(candidate);
            }
        }
        layout.datadir.is_dir().then(|| layout.datadir.clone())
    }
}

/// Discover the packaged `share/anolisa/` root for the running binary.
///
/// Returns `None` when none of the three lookup steps point at an
/// existing directory. Callers must fall back to whatever non-packaged
/// source they care about, such as dev-tree manifests. This helper
/// deliberately does NOT consult those because it lives in a separate concern.
pub fn packaged_datadir_root(layout: &FsLayout, probe: &PackagedDataProbe) -> Option<PathBuf> {
    probe.resolve(layout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    /// `ANOLISA_DATA_DIR` takes precedence over every other probe.
    #[test]
    fn env_override_wins() {
        let tmp = tempdir().expect("tmp");
        let layout = FsLayout::system(Some(PathBuf::from("/nonexistent-anolisa-prefix")));
        let probe = PackagedDataProbe::from_inputs(Some(tmp.path().to_path_buf()), None);
        let got = packaged_datadir_root(&layout, &probe);
        assert_eq!(got.as_deref(), Some(tmp.path()));
    }

    /// When `ANOLISA_DATA_DIR` points at a path that does not exist,
    /// we fall through to the next probe.
    #[test]
    fn env_override_falls_through_when_missing() {
        let layout = FsLayout::system(Some(PathBuf::from("/nonexistent-anolisa-prefix")));
        let probe = PackagedDataProbe::from_inputs(
            Some(PathBuf::from("/definitely/does/not/exist/anolisa")),
            None,
        );
        let got = packaged_datadir_root(&layout, &probe);
        assert!(got.is_none(), "expected fallthrough, got {got:?}");
    }

    #[test]
    fn executable_sibling_precedes_layout_datadir() {
        let tmp = tempdir().expect("tmp");
        let executable = tmp.path().join("prefix/bin/anolisa");
        let packaged = tmp.path().join("prefix/share/anolisa");
        fs::create_dir_all(executable.parent().expect("bin parent")).expect("mkdir bin");
        fs::create_dir_all(&packaged).expect("mkdir packaged");
        let layout = FsLayout::system(Some(tmp.path().join("layout")));
        fs::create_dir_all(&layout.datadir).expect("mkdir layout datadir");
        let probe = PackagedDataProbe::from_inputs(None, Some(executable));

        let got = packaged_datadir_root(&layout, &probe);

        assert_eq!(got.as_deref(), Some(packaged.as_path()));
    }

    /// Without env override, an existing layout.datadir wins over a
    /// missing exe-sibling probe.
    #[test]
    fn layout_datadir_used_when_it_exists() {
        let tmp = tempdir().expect("tmp");
        let prefix = tmp.path().to_path_buf();
        let layout = FsLayout::system(Some(prefix.clone()));
        fs::create_dir_all(&layout.datadir).expect("mkdir datadir");
        let probe = PackagedDataProbe::from_inputs(None, None);
        let got = packaged_datadir_root(&layout, &probe);
        assert_eq!(got.as_deref(), Some(layout.datadir.as_path()));
    }
}
