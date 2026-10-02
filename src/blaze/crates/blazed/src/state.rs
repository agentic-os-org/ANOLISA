// SPDX-License-Identifier: Apache-2.0
//! Daemon-wide shared state: configuration, policy engine, hook registry,
//! and the in-memory instance map. All API handlers
//! receive an [`Arc<ServerState>`] and acquire the relevant `Mutex<...>`
//! lock just long enough to read or mutate the piece they need — locks
//! are never held across `.await` boundaries.

use std::sync::{Arc, Mutex};

#[cfg(test)]
use std::collections::HashMap;

use blaze_core::backend::BackendKind;
use blaze_core::config::DaemonConfig;
use blaze_core::kernel::HookRegistry;
#[cfg(test)]
use blaze_core::lifecycle::SandboxInstance;
use blaze_core::policy::PolicyEngine;
use blaze_core::storage::StorageProvider;
#[cfg(test)]
use uuid::Uuid;

use crate::error::Result;
use crate::metrics::Metrics;
use crate::sandbox::template::TemplateCatalog;
#[cfg(test)]
use crate::sandbox::template::validate_template_roots;
use crate::sandbox::{SandboxManager, SandboxManagerInit};
use crate::spawner::SpawnerRegistry;
use crate::state_store::StateStore;

/// All daemon mutable state. Cloning is via `Arc` (see the `state.clone()`
/// idiom in `daemon.rs`); the struct itself is never `Clone`.
pub struct ServerState {
    pub config: Mutex<DaemonConfig>,
    pub policy: Mutex<PolicyEngine>,
    pub hook: Mutex<HookRegistry>,
    #[cfg(test)]
    pub instances: Arc<Mutex<HashMap<Uuid, SandboxInstance>>>,
    pub manager: Arc<SandboxManager>,
    /// The backend kind that `build_spawner` actually probed and selected.
    /// API handlers use this to constrain availability to the single active
    /// backend rather than reporting all configured binaries.
    pub active_backend: BackendKind,
    pub storage: Arc<dyn StorageProvider>,
    #[cfg(test)]
    pub state_store: StateStore,
    pub metrics: Arc<Metrics>,
}

