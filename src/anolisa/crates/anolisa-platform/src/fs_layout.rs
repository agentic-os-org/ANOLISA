//! Filesystem layout resolution for user-mode vs system-mode installs.
//!
//! `system-mode` strictly follows [FHS 3.0](https://refspecs.linuxfoundation.org/FHS_3.0/fhs/index.html)
//! (binaries under `/usr/local/bin`, state under `/var/lib/anolisa`, etc.);
//! `user-mode` strictly follows systemd [`file-hierarchy(7)`](https://www.freedesktop.org/software/systemd/man/latest/file-hierarchy.html)
//! (`~/.local/bin`, `~/.local/lib`, plus the `~/.config`, `~/.local/share`,
//! `~/.local/state`, `~/.cache` home roots it defines). When a home root's
//! standard environment variable is set it overrides the `$HOME`-based
//! default, and that override is honored. A custom prefix (typically
//! `/opt/<name>`) may be supplied in system-mode to relocate the whole tree.
//!
//! Source of truth: `docs/anolisa/anolisa-cli-design.md` §"Filesystem Layout"
//! (Path Mapping table) and `docs/anolisa/anolisa-cli-launch-spec.md` §8.5
//! (Lock).
//!
//! The [`InstallMode`] enum here mirrors `anolisa_cli::context::InstallMode`
//! but lives in this crate to keep `anolisa-platform` independent of the
//! CLI; the CLI layer converts between the two at the boundary.

use std::path::{Path, PathBuf};

/// Application namespace folder appended to most FHS / file-hierarchy roots.
const NS: &str = "anolisa";
/// Subdirectory under `datadir` where package-owned component contracts
/// are installed (e.g. `{datadir}/components/<component>/component.toml`).
const COMPONENTS_SUBDIR: &str = "components";
/// Subdirectory under `state_dir` where ANOLISA stores its runtime copy of
/// component contracts after install/adopt.
const COMPONENT_MANIFESTS_SUBDIR: &str = "component-manifests";
/// Filename used for both the package-owned component contract and the
/// runtime state snapshot.
const COMPONENT_MANIFEST_FILE: &str = "component.toml";
/// Sidecar provenance file written alongside a contract snapshot.
const PROVENANCE_FILE: &str = "provenance.toml";
/// Audit-log file name written under `log_dir`.
const CENTRAL_LOG_NAME: &str = "central.jsonl";
/// Lock file name written under `state_dir`.
const LOCK_NAME: &str = "lock";
/// Catalog overlay folder name appended to the configuration root.
const OVERLAY_NAME: &str = "manifests";
/// Backup folder name appended to the state root.
const BACKUPS_NAME: &str = "backups";

// ---- System-mode (FHS) defaults -----------------------------------------

/// Default `/usr/local` prefix tree — all system-mode paths derive from
/// these literals and are rebased under [`FsLayout::system`]'s `prefix`.
mod fhs {
    pub const BIN: &str = "/usr/local/bin";
    pub const LIB: &str = "/usr/local/lib/anolisa";
    pub const LIBEXEC: &str = "/usr/local/libexec/anolisa";
    pub const DATADIR: &str = "/usr/local/share/anolisa";
    /// FHS package-managed read-only datadir. RPM/DEB packages install
    /// component contracts and adapter resources here, separate from the
    /// `/usr/local` tree used by raw/tar installs.
    pub const PACKAGE_DATADIR: &str = "/usr/share/anolisa";
    pub const ETC: &str = "/etc/anolisa";
    pub const STATE: &str = "/var/lib/anolisa";
    pub const CACHE: &str = "/var/cache/anolisa";
    pub const LOG: &str = "/var/log/anolisa";
    pub const RUNTIME: &str = "/run/anolisa";
    pub const SYSTEMD_UNITS: &str = "/usr/local/lib/systemd/system";
    /// System-wide *user* unit search dir. Per-user template units
    /// (`foo@.service` driven by `systemctl --user`) install here so every
    /// user's manager can find them — the `/usr/local` analogue of the
    /// distro `/usr/lib/systemd/user`. Distinct from [`SYSTEMD_UNITS`],
    /// which holds system-manager units.
    pub const SYSTEMD_USER_UNITS: &str = "/usr/local/lib/systemd/user";
}

// ---- User-mode (file-hierarchy(7)) leaf names ---------------------------

