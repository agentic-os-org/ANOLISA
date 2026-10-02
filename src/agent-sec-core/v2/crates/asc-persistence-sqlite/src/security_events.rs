//! Security-event `SQLite` binding: table spec, migration, repository, policy.

pub mod migration;
pub mod policy;
pub mod reader;
pub mod repository;
pub mod table;
pub mod writer;

pub use migration::{SecurityEventsMigrator, VERDICT_MIGRATION_BATCH_SIZE};
pub use policy::{DropSink, SecurityEventsFaultPolicy, StderrDropSink, WriteDrop};
pub use reader::SqliteEventReader;
pub use repository::{EventFilters, SecurityEventRepository, VALID_GROUP_FIELDS};
pub use table::SECURITY_EVENTS_TABLES;
pub use writer::{SqliteEventWriter, WriterError};
