//! Fail-quiet startup detection for available cosh-ng upgrades.

use std::cmp::Ordering;
use std::env;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use nix::sys::signal::{killpg, Signal};
use nix::unistd::Pid;
use semver::Version;
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use wait_timeout::ChildExt;

const CACHE_VERSION: u32 = 2;
const COMPONENT: &str = "cosh-ng";
const RPM_PACKAGE: &str = "cosh-ng";
const MAX_CACHE_BYTES: u64 = 64 * 1024;
const MAX_PIPE_OUTPUT_BYTES: usize = 32 * 1024 * 1024;
const ANOLISA_TIMEOUT: Duration = Duration::from_secs(45);
#[cfg(target_os = "linux")]
const RPM_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(target_os = "linux")]
const PACKAGE_MANAGER_TIMEOUT: Duration = Duration::from_secs(30);

/// Upgrade information ready for a startup banner or deferred notice.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub(crate) struct UpgradeNotice {
    pub(crate) package: String,
    pub(crate) current: String,
    pub(crate) latest: String,
    pub(crate) command: String,
}

/// Result of an upgrade detector that completed successfully or failed quietly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum UpgradeVerdict {
    Upgrade(UpgradeNotice),
    UpToDate,
    Unavailable,
}

/// Startup-owned receiver state for the non-blocking upgrade probe.
#[derive(Default)]
pub(crate) struct StartupUpgradeState {
    pub(crate) pending: Option<mpsc::Receiver<Option<UpgradeNotice>>>,
    pub(crate) resolved: Option<Option<UpgradeNotice>>,
    pub(crate) rendered: bool,
}

impl StartupUpgradeState {
    pub(crate) fn wait_ready(&mut self, timeout: Duration) {
        if self.resolved.is_some() {
            return;
        }
        let Some(receiver) = &self.pending else {
            return;
        };
        match receiver.recv_timeout(timeout) {
            Ok(notice) => {
                self.resolved = Some(notice);
                self.pending = None;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                self.pending = None;
            }
        }
    }

    pub(crate) fn poll_ready(&mut self) {
        if self.resolved.is_some() {
            return;
        }
        let Some(receiver) = &self.pending else {
            return;
        };
        match receiver.try_recv() {
            Ok(notice) => {
                self.resolved = Some(notice);
                self.pending = None;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                self.pending = None;
            }
        }
    }

    pub(crate) fn notice(&self) -> Option<&UpgradeNotice> {
        self.resolved.as_ref().and_then(Option::as_ref)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum DetectionMethod {
    Anolisa,
    Rpm,
}

#[derive(Debug, Deserialize, Serialize)]
struct UpgradeCache {
    version: u32,
    checked_at: u64,
    executable: PathBuf,
    method: DetectionMethod,
    notice: Option<UpgradeNotice>,
}

struct DetectionResult {
    executable: PathBuf,
    method: DetectionMethod,
    verdict: UpgradeVerdict,
}

/// Starts the fail-quiet startup probe on a named worker thread.
pub(crate) fn spawn_startup_upgrade_probe(
    running_version: &'static str,
) -> mpsc::Receiver<Option<UpgradeNotice>> {
    let (sender, receiver) = mpsc::sync_channel(1);
    let _ = thread::Builder::new()
        .name("cosh-startup-upgrade-probe".to_string())
        .spawn(move || {
            let result = current_executable()
                .and_then(|executable| detect_upgrade(running_version, &executable));
            let notice = match &result {
                Some(DetectionResult {
                    verdict: UpgradeVerdict::Upgrade(notice),
                    ..
                }) => Some(notice.clone()),
                _ => None,
            };
            if let Some(result) = result {
                if !matches!(result.verdict, UpgradeVerdict::Unavailable) {
                    let _ = write_cache(&result);
                }
            }
            let _ = sender.send(notice);
        });
    receiver
}

/// Reads a cached notice and suppresses it once the running binary catches up.
pub(crate) fn read_cached_notice(running_version: &str) -> Option<UpgradeNotice> {
    let path = cache_path()?;
    if fs::metadata(&path).ok()?.len() > MAX_CACHE_BYTES {
        return None;
    }
    let bytes = fs::read(path).ok()?;
    let cache: UpgradeCache = serde_json::from_slice(&bytes).ok()?;
    if cache.version != CACHE_VERSION || cache.executable != current_executable()? {
        return None;
    }
    let mut notice = cache.notice?;
    if notice.package.trim().is_empty()
        || notice.latest.trim().is_empty()
        || notice.command.trim().is_empty()
    {
        return None;
    }
    match compare_versions(&notice.latest, running_version)? {
        Ordering::Greater => {
            notice.current = running_version.to_string();
            Some(notice)
        }
        Ordering::Equal | Ordering::Less => None,
    }
}

/// Returns false only for explicit opt-out values.
pub(crate) fn startup_upgrade_check_enabled_for_env() -> bool {
    !env::var("COSH_SHELL_UPGRADE_CHECK")
        .ok()
        .is_some_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "off" | "0" | "false" | "no"
            )
        })
}

