impl AcpV1Codec {
    /// Decodes and validates one bounded ACP v1 JSON-RPC line.
    ///
    /// # Errors
    ///
    /// Rejects malformed frames, wrong ordering, identity mismatches, unknown
    /// responses, and invalid callback correlations. Any error makes the codec
    /// terminal so the supervising bridge can fail closed.
    pub fn decode_frame(&mut self, frame: &[u8]) -> Result<AcpV1Observation, AcpV1CodecError> {
        let decoded = self.decode_transport_frame(frame)?;
        if !decoded.outbound_frames.is_empty() || decoded.observations.len() != 1 {
            self.phase = AcpV1ProtocolPhase::Terminal;
            return Err(AcpV1CodecError::MultiMessageFrameRequiresBridge);
        }
        decoded
            .observations
            .into_iter()
            .next()
            .ok_or(AcpV1CodecError::MultiMessageFrameRequiresBridge)
    }

    /// Decodes one single-message or batch-aware ACP transport frame.
    ///
    /// Valid batch entries are processed independently in source order. JSON-RPC
    /// errors for malformed call-shaped entries are returned as outbound frames;
    /// malformed response-shaped entries are ignored as required by JSON-RPC.
    ///
    /// # Errors
    ///
    /// Rejects frame bounds, protocol ordering, identity, or correlation
    /// violations. Any error makes the codec terminal.
    pub(crate) fn decode_transport_frame(
        &mut self,
        frame: &[u8],
    ) -> Result<AcpV1DecodedFrame, AcpV1CodecError> {
        if self.phase == AcpV1ProtocolPhase::Terminal {
            return Err(self.invalid_phase("decode_transport_frame"));
        }
        let mut candidate = self.clone();
        match candidate.decode_transport_frame_inner(frame) {
            Ok(decoded) => {
                *self = candidate;
                Ok(decoded)
            }
            Err(error) => {
                // Never retain a successfully decoded batch prefix after a
                // later entry invalidates the entire protocol generation.
                self.phase = AcpV1ProtocolPhase::Terminal;
                Err(error)
            }
        }
    }

    /// Produces one terminal observation when runtime stdout closes.
    #[must_use]
    pub fn finish_stdout(&mut self) -> Option<AcpV1Observation> {
        if self.phase == AcpV1ProtocolPhase::Terminal {
            return None;
        }
        self.phase = AcpV1ProtocolPhase::Terminal;
        Some(AcpV1Observation::TransportClosed)
    }

    fn decode_transport_frame_inner(
        &mut self,
        frame: &[u8],
    ) -> Result<AcpV1DecodedFrame, AcpV1CodecError> {
        if frame.len() > self.config.max_frame_bytes {
            return Err(AcpV1CodecError::FrameTooLarge {
                limit: self.config.max_frame_bytes,
            });
        }
        let frame = std::str::from_utf8(frame).map_err(|_| AcpV1CodecError::InvalidUtf8)?;
        let frame = frame.trim_end_matches(['\r', '\n']);
        if frame.is_empty() {
            return Err(AcpV1CodecError::EmptyFrame);
        }
        if self.phase == AcpV1ProtocolPhase::Created {
            return Err(self.invalid_phase("decode_transport_frame"));
        }

        match TransportFrame::parse_json(frame) {
            TransportFrame::Single(message) => Ok(AcpV1DecodedFrame {
                observations: vec![
                    self.decode_message(message, InboundResponseDestination::Individual)?
                ],
                outbound_frames: Vec::new(),
            }),
            TransportFrame::Malformed { raw, error }
                if serde_json::from_str::<serde_json::Value>(&raw)
                    .is_ok_and(|value| value.as_array().is_some_and(Vec::is_empty)) =>
            {
                let response = RawJsonRpcMessage::response(RequestId::Null, Err(error));
                Ok(AcpV1DecodedFrame {
                    observations: Vec::new(),
                    outbound_frames: vec![self.encode_raw(&response)?],
                })
            }
            TransportFrame::Malformed { error, .. } => {
                // stdout is a dedicated supervised ACP stream, not a general
                // JSON-RPC server endpoint. Recovery would let log pollution
                // or a corrupted frame blur the process/protocol boundary.
                self.phase = AcpV1ProtocolPhase::Terminal;
                Err(AcpV1CodecError::Sdk(error.to_string()))
            }
            TransportFrame::Batch(batch) => self.decode_batch(batch),
        }
    }

