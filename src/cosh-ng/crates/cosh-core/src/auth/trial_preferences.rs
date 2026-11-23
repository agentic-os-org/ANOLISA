//! Current-user ECS trial preference storage without credentials or identity data.

use std::fmt;
use std::fs::{File, Permissions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;
use std::time::{Duration, Instant};

use fs2::FileExt;
use rustix::fs::{mkdirat, open, openat, renameat, unlinkat, AtFlags, Mode, OFlags};

const FILE_NAME: &str = "preferences.toml";
const LOCK_NAME: &str = "preferences.lock";
const MAX_BYTES: u64 = 64 * 1024;
const LOCK_TIMEOUT: Duration = Duration::from_secs(2);

/// Stable preference errors contain no paths, file contents, or upstream errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Error {
    HomeUnavailable,
    Invalid,
    Version,
    UnsafePath,
    Busy,
    Io,
    Worker,
}

impl Error {
    /// Returns the stable, non-sensitive registry error code.
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::HomeUnavailable => "trial_preferences_home_unavailable",
            Self::Invalid => "trial_preferences_invalid",
            Self::Version => "trial_preferences_version",
            Self::UnsafePath => "trial_preferences_unsafe_path",
            Self::Busy => "trial_preferences_busy",
            Self::Io => "trial_preferences_io",
            Self::Worker => "trial_preferences_worker",
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code())
    }
}

impl std::error::Error for Error {}

/// Loads automatic trial use; callers must fail closed on any error.
pub(crate) fn load() -> Result<bool, Error> {
    let Some(directory) = preference_directory(false)? else {
        return Ok(true);
    };
    let _lock = lock(&directory)?;
    let document = read_document(&directory)?;
    auto_use(&document)
}

/// Saves only automatic trial use, preserving unknown TOML fields.
pub(crate) fn save(value: bool) -> Result<(), Error> {
    let directory = preference_directory(true)?.ok_or(Error::Io)?;
    let _lock = lock(&directory)?;
    let mut document = read_document(&directory)?;
    auto_use(&document)?;
    let table = document.as_table_mut().ok_or(Error::Invalid)?;
    table
        .entry("ecs_trial")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .ok_or(Error::Invalid)?
        .insert("auto_use".into(), toml::Value::Boolean(value));
    let content = toml::to_string(&document).map_err(|_| Error::Invalid)?;
    if content.len() as u64 > MAX_BYTES {
        return Err(Error::Invalid);
    }
    let temporary = format!(".preferences.{}.tmp", uuid::Uuid::new_v4());
    let result = (|| {
        let mut file = open_file(
            &directory,
            &temporary,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL,
        )?;
        file.set_permissions(Permissions::from_mode(0o600))
            .map_err(|_| Error::Io)?;
        file.write_all(content.as_bytes()).map_err(|_| Error::Io)?;
        file.sync_all().map_err(|_| Error::Io)?;
        renameat(&directory, temporary.as_str(), &directory, FILE_NAME).map_err(fs_error)?;
        directory.sync_all().map_err(|_| Error::Io)
    })();
    if result.is_err() {
        let _ = unlinkat(&directory, temporary.as_str(), AtFlags::empty());
    }
    result
}

fn fs_error(error: rustix::io::Errno) -> Error {
    match error {
        rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR => Error::UnsafePath,
        _ => Error::Io,
    }
}

