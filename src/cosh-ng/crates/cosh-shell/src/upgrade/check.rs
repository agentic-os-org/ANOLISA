//! Fail-quiet startup detection for available cosh-ng upgrades.

use std::cmp::Ordering;
use std::env;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use semver::Version;
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

use runner::{run_bounded_command, ProbeProcesses};
pub(crate) use state::StartupUpgradeState;

mod runner;
mod state;

const CACHE_VERSION: u32 = 1;
const COMPONENT: &str = "cosh-ng";
const MAX_CACHE_BYTES: u64 = 64 * 1024;
const ANOLISA_TIMEOUT: Duration = Duration::from_secs(45);
#[cfg(target_os = "linux")]
const RPM_TIMEOUT: Duration = Duration::from_millis(500);
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

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
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

struct RpmInstallation {
    package: String,
    evr: String,
}

#[derive(Deserialize)]
struct AnolisaStatusEnvelope {
    ok: bool,
    data: AnolisaStatusData,
}

#[derive(Deserialize)]
struct AnolisaStatusData {
    components: Vec<AnolisaStatusComponent>,
}

#[derive(Deserialize)]
struct AnolisaStatusComponent {
    active: bool,
    // Kept as a label: synthetic rows such as `not_installed` report `none`.
    #[serde(default)]
    scope: String,
    #[serde(default)]
    health: Vec<AnolisaHealthEntry>,
}

/// State root that owns an anolisa record. Without `--install-mode`, anolisa picks the
/// root from euid, so a non-root probe of a system record must name the scope explicitly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AnolisaScope {
    User,
    System,
}

impl AnolisaScope {
    fn from_label(label: &str) -> Option<Self> {
        match label {
            "user" => Some(Self::User),
            "system" => Some(Self::System),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::System => "system",
        }
    }

    fn update_command(self) -> String {
        match self {
            Self::User => format!("anolisa update {COMPONENT}"),
            Self::System => format!("sudo anolisa --install-mode system update {COMPONENT}"),
        }
    }
}

#[derive(Deserialize)]
struct AnolisaHealthEntry {
    name: String,
}

/// Reads a cached notice and suppresses it once the running binary catches up.
pub(crate) fn read_cached_notice(running_version: &str) -> Option<UpgradeNotice> {
    let path = cache_path()?;
    if fs::metadata(&path).ok()?.len() > MAX_CACHE_BYTES {
        return None;
    }
    let bytes = fs::read(path).ok()?;
    let cache: UpgradeCache = serde_json::from_slice(&bytes).ok()?;
    let executable = current_executable()?;
    if cache.version != CACHE_VERSION || cache.executable != executable {
        return None;
    }
    // The login thread waits for this bounded check, so no shutdown owner is needed.
    let rpm_installation = (cache.method == DetectionMethod::Rpm)
        .then(|| rpm_installation_for_executable(&ProbeProcesses::default(), &executable))
        .flatten();
    cached_notice(&cache, running_version, rpm_installation.as_ref())
}

