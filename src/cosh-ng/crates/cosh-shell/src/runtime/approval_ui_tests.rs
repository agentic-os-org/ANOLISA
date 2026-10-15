use std::collections::BTreeMap;
use std::time::Duration;

use crate::runtime::approval_ui::{ActivationError, ApprovalSliceActivation};
use crate::ui::surface_frame::SLICE_COLUMNS;
use crate::ui::worker_protocol::ProtocolLimits;

const POLICY: [(&str, &str); 11] = [
    ("COSH_UI_TEST_MAX_PAYLOAD_BYTES", "1048576"),
    ("COSH_UI_TEST_MAX_DRAW_OPS", "4096"),
    ("COSH_UI_TEST_MAX_TEXT_BYTES", "16384"),
    ("COSH_UI_TEST_MAX_SURFACE_ROWS", "24"),
    ("COSH_UI_TEST_HANDSHAKE_TIMEOUT_MS", "5000"),
    ("COSH_UI_TEST_RENDER_TIMEOUT_MS", "5000"),
    ("COSH_UI_TEST_HEARTBEAT_INTERVAL_MS", "100"),
    ("COSH_UI_TEST_HEARTBEAT_TIMEOUT_MS", "1000"),
    ("COSH_UI_TEST_BACKPRESSURE_TIMEOUT_MS", "5000"),
    ("COSH_UI_TEST_SHUTDOWN_TIMEOUT_MS", "2000"),
    ("COSH_UI_TEST_FAILURE_LIMIT", "3"),
];

fn environment() -> BTreeMap<String, String> {
    let mut env: BTreeMap<String, String> = POLICY
        .into_iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect();
    env.insert("COSH_UI_APPROVAL_SLICE_TEST".to_string(), "1".to_string());
    env.insert(
        "COSH_UI_WORKER_PATH".to_string(),
        "/opt/cosh/bin/cosh-ui-worker".to_string(),
    );
    env.insert("COSH_SHELL_WIDTH".to_string(), "40".to_string());
    env
}

fn activate(
    env: &BTreeMap<String, String>,
) -> Result<Option<ApprovalSliceActivation>, ActivationError> {
    ApprovalSliceActivation::from_lookup(&|key| env.get(key).cloned())
}

#[test]
fn absent_activation_key_keeps_the_inline_path() {
    let mut env = environment();
    env.remove("COSH_UI_APPROVAL_SLICE_TEST");
    assert_eq!(activate(&env).expect("no request, no error"), None);
}

#[test]
fn full_policy_activates_at_the_slice_width() {
    let env = environment();
    let activation = activate(&env)
        .expect("complete policy must activate")
        .expect("activation requested");
    assert_eq!(activation.columns, SLICE_COLUMNS);
    assert_eq!(activation.max_surface_rows, 24);
    assert_eq!(
        activation.limits,
        ProtocolLimits {
            max_payload_bytes: 1_048_576,
            max_draw_ops: 4096,
            max_text_bytes: 16_384,
        }
    );
    assert_eq!(activation.handshake_timeout, Duration::from_millis(5000));
    assert_eq!(activation.heartbeat_interval, Duration::from_millis(100));
    assert_eq!(activation.heartbeat_timeout, Duration::from_millis(1000));
    assert_eq!(activation.failure_limit, 3);
    assert!(activation.worker_path.is_absolute());

    let policy = activation.coordinator_policy();
    assert_eq!(policy.pending_revision_capacity, 1);
    assert_eq!(policy.protocol_limits, activation.limits);
    assert_eq!(policy.failure_limit, activation.failure_limit);
}

/// Activation is all-or-nothing: dropping any single policy key must refuse the
/// whole slice rather than substituting a default, because a default here would
/// be an unapproved production threshold.
#[test]
fn each_missing_policy_key_refuses_activation() {
    for (key, _) in POLICY {
        let mut env = environment();
        env.remove(key);
        assert_eq!(
            activate(&env),
            Err(ActivationError::MissingKey(key)),
            "missing {key} must refuse activation"
        );
    }
    for key in ["COSH_UI_WORKER_PATH", "COSH_SHELL_WIDTH"] {
        let mut env = environment();
        env.remove(key);
        assert_eq!(
            activate(&env),
            Err(ActivationError::MissingKey(key)),
            "missing {key} must refuse activation"
        );
    }
}

#[test]
fn an_empty_policy_value_is_treated_as_missing() {
    let mut env = environment();
    env.insert("COSH_UI_TEST_MAX_DRAW_OPS".to_string(), "  ".to_string());
    assert_eq!(
        activate(&env),
        Err(ActivationError::MissingKey("COSH_UI_TEST_MAX_DRAW_OPS"))
    );
}

#[test]
fn a_value_other_than_one_does_not_activate() {
    let mut env = environment();
    env.insert(
        "COSH_UI_APPROVAL_SLICE_TEST".to_string(),
        "true".to_string(),
    );
    assert_eq!(
        activate(&env),
        Err(ActivationError::MissingKey("COSH_UI_APPROVAL_SLICE_TEST"))
    );
}

