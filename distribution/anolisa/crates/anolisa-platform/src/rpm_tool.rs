//! Select the native RPM transaction tool without inferring the host package family.

use crate::command::CommandRunner;

/// Native command dialect; a `yum` executable may use either implementation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RpmDialect {
    /// Traditional Yum 3.
    Yum,
    /// DNF 4, including its yum compatibility executable.
    Dnf,
}

/// Selected executable and the command dialect it implements.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RpmTool {
    /// Executable selected once for the operation.
    pub program: &'static str,
    /// Parameter and preflight-output contract.
    pub dialect: RpmDialect,
}

/// Failure to select an executable that implements a supported contract.
#[derive(Debug, thiserror::Error)]
#[error("cannot use {program}: {source}")]
pub struct RpmToolError {
    /// Executable whose probe failed.
    pub program: &'static str,
    /// Spawn failure or unsupported version output.
    #[source]
    pub source: std::io::Error,
}

impl RpmTool {
    /// Prefer yum; only an absent executable permits falling back to dnf.
    pub fn detect(runner: &impl CommandRunner) -> Result<Self, RpmToolError> {
        for program in ["yum", "dnf"] {
            let output = match runner.run(program, &["--version"]) {
                Err(error) if program == "yum" && error.kind() == std::io::ErrorKind::NotFound => {
                    continue;
                }
                result => result.map_err(|source| RpmToolError { program, source })?,
            };
            let major = output
                .stdout
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .split('.')
                .next();
            let dialect = match (output.code, program, major) {
                (Some(0), "yum", Some("3")) => RpmDialect::Yum,
                (Some(0), _, Some("4")) => RpmDialect::Dnf,
                _ => {
                    return Err(RpmToolError {
                        program,
                        source: std::io::Error::other(format!(
                            "unsupported RPM tool version (exit {:?}): {}{}",
                            output.code, output.stdout, output.stderr
                        )),
                    });
                }
            };
            return Ok(Self { program, dialect });
        }
        unreachable!("the final dnf probe returns a tool or an error")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::CommandOutput;
    use std::cell::RefCell;
    struct Probe {
        yum: Result<&'static str, std::io::ErrorKind>,
        calls: RefCell<Vec<String>>,
    }
    impl CommandRunner for Probe {
        fn run(&self, program: &str, args: &[&str]) -> std::io::Result<CommandOutput> {
            assert_eq!(args, ["--version"]);
            self.calls.borrow_mut().push(program.into());
            let version = if program == "yum" {
                self.yum.map_err(std::io::Error::from)?
            } else {
                "4.7.0\n"
            };
            Ok(CommandOutput {
                code: Some(0),
                stdout: version.into(),
                stderr: String::new(),
            })
        }
    }
    #[test]
    fn selects_yum_dialect_and_only_falls_back_on_absence() {
        for (yum, expected, count) in [
            (Ok("3.4.3\n"), Some(("yum", RpmDialect::Yum)), 1),
            (Ok("4.7.0\n"), Some(("yum", RpmDialect::Dnf)), 1),
            (
                Err(std::io::ErrorKind::NotFound),
                Some(("dnf", RpmDialect::Dnf)),
                2,
            ),
            (Err(std::io::ErrorKind::PermissionDenied), None, 1),
            (Ok("unknown\n"), None, 1),
        ] {
            let runner = Probe {
                yum,
                calls: RefCell::new(Vec::new()),
            };
            assert_eq!(
                RpmTool::detect(&runner)
                    .ok()
                    .map(|t| (t.program, t.dialect)),
                expected
            );
            assert_eq!(runner.calls.borrow().len(), count);
        }
    }
}
