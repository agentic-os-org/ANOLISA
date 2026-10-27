//! Explicit registration of a caller-owned, quiescent native process.

pub(super) mod confirm;
mod process;

use super::{adapter::HookBinding, read_file, readiness, run, Arguments, Exit, Result};
use aw_adapter::{
    attachment::Registration, LaunchContext, LaunchFiles, LaunchInput, NativeOptions,
};
use aw_service::{launch_service, Binding, Client, Operation, RunOutcome};
use process::NativeProcess;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
    sync::atomic::Ordering,
    thread,
    time::{Duration, Instant},
};

struct Files(PathBuf);
impl LaunchFiles for Files {
    fn directory(&self) -> &Path {
        &self.0
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    process: Value,
    target: String,
    binding: PathBuf,
    artifacts: PathBuf,
    registration: Registration,
}

fn call(client: &Client, operation: Operation) -> Result<Value> {
    Ok(client.call(operation, Instant::now() + Duration::from_secs(30))?)
}

fn claim(path: &Path, create: bool) -> Result<File> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    if create {
        options.create_new(true).mode(0o600);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if create && error.kind() == std::io::ErrorKind::AlreadyExists => {
            options.create_new(false).create(false).open(path)?
        }
        Err(error) => return Err(error.into()),
    };
    // Acquire the lock before inspecting an abandoned claim. A live installer
    // still owns an empty file during native/service preparation.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err("unsafe or busy attachment ownership record".into());
    }
    let metadata = file.metadata()?;
    let current = fs::symlink_metadata(path)?;
    // SAFETY: the descriptor is owned here and geteuid only reads user identity.
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o7777 != 0o600
        || metadata.nlink() != 1
        || metadata.dev() != current.dev()
        || metadata.ino() != current.ino()
    {
        return Err("unsafe or busy attachment ownership record".into());
    }
    if create && metadata.len() != 0 {
        return Err("native configuration already has an attachment ownership record".into());
    }
    Ok(file)
}

pub(super) fn install(args: &Arguments) -> Result<Exit> {
    args.check(&["--config", "--agent", "--pid", "--remove"], false)?;
    let bytes = read_file(args.required("--config")?, aw_config::MAX_DOCUMENT_BYTES)?;
    let config = aw_config::Validator::new()?.parse(&bytes)?;
    let target = args.required("--agent")?;
    let agent = &config.as_value()["spec"]["agents"][target];
    let name = agent["adapter"]
        .as_str()
        .ok_or("configured target missing")?;
    let program = agent["argv"][0]
        .as_str()
        .ok_or("configured entrypoint missing")?;
    let pid = args.required("--pid")?.parse()?;
    let native = NativeProcess::read(pid, name, program)?;
    run::install_signals()?;
    let ownership = native.scope.join(".aw-attachment.json");
    if args.flags.contains_key("--remove") {
        return detach(args, &native, target, &ownership);
    }
    let mut lock = claim(&ownership, true)?;
    let result = attach(
        (&bytes, Path::new(args.required("--config")?)),
        &config,
        target,
        name,
        &native,
        &ownership,
        &mut lock,
    );
    // Before a complete durable record exists, there is no native registration.
    if result.is_err() && fs::metadata(&ownership).is_ok_and(|metadata| metadata.len() == 0) {
        fs::remove_file(&ownership)?;
    }
    result
}

