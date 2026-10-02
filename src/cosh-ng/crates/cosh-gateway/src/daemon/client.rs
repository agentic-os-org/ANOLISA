/// Thin local client that carries no identity or execution authority.
#[derive(Debug, Clone)]
pub struct LocalGatewayClient {
    socket_path: PathBuf,
}

impl LocalGatewayClient {
    /// Creates a client for one absolute local socket path.
    #[must_use]
    pub fn new(socket_path: PathBuf) -> Self {
        Self { socket_path }
    }

    /// Verifies the daemon transport and authentication path.
    pub fn ping(&self, request_id: RequestId) -> Result<GatewayResult, GatewayDaemonError> {
        self.request(GatewayRequest::Ping {
            api_version: GATEWAY_API_VERSION.to_owned(),
            request_id,
        })
    }

    /// Reads safe Runtime, workspace, checkpoint, and authority capabilities.
    pub fn capabilities(
        &self,
        request_id: RequestId,
    ) -> Result<GatewayResult, GatewayDaemonError> {
        self.request(GatewayRequest::Capabilities {
            api_version: GATEWAY_API_VERSION.to_owned(),
            request_id,
        })
    }

    /// Creates and queues one catalog-selected durable Task.
    pub fn submit_launch(
        &self,
        request: SubmitLaunch,
    ) -> Result<GatewayResult, GatewayDaemonError> {
        self.request(GatewayRequest::SubmitLaunch {
            api_version: GATEWAY_API_VERSION.to_owned(),
            request,
        })
    }

    /// Creates and queues one durable Task.
    pub fn submit(&self, request: SubmitTask) -> Result<GatewayResult, GatewayDaemonError> {
        self.request(GatewayRequest::Submit {
            api_version: GATEWAY_API_VERSION.to_owned(),
            request,
        })
    }

    /// Reads one authorized Task projection.
    pub fn get(
        &self,
        request_id: RequestId,
        task_id: TaskId,
    ) -> Result<GatewayResult, GatewayDaemonError> {
        self.request(GatewayRequest::Get {
            api_version: GATEWAY_API_VERSION.to_owned(),
            request_id,
            task_id,
        })
    }

    /// Reads a bounded newest-first list owned by the authenticated actor.
    pub fn list(
        &self,
        request_id: RequestId,
        limit: u16,
    ) -> Result<GatewayResult, GatewayDaemonError> {
        self.request(GatewayRequest::List {
            api_version: GATEWAY_API_VERSION.to_owned(),
            request_id,
            limit,
        })
    }

    /// Reads a bounded authorized event page.
    pub fn events(
        &self,
        request_id: RequestId,
        task_id: TaskId,
        after_revision: Option<u64>,
        limit: u16,
    ) -> Result<GatewayResult, GatewayDaemonError> {
        self.request(GatewayRequest::Events {
            api_version: GATEWAY_API_VERSION.to_owned(),
            request_id,
            task_id,
            after_revision,
            limit,
        })
    }

    /// Lists proven-created checkpoints owned by one authorized Task.
    pub fn task_snapshots(
        &self,
        request_id: RequestId,
        task_id: TaskId,
    ) -> Result<GatewayResult, GatewayDaemonError> {
        self.request(GatewayRequest::ListTaskSnapshots {
            api_version: GATEWAY_API_VERSION.to_owned(),
            request_id,
            task_id,
        })
    }

    /// Previews one exact Task-owned checkpoint against the live workspace.
    pub fn preview_task_snapshot(
        &self,
        request_id: RequestId,
        request: InspectTaskSnapshot,
    ) -> Result<GatewayResult, GatewayDaemonError> {
        self.request(GatewayRequest::PreviewTaskSnapshot {
            api_version: GATEWAY_API_VERSION.to_owned(),
            request_id,
            request,
        })
    }

    /// Returns the same bounded change projection used by switch confirmation.
    pub fn diff_task_snapshot(
        &self,
        request_id: RequestId,
        request: InspectTaskSnapshot,
    ) -> Result<GatewayResult, GatewayDaemonError> {
        self.request(GatewayRequest::DiffTaskSnapshot {
            api_version: GATEWAY_API_VERSION.to_owned(),
            request_id,
            request,
        })
    }

    /// Switches to one exact Task-owned checkpoint after recovery creation.
    pub fn switch_task_snapshot(
        &self,
        request: SwitchTaskSnapshot,
    ) -> Result<GatewayResult, GatewayDaemonError> {
        self.request(GatewayRequest::SwitchTaskSnapshot {
            api_version: GATEWAY_API_VERSION.to_owned(),
            request,
        })
    }

    /// Persists cancellation of one active Task Run.
    pub fn cancel(&self, request: CancelTask) -> Result<GatewayResult, GatewayDaemonError> {
        self.request(GatewayRequest::Cancel {
            api_version: GATEWAY_API_VERSION.to_owned(),
            request,
        })
    }

    /// Queues one replacement Run from an exact suspended attempt.
    pub fn retry(&self, request: RetryTask) -> Result<GatewayResult, GatewayDaemonError> {
        self.request(GatewayRequest::Retry {
            api_version: GATEWAY_API_VERSION.to_owned(),
            request,
        })
    }

    /// Persists and dispatches one exact pending Runtime input response.
    pub fn append_input(
        &self,
        request: AppendTaskInput,
    ) -> Result<GatewayResult, GatewayDaemonError> {
        self.request(GatewayRequest::AppendInput {
            api_version: GATEWAY_API_VERSION.to_owned(),
            request,
        })
    }

    /// Persists and dispatches one provider-native approval decision.
    pub fn resolve_approval(
        &self,
        request: ResolveApproval,
    ) -> Result<GatewayResult, GatewayDaemonError> {
        self.request(GatewayRequest::ResolveApproval {
            api_version: GATEWAY_API_VERSION.to_owned(),
            request,
        })
    }

    /// Resolves an approval only for its explicitly bound Task.
    pub fn resolve_approval_for_task(
        &self,
        request: ResolveApprovalForTask,
    ) -> Result<GatewayResult, GatewayDaemonError> {
        self.request(GatewayRequest::ResolveApprovalForTask {
            api_version: GATEWAY_API_VERSION.to_owned(),
            request,
        })
    }

    fn request(&self, request: GatewayRequest) -> Result<GatewayResult, GatewayDaemonError> {
        if !self.socket_path.is_absolute() {
            return Err(unsafe_path(
                &self.socket_path,
                "socket path must be absolute",
            ));
        }
        let expected_request_id = request.request_id().clone();
        let mut stream = UnixStream::connect(&self.socket_path)?;
        if peer_uid(&stream)? != Uid::effective().as_raw() {
            return Err(GatewayDaemonError::Unauthorized);
        }
        stream.set_read_timeout(Some(CLIENT_REQUEST_DEADLINE))?;
        stream.set_write_timeout(Some(CLIENT_REQUEST_DEADLINE))?;
        write_frame(&mut stream, &request)?;
        let response = read_frame::<GatewayResponse>(&mut stream)?;
        if response.api_version != GATEWAY_API_VERSION
            || response.request_id.as_ref() != Some(&expected_request_id)
        {
            return Err(GatewayDaemonError::Protocol(
                "response correlation or API version mismatch".to_owned(),
            ));
        }
        match response.outcome {
            GatewayResponseOutcome::Ok { result } => Ok(*result),
            GatewayResponseOutcome::Error { error } => Err(GatewayDaemonError::Remote {
                code: error.code,
                message: error.message,
                recoverable: error.recoverable,
            }),
        }
    }
}
