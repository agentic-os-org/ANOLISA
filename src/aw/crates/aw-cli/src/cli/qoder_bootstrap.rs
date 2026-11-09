//! Native exec-form Qoder callbacks bind their actual parent before the first prompt.

use super::{adapter::HookBinding, Arguments, Exit, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
    time::{Duration, Instant},
};

pub(super) fn descriptor(path: &Path) -> Result<Value> {
    let parent = path.parent().ok_or("startup descriptor has no directory")?;
    let metadata = fs::symlink_metadata(parent)?;
    if !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
        || parent.canonicalize()? != parent
    {
        return Err("unsafe Qoder startup directory".into());
    }
    private_json(path)
}

fn private_json(path: &Path) -> Result<Value> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o7777 != 0o600
        || metadata.nlink() != 1
        || metadata.len() > 1024 * 1024
    {
        return Err("unsafe Qoder startup record".into());
    }
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 {
        return Err("oversized Qoder startup record".into());
    }
    Ok(serde_json::from_slice(&bytes)?)
}

pub(super) fn callback(args: &Arguments) -> Result<Exit> {
    match invoke(args) {
        Ok(exit) => Ok(exit),
        Err(error) => {
            eprintln!("AW Qoder startup callback failed: {error}");
            // Native input and before-tool Hooks use exit 2 as a veto.
            Ok(Exit::Code(2))
        }
    }
}

fn invoke(args: &Arguments) -> Result<Exit> {
    args.check(&["--descriptor"], false)?;
    let path = Path::new(args.required("--descriptor")?);
    let descriptor = descriptor(path)?;
    if descriptor["binary"] != json!(std::env::current_exe()?.canonicalize()?) {
        return Err("startup executable changed; reinstall its native Hooks".into());
    }
    let bytes = super::hook::read_input(Instant::now() + Duration::from_secs(2))?;
    let input: Value = serde_json::from_slice(&bytes)?;
    let event = input["hook_event_name"]
        .as_str()
        .ok_or("native Hook event missing")?;
    let session = input["session_id"]
        .as_str()
        .filter(|value| !value.is_empty() && value.len() <= 512)
        .ok_or("native session identity missing")?;
    let configuration = descriptor["config"]
        .as_str()
        .ok_or("startup configuration missing")?;
    let target = descriptor["agent"]
        .as_str()
        .ok_or("startup target missing")?;
    // No caller-supplied PID: every exec-form callback is Qoder's direct child.
    let pid = unsafe { libc::getppid() } as u32;
    let owner = aw_service::ProcessIdentity::read(pid)?;
    let process = owner.metadata();
    let ticks = process["start_ticks"]
        .as_str()
        .ok_or("process start identity missing")?;
    let locator = path
        .parent()
        .ok_or("startup directory missing")?
        .join(format!("owner-{pid}-{ticks}.json"));
    if event == "SessionStart" {
        let config = aw_config::Validator::new()?.parse(&super::read_file(
            configuration,
            aw_config::MAX_DOCUMENT_BYTES,
        )?)?;
        let agent = &config.as_value()["spec"]["agents"][target];
        if agent["adapter"] != "qoder" {
            return Err("startup target is not Qoder".into());
        }
        let native = native_process(
            pid,
            &descriptor,
            &input,
            agent["argv"][0]
                .as_str()
                .ok_or("configured Qoder executable missing")?,
        )?;
        let configured: Vec<_> = agent["argv"]
            .as_array()
            .ok_or("configured argv missing")?
            .iter()
            .skip(1)
            .map(|value| {
                value
                    .as_str()
                    .map(std::ffi::OsString::from)
                    .ok_or("invalid configured argument")
            })
            .collect::<std::result::Result<_, _>>()?;
        if !native.command.args.starts_with(&configured) {
            return Err("native arguments do not retain configured Qoder arguments".into());
        }
        let launch = launch_identity(&native)?;

        let mut claim = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&locator)?;
        let inode = claim.metadata()?;
        let mut binding = None;
        let prepare = Arguments {
            flags: [
                ("--config".into(), configuration.into()),
                ("--agent".into(), target.into()),
            ]
            .into(),
            native: Vec::new(),
        };
        let result = (|| -> Result<Exit> {
            super::native::prepare_with(&prepare, Some(native.command), &mut |prepared| {
                let value = prepared["binding"]
                    .as_str()
                    .ok_or("prepared binding missing")?;
                binding = Some(value.to_owned());
                let root = HookBinding::read(value)?;
                serde_json::to_writer(
                    &mut claim,
                    &json!({"process":process,"binding":value,"identity":root.binding.identity,"session_id":session,"launch":launch}),
                )?;
                claim.flush()?;
                claim.sync_all()?;
                Ok(())
            })?;
            native.identity.check()?;
            let root = HookBinding::read(binding.as_deref().ok_or("prepared binding missing")?)?;
            super::bridge::invoke_bound(root, bytes)
        })();
        if result.is_err() {
            if let Some(binding) = binding {
                let _ = release(&binding);
            }
            remove_exact(&locator, &inode)?;
        }
        return result;
    }
    let locator_inode = fs::symlink_metadata(&locator)?;
    let record = private_json(&locator)?;
    if record["process"] != process || record["session_id"] != session {
        return Err("stale Qoder startup owner".into());
    }
    let binding_path = record["binding"]
        .as_str()
        .ok_or("startup binding missing")?;
    let binding = record_binding(&record)?;
    // Accepted policy and launch identity survive rejected edits to aw.yaml.
    let native = native_process(
        pid,
        &descriptor,
        &input,
        record["launch"]["program"]
            .as_str()
            .ok_or("pinned Qoder executable missing")?,
    )?;
    if record["launch"] != launch_identity(&native)?
        || binding.cwd != native.command.cwd
        || binding.binding.target != target
    {
        return Err("Qoder startup launch identity changed".into());
    }
    owner.check()?;
    native.identity.check()?;
    match event {
        "PreToolUse" | "PostToolUse" => {
            let event = if event == "PreToolUse" {
                "tool.before"
            } else {
                "tool.after"
            };
            super::hook::emit(super::dispatch::invoke_bound(
                aw_adapter::select("qoder")?,
                binding,
                event,
                bytes,
                Instant::now() + Duration::from_secs(59),
            )?)
        }
        "SessionEnd" => {
            let lifecycle = super::bridge::invoke_bound(binding, bytes);
            release(binding_path)?;
            remove_exact(&locator, &locator_inode)?;
            lifecycle
        }
        _ => super::bridge::invoke_bound(binding, bytes),
    }
}

