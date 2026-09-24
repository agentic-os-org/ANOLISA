//! Dual-write assembly for security and observability events.
//!
//! Daemons own explicitly configured sinks. Legacy process-global access remains
//! for security events only; observability uses [`ConfiguredObservabilitySinks`].
//! This layer reuses the existing writers and owns no persistence implementation.
//!
//! | Entry point | `JSONL` path | `SQLite` path | Reported to caller |
//! |---|---|---|---|
//! | [`log_event`] | swallows | swallows | never |
//! | [`ConfiguredObservabilitySinks::record`] | raises | raises | always |
//!
//! Hosts close configured sinks directly. [`shutdown_sinks`] closes only the
//! process-global security-event sink.

#![forbid(unsafe_code)]

pub mod configured;
pub mod error;
pub mod security_events;
pub mod shutdown;
pub mod singletons;
pub mod telemetry;
#[cfg(test)]
mod test_support;

pub use configured::{ConfiguredObservabilitySinks, ConfiguredSecurityEventSinks};
pub use error::SinkError;
pub use security_events::log_event;
pub use shutdown::{shutdown_sinks, shutdown_sinks_at};
pub use singletons::{initialized_sqlite_writer, reader, sqlite_writer, writer};

#[cfg(feature = "testing")]
pub use singletons::{
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
    }
}
