//! Hermes callbacks use an immutable workspace Scope under their native Gateway root.

use super::{Client, Exit, HookBinding, Instant, Operation, Result};
use aw_service::Binding;
use std::{io::Write, path::PathBuf, time::Duration};

pub(super) fn scope(mut binding: HookBinding, action: &str) -> Result<Exit> {
    if binding.adapter != "hermes" {
        return Err("Gateway workspace requires Hermes".into());
    }
    let client = Client::from_identity(&binding.socket, binding.binding.identity.clone());
    let instance_id = binding.binding.instance_id.clone();
    let deadline = Instant::now() + Duration::from_secs(8);
    match (action, binding.registration.is_some()) {
        ("open", true) => {
            let input = crate::cli::hook::read_input(deadline)?;
            let cwd: PathBuf = serde_json::from_slice(&input)?;
            if !cwd.is_absolute() || !cwd.is_dir() || cwd.canonicalize()? != cwd {
                return Err("Gateway session workspace must be a canonical directory".into());
            }
            // SAFETY: this transport is started directly by the Gateway plugin.
            let pid = unsafe { libc::getppid() } as u32;
            let child: Binding = serde_json::from_value(client.call(
                Operation::OpenScope {
                    parent_id: instance_id,
                    pid,
                    cwd: cwd.to_str().ok_or("non-UTF-8 working directory")?.into(),
                    // The service retains the root's trusted Provider environment.
                    environment: Default::default(),
                },
                deadline,
            )?)?;
            binding.binding = child;
            binding.cwd = cwd;
            binding.registration = None;
            let mut stdout = std::io::stdout().lock();
            serde_json::to_writer(&mut stdout, &binding)?;
            stdout.flush()?;
        }
        ("activate", false) => {
            client.call(Operation::AcknowledgeScope { instance_id }, deadline)?;
        }
        ("close", false) => {
            match client.call(Operation::ReleaseInstance { instance_id }, deadline) {
                Err(aw_service::Error::Rejected(code)) if code == "unknown_instance" => {}
                result => {
                    result?;
                }
            }
        }
        _ => return Err("invalid Gateway workspace action or binding".into()),
    }
    Ok(Exit::Code(0))
}
