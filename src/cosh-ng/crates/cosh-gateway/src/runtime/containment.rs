//! Verifies that an external service manager owns the Gateway process tree.

use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::Duration,
};

use thiserror::Error;
use wait_timeout::ChildExt;

const MAX_SYSTEMCTL_OUTPUT_BYTES: usize = 64 * 1024;
const MAX_UNIT_NAME_BYTES: usize = 128;
const MAX_TIMEOUT_STOP_USEC: u64 = 60_000_000;
const SYSTEMCTL_TIMEOUT: Duration = Duration::from_secs(5);

/// Stable failure returned before production Runtime admission is enabled.
#[derive(Debug, Error)]
pub enum RuntimeContainmentError {
    /// The configured systemd unit name is not a bounded service-unit name.
    #[error("runtime containment unit is invalid")]
    InvalidUnit,
    /// The current process is not in a supported unified cgroup.
    #[error("runtime containment cgroup cannot be verified")]
    CgroupUnavailable,
    /// A trusted systemctl executable is unavailable.
    #[error("runtime containment verifier is unavailable")]
    VerifierUnavailable,
    /// The external service manager did not return usable unit properties.
    #[error("runtime containment unit properties cannot be verified")]
    UnitUnavailable,
    /// The running process or unit properties do not satisfy the hard-crash contract.
    #[error("runtime containment is not verified")]
    Unverified,
}

impl RuntimeContainmentError {
    /// Returns the stable public error code for every failed proof attempt.
    #[must_use]
    pub fn code(&self) -> &'static str {
        "runtime_containment_unverified"
    }
}

/// Opaque proof that an external Linux systemd cgroup owns this Gateway.
///
/// The type has no public constructor. Callers can obtain it only from a live
/// [`LinuxSystemdContainmentVerifier`] check.
#[derive(Debug)]
pub struct VerifiedRuntimeContainment {
    unit: String,
    control_group: PathBuf,
}

impl VerifiedRuntimeContainment {
    /// Returns the verified systemd unit for bounded diagnostics.
    #[must_use]
    pub fn unit(&self) -> &str {
        &self.unit
    }

