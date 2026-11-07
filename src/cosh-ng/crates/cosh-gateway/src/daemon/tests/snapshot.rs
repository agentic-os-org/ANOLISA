use super::*;

struct SnapshotDriver {
    changes: Vec<TaskSnapshotChange>,
    recovery: Option<CheckpointId>,
    failure: &'static str,
    switch_calls: usize,
    preview_failure: Option<ContractError>,
}

impl TaskSnapshotDriver for SnapshotDriver {
    fn preview(
        &mut self,
        _: &TaskSnapshotProviderRequest,
    ) -> Result<TaskSnapshotProviderPreview, ContractError> {
        if let Some(error) = &self.preview_failure {
            return Err(error.clone());
        }
        if self.recovery.is_some() && self.failure == "preview_error" {
            return Err(snapshot_error());
        }
        let drifted = self.recovery.is_some() && self.failure == "changed";
        Ok(TaskSnapshotProviderPreview {
            changes: self.changes.clone(),
            preview_digest: digest_json(&(&self.changes, drifted)).unwrap(),
        })
    }

    fn create_recovery(
        &mut self,
        _: &TaskSnapshotProviderRequest,
        recovery: &CheckpointId,
        _: &Digest,
    ) -> Result<(), ContractError> {
        self.recovery = Some(recovery.clone());
        if self.failure == "unproven" {
            Err(snapshot_error())
        } else {
            Ok(())
        }
    }

    fn switch(
        &mut self,
        request: &TaskSnapshotProviderRequest,
        _: &Digest,
        _: &CheckpointId,
        _: &Digest,
    ) -> Result<TaskSnapshotProviderSwitchResult, ContractError> {
        self.switch_calls += 1;
        match self.failure {
            "rejected" => Ok(TaskSnapshotProviderSwitchResult::Rejected {
                error: snapshot_error(),
            }),
            "unknown" => Ok(TaskSnapshotProviderSwitchResult::PossiblyApplied {
                error: snapshot_error(),
            }),
            "failed" => Err(snapshot_error()),
            _ => Ok(TaskSnapshotProviderSwitchResult::Switched(
                TaskSnapshotProviderSwitch {
                    from: BoundedOpaque::new("generation").unwrap(),
                    to: request.snapshot_id.clone(),
                },
            )),
        }
    }
}

fn snapshot_error() -> ContractError {
    ContractError::new(
        "snapshot_unavailable",
        ErrorCategory::RuntimeUnavailable,
        false,
        "snapshot unavailable",
    )
    .unwrap()
}

fn snapshot_task(path: &Path) -> (TaskCoordinator, ActorId, InspectTaskSnapshot) {
    let mut coordinator = TaskCoordinator::open(path, None).unwrap();
    let actor = actor_ref_for_uid(&coordinator.installation_id, 1000)
        .unwrap()
        .actor_id;
    let task = coordinator.submit(&actor, submit("snapshot-task")).unwrap();
    let run_id = task.active_run_id.clone().unwrap();
    coordinator
        .cancel(
            &actor,
            CancelTask {
                request_id: RequestId::new(),
                idempotency_key: IdempotencyKey::new("cancel").unwrap(),
                task_id: task.task_id.clone(),
                run_id: run_id.clone(),
                expected_revision: Some(task.revision),
            },
        )
        .unwrap();
    let snapshot_id = CheckpointId::new();
    rusqlite::Connection::open(path).unwrap().execute(
        "INSERT INTO pre_runtime_baselines(task_id,run_id,baseline_id,policy,state,evidence_json,created_at_ms,updated_at_ms)
         VALUES (?1,?2,?3,'on','created','{}',1,1)",
        rusqlite::params![task.task_id.as_str(), run_id.as_str(), snapshot_id.as_str()],
    ).unwrap();
    (
        coordinator,
        actor,
        InspectTaskSnapshot {
            task_id: task.task_id,
            snapshot_id,
        },
    )
}

fn switch_request(preview: &TaskSnapshotPreview) -> SwitchTaskSnapshot {
    SwitchTaskSnapshot {
        request_id: RequestId::new(),
        idempotency_key: IdempotencyKey::new("snapshot-switch").unwrap(),
        task_id: preview.task_id.clone(),
        snapshot_id: preview.snapshot_id.clone(),
        preview_digest: preview.preview_digest.clone(),
        expected_revision: preview.revision,
    }
}