/// Home-relative leaf names for the user-mode layout per systemd
/// `file-hierarchy(7)`. Each home root below is overridable by its
/// standard environment variable (resolved in [`FsLayout::user`]).
mod fh_user {
    /// `~/.local/bin` — `file-hierarchy(7)` user binaries dir, kept
    /// independent of the data root per design.md L514.
    pub const HOME_LOCAL_BIN: &str = ".local/bin";
    /// `~/.local/lib` — `file-hierarchy(7)` user libraries dir; the
    /// per-application `anolisa/` subtree nests here.
    pub const HOME_LOCAL_LIB: &str = ".local/lib";
    pub const DATA_HOME: &str = ".local/share";
    pub const CONFIG_HOME: &str = ".config";
    pub const STATE_HOME: &str = ".local/state";
    pub const CACHE_HOME: &str = ".cache";

    /// Helper-executable sub-dir. `file-hierarchy(7)` defines no
    /// `~/.local/libexec`, so non-shell helpers live in a subdirectory of
    /// the application's `~/.local/lib/anolisa/` tree.
    pub const LIBEXEC_SUB: &str = "libexec";
    /// Sub-folder inside the state root used as the runtime fallback when
    /// the user runtime directory is unset.
    pub const RUNTIME_FALLBACK_SUB: &str = "runtime";
    /// User-mode systemd unit search dir, relative to the config root.
    pub const SYSTEMD_USER_SUB: &str = "systemd/user";
}

/// Where ANOLISA installs files: user-mode (`file-hierarchy(7)` under
/// `$HOME`) or system-mode (FHS under `/`, redirectable via a prefix).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallMode {
    /// FHS-style installation under `/usr/local`, `/etc`, and `/var`.
    System,
    /// Per-user installation under the `file-hierarchy(7)` home roots
    /// derived from `$HOME`.
    User,
}

/// Resolved filesystem paths for a given install mode.
///
/// All fields are absolute. `lock_file` and `central_log` are
/// individual file paths; everything else is a directory.
///
/// Note on `prefix`: in system-mode it is the install root that rebases
/// every other path. In user-mode it is **not** a single root; it is kept
/// for compatibility and points at the primary install root (i.e.
/// `datadir`, `~/.local/share/anolisa`). User-mode resolves each
/// `file-hierarchy(7)` home root independently — `bin_dir`, `lib_dir`,
/// `etc_dir`, `state_dir`, `cache_dir` each derive from a different home
/// root (`~/.local/bin`, `~/.local/lib`, `~/.config`, `~/.local/state`,
/// `~/.cache`) and are not children of `prefix`.
#[derive(Debug, Clone)]
pub struct FsLayout {
    /// Install scope that selected the path policy.
    pub mode: InstallMode,
    /// Primary install root; in user mode this is the data root, not a
    /// parent of every other home-derived path.
    pub prefix: PathBuf,
    /// Directory where user-invoked binaries are linked or copied.
    pub bin_dir: PathBuf,
    /// Directory for shared runtime libraries owned by ANOLISA.
    pub lib_dir: PathBuf,
    /// Directory for helper executables not intended as direct commands.
    pub libexec_dir: PathBuf,
    /// Shared read-only data root (skills, adapters, packaged catalogs).
    pub datadir: PathBuf,
    /// Configuration root.
    pub etc_dir: PathBuf,
    /// State root — `installed-state.toml` lives here.
    pub state_dir: PathBuf,
    /// Cache root for downloaded artifacts and reusable probe results.
    pub cache_dir: PathBuf,
    /// Log root; [`Self::central_log`] is placed inside it.
    pub log_dir: PathBuf,
    /// Backup root used by transactional uninstall/rollback paths.
    pub backup_dir: PathBuf,
    /// Advisory install lock file shared by mutating operations.
    pub lock_file: PathBuf,
    /// Central JSONL log file consumed by `anolisa logs`.
    pub central_log: PathBuf,
    /// Volatile runtime root for sockets / PID files.
    pub runtime_dir: PathBuf,
    /// Catalog overlay directory for this install mode.
    pub manifests_overlay: PathBuf,
    /// Distribution-specific systemd *system* unit search directory.
    /// Targets system-scope units (`{unitdir}` placeholder).
    pub systemd_unit_dir: PathBuf,
    /// Systemd *user* unit search directory (`{userunitdir}` placeholder).
    /// Targets user-scope template units. In system mode this is the
    /// system-wide user-unit dir (`/usr/local/lib/systemd/user`); in user
    /// mode it coincides with [`systemd_unit_dir`](Self::systemd_unit_dir)
    /// at `~/.config/systemd/user`, since a user install has no
    /// system-manager units.
    pub systemd_user_unit_dir: PathBuf,
}