fn detect_upgrade(running_version: &str, executable: &Path) -> Option<DetectionResult> {
    if let Some(anolisa) = locate_managed_anolisa(executable) {
        let verdict = detect_with_anolisa(&anolisa, COMPONENT, running_version)?;
        if matches!(verdict, UpgradeVerdict::Unavailable) {
            return None;
        }
        return Some(DetectionResult {
            executable: executable.to_path_buf(),
            method: DetectionMethod::Anolisa,
            verdict,
        });
    }

    let package = rpm_package_for_executable(executable)?;
    if let Some(anolisa) = locate_anolisa() {
        match detect_with_anolisa(&anolisa, COMPONENT, running_version) {
            Some(verdict) if !matches!(verdict, UpgradeVerdict::Unavailable) => {
                return Some(DetectionResult {
                    executable: executable.to_path_buf(),
                    method: DetectionMethod::Anolisa,
                    verdict,
                });
            }
            Some(UpgradeVerdict::Unavailable) => {
                let verdict =
                    detect_with_rpm(&package, running_version, Some("anolisa update cosh-ng"))?;
                return Some(DetectionResult {
                    executable: executable.to_path_buf(),
                    method: DetectionMethod::Anolisa,
                    verdict,
                });
            }
            Some(_) | None => {}
        }
    }

    detect_with_rpm(&package, running_version, None).map(|verdict| DetectionResult {
        executable: executable.to_path_buf(),
        method: DetectionMethod::Rpm,
        verdict,
    })
}

fn detect_with_anolisa(
    program: &Path,
    component: &str,
    running_version: &str,
) -> Option<UpgradeVerdict> {
    let mut command = Command::new(program);
    command.args(["update", component, "--dry-run", "--json"]);
    let output = run_bounded_command(command, ANOLISA_TIMEOUT)?;
    output.status.success().then_some(())?;
    parse_anolisa_output(&output.stdout, component, running_version)
}

#[derive(Deserialize)]
struct AnolisaEnvelope {
    ok: bool,
    data: AnolisaUpdateData,
}

#[derive(Deserialize)]
struct AnolisaUpdateData {
    #[serde(default)]
    package: Option<String>,
    #[serde(default)]
    to_version: Option<String>,
    plan: Vec<String>,
}

fn parse_anolisa_output(
    bytes: &[u8],
    component: &str,
    running_version: &str,
) -> Option<UpgradeVerdict> {
    let envelope: AnolisaEnvelope = serde_json::from_slice(bytes).ok()?;
    if !envelope.ok {
        return None;
    }
    let data = envelope.data;
    if data.plan.is_empty() {
        return Some(UpgradeVerdict::UpToDate);
    }
    let Some(latest) = data.to_version.filter(|version| !version.trim().is_empty()) else {
        return Some(UpgradeVerdict::Unavailable);
    };
    match compare_versions(&latest, running_version) {
        Some(Ordering::Greater) => {}
        Some(Ordering::Equal | Ordering::Less) => return Some(UpgradeVerdict::UpToDate),
        None => return Some(UpgradeVerdict::Unavailable),
    }
    Some(UpgradeVerdict::Upgrade(UpgradeNotice {
        package: data.package.unwrap_or_else(|| component.to_string()),
        current: running_version.to_string(),
        latest,
        command: "anolisa update cosh-ng".to_string(),
    }))
}

