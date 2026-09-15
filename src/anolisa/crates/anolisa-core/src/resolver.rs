//! Raw-backend runtime-dependency preflight.
//!
//! Before the raw backend lays any files, it probes each declared
//! [`RuntimeDependency`] and fails fast on a miss — turning "install succeeded,
//! service silently dead" into a clear remediation. The RPM backend never runs
//! this: dnf resolves `Requires` instead, so a dependency is never resolved
//! twice. Check-only by design: the resolver reports what to install but never
//! mutates the host.

use crate::manifest::{DependencyKind, RuntimeDependency};
use anolisa_platform::command::{CommandRunner, SystemCommandRunner};
use anolisa_platform::pkg_query::{PackageQuery, PackageQueryError};
use anolisa_platform::rpm_query::RpmPackageQuery;

/// Host facts the preflight needs, decoupled from `anolisa_env::EnvFacts` so
/// callers (and tests) supply only the relevant slice.
#[derive(Debug, Clone, Default)]
pub struct ResolverEnv {
    /// Kernel release (`uname -r`), e.g. `5.10.134-007.ali5000`. Gates
    /// `min_kernel` declarations.
    pub kernel: Option<String>,
    /// Coarse package-base family (`"rpm"` / `"deb"`) used for remediation
    /// commands and native package queries. `None` → unsupported package
    /// manager.
    pub pkg_base: Option<String>,
    /// Whether kernel BTF is available (`/sys/kernel/btf/vmlinux`).
    pub btf: Option<bool>,
    /// Whether `CAP_BPF` is available.
    pub cap_bpf: Option<bool>,
}

/// Outcome of probing one dependency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DependencyStatus {
    /// Present, and any verifiable version constraint is satisfied.
    Resolved,
    /// Missing but installable: carries the command the user should run.
    Unresolved {
        /// Remediation command or instruction (e.g. `sudo dnf install
        /// btrfs-progs`).
        remediation: String,
    },
    /// Missing and not installable by any package manager (kernel/capability):
    /// the install cannot proceed on this host.
    Unresolvable {
        /// Why the host cannot satisfy this dependency.
        reason: String,
    },
    /// The dependency's presence could not be determined; installation is unsafe.
    ProbeFailed {
        /// Typed evidence from the query boundary, not an installation hint.
        error: DependencyProbeError,
    },
}

/// Query failures retained in a cloneable dependency resolution report.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DependencyProbeError {
    /// The query executable could not be found.
    #[error("command not found: {command}")]
    CommandMissing {
        /// Executable name.
        command: String,
    },
    /// The query executable could not be started due to permissions.
    #[error("permission denied running {command}")]
    PermissionDenied {
        /// Executable name.
        command: String,
    },
    /// The query did not return a valid presence or absence result.
    #[error("{command} failed (code {code:?}): {stderr}")]
    QueryFailed {
        /// Executable name.
        command: String,
        /// Exit code, or `None` for signal termination or other spawn failures.
        code: Option<i32>,
        /// Original stderr or spawn-error diagnostic from the probe boundary.
        stderr: String,
    },
    /// The query returned malformed or ambiguous metadata.
    #[error("unexpected {command} output: {detail}")]
    UnexpectedOutput {
        /// Executable name.
        command: String,
        /// Explanation from the probe boundary, including unexpected output.
        detail: String,
    },
}

impl From<PackageQueryError> for DependencyProbeError {
    fn from(error: PackageQueryError) -> Self {
        match error {
            PackageQueryError::CommandMissing { command } => Self::CommandMissing { command },
            PackageQueryError::PermissionDenied { command } => Self::PermissionDenied { command },
            PackageQueryError::QueryFailed {
                command,
                code,
                stderr,
            } => Self::QueryFailed {
                command,
                code,
                stderr,
            },
            PackageQueryError::UnexpectedOutput { command, detail } => {
                Self::UnexpectedOutput { command, detail }
            }
        }
    }
}

struct BorrowedRunner<'a, R>(&'a R);

impl<R: CommandRunner> CommandRunner for BorrowedRunner<'_, R> {
    fn run(
        &self,
        program: &str,
        args: &[&str],
    ) -> std::io::Result<anolisa_platform::command::CommandOutput> {
        self.0.run(program, args)
    }
}

/// Per-dependency preflight result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencyResolution {
    /// Logical dependency name.
    pub name: String,
    /// Resolution bucket the dependency was dispatched through.
    pub kind: DependencyKind,
    /// Probe outcome.
    pub status: DependencyStatus,
    /// Optional human note (e.g. "version not verified against '>=20'").
    pub detail: Option<String>,
}

/// Aggregate preflight result over all declared dependencies.
#[derive(Debug, Clone, Default)]
pub struct ResolutionPlan {
    /// One entry per declared dependency, in declaration order.
    pub resolutions: Vec<DependencyResolution>,
    /// Non-fatal notes collected during resolution.
    pub warnings: Vec<String>,
}

impl ResolutionPlan {
    /// Whether every dependency resolved. `false` if any is `Unresolved` or
    /// `Unresolvable` — the install must not proceed.
    pub fn is_satisfied(&self) -> bool {
        self.resolutions
            .iter()
            .all(|r| matches!(r.status, DependencyStatus::Resolved))
    }

    /// One line per unsatisfied dependency, for a single fail-fast message
    /// listing every miss at once.
    pub fn unsatisfied_lines(&self) -> Vec<String> {
        self.resolutions
            .iter()
            .filter_map(|r| match &r.status {
                DependencyStatus::Resolved => None,
                DependencyStatus::Unresolved { remediation } => {
                    Some(format!("{} [{}]: {remediation}", r.name, r.kind.as_str()))
                }
                DependencyStatus::Unresolvable { reason } => {
                    Some(format!("{} [{}]: {reason}", r.name, r.kind.as_str()))
                }
                DependencyStatus::ProbeFailed { error } => Some(format!(
                    "{} [{}]: dependency probe failed: {error}",
                    r.name,
                    r.kind.as_str()
                )),
            })
            .collect()
    }
}

/// Failure that means the contract itself is wrong. Never raised for a merely
/// missing dependency — that is a [`DependencyStatus`], not an error.
#[derive(Debug, thiserror::Error)]
pub enum ResolverError {
    /// A `platform-capability` dependency named a `check` the resolver does not
    /// implement.
    #[error("dependency '{name}' has unknown platform-capability check '{check}'")]
    UnknownCheck {
        /// Dependency name.
        name: String,
        /// The unrecognized check identifier.
        check: String,
    },
}

/// Probes declared runtime dependencies through a command runner and a lazy
/// filesystem-capability reader. The defaults use real host probes.
pub struct DependencyResolver<
    R: CommandRunner = SystemCommandRunner,
    F: Fn() -> std::io::Result<String> = fn() -> std::io::Result<String>,
