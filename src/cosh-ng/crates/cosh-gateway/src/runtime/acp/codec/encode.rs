impl AcpV1Codec {
    fn encode_request(
        &mut self,
        request: ClientRequest,
    ) -> Result<(AcpV1RequestId, String), AcpV1CodecError> {
        let method = request.method().to_owned();
        let params = serde_json::to_value(request)?;
        let request_id = self.next_request_id()?;
        let raw = RawJsonRpcMessage::request(method, params, to_sdk_request_id(&request_id))
            .map_err(|error| AcpV1CodecError::Sdk(error.to_string()))?;
        let frame = self.encode_raw(&raw)?;
        Ok((request_id, frame))
    }

    fn encode_notification(
        &self,
        notification: ClientNotification,
    ) -> Result<String, AcpV1CodecError> {
        let method = notification.method().to_owned();
        let params = serde_json::to_value(notification)?;
        let raw = RawJsonRpcMessage::notification(method, params)
            .map_err(|error| AcpV1CodecError::Sdk(error.to_string()))?;
        self.encode_raw(&raw)
    }

    fn permission_outcome_message(
        &self,
        request_id: &AcpV1RequestId,
        outcome: RequestPermissionOutcome,
    ) -> Result<RawJsonRpcMessage, AcpV1CodecError> {
        let response = RequestPermissionResponse::new(outcome);
        let result = serde_json::to_value(response)?;
        Ok(RawJsonRpcMessage::response(
            to_sdk_request_id(request_id),
            Ok(result),
        ))
    }

    fn settle_inbound_response(
        &mut self,
        destination: InboundResponseDestination,
        response: RawJsonRpcMessage,
    ) -> Result<Vec<String>, AcpV1CodecError> {
        match destination {
            InboundResponseDestination::Individual => Ok(vec![self.encode_raw(&response)?]),
        }
    }

    fn encode_raw(&self, raw: &RawJsonRpcMessage) -> Result<String, AcpV1CodecError> {
        let frame = serde_json::to_string(raw)?;
        self.validate_encoded_frame(frame)
    }

    fn validate_encoded_frame(&self, frame: String) -> Result<String, AcpV1CodecError> {
        if frame.len() > self.config.max_frame_bytes {
            return Err(AcpV1CodecError::FrameTooLarge {
                limit: self.config.max_frame_bytes,
            });
        }
        Ok(frame)
    }

    fn next_request_id(&mut self) -> Result<AcpV1RequestId, AcpV1CodecError> {
        let sequence = self.next_request_sequence;
        self.next_request_sequence = sequence
            .checked_add(1)
            .ok_or(AcpV1CodecError::RequestIdExhausted)?;
        Ok(AcpV1RequestId::String(format!("cosh-acp-{sequence}")))
    }

    fn require_phase(
        &self,
        expected: AcpV1ProtocolPhase,
        operation: &'static str,
    ) -> Result<(), AcpV1CodecError> {
        if self.phase != expected {
            return Err(self.invalid_phase(operation));
        }
        Ok(())
    }

    fn require_session(&self, actual: &str) -> Result<(), AcpV1CodecError> {
        let expected = self
            .session_id
            .as_deref()
            .ok_or(AcpV1CodecError::SessionNotOpen)?;
        if expected != actual {
            return Err(AcpV1CodecError::SessionMismatch {
                expected: expected.to_owned(),
                actual: actual.to_owned(),
            });
        }
        Ok(())
    }

    fn invalid_phase(&self, operation: &'static str) -> AcpV1CodecError {
        AcpV1CodecError::InvalidPhase {
            operation,
            phase: self.phase,
        }
    }
}
