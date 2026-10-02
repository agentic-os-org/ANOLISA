// SPDX-License-Identifier: Apache-2.0
//! Daemon runtime: bind UDS, accept connections, wire signal handlers.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use blaze_core::backend::BackendKind;
use blaze_core::config::{DaemonConfig, PolicyLoadErrorMode, StorageSyncSchedule};
use blaze_core::kernel::HookRegistry;
use blaze_core::policy::PolicyEngine;
use blaze_core::storage::StorageProvider;
use http_body_util::Full;
use hyper::body::Bytes;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio::net::{TcpListener, UnixListener};
use tokio::signal::unix::{SignalKind, signal};

use crate::api;
use crate::error::{BlazeDaemonError, Result};
use crate::sandbox::StorageSyncLoop;
use crate::sandbox::template::{
    PinnedConfigSource, PolicyLoadDisposition, TemplateCatalog,
    validate_template_roots_with_policy_mode,
};
use crate::spawner::{
    BubblewrapSpawner, DynSpawner, FirecrackerSpawner, MockSpawner, SpawnerRegistry,
};
use crate::state::ServerState;
use crate::state_store::StateStore;

/// Boot the daemon: load config + policies, prepare state directories,
/// bind the API socket, and run the accept loop until SIGTERM/SIGINT.
pub async fn run(config_path: &Path) -> Result<()> {
    let loaded = load_daemon_config(config_path)?;
    run_loaded_config(loaded).await
}

struct LoadedDaemonConfig {
    config: DaemonConfig,
    source: PinnedConfigSource,
}

fn load_daemon_config(config_path: &Path) -> Result<LoadedDaemonConfig> {
    let mut source = PinnedConfigSource::open(config_path)?;
    let raw = source.read_to_string()?;
    let mut config: DaemonConfig = toml::from_str(&raw).map_err(blaze_core::BlazeError::from)?;
    absolutize_backend_paths(&mut config)?;
    config.validate()?;
    if config.pool.is_some() {
        tracing::warn!(
            path = %config_path.display(),
            "ignoring legacy packaged [pool] defaults; remove this section because reusable-instance management is unavailable"
        );
    }
    tracing::info!(?config_path, "loaded daemon config");
    Ok(LoadedDaemonConfig { config, source })
}

fn absolutize_backend_paths(config: &mut DaemonConfig) -> Result<()> {
    let current_dir = std::env::current_dir()?;
    for path in config.backends.values_mut() {
        if !path.is_absolute() {
            *path = current_dir.join(&*path);
        }
    }
    Ok(())
}

async fn run_loaded_config(loaded: LoadedDaemonConfig) -> Result<()> {
    let LoadedDaemonConfig { config, source } = loaded;
    let sync_schedule = config.storage.sync_schedule()?;
    let sync_timeout = config.storage.sync_timeout_duration()?;

    let template_roots = validate_template_roots_with_policy_mode(
        &config.template,
        &config.storage.images_dir,
        &config.storage.instances_dir,
        &config.policy.dir,
        &config.backends,
        &config.daemon.state_dir,
        &config.daemon.socket,
        Some(&source),
        config.policy.on_load_error,
    )?;
    let policy_load = template_roots.policy_load_disposition();
    let template_catalog = TemplateCatalog::open_validated(&config.template, template_roots)?;
    ensure_dirs(&config)?;
    // Retain the accepted state-root object before policy, backend, and
    // storage initialization so later code cannot reopen a replacement path.
    let state_store = StateStore::open(config.daemon.state_dir.clone())?;
    let policy = load_policy_engine(&config, policy_load)?;
    let hook = HookRegistry::new();
    let network_required = policy.policies().iter().any(|policy| {
        policy
            .backend
            .firecracker
            .as_ref()
            .is_some_and(|config| config.enable_network)
            && policy
                .select
                .backend_priority
                .contains(&BackendKind::Firecracker)
    });
    let (spawners, active_backend) = build_spawners(&config, network_required).await;

    // Build storage provider
    if config.storage.provider != "file" && config.storage.provider != "auto" {
        tracing::warn!(
            provider = %config.storage.provider,
            "unsupported storage provider, falling back to file"
        );
    }
    let storage: Arc<dyn StorageProvider> = {
        use crate::file_provider::FileStorageProvider;
        // Keep immutable images and provider-owned runtime slots separate.
        tokio::fs::create_dir_all(&config.storage.images_dir).await?;
        tokio::fs::create_dir_all(&config.storage.instances_dir).await?;
        let fp = FileStorageProvider::with_images(
            config.storage.images_dir.clone(),
            config.storage.instances_dir.clone(),
        );
        match fp.probe().await {
            Ok(true) => {
                tracing::info!(dir = %config.storage.images_dir.display(), "storage provider ready");
            }
            _ => {
                tracing::warn!("storage probe returned false, continuing anyway");
            }
        }
        Arc::new(fp)
    };

    let socket_path = config.daemon.socket.clone();
    let http_addr = config.listen.http_addr.clone();
    let state = Arc::new(ServerState::build_with_store(
        config,
        policy,
        hook,
        spawners,
        active_backend,
        storage,
        template_catalog,
        state_store,
    )?);
    let reconciliation = state.manager.reconcile_startup().await;
    tracing::info!(
        attempted = reconciliation.attempted,
        completed = reconciliation.completed,
        failed = reconciliation.failures.len(),
        "startup sandbox reconciliation completed"
    );
    for failure in reconciliation.failures {
        tracing::warn!(
            instance = %failure.instance_id,
            error = %failure.error,
            "sandbox remains recovery-required after startup reconciliation"
        );
    }

    if socket_path.exists() {
        std::fs::remove_file(&socket_path)?;
    }
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let listener = UnixListener::bind(&socket_path)?;
    tracing::info!(socket = %socket_path.display(), "blaze UDS API listening");

    // Optional TCP listener for remote platform API
    let tcp_listener = if !http_addr.is_empty() {
        let tcp = TcpListener::bind(&http_addr)
            .await
            .map_err(|e| BlazeDaemonError::Internal(format!("bind TCP {http_addr}: {e}")))?;
        tracing::info!(addr = %http_addr, "blaze HTTP API listening");
        Some(tcp)
    } else {
        None
    };

    serve(listener, tcp_listener, state, sync_schedule, sync_timeout).await
}

