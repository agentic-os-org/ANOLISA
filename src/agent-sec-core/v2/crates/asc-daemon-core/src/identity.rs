use std::collections::BTreeSet;
use std::sync::RwLock;

/// Kernel-authenticated identity of one daemon socket peer.
///
/// This type is constructed by trusted transport code and is never decoded
/// from request JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerCredentials {
    uid: u32,
    gid: u32,
    pid: u32,
}

impl PeerCredentials {
    /// Creates trusted peer credentials from kernel-provided values.
    pub const fn new(uid: u32, gid: u32, pid: u32) -> Self {
        Self { uid, gid, pid }
    }

    /// Returns the authenticated user ID.
    pub const fn uid(self) -> u32 {
        self.uid
    }

    /// Returns the authenticated group ID.
    pub const fn gid(self) -> u32 {
        self.gid
    }

    /// Returns the authenticated process ID.
    pub const fn pid(self) -> u32 {
        self.pid
    }
}

/// Role assigned by trusted server policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrincipalRole {
    /// Authenticated local caller without Policy administration authority.
    LocalUser,
    /// Caller authorized to administer Policy, Scope, and Binding resources.
    PolicyAdministrator,
}

/// Server-owned policy that maps authenticated peer evidence to a role.
///
/// Implementations may consult deployment-owned UID/GID allowlists or a later
/// authenticated binding. Request JSON is intentionally absent from this port.
pub trait PrincipalPolicy: Send + Sync {
    /// Selects the role for one kernel-authenticated peer.
    fn role_for(&self, peer: PeerCredentials) -> PrincipalRole;
}

/// Deployment-configured Policy-administrator allowlist with root-only delegation.
///
/// UID 0 is always a Policy administrator. Other UIDs must be configured by the
/// deployment operator at startup or added by root at runtime. The allowlist is
/// process-local; configuration-file loading and persistence remain separate work.
#[derive(Debug, Default)]
pub struct RootManagedPrincipalPolicy {
    allowed_uids: RwLock<BTreeSet<u32>>,
}

impl RootManagedPrincipalPolicy {
    /// Initializes additional administrators from trusted deployment configuration.
    ///
    /// Use only at daemon composition, never with request-supplied identity data.
    /// An operator controlling startup already controls the daemon's process and
    /// resources. Configured administrators do not gain runtime delegation rights.
    pub fn with_admin_uids(uids: impl IntoIterator<Item = u32>) -> Self {
        Self {
            allowed_uids: RwLock::new(uids.into_iter().collect()),
        }
    }

    /// Adds one UID to the Policy-administrator allowlist.
    ///
    /// The acting peer must be the kernel-authenticated root peer. Merely being
    /// present in the administrator allowlist does not grant delegation rights.
    ///
    /// # Errors
    /// Returns `RootRequired` for a non-root actor and `StateUnavailable` if the
    /// process-local allowlist lock was poisoned.
    pub fn allow_uid(
        &self,
        acting_peer: PeerCredentials,
        uid: u32,
    ) -> Result<(), PrincipalPolicyError> {
        if acting_peer.uid() != 0 {
            return Err(PrincipalPolicyError::RootRequired);
        }
        self.allowed_uids
            .write()
            .map_err(|_| PrincipalPolicyError::StateUnavailable)?
            .insert(uid);
        Ok(())
    }
}

impl PrincipalPolicy for RootManagedPrincipalPolicy {
    fn role_for(&self, peer: PeerCredentials) -> PrincipalRole {
        if peer.uid() == 0
            || self
                .allowed_uids
                .read()
                .is_ok_and(|allowed| allowed.contains(&peer.uid()))
        {
            PrincipalRole::PolicyAdministrator
        } else {
            PrincipalRole::LocalUser
        }
    }
}

/// Failure to update the root-owned Policy-administrator allowlist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PrincipalPolicyError {
    /// Only kernel-authenticated UID 0 may delegate Policy administration.
    #[error("only root may update the Policy administrator allowlist")]
    RootRequired,
    /// The in-process authorization state cannot be updated safely.
    #[error("Policy administrator allowlist is unavailable")]
    StateUnavailable,
}

/// Trusted daemon principal, never caller-authored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Principal {
    peer: PeerCredentials,
    role: PrincipalRole,
}

impl Principal {
    /// Creates a principal after server-side authentication and role binding.
    pub const fn from_authenticated_peer(peer: PeerCredentials, role: PrincipalRole) -> Self {
        Self { peer, role }
    }

    /// Returns kernel-authenticated peer evidence.
    pub const fn peer(self) -> PeerCredentials {
        self.peer
    }

    /// Returns the server-assigned role.
    pub const fn role(self) -> PrincipalRole {
        self.role
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn principal_keeps_transport_identity_separate_from_server_role() {
        let peer = PeerCredentials::new(1000, 100, 4242);
        let principal =
            Principal::from_authenticated_peer(peer, PrincipalRole::PolicyAdministrator);

        assert_eq!(principal.peer().uid(), 1000);
        assert_eq!(principal.peer().gid(), 100);
        assert_eq!(principal.peer().pid(), 4242);
        assert_eq!(principal.role(), PrincipalRole::PolicyAdministrator);
    }

    #[test]
    fn root_can_delegate_policy_administration_but_delegates_cannot() {
        let policy = RootManagedPrincipalPolicy::default();
        let root = PeerCredentials::new(0, 0, 1);
        let delegated = PeerCredentials::new(1000, 100, 2);
        let other = PeerCredentials::new(2000, 200, 3);

        assert_eq!(policy.role_for(root), PrincipalRole::PolicyAdministrator);
        assert_eq!(policy.role_for(delegated), PrincipalRole::LocalUser);
        policy.allow_uid(root, delegated.uid()).unwrap();
        assert_eq!(
            policy.role_for(delegated),
            PrincipalRole::PolicyAdministrator
        );
        assert_eq!(
            policy.allow_uid(delegated, other.uid()),
            Err(PrincipalPolicyError::RootRequired)
        );
        assert_eq!(policy.role_for(other), PrincipalRole::LocalUser);
    }

    #[test]
    fn startup_administrators_are_explicit_and_cannot_delegate() {
        let configured = PeerCredentials::new(1000, 100, 2);
        let other = PeerCredentials::new(2000, 100, 3);
        let root = PeerCredentials::new(0, 0, 1);
        let policy = RootManagedPrincipalPolicy::with_admin_uids([1000, 1000]);
        assert_eq!(
            policy.role_for(configured),
            PrincipalRole::PolicyAdministrator
        );
        assert_eq!(policy.role_for(root), PrincipalRole::PolicyAdministrator);
        assert_eq!(policy.role_for(other), PrincipalRole::LocalUser);
        assert_eq!(
            policy.allow_uid(configured, 2000),
            Err(PrincipalPolicyError::RootRequired)
        );
        assert_eq!(policy.role_for(other), PrincipalRole::LocalUser);
        // Startup configuration does not become global or persistent state.
        assert_eq!(
            RootManagedPrincipalPolicy::default().role_for(configured),
            PrincipalRole::LocalUser
        );
    }
}
