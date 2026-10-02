/// Bound per-user local Gateway server.
pub struct GatewayDaemon {
    listener: UnixListener,
    coordinator: TaskCoordinator,
    socket_path: PathBuf,
    socket_identity: (u64, u64),
    owner_uid: u32,
    launch_catalog: TaskLaunchCatalog,
    database_path: PathBuf,
    scheduler: Option<TaskScheduler<Box<dyn RuntimeFactory>>>,
    runtime_containment: Option<VerifiedRuntimeContainment>,
    task_snapshot_driver: Option<Box<dyn TaskSnapshotDriver>>,
}

impl GatewayDaemon {
    /// Validates private paths, opens state, and binds the local socket.
    ///
    /// # Errors
    ///
    /// Returns a fail-closed path, storage, socket, or already-running error.
    pub fn bind(config: GatewayDaemonConfig) -> Result<Self, GatewayDaemonError> {
        let owner_uid = Uid::effective().as_raw();
        prepare_socket_path(&config.socket_path, owner_uid)?;
        let database_path = config.database_path.clone();
        let coordinator = TaskCoordinator::open_for_launch_catalog(
            &database_path,
            config.installation_id,
            config.launch_catalog.clone(),
        )?;
        let listener = UnixListener::bind(&config.socket_path)?;
        fs::set_permissions(&config.socket_path, fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        let metadata = fs::symlink_metadata(&config.socket_path)?;
        Ok(Self {
            listener,
            coordinator,
            socket_path: config.socket_path,
            socket_identity: (metadata.dev(), metadata.ino()),
            owner_uid,
            launch_catalog: config.launch_catalog,
            database_path,
            scheduler: None,
            runtime_containment: None,
            task_snapshot_driver: None,
        })
    }

    /// Returns the durable installation identity bound to this daemon.
    #[must_use]
    pub fn installation_id(&self) -> &InstallationId {
        &self.coordinator.installation_id
    }

    /// Serves one request per connection until the shutdown flag is set.
    ///
    /// # Errors
    ///
    /// Returns listener failures. Per-connection protocol and authorization
    /// errors are returned to that client without stopping admission.
    pub fn serve_until(&mut self, shutdown: &AtomicBool) -> Result<(), GatewayDaemonError> {
        while !shutdown.load(Ordering::Relaxed) {
            if let Some(scheduler) = self.scheduler.as_mut() {
                if let Err(error) = scheduler.tick(now_ms()?) {
                    let _ = scheduler.shutdown(now_ms()?);
                    return Err(error);
                }
            }
            match self.listener.accept() {
                Ok((stream, _)) => {
                    let _ = self.handle_connection(stream);
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    std::thread::sleep(ACCEPT_POLL_INTERVAL);
                }
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error) => return Err(error.into()),
            }
        }
        if let Some(scheduler) = self.scheduler.as_mut() {
            scheduler.shutdown(now_ms()?)?;
        }
        Ok(())
    }

    fn handle_connection(&mut self, mut stream: UnixStream) -> Result<(), GatewayDaemonError> {
        stream.set_read_timeout(Some(CONNECTION_ADMISSION_QUANTUM))?;
        stream.set_write_timeout(Some(CONNECTION_ADMISSION_QUANTUM))?;
        let peer_uid = peer_uid(&stream)?;
        if peer_uid != self.owner_uid {
            return Err(GatewayDaemonError::Unauthorized);
        }
        let actor = actor_ref_for_uid(&self.coordinator.installation_id, peer_uid)?;
        let request = match read_frame::<GatewayRequest>(&mut stream) {
            Ok(request) => request,
            Err(error) => {
                let response = error_response(None, &error);
                let _ = write_frame(&mut stream, &response);
                return Err(error);
            }
        };
        let request_id = request.request_id().clone();
        let result = self.dispatch(&actor, request);
        let response = match result {
            Ok(result) => GatewayResponse {
                api_version: GATEWAY_API_VERSION.to_owned(),
                request_id: Some(request_id),
                outcome: GatewayResponseOutcome::Ok {
                    result: Box::new(result),
                },
            },
            Err(error) => error_response(Some(request_id), &error),
        };
        write_frame(&mut stream, &response)
    }

    fn dispatch(
        &mut self,
        actor: &ActorRef,
        request: GatewayRequest,
    ) -> Result<GatewayResult, GatewayDaemonError> {
        let admission = TaskAdmission {
            catalog: &self.launch_catalog,
        };
        let mut ports = DaemonTaskPorts {
            coordinator: &mut self.coordinator,
            scheduler: &mut self.scheduler,
            task_snapshot_driver: &mut self.task_snapshot_driver,
        };
        handler::dispatch(actor, request, admission, &mut ports)
    }
}

#[cfg(test)]
fn supported_daemon_runtime(
    profile: GatewayCapabilityProfile,
    runtime: &RuntimeSelector,
) -> bool {
    runtime_matches_capability_profile(profile, runtime)
}

impl Drop for GatewayDaemon {
    fn drop(&mut self) {
        if let Ok(metadata) = fs::symlink_metadata(&self.socket_path) {
            if (metadata.dev(), metadata.ino()) == self.socket_identity
                && metadata.file_type().is_socket()
            {
                let _ = fs::remove_file(&self.socket_path);
            }
        }
    }
}
