//! Materialize trusted SDK bootstrap inputs and supervise the foreground owner.

use super::{readiness, run, Exit, Result};
use aw_adapter::Readiness;
use aw_contracts::{
    deployment::{Availability, DeploymentDescriptor},
    instrumentation::{InstrumentationProfile, Runtime},
};
use aw_core::admission::AdapterCapabilities;
use aw_exec::{CommandSpec, PackageLeases};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::OpenOptions,
    io::Write,
    os::unix::{fs::OpenOptionsExt, process::ExitStatusExt},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

const BOOTSTRAP: &str = "AW_PROFILE_BOOTSTRAP";
const PRELOAD: &str = r#"--import="@anolisa/aw-sdk/profile-preload""#;
const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);

pub(crate) fn compile(
    bytes: &[u8],
    target: &str,
    profiles: &[&InstrumentationProfile],
) -> Result<DeploymentDescriptor> {
    let capabilities = aw_adapter::deployment::instrumentation_capabilities(profiles)?;
    compile_for_service(bytes, target, capabilities, Availability::Required)
}

pub(crate) fn compile_for_service(
    bytes: &[u8],
    target: &str,
    capabilities: AdapterCapabilities,
    availability: Availability,
) -> Result<DeploymentDescriptor> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").filter(|value| !value.is_empty());
    let paths = aw_service::launch_service::resolve(bytes, runtime.as_deref().map(Path::new))?;
    Ok(aw_core::descriptor::compile_for_endpoint(
        bytes,
        target,
        capabilities,
        availability,
        &paths.socket,
    )?)
}