#[cfg(target_os = "linux")]
fn rpm_package_for_executable(executable: &Path) -> Option<String> {
    let rpm = find_in_path(OsStr::new("rpm"))?;
    let mut ownership = Command::new(rpm);
    ownership.args([OsStr::new("-qf"), OsStr::new("--qf"), OsStr::new("%{NAME}")]);
    ownership.arg(executable);
    let output = run_bounded_command(ownership, RPM_TIMEOUT)?;
    output
        .status
        .success()
        .then(|| parse_rpm_package_name(&output.stdout))
        .flatten()
}

#[cfg(not(target_os = "linux"))]
fn rpm_package_for_executable(_executable: &Path) -> Option<String> {
    None
}

fn parse_rpm_package_name(stdout: &[u8]) -> Option<String> {
    let package = std::str::from_utf8(stdout).ok()?.trim();
    (package == RPM_PACKAGE).then(|| package.to_string())
}

#[cfg(target_os = "linux")]
fn detect_with_rpm(
    package: &str,
    running_version: &str,
    upgrade_command: Option<&str>,
) -> Option<UpgradeVerdict> {
    let (manager, program) = if let Some(path) = find_in_path(OsStr::new("dnf")) {
        ("dnf", path)
    } else {
        ("yum", find_in_path(OsStr::new("yum"))?)
    };
    check_package_manager(&program, manager, package, running_version, upgrade_command)
}

#[cfg(not(target_os = "linux"))]
fn detect_with_rpm(
    _package: &str,
    _running_version: &str,
    _upgrade_command: Option<&str>,
) -> Option<UpgradeVerdict> {
    None
}

#[cfg(target_os = "linux")]
fn check_package_manager(
    program: &Path,
    manager: &str,
    package: &str,
    running_version: &str,
    upgrade_command: Option<&str>,
) -> Option<UpgradeVerdict> {
    let mut command = Command::new(program);
    command.args(["check-update", package]);
    let output = run_bounded_command(command, PACKAGE_MANAGER_TIMEOUT)?;
    classify_package_manager_output(
        output.status.code(),
        &output.stdout,
        package,
        running_version,
        manager,
        upgrade_command,
    )
}

fn classify_package_manager_output(
    status: Option<i32>,
    stdout: &[u8],
    package: &str,
    running_version: &str,
    manager: &str,
    upgrade_command: Option<&str>,
) -> Option<UpgradeVerdict> {
    match status {
        Some(0) => Some(UpgradeVerdict::UpToDate),
        Some(100) => {
            let latest = parse_package_candidate(stdout, package)?;
            Some(UpgradeVerdict::Upgrade(UpgradeNotice {
                package: package.to_string(),
                current: running_version.to_string(),
                latest,
                command: upgrade_command
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("sudo {manager} update {package}")),
            }))
        }
        _ => None,
    }
}

fn parse_package_candidate(stdout: &[u8], package: &str) -> Option<String> {
    let stdout = std::str::from_utf8(stdout).ok()?;
    stdout.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let name = fields.next()?;
        let matches = name == package
            || name
                .strip_prefix(package)
                .is_some_and(|suffix| suffix.starts_with('.'));
        matches.then(|| fields.next().map(str::to_string)).flatten()
    })
}

fn current_executable() -> Option<PathBuf> {
    let executable = env::current_exe().ok()?;
    Some(fs::canonicalize(&executable).unwrap_or(executable))
}

fn locate_managed_anolisa(executable: &Path) -> Option<PathBuf> {
    let candidate = anolisa_candidate_for_managed_executable(executable)?;
    if let Some(path) = configured_anolisa() {
        return Some(path);
    }
    is_executable_file(&candidate).then_some(candidate)
}

fn locate_anolisa() -> Option<PathBuf> {
    configured_anolisa().or_else(|| find_in_path(OsStr::new("anolisa")))
}

fn configured_anolisa() -> Option<PathBuf> {
    let path = PathBuf::from(env::var_os("COSH_SHELL_ANOLISA_BIN")?);
    is_executable_file(&path).then_some(path)
}

