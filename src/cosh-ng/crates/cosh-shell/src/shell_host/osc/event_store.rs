//! Absolute-cursor shell-event retention and incremental journaling.

use std::collections::HashSet;
use std::io;
use std::ops::Deref;
use std::path::{Path, PathBuf};

use crate::journal::ShellEventJournal;
use crate::types::{ShellEvent, ShellEventKind};

use super::super::model::{ShellEventView, INTERACTIVE_EVENT_WINDOW_EVENTS};
use super::super::transcript::TranscriptRetention;
use super::OscParser;

#[derive(Debug)]
pub(crate) struct EventStore {
    events: Vec<ShellEvent>,
    base: usize,
    journaled: usize,
    journal: Option<ShellEventJournal>,
    journal_path: Option<PathBuf>,
}

impl EventStore {
    pub(super) fn new(retention: TranscriptRetention, journal_path: &Path) -> io::Result<Self> {
        let (journal, incremental_path) = match retention {
            TranscriptRetention::Full => (None, None),
            TranscriptRetention::Bounded { .. } => (
                Some(ShellEventJournal::create(journal_path)?),
                Some(journal_path.to_path_buf()),
            ),
        };
        Ok(Self {
            events: Vec::new(),
            base: 0,
            journaled: 0,
            journal,
            journal_path: incremental_path,
        })
    }

    pub(crate) fn push(&mut self, event: ShellEvent) {
        self.events.push(event);
    }

    pub(super) fn view(&self) -> ShellEventView<'_> {
        ShellEventView::new(self.base, &self.events)
    }

    pub(super) fn position(&self) -> usize {
        self.base.saturating_add(self.events.len())
    }

    pub(super) fn persist_pending(&mut self) -> io::Result<()> {
        if self.journal.is_none() {
            return Ok(());
        }
        let position = self.position();
        let start = self
            .journaled
            .checked_sub(self.base)
            .ok_or_else(|| {
                io::Error::other(format!(
                    "event window compacted before journal persistence: journaled={} base={} position={position}",
                    self.journaled, self.base
                ))
            })?;
        if start == self.events.len() {
            return Ok(());
        }
        let journal = self
            .journal
            .as_mut()
            .ok_or_else(|| io::Error::other("incremental event journal disappeared"))?;
        journal.append(&self.events[start..])?;
        self.journaled = self.position();
        Ok(())
    }

    pub(super) fn compact_observed(&mut self, observed: usize) {
        if self.journal.is_none()
            || self.events.len() <= INTERACTIVE_EVENT_WINDOW_EVENTS.saturating_mul(2)
        {
            return;
        }
        let persisted_and_observed = observed.min(self.journaled).min(self.position());
        let max_drop = persisted_and_observed.saturating_sub(self.base);
        let desired_drop = self
            .events
            .len()
            .saturating_sub(INTERACTIVE_EVENT_WINDOW_EVENTS)
            .min(max_drop);
        let Some(boundary) = safe_compaction_boundary(&self.events, desired_drop, max_drop) else {
            return;
        };
        self.events.drain(..boundary);
        self.base = self.base.saturating_add(boundary);
    }

    fn take_output_events(&mut self) -> io::Result<Vec<ShellEvent>> {
        self.persist_pending()?;
        if self.journal.is_some() {
            Ok(Vec::new())
        } else {
            Ok(std::mem::take(&mut self.events))
        }
    }

    fn is_incremental(&self) -> bool {
        self.journal.is_some()
    }

    fn incremental_path(&self) -> Option<&Path> {
        self.journal_path.as_deref()
    }
}

fn safe_compaction_boundary(
    events: &[ShellEvent],
    desired_drop: usize,
    max_drop: usize,
) -> Option<usize> {
    let mut active = HashSet::new();
    let mut intercepted = HashSet::new();
    let mut boundary = None;
    for (index, event) in events.iter().take(max_drop).enumerate() {
        let command_id = event.command_id.as_deref();
        match event.kind {
            ShellEventKind::CommandStarted => {
                if let Some(command_id) = command_id {
                    active.insert(command_id);
                }
            }
            ShellEventKind::UserInputIntercepted => {
                if let Some(command_id) = command_id.filter(|id| active.remove(*id)) {
                    intercepted.insert(command_id);
                }
            }
            ShellEventKind::CommandCompleted | ShellEventKind::CommandFailed => {
                if let Some(command_id) = command_id {
                    active.remove(command_id);
                    intercepted.remove(command_id);
                }
            }
            ShellEventKind::ShellReady => {
                intercepted.clear();
            }
            _ => {}
        }
        if active.is_empty() && intercepted.is_empty() {
            let candidate = index + 1;
            if candidate >= desired_drop {
                // Crossing the target is safe within max_drop and releases
                // an oversized lifecycle as soon as its closing event lands.
                return Some(candidate);
            }
            boundary = Some(candidate);
        }
    }
    boundary
}

impl Deref for EventStore {
    type Target = [ShellEvent];

    fn deref(&self) -> &Self::Target {
        &self.events
    }
}