impl FsLayout {
    /// System (FHS) install layout. `prefix` defaults to `/`.
    ///
    /// When a non-`/` prefix is supplied, every default absolute path
    /// below is rebased under it (so `prefix=/opt/x` yields
    /// `/opt/x/usr/local/bin`, `/opt/x/etc/anolisa`, etc.).
    pub fn system(prefix: Option<PathBuf>) -> Self {
        let prefix = prefix.unwrap_or_else(|| PathBuf::from("/"));
        let rebase = |p: &str| rebase_under(&prefix, p);

        let state_dir = rebase(fhs::STATE);
        let log_dir = rebase(fhs::LOG);
        let backup_dir = state_dir.join(BACKUPS_NAME);
        let lock_file = state_dir.join(LOCK_NAME);
        let central_log = log_dir.join(CENTRAL_LOG_NAME);
        let etc_dir = rebase(fhs::ETC);
        let manifests_overlay = etc_dir.join(OVERLAY_NAME);

        Self {
            mode: InstallMode::System,
            bin_dir: rebase(fhs::BIN),
            lib_dir: rebase(fhs::LIB),
            libexec_dir: rebase(fhs::LIBEXEC),
            datadir: rebase(fhs::DATADIR),
            etc_dir,
            state_dir,
            cache_dir: rebase(fhs::CACHE),
            log_dir,
            backup_dir,
            lock_file,
            central_log,
            runtime_dir: rebase(fhs::RUNTIME),
            manifests_overlay,
            systemd_unit_dir: rebase(fhs::SYSTEMD_UNITS),
            systemd_user_unit_dir: rebase(fhs::SYSTEMD_USER_UNITS),
            prefix,
        }
    }