pub(super) fn files(
    config: &[u8],
    descriptor: &DeploymentDescriptor,
    profiles: &[&InstrumentationProfile],
    ready: Option<&Readiness>,
    mut write: impl FnMut(&'static str, &[u8]) -> Result<PathBuf>,
) -> Result<(Runtime, Vec<PathBuf>)> {
    let binding = descriptor
        .instrumentation_profiles()
        .ok_or("missing instrumentation binding")?;
    let mut files = vec![
        write("configuration.yaml", config)?,
        write("deployment.json", &serde_json::to_vec(descriptor)?)?,
    ];
    // Payloads are bounded independently by the profile contract and verified
    // against the descriptor before either preload installs any wrappers.
    let documents = profiles
        .iter()
        .map(|profile| {
            Ok((
                profile.id(),
                String::from_utf8(profile.to_canonical_bytes()?)?,
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let mut bootstrap = json!({
        "configFile": files[0],
        "descriptorFile": files[1],
        "profiles": documents,
        "cooperative": ready.is_none(),
    });
    if let Some(ready) = ready {
        bootstrap["ready"] =
            json!({"path": ready.path, "token": ready.token, "hooks": ready.hooks});
    }
    let encoded = serde_json::to_vec(&bootstrap)?;
    if encoded.len() > 1024 * 1024 {
        return Err("instrumentation bootstrap exceeds 1 MiB".into());
    }
    if binding.runtime == Runtime::Python {
        files.push(write(
            "sitecustomize.py",
            aw_adapter::deployment::PYTHON_PROFILE_STARTUP,
        )?);
    }
    files.push(write("profile-bootstrap.json", &encoded)?);
    Ok((binding.runtime, files))
}

pub(super) fn environment(
    runtime: Runtime,
    bootstrap: &Path,
    inherited: &BTreeMap<OsString, OsString>,
) -> Result<BTreeMap<OsString, OsString>> {
    let mut env = BTreeMap::from([(BOOTSTRAP.into(), bootstrap.as_os_str().into())]);
    match runtime {
        Runtime::Python => {
            let path = bootstrap
                .parent()
                .ok_or("missing profile bootstrap directory")?;
            let mut paths = vec![path.to_path_buf()];
            if let Some(value) = inherited
                .get(&OsString::from("PYTHONPATH"))
                .filter(|value| !value.is_empty())
            {
                paths.extend(std::env::split_paths(value));
            }
            env.insert("PYTHONPATH".into(), std::env::join_paths(paths)?);
        }
        Runtime::Js => {
            let mut options = inherited
                .get(&OsString::from("NODE_OPTIONS"))
                .cloned()
                .unwrap_or_default();
            if !options.is_empty() {
                options.push(" ");
            }
            options.push(PRELOAD);
            env.insert("NODE_OPTIONS".into(), options);
        }
    }
    Ok(env)
}

pub(super) fn launch(
    args: &super::Arguments,
    bytes: &[u8],
    target: &str,
    adapter: &str,
    profiles: &[&InstrumentationProfile],
    mut command: CommandSpec,
    shutdown_grace: Duration,
) -> Result<Exit> {
    args.check(&["--config", "--agent"], true)?;
    let descriptor = compile(bytes, target, profiles)?;
    let configuration = Path::new(args.required("--config")?);
    let deadline = Instant::now() + Duration::from_secs(30);
    let recorded = super::connection::recorded(configuration)?;
    if let Some((client, _)) = super::connection::selected(configuration, deadline, true)? {
        if client.identity().config_revision != descriptor.configuration_sha256() {
            return Err("selected service differs from the profile configuration".into());
        }
    }
    let service =
        aw_service::launch_service::ensure_service(bytes, &std::env::current_exe()?, deadline)?;
    if let Some(exit) = run::interrupted() {
        return Ok(exit);
    }
    if service.paths.socket != Path::new(descriptor.endpoint()) {
        return Err("selected service differs from the profile endpoint".into());
    }
    if recorded {
        super::connection::remember(
            configuration,
            &service.paths.socket,
            &service.client,
            deadline,
        )?;
    }
    let artifacts = run::Artifacts::create(&service.paths.state_dir)?;
    let dir = artifacts.0.canonicalize()?;
    let ready = Readiness {
        path: dir.join("profile-ready.json"),
        token: std::fs::read_to_string("/proc/sys/kernel/random/uuid")?
            .trim()
            .into(),
        adapter: adapter.into(),
        hooks: descriptor
            .instrumentation_profiles()
            .map_or(0, |binding| binding.events.len()),
        timeout: STARTUP_TIMEOUT,
    };
    let (runtime, files) = files(bytes, &descriptor, profiles, Some(&ready), |name, bytes| {
        let path = dir.join(name);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o400)
            .open(&path)?;
        file.write_all(bytes)?;
        Ok(path)
    })?;
    let bootstrap = files.last().ok_or("missing profile bootstrap")?;
    command
        .environment
        .extend(environment(runtime, bootstrap, &command.environment)?);
    let _leases = PackageLeases::acquire([&mut command])?;
    let failed = AtomicBool::new(false);
    let cleanup = Readiness {
        path: ready.path.with_extension("closed"),
        token: ready.token.clone(),
        adapter: ready.adapter.clone(),
        hooks: ready.hooks,
        timeout: ready.timeout,
    };
    let mut child_pid = None;
    let status = readiness::run(
        &command,
        &run::SIGNAL,
        &failed,
        shutdown_grace,
        false,
        Some(ready),
        |pid| child_pid = Some(pid),
    )?;
    // aw-exec has reclaimed the entire process group before the files are removed.
    // Explicit process.exit, signals and forced cleanup may bypass SDK exit hooks.
    let confirmed = child_pid.is_some_and(|pid| {
        readiness::matching_receipt(&cleanup, &cleanup.path, pid).is_ok_and(|value| value.is_some())
    });
    if !confirmed {
        return Err("source_cleanup_unconfirmed".into());
    }
    artifacts.cleanup()?;
    Ok(match status.code() {
        Some(code) => Exit::Code(code),
        None => Exit::Signal(status.signal().ok_or("Agent exit has no status")?),
    })
}

pub(super) fn guidance(runtime: Runtime, files: Vec<PathBuf>) -> Result<Value> {
    let bootstrap = files.last().ok_or("missing profile bootstrap")?;
    let environment = environment(runtime, bootstrap, &std::env::vars_os().collect())?;
    let environment = environment
        .into_iter()
        .map(|(key, value)| {
            Ok((
                key.to_string_lossy().into_owned(),
                value
                    .into_string()
                    .map_err(|_| "bootstrap environment must be UTF-8")?,
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    Ok(json!({"environment":environment, "files":files}))
}