#[test]
fn proven_snapshot_recovery_survives_switch_failure_and_reopen() {
    for failure in [
        "rejected",
        "unknown",
        "failed",
        "changed",
        "preview_error",
        "unproven",
    ] {
        let root = private_tempdir();
        let path = root.path().join("gateway.db");
        let (mut coordinator, actor, inspect) = snapshot_task(&path);
        let mut driver = SnapshotDriver {
            changes: vec![],
            recovery: None,
            failure,
            switch_calls: 0,
            preview_failure: None,
        };
        let preview = coordinator
            .snapshot_preview(&actor, &inspect, &mut driver)
            .unwrap();
        let request = switch_request(&preview);
        assert!(
            coordinator
                .switch_snapshot(&actor, request.clone(), &mut driver)
                .is_err(),
            "{failure}"
        );
        let record = coordinator
            .store
            .load_task_snapshot_switch(&actor, &request.idempotency_key)
            .unwrap()
            .unwrap();
        assert_eq!(
            record.state,
            match failure {
                "unknown" => "unknown",
                "preview_error" => "recovery_created",
                _ => "failed",
            }
        );
        assert_eq!(
            driver.switch_calls,
            usize::from(matches!(failure, "rejected" | "unknown" | "failed"))
        );
        drop(coordinator);
        let coordinator = TaskCoordinator::open(&path, None).unwrap();
        driver.failure = "";
        let recovery = InspectTaskSnapshot {
            task_id: inspect.task_id.clone(),
            snapshot_id: driver.recovery.clone().unwrap(),
        };
        let inventory = coordinator.snapshots(&actor, &inspect.task_id).unwrap();
        assert_eq!(
            inventory
                .snapshots
                .iter()
                .any(|s| s.snapshot_id == recovery.snapshot_id),
            failure != "unproven",
            "{failure}"
        );
        assert_eq!(
            coordinator
                .snapshot_preview(&actor, &recovery, &mut driver)
                .is_ok(),
            failure != "unproven",
            "{failure}"
        );
        assert!(coordinator
            .snapshot_preview(&ActorId::new(), &recovery, &mut driver)
            .is_err());
    }
}

#[test]
fn snapshot_preview_fits_transport_and_binds_omitted_changes() {
    let root = private_tempdir();
    let (mut coordinator, actor, inspect) = snapshot_task(&root.path().join("gateway.db"));
    let change = TaskSnapshotChange {
        path: BoundedText::new("\u{1}".repeat(MAX_TEXT_BYTES)).unwrap(),
        change: BoundedOpaque::new("modified").unwrap(),
        detail: Some(BoundedText::new("\u{1}".repeat(MAX_TEXT_BYTES)).unwrap()),
    };
    let mut driver = SnapshotDriver {
        changes: vec![change; 64],
        recovery: None,
        failure: "",
        switch_calls: 0,
        preview_failure: None,
    };
    assert!(serde_json::to_vec(&driver.changes).unwrap().len() > MAX_GATEWAY_FRAME_BYTES);
    let preview = coordinator
        .snapshot_preview(&actor, &inspect, &mut driver)
        .unwrap();
    assert!(preview.changes_omitted > 0);
    assert!(!preview.changes.is_empty());
    assert_eq!(
        preview.changes.len() + preview.changes_omitted,
        driver.changes.len()
    );
    assert_eq!(
        preview.preview_digest,
        digest_json(&(&driver.changes, false)).unwrap()
    );
    let mut wire = Vec::new();
    write_frame(
        &mut wire,
        &GatewayResult::TaskSnapshotPreview(preview.clone()),
    )
    .unwrap();
    let GatewayResult::TaskSnapshotPreview(decoded) = read_frame(&mut Cursor::new(wire)).unwrap()
    else {
        panic!("preview response")
    };
    assert_eq!(decoded, preview);
    driver.changes.last_mut().unwrap().detail = None;
    let changed = coordinator
        .snapshot_preview(&actor, &inspect, &mut driver)
        .unwrap();
    assert_eq!(changed.changes, preview.changes);
    assert_ne!(changed.preview_digest, preview.preview_digest);
    assert!(coordinator
        .switch_snapshot(&actor, switch_request(&preview), &mut driver)
        .unwrap_err()
        .to_string()
        .contains("workspace changed"));
    assert!(driver.recovery.is_none());
    coordinator
        .switch_snapshot(&actor, switch_request(&changed), &mut driver)
        .unwrap();
    assert_eq!(driver.switch_calls, 1);
}

