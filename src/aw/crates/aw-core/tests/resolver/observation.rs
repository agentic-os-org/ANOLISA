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

#[test]
fn both_replacement_slots_agree_with_runtime_on_either_side_of_shadow() {
    let caps = catalog_capabilities("qoder");
    let native = source_from_adapter(&caps).unwrap();
    for (event, effect, failure) in [
        ("tool.before", "replace_input", "block"),
        ("tool.after", "replace_result", "report"),
    ] {
        let sight = make_source(
            "sight-observer",
            Sight,
            "qoder",
            &[(event, hook_cap(ObserveMode, &[Observe], &[]))],
        );
        let sources = [native.clone(), sight];
        let replacement = step("replacement", "policy", &[effect], failure);
        let mut shadow = step("shadow", "policy", &["observe"], "report");
        shadow["mode"] = json!("shadow");
        for shadow_first in [false, true] {
            let pair = if shadow_first {
                json!([shadow.clone(), replacement.clone()])
            } else {
                json!([replacement.clone(), shadow.clone()])
            };
            let mut doc = base_document();
            enable_event(&mut doc, event, true, pair);
            let config = parse_config(&doc);
            let admitted = preflight(&config, "qoder", &caps).unwrap();
            let (_, resolved_steps) = resolve_shadow_preflight(&config, "qoder", &caps).unwrap();
            assert_eq!(
                admitted, resolved_steps,
                "{event} shadow_first={shadow_first}"
            );
            let plan = resolve_configuration(&config, "qoder", &sources).unwrap();
            let hook = plan.hook(event).unwrap();
            assert_eq!(hook.steps.len(), 2);
            assert_eq!(hook.steps[usize::from(!shadow_first)].effects, [Observe]);
            assert!(hook.steps[usize::from(!shadow_first)].shadow);
            assert!(hook.steps.iter().all(|step| step.dispatch_owner == "qoder"));
            assert_eq!(hook.observation_sources[0].source_id, "sight-observer");
            assert!(!hook.observation_sources[0].dispatches_steps);
            let serialized = serde_json::to_vec(&plan).unwrap();
            assert_eq!(
                aw_contracts::hook_plan::ResolvedHookPlan::parse(&serialized).unwrap(),
                plan
            );
        }
    }
}