    fn decode_message(
        &mut self,
        message: RawJsonRpcMessage,
        response_destination: InboundResponseDestination,
    ) -> Result<AcpV1Observation, AcpV1CodecError> {
        match message {
            RawJsonRpcMessage::Response(response) => self.decode_response(response),
            RawJsonRpcMessage::Notification(notification) => {
                self.require_phase(AcpV1ProtocolPhase::Ready, "decode_notification")?;
                self.decode_notification(notification.method.as_ref(), notification.params)
            }
            RawJsonRpcMessage::Request(request) => {
                self.require_phase(AcpV1ProtocolPhase::Ready, "decode_request")?;
                self.decode_request(
                    request.id,
                    request.method.as_ref(),
                    request.params,
                    response_destination,
                )
            }
        }
    }

    fn decode_batch(
        &mut self,
        batch: TransportBatch,
    ) -> Result<AcpV1DecodedFrame, AcpV1CodecError> {
        if batch.len() > MAX_ACP_BATCH_ENTRIES {
            return Err(AcpV1CodecError::BatchTooLarge {
                limit: MAX_ACP_BATCH_ENTRIES,
            });
        }

        let response_count = batch
            .entries()
            .filter(|entry| batch_entry_requires_response(entry))
            .count();
        let batch_id = if response_count == 0 {
            None
        } else {
            let batch_id = self.next_inbound_batch_sequence;
            self.next_inbound_batch_sequence = batch_id
                .checked_add(1)
                .ok_or(AcpV1CodecError::BatchIdExhausted)?;
            self.pending_inbound_batches.insert(
                batch_id,
                PendingInboundBatch {
                    responses: vec![None; response_count],
                },
            );
            Some(batch_id)
        };

        let mut next_response_slot = 0;
        let mut observations = Vec::with_capacity(batch.len());
        let mut outbound_frames = Vec::new();
        for entry in batch.into_entries() {
            let requires_response = batch_entry_requires_response(&entry);
            let response_destination = match (batch_id, requires_response) {
                (Some(batch_id), true) => {
                    let destination = InboundResponseDestination::Batch {
                        batch_id,
                        slot: next_response_slot,
                    };
                    next_response_slot += 1;
                    destination
                }
                _ => InboundResponseDestination::Individual,
            };
            match entry {
                TransportBatchEntry::Message(message) => {
                    observations.push(self.decode_message(message, response_destination)?);
                }
                TransportBatchEntry::Malformed { raw: _, error } => {
                    if requires_response {
                        let response = RawJsonRpcMessage::response(RequestId::Null, Err(error));
                        outbound_frames
                            .extend(self.settle_inbound_response(response_destination, response)?);
                    }
                }
            }
        }
        if let Some(batch_id) = batch_id {
            outbound_frames.extend(self.take_completed_batch(batch_id)?);
        }
        Ok(AcpV1DecodedFrame {
            observations,
            outbound_frames,
        })
    }

    fn decode_response(
        &mut self,
        response: Response<serde_json::Value>,
    ) -> Result<AcpV1Observation, AcpV1CodecError> {
        match response {
            Response::Result { id, result } => {
                let request_id = from_sdk_request_id(id)?;
                let pending = self
                    .pending_outbound
                    .remove(&request_id)
                    .ok_or_else(|| AcpV1CodecError::UnknownResponse(request_id.clone()))?;
                self.decode_success(pending, result)
            }
            Response::Error { id, error } => {
                let request_id = from_sdk_request_id(id)?;
                let pending = self
                    .pending_outbound
                    .remove(&request_id)
                    .ok_or_else(|| AcpV1CodecError::UnknownResponse(request_id.clone()))?;
                if pending.kind == AcpV1RequestKind::Prompt {
                    self.prompt_request_id = None;
                    self.cancellation_sent = false;
                }
                if pending.kind == AcpV1RequestKind::Initialize {
                    self.phase = AcpV1ProtocolPhase::Terminal;
                }
                Ok(AcpV1Observation::RequestFailed {
                    request: pending.kind,
                    code: i32::from(error.code),
                    message: error.message,
                })
            }
        }
    }

