//! Security-event `SQLite` binding: table spec, migration, repository, policy.

pub mod migration;
pub mod policy;
pub mod query_source;
pub mod reader;
pub mod repository;
pub mod table;
pub mod writer;

pub use migration::{SecurityEventsMigrator, VERDICT_MIGRATION_BATCH_SIZE};
pub use policy::{DropSink, SecurityEventsFaultPolicy, StderrDropSink, WriteDrop};
pub use query_source::SqliteEventQuerySource;
pub use reader::SqliteEventReader;
pub use repository::{MAX_GROUP_BUCKETS, SecurityEventRepository};
pub use table::SECURITY_EVENTS_TABLES;
pub use writer::{SqliteEventWriter, WriterError};

// The query vocabulary lives in `asc-security-events::query`; re-exported
// here so existing storage-side consumers keep their import paths.
pub use asc_security_events::query::{EventFilters, GroupCounts, VALID_GROUP_FIELDS};
