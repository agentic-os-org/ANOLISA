use std::path::{Path, PathBuf};
use std::process::Command;

pub(crate) fn gateway_executable() -> Result<PathBuf, String> {
    #[cfg(target_os = "linux")]
    if let Some(assets) = linux::SESSION_ASSETS.get() {
        return assets
            .gateway
            .as_ref()
            .map(|asset| asset.program.clone())
            .map_err(Clone::clone);
    }
    resolve_gateway_executable()
}

fn resolve_gateway_executable() -> Result<PathBuf, String> {
    if let Some(configured) = std::env::var_os("COSH_GATEWAY_EXECUTABLE") {
        let configured = PathBuf::from(configured);
        if !configured.is_absolute() {
            return Err("COSH_GATEWAY_EXECUTABLE must be absolute".to_owned());
        }
        return Ok(configured);
    }
    if let Ok(current) = std::env::current_exe() {
        if let Some(parent) = current.parent() {
            let sibling = parent.join("cosh-gateway");
            if is_executable_file(&sibling) {
                return Ok(sibling);
            }
        }
    }
    Ok(PathBuf::from("cosh-gateway"))
}

fn is_executable_file(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

pub(crate) fn admit_session_assets(core_program: Option<&str>) {
    #[cfg(target_os = "linux")]
    linux::SESSION_ASSETS.get_or_init(|| linux::SessionAssets {
        core: core_program.map(|program| {
            linux::SelectedExecutable::capture(
                program.into(),
                std::env::var_os("COSH_CORE_PATH").is_some(),
            )
        }),
        gateway: resolve_gateway_executable().map(|program| {
            linux::SelectedExecutable::capture(
                program,
                std::env::var_os("COSH_GATEWAY_EXECUTABLE").is_some(),
            )
        }),
    });
    #[cfg(not(target_os = "linux"))]
    let _ = core_program;
}

pub(super) fn command_for_session_asset(program: &std::ffi::OsStr) -> Command {
    #[cfg(target_os = "linux")]
    if let Some(assets) = linux::SESSION_ASSETS.get() {
        for asset in [assets.core.as_ref(), assets.gateway.as_ref().ok()]
            .into_iter()
            .flatten()
        {
            if asset.program.as_os_str() == program {
                return asset.command();
            }
        }
    }
    Command::new(program)
}

#[cfg(target_os = "linux")]
mod linux {
    use std::fs::{File, OpenOptions};
    use std::io;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt, PermissionsExt};
    use std::os::unix::process::CommandExt;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::OnceLock;

    use nix::libc;

    pub(super) static SESSION_ASSETS: OnceLock<SessionAssets> = OnceLock::new();

    pub(super) struct SessionAssets {
        pub(super) core: Option<SelectedExecutable>,
        pub(super) gateway: Result<SelectedExecutable, String>,
    }

    pub(super) struct SelectedExecutable {
        pub(super) program: PathBuf,
        native: Result<Option<File>, i32>,
    }

    impl SelectedExecutable {
        pub(super) fn capture(program: PathBuf, external: bool) -> Self {
            let native = if program.is_absolute() {
                Self::open_native(&program, external)
                    .map_err(|error| error.raw_os_error().unwrap_or(libc::EIO))
            } else {
                Ok(None)
            };
            Self { program, native }
        }

        fn open_native(program: &PathBuf, external: bool) -> io::Result<Option<File>> {
            let file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(program)?;
            let metadata = file.metadata()?;
            if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
                return Err(io::Error::from_raw_os_error(libc::EACCES));
            }
            let mut magic = [0; 4];
            let length = file.read_at(&mut magic, 0)?;
            if length != 4 || magic != *b"\x7fELF" {
                return if external {
                    Ok(None)
                } else {
                    Err(io::Error::from_raw_os_error(libc::ENOEXEC))
                };
            }
            let named = std::fs::metadata(program)?;
            if (metadata.dev(), metadata.ino()) != (named.dev(), named.ino()) {
                return Err(io::Error::from_raw_os_error(libc::EAGAIN));
            }
            Ok(Some(file))
        }

        pub(super) fn command(&self) -> Command {
            match &self.native {
                Ok(Some(file)) => {
                    let mut command = Command::new(format!("/proc/self/fd/{}", file.as_raw_fd()));
                    command.arg0(&self.program);
                    command
                }
                Ok(None) => Command::new(&self.program),
                Err(errno) => {
                    let mut command = Command::new(&self.program);
                    let errno = *errno;
                    unsafe {
                        command.pre_exec(move || Err(io::Error::from_raw_os_error(errno)));
                    }
                    command
                }
            }
        }
    }
}
