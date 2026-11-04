//! Safe SysOM error normalization keeps only allowlisted codes and request identifiers.

use super::super::{DispatchState, ProviderFailure, ProviderFailureKind};
use serde_json::Value;

pub(super) fn from_payload(payload: &[u8], dispatch: DispatchState) -> Option<ProviderFailure> {
    if payload.len() > 64 * 1024 {
        return None;
    }
    let value: Value = serde_json::from_slice(payload).ok()?;
    let candidate = ["code", "Code", "error_code", "errorCode"]
        .iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .or_else(|| {
            value
                .get("error")
                .and_then(|error| error.get("code"))
                .and_then(Value::as_str)
        })?;
    let (code, kind) = match candidate {
        "Sysom.ServiceNotActivated" => (
            "Sysom.ServiceNotActivated",
            ProviderFailureKind::ActivationRequired,
        ),
        "Sysom.CreditsPreparing" => (
            "Sysom.CreditsPreparing",
            ProviderFailureKind::CreditsPreparing,
        ),
        "Sysom.CreditsExhausted" => (
            "Sysom.CreditsExhausted",
            ProviderFailureKind::QuotaExhausted,
        ),
        "SecurityTokenExpired" => (
            "SecurityTokenExpired",
            ProviderFailureKind::CredentialsExpired,
        ),
        "InvalidSecurityToken.Expired" => (
            "InvalidSecurityToken.Expired",
            ProviderFailureKind::CredentialsExpired,
        ),
        "InvalidSecurityToken" => (
            "InvalidSecurityToken",
            ProviderFailureKind::CredentialsExpired,
        ),
        "InvalidAccessKeyId" => (
            "InvalidAccessKeyId",
            ProviderFailureKind::CredentialsExpired,
        ),
        "Sysom.Identity.PermissionDenied" => (
            "Sysom.Identity.PermissionDenied",
            ProviderFailureKind::PermissionDenied,
        ),
        "Forbidden" => ("Forbidden", ProviderFailureKind::PermissionDenied),
        "AccessDenied" => ("AccessDenied", ProviderFailureKind::PermissionDenied),
        "Sysom.Identity.InvalidInstanceProof" => (
            "Sysom.Identity.InvalidInstanceProof",
            ProviderFailureKind::Identity,
        ),
        "Sysom.Identity.ChallengeNotRedeemable" => (
            "Sysom.Identity.ChallengeNotRedeemable",
            ProviderFailureKind::Identity,
        ),
        _ => return None,
    };
    let dispatch = if matches!(
        kind,
        ProviderFailureKind::ActivationRequired
            | ProviderFailureKind::CreditsPreparing
            | ProviderFailureKind::QuotaExhausted
    ) {
        dispatch
    } else {
        DispatchState::Unknown
    };
    let request_id = ["request_id", "requestId", "RequestId"]
        .iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .filter(|id| {
            !id.is_empty()
                && id.len() <= 128
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_:".contains(&b))
        })
        .map(str::to_owned);
    Some(ProviderFailure {
        code: code.into(),
        kind,
        dispatch,
        request_id,
    })
}

pub(super) fn fixed(
    code: &str,
    kind: ProviderFailureKind,
    dispatch: DispatchState,
) -> ProviderFailure {
    ProviderFailure {
        code: code.into(),
        kind,
        dispatch,
        request_id: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_exact_commercial_codes_prove_http_rejection_before_dispatch() {
        for (code, kind) in [
            (
                "Sysom.ServiceNotActivated",
                ProviderFailureKind::ActivationRequired,
            ),
            (
                "Sysom.CreditsPreparing",
                ProviderFailureKind::CreditsPreparing,
            ),
            (
                "Sysom.CreditsExhausted",
                ProviderFailureKind::QuotaExhausted,
            ),
        ] {
            let body = serde_json::to_vec(
                &serde_json::json!({"code":code,"message":"secret-canary","request_id":"req-123"}),
            )
            .unwrap();
            let failure = from_payload(&body, DispatchState::NotDispatched).unwrap();
            assert_eq!(failure.kind, kind);
            assert_eq!(failure.dispatch, DispatchState::NotDispatched);
            assert_eq!(failure.code, code);
            assert!(!format!("{failure:?}").contains("canary"));
            assert_eq!(
                from_payload(&body, DispatchState::Unknown)
                    .unwrap()
                    .dispatch,
                DispatchState::Unknown
            );
        }
    }
    #[test]
    fn permission_and_expired_credentials_never_imply_activation_or_safe_replay() {
        for (code, kind) in [
            (
                "Sysom.Identity.PermissionDenied",
                ProviderFailureKind::PermissionDenied,
            ),
            (
                "SecurityTokenExpired",
                ProviderFailureKind::CredentialsExpired,
            ),
        ] {
            let body = serde_json::to_vec(&serde_json::json!({"Code":code})).unwrap();
            let failure = from_payload(&body, DispatchState::NotDispatched).unwrap();
            assert_eq!(failure.kind, kind);
            assert_eq!(failure.dispatch, DispatchState::Unknown);
        }
        for body in [
            br#"{"code":"sk-secret","message":"not activated"}"#.as_slice(),
            r#"{"message":"服务未开通"}"#.as_bytes(),
            b"garbage",
        ] {
            assert!(from_payload(body, DispatchState::Unknown).is_none());
        }
    }
}
