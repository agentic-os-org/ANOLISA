impl<F: RuntimeFactory> TaskScheduler<F> {
    /// Opens the durable Task database with an injected Runtime factory.
    ///
    /// # Errors
    ///
    /// Returns a storage or installation-identity error when durable state
    /// cannot be opened safely.
    pub fn open(
        database_path: impl AsRef<Path>,
        requested_installation_id: Option<InstallationId>,
        worker_id: BoundedOpaque,
        factory: F,
    ) -> Result<Self, GatewayDaemonError> {
        Self::open_for_capability_profile(
            database_path,
            requested_installation_id,
            worker_id,
            GatewayCapabilityProfile::task_only_v1(),
            factory,
        )
    }

    /// Opens durable scheduling bound to one trusted capability profile.
    pub(crate) fn open_for_capability_profile(
        database_path: impl AsRef<Path>,
        requested_installation_id: Option<InstallationId>,
        worker_id: BoundedOpaque,
        expected_profile: GatewayCapabilityProfile,
        factory: F,
    ) -> Result<Self, GatewayDaemonError> {
        let workspace = WorkspaceRef {
            scope_digest: sha256_digest(b"cosh.gateway.test.workspace.v1"),
            display_name: None,
        };
        Self::open_with_catalog_and_config(
            database_path,
            requested_installation_id,
            worker_id,
            TaskLaunchCatalog::for_legacy_profile(workspace, expected_profile),
            factory,
            TaskSchedulerConfig::default(),
        )
    }

    pub(crate) fn open_for_launch_catalog(
        database_path: impl AsRef<Path>,
        requested_installation_id: Option<InstallationId>,
        worker_id: BoundedOpaque,
        launch_catalog: TaskLaunchCatalog,
        factory: F,
    ) -> Result<Self, GatewayDaemonError> {
        Self::open_with_catalog_and_config(
            database_path,
            requested_installation_id,
            worker_id,
            launch_catalog,
            factory,
            TaskSchedulerConfig::default(),
        )
    }

    /// Opens durable state with explicit, validated lease timing bounds.
    pub fn open_with_config(
        database_path: impl AsRef<Path>,
        requested_installation_id: Option<InstallationId>,
        worker_id: BoundedOpaque,
        factory: F,
        config: TaskSchedulerConfig,
    ) -> Result<Self, GatewayDaemonError> {
        let workspace = WorkspaceRef {
            scope_digest: sha256_digest(b"cosh.gateway.test.workspace.v1"),
            display_name: None,
        };
        Self::open_with_catalog_and_config(
            database_path,
            requested_installation_id,
            worker_id,
            TaskLaunchCatalog::for_legacy_profile(
                workspace,
                GatewayCapabilityProfile::task_only_v1(),
            ),
            factory,
            config,
        )
    }

    fn open_with_catalog_and_config(
        database_path: impl AsRef<Path>,
        requested_installation_id: Option<InstallationId>,
        worker_id: BoundedOpaque,
        launch_catalog: TaskLaunchCatalog,
        factory: F,
        config: TaskSchedulerConfig,
    ) -> Result<Self, GatewayDaemonError> {
        Ok(Self {
            coordinator: TaskCoordinator::open_for_launch_catalog(
                database_path,
                requested_installation_id,
                launch_catalog,
            )?,
            worker_id,
            config: config.validate()?,
            factory,
            brokered_driver: Box::new(RejectingBrokeredExecutionDriver),
            checkpoint_driver: None,
            active: None,
            shutting_down: false,
            #[cfg(test)]
            fail_next_brokered_result_completion: false,
            #[cfg(test)]
            fail_next_terminal_lease_release: false,
            #[cfg(test)]
            fail_next_input_dispatch_completion: false,
            #[cfg(test)]
            fail_next_input_request_install: false,
            #[cfg(test)]
            fail_next_input_unknown_cleanup: false,
        })
    }

    /// Installs the trusted brokered policy and execution boundary.
    ///
    /// The default driver rejects every brokered request, so callers must
    /// explicitly install a production driver before selecting that profile.
    pub fn with_brokered_execution_driver(
        mut self,
        driver: Box<dyn BrokeredExecutionDriver>,
    ) -> Self {
        self.brokered_driver = driver;
        self
    }

    /// Installs the provider-neutral pre-Runtime checkpoint boundary.
    #[must_use]
    pub fn with_pre_runtime_checkpoint_driver(
        mut self,
        driver: Box<dyn PreRuntimeCheckpointDriver>,
    ) -> Self {
        self.checkpoint_driver = Some(driver);
        self
    }

    pub(super) fn install_pre_runtime_checkpoint_driver(
        &mut self,
        driver: Box<dyn PreRuntimeCheckpointDriver>,
    ) -> Result<(), GatewayDaemonError> {
        if self.checkpoint_driver.is_some() {
            return Err(GatewayDaemonError::Protocol(
                "pre-Runtime checkpoint driver is already attached".to_owned(),
            ));
        }
        self.checkpoint_driver = Some(driver);
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn fail_next_terminal_lease_release_for_test(&mut self) {
        self.fail_next_terminal_lease_release = true;
    }

    #[cfg(test)]
    fn fail_next_input_dispatch_completion_for_test(&mut self) {
        self.fail_next_input_dispatch_completion = true;
    }

    #[cfg(test)]
    fn fail_next_input_request_install_for_test(&mut self) {
        self.fail_next_input_request_install = true;
    }

    #[cfg(test)]
    fn fail_next_input_unknown_cleanup_for_test(&mut self) {
        self.fail_next_input_unknown_cleanup = true;
    }

}
