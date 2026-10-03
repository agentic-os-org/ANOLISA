use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::ns::validate_user_id;

/// Time-ordered session identifier. Format: `ses_<ULID>`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct SessionId(String);

impl SessionId {
    /// Generate a fresh session id (ULID-based, time-ordered).
    pub fn generate() -> Self {
        Self(format!("ses_{}", ulid::Ulid::new()))
    }

    /// Wrap an externally provided id. Same validation rules as `user_id`:
    /// the value is interpolated into the on-disk session directory name,
    /// so we reject anything that could escape the session base dir.
    pub fn from_string(s: impl Into<String>) -> Result<Self> {
        let s = s.into();
        validate_user_id(&s)?;
        Ok(Self(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for SessionId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::MemoryError;

    #[test]
    fn generate_uses_ses_prefix() {
        let sid = SessionId::generate();
        assert!(sid.as_str().starts_with("ses_"));
    }

    #[test]
    fn from_string_accepts_normal() {
        assert!(SessionId::from_string("ses_x").is_ok());
        assert!(SessionId::from_string("smoke-1").is_ok());
    }

    #[test]
    fn from_string_rejects_traversal() {
        assert!(matches!(
            SessionId::from_string("../escape"),
            Err(MemoryError::InvalidArgument(_))
        ));
        assert!(matches!(
            SessionId::from_string("a/b"),
            Err(MemoryError::InvalidArgument(_))
        ));
        assert!(matches!(
            SessionId::from_string("a\0b"),
            Err(MemoryError::InvalidArgument(_))
        ));
        assert!(matches!(
            SessionId::from_string(""),
            Err(MemoryError::InvalidArgument(_))
        ));
    }
}