fn assert_wire_contract(error: &GatewayDaemonError, code: &str, recoverable: bool) {
    let GatewayDaemonError::Contract {
        code: actual_code,
        message,
        recoverable: actual_recoverable,
    } = error
    else {
        panic!("expected contract error, got {error:?}")
    };
    assert_eq!(actual_code, code);
    assert!(!message.is_empty());
    assert_eq!(*actual_recoverable, recoverable);
    let response = error_response(None, error);
    let GatewayResponseOutcome::Error { error: body } = response.outcome else {
        panic!("expected error response")
    };
    assert_eq!(body.code, code);
    assert_eq!(body.message, *message);
    assert_eq!(body.recoverable, recoverable);
    assert_ne!(body.message, "request violates the Gateway contract");
}

#[test]
fn snapshot_preview_preserves_provider_contract_error_on_wire() {
    let root = private_tempdir();
    let (coordinator, actor, inspect) = snapshot_task(&root.path().join("gateway.db"));
    let mut driver = SnapshotDriver {
        changes: vec![],
        recovery: None,
        failure: "",
        switch_calls: 0,
        preview_failure: Some(
            ContractError::new(
                "checkpoint_daemon_protocol_mismatch",
                ErrorCategory::Transport,
                false,
                "Invalid response from ws-ckpt daemon",
            )
            .unwrap(),
        ),
    };
    let error = coordinator
        .snapshot_preview(&actor, &inspect, &mut driver)
        .unwrap_err();
    assert_wire_contract(&error, "checkpoint_daemon_protocol_mismatch", false);
    let GatewayDaemonError::Contract { message, .. } = &error else {
        panic!("expected contract error")
    };
    assert_eq!(message, "Invalid response from ws-ckpt daemon");
}

#[test]
fn snapshot_switch_reports_revision_conflict_on_wire() {
    let root = private_tempdir();
    let (mut coordinator, actor, inspect) = snapshot_task(&root.path().join("gateway.db"));
    let mut driver = SnapshotDriver {
        changes: vec![],
        recovery: None,
        failure: "",
        switch_calls: 0,
        preview_failure: None,
    };
    let preview = coordinator
        .snapshot_preview(&actor, &inspect, &mut driver)
        .unwrap();
    let mut stale = switch_request(&preview);
    stale.expected_revision += 1;
    let error = coordinator
        .switch_snapshot(&actor, stale, &mut driver)
        .unwrap_err();
    assert_wire_contract(&error, "task_snapshot_revision_conflict", true);
}

#[test]
fn snapshot_switch_reports_stale_preview_digest_on_wire() {
    let root = private_tempdir();
    let (mut coordinator, actor, inspect) = snapshot_task(&root.path().join("gateway.db"));
    let mut driver = SnapshotDriver {
        changes: vec![],
        recovery: None,
        failure: "",
        switch_calls: 0,
        preview_failure: None,
    };
    let preview = coordinator
        .snapshot_preview(&actor, &inspect, &mut driver)
        .unwrap();
    driver.changes.push(TaskSnapshotChange {
        path: BoundedText::new("changed.txt").unwrap(),
        change: BoundedOpaque::new("modified").unwrap(),
        detail: None,
    });
    let error = coordinator
        .switch_snapshot(&actor, switch_request(&preview), &mut driver)
        .unwrap_err();
    assert_wire_contract(&error, "task_snapshot_preview_stale", true);
}

#[test]
fn error_response_preserves_contract_codes_and_legacy_fallbacks() {
    let response = error_response(
        None,
        &GatewayDaemonError::Protocol("framing violation".to_owned()),
    );
    let GatewayResponseOutcome::Error { error: body } = response.outcome else {
        panic!("expected error response")
    };
    assert_eq!(body.code, "invalid_request");
    assert_eq!(body.message, "request violates the Gateway contract");
    assert!(!body.recoverable);

    let response = error_response(
        None,
        &GatewayDaemonError::Store(StoreError::RevisionConflict {
            expected: 1,
            actual: 2,
        }),
    );
    let GatewayResponseOutcome::Error { error: body } = response.outcome else {
        panic!("expected error response")
    };
    assert_eq!(body.code, "task_version_conflict");
    assert!(body.recoverable);

    let response = error_response(
        None,
        &GatewayDaemonError::Contract {
            code: "checkpoint_switch_rejected_diff_mismatch".to_owned(),
            message: "workspace digest differs from the preview".to_owned(),
            recoverable: false,
        },
    );
    let GatewayResponseOutcome::Error { error: body } = response.outcome else {
        panic!("expected error response")
    };
    assert_eq!(body.code, "checkpoint_switch_rejected_diff_mismatch");
    assert_eq!(body.message, "workspace digest differs from the preview");
    assert!(!body.recoverable);
}
