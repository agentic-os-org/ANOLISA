use actix_web::body::to_bytes;
use actix_web::http::StatusCode;
use serde_json::Value;
use uuid::Uuid;

use super::containment_error;
use crate::security::{ContainmentError, RiskCaseStatus, SecurityStoreError};

#[actix_web::test]
async fn typed_errors_have_stable_sanitized_mappings() {
    use ContainmentError as E;
    use RiskCaseStatus as R;
    use StatusCode as S;

    let id = Uuid::new_v4();
    let errors = vec![
        (E::MissingCase(id), S::NOT_FOUND, "case_not_found"),
        (
            E::SourcePolicyUnavailable(id),
            S::CONFLICT,
            "source_policy_unavailable",
        ),
        (E::RootProcessStale(7), S::CONFLICT, "root_process_stale"),
        (E::AmbiguousCandidate(7), S::CONFLICT, "ambiguous_candidate"),
        (
            E::IneligibleCase {
                case_id: id,
                status: R::Resolved,
            },
            S::CONFLICT,
            "case_not_eligible",
        ),
        (E::InvalidDuration, S::BAD_REQUEST, "invalid_duration"),
        (E::InvalidRequestedBy, S::BAD_REQUEST, "invalid_requester"),
        (
            E::IncompatibleAction(id),
            S::CONFLICT,
            "incompatible_action",
        ),
        (
            E::ContainmentInProgress(id),
            S::CONFLICT,
            "action_in_progress",
        ),
        (E::ContainmentExpiring(id), S::CONFLICT, "action_expiring"),
        (
            E::CaseEligibilityChanged {
                case_id: id,
                status: R::Resolved,
            },
            S::CONFLICT,
            "case_eligibility_changed",
        ),
        (
            E::CleanupRequired {
                action_id: id,
                binding_id: id,
                reason: "/root/secret.txt".into(),
            },
            S::SERVICE_UNAVAILABLE,
            "cleanup_required",
        ),
        (
            E::Enforcer("token at /run/private.sock".into()),
            S::SERVICE_UNAVAILABLE,
            "enforcer_unavailable",
        ),
        (
            E::Store(SecurityStoreError::InvalidData(
                "database /private/db".into(),
            )),
            S::INTERNAL_SERVER_ERROR,
            "invalid_stored_data",
        ),
        (
            E::Store(SecurityStoreError::Poisoned),
            S::INTERNAL_SERVER_ERROR,
            "security_store_unavailable",
        ),
        (E::AlreadyRunning, S::CONFLICT, "reconciler_already_running"),
        (
            E::ReconcilerThread(std::io::Error::other("/private/thread")),
            S::SERVICE_UNAVAILABLE,
            "reconciler_unavailable",
        ),
        (
            E::RecoveryFailed {
                action_id: id,
                reason: "/root/recovery".into(),
            },
            S::CONFLICT,
            "recovery_failed",
        ),
        (E::ClaimLost(id), S::CONFLICT, "claim_lost"),
        (
            E::CorruptActions { count: 1 },
            S::INTERNAL_SERVER_ERROR,
            "corrupt_actions",
        ),
    ];

    for (error, status, code) in errors {
        let response = containment_error(error);
        assert_eq!(response.status(), status);
        let body = to_bytes(response.into_body())
            .await
            .expect("error body should read");
        let value: Value = serde_json::from_slice(&body).expect("error should be JSON");
        assert_eq!(value["error"]["code"], code);
        let text = String::from_utf8_lossy(&body);
        assert!(!text.contains("/root/"));
        assert!(!text.contains("/private/"));
        assert!(!text.contains("token at"));
    }
}

#[actix_web::test]
async fn recovery_failure_is_a_non_retryable_conflict() {
    let response = containment_error(ContainmentError::RecoveryFailed {
        action_id: Uuid::new_v4(),
        reason: "sensitive recovery detail".into(),
    });

    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = to_bytes(response.into_body())
        .await
        .expect("error body should read");
    let value: Value = serde_json::from_slice(&body).expect("error should be JSON");
    assert_eq!(value["error"]["retryable"], false);
}
