//! Native dependency provisioning for known RPM and DEB package families.

use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::process::{Command, ExitStatus, Stdio};

use crate::command::{CommandRunner, SystemCommandRunner};
use crate::rpm_tool::{RpmDialect, RpmTool};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PkgError {
    #[error("package manager command failed: {0}")]
    CommandFailed(String),
    #[error("unsupported package base: {0}")]
    Unsupported(String),
}

/// Abstraction over system package managers.
pub trait PackageManager {
    fn install(&self, packages: &[&str]) -> Result<(), PkgError>;
    fn remove(&self, packages: &[&str]) -> Result<(), PkgError>;
    fn is_installed(&self, package: &str) -> bool;
    /// Human-readable installation command for this selected executable.
    fn install_hint(&self, packages: &[&str]) -> String {
        format!(
            "install {} with the host package manager",
            packages.join(" ")
        )
    }
}

/// Selected RPM package manager for dependency provisioning.
pub struct RpmBackend {
    tool: RpmTool,
}

/// APT backend for DEB-based distros (Ubuntu, Debian).
pub struct AptBackend;

impl PackageManager for RpmBackend {
    fn install_hint(&self, packages: &[&str]) -> String {
        format!("sudo {} install {}", self.tool.program, packages.join(" "))
    }

    fn install(&self, packages: &[&str]) -> Result<(), PkgError> {
        if packages.is_empty() {
            return Ok(());
        }
        let mut command = Command::new(self.tool.program);
        command.args(["install", "-y"]);
        match self.tool.dialect {
            RpmDialect::Dnf => {
                command.arg("--setopt=install_weak_deps=False");
            }
            RpmDialect::Yum => {
                command.arg("--setopt=skip_missing_names_on_install=false");
            }
        }
        let status = run_with_progress(command.args(packages)).map_err(|e| {
            PkgError::CommandFailed(format!("failed to spawn {}: {e}", self.tool.program))
        })?;
        if !status.success() {
            return Err(PkgError::CommandFailed(format!(
                "{} install exited with {status}",
                self.tool.program
            )));
        }
        Ok(())
    }

    fn remove(&self, packages: &[&str]) -> Result<(), PkgError> {
        if packages.is_empty() {
            return Ok(());
        }
        let status = run_with_progress(
            Command::new(self.tool.program)
                .args(["remove", "-y"])
                .args(packages),
        )
        .map_err(|e| {
            PkgError::CommandFailed(format!("failed to spawn {}: {e}", self.tool.program))
        })?;
        if !status.success() {
            return Err(PkgError::CommandFailed(format!(
                "{} remove exited with {status}",
                self.tool.program
            )));
        }
        Ok(())
    }

