impl TaskSnapshotAdapter {
    fn resolve_binding(&self) -> Result<CheckpointBinding, ContractError> {
        self.endpoint.verify_socket_unchanged()?;
        let identity = self
            .endpoint
            .client
            .workspace_identity_v2(&self.endpoint.registration_path)
            .map_err(|failure| task_snapshot_failure("checkpoint_identity_unavailable", &failure))?;
        Ok(CheckpointBinding {
            version: BINDING_VERSION,
            ws_id: identity.ws_id,
            registered_path: identity.registered_path,
            generation: identity.generation.into_bytes(),
            owner_uid: self.endpoint.owner_uid,
        })
    }

    pub(crate) fn admit(
        socket_path: PathBuf,
        registration_path: &Path,
        workspaces: TrustedWorkspaceResolver,
        owner_uid: u32,
    ) -> Result<Self, CheckpointAdmissionError> {
        Ok(Self {
            endpoint: CheckpointEndpoint::admit(socket_path, registration_path, owner_uid)?,
            workspaces,
        })
    }

    fn validate_request(&self, request: &TaskSnapshotProviderRequest) -> Result<(), ContractError> {
        if &request.workspace != self.workspaces.workspace_ref() {
            return Err(pre_runtime_checkpoint_error(
                "checkpoint_workspace_mismatch",
                ErrorCategory::InvalidRequest,
                false,
                "The snapshot workspace does not match Gateway admission",
            ));
        }
        self.endpoint.verify_socket_unchanged()
    }

    fn guarded_preview(
        &self,
        request: &TaskSnapshotProviderRequest,
    ) -> Result<(TaskSnapshotProviderPreview, CheckpointBinding, [u8; 32]), ContractError> {
        self.validate_request(request)?;
        let binding = self.resolve_binding()?;
        let preview = self
            .endpoint
            .client
            .guarded_rollback_preview_v2(
                &binding.registered_path,
                &binding.ws_id,
                WorkspaceGenerationTokenV2::from_bytes(binding.generation),
                request.snapshot_id.as_str(),
            )
            .map_err(|failure| task_snapshot_failure("checkpoint_preview_failed", &failure))?;
        let changes = preview
            .changes
            .into_iter()
            .map(|change| {
                let kind = match change.change_type {
                    ChangeType::Added => "added",
                    ChangeType::Modified => "modified",
                    ChangeType::Deleted => "deleted",
                    ChangeType::Renamed => "renamed",
                };
                Ok(TaskSnapshotChange {
                    path: bounded_text(&change.path)?,
                    change: BoundedOpaque::new(kind)
                        .map_err(|_| checkpoint_error("checkpoint_preview_invalid", false))?,
                    detail: change.detail.as_deref().map(bounded_text).transpose()?,
                })
            })
            .collect::<Result<Vec<_>, ContractError>>()?;
        let preview_digest = digest_parts(&[
            TASK_SNAPSHOT_PREVIEW_DOMAIN,
            request.task_id.as_str().as_bytes(),
            request.snapshot_id.as_str().as_bytes(),
            request.workspace.scope_digest.as_str().as_bytes(),
            binding.ws_id.as_bytes(),
            &binding.generation,
            &serde_json::to_vec(&changes)
                .map_err(|_| checkpoint_error("checkpoint_preview_invalid", false))?,
        ])?;
        Ok((
            TaskSnapshotProviderPreview {
                changes,
                preview_digest,
            },
            binding,
            preview.diff_digest,
        ))
    }
}
impl TaskSnapshotDriver for TaskSnapshotAdapter {
    fn preview(
        &mut self,
        request: &TaskSnapshotProviderRequest,
    ) -> Result<TaskSnapshotProviderPreview, ContractError> {
        self.guarded_preview(request).map(|(preview, _, _)| preview)
    }