fn preference_directory(create: bool) -> Result<Option<File>, Error> {
    let home = std::env::var_os("HOME").ok_or(Error::HomeUnavailable)?;
    let home = Path::new(&home);
    if !home.is_absolute() {
        return Err(Error::HomeUnavailable);
    }
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW;
    let mut directory = File::from(open(home, flags, Mode::empty()).map_err(fs_error)?);
    if directory.metadata().map_err(|_| Error::Io)?.uid() != nix::unistd::geteuid().as_raw() {
        return Err(Error::UnsafePath);
    }
    for name in [".copilot-shell", "cosh"] {
        let descriptor = match openat(&directory, name, flags, Mode::empty()) {
            Ok(descriptor) => descriptor,
            Err(rustix::io::Errno::NOENT) if !create => return Ok(None),
            Err(rustix::io::Errno::NOENT) => {
                match mkdirat(&directory, name, Mode::from_bits_truncate(0o700)) {
                    Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                    Err(error) => return Err(fs_error(error)),
                }
                openat(&directory, name, flags, Mode::empty()).map_err(fs_error)?
            }
            Err(error) => return Err(fs_error(error)),
        };
        directory = File::from(descriptor);
        let metadata = directory.metadata().map_err(|_| Error::Io)?;
        if metadata.uid() != nix::unistd::geteuid().as_raw() || metadata.mode() & 0o022 != 0 {
            return Err(Error::UnsafePath);
        }
        if create {
            if metadata.mode() & 0o700 != 0o700 {
                return Err(Error::Io);
            }
            directory
                .set_permissions(Permissions::from_mode(0o700))
                .map_err(|_| Error::Io)?;
        }
    }
    Ok(Some(directory))
}

fn open_file(directory: &File, name: &str, flags: OFlags) -> Result<File, Error> {
    let descriptor = openat(
        directory,
        name,
        flags | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::from_bits_truncate(0o600),
    )
    .map_err(fs_error)?;
    validate_file(File::from(descriptor))
}

fn validate_file(file: File) -> Result<File, Error> {
    let metadata = file.metadata().map_err(|_| Error::Io)?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.uid() != nix::unistd::geteuid().as_raw()
        || metadata.mode() & 0o022 != 0
    {
        return Err(Error::UnsafePath);
    }
    Ok(file)
}

fn lock(directory: &File) -> Result<File, Error> {
    let file = match openat(
        directory,
        LOCK_NAME,
        OFlags::RDWR
            | OFlags::CREATE
            | OFlags::EXCL
            | OFlags::CLOEXEC
            | OFlags::NOFOLLOW
            | OFlags::NONBLOCK,
        Mode::from_bits_truncate(0o600),
    ) {
        Ok(descriptor) => validate_file(File::from(descriptor))?,
        Err(rustix::io::Errno::EXIST) => open_file(directory, LOCK_NAME, OFlags::RDWR)?,
        Err(error) => return Err(fs_error(error)),
    };
    file.set_permissions(Permissions::from_mode(0o600))
        .map_err(|_| Error::Io)?;
    let deadline = Instant::now() + LOCK_TIMEOUT;
    loop {
        match FileExt::try_lock_exclusive(&file) {
            Ok(()) => return Ok(file),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(Error::Busy);
                }
                std::thread::sleep(Duration::from_millis(10).min(remaining));
            }
            Err(_) => return Err(Error::Io),
        }
    }
}

fn read_document(directory: &File) -> Result<toml::Value, Error> {
    let descriptor = match openat(
        directory,
        FILE_NAME,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::empty(),
    ) {
        Ok(descriptor) => descriptor,
        Err(rustix::io::Errno::NOENT) => {
            let mut table = toml::Table::new();
            table.insert("version".into(), toml::Value::Integer(1));
            return Ok(toml::Value::Table(table));
        }
        Err(error) => return Err(fs_error(error)),
    };
    let file = validate_file(File::from(descriptor))?;
    let mut content = String::new();
    file.take(MAX_BYTES + 1)
        .read_to_string(&mut content)
        .map_err(|_| Error::Invalid)?;
    if content.len() as u64 > MAX_BYTES {
        return Err(Error::Invalid);
    }
    let document: toml::Value = content.parse().map_err(|_| Error::Invalid)?;
    if document.get("version").and_then(toml::Value::as_integer) != Some(1) {
        return Err(Error::Version);
    }
    Ok(document)
}

fn auto_use(document: &toml::Value) -> Result<bool, Error> {
    let Some(trial) = document.get("ecs_trial") else {
        return Ok(true);
    };
    let trial = trial.as_table().ok_or(Error::Invalid)?;
    match trial.get("auto_use") {
        None => Ok(true),
        Some(value) => value.as_bool().ok_or(Error::Invalid),
    }
}