    fn is_installed(&self, package: &str) -> bool {
        Command::new("rpm")
            .args(["-q", package])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

impl PackageManager for AptBackend {
    fn install_hint(&self, packages: &[&str]) -> String {
        format!("sudo apt-get install {}", packages.join(" "))
    }

    fn install(&self, packages: &[&str]) -> Result<(), PkgError> {
        if packages.is_empty() {
            return Ok(());
        }
        let status = run_with_progress(
            Command::new("apt-get")
                .args(["install", "-y", "--no-install-recommends"])
                .args(packages)
                .env("DEBIAN_FRONTEND", "noninteractive"),
        )
        .map_err(|e| PkgError::CommandFailed(format!("failed to spawn apt-get: {e}")))?;
        if !status.success() {
            return Err(PkgError::CommandFailed(format!(
                "apt-get install exited with {status}"
            )));
        }
        Ok(())
    }

    fn remove(&self, packages: &[&str]) -> Result<(), PkgError> {
        if packages.is_empty() {
            return Ok(());
        }
        let status = run_with_progress(
            Command::new("apt-get")
                .args(["remove", "-y"])
                .args(packages)
                .env("DEBIAN_FRONTEND", "noninteractive"),
        )
        .map_err(|e| PkgError::CommandFailed(format!("failed to spawn apt-get: {e}")))?;
        if !status.success() {
            return Err(PkgError::CommandFailed(format!(
                "apt-get remove exited with {status}"
            )));
        }
        Ok(())
    }

    fn is_installed(&self, package: &str) -> bool {
        Command::new("dpkg")
            .args(["-s", package])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

fn run_with_progress(command: &mut Command) -> io::Result<ExitStatus> {
    let stderr = io::stderr();
    let progress = stderr.as_fd().try_clone_to_owned()?;
    run_with_progress_to(command, progress)
}

fn run_with_progress_to(command: &mut Command, progress: OwnedFd) -> io::Result<ExitStatus> {
    command
        .stdout(Stdio::from(progress))
        .stderr(Stdio::inherit())
        .status()
}

/// Select a tool only within the already classified host package family.
pub fn detect_package_manager(pkg_base: Option<&str>) -> Result<Box<dyn PackageManager>, PkgError> {
    match pkg_base {
        Some("rpm") => RpmTool::detect(&SystemCommandRunner)
            .map(|tool| Box::new(RpmBackend { tool }) as Box<dyn PackageManager>)
            .map_err(|e| PkgError::CommandFailed(e.to_string())),
        Some("deb") => {
            let output = SystemCommandRunner
                .run("apt-get", &["--version"])
                .map_err(|e| PkgError::CommandFailed(format!("cannot use apt-get: {e}")))?;
            if output.code != Some(0) {
                return Err(PkgError::CommandFailed(format!(
                    "apt-get --version failed: {}{}",
                    output.stdout, output.stderr
                )));
            }
            Ok(Box::new(AptBackend))
        }
        _ => Err(PkgError::Unsupported(
            pkg_base.unwrap_or("unknown").to_string(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::fs::{self, File};
    use std::io::Read;
    use std::path::PathBuf;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use super::*;

    struct Fixture(PathBuf);

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    fn unique_fixture(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("anolisa-pkg-{name}-{}", std::process::id()))
    }

    #[test]
    fn unknown_family_never_selects_a_tool_from_path() {
        for family in [None, Some("unknown"), Some("anolis23")] {
            assert!(matches!(
                detect_package_manager(family),
                Err(PkgError::Unsupported(_))
            ));
        }
    }

    #[cfg(unix)]
    #[test]
    fn package_command_writes_progress_to_supplied_fd() {
        let progress_fixture = Fixture(unique_fixture("progress"));
        let progress_file =
            File::create(&progress_fixture.0).expect("progress fixture must be created");
        let progress: OwnedFd = progress_file.into();
        let mut command = Command::new("sh");
        command.args(["-c", "printf progress-marker"]);

        assert!(
            run_with_progress_to(&mut command, progress)
                .expect("package command must complete")
                .success()
        );
        let mut marker = String::new();
        File::open(&progress_fixture.0)
            .expect("progress fixture must be readable")
            .read_to_string(&mut marker)
            .expect("progress fixture must contain UTF-8 text");

        assert_eq!(marker, "progress-marker");
    }

    #[cfg(unix)]
    #[test]
    fn package_command_does_not_wait_for_background_descendant_holding_progress_fd() {
        let progress = Fixture(unique_fixture("retained-fd"));
        let progress_file = File::create(&progress.0).expect("progress fixture must be created");
        let completion = Fixture(unique_fixture("descendant-complete"));
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "(sleep 1; printf complete > \"$1\") &",
            "sh",
            completion
                .0
                .to_str()
                .expect("completion fixture path must be UTF-8"),
        ]);
        let progress: OwnedFd = progress_file.into();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let result = run_with_progress_to(&mut command, progress);
            let _ = sender.send(result);
        });
        let result = receiver.recv_timeout(Duration::from_millis(500));

        let status = result
            .expect("direct child completion must not depend on its descendant")
            .expect("package command must complete");
        assert!(status.success());
        assert!(!completion.0.exists());

        let deadline = Instant::now() + Duration::from_secs(3);
        while !completion.0.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
        }
        assert_eq!(
            fs::read_to_string(&completion.0)
                .expect("descendant must complete within three seconds"),
            "complete"
        );
    }
}