fn ensure_dirs(cfg: &DaemonConfig) -> Result<()> {
    cfg.validate()?;
    std::fs::create_dir_all(&cfg.daemon.state_dir)?;
    // TemplateCatalog::open_validated creates and retains the accepted catalog
    // object. Reopening its configured path here would discard that binding.
    std::fs::create_dir_all(&cfg.storage.images_dir)?;
    std::fs::create_dir_all(&cfg.storage.instances_dir)?;
    let images_dir = std::fs::canonicalize(&cfg.storage.images_dir)?;
    let instances_dir = std::fs::canonicalize(&cfg.storage.instances_dir)?;
    blaze_core::config::validate_storage_paths(&images_dir, &instances_dir)?;
    if let Some(parent) = cfg.daemon.socket.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

/// Build the [`crate::spawner::BackendSpawner`] implementations used by API
/// handlers. Probes
/// `[backends]` for known backends in priority order:
///   1. `firecracker` → [`FirecrackerSpawner`]
///   2. `bubblewrap` → [`BubblewrapSpawner`]
///   3. fallback → [`MockSpawner`]
async fn build_spawners(
    cfg: &DaemonConfig,
    network_required: bool,
) -> (SpawnerRegistry, BackendKind) {
    let firecracker: DynSpawner = Arc::new(FirecrackerSpawner::with_network_requirement(
        cfg.storage.images_dir.clone(),
        network_required,
    ));
    let bubblewrap: DynSpawner = Arc::new(BubblewrapSpawner);
    let mock: DynSpawner = Arc::new(MockSpawner);
    let mut spawners = SpawnerRegistry::new();
    spawners.insert(BackendKind::Firecracker, firecracker.clone());
    spawners.insert(BackendKind::Bubblewrap, bubblewrap.clone());
    spawners.insert(BackendKind::Mock, mock);

    // --- Firecracker --------------------------------------------------------
    if let Some(fc_path) = cfg.backends.get(BackendKind::Firecracker.as_str()).cloned() {
        match firecracker.probe(&fc_path).await {
            Ok(true) => {
                tracing::info!(
                    binary = %fc_path.display(),
                    images_dir = %cfg.storage.images_dir.display(),
                    "data plane: using FirecrackerSpawner",
                );
                return (spawners, BackendKind::Firecracker);
            }
            Ok(false) => {
                tracing::warn!(
                    binary = %fc_path.display(),
                    "firecracker binary probe failed, trying next backend",
                );
            }
            Err(err) => {
                tracing::warn!(
                    ?err,
                    binary = %fc_path.display(),
                    "firecracker probe error, trying next backend",
                );
            }
        }
    }

    // --- Bubblewrap (bwrap) --------------------------------------------------
    if let Some(path) = cfg.backends.get(BackendKind::Bubblewrap.as_str()).cloned() {
        match bubblewrap.probe(&path).await {
            Ok(true) => {
                tracing::info!(
                    binary = %path.display(),
                    "data plane: using BubblewrapSpawner",
                );
                return (spawners, BackendKind::Bubblewrap);
            }
            Ok(false) => {
                tracing::warn!(
                    binary = %path.display(),
                    "bubblewrap binary missing, falling back to MockSpawner",
                );
            }
            Err(err) => {
                tracing::warn!(
                    ?err,
                    binary = %path.display(),
                    "bubblewrap probe failed, falling back to MockSpawner",
                );
            }
        }
    }

    // --- Fallback: MockSpawner -----------------------------------------------
    tracing::warn!(
        "no usable backend found in [backends], using MockSpawner (data plane is simulated)",
    );
    (spawners, BackendKind::Mock)
}

fn load_policy_engine(
    cfg: &DaemonConfig,
    disposition: PolicyLoadDisposition,
) -> Result<PolicyEngine> {
    if disposition == PolicyLoadDisposition::UseEmpty {
        if cfg.policy.on_load_error == PolicyLoadErrorMode::Fail {
            return Err(BlazeDaemonError::Internal(format!(
                "policy boundary discovery did not complete for {}",
                cfg.policy.dir.display()
            )));
        }
        tracing::warn!(
            dir = %cfg.policy.dir.display(),
            "policy boundary discovery did not complete; starting with empty policy engine"
        );
        return Ok(PolicyEngine::new());
    }
    if !cfg.policy.dir.exists() {
        if cfg.policy.on_load_error == PolicyLoadErrorMode::Fail {
            return Err(BlazeDaemonError::Internal(format!(
                "policy.dir does not exist: {}",
                cfg.policy.dir.display()
            )));
        }
        tracing::warn!(
            dir = %cfg.policy.dir.display(),
            "policy dir missing, starting with empty policy engine"
        );
        return Ok(PolicyEngine::new());
    }
    match PolicyEngine::load_dir(&cfg.policy.dir) {
        Ok(engine) => Ok(engine),
        Err(err) if cfg.policy.on_load_error == PolicyLoadErrorMode::Warn => {
            tracing::warn!(?err, "policy load failed, continuing with empty engine");
            Ok(PolicyEngine::new())
        }
        Err(err) => Err(err.into()),
    }
}

async fn serve(
    uds: UnixListener,
    tcp: Option<TcpListener>,
    state: Arc<ServerState>,
    sync_schedule: StorageSyncSchedule,
    sync_timeout: Duration,
) -> Result<()> {
    let mut sighup = signal(SignalKind::hangup())
        .map_err(|e| BlazeDaemonError::Internal(format!("install SIGHUP handler: {e}")))?;
    let mut sigterm = signal(SignalKind::terminate())
        .map_err(|e| BlazeDaemonError::Internal(format!("install SIGTERM handler: {e}")))?;
    let mut sigint = signal(SignalKind::interrupt())
        .map_err(|e| BlazeDaemonError::Internal(format!("install SIGINT handler: {e}")))?;
    let mut sync_loop = match sync_schedule {
        StorageSyncSchedule::Disabled => {
            tracing::info!("periodic storage artifact synchronization is disabled");
            None
        }
        StorageSyncSchedule::Every(interval) => Some(
            state
                .manager
                .start_storage_sync_loop(interval, sync_timeout),
        ),
    };
    let mut service_result = Ok(());

    loop {
        tokio::select! {
            result = observe_storage_sync_exit(&mut sync_loop), if sync_loop.is_some() => {
                service_result = result;
                break;
            }
            res = uds.accept() => {
                let (stream, _peer) = match res {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::error!(error = %e, "UDS accept failed");
                        continue;
                    }
                };
                spawn_conn(TokioIo::new(stream), state.clone());
            }
            res = async { match &tcp { Some(l) => l.accept().await, None => std::future::pending().await }}, if tcp.is_some() => {
                let (stream, peer) = match res {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::error!(error = %e, "TCP accept failed");
                        continue;
                    }
                };
                tracing::debug!(?peer, "TCP connection");
                spawn_conn(TokioIo::new(stream), state.clone());
            }
            _ = sighup.recv() => {
                tracing::info!("SIGHUP received: reloading policies");
                if let Err(err) = reload_policies(&state) {
                    tracing::error!(?err, "policy reload failed");
                }
            }
            _ = sigterm.recv() => {
                tracing::info!("SIGTERM received: shutting down");
                break;
            }
            _ = sigint.recv() => {
                tracing::info!("SIGINT received: shutting down");
                break;
            }
        }
    }

    state.manager.cancel_template_imports();
    if let Some(sync_loop) = sync_loop.as_mut() {
        service_result = merge_stage_result(
            service_result,
            "storage artifact synchronization shutdown",
            sync_loop.shutdown().await,
        );
    }
    service_result = merge_stage_result(
        service_result,
        "runtime template import shutdown",
        state.manager.wait_for_template_imports().await,
    );
    tracing::info!("blaze daemon stopped");
    service_result
}

