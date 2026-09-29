//! Protocol-independent Unix-domain-socket service framework.
//!
//! This crate owns bounded connection admission, one-request framing, trusted
//! kernel peer credentials, response framing, socket ownership cleanup, and
//! controlled drain. Wire decoding, request identities, authentication,
//! authorization, and application dispatch belong to the injected
//! [`RequestDispatcher`]. Transport rejection projection uses the separate
//! [`RejectionEncoder`] port so it need not depend on application state.

#![forbid(unsafe_code)]

mod config;
mod dispatcher;
mod frame;
mod server;
mod shutdown;

pub use config::{ConfigError, ServiceConfig};
pub use dispatcher::{
    DispatchControl, DispatchError, DispatchRequest, PeerCredentials, RejectedRequest,
    RejectionEncoder, RejectionReason, RequestDispatcher, ResponseDisposition,
};
pub use server::{BindError, BoundUnixSocket, ServeError, ServeReport, UnixService};
pub use shutdown::ShutdownToken;
