//! RPM native-transaction backend for [`PackageTransaction`].
//!
//! Runs yum/dnf transactions (`install`/`update`/`reinstall`/`remove`) through the injectable
//! [`CommandRunner`] so the transaction can be tested with a fake runner
//! instead of a live package manager. Command dialects and repository constraints live here;
//! privilege checks and state refresh stay in the CLI consumer.

use crate::command::{CommandRunner, SystemCommandRunner};
use crate::pkg_transaction::{PackageTransaction, PackageTransactionError};
use crate::rpm_repo::RpmRepoSource;
use crate::rpm_tool::{RpmDialect, RpmTool};
use std::cell::OnceCell;
use std::io::Write;

#[cfg(test)]
const DNF: &str = "dnf";

/// RPM implementation of [`PackageTransaction`].
///
/// Generic over the [`CommandRunner`] so tests can inject a fake; production
/// code uses [`RpmTransaction::system`]. The default type parameter keeps
/// production call sites parameter-free while staying zero-cost.
pub struct RpmTransaction<R: CommandRunner = SystemCommandRunner> {
    runner: R,
    repo: Option<RpmRepoSource>,
    tool: OnceCell<RpmTool>,
}

impl RpmTransaction<SystemCommandRunner> {
    /// Select yum/dnf lazily when the first transaction is requested.
    pub fn system() -> Self {
        Self {
            runner: SystemCommandRunner,
            repo: None,
            tool: OnceCell::new(),
        }
    }

    /// Select yum/dnf lazily and constrain targets to an explicit repository.
    pub fn system_with_repo(repo: RpmRepoSource) -> Self {
        Self {
            runner: SystemCommandRunner,
            repo: Some(repo),
            tool: OnceCell::new(),
        }
    }
}

impl<R: CommandRunner> RpmTransaction<R> {
    /// Build a DNF transaction backed by a custom runner (primarily for tests).
    pub fn with_runner(runner: R) -> Self {
        Self::with_tool(
            runner,
            RpmTool {
                program: "dnf",
                dialect: RpmDialect::Dnf,
            },
            None,
        )
    }

    /// Build a DNF transaction backed by a custom runner and explicit repo.
    pub fn with_runner_and_repo(runner: R, repo: RpmRepoSource) -> Self {
        Self::with_tool(
            runner,
            RpmTool {
                program: "dnf",
                dialect: RpmDialect::Dnf,
            },
            Some(repo),
        )
    }

    /// Use an explicitly selected tool for the entire transaction lifecycle.
    pub fn with_tool(runner: R, tool: RpmTool, repo: Option<RpmRepoSource>) -> Self {
        Self {
            runner,
            repo,
            tool: OnceCell::from(tool),
        }
    }

    fn selected_tool(&self) -> Result<RpmTool, PackageTransactionError> {
        if let Some(tool) = self.tool.get() {
            return Ok(*tool);
        }
        let tool = RpmTool::detect(&self.runner)
            .map_err(|e| map_spawn_error(e.source, e.program, "detect"))?;
        Ok(*self.tool.get_or_init(|| tool))
    }

    fn args(
        &self,
        tool: RpmTool,
        verb: &str,
        packages: &[&str],
    ) -> Result<(Vec<String>, Option<tempfile::NamedTempFile>), PackageTransactionError> {
        if tool.dialect == RpmDialect::Dnf {
            return Ok((self.dnf_args(verb, packages), None));
        }
        let mut config = None;
        let mut args = vec![
            "-y".into(),
            "--setopt=skip_missing_names_on_install=false".into(),
            "--setopt=skip_missing_names_on_update=false".into(),
        ];
        if let Some(repo) = self.repo.as_ref().filter(|_| verb != "remove") {
            let file = (|| -> std::io::Result<tempfile::NamedTempFile> {
                // Values become INI syntax; reject line injection at this boundary.
                if repo.id().contains(['\r', '\n', '[', ']'])
                    || repo.base_url().contains(['\r', '\n'])
                {
                    return Err(std::io::Error::other(
                        "invalid temporary RPM repository configuration",
                    ));
                }
                let mut file = tempfile::NamedTempFile::new()?;
                writeln!(
                    file,
                    "include=file:///etc/yum.conf\n[{}]\nname={}\nbaseurl={}\nenabled=1",
                    repo.id(),
                    repo.id(),
                    repo.base_url()
                )?;
                if let Some(check) = repo.gpgcheck() {
                    writeln!(file, "gpgcheck={}", u8::from(check))?;
                }
                file.flush()?;
                Ok(file)
            })()
            .map_err(|e| map_spawn_error(e, tool.program, verb))?;
            args.extend([
                "-c".into(),
                file.path().to_string_lossy().into_owned(),
                "repository-packages".into(),
                repo.id().into(),
                match verb {
                    "update" => "upgrade-to",
                    "reinstall" => "reinstall-available",
                    other => other,
                }
                .into(),
            ]);
            config = Some(file);
        } else {
            args.push(verb.into());
        }
        if verb == "reinstall" {
            // Pin every installed instance so repair cannot become an update.
            let query = crate::rpm_query::RpmPackageQuery::with_runner(RunnerRef(&self.runner));
            use crate::pkg_query::PackageQuery;
            for package in packages {
                let installed = query
                    .query_installed(package)
                    .map_err(|e| PackageTransactionError::TransactionFailed {
                        command: "rpm".into(),
                        operation: "reinstall".into(),
                        code: None,
                        stderr: e.to_string(),
                    })?
                    .ok_or_else(|| PackageTransactionError::TransactionFailed {
                        command: "rpm".into(),
                        operation: "reinstall".into(),
                        code: None,
                        stderr: format!("package {package} is not installed"),
                    })?;
                args.push(crate::rpm_select::nevra(&installed));
            }
        } else {
            args.extend(packages.iter().map(|p| (*p).into()));
        }
        Ok((args, config))
    }

