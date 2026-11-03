//! Read-only system-key status; initialization never replaces existing trust.

use super::SkillSecService;
use crate::ledger::storage::missing;
use crate::{KeyStore, SkillSecError, check_deadline};
use serde_json::{Value, json};
use std::time::Instant;

impl SkillSecService {
    /// Reports system key readiness without creating keys or trusting caller HOME.
    ///
    /// # Errors
    /// Reports inaccessible private state and malformed keys.
    pub fn key_status(&self, deadline: Instant) -> Result<Value, SkillSecError> {
        check_deadline(deadline)?;
        let result = match KeyStore::open(&self.config()?.state_dir)?.load() {
            Ok(key) => {
                json!({"initialized":true,"fingerprint":key.fingerprint(),"encrypted":false,"keyringSize":0})
            }
            Err(error) if missing(&error) => {
                json!({"initialized":false,"fingerprint":null,"encrypted":false,"keyringSize":0})
            }
            Err(error) => {
                json!({"initialized":false,"fingerprint":null,"error":error.to_string(),"encrypted":false,"keyringSize":0})
            }
        };
        Ok(result)
    }
}
