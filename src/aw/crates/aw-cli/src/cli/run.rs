//! One foreground Agent instance owns its generated files, not the shared service.

use super::{
    adapter::{self, HookBinding, LaunchContext, LaunchInput},
    read_file, readiness, Arguments, Exit, Result,
};
use aw_adapter::LaunchFiles;
use aw_exec::{CommandSpec, Limits, PackageLeases};
use aw_service::{launch_service, Binding, Operation, RunOutcome};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::{self, DirBuilder},
    os::unix::{
        fs::{DirBuilderExt, MetadataExt},
        process::ExitStatusExt,
    },
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicI32},
    thread,
    time::{Duration, Instant},
};

static SIGNAL: AtomicI32 = AtomicI32::new(0);
pub(super) static CANCELLED: AtomicBool = AtomicBool::new(false);
const HERMES_RETIRE_SCRIPT: &str = r#"import json, sys
from pathlib import Path
from hermes_cli.gateway_connection import GatewayConnection

launch_path = Path(sys.argv[1])
session_id = sys.argv[2]
launch = json.loads(launch_path.read_text())
connection = GatewayConnection()
try:
    receipt = connection.call(
        "plugin.aw-native-hooks.retire", timeout=5,
        target_session_id=session_id, launch_path=str(launch_path),
        token=launch["token"])
finally:
    connection.close()
if receipt != {"retired": True}:
    raise SystemExit("AW Hermes Gateway session retirement is pending")
"#;

extern "C" fn signal(value: libc::c_int) {
    SIGNAL.store(value, std::sync::atomic::Ordering::Release);
    CANCELLED.store(true, std::sync::atomic::Ordering::Release);
}

pub(super) struct Artifacts(pub(super) PathBuf, bool);

impl Artifacts {
    fn create(state: &Path) -> Result<Self> {
        let id = fs::read_to_string("/proc/sys/kernel/random/uuid")?;
        let path = state.join(format!("launch-{}", id.trim()));
        DirBuilder::new().mode(0o700).create(&path)?;
        Ok(Self(path, false))
    }

    fn cleanup(&self) -> Result<()> {
        fs::remove_dir_all(&self.0)?;
        Ok(())
    }

    fn retain(&mut self) {
        self.1 = true;
    }
}

impl LaunchFiles for Artifacts {
    fn directory(&self) -> &Path {
        &self.0
    }
}

