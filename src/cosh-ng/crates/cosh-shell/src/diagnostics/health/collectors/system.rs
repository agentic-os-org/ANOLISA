use std::os::unix::fs::MetadataExt;

use super::{run_health_command, HealthCommandConfig, HealthCommandError, HealthCommandOutput};

pub(super) fn system_program(name: &str) -> Result<&'static str, HealthCommandError> {
    let candidates: &[&str] = match name {
        "df" => &["/usr/bin/df", "/bin/df"],
        "journalctl" => &["/usr/bin/journalctl", "/bin/journalctl"],
        "dmesg" => &["/usr/bin/dmesg", "/bin/dmesg", "/sbin/dmesg"],
        "systemctl" => &["/usr/bin/systemctl", "/bin/systemctl"],
        _ => return Err(HealthCommandError::CommandMissing),
    };
    for path in candidates {
        match std::fs::metadata(path) {
            Ok(metadata)
                if metadata.is_file()
                    && metadata.uid() == 0
                    && metadata.mode() & 0o022 == 0
                    && metadata.mode() & 0o111 != 0 =>
            {
                return Ok(path)
            }
            Ok(_) => return Err(HealthCommandError::PermissionDenied),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(HealthCommandError::Io),
        }
    }
    Err(HealthCommandError::CommandMissing)
}

pub(super) fn run_system_command(
    name: &str,
    args: &[&str],
    config: HealthCommandConfig,
) -> Result<HealthCommandOutput, HealthCommandError> {
    run_health_command(system_program(name)?, args, config)
}