    /// Returns the verified unified cgroup path for bounded diagnostics.
    #[must_use]
    pub fn control_group(&self) -> &Path {
        &self.control_group
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SystemdUnitProperties {
    control_group: PathBuf,
    service_type: String,
    main_pid: u32,
    load_state: String,
    active_state: String,
    kill_mode: String,
    send_sigkill: String,
    final_kill_signal: String,
    timeout_stop_usec: u64,
    delegate: String,
    restart: String,
}

trait RuntimeContainmentInspector {
    fn current_control_group(&self) -> Result<PathBuf, RuntimeContainmentError>;

    fn unit_properties(&self, unit: &str)
        -> Result<SystemdUnitProperties, RuntimeContainmentError>;
}

/// Live Linux systemd verifier used before production Runtime admission.
#[derive(Debug, Default)]
pub struct LinuxSystemdContainmentVerifier {
    inspector: SystemdContainmentInspector,
}

impl LinuxSystemdContainmentVerifier {
    /// Creates a verifier backed by `/proc` and a trusted systemctl executable.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Validates the production unit namespace without querying systemd.
    ///
    /// # Errors
    ///
    /// Rejects units outside the bounded Gateway service templates.
    pub fn validate_unit(unit: &str) -> Result<(), RuntimeContainmentError> {
        validate_unit_name(unit)
    }

    /// Verifies live external ownership for the current Gateway process.
    ///
    /// # Errors
    ///
    /// Fails closed when the unit name, cgroup membership, service-manager
    /// query, or kill semantics cannot be proven.
    pub fn verify(
        &self,
        unit: &str,
    ) -> Result<VerifiedRuntimeContainment, RuntimeContainmentError> {
        verify_with(&self.inspector, unit)
    }
}

fn verify_with(
    inspector: &impl RuntimeContainmentInspector,
    unit: &str,
) -> Result<VerifiedRuntimeContainment, RuntimeContainmentError> {
    validate_unit_name(unit)?;
    let current = inspector.current_control_group()?;
    let properties = inspector.unit_properties(unit)?;
    if properties.load_state != "loaded"
        || properties.active_state != "active"
        || properties.service_type != "exec"
        || properties.main_pid != std::process::id()
        || properties.kill_mode != "control-group"
        || properties.send_sigkill != "yes"
        || properties.final_kill_signal != "9"
        || properties.timeout_stop_usec == 0
        || properties.timeout_stop_usec > MAX_TIMEOUT_STOP_USEC
        || properties.delegate != "no"
        || properties.restart != "on-failure"
        || properties.control_group != current
        || properties.control_group == Path::new("/")
    {
        return Err(RuntimeContainmentError::Unverified);
    }
    Ok(VerifiedRuntimeContainment {
        unit: unit.to_owned(),
        control_group: current,
    })
}

fn validate_unit_name(unit: &str) -> Result<(), RuntimeContainmentError> {
    let instance = ["cosh-gateway@", "cosh-gateway-acp@", "cosh-gateway-dev@"]
        .into_iter()
        .find_map(|prefix| unit.strip_prefix(prefix))
        .and_then(|value| value.strip_suffix(".service"));
    if unit.len() > MAX_UNIT_NAME_BYTES
        || instance.is_none_or(str::is_empty)
        || !instance.is_some_and(|value| {
            value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
        })
    {
        return Err(RuntimeContainmentError::InvalidUnit);
    }
    Ok(())
}

#[derive(Debug, Default)]
struct SystemdContainmentInspector;

impl RuntimeContainmentInspector for SystemdContainmentInspector {
    fn current_control_group(&self) -> Result<PathBuf, RuntimeContainmentError> {
        let content = fs::read_to_string("/proc/self/cgroup")
            .map_err(|_| RuntimeContainmentError::CgroupUnavailable)?;
        let path = content
            .lines()
            .find_map(|line| line.strip_prefix("0::"))
            .ok_or(RuntimeContainmentError::CgroupUnavailable)?;
        validated_control_group(path)
    }

    fn unit_properties(
        &self,
        unit: &str,
    ) -> Result<SystemdUnitProperties, RuntimeContainmentError> {
        let systemctl = trusted_systemctl()?;
        let mut child = Command::new(&systemctl)
            .args([
                "--system",
                "show",
                unit,
                "--no-pager",
                "--property=ControlGroup,Type,MainPID,LoadState,ActiveState,KillMode,SendSIGKILL,FinalKillSignal,TimeoutStopUSec,Delegate,Restart",
            ])
            .env_clear()
            .env("LC_ALL", "C")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| RuntimeContainmentError::UnitUnavailable)?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or(RuntimeContainmentError::UnitUnavailable)?;
        let reader = thread::spawn(move || {
            let mut bytes = Vec::new();
            stdout
                .by_ref()
                .take((MAX_SYSTEMCTL_OUTPUT_BYTES + 1) as u64)
                .read_to_end(&mut bytes)
                .map(|_| bytes)
        });
        let status = match child
            .wait_timeout(SYSTEMCTL_TIMEOUT)
            .map_err(|_| RuntimeContainmentError::UnitUnavailable)?
        {
            Some(status) => status,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return Err(RuntimeContainmentError::UnitUnavailable);
            }
        };
        let stdout = reader
            .join()
            .map_err(|_| RuntimeContainmentError::UnitUnavailable)?
            .map_err(|_| RuntimeContainmentError::UnitUnavailable)?;
        if !status.success() || stdout.len() > MAX_SYSTEMCTL_OUTPUT_BYTES {
            return Err(RuntimeContainmentError::UnitUnavailable);
        }
        parse_unit_properties(&stdout)
    }
}

fn trusted_systemctl() -> Result<PathBuf, RuntimeContainmentError> {
    [Path::new("/usr/bin/systemctl"), Path::new("/bin/systemctl")]
        .into_iter()
        .find_map(|path| {
            let metadata = fs::metadata(path).ok()?;
            (metadata.is_file()
                && metadata.uid() == 0
                && metadata.permissions().mode() & 0o022 == 0
                && metadata.permissions().mode() & 0o111 != 0)
                .then(|| path.to_path_buf())
        })
        .ok_or(RuntimeContainmentError::VerifierUnavailable)
}