    /// Run a non-interactive native transaction and classify the outcome.
    ///
    /// Shared by [`install`](PackageTransaction::install),
    /// [`update`](PackageTransaction::update),
    /// [`reinstall`](PackageTransaction::reinstall), and
    /// [`remove`](PackageTransaction::remove) since they differ only in the
    /// dnf verb; `verb` is echoed into the [`TransactionFailed`] operation so
    /// the caller can tell which transaction failed. All packages go into a
    /// single dnf invocation, so the solver resolves the whole set at once
    /// and the transaction commits or fails as a unit.
    fn run_native(&self, verb: &str, packages: &[&str]) -> Result<(), PackageTransactionError> {
        // `-y` is required: ANOLISA orchestrates the lifecycle non-interactively,
        // so there is no TTY to answer dnf's confirmation prompt.
        let tool = self.selected_tool()?;
        // Yum's ordinary scoped upgrade can still substitute a newer host-repo build.
        // upgrade-to with exact NEVRAs keeps the target source constrained.
        let mut targets = Vec::new();
        let target_refs;
        let packages = if tool.dialect == RpmDialect::Yum
            && verb == "update"
            && let Some(repo) = &self.repo
        {
            use crate::pkg_query::{PackageQuery, rpm_evr_cmp};
            let query = crate::rpm_query::RpmPackageQuery::with_runner_and_repo(
                RunnerRef(&self.runner),
                repo.clone(),
            );
            for package in packages {
                let select = || -> Result<Option<String>, crate::pkg_query::PackageQueryError> {
                    let installed = query.query_installed(package)?.ok_or_else(|| {
                        crate::pkg_query::PackageQueryError::QueryFailed {
                            command: "rpm".into(),
                            code: None,
                            stderr: format!("package {package} is not installed"),
                        }
                    })?;
                    let candidate = query
                        .query_available(&installed.name)?
                        .into_iter()
                        .filter(|info| {
                            info.arch == installed.arch
                                || info.arch == std::env::consts::ARCH
                                || info.arch == "noarch"
                        })
                        .max_by(|a, b| {
                            rpm_evr_cmp(&a.version, &b.version).then_with(|| {
                                (a.arch == installed.arch).cmp(&(b.arch == installed.arch))
                            })
                        })
                        .ok_or_else(|| crate::pkg_query::PackageQueryError::QueryFailed {
                            command: "RPM repository query".into(),
                            code: None,
                            stderr: format!(
                                "no compatible candidate for {package} in {}",
                                repo.id()
                            ),
                        })?;
                    Ok(rpm_evr_cmp(&candidate.version, &installed.version)
                        .is_gt()
                        .then(|| crate::rpm_select::nevra(&candidate)))
                };
                if let Some(target) =
                    select().map_err(|e| PackageTransactionError::TransactionFailed {
                        command: tool.program.into(),
                        operation: "update".into(),
                        code: None,
                        stderr: e.to_string(),
                    })?
                {
                    targets.push(target);
                }
            }
            // An empty upgrade-to target list could update the entire repo.
            if targets.is_empty() {
                return Ok(());
            }
            target_refs = targets.iter().map(String::as_str).collect::<Vec<_>>();
            target_refs.as_slice()
        } else {
            packages
        };
        let (args, _config) = self.args(tool, verb, packages)?;
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = self
            .runner
            .run(tool.program, &arg_refs)
            .map_err(|e| map_spawn_error(e, tool.program, verb))?;

        if out.code == Some(0)
            && !out
                .stdout
                .lines()
                .chain(out.stderr.lines())
                .any(missing_target_diagnostic)
        {
            if tool.dialect == RpmDialect::Yum && verb == "update" && self.repo.is_some() {
                use crate::pkg_query::PackageQuery;
                let query = crate::rpm_query::RpmPackageQuery::with_runner(RunnerRef(&self.runner));
                for spec in packages {
                    let installed = query.query_installed(spec).map_err(|error| {
                        PackageTransactionError::TransactionFailed {
                            command: tool.program.into(),
                            operation: verb.into(),
                            code: out.code,
                            stderr: error.to_string(),
                        }
                    })?;
                    if installed.is_none() {
                        return Err(PackageTransactionError::TransactionFailed {
                            command: tool.program.into(),
                            operation: verb.into(),
                            code: out.code,
                            stderr: self.redact_repo_diagnostic(&format!(
                                "requested update {spec} was not applied; check native exclusions or version locks: {}{}",
                                out.stdout, out.stderr
                            )),
                        });
                    }
                }
            }
            return Ok(());
        }

        // Native tools can put the actual refusal on stdout and only warnings on stderr.
        let detail = self.redact_repo_diagnostic(&format!("{}{}", out.stdout, out.stderr));
        Err(PackageTransactionError::TransactionFailed {
            command: tool.program.to_string(),
            operation: verb.to_string(),
            code: out.code,
            stderr: detail,
        })
    }

