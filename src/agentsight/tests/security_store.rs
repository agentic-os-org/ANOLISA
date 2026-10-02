#![cfg(target_os = "linux")]

use agentsight::security::{SecurityCoordinatorError, SecurityStore, SecurityStoreError};
use uuid::Uuid;

#[test]
fn legacy_security_store_constructors_remain_available() {
    let _open_default: fn() -> Result<SecurityStore, SecurityStoreError> =
        SecurityStore::open_default;

    assert_eq!(
        SecurityStore::default_path(),
        agentsight::default_base_path().join("security.db")
    );
}

#[test]
fn legacy_coordinator_errors_remain_matchable() {
    let store_error = SecurityCoordinatorError::Store(SecurityStoreError::Poisoned);
    assert!(matches!(store_error, SecurityCoordinatorError::Store(_)));

    let event_id = Uuid::new_v4();
    let missing = SecurityCoordinatorError::MissingEvidence {
        kind: "source",
        event_id,
    };
    assert!(matches!(
        missing,
        SecurityCoordinatorError::MissingEvidence {
            kind: "source",
            event_id: found,
        } if found == event_id
    ));
}