fn attach(
    source: (&[u8], &Path),
    config: &aw_config::Configuration,
    target: &str,
    name: &str,
    native: &NativeProcess,
    ownership: &Path,
    lock: &mut File,
) -> Result<Exit> {
    let adapter = aw_adapter::select(name)?;
    let mut prepared = adapter.prepare(
        LaunchInput {
            document: config.as_value().clone(),
            target: target.into(),
            options: NativeOptions {
                reloadable: true,
                ..Default::default()
            },
            command: native.command.clone(),
        },
        &run::CANCELLED,
    )?;
    let capabilities = prepared.capabilities();
    aw_adapter::reload::supported(&capabilities)?;
    let steps = aw_provider::admission::preflight(config, target, &capabilities)?;
    prepared.validate_steps(&steps)?;
    let executable = std::env::current_exe()?;
    let deadline = Instant::now() + Duration::from_secs(30);
    let existing = super::connection::selected(source.1, deadline, true)?;
    let service = if existing.is_none() {
        Some(launch_service::ensure_service(
            source.0,
            &executable,
            deadline,
        )?)
    } else {
        None
    };
    let (client, socket) = match (&existing, &service) {
        (Some((client, socket)), _) => (client, socket),
        (_, Some(service)) => (&service.client, &service.paths.socket),
        _ => unreachable!("an existing or ensured service is always selected"),
    };
    let state_dir = socket.parent().ok_or("service socket has no parent")?;
    super::connection::remember(source.1, socket, client)?;
    let directory = tempfile::Builder::new()
        .prefix("attach-")
        .tempdir_in(state_dir)?;
    let files = Files(directory.path().into());
    let binding_path = files.0.join("binding.json");
    let registration = prepared.attachment(&LaunchContext {
        files: &files,
        binding_path: &binding_path,
        executable: &executable,
        steps: &steps,
    })?;
    if registration.scope() != native.scope {
        return Err("native registration scope mismatch".into());
    }
    native.endpoint(&registration)?;
    native.exclusive()?;
    // Keep the bounded Bind reply even if a signal arrives: it carries the ID
    // needed to release a reservation already committed by the service.
    let binding: Binding = serde_json::from_value(
        client.call(
            Operation::Bind {
                target: target.into(),
                capabilities: capabilities.into(),
                cwd: native
                    .command
                    .cwd
                    .to_str()
                    .ok_or("non-UTF-8 native cwd")?
                    .into(),
                environment: native.command.environment.clone(),
            },
            Instant::now() + Duration::from_secs(30),
        )?,
    )?;
    let instance_id = binding.instance_id.clone();
    let record = Record {
        process: native.identity.metadata(),
        target: target.into(),
        binding: binding_path,
        artifacts: files.0.clone(),
        registration,
    };
    let mut published = false;
    let executed = (|| -> Result<Value> {
        call(
            client,
            Operation::EnableReload {
                instance_id: instance_id.clone(),
            },
        )?;
        call(
            client,
            Operation::AssociateProcess {
                instance_id: instance_id.clone(),
                pid: native.pid,
            },
        )?;
        files.write(
            "binding.json",
            &serde_json::to_vec(&HookBinding {
                adapter: name.into(),
                reloadable: true,
                resident_policy_reload: false,
                binding,
                socket: socket.clone(),
                cwd: native.command.cwd.clone(),
                events: aw_adapter::reload::routes(),
            })?,
        )?;
        lock.write_all(&serde_json::to_vec(&record)?)?;
        lock.sync_all()?;
        if run::CANCELLED.load(Ordering::Acquire) {
            return Err("attachment cancelled".into());
        }
        if config.as_value()["spec"]["agents"][target]
            .get("evidence")
            .is_some()
        {
            call(
                client,
                Operation::StartObservation {
                    instance_id: instance_id.clone(),
                },
            )?;
            call(
                client,
                Operation::ReportRun {
                    instance_id: instance_id.clone(),
                    outcome: RunOutcome::Started { pid: native.pid },
                },
            )?;
        }
        native.identity.check()?;
        native.endpoint(&record.registration)?;
        published = true;
        record.registration.install()?;
        let ready = record.registration.readiness()?;
        let deadline = Instant::now() + ready.timeout;
        let mut probe = Instant::now() + Duration::from_millis(500);
        let mut sequence = 0;
        loop {
            native.identity.check()?;
            if run::CANCELLED.load(Ordering::Acquire) {
                return Err("attachment cancelled".into());
            }
            if readiness::receipt(&ready, native.pid)? {
                break;
            }
            if Instant::now() >= deadline {
                return Err("native attachment confirmation timed out".into());
            }
            if Instant::now() >= probe {
                record.registration.probe(sequence)?;
                sequence += 1;
                probe = Instant::now() + Duration::from_secs(1);
            }
            thread::sleep(Duration::from_millis(25));
        }
        native.exclusive()?;
        let status = call(client, Operation::Status)?;
        let active = status["instances"]
            .as_array()
            .and_then(|instances| {
                instances
                    .iter()
                    .find(|instance| instance["instance_id"] == instance_id)
            })
            .ok_or("attached instance disappeared")?;
        if active["ready"] != true || active["requests"] != 0 || active["tools"] != 0 {
            return Err("attachment did not confirm an idle ready bridge".into());
        }
        use sha2::{Digest, Sha256};
        let revision = format!("{:x}", Sha256::digest(source.0));
        if active["policy_revision"] != revision {
            call(
                client,
                Operation::UpdatePolicy {
                    instance_id: instance_id.clone(),
                    expected_revision: active["policy_revision"]
                        .as_str()
                        .ok_or("missing policy revision")?
                        .into(),
                    config: String::from_utf8(source.0.to_vec())?,
                },
            )?;
        }
        Ok(
            json!({"attached":true,"instance_id":instance_id,"policy_revision":revision,"process":native.identity.metadata()}),
        )
    })();
    match executed {
        Ok(value) => {
            let _ = directory.keep();
            println!("{}", serde_json::to_string(&value)?);
            Ok(Exit::Code(0))
        }
        Err(error) => {
            let removed = if published {
                native
                    .identity
                    .check()
                    .map_err(|e| e.to_string())
                    .and_then(|()| {
                        native
                            .endpoint(&record.registration)
                            .map_err(|e| e.to_string())
                    })
                    .and_then(|()| record.registration.detach().map_err(|e| e.to_string()))
            } else {
                Ok(())
            };
            let released = call(client, Operation::ReleaseInstance { instance_id });
            if removed.is_err() || released.is_err() {
                let _ = directory.keep();
                return Err(format!("{error}; attachment cleanup incomplete; retry aw install --remove with the same config, target and PID; native={removed:?}, binding={released:?}").into());
            }
            if let Err(cleanup) = confirm::cleanup(directory.path()) {
                let _ = directory.keep();
                return Err(format!(
                    "{error}; attachment cleanup incomplete: {cleanup}; retry removal"
                )
                .into());
            }
            fs::remove_file(ownership)?;
            Err(error)
        }
    }
}