fn anolisa_candidate_for_managed_executable(executable: &Path) -> Option<PathBuf> {
    if executable.file_name()? != "cosh-shell" {
        return None;
    }
    let cosh_ng = executable.parent()?;
    if cosh_ng.file_name()? != "cosh-ng" {
        return None;
    }
    let parent = cosh_ng.parent()?;

    if parent.file_name()? == "anolisa" {
        let libexec = parent.parent()?;
        let local = libexec.parent()?;
        if libexec.file_name()? == "libexec"
            && local.file_name()? == "local"
            && local.parent()?.file_name()? == "usr"
        {
            return Some(local.join("bin/anolisa"));
        }
    }

    if parent.file_name()? == "libexec" {
        let anolisa = parent.parent()?;
        let lib = anolisa.parent()?;
        let local = lib.parent()?;
        if anolisa.file_name()? == "anolisa"
            && lib.file_name()? == "lib"
            && local.file_name()? == ".local"
        {
            return Some(local.join("bin/anolisa"));
        }
    }

    None
}

fn find_in_path(program: &OsStr) -> Option<PathBuf> {
    let paths = env::var_os("PATH")?;
    env::split_paths(&paths)
        .map(|directory| directory.join(program))
        .find(|path| is_executable_file(path))
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

struct CommandOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
}

fn run_bounded_command(mut command: Command, timeout: Duration) -> Option<CommandOutput> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = command.spawn().ok()?;
    let process_group = child.id();
    let stdout = child.stdout.take()?;
    let stderr = child.stderr.take()?;
    let stdout_reader = thread::spawn(move || read_bounded(stdout));
    let stderr_reader = thread::spawn(move || read_bounded(stderr));
    let status = match child.wait_timeout(timeout).ok()? {
        Some(status) => {
            // A command that exited may still have descendants holding its pipes.
            let _ = killpg(Pid::from_raw(process_group as i32), Signal::SIGKILL);
            status
        }
        None => {
            let _ = killpg(Pid::from_raw(process_group as i32), Signal::SIGKILL);
            let _ = child.kill();
            let _ = child.wait();
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            return None;
        }
    };
    let (stdout, stdout_truncated) = stdout_reader.join().ok()?.ok()?;
    let (_, stderr_truncated) = stderr_reader.join().ok()?.ok()?;
    if stdout_truncated || stderr_truncated {
        return None;
    }
    Some(CommandOutput { status, stdout })
}

fn read_bounded(mut reader: impl Read) -> io::Result<(Vec<u8>, bool)> {
    let mut output = Vec::new();
    let mut buffer = [0_u8; 8192];
    let mut truncated = false;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let remaining = MAX_PIPE_OUTPUT_BYTES.saturating_sub(output.len());
        output.extend_from_slice(&buffer[..read.min(remaining)]);
        truncated |= read > remaining;
    }
    Ok((output, truncated))
}

fn cache_path() -> Option<PathBuf> {
    if let Some(path) = env::var_os("COSH_SHELL_UPGRADE_CHECK_CACHE") {
        return Some(PathBuf::from(path));
    }
    env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".copilot-shell/cosh/upgrade-check.json"))
}

fn write_cache(result: &DetectionResult) -> io::Result<()> {
    let custom_path = env::var_os("COSH_SHELL_UPGRADE_CHECK_CACHE").is_some();
    let Some(path) = cache_path() else {
        return Ok(());
    };
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent_existed = parent.exists();
    fs::create_dir_all(parent)?;
    #[cfg(unix)]
    if !custom_path || !parent_existed {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    }
    let cache = UpgradeCache {
        version: CACHE_VERSION,
        checked_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        executable: result.executable.clone(),
        method: result.method,
        notice: match &result.verdict {
            UpgradeVerdict::Upgrade(notice) => Some(notice.clone()),
            UpgradeVerdict::UpToDate | UpgradeVerdict::Unavailable => None,
        },
    };
    let mut temp = NamedTempFile::new_in(parent)?;
    #[cfg(unix)]
    temp.as_file().set_permissions({
        use std::os::unix::fs::PermissionsExt;
        fs::Permissions::from_mode(0o600)
    })?;
    serde_json::to_writer(temp.as_file_mut(), &cache)?;
    temp.as_file_mut().write_all(b"\n")?;
    temp.as_file_mut().flush()?;
    temp.as_file().sync_all()?;
    temp.persist(&path).map_err(|error| error.error)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn compare_versions(left: &str, right: &str) -> Option<Ordering> {
    fn parse(version: &str) -> Option<Version> {
        let version = version.trim();
        Version::parse(version.strip_prefix('v').unwrap_or(version)).ok()
    }

    Some(parse(left)?.cmp(&parse(right)?))
}

#[cfg(test)]
mod tests;
