use crate::runtime::prelude::ShellEvent;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ShellEventCursor(usize);

impl ShellEventCursor {
    pub(crate) fn position(self) -> usize {
        self.0
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ShellEventBatch<'a> {
    pub(crate) from: ShellEventCursor,
    pub(crate) to: ShellEventCursor,
    pub(crate) events: &'a [ShellEvent],
}

impl ShellEventBatch<'_> {
    pub(crate) fn global_index(&self, local_index: usize) -> usize {
        self.from.position() + local_index
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ShellEventSnapshot<'a> {
    base: usize,
    events: &'a [ShellEvent],
}

impl<'a> ShellEventSnapshot<'a> {
    #[cfg(test)]
    pub(crate) fn new(events: &'a [ShellEvent]) -> Self {
        Self { base: 0, events }
    }

    pub(crate) fn with_base(base: usize, events: &'a [ShellEvent]) -> Self {
        Self { base, events }
    }

    pub(crate) fn base(&self) -> usize {
        self.base
    }

    pub(crate) fn events(&self) -> &[ShellEvent] {
        self.events
    }

    pub(crate) fn cursor(&self) -> ShellEventCursor {
        ShellEventCursor(self.base.saturating_add(self.events.len()))
    }

    pub(crate) fn batch_since(&self, cursor: ShellEventCursor) -> ShellEventBatch<'a> {
        let from = cursor.position().clamp(self.base, self.cursor().position());
        ShellEventBatch {
            from: ShellEventCursor(from),
            to: self.cursor(),
            events: &self.events[from - self.base..],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_drains_batch_since_cursor() {
        let events = vec![
            ShellEvent::user_input_intercepted("s", "one"),
            ShellEvent::user_input_intercepted("s", "two"),
        ];
        let snapshot = ShellEventSnapshot::new(&events);

        let first = snapshot.batch_since(ShellEventCursor::default());
        assert_eq!(first.from.position(), 0);
        assert_eq!(first.to.position(), 2);
        assert_eq!(first.events.len(), 2);

        let second = snapshot.batch_since(first.to);
        assert!(second.events.is_empty());
        assert_eq!(second.from.position(), 2);
        assert_eq!(second.to.position(), 2);
    }

    #[test]
    fn snapshot_and_batch_borrow_the_event_history() {
        let events = vec![ShellEvent::user_input_intercepted("s", "one")];
        let snapshot = ShellEventSnapshot::new(&events);
        let batch = snapshot.batch_since(ShellEventCursor::default());

        assert!(std::ptr::eq(snapshot.events().as_ptr(), events.as_ptr()));
        assert!(std::ptr::eq(batch.events.as_ptr(), events.as_ptr()));
    }

    #[test]
    fn batch_maps_local_to_global_event_index() {
        let events = [ShellEvent::user_input_intercepted("s", "one")];
        let batch = ShellEventBatch {
            from: ShellEventCursor(7),
            to: ShellEventCursor(8),
            events: &events,
        };

        assert_eq!(batch.global_index(0), 7);
    }

    #[test]
    fn compacted_snapshot_keeps_absolute_cursor_and_batch_indices() {
        let events = [
            ShellEvent::user_input_intercepted("s", "one"),
            ShellEvent::user_input_intercepted("s", "two"),
        ];
        let snapshot = ShellEventSnapshot::with_base(4_096, &events);
        let batch = snapshot.batch_since(ShellEventCursor(4_097));

        assert_eq!(snapshot.cursor().position(), 4_098);
        assert_eq!(batch.from.position(), 4_097);
        assert_eq!(batch.to.position(), 4_098);
        assert_eq!(batch.global_index(0), 4_097);
        assert_eq!(batch.events[0].input.as_deref(), Some("two"));
    }
}