> {
    runner: R,
    read_filesystems: F,
}

impl DependencyResolver<SystemCommandRunner> {
    /// Build a resolver that runs real host commands and reads `/proc/filesystems`
    /// only when a btrfs capability check reaches that probe.
    pub fn system() -> Self {
        Self::with_runner(SystemCommandRunner)
    }
}

impl<R: CommandRunner> DependencyResolver<R> {
    /// Build a resolver backed by a custom command runner.
    ///
    /// The btrfs probe still reads the host's `/proc/filesystems`; use
    /// [`Self::with_probes`] to replace both host boundaries.
    pub fn with_runner(runner: R) -> Self {
        Self::with_probes(runner, || std::fs::read_to_string("/proc/filesystems"))
    }
}

impl<R: CommandRunner, F: Fn() -> std::io::Result<String>> DependencyResolver<R, F> {
    /// Build a resolver with custom command and filesystem-capability probes.
    ///
    /// `read_filesystems` supplies raw `/proc/filesystems` content or its read
    /// error. It is called once per evaluated btrfs dependency, after the kernel
    /// gate; construction and unrelated checks do not call it. Results are not
    /// cached, so a later resolution observes the reader again.
    pub fn with_probes(runner: R, read_filesystems: F) -> Self {
        Self {
            runner,
            read_filesystems,
        }
    }

    /// Probe every dependency and aggregate the outcome. Never mutates the host.
    ///
    /// # Errors
    /// Returns [`ResolverError`] only when a dependency declaration is itself
    /// invalid (e.g. an unknown platform-capability `check`); a missing
    /// dependency is reported as a [`DependencyStatus`], not an error.
    pub fn resolve(
        &self,
        deps: &[RuntimeDependency],
        env: &ResolverEnv,
    ) -> Result<ResolutionPlan, ResolverError> {
        let mut plan = ResolutionPlan::default();
        for dep in deps {
            let (status, detail) = match dep.kind {
                DependencyKind::SystemPackage => self.resolve_system_package(dep, env),
                DependencyKind::LanguageRuntime => self.resolve_language_runtime(dep),
                DependencyKind::PlatformCapability => {
                    resolve_platform_capability(dep, env, &self.read_filesystems)?
                }
            };
            plan.resolutions.push(DependencyResolution {
                name: dep.name.clone(),
                kind: dep.kind,
                status,
                detail,
            });
        }
        Ok(plan)
    }

    /// System package: present → resolved; missing → remediation command for
    /// the host package manager. Presence-first (no version gate in MVP).
    fn resolve_system_package(
        &self,
        dep: &RuntimeDependency,
        env: &ResolverEnv,
    ) -> (DependencyStatus, Option<String>) {
        let present = match &dep.probe {
            Some(probe) => match self.run_probe(probe) {
                ProbeOutcome::Present { .. } => true,
                ProbeOutcome::Absent => false,
                ProbeOutcome::Failed(error) => {
                    return (DependencyStatus::ProbeFailed { error }, None);
                }
            },
            None => match self.native_package_present(dep, env) {
                Ok(present) => present,
                Err(error) => return (DependencyStatus::ProbeFailed { error }, None),
            },
        };
        if present {
            return (DependencyStatus::Resolved, None);
        }
        (
            DependencyStatus::Unresolved {
                remediation: system_package_remediation(dep, env),
            },
            None,
        )
    }

    /// Language runtime: probe presence, then a presence-first version check.
    /// Missing → manual-install hint (no vendoring in MVP).
    fn resolve_language_runtime(
        &self,
        dep: &RuntimeDependency,
    ) -> (DependencyStatus, Option<String>) {
        let probe = dep
            .probe
            .clone()
            .unwrap_or_else(|| format!("{} --version", dep.name));
        let stdout = match self.run_probe(&probe) {
            ProbeOutcome::Present { stdout } => stdout,
            ProbeOutcome::Absent => {
                return (
                    DependencyStatus::Unresolved {
                        remediation: language_runtime_hint(dep),
                    },
                    None,
                );
            }
            ProbeOutcome::Failed(error) => return (DependencyStatus::ProbeFailed { error }, None),
        };
        match version_verdict(dep.version.as_deref(), &stdout) {
            VersionVerdict::Ok => (DependencyStatus::Resolved, None),
            VersionVerdict::NotVerified => (
                DependencyStatus::Resolved,
                dep.version
                    .as_deref()
                    .map(|v| format!("version not verified against '{v}'")),
            ),
            VersionVerdict::Mismatch { found } => (
                DependencyStatus::Unresolved {
                    remediation: language_runtime_hint(dep),
                },
                Some(format!(
                    "found {found}, need {}",
                    dep.version.as_deref().unwrap_or("")
                )),
            ),
        }
    }

    // Ordinary nonzero exits and missing executables retain the manifest's
    // presence contract; execution faults must not trigger installation.
    fn run_probe(&self, probe: &str) -> ProbeOutcome {
        let mut parts = probe.split_whitespace();
        let Some(program) = parts.next() else {
            return ProbeOutcome::Absent;
        };
        let args: Vec<&str> = parts.collect();
        match self.runner.run(program, &args) {
            Ok(out) if out.code == Some(0) => ProbeOutcome::Present { stdout: out.stdout },
            Ok(out) if out.code.is_none() => {
                let error = if out.stderr.trim().is_empty() && !out.stdout.trim().is_empty() {
                    DependencyProbeError::UnexpectedOutput {
                        command: program.into(),
                        detail: format!(
                            "{probe} failed (code {:?}); stdout: {}",
                            out.code, out.stdout
                        ),
                    }
                } else {
                    DependencyProbeError::QueryFailed {
                        command: program.into(),
                        code: out.code,
                        stderr: out.stderr,
                    }
                };
                ProbeOutcome::Failed(error)
            }
            Ok(_) => ProbeOutcome::Absent,
            Err(error) => match error.kind() {
                std::io::ErrorKind::NotFound => ProbeOutcome::Absent,
                std::io::ErrorKind::PermissionDenied => {
                    ProbeOutcome::Failed(DependencyProbeError::PermissionDenied {
                        command: program.into(),
                    })
                }
                _ => ProbeOutcome::Failed(DependencyProbeError::QueryFailed {
                    command: program.into(),
                    code: None,
                    stderr: error.to_string(),
                }),
            },
        }
    }

