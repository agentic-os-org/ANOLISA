//! Same-directory replacement for configuration files.
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

pub(crate) fn write_config_atomically(path: &Path, content: &str) -> std::io::Result<()> {
    write_with_permissions(path, content, None)
}

#[cfg(feature = "server")]
pub(crate) fn write_private_config_atomically(path: &Path, content: &str) -> std::io::Result<()> {
    #[cfg(unix)]
    let permissions = {
        use std::os::unix::fs::PermissionsExt;
        Some(std::fs::Permissions::from_mode(0o600))
    };
    #[cfg(not(unix))]
    let permissions = None;
    write_with_permissions(path, content, permissions)
}

fn write_with_permissions(
    path: &Path,
    content: &str,
    requested: Option<std::fs::Permissions>,
) -> std::io::Result<()> {
    use std::io::Write;

    static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    let destination = match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => match std::fs::canonicalize(path) {
            Ok(target) => target,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let target = std::fs::read_link(path)?;
                if target.is_absolute() {
                    target
                } else {
                    path.parent().unwrap_or_else(|| Path::new(".")).join(target)
                }
            }
            Err(error) => return Err(error),
        },
        Ok(_) => path.to_path_buf(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => path.to_path_buf(),
        Err(error) => return Err(error),
    };
    let permissions = match requested {
        Some(permissions) => Some(permissions),
        None => match std::fs::metadata(&destination) {
            Ok(metadata) => Some(metadata.permissions()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        },
    };

    for _ in 0..64 {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temporary =
            destination.with_extension(format!("tmp.{}.{}", std::process::id(), sequence));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        if let Some(permissions) = &permissions {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            options.mode(permissions.mode());
        }
        let mut file = match options.open(&temporary) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };

        let write_result = (|| {
            if let Some(permissions) = &permissions {
                file.set_permissions(permissions.clone())?;
            }
            file.write_all(content.as_bytes())?;
            file.sync_all()
        })();
        drop(file);
        // A same-directory rename exposes either complete generation, never
        // the truncate-then-write window to a concurrent trace/serve worker.
        let result = write_result.and_then(|()| std::fs::rename(&temporary, &destination));
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        return result;
    }

    Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists))
}