    fn decode_success(
        &mut self,
        pending: PendingOutboundRequest,
        result: serde_json::Value,
    ) -> Result<AcpV1Observation, AcpV1CodecError> {
        match pending.kind {
            AcpV1RequestKind::Initialize => self.decode_initialize_response(result),
            AcpV1RequestKind::NewSession => self.decode_new_session_response(result),
            AcpV1RequestKind::Prompt => self.decode_prompt_response(pending, result),
        }
    }

    fn decode_initialize_response(
        &mut self,
        result: serde_json::Value,
    ) -> Result<AcpV1Observation, AcpV1CodecError> {
        let response: InitializeResponse = serde_json::from_value(result.clone())?;
        if response.protocol_version != ProtocolVersion::V1 {
            return Err(AcpV1CodecError::UnsupportedProtocolVersion {
                actual: response.protocol_version.as_u16(),
            });
        }
        if self.config.adapter_profile == AcpV1AdapterProfile::Codex162 {
            validate_codex_initialize(&response, &result)?;
        }
        let capabilities = copy_capabilities(&response.agent_capabilities);
        let agent_info = response.agent_info.map(|info| AcpV1AgentInfo {
            name: info.name,
            title: info.title,
            version: info.version,
        });
        self.capabilities = Some(capabilities);
        self.phase = AcpV1ProtocolPhase::Ready;
        Ok(AcpV1Observation::Initialized {
            agent_info,
            capabilities,
        })
    }

    fn decode_new_session_response(
        &mut self,
        result: serde_json::Value,
    ) -> Result<AcpV1Observation, AcpV1CodecError> {
        let response: NewSessionResponse = serde_json::from_value(result)?;
        let session_id = response.session_id.0.to_string();
        if self.session_id.replace(session_id.clone()).is_some() {
            return Err(AcpV1CodecError::SessionAlreadyBound);
        }
        Ok(AcpV1Observation::SessionOpened { session_id })
    }

    fn decode_prompt_response(
        &mut self,
        pending: PendingOutboundRequest,
        result: serde_json::Value,
    ) -> Result<AcpV1Observation, AcpV1CodecError> {
        let response: PromptResponse = serde_json::from_value(result.clone())?;
        let stop_reason = copy_stop_reason(response.stop_reason);
        if !self.pending_permissions.is_empty() && stop_reason != AcpV1StopReason::Cancelled {
            return Err(AcpV1CodecError::PromptFinishedWithPendingPermissions {
                count: self.pending_permissions.len(),
            });
        }
        if !self.pending_unsupported.is_empty() {
            return Err(AcpV1CodecError::PromptFinishedWithPendingUnsupported {
                count: self.pending_unsupported.len(),
            });
        }
        let session_id = pending.session_id.ok_or(AcpV1CodecError::SessionNotOpen)?;
        self.require_session(&session_id)?;
        let session_failure = if self.config.adapter_profile == AcpV1AdapterProfile::Codex162 {
            parse_codex_session_failure(&result)?
        } else {
            None
        };
        if session_failure.is_some() && !self.pending_permissions.is_empty() {
            return Err(AcpV1CodecError::PromptFinishedWithPendingPermissions {
                count: self.pending_permissions.len(),
            });
        }
        self.prompt_request_id = None;
        self.cancellation_sent = false;
        if let Some(failure) = session_failure {
            if failure.severity == CodexSessionFailureSeverity::Warning {
                return Err(AcpV1CodecError::InvalidCodexSessionFailure(
                    "warning used a terminal PromptResponse carrier",
                ));
            }
            return Ok(AcpV1Observation::RequestFailed {
                request: AcpV1RequestKind::Prompt,
                code: CODEX_SESSION_FAILURE_ERROR_CODE,
                message: failure.title,
            });
        }
        if !self.pending_permissions.is_empty() {
            let request_ids = std::mem::take(&mut self.pending_permissions)
                .into_keys()
                .collect();
            // The provider closed the prompt before any client response could
            // complete, so deferred batch response slots are unreachable.
            self.pending_inbound_batches.clear();
            return Ok(
                AcpV1Observation::PromptCancelledWithPendingPermissions {
                    session_id,
                    request_ids,
                },
            );
        }
        Ok(AcpV1Observation::PromptFinished {
            session_id,
            stop_reason,
        })
    }