impl Drop for Artifacts {
    fn drop(&mut self) {
        if !self.1 && self.0.exists() {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}

pub(super) fn launch(args: &Arguments) -> Result<Exit> {
    args.check(
        &[
            "--reloadable",
            "--config",
            "--agent",
            "--native-settings",
            "--native-profile",
            "--native-state-dir",
        ],
        true,
    )?;
    let bytes = read_file(args.required("--config")?, 4 * 1024 * 1024)?;
    let config = aw_config::Validator::new()?.parse(&bytes)?;
    let target = args.required("--agent")?;
    let agent = &config.as_value()["spec"]["agents"][target];
    // JSON Schema integers also accept decimal and exponent spellings.
    let shutdown_grace = Duration::from_millis(
        agent["shutdown_grace_ms"]
            .as_f64()
            .map_or(2000, |value| value as u64),
    );
    let name = agent["adapter"]
        .as_str()
        .ok_or("configured Agent target not found")?;
    let adapter = adapter::select(name)?;
    let argv: Vec<String> = agent["argv"]
        .as_array()
        .ok_or("Agent argv missing")?
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or("invalid Agent argument")
        })
        .collect::<std::result::Result<_, _>>()?;
    let mut native = argv[1..].to_vec();
    native.extend(args.native.clone());
    let cwd = std::env::current_dir()?.canonicalize()?;
    let working_directory = cwd
        .to_str()
        .ok_or("non-UTF-8 working directory")?
        .to_owned();
    install_signals()?;
    let executable = std::env::current_exe()?;
    let _core_package_leases = PackageLeases::for_paths([executable.clone()])?;
    if !std::fs::metadata(&executable)?.is_file() {
        return Err("current AW executable is no longer available".into());
    }
    let mut command = CommandSpec {
        program: argv[0].clone().into(),
        args: native.iter().map(OsString::from).collect(),
        cwd: cwd.clone(),
        environment: std::env::vars_os().collect::<BTreeMap<_, _>>(),
    };
    let _agent_package_leases = PackageLeases::acquire([&mut command])?;
    let prepared = adapter.prepare(
        LaunchInput {
            document: config.as_value().clone(),
            target: target.into(),
            options: aw_adapter::NativeOptions {
                reloadable: args.flags.contains_key("--reloadable"),
                resident_policy_reload: false,
                settings: args.flags.get("--native-settings").cloned(),
                profile: args.flags.get("--native-profile").cloned(),
                state_dir: args.flags.get("--native-state-dir").cloned(),
            },
            command,
        },
        &CANCELLED,
    );
    let mut prepared = match prepared {
        Ok(prepared) => prepared,
        Err(error) => return interrupted_error(error.into()),
    };
    if let Some(exit) = interrupted() {
        return Ok(exit);
    }
    let capabilities: aw_service::Capabilities = prepared.capabilities().into();
    if capabilities.adapter != name {
        return Err("prepared Adapter does not match configured target".into());
    }
    if args.flags.contains_key("--reloadable") {
        aw_adapter::reload::supported(&prepared.capabilities())?;
    }
    let resident_policy_reload = !args.flags.contains_key("--reloadable")
        && matches!(name, "qoder-sdk" | "qwenpaw" | "agentscope");
    if resident_policy_reload {
        aw_adapter::reload::resident_supported(&prepared.capabilities())?;
    }
    let steps = aw_core::admission::preflight(&config, target, &capabilities.clone().into())?;
    prepared.validate_steps(&steps)?;
    let configuration = Path::new(args.required("--config")?);
    let remember_connection = args.flags.contains_key("--reloadable")
        || resident_policy_reload
        || super::connection::recorded(configuration)?;
    if let Some((client, _)) =
        super::connection::selected(configuration, Instant::now() + Duration::from_secs(5), true)?
    {
        use sha2::{Digest, Sha256};
        if client.identity().config_revision != format!("{:x}", Sha256::digest(&bytes)) {
            return Err("configuration path already selects another service; use aw update for its active instances or stop it before launching the edited configuration".into());
        }
    }
    let service = launch_service::ensure_service(
        &bytes,
        &executable,
        Instant::now() + Duration::from_secs(30),
    )?;
    if let Some(exit) = interrupted() {
        return Ok(exit);
    }
    if remember_connection {
        super::connection::remember(configuration, &service.paths.socket, &service.client)?;
    }
    let mut artifacts = Artifacts::create(&service.paths.state_dir)?;
    let binding_path = artifacts.0.join("binding.json");
    let mut instance = None;
    let mut started = false;
    let mut hermes_command = None;
    let executed: Result<Exit> = (|| {
        let mut plan = prepared.configure(&LaunchContext {
            files: &artifacts,
            binding_path: &binding_path,
            executable: &executable,
            steps: &steps,
        })?;
        if name == "hermes" {
            hermes_command = Some(plan.command.clone());
        }
        let _launch_package_leases = PackageLeases::acquire([&mut plan.command])?;
        let bound = service.client.call_cancellable(
            Operation::Bind {
                target: target.into(),
                capabilities,
                cwd: working_directory,
                environment: plan.provider_environment,
            },
            Instant::now() + Duration::from_secs(30),
            &CANCELLED,
        );
        let binding: Binding = match bound {
            Ok(value) => serde_json::from_value(value)?,
            Err(error) => return interrupted().map(Ok).unwrap_or_else(|| Err(error.into())),
        };
        instance = Some(binding.instance_id.clone());
        let reloadable = args.flags.contains_key("--reloadable");
        if matches!(name, "pi" | "claude-sdk" | "agentscope")
            || (matches!(name, "qoder-sdk" | "qwenpaw" | "agentscope") && !reloadable)
        {
            service.client.call(
                Operation::OwnInstance {
                    instance_id: binding.instance_id.clone(),
                    pid: std::process::id(),
                },
                Instant::now() + Duration::from_secs(2),
            )?;
        }
        if reloadable || resident_policy_reload {
            service.client.call(
                Operation::EnableReload {
                    instance_id: binding.instance_id.clone(),
                },
                Instant::now() + Duration::from_secs(2),
            )?;
        }
        if agent.get("evidence").is_some() {
            let observation = match service.client.call_cancellable(
                Operation::StartObservation {
                    instance_id: binding.instance_id.clone(),
                },
                Instant::now() + Duration::from_secs(3),
                &CANCELLED,
            ) {
                Ok(value) => value,
                Err(error) => return interrupted_error(error.into()),
            };
            eprintln!(
                "AW observation {}; query {}; coverage unknown; audit {}",
                observation["source_state"], observation["query_state"], observation["audit_key"]
            );
        }
        let hook_binding = HookBinding {
            registration: None,
            adapter: name.into(),
            reloadable,
            resident_policy_reload,
            binding,
            socket: service.paths.socket.clone(),
            cwd,
            events: plan.events,
        };
        artifacts.write("binding.json", &serde_json::to_vec(&hook_binding)?)?;
        if let Some(exit) = interrupted() {
            return Ok(exit);
        }
        eprintln!(
            "AW instance {}; service {} (pid {})",
            hook_binding.binding.instance_id,
            service.paths.socket.display(),
            service.pid
        );
        let mut reported: Result<()> = Ok(());
        let failed = AtomicBool::new(false);
        let status = readiness::run(
            &plan.command,
            &SIGNAL,
            &failed,
            shutdown_grace,
            name == "qoder-sdk",
            plan.readiness,
            |pid| {
                if reloadable || resident_policy_reload {
                    reported = service
                        .client
                        .call(
                            Operation::AssociateProcess {
                                instance_id: hook_binding.binding.instance_id.clone(),
                                pid,
                            },
                            Instant::now() + Duration::from_secs(2),
                        )
                        .map(|_| ())
                        .map_err(Into::into);
                    if reported.is_err() {
                        failed.store(true, std::sync::atomic::Ordering::Release);
                    }
                }
                if reported.is_ok() {
                    reported = service
                        .client
                        .call(
                            Operation::ReportRun {
                                instance_id: hook_binding.binding.instance_id.clone(),
                                outcome: RunOutcome::Started { pid },
                            },
                            Instant::now() + Duration::from_secs(2),
                        )
                        .map(|_| ())
                        .map_err(Into::into);
                    if reported.is_err() {
                        failed.store(true, std::sync::atomic::Ordering::Release);
                    }
                }
                started = true;
            },
        );
        reported?;
        let status = status?;
        Ok(match status.code() {
            Some(code) => Exit::Code(code),
            None => Exit::Signal(status.signal().ok_or("Agent exit has no status")?),
        })
    })();
    let preserve = if name == "hermes" && started {
        let retirement_budget = if interrupted().is_some() { 5 } else { 30 };
        match hermes_command.as_ref().map(|command| {
            hermes_retired(
                &artifacts,
                command,
                Instant::now() + Duration::from_secs(retirement_budget),
            )
        }) {
            Some(Ok(true)) => false,
            Some(Err(error)) => {
                eprintln!("AW Hermes Gateway retirement could not be verified: {error}");
                true
            }
            _ => true,
        }
    } else {
        false
    };
    if preserve {
        artifacts.retain();
        eprintln!(
            "AW Hermes Gateway retirement pending; retained instance {:?} and launch {}. \
             Retire the session before releasing these resources.",
            instance,
            artifacts.0.display()
        );
    }
    let reported = instance
        .as_ref()
        .filter(|_| !preserve)
        .map(|instance_id| {
            let outcome = match (&executed, started) {
                (Ok(Exit::Code(code)), true) => RunOutcome::Exited {
                    code: Some(*code),
                    signal: None,
                },
                (Ok(Exit::Signal(signal)), true) => RunOutcome::Exited {
                    code: None,
                    signal: Some(*signal),
                },
                _ => RunOutcome::Failed,
            };
            service.client.call(
                Operation::ReportRun {
                    instance_id: instance_id.clone(),
                    outcome,
                },
                Instant::now() + Duration::from_secs(2),
            )
        })
        .transpose();
    let released = instance
        .filter(|_| !preserve)
        .map(|instance_id| {
            service.client.call(
                Operation::ReleaseInstance { instance_id },
                Instant::now() + Duration::from_secs(5),
            )
        })
        .transpose();
    let native_cleaned = if preserve { Ok(()) } else { prepared.finish() };
    let cleaned = if preserve {
        Ok(())
    } else {
        artifacts.cleanup()
    };
    if let Some(released) = released? {
        if let Some(observation) = released.get("observation").filter(|value| !value.is_null()) {
            eprintln!(
                "AW observation finished {}; reason {}; gaps {}; audit {}",
                observation["source_state"],
                observation["source_reason"],
                observation["gaps"],
                observation["audit_key"]
            );
        }
    }
    native_cleaned?;
    cleaned?;
    reported?;
    executed
}