impl ServerState {
    /// Build a server state after validating every owned lifecycle record.
    #[allow(clippy::too_many_arguments)]
    pub fn build_with_store(
        config: DaemonConfig,
        policy: PolicyEngine,
        hook: HookRegistry,
        spawners: SpawnerRegistry,
        active_backend: BackendKind,
        storage: Arc<dyn StorageProvider>,
        template_catalog: TemplateCatalog,
        state_store: StateStore,
    ) -> Result<Self> {
        Self::assemble(
            config,
            policy,
            hook,
            spawners,
            active_backend,
            storage,
            template_catalog,
            state_store,
        )
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        config: DaemonConfig,
        policy: PolicyEngine,
        hook: HookRegistry,
        spawners: SpawnerRegistry,
        active_backend: BackendKind,
        storage: Arc<dyn StorageProvider>,
    ) -> Result<Self> {
        let template_roots = validate_template_roots(
            &config.template,
            &config.storage.images_dir,
            &config.storage.instances_dir,
            &config.policy.dir,
            &config.backends,
            &config.daemon.state_dir,
            &config.daemon.socket,
            None,
        )?;
        let template_catalog = TemplateCatalog::open_validated(&config.template, template_roots)?;
        let state_store = StateStore::new(config.daemon.state_dir.clone());
        Self::assemble(
            config,
            policy,
            hook,
            spawners,
            active_backend,
            storage,
            template_catalog,
            state_store,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn assemble(
        config: DaemonConfig,
        policy: PolicyEngine,
        hook: HookRegistry,
        spawners: SpawnerRegistry,
        active_backend: BackendKind,
        storage: Arc<dyn StorageProvider>,
        template_catalog: TemplateCatalog,
        state_store: StateStore,
    ) -> Result<Self> {
        let instances = state_store.scan()?;
        let (manager, resources) = SandboxManager::new(SandboxManagerInit {
            instances,
            spawners,
            active_backend,
            storage: storage.clone(),
            state_store: state_store.clone(),
            rootfs_size: config.storage.rootfs_size,
            mem_size: config.storage.mem_size,
            template_catalog,
        });

        Ok(Self {
            config: Mutex::new(config),
            policy: Mutex::new(policy),
            hook: Mutex::new(hook),
            #[cfg(test)]
            instances: resources.instances,
            manager: Arc::new(manager),
            active_backend,
            storage,
            #[cfg(test)]
            state_store,
            metrics: resources.metrics,
        })
    }
}

#[cfg(test)]
mod tests {
    use blaze_core::lifecycle::{BackendOwnership, SandboxState};
    use blaze_core::policy::WorkloadClass;

    use crate::file_provider::FileStorageProvider;
    use crate::spawner::{MockSpawner, SpawnerRegistry};

    use super::*;

    fn build_with_state_root(
        temporary: &std::path::Path,
        state_root: std::path::PathBuf,
    ) -> Result<ServerState> {
        let images = temporary.join("images");
        let instances = temporary.join("instances");
        for directory in [&state_root, &images, &instances] {
            std::fs::create_dir_all(directory).expect("test directory");
        }

        let mut config = DaemonConfig::default();
        config.daemon.state_dir = state_root.clone();
        config.storage.images_dir = images.clone();
        config.storage.instances_dir = instances.clone();
        config.template.dir = temporary.join("templates");
        config.template.import_root = Some(temporary.join("template-imports"));
        std::fs::create_dir(
            config
                .template
                .import_root
                .as_ref()
                .expect("template import root"),
        )
        .expect("template import directory");
        let template_catalog = TemplateCatalog::open(&config.template).expect("template catalog");
        let mut spawners = SpawnerRegistry::new();
        spawners.insert(BackendKind::Mock, Arc::new(MockSpawner));
        let storage: Arc<dyn StorageProvider> =
            Arc::new(FileStorageProvider::with_images(images, instances));

        ServerState::build_with_store(
            config,
            PolicyEngine::new(),
            HookRegistry::new(),
            spawners,
            BackendKind::Mock,
            storage,
            template_catalog,
            StateStore::new(state_root),
        )
    }

    #[test]
    fn builder_uses_the_preopened_state_store_after_path_replacement() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let configured_root = temporary.path().join("state");
        let retained_root = temporary.path().join("retained-state");
        let images = temporary.path().join("images");
        let instances = temporary.path().join("instances");
        for directory in [&configured_root, &images, &instances] {
            std::fs::create_dir(directory).expect("test directory");
        }

        let mut config = DaemonConfig::default();
        config.daemon.state_dir = configured_root.clone();
        config.storage.images_dir = images.clone();
        config.storage.instances_dir = instances.clone();
        config.template.dir = temporary.path().join("templates");
        config.template.import_root = Some(temporary.path().join("template-imports"));
        std::fs::create_dir(
            config
                .template
                .import_root
                .as_ref()
                .expect("template import root"),
        )
        .expect("template import directory");
        let template_catalog = TemplateCatalog::open(&config.template).expect("template catalog");
        let existing = SandboxInstance::new(
            BackendKind::Mock,
            WorkloadClass::AgentTool,
            "sha256:existing".into(),
            "default".into(),
        );
        existing
            .persist(&configured_root)
            .expect("persist existing fixture");
        let state_store = StateStore::new(configured_root.clone());

        std::fs::rename(&configured_root, &retained_root).expect("move opened state root");
        std::fs::create_dir(&configured_root).expect("replacement state root");

        let mut spawners = SpawnerRegistry::new();
        spawners.insert(BackendKind::Mock, Arc::new(MockSpawner));
        let storage: Arc<dyn StorageProvider> =
            Arc::new(FileStorageProvider::with_images(images, instances));
        let state = ServerState::build_with_store(
            config,
            PolicyEngine::new(),
            HookRegistry::new(),
            spawners,
            BackendKind::Mock,
            storage,
            template_catalog,
            state_store,
        )
        .expect("server state");

        assert!(
            state
                .instances
                .lock()
                .expect("instances")
                .contains_key(&existing.id)
        );
        let new_instance = SandboxInstance::new(
            BackendKind::Mock,
            WorkloadClass::AgentTool,
            "sha256:new".into(),
            "default".into(),
        );
        state
            .state_store
            .persist(&new_instance)
            .expect("persist through retained store");

        assert!(
            retained_root
                .join(new_instance.id.to_string())
                .join("state.json")
                .is_file()
        );
        assert!(!configured_root.join(new_instance.id.to_string()).exists());
    }

    #[test]
    fn builder_propagates_an_invalid_owned_inventory() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let state_root = temporary.path().join("state");
        let corrupt_id = Uuid::new_v4();
        let corrupt_owner = state_root.join(corrupt_id.to_string());
        std::fs::create_dir_all(&corrupt_owner).expect("corrupt owner directory");
        std::fs::write(corrupt_owner.join("state.json"), b"{not-json").expect("corrupt state");

        let result = build_with_state_root(temporary.path(), state_root);

        assert!(matches!(
            result,
            Err(crate::error::BlazeDaemonError::RecoveryRequired(message))
                if message.contains(&corrupt_id.to_string())
                    && message.contains("cannot load persisted instance")
        ));
    }

    #[test]
    fn builder_propagates_a_terminal_cleanup_violation() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let state_root = temporary.path().join("state");
        let mut stored = SandboxInstance::new(
            BackendKind::Mock,
            WorkloadClass::AgentTool,
            "sha256:terminal".into(),
            "default".into(),
        );
        stored
            .transition(SandboxState::Destroyed)
            .expect("terminal transition");
        stored.backend_ownership = BackendOwnership::Running;
        stored.persist(&state_root).expect("persist state");

        let result = build_with_state_root(temporary.path(), state_root);

        assert!(matches!(
            result,
            Err(crate::error::BlazeDaemonError::RecoveryRequired(message))
                if message.contains(&stored.id.to_string())
                    && message.contains("does not prove completed cleanup")
                    && message.contains("backend ownership Running")
        ));
    }
}
