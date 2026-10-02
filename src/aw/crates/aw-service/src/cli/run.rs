//! One foreground Agent instance owns its generated files, not the shared service.

use super::{qoder, read_file, Arguments, Exit, Result};
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
    sync::atomic::{AtomicBool, AtomicI32},
    time::{Duration, Instant},
};

static SIGNAL: AtomicI32 = AtomicI32::new(0);
static CANCELLED: AtomicBool = AtomicBool::new(false);

extern "C" fn signal(value: libc::c_int) {
    SIGNAL.store(value, std::sync::atomic::Ordering::Release);
    CANCELLED.store(true, std::sync::atomic::Ordering::Release);
}

struct Artifacts(PathBuf);

impl Artifacts {
    fn create(state: &Path) -> Result<Self> {
        let id = fs::read_to_string("/proc/sys/kernel/random/uuid")?;
        let path = state.join(format!("launch-{}", id.trim()));
        DirBuilder::new().mode(0o700).create(&path)?;
        Ok(Self(path))
    }

    fn write(&self, name: &str, bytes: &[u8]) -> Result<PathBuf> {
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
    args.check(&["--config", "--agent", "--native-settings"], true)?;
    let bytes = read_file(args.required("--config")?, 4 * 1024 * 1024)?;
    let config = aw_config::Validator::new()?.parse(&bytes)?;
    let target = args.required("--agent")?;
    let agent = &config.as_value()["spec"]["agents"][target];
    if agent["adapter"] != "qoder" {
        return Err("aw run currently supports configured Qoder targets only".into());
    }
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
    qoder::check_args(&native)?;
    let cwd = std::env::current_dir()?.canonicalize()?;
    let settings = qoder::settings(args.flags.get("--native-settings"))?;
    qoder::check_sources(&cwd, &settings)?;
    install_signals()?;
    let environment: BTreeMap<OsString, OsString> = std::env::vars_os().collect();
    let mut command = CommandSpec {
        program: argv[0].clone().into(),
        args: vec!["--version".into()],
        cwd: cwd.clone(),
        environment,
    };
    let version = aw_exec::run(
        &command,
        &[],
        aw_exec::Limits {
            input_bytes: 0,
            stdout_bytes: 4096,
            stderr_bytes: 4096,
        },
        Instant::now() + Duration::from_secs(5),
        &AtomicBool::new(false),
    )?;
    if !version.status.success() || std::str::from_utf8(&version.stdout)?.trim() != qoder::VERSION {
        return Err(format!("Qoder CLI {} is required by this adapter", qoder::VERSION).into());
    }
    if let Some(exit) = interrupted() {
        return Ok(exit);
    }
    let capabilities = qoder::capabilities(&native);
    let steps = aw_provider::admission::preflight(&config, target, &capabilities.clone().into())?;
    let executable = std::env::current_exe()?;
    // Validate generated settings and budgets before creating a service or binding.
    qoder::generate(
        settings.clone(),
        config.as_value(),
        target,
        &steps,
        &executable,
        Path::new("/binding.json"),
    )?;
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
    let (settings, events) = qoder::generate(
        settings,
        config.as_value(),
        target,
        &steps,
        &executable,
        &binding_path,
    )?;
    let settings_path = artifacts.write("settings.json", &serde_json::to_vec(&settings)?)?;
    let mut environment = command.environment.clone();
    // Qoder adds these to command hooks. Pin the same verified project directory
    // without allowing arbitrary callback environment to replace Provider context.
    for name in ["QODER_PROJECT_DIR", "CLAUDE_PROJECT_DIR"] {
        environment.insert(
            name.into(),
            cwd.to_str().ok_or("non-UTF-8 working directory")?.into(),
        );
    }
    let source = if environment
        .get(std::ffi::OsStr::new("QODER_WORK_INTEGRATION_MODE"))
        .is_some_and(|v| v == "1")
    {
        "qoderwork"
    } else {
        "cli"
    };
    let version = environment
        .get(std::ffi::OsStr::new("QODER_CLIENT_VERSION"))
        .filter(|v| !v.is_empty())
        .cloned()
        .unwrap_or_else(|| qoder::VERSION.into());
    environment.insert("QODER_HOOK_SOURCE".into(), source.into());
    environment.insert("QODER_HOOK_VERSION".into(), version);
    environment.insert("QODER_SITE".into(), "GLOBAL".into());
    let bound = service.client.call_cancellable(
        Operation::Bind {
            target: target.into(),
            capabilities,
            cwd: cwd.to_str().ok_or("non-UTF-8 working directory")?.into(),
            environment,
        },
        Instant::now() + Duration::from_secs(30),
        &CANCELLED,
    );
    let binding: Binding = match bound {
        Ok(value) => serde_json::from_value(value)?,
        Err(error) => return interrupted().map(Ok).unwrap_or_else(|| Err(error.into())),
    };
    let instance = binding.instance_id.clone();
    let executed: Result<std::process::ExitStatus> = (|| {
        if let Some(Exit::Signal(signal)) = interrupted() {
            return Ok(std::process::ExitStatus::from_raw(signal));
        }
        let hook_binding = qoder::HookBinding {
            binding,
            socket: service.paths.socket.clone(),
            cwd,
            events,
        };
        artifacts.write("binding.json", &serde_json::to_vec(&hook_binding)?)?;
        // The flag layer carries only owned generated hooks plus explicitly merged settings.
        // Place it before native arguments so a native `--` cannot turn it into a prompt.
        command.args = vec![OsString::from("--settings"), settings_path.into_os_string()];
        command.args.extend(native.iter().map(OsString::from));
        eprintln!(
            "AW instance {instance}; service {} (pid {})",
            service.paths.socket.display(),
            service.pid
        );
        aw_exec::run_foreground(&command, &SIGNAL).map_err(Into::into)
    })();
    let released = service.client.call(
        Operation::ReleaseInstance {
            instance_id: instance,
        },
        Instant::now() + Duration::from_secs(5),
    );
    let cleaned = artifacts.cleanup();
    // Report teardown failures even when the Agent itself already failed.
    released?;
    cleaned?;
    let status: std::process::ExitStatus = executed?;
    Ok(match status.code() {
        Some(code) => Exit::Code(code),
        None => Exit::Signal(status.signal().ok_or("Agent exit has no status")?),
    })
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