fn hermes_retired(artifacts: &Artifacts, command: &CommandSpec, deadline: Instant) -> Result<bool> {
    let path = artifacts.0.join("hermes-sessions.json");
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file()
        || metadata.mode() & 0o7777 != 0o600
        || metadata.uid() != unsafe { libc::geteuid() }
    {
        return Err("AW Hermes session list is not a private regular file".into());
    }
    let sessions: Vec<String> = serde_json::from_slice(&read_file(&path, 256 * 1024)?)?;
    if sessions.len() > 512
        || sessions
            .iter()
            .any(|session| session.is_empty() || session.len() > 512)
    {
        return Err("AW Hermes session list has invalid identities".into());
    }
    let launch: serde_json::Value =
        serde_json::from_slice(&read_file(artifacts.0.join("hermes-launch.json"), 4096)?)?;
    let profile = launch["profile"]
        .as_str()
        .ok_or("AW Hermes launch has no profile")?;
    let mut retire = command.clone();
    retire
        .environment
        .insert(OsString::from("HERMES_HOME"), OsString::from(profile));
    let uncancelled = AtomicBool::new(false);
    for session in sessions {
        retire.args = vec![
            OsString::from("-I"),
            OsString::from("-B"),
            OsString::from("-c"),
            OsString::from(HERMES_RETIRE_SCRIPT),
            artifacts.0.join("hermes-launch.json").into_os_string(),
            OsString::from(session),
        ];
        loop {
            let attempt_deadline = (Instant::now() + Duration::from_secs(8)).min(deadline);
            if attempt_deadline <= Instant::now() {
                return Ok(false);
            }
            let result = aw_exec::run(
                &retire,
                &[],
                Limits {
                    input_bytes: 0,
                    stdout_bytes: 1024,
                    stderr_bytes: 4096,
                },
                attempt_deadline,
                &uncancelled,
            );
            if matches!(result, Ok(ref output) if output.status.success()) {
                break;
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            thread::sleep(Duration::from_millis(200));
        }
    }
    Ok(true)
}

pub(super) fn interrupted() -> Option<Exit> {
    let signal = SIGNAL.load(std::sync::atomic::Ordering::Acquire);
    (signal != 0).then_some(Exit::Signal(signal))
}

pub(super) fn interrupted_error(error: Box<dyn std::error::Error>) -> Result<Exit> {
    if let Some(exit) = interrupted() {
        eprintln!("aw: {error}");
        return Ok(exit);
    }
    Err(error)
}

pub(super) fn install_signals() -> Result<()> {
    // SAFETY: handlers only update a lock-free atomic; aw-exec owns child reaping.
    unsafe {
        let mut action = std::mem::zeroed::<libc::sigaction>();
        action.sa_sigaction = signal as *const () as usize;
        libc::sigemptyset(&mut action.sa_mask);
        for value in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT] {
            if libc::sigaction(value, &action, std::ptr::null_mut()) != 0 {
                return Err(std::io::Error::last_os_error().into());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{hermes_retired, Artifacts};
    use aw_exec::CommandSpec;
    use std::{
        collections::BTreeMap,
        fs,
        os::unix::fs::PermissionsExt,
        time::{Duration, Instant},
    };

    #[test]
    fn hermes_retirement_requires_private_session_list() {
        let root = tempfile::tempdir().unwrap();
        let launch = root.path().join("launch");
        fs::create_dir(&launch).unwrap();
        let artifacts = Artifacts(launch.clone(), true);
        let command = CommandSpec {
            program: "/bin/true".into(),
            args: Vec::new(),
            cwd: launch.clone(),
            environment: BTreeMap::new(),
        };
        let deadline = || Instant::now() + Duration::from_millis(100);
        assert!(hermes_retired(&artifacts, &command, deadline()).unwrap());
        let sessions = launch.join("hermes-sessions.json");
        fs::write(&sessions, r#"["session"]"#).unwrap();
        assert!(hermes_retired(&artifacts, &command, deadline()).is_err());
        fs::set_permissions(&sessions, fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(&sessions, r#"[""]"#).unwrap();
        assert!(hermes_retired(&artifacts, &command, deadline()).is_err());
    }

    #[test]
    fn hermes_retirement_ignores_child_receipt_without_gateway_confirmation() {
        let root = tempfile::tempdir().unwrap();
        let launch = root.path().join("launch");
        fs::create_dir(&launch).unwrap();
        let artifacts = Artifacts(launch.clone(), true);
        fs::write(launch.join("hermes-launch.json"), r#"{"profile":"/tmp"}"#).unwrap();
        let sessions = launch.join("hermes-sessions.json");
        fs::write(&sessions, r#"["session"]"#).unwrap();
        fs::set_permissions(&sessions, fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(
            launch.join("hermes-retired.json"),
            r#"{"token":"owned","pid":42}"#,
        )
        .unwrap();
        fs::write(launch.join("hermes-retire.py"), "raise SystemExit(0)").unwrap();
        let command = CommandSpec {
            program: "/bin/false".into(),
            args: Vec::new(),
            cwd: launch,
            environment: BTreeMap::new(),
        };
        assert!(!hermes_retired(
            &artifacts,
            &command,
            Instant::now() + Duration::from_millis(100),
        )
        .unwrap());
    }
}
