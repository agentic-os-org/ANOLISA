//! Byte admission follows an event through queueing and callback dispatch.

use super::FfiEvent;
use crate::probes::budget::{BudgetReservation, ChannelBudget};
use std::sync::Arc;

pub(super) struct QueueBudget {
    budget: ChannelBudget,
}

impl QueueBudget {
    pub(super) fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            budget: ChannelBudget::new(limit),
        })
    }

    pub(super) fn admit(self: &Arc<Self>, event: FfiEvent) -> Result<QueuedEvent, Box<FfiEvent>> {
        let bytes = event.retained_bytes();
        let event = Box::new(event);
        let Some(reservation) = self.budget.reserve_owned(bytes) else {
            return Err(event);
        };
        metrics::gauge!("agentsight_ffi_inflight_bytes").increment(bytes as f64);
        Ok(QueuedEvent {
            event,
            reservation: Reservation(reservation),
        })
    }
}

struct Reservation(BudgetReservation);

impl Drop for Reservation {
    fn drop(&mut self) {
        metrics::gauge!("agentsight_ffi_inflight_bytes").decrement(self.0.bytes as f64);
    }
}

pub(super) struct QueuedEvent {
    pub(super) event: Box<FfiEvent>,
    reservation: Reservation,
}

impl QueuedEvent {
    pub(super) fn dispatch(self, callback: impl FnOnce(FfiEvent)) {
        let Self { event, reservation } = self;
        callback(*event);
        drop(reservation);
    }
}

impl FfiEvent {
    fn retained_bytes(&self) -> usize {
        match self {
            Self::Llm(call) => call.retained_bytes(),
            Self::Https(record, agent) => {
                std::mem::size_of::<Self>()
                    + record.comm.capacity()
                    + record.method.capacity()
                    + record.path.capacity()
                    + record.request_headers.capacity()
                    + record.response_headers.capacity()
                    + record.request_body.as_ref().map_or(0, String::capacity)
                    + record.response_body.as_ref().map_or(0, String::capacity)
                    + agent.as_ref().map_or(0, String::capacity)
            }
            Self::Security(event) => {
                struct SizeWriter(usize);
                impl std::io::Write for SizeWriter {
                    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
                        self.0 = self.0.saturating_add(data.len());
                        Ok(data.len())
                    }
                    fn flush(&mut self) -> std::io::Result<()> {
                        Ok(())
                    }
                }
                let mut writer = SizeWriter(0);
                let _ = serde_json::to_writer(&mut writer, event);
                std::mem::size_of::<Self>().saturating_add(writer.0.saturating_mul(4))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reservation_survives_dequeue_and_is_released_after_callback() {
        let event = FfiEvent::Llm(super::super::tests::make_llm_call(None, 1));
        let bytes = event.retained_bytes();
        let budget = QueueBudget::new(bytes);
        let queued = budget.admit(event).expect("first event fits");
        assert!(
            budget
                .admit(FfiEvent::Llm(super::super::tests::make_llm_call(None, 2)))
                .is_err()
        );
        queued.dispatch(|_| assert_eq!(budget.budget.watermarks().in_flight_bytes, bytes));
        assert_eq!(budget.budget.watermarks().in_flight_bytes, 0);
    }

    #[test]
    fn failed_send_and_receiver_drop_refund_reservations() {
        let budget = QueueBudget::new(usize::MAX);
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        for _ in 0..2 {
            let event = FfiEvent::Llm(super::super::tests::make_llm_call(None, 1));
            let _ = tx.try_send(budget.admit(event).expect("event fits"));
        }
        assert!(budget.budget.watermarks().in_flight_bytes > 0);
        drop(rx);
        assert_eq!(budget.budget.watermarks().in_flight_bytes, 0);
    }
}
