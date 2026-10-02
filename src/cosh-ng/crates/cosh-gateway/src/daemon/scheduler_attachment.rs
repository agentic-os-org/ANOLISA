//! Production scheduler attachment for the task-only Gateway runtime.

use super::*;

impl GatewayDaemon {
    /// Installs the Task-scoped snapshot inspection and switch driver.
    ///
    /// # Errors
    ///
    /// Returns when a driver is already installed.
    pub fn attach_task_snapshot_driver(
        &mut self,
        driver: Box<dyn TaskSnapshotDriver>,
    ) -> Result<(), GatewayDaemonError> {
        if self.task_snapshot_driver.is_some() {
            return Err(GatewayDaemonError::Protocol(
                "Gateway Task snapshot driver is already attached".to_owned(),
            ));
        }
        self.task_snapshot_driver = Some(driver);
        Ok(())
    }

    /// Installs the provider-neutral pre-Runtime checkpoint driver.
    ///
    /// # Errors
    ///
    /// Returns when no scheduler is attached or a driver is already installed.
    pub fn attach_pre_runtime_checkpoint_driver(
        &mut self,
        driver: Box<dyn PreRuntimeCheckpointDriver>,
    ) -> Result<(), GatewayDaemonError> {
        let scheduler = self.scheduler.as_mut().ok_or_else(|| {
            GatewayDaemonError::Protocol(
                "Gateway scheduler must be attached before its checkpoint driver".to_owned(),
            )
        })?;
        scheduler.install_pre_runtime_checkpoint_driver(driver)
    }

    /// Enables scheduling with an explicitly injected generic brokered driver.
    ///
    /// Production callers may use this boundary for a fail-closed task-only
    /// driver. The attachment itself does not construct or select a concrete
    /// execution target; checkpoint providers are intentionally absent from
    /// this crate's production wiring.
    ///
    /// # Errors
    ///
    /// Returns when a scheduler is already attached or durable state cannot be
    /// opened under the same installation identity.
    pub fn attach_brokered_scheduler(
        &mut self,
        containment: VerifiedRuntimeContainment,
        worker_id: BoundedOpaque,
        factory: Box<dyn RuntimeFactory>,
        driver: Box<dyn BrokeredExecutionDriver>,
    ) -> Result<(), GatewayDaemonError> {
        if self.scheduler.is_some() {
            return Err(GatewayDaemonError::Protocol(
                "Gateway scheduler is already attached".to_owned(),
            ));
        }
        self.scheduler = Some(
            TaskScheduler::open_for_launch_catalog(
                &self.database_path,
                Some(self.coordinator.installation_id.clone()),
                worker_id,
                self.launch_catalog.clone(),
                factory,
            )?
            .with_brokered_execution_driver(driver),
        );
        self.runtime_containment = Some(containment);
        Ok(())
    }

    /// Enables task-only scheduling with the default rejecting brokered driver.
    ///
    /// Generic Capability, Approval, Permit, and Execution contracts remain
    /// available to the scheduler, but this production attachment does not
    /// install a target provider. Brokered Runtime requests therefore fail
    /// closed before any external side effect can be attempted.
    ///
    /// # Errors
    ///
    /// Returns when a scheduler is already attached or durable state cannot be
    /// opened under the same installation identity.
    pub fn attach_task_only_scheduler(
        &mut self,
        containment: VerifiedRuntimeContainment,
        worker_id: BoundedOpaque,
        factory: Box<dyn RuntimeFactory>,
    ) -> Result<(), GatewayDaemonError> {
        if self.scheduler.is_some() {
            return Err(GatewayDaemonError::Protocol(
                "Gateway scheduler is already attached".to_owned(),
            ));
        }
        self.scheduler = Some(TaskScheduler::open_for_launch_catalog(
            &self.database_path,
            Some(self.coordinator.installation_id.clone()),
            worker_id,
            self.launch_catalog.clone(),
            factory,
        )?);
        self.runtime_containment = Some(containment);
        Ok(())
    }
}
