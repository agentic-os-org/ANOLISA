//! Process-wide CLI context constructed from global flags.
//!
//! Global flags (`--install-mode`, `--prefix`, `--json`, `--dry-run`,
//! `--verbose`, `--quiet`, `--no-color`) are parsed once on the top-level
//! `Cli` struct, projected into [`CliContext`], and then threaded through
//! every command handler. Handlers must not re-parse globals from the args
//! struct; instead they read from the shared context so that semantics stay
//! consistent across surfaces.
//!
//! When `--install-mode` is omitted, the effective scope is inferred from
//! the process's effective UID: root defaults to system, non-root to user.

use std::path::PathBuf;

use anolisa_platform::fs_layout::FsLayout;
use anolisa_platform::privilege;
use clap::ValueEnum;

use crate::packaged::PackagedDataProbe;

/// Where ANOLISA installs files: user-mode (`file-hierarchy(7)` under `$HOME`)
/// or system-mode (FHS under `/usr/local`, redirectable via `--prefix`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[clap(rename_all = "lowercase")]
pub enum InstallMode {
    User,
    System,
}

impl InstallMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            InstallMode::User => "user",
            InstallMode::System => "system",
        }
    }
}

/// Snapshot of global CLI flags, immutable for the lifetime of the process.
#[derive(Debug, Clone)]
pub struct CliContext {
    pub install_mode: InstallMode,
    pub prefix: Option<PathBuf>,
    pub json: bool,
    pub dry_run: bool,
    pub verbose: bool,
    pub quiet: bool,
    pub no_color: bool,
    layouts: ResolvedLayouts,
    packaged_data_probe: PackagedDataProbe,
}

/// Filesystem layouts resolved once at the process boundary.
#[derive(Debug, Clone)]
pub(crate) struct ResolvedLayouts {
    writable: FsLayout,
    visible_system: FsLayout,
}

impl ResolvedLayouts {
    /// Build an explicit layout snapshot.
    pub(crate) fn new(writable: FsLayout, visible_system: FsLayout) -> Self {
        Self {
            writable,
            visible_system,
        }
    }
}

/// Resolve the effective install mode from the explicit CLI value and a
/// privilege flag.
///
/// When the user passes `--install-mode`, that value wins unconditionally.
/// Otherwise the default is inferred from the process's effective UID:
/// root → [`InstallMode::System`], non-root → [`InstallMode::User`].
fn resolve_install_mode(explicit: Option<InstallMode>, effective_uid: u32) -> InstallMode {
    match explicit {
        Some(mode) => mode,
        None if effective_uid == 0 => InstallMode::System,
        None => InstallMode::User,
    }
}

impl CliContext {
    /// Build a context from the parsed top-level [`crate::commands::Cli`].
    ///
    /// Borrows the CLI so the caller can still consume `cli.command` after.
    /// The effective [`InstallMode`] is inferred from euid when
    /// `--install-mode` is not provided on the command line.
    pub fn from_cli(cli: &crate::commands::Cli) -> Self {
        let effective_uid = privilege::effective_uid();
        let effective_mode = resolve_install_mode(cli.install_mode, effective_uid);
        let visible_system = FsLayout::system(cli.prefix.clone());
        let writable = match effective_mode {
            InstallMode::System => visible_system.clone(),
            InstallMode::User => {
                let home = anolisa_env::EnvService::detect().home;
                FsLayout::user(home)
            }
        };

        Self {
            install_mode: effective_mode,
            prefix: cli.prefix.clone(),
            json: cli.json,
            dry_run: cli.dry_run,
            verbose: cli.verbose,
            quiet: cli.quiet,
            no_color: cli.no_color,
            layouts: ResolvedLayouts::new(writable, visible_system),
            packaged_data_probe: PackagedDataProbe::detect(),
        }
    }

    /// Build a context around already-resolved process inputs.
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_resolved(
        install_mode: InstallMode,
        prefix: Option<PathBuf>,
        json: bool,
        dry_run: bool,
        verbose: bool,
        quiet: bool,
        no_color: bool,
        layouts: ResolvedLayouts,
        packaged_data_probe: PackagedDataProbe,
    ) -> Self {
        Self {
            install_mode,
            prefix,
            json,
            dry_run,
            verbose,
            quiet,
            no_color,
            layouts,
            packaged_data_probe,
        }
    }

    /// Override packaged-data discovery for an isolated test context.
    #[cfg(test)]
    pub(crate) fn with_packaged_data_root(mut self, root: PathBuf) -> Self {
        self.packaged_data_probe = PackagedDataProbe::from_inputs(Some(root), None);
        self
    }

    /// Current invocation's writable filesystem layout.
    pub(crate) fn layout(&self) -> &FsLayout {
        &self.layouts.writable
    }

    /// System layout visible to a user-mode invocation.
    pub(crate) fn visible_system_layout(&self) -> &FsLayout {
        &self.layouts.visible_system
    }

    /// Packaged-data discovery inputs captured at process startup.
    pub(crate) fn packaged_data_probe(&self) -> &PackagedDataProbe {
        &self.packaged_data_probe
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn omitted_as_root_resolves_to_system() {
        assert_eq!(resolve_install_mode(None, 0), InstallMode::System);
    }

    #[test]
    fn omitted_as_non_root_resolves_to_user() {
        assert_eq!(resolve_install_mode(None, 1000), InstallMode::User);
    }

    #[test]
    fn explicit_user_stays_user_even_as_root() {
        assert_eq!(
            resolve_install_mode(Some(InstallMode::User), 0),
            InstallMode::User,
        );
    }

    #[test]
    fn explicit_system_stays_system_even_as_non_root() {
        assert_eq!(
            resolve_install_mode(Some(InstallMode::System), 1000),
            InstallMode::System,
        );
    }
}
