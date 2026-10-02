use super::*;

struct SnapshotDriver {
    changes: Vec<TaskSnapshotChange>,
    recovery: Option<CheckpointId>,
    failure: &'static str,
    switch_calls: usize,
}

impl TaskSnapshotDriver for SnapshotDriver {
    fn preview(
        &mut self,
        _: &TaskSnapshotProviderRequest,
    ) -> Result<TaskSnapshotProviderPreview, ContractError> {
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
                reason: BoundedText::new("rejected").unwrap(),
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
