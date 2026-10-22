//! Same-user native lifecycle transport; Adapters own native phase interpretation.

use super::{adapter::HookBinding, Arguments, Exit, Result};
use aw_service::{Client, LifecyclePhase, Operation};
use std::{
    fs::OpenOptions,
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    time::{Duration, Instant},
};

pub(super) fn binding(path: &str) -> Result<HookBinding> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    // SAFETY: geteuid reads the current user identity without changing it.
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o7777 != 0o600
        || metadata.len() > 1024 * 1024
    {
        return Err("unsafe or oversized hook binding".into());
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 {
        return Err("oversized hook binding".into());
    }
    let binding: HookBinding = serde_json::from_slice(&bytes)?;
    if !binding.reloadable {
        return Err("instance has no stable reload bridge".into());
    }
    Ok(binding)
}

pub(super) fn lifecycle(args: &Arguments) -> Result<Exit> {
    match invoke(args) {
        Ok(exit) => Ok(exit),
        Err(error) => {
            eprintln!("AW lifecycle failed: {error}");
            // Qoder requires exit 2 to stop a prompt before model dispatch.
            Ok(Exit::Code(2))
        }
    }
}

fn invoke(args: &Arguments) -> Result<Exit> {
    args.check(&["--adapter", "--binding"], false)?;
    let binding = binding(args.required("--binding")?)?;
    if binding.adapter != args.required("--adapter")? {
        return Err("lifecycle adapter mismatch".into());
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    let bytes = super::hook::read_input(deadline)?;
    let native: serde_json::Value = serde_json::from_slice(&bytes)?;
    let (phase, key, pid) = if binding.adapter == "qoder" {
        let (phase, key) = aw_adapter::reload::qoder_lifecycle(&binding.native_context(), &native)?;
        // SAFETY: getppid reads the native exec-form callback's parent process.
        (
            serde_json::from_value(serde_json::json!(phase))?,
            key,
            unsafe { libc::getppid() } as u32,
        )
    } else if binding.adapter == "qwenpaw" {
        (
            serde_json::from_value::<LifecyclePhase>(native["phase"].clone())?,
            native["key"]
                .as_str()
                .ok_or("missing lifecycle key")?
                .into(),
            native["pid"]
                .as_u64()
                .and_then(|pid| u32::try_from(pid).ok())
                .ok_or("invalid native pid")?,
        )
    } else {
        return Err("unsupported lifecycle adapter".into());
    };
    let client = Client::connect(&binding.socket, deadline)?;
    if client.identity() != &binding.binding.identity {
        return Err("stale lifecycle binding".into());
    }
    client.call(
        Operation::BridgeLifecycle {
            instance_id: binding.binding.instance_id,
            pid,
            phase,
            key,
        },
        deadline,
    )?;
    Ok(Exit::Code(0))
}
