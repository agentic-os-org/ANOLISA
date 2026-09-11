//! Process-wide dual-write sinks for security and observability events.
//!
//! Migrated from v1 `security_events/__init__.py` and
//! `observability/__init__.py`. This is the assembly layer: it owns the lazily
//! built process-wide sinks and the two dual-write entry points, and contains no
//! persistence logic of its own.
//!
//! The two entry points differ on purpose, and the difference is v1's:
//!
//! | Entry point | `JSONL` path | `SQLite` path | Reported to caller |
//! |---|---|---|---|
//! | [`log_event`] | swallows | swallows | never |
//! | [`record_observability`] | raises | raises | always |
//!
//! [`shutdown::shutdown_sinks`] documents the one place v2 cannot match v1: there
//! is no `atexit`, so the host owns shutdown.

#![forbid(unsafe_code)]

pub mod error;
pub mod observability;
pub mod security_events;
pub mod shutdown;
pub mod singletons;
#[cfg(test)]
mod test_support;

pub use error::SinkError;
pub use observability::record_observability;
pub use security_events::log_event;
pub use shutdown::{shutdown_sinks, shutdown_sinks_at};
pub use singletons::{
    initialized_observability_sqlite_writer, initialized_sqlite_writer,
    observability_sqlite_writer, observability_writer, reader, sqlite_writer, writer,
};

#[cfg(feature = "testing")]
pub use singletons::{
    install_observability_sqlite_writer_for_test, install_observability_writer_for_test,
    install_reader_for_test, install_sqlite_writer_for_test, install_writer_for_test,
    reset_sinks_for_test,
};

#[cfg(test)]
mod tests {
    use asc_sqlite_kernel::assert_send;

    use crate::singletons::reset_sinks_for_test;
    use crate::test_support::serial;

    /// The sinks are shared across threads through an `Arc`, so every one of them
    /// must be `Send`.
    #[test]
    fn the_shared_sinks_are_send() {
        assert_send::<asc_event_log::SecurityEventWriter>();
        assert_send::<asc_event_log::ObservabilityWriter>();
        assert_send::<asc_persistence_sqlite::security_events::SqliteEventWriter>();
        assert_send::<asc_persistence_sqlite::security_events::SqliteEventReader>();
        assert_send::<asc_persistence_sqlite::observability::ObservabilitySqliteWriter>();
    }

    /// Nothing in this crate may touch the filesystem before an accessor is
    /// called — v1's globals are lazily assigned and so are these.
    #[test]
    fn merely_linking_this_crate_initializes_nothing() {
        let _guard = serial();
        reset_sinks_for_test();

        assert!(crate::initialized_sqlite_writer().is_none());
        assert!(crate::initialized_observability_sqlite_writer().is_none());
    }
}
