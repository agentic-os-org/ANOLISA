//! Compatibility facade for system-audit persistence.

use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use agentsight_audit::{AuditEventFilter, AuditEventPage, AuditStore};
use agentsight_enforcement_protocol::SecurityEvent;
use rusqlite::Connection;
use uuid::Uuid;

pub use agentsight_audit::{
    AuditError as SecurityStoreError, AuditEventStore as SecurityEventStore,
    ContainmentActivationResult, ContainmentClaimResult,
};

#[cfg(target_os = "linux")]
pub(crate) use agentsight_audit::DueContainmentAction;

/// Backward-compatible facade over the shared audit store.
///
/// New code should use [`agentsight_audit::AuditStore`] directly. The facade
/// retains the existing AgentSight constructors while delegating all storage
/// behavior to the independent audit crate.
pub struct SecurityStore {
    inner: Arc<AuditStore>,
}

impl SecurityStore {
    /// Opens the default AgentSight security database.
    ///
    /// # Errors
    ///
    /// Returns a typed open or schema error.
    pub fn open_default() -> Result<Self, SecurityStoreError> {
        super::open_private_store(crate::config::default_base_path())
    }

    /// Opens a security store at `path` and applies additive schema creation.
    ///
    /// # Errors
    ///
    /// Returns a typed open or schema error.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, SecurityStoreError> {
        Ok(Self::from_audit_store(AuditStore::open(path)?))
    }

    /// Opens an isolated in-memory security store.
    ///
    /// # Errors
    ///
    /// Returns a SQLite error when the connection or schema cannot be created.
    pub fn open_in_memory() -> Result<Self, SecurityStoreError> {
        Ok(Self::from_audit_store(AuditStore::open_in_memory()?))
    }

    /// Constructs the compatibility facade from a host-opened connection.
    ///
    /// # Errors
    ///
    /// Returns a typed schema error when initialization fails.
    pub fn from_connection(connection: Connection) -> Result<Self, SecurityStoreError> {
        Ok(Self::from_audit_store(AuditStore::from_connection(
            connection,
        )?))
    }

    /// Returns the default security database path.
    pub fn default_path() -> PathBuf {
        crate::config::default_base_path().join("security.db")
    }

    /// Returns the shared audit-crate persistence boundary.
    pub fn audit_store(&self) -> Arc<AuditStore> {
        Arc::clone(&self.inner)
    }

    fn from_audit_store(store: AuditStore) -> Self {
        Self {
            inner: Arc::new(store),
        }
    }
}

impl Deref for SecurityStore {
    type Target = AuditStore;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl SecurityEventStore for SecurityStore {
    fn insert_event(&self, event: &SecurityEvent) -> Result<bool, SecurityStoreError> {
        self.inner.insert_event(event)
    }

    fn event(&self, event_id: Uuid) -> Result<Option<SecurityEvent>, SecurityStoreError> {
        self.inner.event(event_id)
    }

    fn list_events(&self, filter: &AuditEventFilter) -> Result<AuditEventPage, SecurityStoreError> {
        self.inner.list_events(filter)
    }
}
