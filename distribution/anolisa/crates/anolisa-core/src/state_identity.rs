//! Repairs narrowly identified historical component-name mistakes.

use crate::domain::{Installation, NativePm, PackageIdentity, ProviderBinding};
use crate::state::ObjectKind;
use crate::transaction::Transaction;

const LEGACY_COSH_NAME: &str = "cosh";
const COSH_NG_NAME: &str = "cosh-ng";

/// Repairs the historical `cosh` identity emitted by the `cosh-ng` RPM.
///
/// The backing RPM name is the disambiguating evidence: legitimate
/// `copilot-shell` installations remain `cosh`. A conflicting canonical
/// record, or duplicate legacy claims in the same scope, is left untouched
/// so loading state never guesses how to merge records.
pub(crate) fn repair_known_installation_identities(installations: &mut [Installation]) {
    let candidates = installations
        .iter()
        .enumerate()
        .filter_map(|(index, installation)| {
            is_legacy_cosh_ng_identity(installation).then_some((index, installation.scope))
        })
        .collect::<Vec<_>>();

    for (index, scope) in &candidates {
        let competing_candidate = candidates
            .iter()
            .any(|(other_index, other_scope)| other_index != index && other_scope == scope);
        let canonical_exists =
            installations
                .iter()
                .enumerate()
                .any(|(other_index, installation)| {
                    other_index != *index
                        && installation.kind == ObjectKind::Component
                        && installation.name == COSH_NG_NAME
                        && installation.scope == *scope
                });
        if !competing_candidate && !canonical_exists {
            installations[*index].name = COSH_NG_NAME.to_string();
        }
    }
}

/// Repairs pending journals written before the `cosh-ng` state rename.
///
/// The delegated RPM package is durable recovery evidence for the renamed
/// subject. Journals for the unrelated `copilot-shell` package remain `cosh`.
pub(crate) fn repair_known_transaction_identity(transaction: &mut Transaction) {
    if transaction.subject.as_deref() == Some(LEGACY_COSH_NAME)
        && matches!(
            &transaction.delegated_recovery,
            Some(context)
                if context.pm == NativePm::Rpm
                    && context.package.as_deref() == Some(COSH_NG_NAME)
        )
    {
        transaction.subject = Some(COSH_NG_NAME.to_string());
    }
}

fn is_legacy_cosh_ng_identity(installation: &Installation) -> bool {
    installation.kind == ObjectKind::Component
        && installation.name == LEGACY_COSH_NAME
        && matches!(
            &installation.binding,
            ProviderBinding::Delegated {
                pm: NativePm::Rpm,
                package: PackageIdentity::Resolved { name },
                ..
            } if name == COSH_NG_NAME
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{InstallationScope, LifecycleStatus, ManagementRelation};

    fn delegated_component(name: &str, package: &str, scope: InstallationScope) -> Installation {
        Installation {
            kind: ObjectKind::Component,
            name: name.to_string(),
            scope,
            binding: ProviderBinding::Delegated {
                pm: NativePm::Rpm,
                package: PackageIdentity::Resolved {
                    name: package.to_string(),
                },
                relation: ManagementRelation::Managed {
                    since: "2026-07-01T00:00:00Z".to_string(),
                },
                last_observed: None,
            },
            status: LifecycleStatus::Installed,
            installed_at: "2026-07-01T00:00:00Z".to_string(),
            last_operation_id: None,
            subscription_scope: Default::default(),
            enabled_features: Vec::new(),
            health: Vec::new(),
        }
    }

    #[test]
    fn repairs_cosh_record_backed_by_cosh_ng_rpm() {
        let mut installations = vec![delegated_component(
            LEGACY_COSH_NAME,
            COSH_NG_NAME,
            InstallationScope::System,
        )];

        repair_known_installation_identities(&mut installations);

        assert_eq!(installations[0].name, COSH_NG_NAME);
    }

    #[test]
    fn preserves_legitimate_copilot_shell_identity() {
        let mut installations = vec![delegated_component(
            LEGACY_COSH_NAME,
            "copilot-shell",
            InstallationScope::System,
        )];

        repair_known_installation_identities(&mut installations);

        assert_eq!(installations[0].name, LEGACY_COSH_NAME);
    }

    #[test]
    fn leaves_conflicting_canonical_record_untouched() {
        let mut installations = vec![
            delegated_component(LEGACY_COSH_NAME, COSH_NG_NAME, InstallationScope::System),
            delegated_component(COSH_NG_NAME, COSH_NG_NAME, InstallationScope::System),
        ];

        repair_known_installation_identities(&mut installations);

        assert_eq!(installations[0].name, LEGACY_COSH_NAME);
        assert_eq!(installations[1].name, COSH_NG_NAME);
    }

    #[test]
    fn repairs_each_scope_independently() {
        let mut installations = vec![
            delegated_component(LEGACY_COSH_NAME, COSH_NG_NAME, InstallationScope::System),
            delegated_component(
                LEGACY_COSH_NAME,
                COSH_NG_NAME,
                InstallationScope::User { uid: 1000 },
            ),
        ];

        repair_known_installation_identities(&mut installations);

        assert!(
            installations
                .iter()
                .all(|installation| installation.name == COSH_NG_NAME)
        );
    }
}