    fn dnf_args(&self, verb: &str, packages: &[&str]) -> Vec<String> {
        let Some(repo) = self.repo.as_ref().filter(|_| verb != "remove") else {
            let mut args = vec![verb.to_string(), "-y".to_string()];
            args.extend(packages.iter().map(|p| (*p).to_string()));
            return args;
        };

        let mut args = vec!["-y".to_string()];
        repo.append_dnf_txn_options(&mut args);

        // For install/upgrade, use `repository-packages <repo-id>` to constrain
        // the primary target to the configured repo. System repos stay enabled
        // for dependency resolution, but dnf will only pull the requested
        // package from the ANOLISA-configured repo — not a higher-EVR build
        // from a host-enabled system repo. For remove, use the plain verb
        // since the package should be removed regardless of its source repo.
        match verb {
            "install" => {
                args.push("repository-packages".to_string());
                args.push(repo.id().to_string());
                args.push("install".to_string());
            }
            "update" => {
                // `dnf repository-packages` uses `upgrade`, not `update`.
                args.push("repository-packages".to_string());
                args.push(repo.id().to_string());
                args.push("upgrade".to_string());
            }
            "reinstall" => {
                args.push("repository-packages".to_string());
                args.push(repo.id().to_string());
                // Unlike reinstall, move-to covers every target regardless of its old repo.
                args.push("move-to".to_string());
            }
            _ => {
                args.push(verb.to_string());
            }
        }
        args.extend(packages.iter().map(|p| (*p).to_string()));
        args
    }

    fn redact_repo_diagnostic(&self, detail: &str) -> String {
        self.repo
            .as_ref()
            .map_or_else(|| detail.to_string(), |repo| repo.redact_diagnostic(detail))
    }
}

impl<R: CommandRunner> PackageTransaction for RpmTransaction<R> {
    fn repository_source(&self) -> Option<&str> {
        self.repo.as_ref().map(RpmRepoSource::id)
    }

    fn check_install(&self, packages: &[&str]) -> Result<(), PackageTransactionError> {
        let tool = self.selected_tool()?;
        let (mut args, _config) = self.args(tool, "install", packages)?;
        // Override both our non-interactive apply flag and host configuration.
        args.retain(|arg| arg != "-y");
        // A site may allow skipped targets or hide the messages we classify.
        // Preflight must prove every requested package is installable.
        args.splice(
            0..0,
            [
                "--assumeno",
                if tool.dialect == RpmDialect::Dnf {
                    "--setopt=strict=1"
                } else {
                    "--setopt=alwaysprompt=1"
                },
                "--setopt=debuglevel=2",
                "--setopt=errorlevel=2",
            ]
            .map(str::to_string),
        );
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = self
            .runner
            .run(tool.program, &refs)
            .map_err(|err| map_spawn_error(err, tool.program, "install preflight"))?;
        // DNF 4 declines a solved transaction with exit 1. Logging and plugins
        // may put the confirmation marker in either stream alongside warnings.
        // Other pre-confirmation refusals reuse the same bare abort message.
        let lines = || out.stdout.lines().chain(out.stderr.lines());
        let resolved = lines().any(|line| {
            line.trim()
                == if tool.dialect == RpmDialect::Dnf {
                    "Dependencies resolved."
                } else {
                    "Dependencies Resolved"
                }
        });
        let declined = lines().any(|line| {
            matches!(
                line.trim(),
                "Operation aborted." | "Error: Operation aborted." | "Exiting on user command"
            )
        });
        let refused = lines().any(|line| {
            missing_target_diagnostic(line)
                || line.contains("usr_drift_protected_paths")
                || line.contains("Persistent transactions aren't supported")
                || line.contains("configured to be read-only")
        });
        if !refused && (out.code == Some(0) || (out.code == Some(1) && resolved && declined)) {
            return Ok(());
        }
        Err(PackageTransactionError::TransactionFailed {
            command: tool.program.to_string(),
            operation: "install preflight".to_string(),
            code: out.code,
            stderr: self.redact_repo_diagnostic(&format!("{}{}", out.stdout, out.stderr)),
        })
    }

