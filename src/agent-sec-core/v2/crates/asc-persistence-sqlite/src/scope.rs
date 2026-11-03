//! Server-owned row-level authorization for store reads.
//!
//! The v1 daemon enforced one owner per database by running per-user with a
//! 0600 socket; the v2 daemon is a single system service for many local UIDs,
//! so read paths must carry the owner boundary explicitly. A
//! [`QueryScope`] is constructed only by trusted server code from
//! kernel-authenticated peer credentials — it is never decoded from request
//! parameters, and caller-supplied identity fields never influence it.
//!
//! The scope lives in `asc-security-events` with the rest of the query
//! contract so daemon handlers can depend on the port without depending on
//! this storage crate; it is re-exported here for the repository and reader
//! facades that apply it in SQL.

pub use asc_security_events::query::QueryScope;
