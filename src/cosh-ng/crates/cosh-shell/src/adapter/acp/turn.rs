//! Per-turn read-only restriction, frozen in design section 6.4.
//!
//! The envelope stays standard ACP: the agent advertises the capability under
//! the `initialize` response `_meta`, and the client carries the restriction on
//! `session/prompt` `_meta`. Only the Cosh payload is defined here. This module
//! never grants execution; it decides whether a restricted turn may be sent.

use agent_client_protocol::v1::schema::Meta;

/// Capability key an agent advertises when it can enforce a read-only turn.
pub const CAPABILITY_KEY: &str = "_cosh/turn";
/// Extension version negotiated independently of the ACP wire version.
pub const VERSION: u64 = 1;

/// The one restriction payload this client sends and accepts.
pub fn restriction() -> serde_json::Value {
    serde_json::json!({ "version": VERSION, "readOnly": true })
}

/// Whether the agent promised exactly the restriction this client will send.
///
/// A different version, a missing flag or another shape is not a promise, so
/// the caller must refuse the turn instead of hoping the peer complies.
pub fn advertised(meta: Option<&Meta>) -> bool {
    meta.and_then(|meta| meta.get(CAPABILITY_KEY))
        .is_some_and(|value| value == &restriction())
}