fn launch_identity(native: &super::attachment::process::NativeProcess) -> Result<Value> {
    let args_digest = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&native.command.args)?)
    );
    Ok(
        json!({"program":native.command.program,"args_digest":args_digest,
        "cwd":native.command.cwd,"scope":native.scope}),
    )
}

fn native_process(
    pid: u32,
    descriptor: &Value,
    input: &Value,
    program: &str,
) -> Result<super::attachment::process::NativeProcess> {
    let native = super::attachment::process::NativeProcess::read_identity(pid, "qoder", program)?;
    if native.scope.join("settings.json")
        != Path::new(
            descriptor["settings"]
                .as_str()
                .ok_or("startup settings missing")?,
        )
        || input["cwd"] != json!(native.command.cwd)
    {
        return Err("native startup profile or working directory mismatch".into());
    }
    Ok(native)
}

fn release(binding: &str) -> Result<Exit> {
    super::native::release(&Arguments {
        flags: [("--binding".into(), binding.into())].into(),
        native: Vec::new(),
    })
}

fn remove_exact(path: &Path, expected: &fs::Metadata) -> Result<()> {
    let current = fs::symlink_metadata(path)?;
    if current.dev() != expected.dev() || current.ino() != expected.ino() {
        return Err("Qoder startup locator changed during cleanup".into());
    }
    fs::remove_file(path)?;
    File::open(path.parent().ok_or("locator directory missing")?)?.sync_all()?;
    Ok(())
}

