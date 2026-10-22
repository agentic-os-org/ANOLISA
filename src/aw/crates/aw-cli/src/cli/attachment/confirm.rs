//! A native ConfigChange confirms Qoder adopted registration before the first prompt.

use super::super::{adapter::HookBinding, read_file, Result};
use aw_service::{Client, LifecyclePhase, Operation};
use serde_json::{json, Value};
use std::{fs, io::Write, os::unix::fs::OpenOptionsExt, path::PathBuf, time::Instant};

pub(crate) fn confirm(
    binding: &HookBinding,
    path: &str,
    native: &Value,
    deadline: Instant,
) -> Result<()> {
    let description: Value = serde_json::from_slice(&read_file(path, 4096)?)?;
    if binding.adapter != "qoder"
        || native["hook_event_name"] != "ConfigChange"
        || native["file_path"] != description["settings"]
    {
        return Err("native configuration confirmation mismatch".into());
    }
    let ready: PathBuf = description["ready"]
        .as_str()
        .ok_or("missing ready path")?
        .into();
    let client = Client::connect(&binding.socket, deadline)?;
    if client.identity() != &binding.binding.identity {
        return Err("stale attachment binding".into());
    }
    // SAFETY: exec-form native callbacks are children of Qoder, without a shell.
    let pid = unsafe { libc::getppid() } as u32;
    if ready.try_exists()? {
        return Ok(());
    }
    if description["detaching"] != true {
        client.call(
            Operation::BridgeLifecycle {
                instance_id: binding.binding.instance_id.clone(),
                pid,
                phase: LifecyclePhase::Ready,
                key: String::new(),
            },
            deadline,
        )?;
    }
    let temporary = ready.with_extension(format!("{}.tmp", std::process::id()));
    let result = (|| -> Result<()> {
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        output.write_all(&serde_json::to_vec(&json!({"version":1,"adapter":"qoder","pid":pid,"token":description["token"],"hooks":0}))?)?;
        output.sync_all()?;
        fs::rename(&temporary, &ready)?;
        Ok(())
    })();
    if temporary.try_exists()? {
        fs::remove_file(temporary)?;
    }
    result
}
