//! Observability `SQLite` binding: table spec, repository, policy.
//!
//! Every writer supplies a trusted UID. The owner column stays nullable so
//! revision-1 stores converge without backfilling an owner for historical rows.

pub mod policy;
pub mod reader;
pub mod repository;
pub mod table;
pub mod writer;

pub use policy::ObservabilityFaultPolicy;
pub use reader::ObservabilityReader;
pub use repository::ObservabilityEventRepository;
pub use table::OBSERVABILITY_TABLES;
pub use writer::{ObservabilitySqliteWriter, ObservabilityWriterError};
