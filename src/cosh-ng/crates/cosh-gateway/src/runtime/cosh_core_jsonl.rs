//! Pure codec for the private cosh-core newline-delimited JSON protocol.
//!
//! `PRIVATE_COSH_CONTROL_PROTOCOL_VERSION` versions this internal COSH wire
//! contract. It is unrelated to ACP and must never be advertised as ACP.

mod codec;
mod types;

#[cfg(test)]
mod tests;

pub use codec::CoshCoreJsonlCodec;
pub use types::*;
