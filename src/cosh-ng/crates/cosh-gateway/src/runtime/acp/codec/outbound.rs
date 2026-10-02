impl AcpV1Codec {
    /// Creates one codec with an explicit client identity and frame bound.
    ///
    /// # Errors
    ///
    /// Rejects empty implementation metadata and a zero frame bound.
    pub fn new(config: AcpV1ClientConfig) -> Result<Self, AcpV1CodecError> {
        if config.max_frame_bytes == 0 || config.max_frame_bytes > MAX_ACP_FRAME_BYTES {
            return Err(AcpV1CodecError::InvalidFrameLimit {
                actual: config.max_frame_bytes,
                maximum: MAX_ACP_FRAME_BYTES,
            });
        }
        if config.name.trim().is_empty() {
            return Err(AcpV1CodecError::InvalidClientInfo { field: "name" });
        }
        if config.version.trim().is_empty() {
            return Err(AcpV1CodecError::InvalidClientInfo { field: "version" });
        }
        Ok(Self {
            config,
            phase: AcpV1ProtocolPhase::Created,
            next_request_sequence: 1,
            next_inbound_batch_sequence: 1,
            pending_outbound: BTreeMap::new(),
            pending_permissions: BTreeMap::new(),
            pending_unsupported: BTreeMap::new(),
            pending_inbound_batches: BTreeMap::new(),
            capabilities: None,
            session_id: None,
            prompt_request_id: None,
            cancellation_sent: false,
        })
    }

    /// Returns the current ACP protocol phase.
    #[must_use]
    pub fn phase(&self) -> AcpV1ProtocolPhase {
        self.phase
    }

    /// Returns the opaque session bound after `session/new` succeeds.
    #[must_use]
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    /// Encodes the mandatory ACP v1 initialize request.
    ///
    /// # Errors
    ///
    /// Rejects repeated initialization and frames above the configured bound.
    pub fn initialize_frame(&mut self) -> Result<String, AcpV1CodecError> {
        self.require_phase(AcpV1ProtocolPhase::Created, "initialize_frame")?;
        let mut request = InitializeRequest::new(ProtocolVersion::V1).client_info(
            Implementation::new(self.config.name.clone(), self.config.version.clone()),
        );
        if self.config.adapter_profile == AcpV1AdapterProfile::Codex162 {
            let meta = serde_json::json!({
                "jetbrains": {
                    "air": {
                        "version": 1,
                        "capabilities": ["sessionFailure"]
                    }
                }
            })
            .as_object()
            .cloned()
            .ok_or_else(|| AcpV1CodecError::Sdk("invalid static Codex capability".into()))?;
            request = request.client_capabilities(ClientCapabilities::new().meta(meta));
        }
        let (id, frame) = self.encode_request(ClientRequest::InitializeRequest(request))?;
        self.pending_outbound.insert(
            id,
            PendingOutboundRequest {
                kind: AcpV1RequestKind::Initialize,
                session_id: None,
            },
        );
        self.phase = AcpV1ProtocolPhase::AwaitingInitialize;
        Ok(frame)
    }

    /// Encodes `session/new` for one pinned workspace.
    ///
    /// # Errors
    ///
    /// Requires successful initialization, absolute roots, no existing
    /// session, and advertised additional-directory support when used.
    pub fn new_session_frame(
        &mut self,
        workspace: impl Into<PathBuf>,
        additional_directories: Vec<PathBuf>,
    ) -> Result<String, AcpV1CodecError> {
        self.require_phase(AcpV1ProtocolPhase::Ready, "new_session_frame")?;
        if self.session_id.is_some()
            || self
                .pending_outbound
                .values()
                .any(|pending| pending.kind == AcpV1RequestKind::NewSession)
        {
            return Err(AcpV1CodecError::SessionAlreadyBound);
        }
        let workspace = workspace.into();
        validate_absolute(&workspace)?;
        for directory in &additional_directories {
            validate_absolute(directory)?;
        }
        if !additional_directories.is_empty()
            && !self
                .capabilities
                .is_some_and(|capabilities| capabilities.additional_directories)
        {
            return Err(AcpV1CodecError::UnsupportedCapability(
                "session.additionalDirectories",
            ));
        }

        let request =
            NewSessionRequest::new(workspace).additional_directories(additional_directories);
        let (id, frame) = self.encode_request(ClientRequest::NewSessionRequest(request))?;
        self.pending_outbound.insert(
            id,
            PendingOutboundRequest {
                kind: AcpV1RequestKind::NewSession,
                session_id: None,
            },
        );
        Ok(frame)
    }

    /// Encodes one text-only prompt in the bound Agent session.
    ///
    /// # Errors
    ///
    /// Requires an open session, non-empty text, and no active prompt.
    pub fn prompt_frame(&mut self, text: impl Into<String>) -> Result<String, AcpV1CodecError> {
        self.require_phase(AcpV1ProtocolPhase::Ready, "prompt_frame")?;
        let session_id = self
            .session_id
            .clone()
            .ok_or(AcpV1CodecError::SessionNotOpen)?;
        if self.prompt_request_id.is_some() {
            return Err(AcpV1CodecError::PromptAlreadyActive);
        }
        let text = text.into();
        if text.trim().is_empty() {
            return Err(AcpV1CodecError::EmptyPrompt);
        }
        let request = PromptRequest::new(
            session_id.clone(),
            vec![ContentBlock::Text(TextContent::new(text))],
        );
        let (id, frame) = self.encode_request(ClientRequest::PromptRequest(request))?;
        self.pending_outbound.insert(
            id.clone(),
            PendingOutboundRequest {
                kind: AcpV1RequestKind::Prompt,
                session_id: Some(session_id),
            },
        );
        self.prompt_request_id = Some(id);
        self.cancellation_sent = false;
        Ok(frame)
    }

    /// Encodes `session/cancel` and cancels every pending permission callback.
    ///
    /// The first frame is the cancellation notification. Remaining frames are
    /// mandatory `cancelled` responses for outstanding permission requests.
    ///
    /// # Errors
    ///
    /// Requires an active prompt and rejects duplicate cancellation.
    pub fn cancel_frames(&mut self) -> Result<Vec<String>, AcpV1CodecError> {
        let mut candidate = self.clone();
        let frames = candidate.cancel_frames_inner()?;
        *self = candidate;
        Ok(frames)
    }

    fn cancel_frames_inner(&mut self) -> Result<Vec<String>, AcpV1CodecError> {
        self.require_phase(AcpV1ProtocolPhase::Ready, "cancel_frames")?;
        if self.prompt_request_id.is_none() {
            return Err(AcpV1CodecError::PromptNotActive);
        }
        if self.cancellation_sent {
            return Err(AcpV1CodecError::CancellationAlreadySent);
        }
        let session_id = self
            .session_id
            .clone()
            .ok_or(AcpV1CodecError::SessionNotOpen)?;
        let notification =
            ClientNotification::CancelNotification(CancelNotification::new(session_id));
        let mut frames = vec![self.encode_notification(notification)?];
        let permission_ids = self.pending_permissions.keys().cloned().collect::<Vec<_>>();
        for request_id in permission_ids {
            frames.extend(
                self.permission_response_frames(&request_id, AcpV1PermissionDecision::Cancelled)?,
            );
        }
        let unsupported_ids = self.pending_unsupported.keys().cloned().collect::<Vec<_>>();
        for request_id in unsupported_ids {
            let raw = RawJsonRpcMessage::response(
                to_sdk_request_id(&request_id),
                Err(AcpError::method_not_found()),
            );
            let destination = self
                .pending_unsupported
                .remove(&request_id)
                .ok_or_else(|| AcpV1CodecError::UnknownUnsupportedRequest(request_id.clone()))?;
            frames.extend(self.settle_inbound_response(destination, raw)?);
        }
        self.cancellation_sent = true;
        Ok(frames)
    }

    /// Encodes a response to one pending permission callback.
    ///
    /// # Errors
    ///
    /// Rejects unknown requests and selected option IDs not offered by the
    /// correlated Agent request.
    pub fn permission_response_frames(
        &mut self,
        request_id: &AcpV1RequestId,
        decision: AcpV1PermissionDecision,
    ) -> Result<Vec<String>, AcpV1CodecError> {
        let mut candidate = self.clone();
        let frames = candidate.permission_response_frames_inner(request_id, decision)?;
        *self = candidate;
        Ok(frames)
    }

    fn permission_response_frames_inner(
        &mut self,
        request_id: &AcpV1RequestId,
        decision: AcpV1PermissionDecision,
    ) -> Result<Vec<String>, AcpV1CodecError> {
        self.require_phase(AcpV1ProtocolPhase::Ready, "permission_response_frames")?;
        let pending = self
            .pending_permissions
            .get(request_id)
            .ok_or_else(|| AcpV1CodecError::UnknownPermissionRequest(request_id.clone()))?;
        let outcome = match decision {
            AcpV1PermissionDecision::Cancelled => RequestPermissionOutcome::Cancelled,
            AcpV1PermissionDecision::Selected { option_id } => {
                let Some(kind) = pending.option_ids.get(&option_id) else {
                    return Err(AcpV1CodecError::UnknownPermissionOption {
                        request_id: request_id.clone(),
                        option_id,
                    });
                };
                if !matches!(
                    kind,
                    AcpV1PermissionOptionKind::AllowOnce | AcpV1PermissionOptionKind::RejectOnce
                ) {
                    return Err(AcpV1CodecError::UnsupportedPermissionOption {
                        request_id: request_id.clone(),
                        option_id,
                    });
                }
                RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(option_id))
            }
        };
        let response = self.permission_outcome_message(request_id, outcome)?;
        let destination = self
            .pending_permissions
            .remove(request_id)
            .ok_or_else(|| AcpV1CodecError::UnknownPermissionRequest(request_id.clone()))?
            .response_destination;
        self.settle_inbound_response(destination, response)
    }

    /// Encodes a fail-closed method-not-found response for an unadvertised callback.
    ///
    /// # Errors
    ///
    /// Rejects request IDs that were not returned by
    /// [`AcpV1Observation::UnsupportedClientRequest`].
    pub fn reject_unsupported_request_frames(
        &mut self,
        request_id: &AcpV1RequestId,
    ) -> Result<Vec<String>, AcpV1CodecError> {
        let mut candidate = self.clone();
        let frames = candidate.reject_unsupported_request_frames_inner(request_id)?;
        *self = candidate;
        Ok(frames)
    }

    fn reject_unsupported_request_frames_inner(
        &mut self,
        request_id: &AcpV1RequestId,
    ) -> Result<Vec<String>, AcpV1CodecError> {
        self.require_phase(
            AcpV1ProtocolPhase::Ready,
            "reject_unsupported_request_frames",
        )?;
        let destination = self
            .pending_unsupported
            .remove(request_id)
            .ok_or_else(|| AcpV1CodecError::UnknownUnsupportedRequest(request_id.clone()))?;
        let raw = RawJsonRpcMessage::response(
            to_sdk_request_id(request_id),
            Err(AcpError::method_not_found()),
        );
        self.settle_inbound_response(destination, raw)
    }

}
