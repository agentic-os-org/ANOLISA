use std::num::NonZeroU32;

use serde::{Deserialize, Serialize};

/// Positive immutable revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Revision(NonZeroU32);

impl Revision {
    /// Builds a positive revision.
    ///
    /// # Errors
    /// Revision zero is invalid.
    pub const fn new(value: u32) -> Result<Self, RevisionError> {
        match NonZeroU32::new(value) {
            Some(value) => Ok(Self(value)),
            None => Err(RevisionError),
        }
    }

    /// Returns the numeric revision.
    pub const fn get(self) -> u32 {
        self.0.get()
    }

    /// Allocates the next positive revision without wrapping.
    ///
    /// # Errors
    /// Returns an error when the revision space is exhausted.
    pub const fn checked_next(self) -> Result<Self, RevisionError> {
        match self.get().checked_add(1) {
            Some(value) => Self::new(value),
            None => Err(RevisionError),
        }
    }
}

/// Invalid revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("revision must be between 1 and u32::MAX")]
pub struct RevisionError;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revision_is_positive_and_bounded() {
        assert!(Revision::new(0).is_err());
        let maximum = Revision::new(u32::MAX).unwrap();
        assert_eq!(maximum.get(), u32::MAX);
        assert!(maximum.checked_next().is_err());
    }
}
