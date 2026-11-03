//! CLI-owned binding and persistent-install orchestration.

use super::{read_file, Arguments, Exit, Result};
pub(super) use aw_adapter::*;
use aw_exec::{CommandSpec, PackageLeases};
use aw_service::Binding;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, ffi::OsString, path::PathBuf};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HookBinding {
    pub adapter: String,
    #[serde(default)]
    pub reloadable: bool,
    #[serde(default)]
    pub resident_policy_reload: bool,
    pub binding: Binding,
    pub socket: PathBuf,
    pub cwd: PathBuf,
    pub events: BTreeMap<String, HookEvent>,
}

impl HookBinding {
    pub(super) fn read(path: &str) -> Result<Self> {
        use std::io::Read;
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?;
        let metadata = file.metadata()?;
        // SAFETY: geteuid reads the process identity without changing it.
        if !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o7777 != 0o600
            || metadata.len() > 1024 * 1024
        {
            return Err("unsafe or oversized hook binding".into());
        }
        let mut bytes = Vec::new();
        file.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > 1024 * 1024 {
            return Err("oversized hook binding".into());
        }
        Ok(serde_json::from_slice(&bytes)?)
    }

    pub(super) fn native_context(&self) -> NativeContext {
        NativeContext {
            target: self.binding.target.clone(),
            instance_id: self.binding.instance_id.clone(),
            cwd: self.cwd.clone(),
        }
    }
}

pub(super) fn install(args: &Arguments) -> Result<Exit> {
    if args.flags.contains_key("--pid") || args.flags.contains_key("--remove") {
        return super::attachment::install(args);
    }
    args.check(&["--config", "--agent", "--native-profile"], false)?;
    let bytes = read_file(args.required("--config")?, 4 * 1024 * 1024)?;
    let config = aw_config::Validator::new()?.parse(&bytes)?;
    let target = args.required("--agent")?;
    let name = config.as_value()["spec"]["agents"][target]["adapter"]
        .as_str()
        .ok_or("configured Agent target not found")?;
    let adapter = select(name)?;
    super::run::install_signals()?;
    let executable = std::env::current_exe()?;
    let _core_package_leases = PackageLeases::for_paths([executable.clone()])?;
    if !std::fs::metadata(&executable)?.is_file() {
        return Err("current AW executable is no longer available".into());
    }
    let argv = config.as_value()["spec"]["agents"][target]["argv"]
        .as_array()
        .ok_or("Agent argv missing")?;
    let program = argv
        .first()
        .and_then(serde_json::Value::as_str)
        .ok_or("Agent executable missing")?;
    let mut command = CommandSpec {
        program: program.into(),
        args: argv
            .iter()
            .skip(1)
            .map(|argument| {
                argument
                    .as_str()
                    .map(OsString::from)
                    .ok_or("invalid Agent argument")
            })
            .collect::<std::result::Result<_, _>>()?,
        cwd: std::env::current_dir()?.canonicalize()?,
        environment: std::env::vars_os().collect(),
    };
    // Persistent registration can invoke the configured Agent repeatedly; keep its files
    // and the current AW executable protected until the Adapter has finished or cancelled.
    let _agent_package_leases = PackageLeases::acquire([&mut command])?;
    // The Adapter reads argv from its snapshot. Pin the same resolved program so a newly
    // installed earlier PATH candidate cannot redirect execution outside the held leases.
    let mut document = config.as_value().clone();
    let program = command
        .program
        .to_str()
        .ok_or("Agent executable path must be UTF-8 for persistent registration")?;
    document["spec"]["agents"][target]["argv"][0] = program.into();
    let config = aw_config::Validator::new()?.parse(&serde_json::to_vec(&document)?)?;
    let options = InstallOptions {
        target: target.into(),
        profile: args.flags.get("--native-profile").cloned(),
    };
    match adapter.install(&options, &config, &super::run::CANCELLED) {
        Ok(()) => Ok(super::run::interrupted().unwrap_or(Exit::Code(0))),
        Err(error) => super::run::interrupted_error(error.into()),
    }
}