impl OscParser {
    pub(crate) fn observe_events<W, F, T>(
        &mut self,
        output: &mut W,
        observer: &mut F,
    ) -> io::Result<T>
    where
        F: FnMut(ShellEventView<'_>, &mut W) -> io::Result<T>,
    {
        self.events.persist_pending()?;
        let result = observer(self.events.view(), output)?;
        let observed = self.events.position();
        self.events.compact_observed(observed);
        Ok(result)
    }

    pub(crate) fn take_output_events(&mut self) -> io::Result<Vec<ShellEvent>> {
        self.events.take_output_events()
    }

    pub(crate) fn uses_incremental_event_journal(&self) -> bool {
        self.events.is_incremental()
    }

    pub(crate) fn incremental_event_journal_path(&self) -> Option<&Path> {
        self.events.incremental_path()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ShellEvent;
    use std::os::unix::fs::PermissionsExt;

    fn ready(index: usize) -> ShellEvent {
        let mut event = ShellEvent::user_input_intercepted("session", format!("ready-{index}"));
        event.kind = ShellEventKind::ShellReady;
        event
    }

    #[test]
    fn bounded_store_keeps_absolute_positions_after_compaction() {
        let work_dir = std::env::temp_dir().join(format!(
            "cosh-event-window-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&work_dir);
        std::fs::create_dir_all(&work_dir).expect("work dir");
        let mut store = EventStore::new(
            TranscriptRetention::Bounded { window_bytes: 4 },
            &work_dir.join("events.jsonl"),
        )
        .expect("event store");

        store.push(ShellEvent::user_input_intercepted(
            "session",
            "curl --token event-window-secret",
        ));
        for index in 0..(INTERACTIVE_EVENT_WINDOW_EVENTS * 3) {
            store.push(ready(index));
        }
        store.persist_pending().expect("persist events");
        let position = store.position();
        store.compact_observed(position);

        assert_eq!(store.position(), position);
        assert!(store.len() <= INTERACTIVE_EVENT_WINDOW_EVENTS);
        assert!(store.view().base() > 0);
        let journal =
            std::fs::read_to_string(work_dir.join("events.jsonl")).expect("incremental journal");
        assert_eq!(journal.lines().count(), position);
        assert!(!journal.contains("event-window-secret"), "{journal}");
        assert_eq!(
            std::fs::metadata(work_dir.join("events.jsonl"))
                .expect("journal metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let _ = std::fs::remove_dir_all(work_dir);
    }

    #[test]
    fn registered_cursor_reads_each_event_once_across_compactions() {
        let work_dir = std::env::temp_dir().join(format!(
            "cosh-event-cursor-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&work_dir);
        std::fs::create_dir_all(&work_dir).expect("work dir");
        let mut store = EventStore::new(
            TranscriptRetention::Bounded { window_bytes: 4 },
            &work_dir.join("events.jsonl"),
        )
        .expect("event store");
        let mut cursor = 0;
        let mut observed = Vec::new();
        let mut expected = Vec::new();

        for batch in 0..40 {
            let batch_len = (batch * 37 % 211) + 1;
            for offset in 0..batch_len {
                let id = format!("event-{batch}-{offset}");
                expected.push(id.clone());
                let mut event = ready(batch * 1_000 + offset);
                event.input = Some(id);
                store.push(event);
            }
            store.persist_pending().expect("persist batch");
            let view = store.view();
            let local_cursor = cursor - view.base();
            observed.extend(
                view.events()[local_cursor..]
                    .iter()
                    .filter_map(|event| event.input.clone()),
            );
            cursor = view.position();
            store.compact_observed(cursor);
        }

        assert_eq!(observed, expected);
        assert_eq!(cursor, store.position());
        assert!(store.len() <= INTERACTIVE_EVENT_WINDOW_EVENTS * 2);
        let _ = std::fs::remove_dir_all(work_dir);
    }

    #[test]
    fn bounded_store_compacts_through_a_long_commands_closing_boundary() {
        let work_dir = std::env::temp_dir().join(format!(
            "cosh-event-long-command-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&work_dir);
        std::fs::create_dir_all(&work_dir).expect("work dir");
        let mut store = EventStore::new(
            TranscriptRetention::Bounded { window_bytes: 4 },
            &work_dir.join("events.jsonl"),
        )
        .expect("event store");

        store.push(ShellEvent::command_started(
            "session",
            "command-1",
            "long-command",
            "/tmp",
            1,
        ));
        for index in 0..(INTERACTIVE_EVENT_WINDOW_EVENTS * 3) {
            store.push(ready(index));
        }
        store.push(ShellEvent::command_finished(
            ShellEventKind::CommandCompleted,
            "session",
            "command-1",
            0,
            2,
            "",
        ));
        store.persist_pending().expect("persist events");
        let position = store.position();
        store.compact_observed(position);

        assert_eq!(store.position(), position);
        assert!(store.len() <= INTERACTIVE_EVENT_WINDOW_EVENTS);
        assert_eq!(store.view().base(), position);
        let _ = std::fs::remove_dir_all(work_dir);
    }

    #[test]
    fn journal_invariant_error_reports_absolute_cursor_context() {
        let work_dir = std::env::temp_dir().join(format!(
            "cosh-event-invariant-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&work_dir);
        std::fs::create_dir_all(&work_dir).expect("work dir");
        let mut store = EventStore::new(
            TranscriptRetention::Bounded { window_bytes: 4 },
            &work_dir.join("events.jsonl"),
        )
        .expect("event store");
        store.base = 2;
        store.journaled = 1;

        let error = store.persist_pending().expect_err("invalid cursor");
        let message = error.to_string();
        assert!(message.contains("journaled=1"), "{message}");
        assert!(message.contains("base=2"), "{message}");
        assert!(message.contains("position=2"), "{message}");
        let _ = std::fs::remove_dir_all(work_dir);
    }
}
