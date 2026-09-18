//! Small, stable primitives shared across protocol and application crates.

#![forbid(unsafe_code)]

mod daemon_socket;
mod identifier;
mod revision;

pub use daemon_socket::{DAEMON_SOCKET_ENV, DaemonSocketPathError, daemon_socket_path_from_env};
pub use identifier::{IdentifierError, ResourceId};
pub use revision::{Revision, RevisionError};