#[test]
fn a_relative_worker_path_refuses_activation() {
    let mut env = environment();
    env.insert(
        "COSH_UI_WORKER_PATH".to_string(),
        "target/debug/cosh-ui-worker".to_string(),
    );
    assert_eq!(activate(&env), Err(ActivationError::WorkerPathNotAbsolute));
}

/// The surface is Host-owned at 40 columns; a wider terminal must not silently
/// widen the slice, and a narrower one cannot host the card at all.
#[test]
fn a_width_other_than_the_slice_width_refuses_activation() {
    for width in ["39", "41", "80", "120"] {
        let mut env = environment();
        env.insert("COSH_SHELL_WIDTH".to_string(), width.to_string());
        let parsed: u16 = width.parse().expect("test width");
        assert_eq!(
            activate(&env),
            Err(ActivationError::WidthMismatch(parsed)),
            "width {width} must refuse activation"
        );
    }
}

#[test]
fn a_zero_row_ceiling_refuses_activation() {
    let mut env = environment();
    env.insert("COSH_UI_TEST_MAX_SURFACE_ROWS".to_string(), "0".to_string());
    assert_eq!(
        activate(&env),
        Err(ActivationError::NotU16("COSH_UI_TEST_MAX_SURFACE_ROWS"))
    );
}

#[test]
fn a_zero_timeout_refuses_activation() {
    for key in [
        "COSH_UI_TEST_HANDSHAKE_TIMEOUT_MS",
        "COSH_UI_TEST_RENDER_TIMEOUT_MS",
        "COSH_UI_TEST_HEARTBEAT_INTERVAL_MS",
        "COSH_UI_TEST_HEARTBEAT_TIMEOUT_MS",
        "COSH_UI_TEST_BACKPRESSURE_TIMEOUT_MS",
        "COSH_UI_TEST_SHUTDOWN_TIMEOUT_MS",
    ] {
        let mut env = environment();
        env.insert(key.to_string(), "0".to_string());
        assert_eq!(
            activate(&env),
            Err(ActivationError::ZeroTimeout(key)),
            "{key}=0 must refuse activation"
        );
    }
}

#[test]
fn unparsable_values_refuse_activation() {
    let cases: [(&str, &str, ActivationError); 4] = [
        (
            "COSH_UI_TEST_MAX_PAYLOAD_BYTES",
            "1MiB",
            ActivationError::NotUsize("COSH_UI_TEST_MAX_PAYLOAD_BYTES"),
        ),
        (
            "COSH_UI_TEST_MAX_DRAW_OPS",
            "-1",
            ActivationError::NotUsize("COSH_UI_TEST_MAX_DRAW_OPS"),
        ),
        (
            "COSH_UI_TEST_FAILURE_LIMIT",
            "many",
            ActivationError::NotU64("COSH_UI_TEST_FAILURE_LIMIT"),
        ),
        (
            "COSH_SHELL_WIDTH",
            "wide",
            ActivationError::NotU16("COSH_SHELL_WIDTH"),
        ),
    ];
    for (key, value, expected) in cases {
        let mut env = environment();
        env.insert(key.to_string(), value.to_string());
        assert_eq!(activate(&env), Err(expected), "{key}={value}");
    }
}

/// `Instant + Duration` panics on overflow, and these values reach deadline
/// arithmetic on the controller thread, so an absurd bound is refused at parse
/// time rather than being trusted.
#[test]
fn an_absurd_timeout_refuses_activation() {
    for key in [
        "COSH_UI_TEST_HANDSHAKE_TIMEOUT_MS",
        "COSH_UI_TEST_RENDER_TIMEOUT_MS",
        "COSH_UI_TEST_HEARTBEAT_TIMEOUT_MS",
        "COSH_UI_TEST_BACKPRESSURE_TIMEOUT_MS",
        "COSH_UI_TEST_SHUTDOWN_TIMEOUT_MS",
    ] {
        let mut env = environment();
        env.insert(key.to_string(), u64::MAX.to_string());
        assert_eq!(
            activate(&env),
            Err(ActivationError::TimeoutTooLarge(key)),
            "{key} must be bounded"
        );
    }
    // One hour is accepted, so the bound is not simply "large values are refused".
    let mut env = environment();
    env.insert(
        "COSH_UI_TEST_SHUTDOWN_TIMEOUT_MS".to_string(),
        "3600000".to_string(),
    );
    env.insert(
        "COSH_UI_TEST_HEARTBEAT_TIMEOUT_MS".to_string(),
        "3600000".to_string(),
    );
    assert!(activate(&env).expect("bounded values parse").is_some());
}

/// A heartbeat interval at or above its own timeout would declare every live
/// worker hung, so it is rejected instead of being clamped.
#[test]
fn a_heartbeat_interval_not_below_its_timeout_refuses_activation() {
    for interval in ["1000", "1500"] {
        let mut env = environment();
        env.insert(
            "COSH_UI_TEST_HEARTBEAT_INTERVAL_MS".to_string(),
            interval.to_string(),
        );
        assert_eq!(
            activate(&env),
            Err(ActivationError::HeartbeatNotBelowTimeout),
            "interval {interval} must refuse activation"
        );
    }
}