    fn create_recovery(
        &mut self,
        request: &TaskSnapshotProviderRequest,
        recovery_id: &CheckpointId,
        preview_digest: &Digest,
    ) -> Result<(), ContractError> {
        self.validate_request(request)?;
        let binding = self.resolve_binding()?;
        let generation = WorkspaceGenerationTokenV2::from_bytes(binding.generation);
        let operation_digest = digest_parts(&[
            TASK_SWITCH_RECOVERY_DOMAIN,
            request.task_id.as_str().as_bytes(),
            request.snapshot_id.as_str().as_bytes(),
            recovery_id.as_str().as_bytes(),
            preview_digest.as_str().as_bytes(),
            binding.ws_id.as_bytes(),
            &binding.generation,
        ])?;
        let operation_bytes = digest_bytes(&operation_digest)?;
        match self.endpoint.client.guarded_create_v2(
            &binding.ws_id,
            generation,
            recovery_id.as_str(),
            operation_bytes,
            Some("COSH Task snapshot switch recovery"),
            None,
            true,
        ) {
            Ok(evidence)
                if task_recovery_evidence_matches(
                    &evidence,
                    &binding,
                    recovery_id,
                    operation_bytes,
                ) =>
            {
                Ok(())
            }
            Ok(_) => Err(checkpoint_error("checkpoint_recovery_not_created", false)),
            Err(failure) => match self.endpoint.client.checkpoint_evidence_v2(
                &binding.ws_id,
                generation,
                recovery_id.as_str(),
                operation_bytes,
            ) {
                Ok(Some(evidence))
                    if task_recovery_evidence_matches(
                        &evidence,
                        &binding,
                        recovery_id,
                        operation_bytes,
                    ) =>
                {
                    Ok(())
                }
                _ => Err(pre_runtime_checkpoint_error(
                    "checkpoint_recovery_unproven",
                    ErrorCategory::Transport,
                    false,
                    &failure.error.message,
                )),
            },
        }
    }

    fn switch(
        &mut self,
        request: &TaskSnapshotProviderRequest,
        expected_preview_digest: &Digest,
        operation_id: &CheckpointId,
        operation_digest: &Digest,
    ) -> Result<TaskSnapshotProviderSwitchResult, ContractError> {
        let (preview, binding, provider_diff_digest) = self.guarded_preview(request)?;
        if preview.preview_digest != *expected_preview_digest {
            return Err(checkpoint_error(
                "checkpoint_switch_preview_mismatch",
                false,
            ));
        }
        let operation_digest = digest_bytes(operation_digest)?;
        match self.endpoint.client.guarded_rollback_v2(
            &binding.registered_path,
            &binding.ws_id,
            WorkspaceGenerationTokenV2::from_bytes(binding.generation),
            request.snapshot_id.as_str(),
            provider_diff_digest,
            operation_id.as_str(),
            operation_digest,
        ) {
            Ok(_) => {
                if self.workspaces.refresh_after_snapshot_switch().is_err() {
                    return Ok(TaskSnapshotProviderSwitchResult::PossiblyApplied {
                        error: pre_runtime_checkpoint_error(
                            "checkpoint_workspace_refresh_failed",
                            ErrorCategory::RuntimeUnavailable,
                            false,
                            "Snapshot switched but workspace refresh failed; new Tasks are blocked; repair the workspace and restart Gateway",
                        ),
                    });
                }
                Ok(TaskSnapshotProviderSwitchResult::Switched(
                    TaskSnapshotProviderSwitch {
                        from: BoundedOpaque::new(hex_bytes(&binding.generation)).map_err(|_| {
                            checkpoint_error("checkpoint_switch_result_invalid", false)
                        })?,
                        to: request.snapshot_id.clone(),
                    },
                ))
            }
            Err(failure) if failure.effect == CkptRequestEffect::KnownNoEffect => {
                Ok(TaskSnapshotProviderSwitchResult::Rejected {
                    error: task_snapshot_failure("checkpoint_switch_rejected", &failure.error),
                })
            }
            Err(failure) => {
                // An uncertain rollback may already have removed the admitted inode.
                let _ = self.workspaces.invalidate_after_snapshot_switch();
                Ok(TaskSnapshotProviderSwitchResult::PossiblyApplied {
                    error: pre_runtime_checkpoint_error(
                        "checkpoint_switch_uncertain",
                        ErrorCategory::Transport,
                        false,
                        &failure.error.message,
                    ),
                })
            }
        }
    }
}

fn task_recovery_evidence_matches(
    evidence: &GuardedCheckpointEvidenceV2,
    binding: &CheckpointBinding,
    recovery_id: &CheckpointId,
    operation_digest: [u8; 32],
) -> bool {
    evidence.ws_id == binding.ws_id
        && evidence.registered_path == binding.registered_path
        && evidence.generation.as_bytes() == &binding.generation
        && evidence.checkpoint_id == recovery_id.as_str()
        && evidence.operation_digest == operation_digest
        && evidence.caller_uid == binding.owner_uid
        && matches!(
            &evidence.outcome,
            GuardedCheckpointOutcomeV2::Created { snapshot_id }
                if snapshot_id == recovery_id.as_str()
        )
}
