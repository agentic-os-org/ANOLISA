//! Observability `SQLite` binding: table spec, repository, policy.
//!
//! This stream passes **no** migrator to the kernel: it is still at schema
//! revision 1, so generic convergence is all it needs.

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
