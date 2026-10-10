//! Shared Policy resource identifiers and immutable revisions.

pub use asc_foundation_types::{ResourceId, Revision};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revision_wire_values_are_positive_and_bounded() {
        assert!(Revision::new(0).is_err());
        let maximum = Revision::new(u32::MAX).unwrap();
        assert_eq!(maximum.get(), u32::MAX);
        assert!(maximum.checked_next().is_err());

        assert!(serde_json::from_str::<Revision>("0").is_err());
        assert!(serde_json::from_str::<Revision>("4294967296").is_err());
    }

    #[test]
    fn shared_foundation_types_keep_one_rust_and_wire_contract() {
        let foundation_id = asc_foundation_types::ResourceId::new("resource:shared-1").unwrap();
        let policy_id: ResourceId = foundation_id;
        let id_wire = serde_json::to_string(&policy_id).unwrap();
        let foundation_id: asc_foundation_types::ResourceId =
            serde_json::from_str(&id_wire).unwrap();
        assert_eq!(foundation_id.as_str(), "resource:shared-1");
        assert!(ResourceId::new("a".repeat(129)).is_err());

        let foundation_revision = asc_foundation_types::Revision::new(7).unwrap();
        let policy_revision: Revision = foundation_revision;
        let revision_wire = serde_json::to_string(&policy_revision).unwrap();
        let foundation_revision: asc_foundation_types::Revision =
            serde_json::from_str(&revision_wire).unwrap();
        assert_eq!(foundation_revision.get(), 7);
    }
}
