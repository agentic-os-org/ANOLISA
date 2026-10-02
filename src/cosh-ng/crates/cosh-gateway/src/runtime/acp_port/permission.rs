impl AcpAgentRuntime {
    fn map_permission_requested(
        &mut self,
        request: AcpV1PermissionRequest,
    ) -> Result<Option<RuntimeEventEnvelope>, AgentRuntimePortError> {
        self.require_session(&request.session_id)?;
        let turn_id = self
            .active_turn
            .clone()
            .ok_or(AgentRuntimePortError::Protocol)?;
        let tool_call_id = request
            .tool_call
            .get("toolCallId")
            .and_then(serde_json::Value::as_str)
            .ok_or(AgentRuntimePortError::Protocol)?;
        if tool_call_id.is_empty() || tool_call_id.len() > DEFAULT_MAX_TOOL_IDENTIFIER_BYTES {
            return Err(AgentRuntimePortError::Protocol);
        }

        // A callback may refine a Codex presentation snapshot into the exact
        // command being approved, but it never bypasses the canonical tool
        // lifecycle or changes the stable COSH tool identity.
        let mut canonical_request = request.clone();
        let mut promoted_projection = None;
        let (tool_use_id, summary) = if let Some(tool_snapshot) =
            self.tools
                .snapshot(&request.session_id, &turn_id, tool_call_id)
        {
            if permission_carrier_matches_snapshot(
                &request.tool_call,
                &tool_snapshot.tool_call,
            ) {
                canonical_request.tool_call = tool_snapshot.tool_call;
                (
                    tool_snapshot.projection.tool_use_id,
                    tool_snapshot.projection.summary,
                )
            } else {
                if self.config.session.client.adapter_profile != AcpV1AdapterProfile::Codex162 {
                    return Err(AgentRuntimePortError::Protocol);
                }
                let canonical_carrier = canonicalize_codex_command_permission_refinement(
                    &request.tool_call,
                    &tool_snapshot.tool_call,
                )?;
                let refined = self
                    .tools
                    .refine_in_progress_permission_carrier(
                        &request.session_id,
                        &turn_id,
                        &canonical_carrier,
                    )
                    .map_err(|_| AgentRuntimePortError::Protocol)?;
                canonical_request.tool_call = refined.tool_call;
                promoted_projection = Some(refined.projection.clone());
                (refined.projection.tool_use_id, refined.projection.summary)
            }
        } else {
            if self.config.session.client.adapter_profile != AcpV1AdapterProfile::Codex162 {
                return Err(AgentRuntimePortError::Protocol);
            }
            let canonical_carrier =
                canonicalize_self_contained_permission_carrier(&request.tool_call)?;
            let tool_snapshot = self
                .tools
                .promote_permission_carrier(
                    &request.session_id,
                    &turn_id,
                    &canonical_carrier,
                )
                .map_err(|_| AgentRuntimePortError::Protocol)?;
            canonical_request.tool_call = tool_snapshot.tool_call;
            promoted_projection = Some(tool_snapshot.projection.clone());
            (
                tool_snapshot.projection.tool_use_id,
                tool_snapshot.projection.summary,
            )
        };
        let context = AcpPermissionContext {
            actor: self.config.identity.actor.clone(),
            task_id: self.config.identity.task_id.clone(),
            run_id: self.config.identity.run_id.clone(),
        };
        let normalized = self.normalizer.normalize(&canonical_request, &context)?;
        if normalized.task_id != context.task_id
            || normalized.run_id != context.run_id
            || normalized.actor != context.actor
            || self.permissions.contains_key(&normalized.request_id)
        {
            return Err(AgentRuntimePortError::IdentityMismatch);
        }
        let callback = provider_permission_callback(&request, &normalized)?;
        let allow_once = request
            .options
            .iter()
            .find(|option| option.kind == AcpV1PermissionOptionKind::AllowOnce)
            .map(|option| option.option_id.clone());
        let reject_once = request
            .options
            .iter()
            .find(|option| option.kind == AcpV1PermissionOptionKind::RejectOnce)
            .map(|option| option.option_id.clone());
        self.permissions.insert(
            normalized.request_id.clone(),
            PendingPermission {
                acp_request_id: request.request_id,
                allow_once,
                reject_once,
            },
        );
        let permission_event = AgentRuntimeEvent::ExecutionPermissionRequested {
            turn_id,
            tool_use_id: Some(tool_use_id),
            summary,
            request: normalized,
            callback,
        };
        if let Some(snapshot) = promoted_projection {
            let observed = self.event(AgentRuntimeEvent::ToolInvocationUpdated { snapshot });
            let permission = self.event(permission_event);
            self.events.push_back(permission);
            Ok(Some(observed))
        } else {
            Ok(Some(self.event(permission_event)))
        }
    }
}