async fn observe_storage_sync_exit(sync_loop: &mut Option<StorageSyncLoop>) -> Result<()> {
    sync_loop
        .as_mut()
        .expect("storage sync loop exists while its select branch is enabled")
        .observe_exit()
        .await
}

fn merge_stage_result(current: Result<()>, stage: &str, next: Result<()>) -> Result<()> {
    match (current, next) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(previous), Err(next)) => Err(BlazeDaemonError::RecoveryRequired(format!(
            "service failed: {previous}; {stage} failed: {next}"
        ))),
    }
}

fn spawn_conn<I>(io: TokioIo<I>, state: Arc<ServerState>)
where
    I: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let svc = service_fn(move |req| {
            let state = state.clone();
            async move { api::handle(req, state).await }
        });
        if let Err(err) = http1::Builder::new().serve_connection(io, svc).await {
            tracing::debug!(?err, "connection closed with error");
        }
        let _: Option<Full<Bytes>> = None;
    });
}

fn reload_policies(state: &Arc<ServerState>) -> Result<()> {
    let dir = {
        let cfg = state
            .config
            .lock()
            .map_err(|_| BlazeDaemonError::Internal("config lock poisoned".into()))?;
        cfg.policy.dir.clone()
    };
    let engine = PolicyEngine::load_dir(&dir)?;
    let count = engine.policies().len();
    {
        let mut policy = state
            .policy
            .lock()
            .map_err(|_| BlazeDaemonError::Internal("policy lock poisoned".into()))?;
        *policy = engine;
    }
    tracing::info!(policies = count, "policy engine reloaded via SIGHUP");
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

    use super::*;

    #[test]
    fn policy_boundary_fallback_prevents_a_later_directory_rescan() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut config = DaemonConfig::default();
        config.template.dir = temp.path().join("catalog");
        config.template.import_root = Some(temp.path().join("imports"));
        config.storage.images_dir = temp.path().join("images");
        config.storage.instances_dir = temp.path().join("instances");
        config.policy.dir = temp.path().join("policies");
        config.policy.on_load_error = PolicyLoadErrorMode::Warn;
        config.daemon.state_dir = temp.path().join("state");
        config.daemon.socket = temp.path().join("run/api.sock");
        for directory in [
            &config.template.dir,
            config.template.import_root.as_ref().expect("import root"),
            &config.storage.images_dir,
            &config.storage.instances_dir,
            &config.policy.dir,
            &config.daemon.state_dir,
        ] {
            std::fs::create_dir(directory).expect("startup directory");
        }
        let policy_path = config.policy.dir.join("recovered.toml");
        symlink("recovered.toml", &policy_path).expect("policy loop");

        let roots = validate_template_roots_with_policy_mode(
            &config.template,
            &config.storage.images_dir,
            &config.storage.instances_dir,
            &config.policy.dir,
            &config.backends,
            &config.daemon.state_dir,
            &config.daemon.socket,
            None,
            config.policy.on_load_error,
        )
        .expect("warn mode boundary validation");
        let disposition = roots.policy_load_disposition();
        let _catalog =
            TemplateCatalog::open_validated(&config.template, roots).expect("catalog owner");

        std::fs::remove_file(&policy_path).expect("remove policy loop");
        std::fs::write(
            &policy_path,
            r#"
manifest_version = 1
policy_name = "recovered"

[match]
workload_class = "agent-rl"

[select]
backend_priority = ["bubblewrap"]
"#,
        )
        .expect("policy file");

        let empty = load_policy_engine(&config, disposition).expect("use empty policy engine");
        assert!(empty.policies().is_empty());

        let recovered_roots = validate_template_roots_with_policy_mode(
            &config.template,
            &config.storage.images_dir,
            &config.storage.instances_dir,
            &config.policy.dir,
            &config.backends,
            &config.daemon.state_dir,
            &config.daemon.socket,
            None,
            config.policy.on_load_error,
        )
        .expect("recovered policy boundary validation");
        let loaded = load_policy_engine(&config, recovered_roots.policy_load_disposition())
            .expect("load validated policy directory");
        assert_eq!(loaded.policies().len(), 1);

        config.policy.on_load_error = PolicyLoadErrorMode::Fail;
        load_policy_engine(&config, PolicyLoadDisposition::UseEmpty)
            .expect_err("fail mode must not accept an incomplete boundary discovery");
    }

    #[test]
    fn generic_directory_setup_does_not_reopen_the_template_catalog() {
        let temp = tempfile::tempdir().expect("tempdir");
        let configured_parent = temp.path().join("configured");
        let detached_parent = temp.path().join("detached");
        std::fs::create_dir(&configured_parent).expect("configured parent");
        let mut config = DaemonConfig::default();
        config.template.dir = configured_parent.join("catalog");
        config.template.import_root = Some(temp.path().join("imports"));
        config.storage.images_dir = temp.path().join("images");
        config.storage.instances_dir = temp.path().join("instances");
        config.policy.dir = temp.path().join("policies");
        config.daemon.state_dir = temp.path().join("state");
        config.daemon.socket = temp.path().join("run/api.sock");
        std::fs::create_dir(config.template.import_root.as_ref().expect("import root"))
            .expect("import root");
        std::fs::create_dir(&config.policy.dir).expect("policy directory");

        let roots = validate_template_roots_with_policy_mode(
            &config.template,
            &config.storage.images_dir,
            &config.storage.instances_dir,
            &config.policy.dir,
            &config.backends,
            &config.daemon.state_dir,
            &config.daemon.socket,
            None,
            config.policy.on_load_error,
        )
        .expect("validated roots");
        let _catalog =
            TemplateCatalog::open_validated(&config.template, roots).expect("catalog owner");

        std::fs::rename(&configured_parent, &detached_parent).expect("detach catalog ancestor");
        std::fs::create_dir(&configured_parent).expect("replacement catalog ancestor");

        ensure_dirs(&config).expect("prepare non-catalog directories");

        assert!(!config.template.dir.exists());
        assert!(detached_parent.join("catalog").is_dir());
        assert!(config.storage.images_dir.is_dir());
        assert!(config.storage.instances_dir.is_dir());
        assert!(config.daemon.state_dir.is_dir());
    }

    #[test]
    fn service_and_shutdown_failures_are_all_retained() {
        let service = Err(BlazeDaemonError::Internal(
            "storage synchronization worker failed".to_string(),
        ));
        let storage_shutdown = Err(BlazeDaemonError::Internal(
            "worker shutdown failed".to_string(),
        ));
        let import_shutdown = Err(BlazeDaemonError::Internal(
            "import shutdown failed".to_string(),
        ));

        let result = merge_stage_result(
            service,
            "storage artifact synchronization shutdown",
            storage_shutdown,
        );
        let error = merge_stage_result(result, "runtime template import shutdown", import_shutdown)
            .expect_err("all failures must be reported");

        assert!(error.to_string().contains("service failed"));
        assert!(error.to_string().contains("synchronization worker failed"));
        assert!(
            error
                .to_string()
                .contains("storage artifact synchronization shutdown")
        );
        assert!(error.to_string().contains("worker shutdown failed"));
        assert!(
            error
                .to_string()
                .contains("runtime template import shutdown")
        );
        assert!(error.to_string().contains("import shutdown failed"));
    }

    #[tokio::test]
    async fn resolved_catalog_overlap_fails_before_owned_directory_creation() {
        let temp = tempfile::tempdir().expect("tempdir");
        let catalog = temp.path().join("catalog");
        let alias = temp.path().join("alias");
        let import_root = temp.path().join("imports");
        std::fs::create_dir(&catalog).expect("catalog");
        std::fs::create_dir(&import_root).expect("import root");
        symlink(&catalog, &alias).expect("catalog alias");

        let mut config = DaemonConfig::default();
        config.template.dir = catalog.clone();
        config.template.import_root = Some(import_root);
        config.storage.images_dir = alias.join("images");
        config.storage.instances_dir = temp.path().join("instances");
        config.policy.dir = temp.path().join("policies");
        config.daemon.state_dir = temp.path().join("state");
        config.daemon.socket = temp.path().join("run/api.sock");
        let config_path = temp.path().join("config.toml");
        std::fs::write(
            &config_path,
            toml::to_string(&config).expect("serialize config"),
        )
        .expect("write config");

        let error = run(&config_path)
            .await
            .expect_err("resolved catalog overlap must fail startup");

        assert!(
            error.to_string().contains("storage.images_dir"),
            "unexpected startup error: {error}"
        );
        assert!(!catalog.join("images").exists());
        assert!(!config.storage.instances_dir.exists());
        assert!(config.template.dir.exists());
        assert!(!config.daemon.state_dir.exists());
    }

    #[tokio::test]
    async fn configured_backend_binaries_cannot_overlap_template_roots_before_mutation() {
        for (backend, binary_in_import_root) in [("firecracker", false), ("bubblewrap", true)] {
            let temp = tempfile::tempdir().expect("tempdir");
            let catalog = temp.path().join("catalog");
            let import_root = temp.path().join("imports");
            for root in [&catalog, &import_root] {
                std::fs::create_dir(root).expect("runtime template root");
                std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o750))
                    .expect("root mode");
            }
            let protected_root = if binary_in_import_root {
                &import_root
            } else {
                &catalog
            };
            let binary = protected_root.join(backend);
            std::fs::write(&binary, b"backend executable").expect("backend binary");
            std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o750))
                .expect("binary mode");

            let mut config = DaemonConfig::default();
            config.template.dir = catalog.clone();
            config.template.import_root = Some(import_root.clone());
            config.storage.images_dir = temp.path().join("images");
            config.storage.instances_dir = temp.path().join("instances");
            config.policy.dir = temp.path().join("policies");
            config.daemon.state_dir = temp.path().join("state");
            config.daemon.socket = temp.path().join("run/api.sock");
            config.backends.insert(backend.to_string(), binary.clone());
            let config_path = temp.path().join("config.toml");
            std::fs::write(
                &config_path,
                toml::to_string(&config).expect("serialize config"),
            )
            .expect("write config");

            let error = run(&config_path)
                .await
                .expect_err("backend binary must remain outside catalog ownership");

            let message = error.to_string();
            assert!(message.contains(&format!("backends.{backend}")));
            assert_eq!(
                std::fs::read(&binary).expect("binary remains readable"),
                b"backend executable"
            );
            for root in [&catalog, &import_root] {
                assert_eq!(
                    std::fs::metadata(root).expect("root metadata").mode() & 0o777,
                    0o750
                );
            }
            assert!(!config.storage.images_dir.exists());
            assert!(!config.storage.instances_dir.exists());
            assert!(config.template.dir.exists());
            assert!(!config.daemon.state_dir.exists());
        }
    }

    #[tokio::test]
    async fn configured_backend_link_locations_cannot_overlap_template_roots_before_mutation() {
        for (backend, link_in_import_root) in [("firecracker", false), ("bubblewrap", true)] {
            let temp = tempfile::tempdir().expect("tempdir");
            let catalog = temp.path().join("catalog");
            let import_root = temp.path().join("imports");
            let backend_root = temp.path().join("backends");
            for root in [&catalog, &import_root, &backend_root] {
                std::fs::create_dir(root).expect("root directory");
                std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o750))
                    .expect("root mode");
            }
            let target = backend_root.join(backend);
            std::fs::write(&target, b"backend executable").expect("backend binary");
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o750))
                .expect("binary mode");
            let protected_root = if link_in_import_root {
                &import_root
            } else {
                &catalog
            };
            let configured_link = protected_root.join(backend);
            symlink(&target, &configured_link).expect("backend symlink");

            let mut config = DaemonConfig::default();
            config.template.dir = catalog.clone();
            config.template.import_root = Some(import_root.clone());
            config.storage.images_dir = temp.path().join("images");
            config.storage.instances_dir = temp.path().join("instances");
            config.policy.dir = temp.path().join("policies");
            config.daemon.state_dir = temp.path().join("state");
            config.daemon.socket = temp.path().join("run/api.sock");
            config
                .backends
                .insert(backend.to_string(), configured_link.clone());
            let config_path = temp.path().join("config.toml");
            std::fs::write(
                &config_path,
                toml::to_string(&config).expect("serialize config"),
            )
            .expect("write config");

            let error = run(&config_path)
                .await
                .expect_err("backend link location must remain outside catalog ownership");

            assert!(
                error
                    .to_string()
                    .contains(&format!("backends.{backend} configured path"))
            );
            assert!(
                std::fs::symlink_metadata(&configured_link)
                    .expect("configured backend link")
                    .file_type()
                    .is_symlink()
            );
            assert_eq!(
                std::fs::read(&target).expect("backend target remains readable"),
                b"backend executable"
            );
            for root in [&catalog, &import_root, &backend_root] {
                assert_eq!(
                    std::fs::metadata(root).expect("root metadata").mode() & 0o777,
                    0o750
                );
            }
            assert!(!config.storage.images_dir.exists());
            assert!(!config.storage.instances_dir.exists());
            assert!(config.template.dir.exists());
            assert!(!config.daemon.state_dir.exists());
        }
    }

    #[tokio::test]
    async fn configured_config_link_locations_cannot_overlap_template_roots_before_mutation() {
        for link_in_import_root in [false, true] {
            let temp = tempfile::tempdir().expect("tempdir");
            let catalog = temp.path().join("catalog");
            let import_root = temp.path().join("imports");
            let config_root = temp.path().join("config");
            for root in [&catalog, &import_root, &config_root] {
                std::fs::create_dir(root).expect("root directory");
                std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o750))
                    .expect("root mode");
            }

            let mut config = DaemonConfig::default();
            config.template.dir = catalog.clone();
            config.template.import_root = Some(import_root.clone());
            config.storage.images_dir = temp.path().join("images");
            config.storage.instances_dir = temp.path().join("instances");
            config.policy.dir = temp.path().join("policies");
            config.daemon.state_dir = temp.path().join("state");
            config.daemon.socket = temp.path().join("run/api.sock");

            let target = config_root.join("config.toml");
            let contents = toml::to_string(&config).expect("serialize config");
            std::fs::write(&target, &contents).expect("write config target");
            let protected_root = if link_in_import_root {
                &import_root
            } else {
                &catalog
            };
            let configured_link = protected_root.join("config.toml");
            symlink(&target, &configured_link).expect("config symlink");

            let error = run(&configured_link)
                .await
                .expect_err("config link location must remain outside catalog ownership");

            assert!(error.to_string().contains("config_path configured path"));
            assert!(
                std::fs::symlink_metadata(&configured_link)
                    .expect("configured config link")
                    .file_type()
                    .is_symlink()
            );
            assert_eq!(
                std::fs::read_to_string(&target).expect("config target remains readable"),
                contents
            );
            for root in [&catalog, &import_root, &config_root] {
                assert_eq!(
                    std::fs::metadata(root).expect("root metadata").mode() & 0o777,
                    0o750
                );
            }
            assert!(!config.storage.images_dir.exists());
            assert!(!config.storage.instances_dir.exists());
            assert!(config.template.dir.exists());
            assert!(!config.daemon.state_dir.exists());
        }
    }

    #[tokio::test]
    async fn every_configured_backend_path_is_checked_before_catalog_creation() {
        let current_dir = std::env::current_dir().expect("current directory");
        let temp = tempfile::tempdir_in(&current_dir).expect("tempdir below current directory");
        let catalog = temp.path().join("catalog");
        let import_root = temp.path().join("imports");
        std::fs::create_dir(&import_root).expect("import root");

        let mut config = DaemonConfig::default();
        config.template.dir = catalog.clone();
        config.template.import_root = Some(import_root);
        config.storage.images_dir = temp.path().join("images");
        config.storage.instances_dir = temp.path().join("instances");
        config.policy.dir = temp.path().join("policies");
        config.daemon.state_dir = temp.path().join("state");
        config.daemon.socket = temp.path().join("run/api.sock");
        let missing_binary = catalog.join("future-backend");
        config.backends.insert(
            "future-backend".to_string(),
            missing_binary
                .strip_prefix(&current_dir)
                .expect("backend path below current directory")
                .to_path_buf(),
        );
        let config_path = temp.path().join("config.toml");
        std::fs::write(
            &config_path,
            toml::to_string(&config).expect("serialize config"),
        )
        .expect("write config");

        let error = run(&config_path)
            .await
            .expect_err("every configured backend path must remain outside catalog ownership");

        assert!(error.to_string().contains("backends.future-backend"));
        assert!(!missing_binary.exists());
        assert!(!catalog.exists());
        assert!(!config.storage.images_dir.exists());
        assert!(!config.storage.instances_dir.exists());
        assert!(!config.template.dir.exists());
        assert!(!config.daemon.state_dir.exists());
    }

    #[test]
    fn config_load_reuses_absolute_backend_paths() {
        let current_dir = std::env::current_dir().expect("current directory");
        let temp = tempfile::tempdir_in(&current_dir).expect("tempdir below current directory");
        let relative_binary = temp
            .path()
            .join("future-backend")
            .strip_prefix(&current_dir)
            .expect("backend path below current directory")
            .to_path_buf();
        let mut config = DaemonConfig::default();
        config
            .backends
            .insert("future-backend".to_string(), relative_binary.clone());
        let config_path = temp.path().join("config.toml");
        std::fs::write(
            &config_path,
            toml::to_string(&config).expect("serialize config"),
        )
        .expect("write config");

        let loaded = load_daemon_config(&config_path).expect("load daemon config");

        assert_eq!(
            loaded.config.backends.get("future-backend"),
            Some(&current_dir.join(relative_binary))
        );
    }

    #[test]
    fn config_load_accepts_a_preserved_packaged_pool_section() {
        let current_dir = std::env::current_dir().expect("current directory");
        let temp = tempfile::tempdir_in(&current_dir).expect("tempdir below current directory");
        let config_path = temp.path().join("config.toml");
        std::fs::write(
            &config_path,
            "[daemon]\nlog_level = \"debug\"\n\n[pool]\ndefault_warm_ttl = \"30m\"\ngc_interval = \"5m\"\n",
        )
        .expect("write legacy packaged configuration");

        let loaded = load_daemon_config(&config_path).expect("load preserved package config");

        assert!(loaded.config.pool.is_some());
        assert_eq!(loaded.config.daemon.log_level, "debug");
    }

    #[tokio::test]
    async fn loaded_config_inside_catalog_fails_before_startup_changes_catalog() {
        let temp = tempfile::tempdir().expect("tempdir");
        let catalog = temp.path().join("catalog");
        let import_root = temp.path().join("imports");
        std::fs::create_dir(&catalog).expect("catalog");
        std::fs::create_dir(&import_root).expect("import root");

        let mut config = DaemonConfig::default();
        config.template.dir = catalog.clone();
        config.template.import_root = Some(import_root);
        config.storage.images_dir = temp.path().join("images");
        config.storage.instances_dir = temp.path().join("instances");
        config.policy.dir = temp.path().join("policies");
        config.daemon.state_dir = temp.path().join("state");
        config.daemon.socket = temp.path().join("run/api.sock");
        let config_path = catalog.join("config.toml");
        let contents = toml::to_string(&config).expect("serialize config");
        std::fs::write(&config_path, &contents).expect("write config");

        let error = run(&config_path)
            .await
            .expect_err("catalog must not contain the loaded config");

        let message = error.to_string();
        assert!(message.contains("template.dir"));
        assert!(message.contains("config_path"));
        assert_eq!(
            std::fs::read_to_string(&config_path).expect("config remains readable"),
            contents
        );
        assert!(!config.storage.images_dir.exists());
        assert!(!config.daemon.state_dir.exists());
    }

    #[tokio::test]
    async fn loaded_config_inside_import_root_fails_before_catalog_creation() {
        let temp = tempfile::tempdir().expect("tempdir");
        let catalog = temp.path().join("catalog");
        let import_root = temp.path().join("imports");
        std::fs::create_dir(&import_root).expect("import root");

        let mut config = DaemonConfig::default();
        config.template.dir = catalog.clone();
        config.template.import_root = Some(import_root.clone());
        config.storage.images_dir = temp.path().join("images");
        config.storage.instances_dir = temp.path().join("instances");
        config.policy.dir = temp.path().join("policies");
        config.daemon.state_dir = temp.path().join("state");
        config.daemon.socket = temp.path().join("run/api.sock");
        let config_path = import_root.join("config.toml");
        std::fs::write(
            &config_path,
            toml::to_string(&config).expect("serialize config"),
        )
        .expect("write config");

        let error = run(&config_path)
            .await
            .expect_err("import root must not contain the loaded config");

        let message = error.to_string();
        assert!(message.contains("template.import_root"));
        assert!(message.contains("config_path"));
        assert!(config_path.exists());
        assert!(!catalog.exists());
        assert!(!config.storage.images_dir.exists());
        assert!(!config.daemon.state_dir.exists());
    }

    #[tokio::test]
    async fn loaded_catalog_config_stays_bound_after_alias_retarget() {
        let temp = tempfile::tempdir().expect("tempdir");
        let catalog = temp.path().join("catalog");
        let import_root = temp.path().join("imports");
        let replacement = temp.path().join("replacement");
        let alias = temp.path().join("config-alias");
        for directory in [&catalog, &import_root, &replacement] {
            std::fs::create_dir(directory).expect("directory");
        }
        std::fs::set_permissions(&catalog, std::fs::Permissions::from_mode(0o750))
            .expect("catalog mode");

        let mut config = DaemonConfig::default();
        config.template.dir = catalog.clone();
        config.template.import_root = Some(import_root);
        config.storage.images_dir = temp.path().join("images");
        config.storage.instances_dir = temp.path().join("instances");
        config.policy.dir = temp.path().join("policies");
        config.daemon.state_dir = temp.path().join("state");
        config.daemon.socket = temp.path().join("run/api.sock");
        let source_path = catalog.join("config.toml");
        let contents = toml::to_string(&config).expect("serialize config");
        std::fs::write(&source_path, &contents).expect("write source config");
        std::fs::set_permissions(&source_path, std::fs::Permissions::from_mode(0o640))
            .expect("source config mode");
        std::fs::write(replacement.join("config.toml"), "replacement = true\n")
            .expect("write replacement config");
        symlink(&catalog, &alias).expect("config alias");

        let loaded = load_daemon_config(&alias.join("config.toml")).expect("load source config");
        std::fs::remove_file(&alias).expect("remove old alias");
        symlink(&replacement, &alias).expect("retarget config alias");

        let error = run_loaded_config(loaded)
            .await
            .expect_err("the loaded catalog config object must remain protected");

        let message = error.to_string();
        assert!(message.contains("template.dir"));
        assert!(message.contains("config_path"));
        assert_eq!(
            std::fs::read_to_string(&source_path).expect("source config remains readable"),
            contents
        );
        assert_eq!(
            std::fs::metadata(&catalog)
                .expect("catalog metadata")
                .mode()
                & 0o777,
            0o750
        );
        assert_eq!(
            std::fs::metadata(&source_path)
                .expect("source config metadata")
                .mode()
                & 0o777,
            0o640
        );
        assert!(!config.storage.images_dir.exists());
        assert!(!config.daemon.state_dir.exists());
    }

    #[tokio::test]
    async fn loaded_import_config_stays_bound_after_alias_retarget() {
        let temp = tempfile::tempdir().expect("tempdir");
        let catalog = temp.path().join("catalog");
        let import_root = temp.path().join("imports");
        let replacement = temp.path().join("replacement");
        let alias = temp.path().join("config-alias");
        for directory in [&import_root, &replacement] {
            std::fs::create_dir(directory).expect("directory");
        }

        let mut config = DaemonConfig::default();
        config.template.dir = catalog.clone();
        config.template.import_root = Some(import_root.clone());
        config.storage.images_dir = temp.path().join("images");
        config.storage.instances_dir = temp.path().join("instances");
        config.policy.dir = temp.path().join("policies");
        config.daemon.state_dir = temp.path().join("state");
        config.daemon.socket = temp.path().join("run/api.sock");
        let source_path = import_root.join("config.toml");
        let contents = toml::to_string(&config).expect("serialize config");
        std::fs::write(&source_path, &contents).expect("write source config");
        std::fs::set_permissions(&source_path, std::fs::Permissions::from_mode(0o640))
            .expect("source config mode");
        std::fs::write(replacement.join("config.toml"), "replacement = true\n")
            .expect("write replacement config");
        symlink(&import_root, &alias).expect("config alias");

        let loaded = load_daemon_config(&alias.join("config.toml")).expect("load source config");
        std::fs::remove_file(&alias).expect("remove old alias");
        symlink(&replacement, &alias).expect("retarget config alias");

        let error = run_loaded_config(loaded)
            .await
            .expect_err("the loaded import config object must remain protected");

        let message = error.to_string();
        assert!(message.contains("template.import_root"));
        assert!(message.contains("config_path"));
        assert_eq!(
            std::fs::read_to_string(&source_path).expect("source config remains readable"),
            contents
        );
        assert_eq!(
            std::fs::metadata(&source_path)
                .expect("source config metadata")
                .mode()
                & 0o777,
            0o640
        );
        assert!(!catalog.exists());
        assert!(!config.storage.images_dir.exists());
        assert!(!config.daemon.state_dir.exists());
    }

    #[tokio::test]
    async fn replaced_config_ancestor_fails_before_catalog_mode_change() {
        let temp = tempfile::tempdir().expect("tempdir");
        let catalog = temp.path().join("catalog");
        let config_root = temp.path().join("config-root");
        let detached_root = temp.path().join("detached-config-root");
        let import_root = temp.path().join("imports");
        for directory in [&catalog, &config_root, &import_root] {
            std::fs::create_dir(directory).expect("directory");
        }
        std::fs::set_permissions(&catalog, std::fs::Permissions::from_mode(0o750))
            .expect("catalog mode");

        let mut config = DaemonConfig::default();
        config.template.dir = catalog.clone();
        config.template.import_root = Some(import_root);
        config.storage.images_dir = temp.path().join("images");
        config.storage.instances_dir = temp.path().join("instances");
        config.policy.dir = temp.path().join("policies");
        config.daemon.state_dir = temp.path().join("state");
        config.daemon.socket = temp.path().join("run/api.sock");
        let config_path = config_root.join("config.toml");
        let contents = toml::to_string(&config).expect("serialize config");
        std::fs::write(&config_path, &contents).expect("write source config");

        let loaded = load_daemon_config(&config_path).expect("load source config");
        std::fs::rename(&config_root, &detached_root).expect("detach loaded config ancestor");
        std::fs::create_dir(&config_root).expect("replacement config root");
        std::fs::write(config_root.join("config.toml"), "replacement = true\n")
            .expect("write replacement config");

        let error = run_loaded_config(loaded)
            .await
            .expect_err("replaced config ancestor must fail closed");

        assert!(
            error
                .to_string()
                .contains("changed while startup boundaries")
        );
        assert_eq!(
            std::fs::read_to_string(detached_root.join("config.toml"))
                .expect("loaded config remains readable"),
            contents
        );
        assert_eq!(
            std::fs::metadata(&catalog)
                .expect("catalog metadata")
                .mode()
                & 0o777,
            0o750
        );
        assert!(!config.storage.images_dir.exists());
        assert!(!config.daemon.state_dir.exists());
    }
}
