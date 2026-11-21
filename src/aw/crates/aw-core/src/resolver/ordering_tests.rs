//! Replacement ordering and observation-only steps.

use super::*;

#[test]
fn replacement_order_only_counts_enforcing_steps() {
    for (event, effect, failure) in [
        (
            "tool.before",
            HookEffect::ReplaceInput,
            FailureAction::Block,
        ),
        (
            "tool.after",
            HookEffect::ReplaceResult,
            FailureAction::Report,
        ),
    ] {
        let source = SourceCapability::new(
            "native",
            SourceKind::Native,
            "qoder",
            "test",
            "native",
            BTreeMap::from([(
                event.to_owned(),
                HookPointCapability {
                    mode: HookAuthority::Control,
                    effects: vec![HookEffect::Observe, effect],
                    failure_actions: vec![],
                },
            )]),
        )
        .unwrap();
        let replacement = ResolvedStepPlan {
            step_id: "replace".into(),
            provider: "policy".into(),
            effects: vec![effect],
            on_error: failure,
            step_tools: vec!["*".into()],
            native: false,
            shadow: false,
            dispatch_owner: "native".into(),
        };
        let shadow = ResolvedStepPlan {
            step_id: "observe".into(),
            effects: vec![HookEffect::Observe],
            on_error: FailureAction::Report,
            shadow: true,
            ..replacement.clone()
        };
        let enforce = ResolvedStepPlan {
            shadow: false,
            on_error: failure,
            ..shadow.clone()
        };
        let sources = &[source];
        check_step_ordering(event, &[replacement.clone(), shadow.clone()], sources).unwrap();
        check_step_ordering(event, &[shadow, replacement.clone()], sources).unwrap();
        let misordered = if effect == HookEffect::ReplaceInput {
            [enforce, replacement]
        } else {
            [replacement, enforce]
        };
        assert!(check_step_ordering(event, &misordered, sources).is_err());
    }
}