    fn decode_notification(
        &mut self,
        method: &str,
        params: Option<RawJsonRpcParams>,
    ) -> Result<AcpV1Observation, AcpV1CodecError> {
        if method != CLIENT_METHOD_NAMES.session_update {
            return Ok(AcpV1Observation::UnsupportedNotification {
                method: method.to_owned(),
            });
        }
        let notification: SessionNotification = decode_params(params)?;
        let session_id = notification.session_id.0.to_string();
        self.require_session(&session_id)?;
        if self.prompt_request_id.is_none() {
            return Err(AcpV1CodecError::PromptNotActive);
        }
        if self.cancellation_sent {
            return Err(AcpV1CodecError::CancellationAlreadySent);
        }
        let update = serde_json::to_value(notification.update)?;
        if self.config.adapter_profile == AcpV1AdapterProfile::Codex162 {
            if let Some(failure) = parse_codex_session_failure(&update)? {
                if update.get("sessionUpdate").and_then(serde_json::Value::as_str)
                    != Some("session_info_update")
                {
                    return Err(AcpV1CodecError::InvalidCodexSessionFailure(
                        "failure notification used a non-session_info_update carrier",
                    ));
                }
                if failure.severity != CodexSessionFailureSeverity::Warning {
                    return Err(AcpV1CodecError::InvalidCodexSessionFailure(
                        "error used a non-terminal session_info_update carrier",
                    ));
                }
            }
        }
        Ok(AcpV1Observation::SessionUpdate { session_id, update })
    }

    fn decode_request(
        &mut self,
        id: RequestId,
        method: &str,
        params: Option<RawJsonRpcParams>,
        response_destination: InboundResponseDestination,
    ) -> Result<AcpV1Observation, AcpV1CodecError> {
        let request_id = from_sdk_request_id(id)?;
        if self.pending_permissions.contains_key(&request_id)
            || self.pending_unsupported.contains_key(&request_id)
        {
            return Err(AcpV1CodecError::DuplicateInboundRequest(request_id));
        }
        if self.pending_permissions.len() + self.pending_unsupported.len()
            >= MAX_PENDING_CLIENT_REQUESTS
        {
            return Err(AcpV1CodecError::TooManyPendingClientRequests {
                limit: MAX_PENDING_CLIENT_REQUESTS,
            });
        }
        if method != CLIENT_METHOD_NAMES.session_request_permission {
            self.pending_unsupported
                .insert(request_id.clone(), response_destination);
            return Ok(AcpV1Observation::UnsupportedClientRequest {
                request_id,
                method: method.to_owned(),
            });
        }
        if self.prompt_request_id.is_none() {
            return Err(AcpV1CodecError::PromptNotActive);
        }
        if self.cancellation_sent {
            return Err(AcpV1CodecError::CancellationAlreadySent);
        }
        let raw_params = params.map_or(serde_json::Value::Null, RawJsonRpcParams::into_value);
        let callback_payload_digest = permission_callback_digest(
            &request_id,
            CLIENT_METHOD_NAMES.session_request_permission,
            &raw_params,
        );
        let request: RequestPermissionRequest = serde_json::from_value(raw_params)?;
        let session_id = request.session_id.0.to_string();
        self.require_session(&session_id)?;
        if request.options.is_empty() {
            return Err(AcpV1CodecError::EmptyPermissionOptions);
        }
        let mut option_ids = BTreeMap::new();
        let mut options = Vec::with_capacity(request.options.len());
        for option in request.options {
            let option_id = option.option_id.0.to_string();
            let kind = copy_permission_kind(option.kind);
            if option_ids.insert(option_id.clone(), kind).is_some() {
                return Err(AcpV1CodecError::DuplicatePermissionOption(option_id));
            }
            options.push(AcpV1PermissionOption {
                option_id,
                name: option.name,
                kind,
            });
        }
        let tool_call = serde_json::to_value(request.tool_call)?;
        self.pending_permissions.insert(
            request_id.clone(),
            PendingPermission {
                option_ids,
                response_destination,
            },
        );
        Ok(AcpV1Observation::PermissionRequested(
            AcpV1PermissionRequest {
                request_id,
                session_id,
                tool_call,
                options,
                callback_payload_digest,
            },
        ))
    }

}
