use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::process::Command;

const SYSTEM_TELEMETRY_DISABLED_PATH: &str = "/etc/anolisa/.telemetry_disabled";

/// Resolve the path to the `cosh-core` binary under test.
#[allow(dead_code)] // not every integration test binary uses this helper
pub fn binary_path() -> std::path::PathBuf {
    let mut path = std::env::current_exe()
        .expect("current test executable")
        .parent()
        .expect("deps directory")
        .parent()
        .expect("target profile directory")
        .to_path_buf();
    path.push("cosh-core");
    path
}

/// Build a `Command` for the real `cosh-core` binary with telemetry safely
/// disabled by default.
///
/// Sets `HOME` to `home` and creates the per-user opt-out sentinel so the test
/// does not emit telemetry. Tests that intentionally exercise the upload path
/// (e.g. `telemetry_upload.rs`) should construct their own `Command` instead.
#[allow(dead_code)] // not every integration test binary uses this helper
pub fn cosh_core_command(home: &Path) -> Command {
    let mut command = Command::new(binary_path());
    command.env("HOME", home);
    opt_out_telemetry(&mut command, home);
    command
}

/// Disable telemetry for a binary test process.
///
/// Creates a per-user opt-out sentinel under `home` and redirects the user
/// sentinel path to it so the test does not emit telemetry.
#[allow(dead_code)] // used by whichever integration tests include this module
pub fn opt_out_telemetry(command: &mut Command, home: &Path) {
    let dir = home.join(".copilot-shell");
    fs::create_dir_all(&dir).expect("create .copilot-shell");

    let user_sentinel = dir.join("telemetry_disabled");
    fs::write(&user_sentinel, "").expect("create user telemetry opt-out sentinel");
    command.env("COSH_TELEMETRY_DISABLED_PATH", &user_sentinel);
}

/// Returns true when the host's fixed system-level opt-out is present or
/// inaccessible. Tests that require enabled telemetry must not bypass it.
#[allow(dead_code)] // used only by integration targets that enable telemetry
pub fn system_telemetry_is_disabled() -> bool {
    match fs::symlink_metadata(SYSTEM_TELEMETRY_DISABLED_PATH) {
        Ok(_) => true,
        Err(error) if error.kind() == ErrorKind::NotFound => false,
        Err(_) => true,
    }
}