fn detach(
    args: &Arguments,
    native: &NativeProcess,
    target: &str,
    ownership: &Path,
) -> Result<Exit> {
    let lock = claim(ownership, false)?;
    if lock.metadata()?.len() == 0 {
        fs::remove_file(ownership)?;
        println!(
            "{}",
            json!({"detached":true,"process":native.identity.metadata()})
        );
        return Ok(Exit::Code(0));
    }
    let record: Record = serde_json::from_slice(&read_file(ownership, 1024 * 1024)?)?;
    if record.target != target || record.process != native.identity.metadata() {
        return Err("attachment process or target changed".into());
    }
    let client = super::connect_service(args, Instant::now() + Duration::from_secs(5))?;
    let binding = super::bridge::binding(record.binding.to_str().ok_or("non-UTF-8 binding")?)?;
    if &binding.binding.identity != client.identity() {
        return Err("attachment service generation changed".into());
    }
    if record.artifacts.parent() != binding.socket.parent()
        || !record
            .artifacts
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("attach-"))
        || record.binding != record.artifacts.join("binding.json")
        || record.registration.scope() != native.scope
    {
        return Err("attachment ownership paths mismatch".into());
    }
    native.endpoint(&record.registration)?;
    let status = call(&client, Operation::Status)?;
    let exists = status["instances"]
        .as_array()
        .ok_or("invalid service status")?
        .iter()
        .any(|value| value["instance_id"] == binding.binding.instance_id);
    if exists {
        // Unbind checks the complete request/tool idle gate and closes admission atomically.
        call(
            &client,
            Operation::Unbind {
                instance_id: binding.binding.instance_id,
            },
        )?;
    }
    native.identity.check()?;
    native.endpoint(&record.registration)?;
    record.registration.detach()?;
    native.identity.check()?;
    confirm::cleanup(&record.artifacts)?;
    fs::remove_file(ownership)?;
    println!(
        "{}",
        json!({"detached":true,"process":native.identity.metadata()})
    );
    Ok(Exit::Code(0))
}