fn parse_unit_properties(bytes: &[u8]) -> Result<SystemdUnitProperties, RuntimeContainmentError> {
    let text = std::str::from_utf8(bytes).map_err(|_| RuntimeContainmentError::UnitUnavailable)?;
    let values = text
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect::<BTreeMap<_, _>>();
    let value = |key: &str| {
        values
            .get(key)
            .filter(|value| !value.is_empty())
            .cloned()
            .ok_or(RuntimeContainmentError::UnitUnavailable)
    };
    Ok(SystemdUnitProperties {
        control_group: validated_control_group(&value("ControlGroup")?)?,
        service_type: value("Type")?,
        main_pid: value("MainPID")?
            .parse()
            .map_err(|_| RuntimeContainmentError::UnitUnavailable)?,
        load_state: value("LoadState")?,
        active_state: value("ActiveState")?,
        kill_mode: value("KillMode")?,
        send_sigkill: value("SendSIGKILL")?,
        final_kill_signal: value("FinalKillSignal")?,
        timeout_stop_usec: parse_systemd_usec(&value("TimeoutStopUSec")?)?,
        delegate: value("Delegate")?,
        restart: value("Restart")?,
    })
}

fn parse_systemd_usec(value: &str) -> Result<u64, RuntimeContainmentError> {
    if value.bytes().all(|byte| byte.is_ascii_digit()) {
        return value
            .parse()
            .map_err(|_| RuntimeContainmentError::UnitUnavailable);
    }
    let mut total = 0_u64;
    for token in value.split_ascii_whitespace() {
        let digit_count = token.bytes().take_while(u8::is_ascii_digit).count();
        if digit_count == 0 || digit_count == token.len() {
            return Err(RuntimeContainmentError::UnitUnavailable);
        }
        let amount = token[..digit_count]
            .parse::<u64>()
            .map_err(|_| RuntimeContainmentError::UnitUnavailable)?;
        let multiplier = match &token[digit_count..] {
            "us" => 1,
            "ms" => 1_000,
            "s" => 1_000_000,
            "min" => 60_000_000,
            "h" => 3_600_000_000,
            "d" => 86_400_000_000,
            _ => return Err(RuntimeContainmentError::UnitUnavailable),
        };
        total = total
            .checked_add(
                amount
                    .checked_mul(multiplier)
                    .ok_or(RuntimeContainmentError::UnitUnavailable)?,
            )
            .ok_or(RuntimeContainmentError::UnitUnavailable)?;
    }
    (total != 0)
        .then_some(total)
        .ok_or(RuntimeContainmentError::UnitUnavailable)
}

