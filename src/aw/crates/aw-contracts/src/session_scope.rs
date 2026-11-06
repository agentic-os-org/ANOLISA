//! Session identity checks shared by the contract validator.

use crate::{require, validation::same, Error};
use serde_json::Value;

/// Ensures a scope claims only a session confirmed by the runtime binding.
pub(crate) fn validate(scope: &Value, runtime: &Value) -> Result<(), Error> {
    if runtime.get("session_id").is_some() {
        same(scope, runtime, &["session_id"])
    } else {
        require(
            scope.get("session_id").is_none(),
            "scope session is not confirmed by the runtime binding",
        )
    }
}
