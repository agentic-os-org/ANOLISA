//! Verify that an optional Sight Source cannot take ownership of policy steps.

use super::*;

#[test]
fn sight_cannot_take_over_policy_dispatch_when_the_native_source_is_absent() {
    let mut doc = base_document();
    enable_event(
        &mut doc,
        "tool.after",
        false,
        json!([step("audit", "policy", &["observe"], "report")]),
    );
    let sight = make_source(
        "sight-observer",
        Sight,
        "qoder",
        &[
            ("tool.before", hook_cap(ObserveMode, &[Observe], &[])),
            ("tool.after", hook_cap(ObserveMode, &[Observe], &[])),
        ],
    );
    let native = make_source(
        "qoder-native",
        Native,
        "qoder",
        &[("tool.before", ctrl(&[Observe, Block]))],
    );
    let plan = resolve_configuration(&parse_config(&doc), "qoder", &[native, sight]).unwrap();
    assert!(plan.hook("tool.after").is_none());
    assert_eq!(plan.coverage().gaps[0].event, "tool.after");
    assert_eq!(
        plan.coverage().gaps[0].reason,
        CoverageGapReason::UnsupportedEvent
    );
}
