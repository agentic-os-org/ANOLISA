//! Same-user native lifecycle transport; Adapters own native phase interpretation.

use super::{adapter::HookBinding, Arguments, Exit, Result};
use aw_service::{Client, LifecyclePhase, Operation};
use std::time::{Duration, Instant};

pub(super) fn binding(path: &str) -> Result<HookBinding> {
    let binding = HookBinding::read(path)?;
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
            Ok(Exit::Code(if args.flags.contains_key("--confirm") {
                0
            } else {
                2
            }))
        }
    }
}

fn invoke(args: &Arguments) -> Result<Exit> {
    args.check(&["--adapter", "--binding", "--confirm"], false)?;
    let binding = binding(args.required("--binding")?)?;
    if binding.adapter != args.required("--adapter")? {
        return Err("lifecycle adapter mismatch".into());
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    let bytes = super::hook::read_input(deadline)?;
    let native: serde_json::Value = serde_json::from_slice(&bytes)?;
    if let Some(path) = args.flags.get("--confirm") {
        super::attachment::confirm::confirm(&binding, path, &native, deadline)?;
        return Ok(Exit::Code(0));
    }
    let (phase, key, pid) = if binding.adapter == "qoder" {
        let (phase, key) = aw_adapter::reload::qoder_lifecycle(&binding.native_context(), &native)?;
        // SAFETY: getppid reads the native exec-form callback's parent process.
        (
            serde_json::from_value(serde_json::json!(phase))?,
            key,
            unsafe { libc::getppid() } as u32,
        )
    } else if matches!(binding.adapter.as_str(), "qwenpaw" | "qoder-sdk") {
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
