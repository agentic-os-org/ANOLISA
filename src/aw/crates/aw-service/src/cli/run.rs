//! One foreground Agent instance owns its generated files, not the shared service.

use super::{
    adapter::{self, HookBinding, LaunchContext, LaunchInput},
    read_file, readiness, Arguments, Exit, Result,
};
use aw_exec::CommandSpec;
use aw_service::{launch_service, Binding, Operation};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::{self, DirBuilder, OpenOptions},
    io::Write,
    os::unix::{
        fs::{DirBuilderExt, OpenOptionsExt},
        process::ExitStatusExt,
    },
    path::{Path, PathBuf},
    sync::atomic::AtomicI32,
    time::{Duration, Instant},
};

static SIGNAL: AtomicI32 = AtomicI32::new(0);

extern "C" fn signal(value: libc::c_int) {
    SIGNAL.store(value, std::sync::atomic::Ordering::Release);
}

pub(super) struct Artifacts(pub(super) PathBuf);

impl Artifacts {
    fn create(state: &Path) -> Result<Self> {
        let id = fs::read_to_string("/proc/sys/kernel/random/uuid")?;
        let path = state.join(format!("launch-{}", id.trim()));
        DirBuilder::new().mode(0o700).create(&path)?;
        Ok(Self(path))
    }

    pub(super) fn write(&self, name: &str, bytes: &[u8]) -> Result<PathBuf> {
        let path = self.0.join(name);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(path)
    }

    fn cleanup(&self) -> Result<()> {
        fs::remove_dir_all(&self.0)?;
        Ok(())
    }
}

impl Drop for Artifacts {
    fn drop(&mut self) {
        if self.0.exists() {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}

pub(super) fn launch(args: &Arguments) -> Result<Exit> {
    args.check(
        &[
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
    install_signals()?;
    let command = CommandSpec {
        program: argv[0].clone().into(),
        args: native.iter().map(OsString::from).collect(),
        cwd: cwd.clone(),
        environment: std::env::vars_os().collect::<BTreeMap<_, _>>(),
    };
    let mut prepared = adapter.prepare(LaunchInput {
        document: config.as_value().clone(),
        target: target.into(),
        flags: args.flags.clone(),
        command,
    })?;
    if let Some(exit) = interrupted() {
        return Ok(exit);
    }
    let capabilities = prepared.capabilities();
    if capabilities.adapter != name {
        return Err("prepared Adapter does not match configured target".into());
    }
    let steps = aw_provider::admission::preflight(&config, target, &capabilities.clone().into())?;
    prepared.validate_steps(&steps)?;
    let executable = std::env::current_exe()?;
    let service = launch_service::ensure_service(
        &bytes,
        &executable,
        Instant::now() + Duration::from_secs(30),
    )?;
    if let Some(exit) = interrupted() {
        return Ok(exit);
    }
    let artifacts = Artifacts::create(&service.paths.state_dir)?;
    let binding_path = artifacts.0.join("binding.json");
    let mut instance = None;
    let executed: Result<Exit> = (|| {
        let plan = prepared.configure(&LaunchContext {
            files: &artifacts,
            binding_path: &binding_path,
            executable: &executable,
            steps: &steps,
        })?;
        let binding: Binding = serde_json::from_value(service.client.call(
            Operation::Bind {
                target: target.into(),
                capabilities,
                cwd: cwd.to_str().ok_or("non-UTF-8 working directory")?.into(),
                environment: plan.provider_environment,
            },
            Instant::now() + Duration::from_secs(30),
        )?)?;
        instance = Some(binding.instance_id.clone());
        let hook_binding = HookBinding {
            adapter: name.into(),
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
        let status = readiness::run(&plan.command, &SIGNAL, plan.readiness)?;
        Ok(match status.code() {
            Some(code) => Exit::Code(code),
            None => Exit::Signal(status.signal().ok_or("Agent exit has no status")?),
        })
    })();
    let released = instance
        .map(|instance_id| {
            service.client.call(
                Operation::ReleaseInstance { instance_id },
                Instant::now() + Duration::from_secs(5),
            )
        })
        .transpose();
    let native_cleaned = prepared.finish();
    let cleaned = artifacts.cleanup();
    released?;
    native_cleaned?;
    cleaned?;
    executed
}

fn interrupted() -> Option<Exit> {
    let signal = SIGNAL.load(std::sync::atomic::Ordering::Acquire);
    (signal != 0).then_some(Exit::Signal(signal))
}

fn install_signals() -> Result<()> {
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