    /// User (`file-hierarchy(7)`) install layout under `home`. When a home
    /// root's standard environment variable is set, it takes precedence
    /// over the `$HOME`-based default.
    pub fn user(home: PathBuf) -> Self {
        // The home-root overrides are read from their standard environment
        // variables; an unset variable selects the `$HOME`-based default.
        let data_override = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from);
        let config_override = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
        let state_override = std::env::var_os("XDG_STATE_HOME").map(PathBuf::from);
        let cache_override = std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from);
        let runtime_override = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
        Self::user_with_overrides(
            home,
            data_override,
            config_override,
            state_override,
            cache_override,
            runtime_override,
        )
    }

    /// Test-friendly variant of [`Self::user`] that takes the home-root
    /// override directories explicitly instead of reading them from the
    /// process environment.
    ///
    /// Each `Option` is one home-root override; `None` selects the
    /// `$HOME`-based home root that `file-hierarchy(7)` defines.
    pub fn user_with_overrides(
        home: PathBuf,
        data_override: Option<PathBuf>,
        config_override: Option<PathBuf>,
        state_override: Option<PathBuf>,
        cache_override: Option<PathBuf>,
        runtime_override: Option<PathBuf>,
    ) -> Self {
        let data = sanitize_absolute_root(data_override, home.join(fh_user::DATA_HOME));
        let config = sanitize_absolute_root(config_override, home.join(fh_user::CONFIG_HOME));
        let state = sanitize_absolute_root(state_override, home.join(fh_user::STATE_HOME));
        let cache = sanitize_absolute_root(cache_override, home.join(fh_user::CACHE_HOME));

        let datadir = data.join(NS);
        let etc_dir = config.join(NS);
        let state_dir = state.join(NS);
        let cache_dir = cache.join(NS);

        // bin lives at the file-hierarchy(7) `~/.local/bin`, independent of
        // any other home root — see design.md L514 and L530.
        let bin_dir = home.join(fh_user::HOME_LOCAL_BIN);

        // lib lives at the file-hierarchy(7) `~/.local/lib/anolisa`, NOT
        // under the data root. file-hierarchy(7) defines no
        // `~/.local/libexec`, so non-shell helpers nest in a subdirectory
        // of the lib tree.
        let lib_dir = home.join(fh_user::HOME_LOCAL_LIB).join(NS);
        let libexec_dir = lib_dir.join(fh_user::LIBEXEC_SUB);

        // Logs / lock / backups live under the state root so a cache wipe
        // does not destroy audit history — see design.md L519 + L535 and
        // launch-spec §8.5.
        let log_dir = state_dir.clone();
        let backup_dir = state_dir.join(BACKUPS_NAME);
        let lock_file = state_dir.join(LOCK_NAME);
        let central_log = state_dir.join(CENTRAL_LOG_NAME);

        // The user runtime directory is not guaranteed to be set (e.g.
        // headless installs); fall back to a subdir under state_dir per
        // design.md L536 so socket/pid paths still resolve.
        let runtime_dir = runtime_override
            .filter(|r| is_safe_absolute_root(r))
            .map(|r| r.join(NS))
            .unwrap_or_else(|| state_dir.join(fh_user::RUNTIME_FALLBACK_SUB));

        let manifests_overlay = etc_dir.join(OVERLAY_NAME);
        // A user install has no system-manager units, so the system and
        // user unit dirs coincide at `~/.config/systemd/user`.
        let systemd_unit_dir = config.join(fh_user::SYSTEMD_USER_SUB);
        let systemd_user_unit_dir = systemd_unit_dir.clone();

        Self {
            mode: InstallMode::User,
            // "primary install root" for compatibility — points at the
            // shared data root, not at a single rebase prefix.
            prefix: datadir.clone(),
            bin_dir,
            lib_dir,
            libexec_dir,
            datadir,
            etc_dir,
            state_dir,
            cache_dir,
            log_dir,
            backup_dir,
            lock_file,
            central_log,
            runtime_dir,
            manifests_overlay,
            systemd_unit_dir,
            systemd_user_unit_dir,
        }
    }

    /// Package-owned component contract path under this layout's datadir:
    /// `{datadir}/components/<component>/component.toml`.
    ///
    /// RPM packages and raw archives install the ANOLISA component
    /// contract at this location. The path derives from [`Self::datadir`],
    /// so it respects system-mode prefixes and user-mode XDG overrides.
    pub fn contract_path(&self, component: &str) -> PathBuf {
        Self::component_contract_path(&self.datadir, component)
    }

    /// Installed-state snapshot path under this layout's state_dir:
    /// `{state_dir}/component-manifests/<component>/component.toml`.
    ///
    /// ANOLISA copies the resolved contract into this location after
    /// install or adopt. Commands such as `adapter enable` read from here
    /// first, falling back to the package-owned contract when absent.
    pub fn snapshot_path(&self, component: &str) -> PathBuf {
        Self::component_manifest_snapshot_path(&self.state_dir, component)
    }

    /// FHS package-managed read-only datadir (`/usr/share/anolisa`),
    /// rebased under the same prefix as the rest of the system layout.
    ///
    /// RPM/DEB packages install component contracts and adapter resources
    /// under this path. It is separate from [`Self::datadir`]
    /// (`/usr/local/share/anolisa`) because FHS reserves `/usr/local` for
    /// locally-installed software and `/usr/share` for
    /// distribution-packaged read-only data.
    ///
    /// Returns `None` in user mode (user-mode installs never consult the
    /// FHS package tree) or when the result would equal `self.datadir`
    /// (a custom prefix can collapse the two).
    pub fn package_datadir(&self) -> Option<PathBuf> {
        match self.mode {
            InstallMode::System => {
                let path = rebase_under(&self.prefix, fhs::PACKAGE_DATADIR);
                if path != self.datadir {
                    Some(path)
                } else {
                    None
                }
            }
            InstallMode::User => None,
        }
    }

    /// Package-owned component contract path under an arbitrary datadir root.
    ///
    /// Use this when computing candidates across multiple roots; for a
    /// single layout prefer [`Self::contract_path`].
    pub fn component_contract_path(datadir_root: &Path, component: &str) -> PathBuf {
        datadir_root
            .join(COMPONENTS_SUBDIR)
            .join(component)
            .join(COMPONENT_MANIFEST_FILE)
    }

    /// Installed-state snapshot path under an arbitrary state root.
    ///
    /// Use this when computing candidates across multiple roots; for a
    /// single layout prefer [`Self::snapshot_path`].
    pub fn component_manifest_snapshot_path(state_root: &Path, component: &str) -> PathBuf {
        state_root
            .join(COMPONENT_MANIFESTS_SUBDIR)
            .join(component)
            .join(COMPONENT_MANIFEST_FILE)
    }

    /// Provenance sidecar path derived from an existing snapshot path.
    /// Replaces the filename with `provenance.toml`.
    pub fn provenance_path_for_snapshot(snapshot_path: &Path) -> PathBuf {
        snapshot_path.with_file_name(PROVENANCE_FILE)
    }
}

