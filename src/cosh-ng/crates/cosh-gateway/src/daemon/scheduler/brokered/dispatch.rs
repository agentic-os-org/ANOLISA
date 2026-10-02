impl<F: RuntimeFactory> TaskScheduler<F> {
    fn dispatch_brokered_acknowledgement(
        &mut self,
        actor_id: &ActorId,
        lease: &LeaseClaim,
        brokered: &BrokeredExecutionRef,
        acknowledgement: BrokeredRequestAcknowledgement,
        payload_digest: &Digest,
        prepared: BrokeredRuntimeDispatchRecord,
        now_ms: u64,
    ) -> Result<Option<SchedulerTick>, GatewayDaemonError> {
        if prepared.state != BrokeredRuntimeDispatchState::Prepared {
            return self
                .reject_replayed_brokered_dispatch(prepared, now_ms)
                .map(Some);
        }
        let start_command = brokered_dispatch_command(
            actor_id,
            "start",
            BrokeredRuntimeDispatchKind::Acknowledgement,
            brokered,
            prepared.revision,
            now_ms,
        )?;
        let started = match self.coordinator.store.start_brokered_runtime_dispatch(
            &start_command,
            BrokeredRuntimeDispatchKind::Acknowledgement,
            brokered,
            payload_digest,
            prepared.revision,
            lease,
        )? {
            LedgerOutcome::Applied(record) => record,
            LedgerOutcome::Replayed(record) => {
                return self
                    .reject_replayed_brokered_dispatch(record, now_ms)
                    .map(Some)
            }
        };
        let write = self
            .active
            .as_mut()
            .ok_or_else(no_active_run)?
            .handle
            .acknowledge_brokered_request(brokered, acknowledgement);
        let dispatched_at_ms = refreshed_now_ms(now_ms)?;
        self.require_active_lease_time(dispatched_at_ms)?;
        if let Err(error) = write {
            return self
                .fail_unknown_brokered_dispatch(actor_id, lease, &started, error, dispatched_at_ms)
                .map(Some);
        }
        let complete_command = brokered_dispatch_command(
            actor_id,
            "complete",
            BrokeredRuntimeDispatchKind::Acknowledgement,
            brokered,
            started.revision,
            dispatched_at_ms,
        )?;
        match self.coordinator.store.complete_brokered_runtime_dispatch(
            &complete_command,
            BrokeredRuntimeDispatchKind::Acknowledgement,
            brokered,
            payload_digest,
            started.revision,
            lease,
        ) {
            Ok(LedgerOutcome::Applied(record))
                if record.state == BrokeredRuntimeDispatchState::Delivered =>
            {
                Ok(None)
            }
            Ok(LedgerOutcome::Applied(record)) | Ok(LedgerOutcome::Replayed(record)) => self
                .reject_replayed_brokered_dispatch(record, dispatched_at_ms)
                .map(Some),
            Err(_) => self
                .fail_unknown_brokered_dispatch(
                    actor_id,
                    lease,
                    &started,
                    runtime_lost_error(
                        "brokered_acknowledgement_receipt_unknown",
                        "Runtime accepted an acknowledgement whose receipt could not be persisted",
                    )?,
                    dispatched_at_ms,
                )
                .map(Some),
        }
    }

    fn dispatch_brokered_result(
        &mut self,
        actor_id: &ActorId,
        lease: &LeaseClaim,
        brokered: &BrokeredExecutionRef,
        delivery: BrokeredExecutionDelivery,
        payload_digest: &Digest,
        prepared: BrokeredRuntimeDispatchRecord,
        now_ms: u64,
    ) -> Result<Option<SchedulerTick>, GatewayDaemonError> {
        if prepared.state != BrokeredRuntimeDispatchState::Prepared {
            return self
                .reject_replayed_brokered_dispatch(prepared, now_ms)
                .map(Some);
        }
        let start_command = brokered_dispatch_command(
            actor_id,
            "start",
            BrokeredRuntimeDispatchKind::Result,
            brokered,
            prepared.revision,
            now_ms,
        )?;
        let started = match self.coordinator.store.start_brokered_runtime_dispatch(
            &start_command,
            BrokeredRuntimeDispatchKind::Result,
            brokered,
            payload_digest,
            prepared.revision,
            lease,
        )? {
            LedgerOutcome::Applied(record) => record,
            LedgerOutcome::Replayed(record) => {
                return self
                    .reject_replayed_brokered_dispatch(record, now_ms)
                    .map(Some)
            }
        };
        let write = self
            .active
            .as_mut()
            .ok_or_else(no_active_run)?
            .handle
            .deliver_brokered_result(brokered, delivery);
        let dispatched_at_ms = refreshed_now_ms(now_ms)?;
        self.require_active_lease_time(dispatched_at_ms)?;
        if let Err(error) = write {
            return self
                .fail_unknown_brokered_dispatch(actor_id, lease, &started, error, dispatched_at_ms)
                .map(Some);
        }
        #[cfg(test)]
        if std::mem::take(&mut self.fail_next_brokered_result_completion) {
            return self
                .fail_unknown_brokered_dispatch(
                    actor_id,
                    lease,
                    &started,
                    runtime_lost_error(
                        "brokered_result_receipt_unknown",
                        "Runtime accepted a brokered result whose receipt could not be persisted",
                    )?,
                    dispatched_at_ms,
                )
                .map(Some);
        }
        let complete_command = brokered_dispatch_command(
            actor_id,
            "complete",
            BrokeredRuntimeDispatchKind::Result,
            brokered,
            started.revision,
            dispatched_at_ms,
        )?;
        match self.coordinator.store.complete_brokered_runtime_dispatch(
            &complete_command,
            BrokeredRuntimeDispatchKind::Result,
            brokered,
            payload_digest,
            started.revision,
            lease,
        ) {
            Ok(LedgerOutcome::Applied(record))
                if record.state == BrokeredRuntimeDispatchState::Delivered =>
            {
                Ok(None)
            }
            Ok(LedgerOutcome::Applied(record)) | Ok(LedgerOutcome::Replayed(record)) => self
                .reject_replayed_brokered_dispatch(record, dispatched_at_ms)
                .map(Some),
            Err(_) => self
                .fail_unknown_brokered_dispatch(
                    actor_id,
                    lease,
                    &started,
                    runtime_lost_error(
                        "brokered_result_receipt_unknown",
                        "Runtime accepted a brokered result whose receipt could not be persisted",
                    )?,
                    dispatched_at_ms,
                )
                .map(Some),
        }
    }

}