fn validated_control_group(value: &str) -> Result<PathBuf, RuntimeContainmentError> {
    let path = Path::new(value);
    if !path.is_absolute()
        || value.contains('\0')
        || path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(RuntimeContainmentError::CgroupUnavailable);
    }
    Ok(path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct FakeInspector {
        current: Result<PathBuf, RuntimeContainmentError>,
        properties: Result<SystemdUnitProperties, RuntimeContainmentError>,
    }

    impl RuntimeContainmentInspector for FakeInspector {
        fn current_control_group(&self) -> Result<PathBuf, RuntimeContainmentError> {
            self.current
                .as_ref()
                .map(Clone::clone)
                .map_err(|error| match error {
                    RuntimeContainmentError::CgroupUnavailable => {
                        RuntimeContainmentError::CgroupUnavailable
                    }
                    _ => RuntimeContainmentError::Unverified,
                })
        }

        fn unit_properties(
            &self,
            _unit: &str,
        ) -> Result<SystemdUnitProperties, RuntimeContainmentError> {
            self.properties
                .as_ref()
                .map(Clone::clone)
                .map_err(|_| RuntimeContainmentError::UnitUnavailable)
        }
    }

    fn properties() -> SystemdUnitProperties {
        SystemdUnitProperties {
            control_group: PathBuf::from("/system.slice/cosh-gateway@main.service"),
            service_type: "exec".to_owned(),
            main_pid: std::process::id(),
            load_state: "loaded".to_owned(),
            active_state: "active".to_owned(),
            kill_mode: "control-group".to_owned(),
            send_sigkill: "yes".to_owned(),
            final_kill_signal: "9".to_owned(),
            timeout_stop_usec: 15_000_000,
            delegate: "no".to_owned(),
            restart: "on-failure".to_owned(),
        }
    }

    #[test]
    fn live_control_group_properties_create_an_opaque_proof() {
        let inspector = FakeInspector {
            current: Ok(properties().control_group.clone()),
            properties: Ok(properties()),
        };

        let proof = verify_with(&inspector, "cosh-gateway@main.service").unwrap();

        assert_eq!(proof.unit(), "cosh-gateway@main.service");
        assert_eq!(proof.control_group(), properties().control_group);
    }

    #[test]
    fn mismatched_membership_and_weak_kill_modes_fail_closed() {
        let mismatch = FakeInspector {
            current: Ok(PathBuf::from("/system.slice/another.service")),
            properties: Ok(properties()),
        };
        assert!(matches!(
            verify_with(&mismatch, "cosh-gateway@main.service"),
            Err(RuntimeContainmentError::Unverified)
        ));

        let mut weak = properties();
        weak.kill_mode = "process".to_owned();
        let weak = FakeInspector {
            current: Ok(weak.control_group.clone()),
            properties: Ok(weak),
        };
        assert!(matches!(
            verify_with(&weak, "cosh-gateway@main.service"),
            Err(RuntimeContainmentError::Unverified)
        ));
    }

    #[test]
    fn unit_names_and_root_cgroup_cannot_create_proof() {
        let root = SystemdUnitProperties {
            control_group: PathBuf::from("/"),
            ..properties()
        };
        let inspector = FakeInspector {
            current: Ok(PathBuf::from("/")),
            properties: Ok(root),
        };
        assert!(matches!(
            verify_with(&inspector, "cosh-gateway@main.service"),
            Err(RuntimeContainmentError::Unverified)
        ));
        assert!(matches!(
            verify_with(&inspector, "../cosh-gateway.service"),
            Err(RuntimeContainmentError::InvalidUnit)
        ));
        assert!(
            LinuxSystemdContainmentVerifier::validate_unit("cosh-gateway-acp@root.service").is_ok()
        );
        assert!(LinuxSystemdContainmentVerifier::validate_unit(
            "cosh-gateway-dev@developer.service"
        )
        .is_ok());
        assert!(matches!(
            LinuxSystemdContainmentVerifier::validate_unit(
                "cosh-gateway-dev-extra@developer.service"
            ),
            Err(RuntimeContainmentError::InvalidUnit)
        ));
        assert!(matches!(
            LinuxSystemdContainmentVerifier::validate_unit("cosh-gateway-untrusted@root.service"),
            Err(RuntimeContainmentError::InvalidUnit)
        ));
    }

    #[test]
    fn parser_requires_every_security_property() {
        let output = format!(
            "ControlGroup=/system.slice/cosh-gateway.service\nType=exec\nMainPID={}\n\
LoadState=loaded\nActiveState=active\nKillMode=control-group\nSendSIGKILL=yes\nFinalKillSignal=9\nTimeoutStopUSec=15s\nDelegate=no\nRestart=on-failure\n",
            std::process::id()
        );
        assert_eq!(
            parse_unit_properties(output.as_bytes()).unwrap(),
            SystemdUnitProperties {
                control_group: PathBuf::from("/system.slice/cosh-gateway.service"),
                service_type: "exec".to_owned(),
                main_pid: std::process::id(),
                load_state: "loaded".to_owned(),
                active_state: "active".to_owned(),
                kill_mode: "control-group".to_owned(),
                send_sigkill: "yes".to_owned(),
                final_kill_signal: "9".to_owned(),
                timeout_stop_usec: 15_000_000,
                delegate: "no".to_owned(),
                restart: "on-failure".to_owned(),
            }
        );
        assert!(parse_unit_properties(b"ControlGroup=/x\nLoadState=loaded\n").is_err());
        assert_eq!(parse_systemd_usec("1min 30s").unwrap(), 90_000_000);
        assert!(parse_systemd_usec("infinity").is_err());
    }
}