/// Join `path` under `prefix`, stripping the leading `/` so that
/// `Path::join` does not discard the prefix.
fn rebase_under(prefix: &Path, path: &str) -> PathBuf {
    if prefix == Path::new("/") {
        return PathBuf::from(path);
    }
    let stripped = path.strip_prefix('/').unwrap_or(path);
    prefix.join(stripped)
}

fn sanitize_absolute_root(candidate: Option<PathBuf>, fallback: PathBuf) -> PathBuf {
    match candidate {
        Some(path) if is_safe_absolute_root(&path) => path,
        _ => fallback,
    }
}

fn is_safe_absolute_root(path: &Path) -> bool {
    path.is_absolute() && !path.as_os_str().is_empty() && !has_dot_segment(path)
}

fn has_dot_segment(path: &Path) -> bool {
    let raw = path.to_string_lossy();
    raw.split(std::path::MAIN_SEPARATOR)
        .any(|segment| segment == "." || segment == "..")
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- system-mode --------------------------------------------------

    #[test]
    fn system_default_uses_fhs_root() {
        let layout = FsLayout::system(None);
        assert_eq!(layout.mode, InstallMode::System);
        assert_eq!(layout.prefix, PathBuf::from("/"));
        assert_eq!(layout.bin_dir, PathBuf::from("/usr/local/bin"));
        assert_eq!(layout.lib_dir, PathBuf::from("/usr/local/lib/anolisa"));
        assert_eq!(
            layout.libexec_dir,
            PathBuf::from("/usr/local/libexec/anolisa")
        );
        assert_eq!(layout.datadir, PathBuf::from("/usr/local/share/anolisa"));
        assert_eq!(layout.etc_dir, PathBuf::from("/etc/anolisa"));
        assert_eq!(layout.state_dir, PathBuf::from("/var/lib/anolisa"));
        assert_eq!(layout.cache_dir, PathBuf::from("/var/cache/anolisa"));
        assert_eq!(layout.log_dir, PathBuf::from("/var/log/anolisa"));
        assert_eq!(layout.backup_dir, PathBuf::from("/var/lib/anolisa/backups"));
        assert_eq!(layout.runtime_dir, PathBuf::from("/run/anolisa"));
        assert_eq!(
            layout.systemd_unit_dir,
            PathBuf::from("/usr/local/lib/systemd/system")
        );
        assert_eq!(
            layout.systemd_user_unit_dir,
            PathBuf::from("/usr/local/lib/systemd/user")
        );
        assert_eq!(
            layout.central_log,
            PathBuf::from("/var/log/anolisa/central.jsonl")
        );
        assert_eq!(
            layout.manifests_overlay,
            PathBuf::from("/etc/anolisa/manifests")
        );
    }

    #[test]
    fn system_default_lock_under_var_lib() {
        // launch-spec §8.5: system-mode lock lives at
        // /var/lib/anolisa/lock, NOT under /run.
        let layout = FsLayout::system(None);
        assert_eq!(layout.lock_file, PathBuf::from("/var/lib/anolisa/lock"));
    }

    #[test]
    fn system_default_bin_is_usr_local_bin() {
        // design.md L514/L530: system-mode binaries install under
        // /usr/local/bin, NOT /usr/bin.
        let layout = FsLayout::system(None);
        assert_eq!(layout.bin_dir, PathBuf::from("/usr/local/bin"));
    }

    #[test]
    fn system_custom_prefix_rebases_paths() {
        let layout = FsLayout::system(Some(PathBuf::from("/opt/x")));
        assert_eq!(layout.bin_dir, PathBuf::from("/opt/x/usr/local/bin"));
        assert_eq!(layout.etc_dir, PathBuf::from("/opt/x/etc/anolisa"));
        assert_eq!(
            layout.lock_file,
            PathBuf::from("/opt/x/var/lib/anolisa/lock")
        );
        assert_eq!(
            layout.central_log,
            PathBuf::from("/opt/x/var/log/anolisa/central.jsonl")
        );
        assert_eq!(layout.runtime_dir, PathBuf::from("/opt/x/run/anolisa"));
        assert_eq!(
            layout.systemd_unit_dir,
            PathBuf::from("/opt/x/usr/local/lib/systemd/system")
        );
        assert_eq!(
            layout.systemd_user_unit_dir,
            PathBuf::from("/opt/x/usr/local/lib/systemd/user")
        );
    }

    // ---- user-mode ----------------------------------------------------

    fn user_no_overrides(home: &str) -> FsLayout {
        // env-free helper so parallel tests in other crates can't race
        // us by mutating the home-root env vars in their own processes.
        FsLayout::user_with_overrides(PathBuf::from(home), None, None, None, None, None)
    }

    #[test]
    fn user_layout_under_home_with_no_overrides() {
        let layout = user_no_overrides("/tmp/h");
        assert_eq!(layout.mode, InstallMode::User);
        assert_eq!(layout.prefix, PathBuf::from("/tmp/h/.local/share/anolisa"));
        assert_eq!(layout.datadir, PathBuf::from("/tmp/h/.local/share/anolisa"));
        assert_eq!(layout.bin_dir, PathBuf::from("/tmp/h/.local/bin"));
        // file-hierarchy(7): lib/libexec live under ~/.local/lib, NOT
        // under the ~/.local/share data root.
        assert_eq!(layout.lib_dir, PathBuf::from("/tmp/h/.local/lib/anolisa"));
        assert_eq!(
            layout.libexec_dir,
            PathBuf::from("/tmp/h/.local/lib/anolisa/libexec")
        );
        assert!(!layout.lib_dir.starts_with("/tmp/h/.local/share"));
        assert!(!layout.libexec_dir.starts_with("/tmp/h/.local/share"));
        assert_eq!(layout.etc_dir, PathBuf::from("/tmp/h/.config/anolisa"));
        assert_eq!(
            layout.state_dir,
            PathBuf::from("/tmp/h/.local/state/anolisa")
        );
        assert_eq!(layout.cache_dir, PathBuf::from("/tmp/h/.cache/anolisa"));
        assert_eq!(layout.log_dir, PathBuf::from("/tmp/h/.local/state/anolisa"));
        assert_eq!(
            layout.backup_dir,
            PathBuf::from("/tmp/h/.local/state/anolisa/backups")
        );
        assert_eq!(
            layout.lock_file,
            PathBuf::from("/tmp/h/.local/state/anolisa/lock")
        );
        assert_eq!(
            layout.central_log,
            PathBuf::from("/tmp/h/.local/state/anolisa/central.jsonl")
        );
        assert_eq!(
            layout.manifests_overlay,
            PathBuf::from("/tmp/h/.config/anolisa/manifests")
        );
        assert_eq!(
            layout.systemd_unit_dir,
            PathBuf::from("/tmp/h/.config/systemd/user")
        );
        // User mode has no system-manager units: both unit dirs coincide.
        assert_eq!(
            layout.systemd_user_unit_dir,
            PathBuf::from("/tmp/h/.config/systemd/user")
        );
    }

    #[test]
    fn user_layout_honors_explicit_override_dirs() {
        let layout = FsLayout::user_with_overrides(
            PathBuf::from("/tmp/h"),
            Some(PathBuf::from("/data")),
            Some(PathBuf::from("/conf")),
            Some(PathBuf::from("/state")),
            Some(PathBuf::from("/cache")),
            Some(PathBuf::from("/run/user/1000")),
        );
        assert_eq!(layout.prefix, PathBuf::from("/data/anolisa"));
        assert_eq!(layout.datadir, PathBuf::from("/data/anolisa"));
        assert_eq!(layout.etc_dir, PathBuf::from("/conf/anolisa"));
        assert_eq!(layout.state_dir, PathBuf::from("/state/anolisa"));
        assert_eq!(layout.cache_dir, PathBuf::from("/cache/anolisa"));
        assert_eq!(layout.log_dir, PathBuf::from("/state/anolisa"));
        assert_eq!(layout.lock_file, PathBuf::from("/state/anolisa/lock"));
        assert_eq!(layout.runtime_dir, PathBuf::from("/run/user/1000/anolisa"));
        assert_eq!(layout.systemd_unit_dir, PathBuf::from("/conf/systemd/user"));
        assert_eq!(
            layout.systemd_user_unit_dir,
            PathBuf::from("/conf/systemd/user")
        );
        // bin/lib/libexec are HOME-rooted regardless of the data-root
        // override: file-hierarchy(7) places them under ~/.local/bin and
        // ~/.local/lib, decoupled from the data root.
        assert_eq!(layout.bin_dir, PathBuf::from("/tmp/h/.local/bin"));
        assert_eq!(layout.lib_dir, PathBuf::from("/tmp/h/.local/lib/anolisa"));
        assert_eq!(
            layout.libexec_dir,
            PathBuf::from("/tmp/h/.local/lib/anolisa/libexec")
        );
    }

    #[test]
    fn user_layout_ignores_relative_or_traversing_override_dirs() {
        let layout = FsLayout::user_with_overrides(
            PathBuf::from("/tmp/h"),
            Some(PathBuf::from("relative-data")),
            Some(PathBuf::from("/conf/../escape")),
            Some(PathBuf::from("/state/.")),
            Some(PathBuf::from("/cache")),
            Some(PathBuf::from("relative-runtime")),
        );

        assert_eq!(layout.datadir, PathBuf::from("/tmp/h/.local/share/anolisa"));
        assert_eq!(layout.etc_dir, PathBuf::from("/tmp/h/.config/anolisa"));
        assert_eq!(
            layout.state_dir,
            PathBuf::from("/tmp/h/.local/state/anolisa")
        );
        assert_eq!(layout.cache_dir, PathBuf::from("/cache/anolisa"));
        assert_eq!(
            layout.runtime_dir,
            PathBuf::from("/tmp/h/.local/state/anolisa/runtime")
        );
    }

    #[test]
    fn user_log_under_state_root() {
        // design.md L519/L535: audit log lives under the state root so
        // it survives a cache wipe.
        let layout = user_no_overrides("/tmp/h");
        assert_eq!(layout.log_dir, layout.state_dir);
        assert_ne!(layout.log_dir, layout.cache_dir);
        let with_override = FsLayout::user_with_overrides(
            PathBuf::from("/tmp/h"),
            None,
            None,
            Some(PathBuf::from("/state")),
            Some(PathBuf::from("/cache")),
            None,
        );
        assert_eq!(with_override.log_dir, PathBuf::from("/state/anolisa"));
        assert_ne!(with_override.log_dir, with_override.cache_dir);
    }

    #[test]
    fn user_bin_is_local_bin() {
        // bin must be HOME/.local/bin regardless of the data-root
        // override — see design.md L514/L530.
        let layout = FsLayout::user_with_overrides(
            PathBuf::from("/tmp/h"),
            Some(PathBuf::from("/somewhere/else/data")),
            None,
            None,
            None,
            None,
        );
        assert_eq!(layout.bin_dir, PathBuf::from("/tmp/h/.local/bin"));
    }

    #[test]
    fn user_runtime_falls_back_when_runtime_override_unset() {
        // design.md L536 / runtime_dir fallback path.
        let layout = FsLayout::user_with_overrides(
            PathBuf::from("/tmp/h"),
            None,
            None,
            Some(PathBuf::from("/state")),
            None,
            None,
        );
        assert_eq!(layout.runtime_dir, PathBuf::from("/state/anolisa/runtime"));
        // ...and when the runtime override is set, it wins.
        let with_runtime = FsLayout::user_with_overrides(
            PathBuf::from("/tmp/h"),
            None,
            None,
            Some(PathBuf::from("/state")),
            None,
            Some(PathBuf::from("/run/user/42")),
        );
        assert_eq!(
            with_runtime.runtime_dir,
            PathBuf::from("/run/user/42/anolisa")
        );
    }

    // ---- component contract paths ----------------------------------------

    #[test]
    fn system_component_contract_path_derives_from_datadir() {
        let layout = FsLayout::system(None);
        assert_eq!(
            layout.contract_path("sec-core"),
            PathBuf::from("/usr/local/share/anolisa/components/sec-core/component.toml")
        );
    }

    #[test]
    fn system_component_manifest_snapshot_path_derives_from_state_dir() {
        let layout = FsLayout::system(None);
        assert_eq!(
            layout.snapshot_path("sec-core"),
            PathBuf::from("/var/lib/anolisa/component-manifests/sec-core/component.toml")
        );
    }

    #[test]
    fn system_component_contract_path_with_prefix() {
        let layout = FsLayout::system(Some(PathBuf::from("/opt/x")));
        assert_eq!(
            layout.contract_path("tokenless"),
            PathBuf::from("/opt/x/usr/local/share/anolisa/components/tokenless/component.toml")
        );
        assert_eq!(
            layout.snapshot_path("tokenless"),
            PathBuf::from("/opt/x/var/lib/anolisa/component-manifests/tokenless/component.toml")
        );
    }

    #[test]
    fn user_component_contract_path_derives_from_datadir() {
        let layout = user_no_overrides("/tmp/h");
        assert_eq!(
            layout.contract_path("os-skills"),
            PathBuf::from("/tmp/h/.local/share/anolisa/components/os-skills/component.toml")
        );
    }

    #[test]
    fn user_component_manifest_snapshot_path_derives_from_state_dir() {
        let layout = user_no_overrides("/tmp/h");
        assert_eq!(
            layout.snapshot_path("os-skills"),
            PathBuf::from(
                "/tmp/h/.local/state/anolisa/component-manifests/os-skills/component.toml"
            )
        );
    }

    // ---- package_datadir ----------------------------------------------------

    #[test]
    fn system_layout_keeps_local_datadir() {
        let layout = FsLayout::system(None);
        assert_eq!(
            layout.datadir,
            PathBuf::from("/usr/local/share/anolisa"),
            "system datadir must remain /usr/local/share/anolisa for raw/tar installs"
        );
        let pkg = layout.package_datadir();
        assert_eq!(
            pkg,
            Some(PathBuf::from("/usr/share/anolisa")),
            "package_datadir must be /usr/share/anolisa, distinct from datadir"
        );
        assert_ne!(
            layout.datadir,
            pkg.unwrap(),
            "package_datadir must not equal the primary datadir"
        );
    }

    #[test]
    fn package_datadir_respects_prefix() {
        let layout = FsLayout::system(Some(PathBuf::from("/opt/x")));
        assert_eq!(
            layout.datadir,
            PathBuf::from("/opt/x/usr/local/share/anolisa")
        );
        assert_eq!(
            layout.package_datadir(),
            Some(PathBuf::from("/opt/x/usr/share/anolisa"))
        );
    }

    #[test]
    fn package_datadir_is_none_for_user_mode() {
        let layout = user_no_overrides("/tmp/h");
        assert_eq!(
            layout.package_datadir(),
            None,
            "user mode must not expose a package_datadir"
        );
    }

    #[test]
    fn package_contract_does_not_change_raw_install_target() {
        let layout = FsLayout::system(None);
        assert_eq!(
            layout.datadir,
            PathBuf::from("/usr/local/share/anolisa"),
            "raw/system install datadir must remain /usr/local/share/anolisa"
        );
        assert_eq!(
            layout.contract_path("sec-core"),
            PathBuf::from("/usr/local/share/anolisa/components/sec-core/component.toml"),
            "contract_path must derive from the primary (local-install) datadir"
        );
        let pkg = layout.package_datadir().unwrap();
        assert_eq!(
            FsLayout::component_contract_path(&pkg, "sec-core"),
            PathBuf::from("/usr/share/anolisa/components/sec-core/component.toml"),
            "package contract path must be under /usr/share/anolisa"
        );
    }

    #[test]
    fn user_component_contract_path_honors_xdg_overrides() {
        let layout = FsLayout::user_with_overrides(
            PathBuf::from("/tmp/h"),
            Some(PathBuf::from("/data")),
            None,
            Some(PathBuf::from("/state")),
            None,
            None,
        );
        assert_eq!(
            layout.contract_path("sec-core"),
            PathBuf::from("/data/anolisa/components/sec-core/component.toml")
        );
        assert_eq!(
            layout.snapshot_path("sec-core"),
            PathBuf::from("/state/anolisa/component-manifests/sec-core/component.toml")
        );
    }
}