    /// RPM errors remain distinct from absence; other package families retain
    /// the legacy presence policy until their query contracts are migrated.
    fn native_package_present(
        &self,
        dep: &RuntimeDependency,
        env: &ResolverEnv,
    ) -> Result<bool, DependencyProbeError> {
        if env.pkg_base.as_deref() == Some("rpm") {
            return RpmPackageQuery::with_runner(BorrowedRunner(&self.runner))
                .is_installed(dep.packages.rpm.as_deref().unwrap_or(&dep.name))
                .map_err(DependencyProbeError::from);
        }
        let (program, args): (&str, [&str; 2]) = match env.pkg_base.as_deref() {
            Some("deb") => (
                "dpkg",
                ["-s", dep.packages.deb.as_deref().unwrap_or(&dep.name)],
            ),
            _ => return Ok(false),
        };
        Ok(matches!(self.runner.run(program, &args), Ok(out) if out.code == Some(0)))
    }
}

/// Probe result, carrying stdout for an optional version parse.
enum ProbeOutcome {
    Present { stdout: String },
    Absent,
    Failed(DependencyProbeError),
}

/// Platform capability: gate `min_kernel` first, then evaluate the built-in
/// `check`. Never installs; a miss is `Unresolvable`.
fn resolve_platform_capability(
    dep: &RuntimeDependency,
    env: &ResolverEnv,
    read_filesystems: &impl Fn() -> std::io::Result<String>,
) -> Result<(DependencyStatus, Option<String>), ResolverError> {
    if let Some(min) = &dep.min_kernel {
        match kernel_satisfies(env.kernel.as_deref(), min) {
            KernelCheck::Satisfied => {}
            KernelCheck::Below { have } => {
                return Ok((
                    DependencyStatus::Unresolvable {
                        reason: format!("requires kernel >= {min}, host is {have}"),
                    },
                    None,
                ));
            }
            KernelCheck::Unknown => {
                return Ok((
                    DependencyStatus::Unresolvable {
                        reason: format!(
                            "requires kernel >= {min}, but the host kernel could not be determined"
                        ),
                    },
                    None,
                ));
            }
        }
    }

    if let Some(check) = &dep.check {
        let result = evaluate_check(check, env, read_filesystems).ok_or_else(|| {
            ResolverError::UnknownCheck {
                name: dep.name.clone(),
                check: check.clone(),
            }
        })?;
        return Ok(match result {
            CheckResult::Supported => (DependencyStatus::Resolved, None),
            CheckResult::Unsupported { reason } => {
                (DependencyStatus::Unresolvable { reason }, None)
            }
        });
    }

    // A min_kernel-only declaration that passed the gate has nothing left to
    // verify.
    Ok((DependencyStatus::Resolved, None))
}

/// Remediation command for a missing system package, by host package format.
fn system_package_remediation(dep: &RuntimeDependency, env: &ResolverEnv) -> String {
    match env.pkg_base.as_deref() {
        Some("rpm") => format!(
            "sudo dnf install {}",
            dep.packages.rpm.as_deref().unwrap_or(&dep.name)
        ),
        Some("deb") => format!(
            "sudo apt install {}",
            dep.packages.deb.as_deref().unwrap_or(&dep.name)
        ),
        _ => format!(
            "unsupported package manager — install '{}' with the host package manager",
            dep.name
        ),
    }
}

/// Manual-install hint for a missing language runtime (vendoring is a later
/// phase, so MVP only tells the user what to install).
fn language_runtime_hint(dep: &RuntimeDependency) -> String {
    let version = dep
        .version
        .as_deref()
        .map(|v| format!(" {v}"))
        .unwrap_or_default();
    let source = dep
        .source
        .as_deref()
        .map(|s| format!(" (source: {s})"))
        .unwrap_or_default();
    format!("install {}{version} manually{source}", dep.name)
}

/// Result of a built-in platform-capability check.
enum CheckResult {
    Supported,
    Unsupported { reason: String },
}

/// Dispatch a built-in `check` identifier. `None` → unknown identifier (a
/// contract bug the caller turns into [`ResolverError::UnknownCheck`]). The set
/// is intentionally small; extend it deliberately.
fn evaluate_check(
    check: &str,
    env: &ResolverEnv,
    read_filesystems: &impl Fn() -> std::io::Result<String>,
) -> Option<CheckResult> {
    match check {
        "btf" => Some(bool_fact(env.btf, "kernel BTF (/sys/kernel/btf/vmlinux)")),
        "cap_bpf" => Some(bool_fact(env.cap_bpf, "CAP_BPF capability")),
        "btrfs" => Some(btrfs_supported(read_filesystems)),
        _ => None,
    }
}

/// Map an `Option<bool>` env fact to a check result. `None` is conservative —
/// "could not determine" fails the preflight, since the bug we're fixing is a
/// silent miss.
fn bool_fact(fact: Option<bool>, label: &str) -> CheckResult {
    match fact {
        Some(true) => CheckResult::Supported,
        Some(false) => CheckResult::Unsupported {
            reason: format!("{label} is not available on this host"),
        },
        None => CheckResult::Unsupported {
            reason: format!("{label} could not be determined on this host"),
        },
    }
}

/// Whether the running kernel supports btrfs, read from `/proc/filesystems`.
fn btrfs_supported(read_filesystems: &impl Fn() -> std::io::Result<String>) -> CheckResult {
    match read_filesystems() {
        Ok(contents) if fs_supported(&contents, "btrfs") => CheckResult::Supported,
        Ok(_) => CheckResult::Unsupported {
            reason: "btrfs is not supported by the running kernel (absent from /proc/filesystems)"
                .to_string(),
        },
        Err(_) => CheckResult::Unsupported {
            reason: "could not read /proc/filesystems to verify btrfs support".to_string(),
        },
    }
}

/// Whether `proc_filesystems` content lists `fs`. Each line is `nodev\t<fs>` or
/// `\t<fs>`; the filesystem name is the trailing token.
fn fs_supported(proc_filesystems: &str, fs: &str) -> bool {
    proc_filesystems
        .lines()
        .any(|line| line.split_whitespace().last() == Some(fs))
}

/// Result of comparing the host kernel against a `min_kernel` requirement.
enum KernelCheck {
    Satisfied,
    Below { have: String },
    Unknown,
}

/// Compare the host kernel release against a minimum. Either side unparseable
/// → `Unknown` (conservative: the preflight then refuses rather than guessing).
fn kernel_satisfies(have: Option<&str>, min: &str) -> KernelCheck {
    let Some(have_raw) = have else {
        return KernelCheck::Unknown;
    };
    match (parse_kernel(have_raw), parse_kernel(min)) {
        (Some(have_v), Some(min_v)) if have_v >= min_v => KernelCheck::Satisfied,
        (Some(_), Some(_)) => KernelCheck::Below {
            have: have_raw.to_string(),
        },
        _ => KernelCheck::Unknown,
    }
}

