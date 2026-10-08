//! Server-owned row-level authorization for store reads.
//!
//! The v1 daemon enforced one owner per database by running per-user with a
//! 0600 socket; the v2 daemon is a single system service for many local UIDs,
//! so read paths must carry the owner boundary explicitly. A
//! [`QueryScope`] is constructed only by trusted server code from
//! kernel-authenticated peer credentials — it is never decoded from request
//! parameters, and caller-supplied identity fields never influence it.

/// The owner whose rows one server query is authorized to read.
///
/// Exactly one variant exists today: every caller reads exactly one owner's
/// rows. Cross-owner audit access requires an explicitly authorized server
/// role that this daemon does not assign yet, so it is deliberately absent
/// rather than silently available (issue #6608).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryScope {
    /// Rows whose producing peer was kernel-authenticated as this UID.
    Owner(u32),
}

impl QueryScope {
    /// Returns the owner UID this scope may read.
    #[must_use]
    pub const fn owner_uid(self) -> u32 {
        match self {
            Self::Owner(uid) => uid,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_owner_scope_carries_its_uid() {
        assert_eq!(QueryScope::Owner(1000).owner_uid(), 1000);
    }
}