fn record_binding(record: &Value) -> Result<HookBinding> {
    let binding = HookBinding::read(
        record["binding"]
            .as_str()
            .ok_or("startup binding missing")?,
    )?;
    let registration = serde_json::to_value(
        binding
            .registration
            .as_ref()
            .ok_or("startup registration missing")?,
    )?;
    let owner = &registration["handle"]["key"]["owner"];
    if record["identity"] != json!(binding.binding.identity)
        || binding.adapter != "qoder"
        || owner["pid"] != record["process"]["pid"]
        || owner["start_ticks"] != record["process"]["start_ticks"]
    {
        return Err("startup service generation or owner mismatch".into());
    }
    Ok(binding)
}

pub(super) fn recover(directory: &Path) -> Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if !entry.file_name().to_string_lossy().starts_with("owner-") {
            continue;
        }
        let path = entry.path();
        let inode = fs::symlink_metadata(&path)?;
        let filename = entry.file_name();
        let (pid, ticks) = filename
            .to_str()
            .and_then(|name| name.strip_prefix("owner-"))
            .and_then(|name| name.strip_suffix(".json"))
            .and_then(|name| name.split_once('-'))
            .ok_or("invalid Qoder locator filename")?;
        let pid: u32 = pid.parse()?;
        if pid == 0
            || ticks.is_empty()
            || ticks.len() > 20
            || !ticks.bytes().all(|byte| byte.is_ascii_digit())
            || filename != format!("owner-{pid}-{ticks}.json").as_str()
        {
            return Err("invalid Qoder locator process identity".into());
        }
        match aw_service::ProcessIdentity::read(pid) {
            Ok(process) if process.metadata()["start_ticks"] == ticks => {
                return Err("Qoder startup still has a live owner; exit it before removal".into())
            }
            Err(_) if Path::new(&format!("/proc/{pid}")).exists() => {
                return Err("Qoder owner exit is not yet confirmed".into())
            }
            _ => {}
        }
        let record = match private_json(&path) {
            Ok(record) => record,
            Err(error) if error.downcast_ref::<serde_json::Error>().is_some() => {
                // A killed startup CLI can leave an unpublished claim. The exact
                // filename proves the dead owner; malformed bytes never select assets.
                remove_exact(&path, &inode)?;
                continue;
            }
            Err(error) => return Err(error),
        };
        if record["process"]["pid"] != pid || record["process"]["start_ticks"] != ticks {
            return Err("locator filename does not match its process identity".into());
        }
        let binding_path = Path::new(
            record["binding"]
                .as_str()
                .ok_or("startup binding missing")?,
        );
        if binding_path.try_exists()? {
            let binding = record_binding(&record)?;
            let assets = super::native::owned_directory(&binding, binding_path)?;
            if binding.socket.try_exists()? {
                let deadline = Instant::now() + Duration::from_secs(3);
                let client = aw_service::Client::connect(&binding.socket, deadline)?;
                let status = if client.identity() == &binding.binding.identity {
                    client.call(aw_service::Operation::Status, deadline)?
                } else {
                    // A restarted service cannot own the old generation's binding.
                    json!({"instances":[]})
                };
                if status["instances"]
                    .as_array()
                    .ok_or("service instances missing")?
                    .iter()
                    .any(|instance| instance["instance_id"] == binding.binding.instance_id)
                {
                    return Err("Qoder owner has exited but its service resources are still draining; retry removal".into());
                }
            }
            fs::remove_dir_all(assets)?;
        }
        remove_exact(&path, &inode)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn interrupted_publication_reclaims_only_private_claims_of_exited_owners() {
        for contents in ["", "{\"binding\":"] {
            let directory = tempfile::tempdir().unwrap();
            let mut owner = Command::new("/bin/sleep").arg("30").spawn().unwrap();
            let identity = aw_service::ProcessIdentity::read(owner.id())
                .unwrap()
                .metadata();
            let path = directory.path().join(format!(
                "owner-{}-{}.json",
                owner.id(),
                identity["start_ticks"].as_str().unwrap()
            ));
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
                .unwrap();
            file.write_all(contents.as_bytes()).unwrap();
            assert!(recover(directory.path()).is_err());
            assert!(path.exists());
            owner.kill().unwrap();
            owner.wait().unwrap();
            let unknown = directory.path().join("unrelated");
            fs::write(&unknown, "keep").unwrap();
            recover(directory.path()).unwrap();
            assert!(!path.exists());
            assert_eq!(fs::read_to_string(unknown).unwrap(), "keep");
        }
    }
}
