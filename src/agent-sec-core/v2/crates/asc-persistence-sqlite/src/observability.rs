//! Observability `SQLite` binding: table spec, repository, policy.
//!
//! Legacy facades retain revision 1. The system daemon uses [`owned`] at revision 2;
//! the kernel transactionally converges its nullable owner column and indexes.

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

/// System-daemon owner-aware schema and atomic writer.
pub mod owned;
