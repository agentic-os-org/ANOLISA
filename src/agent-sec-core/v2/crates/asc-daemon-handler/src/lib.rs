//! Inbound daemon protocol adapters for application use cases.
//!
//! This crate translates bounded transport requests into versioned daemon
//! protocol calls, applies server-owned authorization, routes accepted calls to
//! application ports, and projects application or transport failures back into
//! protocol responses. Process bootstrap and transport execution remain in
//! `asc-daemon` and `asc-daemon-service`, respectively.

#![forbid(unsafe_code)]

mod action;
mod dispatcher;
mod pap;
mod rejection;

pub use dispatcher::DaemonDispatcher;
pub use rejection::JsonRejectionEncoder;
