//! Application-owned startup reuses native preparation and independent registration.

use super::{adapter::HookBinding, read_file, Arguments, Exit, Result};
use aw_adapter::{LaunchContext, LaunchFiles, LaunchInput, NativeOptions};
use aw_exec::{CommandSpec, PackageLeases};
use aw_service::{
    launch_service, Client, Operation, ProcessIdentity, RegistrationCommand, RegistrationHandle,
    RegistrationKey, RegistrationOwner,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::BTreeMap,
    fs::{self, DirBuilder},
    io::Write,
    os::unix::fs::{DirBuilderExt, MetadataExt},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Registration {
    handle: RegistrationHandle,
    policy_revision: String,
}

struct Files {
    path: PathBuf,
    retained: bool,
}

impl LaunchFiles for Files {
    fn directory(&self) -> &Path {
        &self.path
    }
}

impl Drop for Files {
    fn drop(&mut self) {
        if !self.retained {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

pub(super) fn prepare(args: &Arguments) -> Result<Exit> {
    args.check(
        &[
            "--config",
            "--agent",
            "--native-settings",
            "--native-profile",
            "--native-state-dir",
        ],
        false,
    )?;
    let configuration = Path::new(args.required("--config")?).canonicalize()?;
    let bytes = read_file(&configuration, aw_config::MAX_DOCUMENT_BYTES)?;
    let config = aw_config::Validator::new()?.parse(&bytes)?;
    let target = args.required("--agent")?;
    let agent = &config.as_value()["spec"]["agents"][target];
    let name = agent["adapter"]
        .as_str()
        .ok_or("configured Agent target not found")?;
    let native = matches!(name, "openclaw" | "hermes");
    if !native
        && !matches!(
            name,
            "qoder-sdk" | "agentscope" | "pi" | "claude-sdk" | "qwenpaw"
        )
    {
        return Err("native factory preparation requires a supported SDK adapter".into());
    }
    let argv = agent["argv"].as_array().ok_or("Agent argv missing")?;
    let cwd = std::env::current_dir()?.canonicalize()?;
    let environment: BTreeMap<_, _> = std::env::vars_os().collect();
    let mut command = CommandSpec {
        program: argv
            .first()
            .and_then(|value| value.as_str())
            .ok_or("Agent executable missing")?
            .into(),
        args: argv
            .iter()
            .skip(1)
            .map(|value| {
                value
                    .as_str()
                    .map(Into::into)
                    .ok_or("invalid Agent argument")
            })
            .collect::<std::result::Result<_, _>>()?,
        cwd: cwd.clone(),
        environment: environment.clone(),
    };
    super::run::install_signals()?;
    let executable = std::env::current_exe()?;
    let _core_lease = PackageLeases::for_paths([executable.clone()])?;
    let _agent_lease = PackageLeases::acquire([&mut command])?;
    // The CLI transport is a direct child; a controller cannot nominate another PID.
    let pid = unsafe { libc::getppid() } as u32;
    let owner = ProcessIdentity::read(pid)?;
    let mut prepared = aw_adapter::select(name)?.prepare(
        LaunchInput {
            document: config.as_value().clone(),
            target: target.into(),
            options: NativeOptions {
                resident_policy_reload: !native,
                reloadable: native,
                settings: args.flags.get("--native-settings").cloned(),
                profile: args.flags.get("--native-profile").cloned(),
                state_dir: args.flags.get("--native-state-dir").cloned(),
            },
            command,
        },
        &super::run::CANCELLED,
    )?;
    let capabilities = prepared.capabilities();
    if native {
        aw_adapter::reload::supported(&capabilities)?;
    } else {
        aw_adapter::reload::resident_supported(&capabilities)?;
    }
    let steps = aw_core::admission::preflight(&config, target, &capabilities)?;
    prepared.validate_steps(&steps)?;
    let deadline = Instant::now() + Duration::from_secs(30);
    let selected = super::connection::selected(&configuration, deadline, true)?;
    let service = if selected.is_none() {
        Some(launch_service::ensure_service(
            &bytes,
            &executable,
            deadline,
        )?)
    } else {
        None
    };
    let (client, socket) = match selected {
        Some(selected) => selected,
        None => {
            let service = service.as_ref().ok_or("native service missing")?;
            (service.client.clone(), service.paths.socket.clone())
        }
    };
    super::connection::remember(&configuration, &socket, &client)?;
    let state_dir = socket.parent().ok_or("native service directory missing")?;
    let request_id = fs::read_to_string("/proc/sys/kernel/random/uuid")?
        .trim()
        .to_owned();
    let mut files = Files {
        path: state_dir.join(format!("native-{request_id}")),
        retained: false,
    };
    DirBuilder::new().mode(0o700).create(&files.path)?;
    let binding_path = files.path.join("binding.json");
    let plan = prepared.configure(&LaunchContext {
        files: &files,
        binding_path: &binding_path,
        executable: &executable,
        steps: &steps,
    })?;
    owner.check()?;
    let command = RegistrationCommand::Register {
        config: Some(String::from_utf8(bytes)?),
        key: RegistrationKey {
            request_id,
            owner: RegistrationOwner {
                pid,
                start_ticks: owner.metadata()["start_ticks"]
                    .as_str()
                    .ok_or("missing process start identity")?
                    .into(),
            },
        },
        target: target.into(),
        capabilities: capabilities.into(),
        cwd: cwd.to_str().ok_or("non-UTF-8 working directory")?.into(),
        environment: plan.provider_environment,
    };
    let command = if native {
        let RegistrationCommand::Register {
            key,
            target,
            capabilities,
            cwd,
            environment,
            config,
        } = command
        else {
            unreachable!()
        };
        RegistrationCommand::RegisterNative {
            key,
            target,
            capabilities,
            cwd,
            environment,
            config,
        }
    } else {
        command
    };
    let snapshot = client.call(Operation::Registration { command }, deadline)?;
    let registration = Registration {
        handle: serde_json::from_value(snapshot["handle"].clone())?,
        policy_revision: snapshot["policy_revision"]
            .as_str()
            .ok_or("missing registered policy revision")?
            .into(),
    };
    let result = (|| -> Result<()> {
        client.call(
            Operation::EnableReload {
                instance_id: registration.handle.binding.instance_id.clone(),
            },
            deadline,
        )?;
        let binding = HookBinding {
            adapter: name.into(),
            reloadable: native,
            resident_policy_reload: !native,
            binding: registration.handle.binding.clone(),
            registration: Some(registration.clone()),
            socket: socket.clone(),
            cwd,
            events: plan.events,
        };
        files.write("binding.json", &serde_json::to_vec(&binding)?)?;
        // Return only bridge mutations, never echo the application's complete environment.
        let changes: BTreeMap<_, _> = plan
            .command
            .environment
            .iter()
            .filter(|(key, value)| environment.get(*key) != Some(*value))
            .map(|(key, value)| {
                Ok((
                    key.to_str().ok_or("non-UTF-8 native environment key")?,
                    value.to_str().ok_or("non-UTF-8 native environment value")?,
                ))
            })
            .collect::<Result<_>>()?;
        let mut stdout = std::io::stdout().lock();
        serde_json::to_writer(
            &mut stdout,
            &json!({"instance_id":binding.binding.instance_id,
            "environment":changes,"binding":binding_path,
            "command":{"program":plan.command.program,"args":plan.command.args.iter().map(|arg| arg.to_str().ok_or("non-UTF-8 native argument")).collect::<std::result::Result<Vec<_>, _>>()?},"state":"pending_ready"}),
        )?;
        writeln!(stdout)?;
        stdout.flush()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = client.call(
            Operation::Registration {
                command: RegistrationCommand::Release {
                    handle: registration.handle,
                },
            },
            Instant::now() + Duration::from_secs(5),
        );
    }
    result?;
    files.retained = true;
    Ok(Exit::Code(0))
}

pub(super) fn ready(
    root: &HookBinding,
    client: &Client,
    pid: u32,
    deadline: Instant,
) -> Result<()> {
    let Some(registration) = &root.registration else {
        return Ok(());
    };
    if registration.handle.key.owner.pid != pid
        || serde_json::to_value(&registration.handle.binding)?
            != serde_json::to_value(&root.binding)?
    {
        return Err("native factory owner or binding mismatch".into());
    }
    let snapshot = client.call(
        Operation::Registration {
            command: RegistrationCommand::Query {
                key: registration.handle.key.clone(),
            },
        },
        deadline,
    )?;
    if snapshot["state"] == "pending_ready" {
        client.call(
            Operation::Registration {
                command: RegistrationCommand::Ready {
                    handle: registration.handle.clone(),
                    policy_revision: registration.policy_revision.clone(),
                },
            },
            deadline,
        )?;
    } else if snapshot["state"] != "ready" {
        return Err("native registration is unavailable".into());
    }
    match client.call(
        Operation::AssociateProcess {
            instance_id: root.binding.instance_id.clone(),
            pid,
        },
        deadline,
    ) {
        Err(aw_service::Error::Rejected(code)) if code == "process_already_associated" => {}
        result => {
            result?;
        }
    }
    if root.reloadable {
        client.call(
            Operation::StartObservation {
                instance_id: root.binding.instance_id.clone(),
            },
            deadline,
        )?;
        client.call(
            Operation::ReportRun {
                instance_id: root.binding.instance_id.clone(),
                outcome: aw_service::RunOutcome::Started { pid },
            },
            deadline,
        )?;
    }
    Ok(())
}

pub(super) fn release(args: &Arguments) -> Result<Exit> {
    args.check(&["--binding"], false)?;
    let path = Path::new(args.required("--binding")?);
    let root = HookBinding::read(args.required("--binding")?)?;
    let registration = root
        .registration
        .ok_or("not an application-owned binding")?;
    if serde_json::to_value(&registration.handle.binding)? != serde_json::to_value(&root.binding)? {
        return Err("registration binding mismatch".into());
    }
    let directory = path.parent().ok_or("missing native asset directory")?;
    if path.file_name().is_none_or(|name| name != "binding.json")
        || directory.file_name().is_none_or(|name| {
            name != format!("native-{}", registration.handle.key.request_id).as_str()
        })
        || directory.canonicalize()? != directory
    {
        return Err("invalid native asset directory".into());
    }
    let metadata = fs::symlink_metadata(directory)?;
    if !metadata.is_dir()
        || metadata.mode() & 0o7777 != 0o700
        || metadata.uid() != unsafe { libc::geteuid() }
    {
        return Err("unsafe native asset directory".into());
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    let client = Client::connect(&root.socket, deadline)?;
    if client.identity() != &root.binding.identity {
        return Err("stale native service".into());
    }
    client.call(
        Operation::Registration {
            command: RegistrationCommand::Release {
                handle: registration.handle,
            },
        },
        deadline,
    )?;
    fs::remove_dir_all(directory)?;
    Ok(Exit::Code(0))
}