/// Parse a kernel release's leading `MAJOR.MINOR[.PATCH]` into a semver
/// `Version`, ignoring any `-suffix`: `5.10.134-007.ali5000` → `5.10.134`,
/// `5.4` → `5.4.0`.
fn parse_kernel(s: &str) -> Option<semver::Version> {
    let head = s.split('-').next()?.trim();
    let mut parts = head.split('.');
    let major = leading_u64(parts.next()?)?;
    let minor = parts.next().and_then(leading_u64).unwrap_or(0);
    let patch = parts.next().and_then(leading_u64).unwrap_or(0);
    Some(semver::Version::new(major, minor, patch))
}

/// Leading run of ASCII digits parsed as `u64` (`"134"` from `"134abc"`).
fn leading_u64(s: &str) -> Option<u64> {
    let digits: String = s
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

/// Outcome of a presence-first version check.
enum VersionVerdict {
    /// No constraint, or constraint satisfied.
    Ok,
    /// Constraint set but unverifiable (either side unparseable). Not a failure.
    NotVerified,
    /// Constraint set, a version was found, and it does not satisfy.
    Mismatch { found: String },
}

/// Presence-first version check: only a confidently-parsed mismatch fails;
/// anything ambiguous is `NotVerified` (never silently downgraded).
fn version_verdict(constraint: Option<&str>, stdout: &str) -> VersionVerdict {
    let Some(constraint) = constraint else {
        return VersionVerdict::Ok;
    };
    let Ok(req) = semver::VersionReq::parse(constraint) else {
        return VersionVerdict::NotVerified;
    };
    let Some(found) = extract_semver(stdout) else {
        return VersionVerdict::NotVerified;
    };
    if req.matches(&found) {
        VersionVerdict::Ok
    } else {
        VersionVerdict::Mismatch {
            found: found.to_string(),
        }
    }
}

/// First whitespace-delimited token in `stdout` that parses as semver
/// (tolerating a leading `v`): `node --version` → `v20.3.1` → `20.3.1`.
fn extract_semver(stdout: &str) -> Option<semver::Version> {
    stdout.split_whitespace().find_map(|tok| {
        let t = tok.strip_prefix('v').unwrap_or(tok);
        semver::Version::parse(t).ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use anolisa_platform::command::CommandOutput;
    use std::collections::HashMap;
    use std::io;

    /// Canned outcome for one program under the fake runner.
    enum Fake {
        Ok(CommandOutput),
        Spawn(io::ErrorKind),
    }

    /// Maps a program name to a canned result. An unmapped program spawns
    /// `NotFound`, mirroring a missing binary on the host.
    #[derive(Default)]
    struct FakeRunner {
        map: HashMap<String, Fake>,
    }

    impl FakeRunner {
        fn ok(mut self, program: &str, code: i32, stdout: &str) -> Self {
            self.map.insert(
                program.to_string(),
                Fake::Ok(CommandOutput {
                    code: Some(code),
                    stdout: stdout.to_string(),
                    stderr: String::new(),
                }),
            );
            self
        }
        fn missing(mut self, program: &str) -> Self {
            self.map
                .insert(program.to_string(), Fake::Spawn(io::ErrorKind::NotFound));
            self
        }
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, program: &str, _args: &[&str]) -> io::Result<CommandOutput> {
            match self.map.get(program) {
                Some(Fake::Ok(out)) => Ok(out.clone()),
                Some(Fake::Spawn(kind)) => Err(io::Error::new(*kind, "fake spawn failure")),
                None => Err(io::Error::new(io::ErrorKind::NotFound, "missing binary")),
            }
        }
    }

    fn dep(name: &str, kind: DependencyKind) -> RuntimeDependency {
        RuntimeDependency {
            name: name.to_string(),
            kind,
            version: None,
            probe: None,
            source: None,
            packages: crate::manifest::PackageNames::default(),
            check: None,
            min_kernel: None,
        }
    }

    fn rpm_env() -> ResolverEnv {
        ResolverEnv {
            pkg_base: Some("rpm".to_string()),
            ..Default::default()
        }
    }

    fn resolve_one(
        runner: FakeRunner,
        d: RuntimeDependency,
        env: &ResolverEnv,
    ) -> DependencyResolution {
        let plan = DependencyResolver::with_runner(runner)
            .resolve(&[d], env)
            .expect("resolve");
        plan.resolutions.into_iter().next().expect("one resolution")
    }

    #[test]
    fn custom_probe_execution_failure_is_not_missing() {
        for kind in [
            DependencyKind::SystemPackage,
            DependencyKind::LanguageRuntime,
        ] {
            let mut dependency = dep("node", kind);
            dependency.probe = Some("node --version".into());
            let runner = FakeRunner {
                map: HashMap::from([("node".into(), Fake::Spawn(io::ErrorKind::PermissionDenied))]),
            };
            let result = resolve_one(runner, dependency, &rpm_env());
            assert_eq!(
                result.status,
                DependencyStatus::ProbeFailed {
                    error: DependencyProbeError::PermissionDenied {
                        command: "node".into()
                    }
                }
            );
        }
    }

    #[test]
    fn custom_probes_preserve_execution_evidence_and_order() {
        use std::cell::RefCell;
        use std::collections::VecDeque;

        struct Runner {
            calls: RefCell<Vec<(String, Vec<String>)>>,
            outputs: RefCell<VecDeque<io::Result<CommandOutput>>>,
        }
        impl CommandRunner for &Runner {
            fn run(&self, program: &str, args: &[&str]) -> io::Result<CommandOutput> {
                self.calls
                    .borrow_mut()
                    .push((program.into(), args.iter().map(|s| s.to_string()).collect()));
                self.outputs
                    .borrow_mut()
                    .pop_front()
                    .expect("unexpected probe or native fallback")
            }
        }
        for (kind, explicit) in [
            (DependencyKind::SystemPackage, true),
            (DependencyKind::LanguageRuntime, true),
            (DependencyKind::LanguageRuntime, false),
        ] {
            for case in [
                "success",
                "nonzero",
                "warning",
                "missing",
                "permission",
                "io",
                "signal",
                "stdout",
                "empty-signal",
            ] {
                let mut dependency = dep("node", kind);
                dependency.probe = explicit.then(|| "node  --version".into());
                dependency.version = Some(">=20".into());
                let probe = if explicit {
                    "node  --version"
                } else {
                    "node --version"
                };
                let output = match case {
                    "missing" => Err(io::Error::new(io::ErrorKind::NotFound, "missing node")),
                    "permission" => Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied")),
                    "io" => Err(io::Error::other(" spawn diagnostic\n")),
                    _ => Ok(CommandOutput {
                        code: match case {
                            "signal" | "stdout" | "empty-signal" => None,
                            "success" => Some(0),
                            "warning" => Some(2),
                            _ => Some(1),
                        },
                        stdout: match case {
                            "success" => "v20.1.0",
                            "stdout" => " partial version\n",
                            _ => "",
                        }
                        .into(),
                        stderr: match case {
                            "success" | "warning" => "warning\n",
                            "signal" => " terminated\n",
                            "stdout" => " \t\n",
                            _ => "",
                        }
                        .into(),
                    }),
                };
                let expected_error = match case {
                    "permission" => Some(DependencyProbeError::PermissionDenied {
                        command: "node".into(),
                    }),
                    "io" | "signal" | "empty-signal" => Some(DependencyProbeError::QueryFailed {
                        command: "node".into(),
                        code: None,
                        stderr: match case {
                            "io" => " spawn diagnostic\n",
                            "signal" => " terminated\n",
                            _ => "",
                        }
                        .into(),
                    }),
                    "stdout" => Some(DependencyProbeError::UnexpectedOutput {
                        command: "node".into(),
                        detail: format!("{probe} failed (code None); stdout:  partial version\n"),
                    }),
                    _ => None,
                };
                let runner = Runner {
                    calls: RefCell::new(Vec::new()),
                    outputs: RefCell::new(VecDeque::from([
                        output,
                        Ok(CommandOutput {
                            code: Some(0),
                            stdout: "ok".into(),
                            stderr: String::new(),
                        }),
                    ])),
                };
                let mut following = dep("following", DependencyKind::SystemPackage);
                following.probe = Some("next --check".into());
                let deps = [dependency, following];
                let plan = DependencyResolver::with_probes(&runner, || panic!("no btrfs read"))
                    .resolve(&deps, &rpm_env())
                    .expect("valid declarations");
                assert_eq!(
                    runner.calls.into_inner(),
                    vec![
                        ("node".into(), vec!["--version".into()]),
                        ("next".into(), vec!["--check".into()]),
                    ]
                );
                assert!(runner.outputs.into_inner().is_empty());
                assert_eq!(
                    plan.resolutions
                        .iter()
                        .map(|r| r.name.as_str())
                        .collect::<Vec<_>>(),
                    ["node", "following"]
                );
                assert_eq!(plan.resolutions[1].status, DependencyStatus::Resolved);
                assert!(plan.warnings.is_empty());
                let provision = crate::ProvisionPlan::from_resolution(&plan, &deps, &rpm_env());
                if let Some(error) = expected_error {
                    assert_eq!(
                        plan.resolutions[0].status,
                        DependencyStatus::ProbeFailed {
                            error: error.clone()
                        }
                    );
                    assert!(plan.unsatisfied_lines()[0].contains(&error.to_string()));
                    assert!(plan.resolutions[0].detail.is_none());
                    assert!(provision.has_blockers());
                    assert!(provision.installable.is_empty());
                    assert!(provision.manual.is_empty());
                } else if case == "success" {
                    assert_eq!(plan.resolutions[0].status, DependencyStatus::Resolved);
                    assert!(provision.is_satisfied());
                } else {
                    assert!(matches!(
                        plan.resolutions[0].status,
                        DependencyStatus::Unresolved { .. }
                    ));
                    assert!(!provision.has_blockers());
                    assert_eq!(
                        provision.installable.len(),
                        usize::from(kind == DependencyKind::SystemPackage)
                    );
                    assert_eq!(
                        provision.manual.len(),
                        usize::from(kind == DependencyKind::LanguageRuntime)
                    );
                }
            }
        }
    }

    #[test]
    fn empty_custom_probes_remain_absent_without_execution() {
        struct NoRunner;
        impl CommandRunner for NoRunner {
            fn run(&self, _: &str, _: &[&str]) -> io::Result<CommandOutput> {
                panic!("empty probe must not execute")
            }
        }
        for kind in [
            DependencyKind::SystemPackage,
            DependencyKind::LanguageRuntime,
        ] {
            for probe in ["", " \t\n"] {
                let mut dependency = dep("node", kind);
                dependency.probe = Some(probe.into());
                let plan = DependencyResolver::with_probes(NoRunner, || panic!("no btrfs read"))
                    .resolve(&[dependency], &rpm_env())
                    .unwrap();
                assert!(matches!(
                    plan.resolutions[0].status,
                    DependencyStatus::Unresolved { .. }
                ));
            }
        }
    }

    #[test]
    fn native_rpm_probe_failure_is_not_installable() {
        let runner = FakeRunner::default().ok("rpm", 2, "package btrfs-progs is not installed");
        let deps = [dep("btrfs-progs", DependencyKind::SystemPackage)];
        let plan = DependencyResolver::with_probes(runner, || panic!("no filesystem probe"))
            .resolve(&deps, &rpm_env())
            .expect("valid declaration");
        let provision = crate::ProvisionPlan::from_resolution(&plan, &deps, &rpm_env());
        assert!(provision.has_blockers());
        assert!(provision.installable.is_empty());
        assert!(!matches!(
            plan.resolutions[0].status,
            DependencyStatus::Unresolved { .. }
        ));
    }

    #[test]
    fn native_rpm_failures_retain_typed_evidence_and_declaration_order() {
        use std::cell::RefCell;
        use std::collections::VecDeque;

        struct Runner(RefCell<VecDeque<io::Result<CommandOutput>>>);
        impl CommandRunner for Runner {
            fn run(&self, program: &str, args: &[&str]) -> io::Result<CommandOutput> {
                assert_eq!(program, "rpm");
                let remaining = self.0.borrow().len();
                assert_eq!(args, ["-q", if remaining == 2 { "rpm-foo" } else { "bar" }]);
                self.0
                    .borrow_mut()
                    .pop_front()
                    .expect("one query per dependency")
            }
        }
        let mut first = dep("foo", DependencyKind::SystemPackage);
        first.packages.rpm = Some("rpm-foo".into());
        let mut platform = dep("kernel", DependencyKind::PlatformCapability);
        platform.min_kernel = Some("1.0".into());
        let deps = [first, dep("bar", DependencyKind::SystemPackage), platform];
        let mut env = rpm_env();
        env.kernel = Some("6.1.0".into());
        let cases = vec![
            (
                Err(io::Error::new(io::ErrorKind::NotFound, "missing")),
                DependencyProbeError::CommandMissing {
                    command: "rpm".into(),
                },
            ),
            (
                Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied")),
                DependencyProbeError::PermissionDenied {
                    command: "rpm".into(),
                },
            ),
            (
                Err(io::Error::other("io detail")),
                DependencyProbeError::QueryFailed {
                    command: "rpm".into(),
                    code: None,
                    stderr: "io detail".into(),
                },
            ),
            (
                Ok(CommandOutput {
                    code: Some(1),
                    stdout: "package rpm-foo is not installed\n".into(),
                    stderr: " rpmdb unavailable\n".into(),
                }),
                DependencyProbeError::QueryFailed {
                    command: "rpm".into(),
                    code: Some(1),
                    stderr: " rpmdb unavailable\n".into(),
                },
            ),
            (
                Ok(CommandOutput {
                    code: None,
                    stdout: String::new(),
                    stderr: "terminated".into(),
                }),
                DependencyProbeError::QueryFailed {
                    command: "rpm".into(),
                    code: None,
                    stderr: "terminated".into(),
                },
            ),
        ];
        for (output, expected) in cases {
            let runner = Runner(RefCell::new(VecDeque::from([
                output,
                Ok(CommandOutput {
                    code: Some(1),
                    stdout: "package bar is not installed\n".into(),
                    stderr: String::new(),
                }),
            ])));
            let resolver = DependencyResolver::with_probes(runner, || panic!("no btrfs read"));
            let plan = resolver.resolve(&deps, &env).expect("valid declarations");
            assert_eq!(
                plan.resolutions
                    .iter()
                    .map(|r| r.name.as_str())
                    .collect::<Vec<_>>(),
                ["foo", "bar", "kernel"]
            );
            assert_eq!(
                plan.resolutions[0].status,
                DependencyStatus::ProbeFailed {
                    error: expected.clone()
                }
            );
            assert_eq!(plan.clone().resolutions, plan.resolutions);
            assert!(matches!(
                plan.resolutions[1].status,
                DependencyStatus::Unresolved { .. }
            ));
            assert_eq!(plan.resolutions[2].status, DependencyStatus::Resolved);
            assert!(plan.unsatisfied_lines()[0].contains(&expected.to_string()));
            let provision = crate::ProvisionPlan::from_resolution(&plan, &deps, &env);
            assert!(provision.has_blockers());
            assert_eq!(provision.installable_package_names(), ["bar"]);
            assert_eq!(provision.unresolvable.len(), 1);
            assert!(provision.manual.is_empty());
            assert!(resolver.runner.0.borrow().is_empty());
        }
        let unexpected = DependencyProbeError::from(PackageQueryError::UnexpectedOutput {
            command: "rpm".into(),
            detail: "bad metadata".into(),
        });
        assert_eq!(
            unexpected,
            DependencyProbeError::UnexpectedOutput {
                command: "rpm".into(),
                detail: "bad metadata".into()
            }
        );
    }

    #[test]
    fn explicit_probe_and_dpkg_keep_their_presence_policy() {
        let mut explicit = dep("foo", DependencyKind::SystemPackage);
        explicit.probe = Some("grep --version".into());
        let env = rpm_env();
        let result = resolve_one(FakeRunner::default().ok("grep", 1, ""), explicit, &env);
        assert!(matches!(result.status, DependencyStatus::Unresolved { .. }));
        let deb = ResolverEnv {
            pkg_base: Some("deb".into()),
            ..Default::default()
        };
        let result = resolve_one(
            FakeRunner::default().missing("dpkg"),
            dep("foo", DependencyKind::SystemPackage),
            &deb,
        );
        assert!(matches!(result.status, DependencyStatus::Unresolved { .. }));
    }

    #[test]
    fn system_package_present_is_resolved() {
        let mut d = dep("btrfs-progs", DependencyKind::SystemPackage);
        d.probe = Some("btrfs version".to_string());
        // Only the probe binary is faked — if native `rpm -q` were used instead,
        // it would be missing and the dep would be unresolved. Resolving proves
        // the explicit probe is preferred.
        let r = resolve_one(
            FakeRunner::default().ok("btrfs", 0, "btrfs-progs v6.6"),
            d,
            &rpm_env(),
        );
        assert_eq!(r.status, DependencyStatus::Resolved);
    }

    #[test]
    fn system_package_missing_rpm_remediation() {
        let mut d = dep("btrfs-progs", DependencyKind::SystemPackage);
        d.probe = Some("btrfs version".to_string());
        d.packages.rpm = Some("btrfs-progs".to_string());
        let r = resolve_one(FakeRunner::default().missing("btrfs"), d, &rpm_env());
        assert_eq!(
            r.status,
            DependencyStatus::Unresolved {
                remediation: "sudo dnf install btrfs-progs".to_string()
            }
        );
    }

    #[test]
    fn system_package_missing_deb_remediation() {
        let mut d = dep("btrfs-progs", DependencyKind::SystemPackage);
        d.probe = Some("btrfs version".to_string());
        d.packages.deb = Some("btrfs-progs".to_string());
        let env = ResolverEnv {
            pkg_base: Some("deb".to_string()),
            ..Default::default()
        };
        let r = resolve_one(FakeRunner::default().missing("btrfs"), d, &env);
        assert_eq!(
            r.status,
            DependencyStatus::Unresolved {
                remediation: "sudo apt install btrfs-progs".to_string()
            }
        );
    }

    #[test]
    fn system_package_unknown_pkg_base_manual_hint() {
        let mut d = dep("btrfs-progs", DependencyKind::SystemPackage);
        d.probe = Some("btrfs version".to_string());
        let env = ResolverEnv::default(); // pkg_base = None
        let r = resolve_one(FakeRunner::default().missing("btrfs"), d, &env);
        match r.status {
            DependencyStatus::Unresolved { remediation } => {
                assert!(
                    remediation.contains("unsupported package manager"),
                    "{remediation}"
                );
            }
            other => panic!("expected unresolved, got {other:?}"),
        }
    }

    #[test]
    fn system_package_native_query_present_when_no_probe() {
        let d = dep("btrfs-progs", DependencyKind::SystemPackage);
        // No probe → native `rpm -q` path; exit 0 means installed.
        let r = resolve_one(FakeRunner::default().ok("rpm", 0, ""), d, &rpm_env());
        assert_eq!(r.status, DependencyStatus::Resolved);
    }

    #[test]
    fn system_package_native_query_absent_when_no_probe() {
        let d = dep("btrfs-progs", DependencyKind::SystemPackage);
        let r = resolve_one(
            FakeRunner::default().ok("rpm", 1, "package btrfs-progs is not installed\n"),
            d,
            &rpm_env(),
        );
        assert!(matches!(r.status, DependencyStatus::Unresolved { .. }));
    }

    #[test]
    fn platform_capability_min_kernel_below_is_unresolvable() {
        let mut d = dep("btrfs", DependencyKind::PlatformCapability);
        d.min_kernel = Some("5.4".to_string());
        let env = ResolverEnv {
            kernel: Some("3.10.0-1160.el7".to_string()),
            ..Default::default()
        };
        let r = resolve_one(FakeRunner::default(), d, &env);
        match r.status {
            DependencyStatus::Unresolvable { reason } => {
                assert!(reason.contains("requires kernel >= 5.4"), "{reason}");
            }
            other => panic!("expected unresolvable, got {other:?}"),
        }
    }

    #[test]
    fn platform_capability_min_kernel_satisfied_is_resolved() {
        let mut d = dep("btrfs", DependencyKind::PlatformCapability);
        d.min_kernel = Some("5.4".to_string());
        let env = ResolverEnv {
            kernel: Some("5.10.134-007.ali5000.al8.x86_64".to_string()),
            ..Default::default()
        };
        let r = resolve_one(FakeRunner::default(), d, &env);
        assert_eq!(r.status, DependencyStatus::Resolved);
    }

    #[test]
    fn platform_capability_min_kernel_unknown_host_is_unresolvable() {
        let mut d = dep("btrfs", DependencyKind::PlatformCapability);
        d.min_kernel = Some("5.4".to_string());
        let env = ResolverEnv::default(); // kernel = None
        let r = resolve_one(FakeRunner::default(), d, &env);
        assert!(matches!(r.status, DependencyStatus::Unresolvable { .. }));
    }

    #[test]
    fn platform_capability_btf_supported_and_missing() {
        let mut d = dep("ebpf", DependencyKind::PlatformCapability);
        d.check = Some("btf".to_string());
        let yes = ResolverEnv {
            btf: Some(true),
            ..Default::default()
        };
        let no = ResolverEnv {
            btf: Some(false),
            ..Default::default()
        };
        let none = ResolverEnv {
            btf: None,
            ..Default::default()
        };
        assert_eq!(
            resolve_one(FakeRunner::default(), d.clone(), &yes).status,
            DependencyStatus::Resolved
        );
        assert!(matches!(
            resolve_one(FakeRunner::default(), d.clone(), &no).status,
            DependencyStatus::Unresolvable { .. }
        ));
        assert!(matches!(
            resolve_one(FakeRunner::default(), d, &none).status,
            DependencyStatus::Unresolvable { .. }
        ));
    }

    #[test]
    fn platform_capability_unknown_check_is_error() {
        let mut d = dep("frob", DependencyKind::PlatformCapability);
        d.check = Some("frobnicate".to_string());
        let err = DependencyResolver::with_runner(FakeRunner::default())
            .resolve(&[d], &ResolverEnv::default())
            .expect_err("unknown check must error");
        assert!(matches!(err, ResolverError::UnknownCheck { .. }));
    }

    #[test]
    fn language_runtime_present_satisfies_version() {
        let mut d = dep("node", DependencyKind::LanguageRuntime);
        d.version = Some(">=20".to_string());
        let r = resolve_one(
            FakeRunner::default().ok("node", 0, "v20.3.1"),
            d,
            &ResolverEnv::default(),
        );
        assert_eq!(r.status, DependencyStatus::Resolved);
    }

    #[test]
    fn language_runtime_version_mismatch_is_unresolved() {
        let mut d = dep("node", DependencyKind::LanguageRuntime);
        d.version = Some(">=20".to_string());
        let r = resolve_one(
            FakeRunner::default().ok("node", 0, "v18.19.0"),
            d,
            &ResolverEnv::default(),
        );
        assert!(matches!(r.status, DependencyStatus::Unresolved { .. }));
        assert!(r.detail.unwrap().contains("found 18.19.0"));
    }

    #[test]
    fn language_runtime_missing_reports_manual_hint_no_install_cmd() {
        let mut d = dep("node", DependencyKind::LanguageRuntime);
        d.version = Some(">=20".to_string());
        d.source = Some("nodejs-official".to_string());
        let r = resolve_one(
            FakeRunner::default().missing("node"),
            d,
            &ResolverEnv::default(),
        );
        match r.status {
            DependencyStatus::Unresolved { remediation } => {
                assert!(
                    remediation.contains("install node >=20 manually"),
                    "{remediation}"
                );
                assert!(remediation.contains("nodejs-official"), "{remediation}");
                // No package-manager install is issued for a language runtime.
                assert!(
                    !remediation.contains("dnf") && !remediation.contains("apt"),
                    "{remediation}"
                );
            }
            other => panic!("expected unresolved, got {other:?}"),
        }
    }

    #[test]
    fn language_runtime_unparseable_version_does_not_fail() {
        let mut d = dep("node", DependencyKind::LanguageRuntime);
        d.version = Some("latest".to_string()); // not a semver req
        let r = resolve_one(
            FakeRunner::default().ok("node", 0, "v20.3.1"),
            d,
            &ResolverEnv::default(),
        );
        assert_eq!(r.status, DependencyStatus::Resolved);
        assert!(r.detail.unwrap().contains("not verified"));
    }

    #[test]
    fn aggregate_fails_and_lists_all_missing() {
        let mut present = dep("present-pkg", DependencyKind::SystemPackage);
        present.probe = Some("present-tool x".to_string());
        let mut missing = dep("btrfs-progs", DependencyKind::SystemPackage);
        missing.probe = Some("btrfs version".to_string());
        missing.packages.rpm = Some("btrfs-progs".to_string());
        let mut cap = dep("btrfs", DependencyKind::PlatformCapability);
        cap.min_kernel = Some("5.4".to_string());

        let env = ResolverEnv {
            pkg_base: Some("rpm".to_string()),
            kernel: Some("3.10.0-1160".to_string()),
            ..Default::default()
        };
        let plan = DependencyResolver::with_runner(
            FakeRunner::default()
                .ok("present-tool", 0, "ok")
                .missing("btrfs"),
        )
        .resolve(&[present, missing, cap], &env)
        .expect("resolve");

        assert!(!plan.is_satisfied());
        let lines = plan.unsatisfied_lines();
        assert_eq!(lines.len(), 2, "both misses listed: {lines:?}");
        assert!(lines.iter().any(|l| l.contains("btrfs-progs")));
        assert!(
            lines
                .iter()
                .any(|l| l.contains("btrfs [platform-capability]"))
        );
    }

    #[test]
    fn aggregate_all_present_is_satisfied() {
        let mut a = dep("a", DependencyKind::SystemPackage);
        a.probe = Some("a v".to_string());
        let mut b = dep("b", DependencyKind::PlatformCapability);
        b.check = Some("btf".to_string());
        let env = ResolverEnv {
            btf: Some(true),
            ..Default::default()
        };
        let plan = DependencyResolver::with_runner(FakeRunner::default().ok("a", 0, "1"))
            .resolve(&[a, b], &env)
            .expect("resolve");
        assert!(plan.is_satisfied());
        assert!(plan.unsatisfied_lines().is_empty());
    }

    #[test]
    fn fs_supported_matches_trailing_token() {
        let procfs = "nodev\tsysfs\nnodev\ttmpfs\n\text4\n\tbtrfs\n";
        assert!(fs_supported(procfs, "btrfs"));
        assert!(fs_supported(procfs, "ext4"));
        assert!(!fs_supported(procfs, "xfs"));
    }

    #[test]
    fn btrfs_probe_classifies_injected_filesystems() {
        for (contents, supported) in [
            ("nodev\tsysfs\n\tbtrfs\n", true),
            ("nodev\tbtrfs\n", true),
            ("\text4\n", false),
            ("", false),
            ("\tbtrfs_backup\n\tmybtrfs\nbtrfs\text4\n", false),
        ] {
            let reads = std::cell::Cell::new(0);
            let resolver = DependencyResolver::with_probes(FakeRunner::default(), || {
                reads.set(reads.get() + 1);
                Ok(contents.to_string())
            });
            assert_eq!(reads.get(), 0, "construction must not read the host");
            let mut dependency = dep("kernel-btrfs", DependencyKind::PlatformCapability);
            dependency.check = Some("btrfs".to_string());
            let plan = resolver
                .resolve(&[dependency], &rpm_env())
                .expect("resolve");
            assert_eq!(reads.get(), 1);
            assert_eq!(plan.resolutions[0].detail, None);
            assert_eq!(
                plan.resolutions[0].status,
                if supported {
                    DependencyStatus::Resolved
                } else {
                    DependencyStatus::Unresolvable {
                        reason: "btrfs is not supported by the running kernel (absent from /proc/filesystems)".to_string(),
                    }
                },
                "{contents:?}"
            );
        }
    }

    #[test]
    fn btrfs_probe_read_failure_keeps_existing_reason() {
        for kind in [
            io::ErrorKind::NotFound,
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::InvalidData,
        ] {
            let reads = std::cell::Cell::new(0);
            let resolver = DependencyResolver::with_probes(FakeRunner::default(), || {
                reads.set(reads.get() + 1);
                Err(io::Error::new(kind, "scripted filesystem read error"))
            });
            let mut dependency = dep("kernel-btrfs", DependencyKind::PlatformCapability);
            dependency.check = Some("btrfs".to_string());
            let plan = resolver
                .resolve(&[dependency], &rpm_env())
                .expect("read error is evidence");
            assert_eq!(reads.get(), 1);
            assert_eq!(
                plan.resolutions[0].status,
                DependencyStatus::Unresolvable {
                    reason: "could not read /proc/filesystems to verify btrfs support".to_string(),
                }
            );
        }
    }

    #[test]
    fn unrelated_checks_and_kernel_gates_never_read_filesystems() {
        let resolver = DependencyResolver::with_probes(
            FakeRunner::default()
                .ok("rpm", 0, "")
                .ok("node", 0, "v20.0.0"),
            || -> io::Result<String> { panic!("filesystem reader must not run") },
        );
        assert!(
            resolver
                .resolve(&[], &rpm_env())
                .expect("empty")
                .is_satisfied()
        );
        for kind in [
            DependencyKind::SystemPackage,
            DependencyKind::LanguageRuntime,
        ] {
            assert!(
                resolver
                    .resolve(&[dep("node", kind)], &rpm_env())
                    .expect("command")
                    .is_satisfied()
            );
        }
        for check in [None, Some("btf"), Some("cap_bpf")] {
            let mut dependency = dep("capability", DependencyKind::PlatformCapability);
            dependency.check = check.map(str::to_string);
            let env = ResolverEnv {
                btf: Some(true),
                cap_bpf: Some(true),
                ..rpm_env()
            };
            assert!(
                resolver
                    .resolve(&[dependency], &env)
                    .expect("capability")
                    .is_satisfied()
            );
        }
        for kernel in [None, Some("3.10.0")] {
            let mut dependency = dep("kernel-btrfs", DependencyKind::PlatformCapability);
            dependency.check = Some("btrfs".to_string());
            dependency.min_kernel = Some("5.4".to_string());
            let env = ResolverEnv {
                kernel: kernel.map(str::to_string),
                ..rpm_env()
            };
            assert!(
                !resolver
                    .resolve(&[dependency], &env)
                    .expect("kernel gate")
                    .is_satisfied()
            );
        }
        let mut invalid = dep("future-capability", DependencyKind::PlatformCapability);
        invalid.check = Some("unknown".to_string());
        assert!(matches!(
            resolver.resolve(&[invalid], &rpm_env()),
            Err(ResolverError::UnknownCheck { .. })
        ));
    }

    #[test]
    fn dependency_probes_preserve_order_and_repeat_reads() {
        use std::cell::RefCell;

        struct RecordingRunner<'a>(&'a RefCell<Vec<String>>);
        impl CommandRunner for RecordingRunner<'_> {
            fn run(&self, program: &str, args: &[&str]) -> io::Result<CommandOutput> {
                self.0
                    .borrow_mut()
                    .push(format!("{program} {}", args.join(" ")));
                Ok(CommandOutput {
                    code: Some(0),
                    stdout: String::new(),
                    stderr: String::new(),
                })
            }
        }
        let events = RefCell::new(Vec::new());
        let reads = std::cell::Cell::new(0);
        let resolver = DependencyResolver::with_probes(RecordingRunner(&events), || {
            events.borrow_mut().push("filesystems".to_string());
            reads.set(reads.get() + 1);
            Ok(if reads.get() <= 2 {
                "btrfs\n"
            } else {
                "ext4\n"
            }
            .to_string())
        });
        let mut capability = dep("kernel-btrfs", DependencyKind::PlatformCapability);
        capability.check = Some("btrfs".to_string());
        capability.min_kernel = Some("5.4".to_string());
        let env = ResolverEnv {
            kernel: Some("5.10.0".to_string()),
            ..rpm_env()
        };
        let deps = [
            capability.clone(),
            dep("tool", DependencyKind::SystemPackage),
            capability,
        ];
        assert!(
            resolver
                .resolve(&deps, &env)
                .expect("first pass")
                .is_satisfied()
        );
        assert!(
            !resolver
                .resolve(&deps, &env)
                .expect("second pass")
                .is_satisfied()
        );
        assert_eq!(reads.get(), 4);
        assert_eq!(
            *events.borrow(),
            [
                "filesystems",
                "rpm -q tool",
                "filesystems",
                "filesystems",
                "rpm -q tool",
                "filesystems"
            ]
        );
    }

    #[test]
    fn parse_kernel_handles_vendor_suffixes() {
        assert_eq!(
            parse_kernel("5.10.134-007.ali5000.al8.x86_64"),
            Some(semver::Version::new(5, 10, 134))
        );
        assert_eq!(parse_kernel("5.4"), Some(semver::Version::new(5, 4, 0)));
        assert_eq!(
            parse_kernel("3.10.0-1160.el7"),
            Some(semver::Version::new(3, 10, 0))
        );
    }
}