fn cached_notice(
    cache: &UpgradeCache,
    running_version: &str,
    rpm_installation: Option<&RpmInstallation>,
) -> Option<UpgradeNotice> {
    let mut notice = cache.notice.clone()?;
    if notice.package.trim().is_empty()
        || notice.latest.trim().is_empty()
        || notice.command.trim().is_empty()
    {
        return None;
    }
    match cache.method {
        DetectionMethod::Anolisa => match compare_versions(&notice.latest, running_version)? {
            Ordering::Greater => {
                notice.current = running_version.to_string();
                Some(notice)
            }
            Ordering::Equal | Ordering::Less => None,
        },
        DetectionMethod::Rpm => {
            let installation = rpm_installation?;
            (notice.package == installation.package && notice.current == installation.evr)
                .then_some(notice)
        }
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

fn detect_upgrade(
    processes: &ProbeProcesses,
    running_version: &str,
    executable: &Path,
) -> Option<DetectionResult> {
    tracing::debug!(executable = %executable.display(), %running_version, "detecting upgrade source");
    if let Some(scope) = anolisa_owner_scope(processes, executable) {
        let verdict = detect_with_anolisa(processes, COMPONENT, running_version, scope)?;
        return Some(DetectionResult {
            executable: executable.to_path_buf(),
            method: DetectionMethod::Anolisa,
            verdict,
        });
    }

    if let Some(installation) = rpm_installation_for_executable(processes, executable) {
        let verdict = detect_with_rpm(processes, &installation.package, &installation.evr, None)?;
        return Some(DetectionResult {
            executable: executable.to_path_buf(),
            method: DetectionMethod::Rpm,
            verdict,
        });
    }

    tracing::debug!(executable = %executable.display(), "executable is not managed by anolisa or RPM");
    None
}

fn detect_with_anolisa(
    processes: &ProbeProcesses,
    component: &str,
    running_version: &str,
    scope: AnolisaScope,
) -> Option<UpgradeVerdict> {
    let mut command = Command::new("anolisa");
    command.args([
        "--install-mode",
        scope.as_str(),
        "update",
        component,
        "--dry-run",
        "--json",
    ]);
    let output = match run_bounded_command(processes, command, ANOLISA_TIMEOUT) {
        Some(output) => output,
        None => {
            tracing::warn!(%component, "anolisa upgrade probe did not complete");
            return None;
        }
    };
    if !output.status.success() {
        tracing::warn!(
            %component,
            scope = scope.as_str(),
            status = ?output.status.code(),
            "anolisa upgrade probe failed"
        );
        return None;
    }
    parse_anolisa_output(&output.stdout, component, running_version, scope)
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
    scope: AnolisaScope,
) -> Option<UpgradeVerdict> {
    let envelope: AnolisaEnvelope = match serde_json::from_slice(bytes) {
        Ok(envelope) => envelope,
        Err(error) => {
            tracing::warn!(%component, %error, "cannot parse anolisa upgrade response");
            return None;
        }
    };
    if !envelope.ok {
        tracing::warn!(%component, "anolisa upgrade response reported failure");
        return None;
    }
    let data = envelope.data;
    if data.plan.is_empty() {
        tracing::debug!(%component, "anolisa reports no upgrade plan");
        return Some(UpgradeVerdict::UpToDate);
    }
    let Some(latest) = data.to_version.filter(|version| !version.trim().is_empty()) else {
        tracing::warn!(%component, plan_steps = data.plan.len(), "anolisa upgrade plan has no target version");
        return Some(UpgradeVerdict::Unavailable);
    };
    match compare_versions(&latest, running_version) {
        Some(Ordering::Greater) => {
            tracing::debug!(%component, %running_version, %latest, "anolisa reports an upgrade");
        }
        Some(Ordering::Equal | Ordering::Less) => {
            tracing::debug!(%component, %running_version, %latest, "anolisa target is not newer than the running binary");
            return Some(UpgradeVerdict::UpToDate);
        }
        None => {
            tracing::warn!(%component, %running_version, %latest, "cannot compare anolisa target with running version");
            return Some(UpgradeVerdict::Unavailable);
        }
    }
    Some(UpgradeVerdict::Upgrade(UpgradeNotice {
        package: data.package.unwrap_or_else(|| component.to_string()),
        current: running_version.to_string(),
        latest,
        command: scope.update_command(),
    }))
}

fn anolisa_owner_scope(processes: &ProbeProcesses, executable: &Path) -> Option<AnolisaScope> {
    let mut command = Command::new("anolisa");
    command.args(["status", COMPONENT, "--json"]);
    let output = match run_bounded_command(processes, command, ANOLISA_TIMEOUT) {
        Some(output) => output,
        None => {
            tracing::debug!("anolisa status probe did not complete");
            return None;
        }
    };
    if !output.status.success() {
        tracing::warn!(
            status = ?output.status.code(),
            "anolisa status probe failed"
        );
        return None;
    }
    let scope = anolisa_status_owner_scope(&output.stdout, executable);
    tracing::debug!(
        executable = %executable.display(),
        scope = scope.map(AnolisaScope::as_str),
        "checked anolisa executable ownership"
    );
    scope
}

fn anolisa_status_owner_scope(bytes: &[u8], executable: &Path) -> Option<AnolisaScope> {
    let envelope = serde_json::from_slice::<AnolisaStatusEnvelope>(bytes).ok()?;
    if !envelope.ok {
        return None;
    }
    envelope
        .data
        .components
        .into_iter()
        .find(|component| {
            component.active
                && component.health.iter().any(|entry| {
                    entry
                        .name
                        .strip_prefix("integrity:")
                        .and_then(canonical_path)
                        == canonical_path(executable)
                })
        })
        .and_then(|component| AnolisaScope::from_label(&component.scope))
}

#[cfg(target_os = "linux")]
fn rpm_installation_for_executable(
    processes: &ProbeProcesses,
    executable: &Path,
) -> Option<RpmInstallation> {
    let rpm = find_in_path(OsStr::new("rpm"))?;
    let mut ownership = Command::new(rpm);
    ownership.args([
        OsStr::new("-qf"),
        OsStr::new("--qf"),
        OsStr::new("%{NAME}\\n%{EPOCH}:%{VERSION}-%{RELEASE}"),
    ]);
    ownership.arg(executable);
    let output = run_bounded_command(processes, ownership, RPM_TIMEOUT)?;
    output.status.success().then_some(())?;
    parse_rpm_installation(&output.stdout)
}

fn parse_rpm_installation(stdout: &[u8]) -> Option<RpmInstallation> {
    let mut lines = std::str::from_utf8(stdout).ok()?.lines();
    let package = lines.next()?.trim();
    let evr = lines.next()?.trim();
    // `%{EPOCH}` renders as `(none)` for epoch-less packages. Drop absent and zero
    // epochs so the current side matches the EVR form `dnf check-update` prints.
    let evr = evr
        .strip_prefix("(none):")
        .or_else(|| evr.strip_prefix("0:"))
        .unwrap_or(evr);
    (!package.is_empty() && !evr.is_empty()).then(|| RpmInstallation {
        package: package.to_string(),
        evr: evr.to_string(),
    })
}

#[cfg(not(target_os = "linux"))]
fn rpm_installation_for_executable(
    _processes: &ProbeProcesses,
    _executable: &Path,
) -> Option<RpmInstallation> {
    None
}

#[cfg(target_os = "linux")]
fn detect_with_rpm(
    processes: &ProbeProcesses,
    package: &str,
    installed_evr: &str,
    upgrade_command: Option<&str>,
) -> Option<UpgradeVerdict> {
    let (manager, program) = if let Some(path) = find_in_path(OsStr::new("dnf")) {
        ("dnf", path)
    } else {
        ("yum", find_in_path(OsStr::new("yum"))?)
    };
    check_package_manager(
        processes,
        &program,
        manager,
        package,
        installed_evr,
        upgrade_command,
    )
}

#[cfg(not(target_os = "linux"))]
fn detect_with_rpm(
    _processes: &ProbeProcesses,
    _package: &str,
    _installed_evr: &str,
    _upgrade_command: Option<&str>,
) -> Option<UpgradeVerdict> {
    None
}

#[cfg(target_os = "linux")]
fn check_package_manager(
    processes: &ProbeProcesses,
    program: &Path,
    manager: &str,
    package: &str,
    installed_evr: &str,
    upgrade_command: Option<&str>,
) -> Option<UpgradeVerdict> {
    let mut command = Command::new(program);
    command.args(["check-update", package]);
    let output = run_bounded_command(processes, command, PACKAGE_MANAGER_TIMEOUT)?;
    classify_package_manager_output(
        output.status.code(),
        &output.stdout,
        package,
        installed_evr,
        manager,
        upgrade_command,
    )
}

fn classify_package_manager_output(
    status: Option<i32>,
    stdout: &[u8],
    package: &str,
    installed_evr: &str,
    manager: &str,
    upgrade_command: Option<&str>,
) -> Option<UpgradeVerdict> {
    match status {
        Some(0) => Some(UpgradeVerdict::UpToDate),
        Some(100) => {
            let latest = parse_package_candidate(stdout, package)?;
            Some(UpgradeVerdict::Upgrade(UpgradeNotice {
                package: package.to_string(),
                current: installed_evr.to_string(),
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
    env::current_exe()
        .ok()
        .and_then(|path| canonical_path(&path))
}

fn canonical_path(path: impl AsRef<Path>) -> Option<PathBuf> {
    let path = path.as_ref();
    Some(fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()))
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
        method: result.method.clone(),
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