    fn install(&self, packages: &[&str]) -> Result<(), PackageTransactionError> {
        self.run_native("install", packages)
    }

    fn update(&self, packages: &[&str]) -> Result<(), PackageTransactionError> {
        self.run_native("update", packages)
    }

    fn reinstall(&self, packages: &[&str]) -> Result<(), PackageTransactionError> {
        self.run_native("reinstall", packages)
    }

    fn remove(&self, packages: &[&str]) -> Result<(), PackageTransactionError> {
        self.run_native("remove", packages)
    }
}

fn missing_target_diagnostic(line: &str) -> bool {
    line.contains("No match for argument:")
        || line.contains("Unable to find a match")
        || line.contains("No package(s) available")
        || (line.starts_with("Installed package ") && line.ends_with(" not available."))
}

struct RunnerRef<'a, R>(&'a R);
impl<R: CommandRunner> CommandRunner for RunnerRef<'_, R> {
    fn run(&self, program: &str, args: &[&str]) -> std::io::Result<crate::command::CommandOutput> {
        self.0.run(program, args)
    }
}

/// Map a spawn-phase [`std::io::Error`] to a transaction error by
/// [`std::io::ErrorKind`], mirroring the query backend's classification.
///
/// `verb` records which dnf transaction was being spawned so a non-spawn
/// error kind still names the operation that failed.
fn map_spawn_error(e: std::io::Error, command: &str, verb: &str) -> PackageTransactionError {
    match e.kind() {
        std::io::ErrorKind::NotFound => PackageTransactionError::CommandMissing {
            command: command.to_string(),
        },
        std::io::ErrorKind::PermissionDenied => PackageTransactionError::PermissionDenied {
            command: command.to_string(),
        },
        _ => PackageTransactionError::TransactionFailed {
            command: command.to_string(),
            operation: verb.to_string(),
            code: None,
            stderr: e.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::CommandOutput;
    use std::io;

    #[test]
    fn yum_updates_only_newer_candidates_in_mixed_and_noop_batches() {
        use sha2::{Digest, Sha256};
        use std::cell::RefCell;
        struct Runner(RefCell<Vec<Vec<String>>>);
        impl CommandRunner for &Runner {
            fn run(&self, program: &str, args: &[&str]) -> io::Result<CommandOutput> {
                let stdout = match program {
                    "yum" => {
                        let start = args.iter().position(|arg| *arg == "upgrade-to").unwrap() + 1;
                        assert_eq!(&args[start..], ["newer-2-1.noarch"]);
                        self.0
                            .borrow_mut()
                            .push(args.iter().map(|s| s.to_string()).collect());
                        String::new()
                    }
                    "rpm" => {
                        let package = *args.last().unwrap();
                        let (name, version) = match package {
                            "newer" => ("newer", "1"),
                            "equal" => ("equal", "2"),
                            "older" => ("older", "3"),
                            "newer-2-1.noarch" if !self.0.borrow().is_empty() => ("newer", "2"),
                            _ => panic!("unexpected RPM query: {args:?}"),
                        };
                        format!("{name}|0|{version}|1|noarch\n")
                    }
                    _ => panic!("unexpected command: {program}"),
                };
                Ok(CommandOutput {
                    code: Some(0),
                    stdout,
                    stderr: String::new(),
                })
            }
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("repodata")).unwrap();
        let mut xml = String::from(
            r#"<metadata xmlns="http://linux.duke.edu/metadata/common" xmlns:rpm="http://linux.duke.edu/metadata/rpm" packages="3">"#,
        );
        for name in ["newer", "equal", "older"] {
            xml.push_str(&format!(r#"<package type="rpm"><name>{name}</name><arch>noarch</arch><version epoch="0" ver="2" rel="1"/><checksum type="sha256" pkgid="YES">{}</checksum><summary/><description/><packager/><url/><time file="0" build="0"/><size package="1" installed="1" archive="1"/><location href="{name}.rpm"/><format><rpm:license>MIT</rpm:license><rpm:vendor/><rpm:group/><rpm:buildhost/><rpm:sourcerpm/><rpm:header-range start="0" end="0"/></format></package>"#, "0".repeat(64)));
        }
        xml.push_str("</metadata>");
        std::fs::write(dir.path().join("repodata/primary.xml"), &xml).unwrap();
        std::fs::write(dir.path().join("repodata/repomd.xml"), format!(r#"<repomd xmlns="http://linux.duke.edu/metadata/repo"><data type="primary"><checksum type="sha256">{:x}</checksum><location href="repodata/primary.xml"/><size>{}</size><timestamp>0</timestamp></data></repomd>"#, Sha256::digest(xml.as_bytes()), xml.len())).unwrap();
        let runner = Runner(RefCell::new(Vec::new()));
        let txn = RpmTransaction::with_tool(
            &runner,
            RpmTool {
                program: "yum",
                dialect: RpmDialect::Yum,
            },
            Some(RpmRepoSource::new(
                "fixture",
                url::Url::from_directory_path(dir.path())
                    .unwrap()
                    .to_string(),
                Some(false),
            )),
        );
        txn.update(&["older", "equal", "newer"]).unwrap();
        assert_eq!(runner.0.borrow().len(), 1);
        for batch in [&["older", "equal"][..], &["older"][..], &["equal"][..]] {
            txn.update(batch).unwrap();
            assert_eq!(
                runner.0.borrow().len(),
                1,
                "no-op must not invoke yum without targets"
            );
        }
    }

    /// Preset result for the fake runner: either a captured output or a
    /// spawn-phase error kind to replay.
    enum FakeOutcome {
        Ok(CommandOutput),
        Err(io::ErrorKind),
    }

    /// Fake runner that asserts the dnf call contract and replays a canned
    /// outcome. A program with no preset yields `NotFound`.
    struct FakeCommandRunner {
        dnf: Option<FakeOutcome>,
        expected_verb: String,
        expected_package: String,
        expected_args: Option<Vec<String>>,
    }

    impl CommandRunner for FakeCommandRunner {
        fn run(&self, program: &str, args: &[&str]) -> io::Result<CommandOutput> {
            // Pin the invocation shape: a regression that drops `-y`, swaps the
            // verb, or misplaces the package argument must fail loudly rather
            // than pass on the canned output alone.
            assert_eq!(program, DNF, "transaction must shell out to dnf: {program}");
            if let Some(expected_args) = &self.expected_args {
                assert_eq!(
                    args, expected_args,
                    "dnf args drifted from configured repo contract: {args:?}"
                );
            } else {
                assert_eq!(
                    args,
                    [
                        self.expected_verb.as_str(),
                        "-y",
                        self.expected_package.as_str()
                    ],
                    "dnf args drifted: {args:?}"
                );
            }
            match &self.dnf {
                Some(FakeOutcome::Ok(o)) => Ok(o.clone()),
                Some(FakeOutcome::Err(kind)) => {
                    Err(io::Error::new(*kind, format!("fake {program} failure")))
                }
                None => Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("no fake preset for {program}"),
                )),
            }
        }
    }

    fn txn(
        expected_verb: &str,
        expected_package: &str,
        outcome: FakeOutcome,
    ) -> RpmTransaction<FakeCommandRunner> {
        RpmTransaction::with_runner(FakeCommandRunner {
            dnf: Some(outcome),
            expected_verb: expected_verb.to_string(),
            expected_package: expected_package.to_string(),
            expected_args: None,
        })
    }

    fn txn_with_repo(
        expected_verb: &str,
        expected_package: &str,
        expected_args: &[&str],
        outcome: FakeOutcome,
    ) -> RpmTransaction<FakeCommandRunner> {
        RpmTransaction::with_runner_and_repo(
            FakeCommandRunner {
                dnf: Some(outcome),
                expected_verb: expected_verb.to_string(),
                expected_package: expected_package.to_string(),
                expected_args: Some(expected_args.iter().map(|s| s.to_string()).collect()),
            },
            RpmRepoSource::new(
                "anolisa-configured",
                "http://repo.example/alinux/4/agentic-os/x86_64/os",
                Some(true),
            ),
        )
    }

    fn ok_out(code: Option<i32>, stdout: &str, stderr: &str) -> FakeOutcome {
        FakeOutcome::Ok(CommandOutput {
            code,
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
        })
    }

    #[test]
    fn install_preflight_distinguishes_solver_success_from_failures() {
        for (code, stdout, stderr, succeeds) in [
            (
                Some(1),
                "No match for argument: absent\nDependencies resolved.\nOperation aborted.\n",
                "",
                false,
            ),
            (
                Some(0),
                "No match for argument: absent\nNothing to do.\n",
                "",
                false,
            ),
            (Some(0), "Nothing to do.\n", "", true),
            (
                Some(1),
                "Dependencies resolved.\nTransaction Summary\n",
                "Operation aborted.\n",
                true,
            ),
            (
                Some(1),
                "Dependencies resolved.\nOperation aborted.\n",
                "",
                true,
            ),
            (
                Some(1),
                "Dependencies resolved.\n",
                "Error: Operation aborted.\n",
                true,
            ),
            (
                Some(1),
                "Dependencies resolved.\n",
                "Plugin warning: optional integration unavailable\nOperation aborted.\n",
                true,
            ),
            (
                Some(1),
                "Operation aborted.\n",
                "Dependencies resolved.\n",
                true,
            ),
            (
                Some(1),
                "Dependencies resolved.\nThis bootc system is configured to be read-only.\n",
                "Operation aborted.\n",
                false,
            ),
            (
                Some(1),
                "Dependencies resolved.\nPersistent transactions aren't supported on bootc systems.\n",
                "Error: Operation aborted.\n",
                false,
            ),
            (
                Some(1),
                "Dependencies resolved.\n",
                "Operation aborted. Pass --setopt=usr_drift_protected_paths= to disable this check.\n",
                false,
            ),
            (
                Some(1),
                "",
                "installed cosh-ng conflicts with copilot-shell",
                false,
            ),
            (Some(1), "", "Operation aborted.\n", false),
            (
                Some(1),
                "Dependencies resolved.\n",
                "repository failed",
                false,
            ),
            (
                None,
                "Dependencies resolved.\n",
                "Operation aborted.\n",
                false,
            ),
            (
                Some(1),
                "",
                "This command has to be run with superuser privileges",
                false,
            ),
        ] {
            let t = RpmTransaction::with_runner(FakeCommandRunner {
                dnf: Some(ok_out(code, stdout, stderr)),
                expected_verb: String::new(),
                expected_package: String::new(),
                expected_args: Some(vec![
                    "--assumeno".into(),
                    "--setopt=strict=1".into(),
                    "--setopt=debuglevel=2".into(),
                    "--setopt=errorlevel=2".into(),
                    "install".into(),
                    "copilot-shell".into(),
                ]),
            });
            let result = t.check_install(&["copilot-shell"]);
            assert_eq!(result.is_ok(), succeeds, "{code:?}: {stdout} {stderr}");
            if let Err(err) = result {
                assert!(err.to_string().contains(stderr.trim()));
            }
        }
    }

    #[test]
    fn install_preflight_keeps_repo_pins_and_joint_solver_targets() {
        let t = txn_with_repo(
            "install",
            "cosh-ng-0.23.0-1.alnx4.x86_64",
            &[
                "--assumeno",
                "--setopt=strict=1",
                "--setopt=debuglevel=2",
                "--setopt=errorlevel=2",
                "--repofrompath=anolisa-configured,http://repo.example/alinux/4/agentic-os/x86_64/os",
                "--enablerepo=anolisa-configured",
                "--setopt=anolisa-configured.gpgcheck=1",
                "repository-packages",
                "anolisa-configured",
                "install",
                "cosh-ng-0.23.0-1.alnx4.x86_64",
                "copilot-shell",
            ],
            ok_out(Some(1), "", "cosh-ng conflicts with copilot-shell"),
        );
        let err = t
            .check_install(&["cosh-ng-0.23.0-1.alnx4.x86_64", "copilot-shell"])
            .expect_err("joint solver conflict");
        assert!(err.to_string().contains("cosh-ng conflicts"));
    }

    #[test]
    fn update_success_returns_ok() {
        let t = txn(
            "update",
            "copilot-shell",
            ok_out(Some(0), "Upgraded:\n  copilot-shell\n", ""),
        );
        t.update(&["copilot-shell"]).expect("update ok");
    }

    #[test]
    fn install_success_returns_ok() {
        let t = txn(
            "install",
            "copilot-shell",
            ok_out(Some(0), "Installed:\n  copilot-shell\n", ""),
        );
        t.install(&["copilot-shell"]).expect("install ok");
    }

    #[test]
    fn install_with_repo_uses_repository_packages() {
        // Transactions keep system repos enabled (no --disablerepo=*) so dnf
        // can resolve cross-repo Requires (e.g. bubblewrap in EPEL). The
        // primary target is constrained to the configured repo via
        // `repository-packages`, preventing dnf from pulling a higher-EVR
        // build from a host-enabled system repo.
        let t = txn_with_repo(
            "install",
            "copilot-shell",
            &[
                "-y",
                "--repofrompath=anolisa-configured,http://repo.example/alinux/4/agentic-os/x86_64/os",
                "--enablerepo=anolisa-configured",
                "--setopt=anolisa-configured.gpgcheck=1",
                "repository-packages",
                "anolisa-configured",
                "install",
                "copilot-shell",
            ],
            ok_out(Some(0), "Installed:\n  copilot-shell\n", ""),
        );
        t.install(&["copilot-shell"]).expect("install ok");
    }

    #[test]
    fn install_with_repo_places_pinned_nevra_after_repository_packages_install() {
        // A version-pinned install hands an exact NEVRA in place of the bare
        // package; it must land immediately after `repository-packages <repo>
        // install`, unchanged, so dnf pulls exactly that build from the
        // configured repo.
        let t = txn_with_repo(
            "install",
            "agentsight-0.6.2-1.alnx4.x86_64",
            &[
                "-y",
                "--repofrompath=anolisa-configured,http://repo.example/alinux/4/agentic-os/x86_64/os",
                "--enablerepo=anolisa-configured",
                "--setopt=anolisa-configured.gpgcheck=1",
                "repository-packages",
                "anolisa-configured",
                "install",
                "agentsight-0.6.2-1.alnx4.x86_64",
            ],
            ok_out(Some(0), "Installed:\n  agentsight\n", ""),
        );
        t.install(&["agentsight-0.6.2-1.alnx4.x86_64"])
            .expect("pinned install ok");
    }

    #[test]
    fn install_many_packages_share_one_dnf_invocation() {
        // The whole point of the multi-package contract: one dnf process sees
        // the full set, so the solver resolves it as a single transaction.
        let t = RpmTransaction::with_runner(FakeCommandRunner {
            dnf: Some(ok_out(Some(0), "Installed:\n  a b c\n", "")),
            expected_verb: "install".to_string(),
            expected_package: String::new(),
            expected_args: Some(
                ["install", "-y", "a", "b", "c"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
            ),
        });
        t.install(&["a", "b", "c"]).expect("install ok");
    }

    #[test]
    fn install_many_with_repo_appends_all_packages_after_verb() {
        let t = txn_with_repo(
            "install",
            "a",
            &[
                "-y",
                "--repofrompath=anolisa-configured,http://repo.example/alinux/4/agentic-os/x86_64/os",
                "--enablerepo=anolisa-configured",
                "--setopt=anolisa-configured.gpgcheck=1",
                "repository-packages",
                "anolisa-configured",
                "install",
                "a",
                "b",
                "c",
            ],
            ok_out(Some(0), "Installed:\n  a b c\n", ""),
        );
        t.install(&["a", "b", "c"]).expect("install ok");
    }

    #[test]
    fn update_with_repo_uses_repository_packages_upgrade() {
        // `dnf repository-packages` uses `upgrade`, not `update`.
        let t = txn_with_repo(
            "update",
            "copilot-shell",
            &[
                "-y",
                "--repofrompath=anolisa-configured,http://repo.example/alinux/4/agentic-os/x86_64/os",
                "--enablerepo=anolisa-configured",
                "--setopt=anolisa-configured.gpgcheck=1",
                "repository-packages",
                "anolisa-configured",
                "upgrade",
                "copilot-shell",
            ],
            ok_out(Some(0), "Upgraded:\n  copilot-shell\n", ""),
        );
        t.update(&["copilot-shell"]).expect("update ok");
    }

    #[test]
    fn reinstall_success_returns_ok() {
        let t = txn(
            "reinstall",
            "copilot-shell",
            ok_out(Some(0), "Reinstalled:\n  copilot-shell\n", ""),
        );
        t.reinstall(&["copilot-shell"]).expect("reinstall ok");
    }

    #[test]
    fn reinstall_with_repo_uses_repository_packages() {
        // Like install/update, reinstall constrains the primary target to the
        // configured repo so the payload comes from the repo ANOLISA trusts,
        // not a same-EVR build in a host-enabled system repo.
        let t = txn_with_repo(
            "reinstall",
            "copilot-shell",
            &[
                "-y",
                "--repofrompath=anolisa-configured,http://repo.example/alinux/4/agentic-os/x86_64/os",
                "--enablerepo=anolisa-configured",
                "--setopt=anolisa-configured.gpgcheck=1",
                "repository-packages",
                "anolisa-configured",
                "move-to",
                "copilot-shell",
            ],
            ok_out(Some(0), "Reinstalled:\n  copilot-shell\n", ""),
        );
        t.reinstall(&["copilot-shell"]).expect("reinstall ok");
    }

    #[test]
    fn reinstall_nonzero_exit_records_reinstall_operation() {
        let t = txn(
            "reinstall",
            "copilot-shell",
            ok_out(
                Some(1),
                "",
                "Error: Installed package copilot-shell not available.",
            ),
        );
        let err = t.reinstall(&["copilot-shell"]).unwrap_err();
        match err {
            PackageTransactionError::TransactionFailed {
                operation, stderr, ..
            } => {
                assert_eq!(operation, "reinstall");
                assert!(stderr.contains("not available"));
            }
            other => panic!("expected TransactionFailed, got {other:?}"),
        }
    }

    #[test]
    fn remove_with_repo_uses_plain_verb() {
        // Remove must NOT use `repository-packages`: the package should be
        // removed regardless of which repo it was installed from (e.g. an
        // adopted system RPM that was later recorded as rpm-managed).
        let t = txn_with_repo(
            "remove",
            "copilot-shell",
            &["remove", "-y", "copilot-shell"],
            ok_out(Some(0), "Removed:\n  copilot-shell\n", ""),
        );
        t.remove(&["copilot-shell"]).expect("remove ok");
    }

    #[test]
    fn remove_success_returns_ok() {
        let t = txn(
            "remove",
            "copilot-shell",
            ok_out(Some(0), "Removed:\n  copilot-shell\n", ""),
        );
        t.remove(&["copilot-shell"]).expect("remove ok");
    }

    #[test]
    fn remove_nonzero_exit_records_remove_operation() {
        // The failed-operation label must follow the verb so callers can tell a
        // remove failure apart from an install/update failure.
        let t = txn(
            "remove",
            "copilot-shell",
            ok_out(Some(1), "", "Error: No match for argument: copilot-shell"),
        );
        let err = t.remove(&["copilot-shell"]).unwrap_err();
        match err {
            PackageTransactionError::TransactionFailed {
                operation, stderr, ..
            } => {
                assert_eq!(operation, "remove");
                assert!(stderr.contains("No match for argument"));
            }
            other => panic!("expected TransactionFailed, got {other:?}"),
        }
    }

    #[test]
    fn update_nonzero_exit_maps_to_transaction_failed() {
        let t = txn(
            "update",
            "copilot-shell",
            ok_out(Some(1), "", "Error: nothing to do, repo unreachable"),
        );
        let err = t.update(&["copilot-shell"]).unwrap_err();
        match err {
            PackageTransactionError::TransactionFailed {
                command,
                operation,
                code,
                stderr,
            } => {
                assert_eq!(command, DNF);
                assert_eq!(operation, "update");
                assert_eq!(code, Some(1));
                assert!(stderr.contains("repo unreachable"));
            }
            other => panic!("expected TransactionFailed, got {other:?}"),
        }
    }

    #[test]
    fn install_nonzero_exit_records_install_operation() {
        // The failed-operation label must follow the verb so callers can tell
        // an install failure apart from an update failure.
        let t = txn(
            "install",
            "copilot-shell",
            ok_out(Some(1), "", "Error: No match for argument"),
        );
        let err = t.install(&["copilot-shell"]).unwrap_err();
        match err {
            PackageTransactionError::TransactionFailed {
                operation, stderr, ..
            } => {
                assert_eq!(operation, "install");
                assert!(stderr.contains("No match for argument"));
            }
            other => panic!("expected TransactionFailed, got {other:?}"),
        }
    }

    #[test]
    fn update_failure_falls_back_to_stdout_when_stderr_empty() {
        // dnf's privilege refusal is written to stdout; surface it rather than
        // an empty diagnostic.
        let t = txn(
            "update",
            "copilot-shell",
            ok_out(
                Some(1),
                "Error: This command has to be run with superuser privileges",
                "",
            ),
        );
        let err = t.update(&["copilot-shell"]).unwrap_err();
        match err {
            PackageTransactionError::TransactionFailed { stderr, .. } => {
                assert!(stderr.contains("superuser privileges"), "got: {stderr}");
            }
            other => panic!("expected TransactionFailed, got {other:?}"),
        }
    }

    #[test]
    fn command_missing_maps_to_error() {
        let t = txn("update", "x", FakeOutcome::Err(io::ErrorKind::NotFound));
        let err = t.update(&["x"]).unwrap_err();
        assert!(matches!(
            err,
            PackageTransactionError::CommandMissing { command } if command == DNF
        ));
    }

    #[test]
    fn permission_denied_maps_to_error() {
        let t = txn(
            "update",
            "x",
            FakeOutcome::Err(io::ErrorKind::PermissionDenied),
        );
        let err = t.update(&["x"]).unwrap_err();
        assert!(matches!(
            err,
            PackageTransactionError::PermissionDenied { command } if command == DNF
        ));
    }
}
