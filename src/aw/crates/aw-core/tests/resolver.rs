//! Pure Hook Source resolution and Shadow Plan equivalence against static admission.

#[path = "resolver/configuration.rs"]
mod configuration;
#[path = "resolver/observation.rs"]
mod observation;

use aw_config::Validator;
use aw_core::{
    admission::{preflight, AdapterCapabilities, AdmissionError},
    resolver::{
        project_capability_cells, resolve, resolve_configuration, resolve_shadow_preflight,
        source_from_adapter, CapabilityStatus, CoverageGapReason, CoverageGapStatus, FailureAction,
        HookAuthority, HookEffect, HookPointCapability, ResolveError, SourceCapability, SourceKind,
        HOOK_EVENTS, MAX_CANDIDATE_SOURCES,
    },
};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::LazyLock,
};
use HookAuthority::{Control, Observe as ObserveMode};
use HookEffect::{Block, Observe, ReplaceInput, ReplaceResult};
use SourceKind::{Js, Native, Python, Sight};

static CONFIGURATION: LazyLock<Validator> = LazyLock::new(|| Validator::new().unwrap());

fn matrix_capabilities() -> BTreeMap<String, AdapterCapabilities> {
    let matrix: Value =
        serde_json::from_str(include_str!("../../../tests/regression-matrix.json")).unwrap();
    let schema: Value = serde_json::from_str(include_str!(
        "../../aw-config/schemas/configuration-v1alpha1.schema.json"
    ))
    .unwrap();
    let expected: BTreeSet<&str> = schema["$defs"]["agent"]["properties"]["adapter"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    let profiles: BTreeMap<String, AdapterCapabilities> = matrix["agents"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(adapter, row)| {
            let events = row["events"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(e, fx)| (e.clone(), fx.as_object().unwrap().keys().cloned().collect()))
                .collect();
            let caps = AdapterCapabilities {
                post_permission: false,
                adapter: adapter.clone(),
                version: "catalog/v1".into(),
                entrypoint: "native".into(),
                events,
                failure_actions: BTreeMap::new(),
            };
            (adapter.clone(), caps)
        })
        .collect();
    assert_eq!(
        profiles.keys().map(String::as_str).collect::<BTreeSet<_>>(),
        expected
    );
    profiles
}

fn catalog_capabilities(adapter: &str) -> AdapterCapabilities {
    matrix_capabilities().remove(adapter).unwrap()
}

fn deployment_profile(
    spec: (&str, &str, &str, bool, bool, bool),
    input_enabled: bool,
) -> AdapterCapabilities {
    let (adapter, ver, ep, before_block_only, after_obs_only, withhold) = spec;
    let mut c = catalog_capabilities(adapter);
    c.version = ver.into();
    c.entrypoint = ep.into();
    if before_block_only {
        c.events
            .insert("tool.before".into(), vec!["observe".into(), "block".into()]);
    }
    if after_obs_only {
        c.events.insert("tool.after".into(), vec!["observe".into()]);
    }
    if withhold {
        c.failure_actions
            .insert("tool.after".into(), vec!["withhold_result".into()]);
    }
    if !input_enabled {
        c.events.remove("input.submit");
    }
    c
}

fn base_document() -> Value {
    let mut doc = CONFIGURATION
        .parse(include_bytes!("../../aw-config/examples/aw.minimal.yaml"))
        .unwrap()
        .as_value()
        .clone();
    for adapter in matrix_capabilities().keys() {
        doc["spec"]["agents"][adapter.as_str()] =
            json!({"adapter": adapter, "argv": ["/does-not-exist/agent", "--fixture"]});
    }
    let stdio = |argv: &str| json!({"type": "stdio", "argv": [argv], "location": "agent"});
    doc["spec"]["providers"]["policy"] = json!({
        "protocol": "aw-provider/v1alpha1", "transport": stdio("/does-not-exist/policy"),
        "timeout_ms": 250, "max_output_bytes": 4096, "config": {"threshold": 0.75}
    });
    doc["spec"]["providers"]["boolean"] = json!({
        "protocol": "aw-hook/v1alpha1", "transport": stdio("/does-not-exist/hook"),
        "timeout_ms": 250, "max_output_bytes": 4096
    });
    doc["spec"]["providers"]["native"] = json!({
        "protocol": "native-hook/v1alpha1", "transport": stdio("/does-not-exist/native"),
        "timeout_ms": 250, "max_output_bytes": 4096, "config": {}
    });
    for (event, op, effects, on_error) in [
        ("tool.before", "check", json!(["observe", "block"]), "block"),
        ("tool.after", "record", json!(["observe"]), "report"),
    ] {
        doc["spec"]["events"][event]["steps"] = json!([{
            "id": op, "provider": "policy", "operation": op, "effects": effects, "on_error": on_error
        }]);
    }
    doc
}

fn step(id: &str, provider: &str, effects: &[&str], on_error: &str) -> Value {
    json!({"id": id, "provider": provider, "operation": id, "effects": effects, "on_error": on_error})
}

fn enable_event(doc: &mut Value, event: &str, required: bool, steps: Value) {
    doc["spec"]["events"][event] = json!({"enabled": true, "required": required, "steps": steps});
}

fn parse_config(doc: &Value) -> aw_config::Configuration {
    CONFIGURATION
        .parse(&serde_json::to_vec(doc).unwrap())
        .unwrap()
}

fn ctrl(effects: &[HookEffect]) -> HookPointCapability {
    hook_cap(Control, effects, &[])
}

fn hook_cap(
    mode: HookAuthority,
    effects: &[HookEffect],
    failure_actions: &[FailureAction],
) -> HookPointCapability {
    HookPointCapability {
        post_permission: false,
        mode,
        effects: effects.to_vec(),
        failure_actions: failure_actions.to_vec(),
    }
}

fn make_source(
    source_id: &str,
    kind: SourceKind,
    adapter: &str,
    hooks: &[(&str, HookPointCapability)],
) -> SourceCapability {
    let map = hooks
        .iter()
        .map(|(e, c)| ((*e).to_owned(), c.clone()))
        .collect();
    SourceCapability::new(source_id, kind, adapter, "contract/v1", "entrypoint", map).unwrap()
}

fn reason(error: AdmissionError) -> &'static str {
    match error {
        AdmissionError::Rejected(reason) => reason,
        AdmissionError::Configuration(error) => panic!("unexpected configuration error: {error}"),
    }
}

fn assert_shadow_equivalence(doc: &Value, target: &str, capabilities: &AdapterCapabilities) {
    let config = parse_config(doc);
    let expected = preflight(&config, target, capabilities).map_err(reason);
    let actual = resolve_shadow_preflight(&config, target, capabilities).map_err(reason);
    match (actual, expected) {
        (Ok((plan, shadow_steps)), Ok(preflight_steps)) => {
            assert_eq!(shadow_steps, preflight_steps, "{target}");
            assert!(plan.coverage().is_complete(), "{target}");
            assert_eq!(
                (plan.target(), plan.adapter()),
                (target, capabilities.adapter.as_str())
            );
            for point in plan.hooks().values() {
                let owner = point.control_owner.as_ref().unwrap();
                assert_eq!(
                    (owner.source_id.as_str(), owner.kind, owner.dispatches_steps),
                    (capabilities.adapter.as_str(), Native, true)
                );
                assert!(point.observation_sources.is_empty());
                assert!(point
                    .steps
                    .iter()
                    .all(|s| s.dispatch_owner == capabilities.adapter));
            }
        }
        (Err(actual_reason), Err(expected_reason)) => {
            assert_eq!(actual_reason, expected_reason, "{target}");
        }
        (left, right) => panic!("Shadow Plan diverged for {target}: {left:?} vs {right:?}"),
    }
}

#[test]
fn native_hook_wins_over_language_and_sight_sources_regardless_of_input_order() {
    let mut doc = base_document();
    let obs_step = json!([step("obs-model", "policy", &["observe"], "report")]);
    enable_event(&mut doc, "model.before_request", false, obs_step);
    let config = parse_config(&doc);
    let obs = hook_cap(ObserveMode, &[Observe], &[]);
    let withhold = [FailureAction::WithholdResult];
    let after_withhold = hook_cap(Control, &[Observe, ReplaceResult], &withhold);
    let h_nat = [
        ("tool.before", ctrl(&[Observe, Block, ReplaceInput])),
        ("tool.after", after_withhold),
    ];
    let native = make_source("qwenpaw-native", Native, "qwenpaw", &h_nat);
    let h_py = [
        ("tool.before", ctrl(&[Observe, Block])),
        ("model.before_request", obs.clone()),
    ];
    let python = make_source("python-wrapper", Python, "qwenpaw", &h_py);
    let h_sight = [
        ("tool.before", obs.clone()),
        ("tool.after", obs.clone()),
        ("model.before_request", obs),
    ];
    let sight = make_source("sight-observer", Sight, "qwenpaw", &h_sight);

    let s_native = [native.clone(), python.clone(), sight.clone()];
    let s_reverse = [sight.clone(), python.clone(), native.clone()];
    let forward = resolve_configuration(&config, "qwenpaw", &s_native).unwrap();
    let reverse = resolve_configuration(&config, "qwenpaw", &s_reverse).unwrap();
    let middle = resolve_configuration(&config, "qwenpaw", &[python, sight, native]).unwrap();
    assert_eq!((&forward, &forward), (&reverse, &middle));
    assert_eq!(
        forward.to_canonical_bytes().unwrap(),
        reverse.to_canonical_bytes().unwrap()
    );
    assert!(forward.coverage().is_complete());

    let before = forward.hook("tool.before").unwrap();
    let before_owner = &before.control_owner.as_ref().unwrap().source_id;
    assert_eq!(before_owner, "qwenpaw-native");
    let before_obs: Vec<_> = before
        .observation_sources
        .iter()
        .map(|s| (s.source_id.as_str(), s.dispatches_steps))
        .collect();
    assert_eq!(
        before_obs,
        [("python-wrapper", false), ("sight-observer", false)]
    );
    assert_eq!(before.steps[0].dispatch_owner, "qwenpaw-native");

    let model = forward.hook("model.before_request").unwrap();
    assert!(model.control_owner.is_none());
    let model_obs: Vec<_> = model
        .observation_sources
        .iter()
        .map(|s| (s.source_id.as_str(), s.dispatches_steps))
        .collect();
    assert_eq!(
        model_obs,
        [("python-wrapper", true), ("sight-observer", false)]
    );
    assert_eq!(model.steps[0].dispatch_owner, "python-wrapper");
}

#[test]
fn language_control_source_fills_in_when_native_hook_lacks_required_effect() {
    let mut doc = base_document();
    doc["spec"]["events"]["tool.after"]["steps"] = json!([
        step("audit", "policy", &["observe"], "report"),
        step("compress", "policy", &["replace_result"], "report")
    ]);
    let config = parse_config(&doc);
    let hooks_pub = [
        ("tool.before", ctrl(&[Observe, Block])),
        ("tool.after", ctrl(&[Observe])),
    ];
    let native_public = make_source("qoder-public-native", Native, "qoder-sdk", &hooks_pub);

    let err = resolve_configuration(&config, "qoder-sdk", std::slice::from_ref(&native_public))
        .unwrap_err();
    assert!(matches!(
        err,
        ResolveError::UnsatisfiedRequired {
            ref event,
            reason: CoverageGapReason::UnsupportedEffect,
            ref missing_effects,
            ..
        } if event == "tool.after" && missing_effects == &[ReplaceResult]
    ));

    let hooks_js = [("tool.after", ctrl(&[Observe, ReplaceResult]))];
    let js_final_gate = make_source("js-final-gate", Js, "qoder-sdk", &hooks_js);
    let sources = [native_public.clone(), js_final_gate.clone()];
    let plan = resolve_configuration(&config, "qoder-sdk", &sources).unwrap();
    assert!(plan.coverage().is_complete());
    let after = plan.hook("tool.after").unwrap();
    let owner = after.control_owner.as_ref().unwrap();
    assert_eq!(
        (owner.source_id.as_str(), owner.kind, owner.dispatches_steps),
        ("js-final-gate", Js, true)
    );
    assert_eq!(after.observation_sources.len(), 1);
    let obs0 = &after.observation_sources[0];
    assert_eq!(
        (obs0.source_id.as_str(), obs0.dispatches_steps),
        ("qoder-public-native", false)
    );
    assert_eq!(after.steps.len(), 2);
    assert!(after
        .steps
        .iter()
        .all(|s| s.dispatch_owner == "js-final-gate"));

    // Observe-only tool.after with higher-priority Python observe + lower-priority JS control succeeds.
    let h_py_obs = [
        ("tool.before", ctrl(&[Observe, Block])),
        ("tool.after", hook_cap(ObserveMode, &[Observe], &[])),
    ];
    let py_obs = make_source("python-monitoring", Python, "qoder-sdk", &h_py_obs);
    let mixed_sources = [py_obs, js_final_gate];
    for required in [true, false] {
        let mut obs_only_doc = base_document();
        obs_only_doc["spec"]["events"]["tool.after"]["required"] = json!(required);
        let obs_cfg = parse_config(&obs_only_doc);
        let mixed = resolve_configuration(&obs_cfg, "qoder-sdk", &mixed_sources).unwrap();
        let pt = mixed.hook("tool.after").unwrap();
        assert!(pt.control_owner.is_none());
        let obs_list: Vec<_> = pt
            .observation_sources
            .iter()
            .map(|s| (s.source_id.as_str(), s.mode, s.dispatches_steps))
            .collect();
        assert_eq!(
            obs_list,
            [
                ("python-monitoring", ObserveMode, true),
                ("js-final-gate", Control, false)
            ]
        );
    }
}

#[test]
fn single_control_owner_invariant_rejects_split_effects_across_two_sources() {
    let mut doc = base_document();
    doc["spec"]["events"]["tool.before"]["steps"] = json!([
        step("rewrite", "policy", &["replace_input"], "block"),
        step("scan", "policy", &["observe", "block"], "block")
    ]);
    let config = parse_config(&doc);
    let hooks_blk = [
        ("tool.before", ctrl(&[Observe, Block])),
        ("tool.after", ctrl(&[Observe])),
    ];
    let native_block = make_source("qoder-native-block", Native, "qoder", &hooks_blk);
    let hooks_rw = [("tool.before", ctrl(&[Observe, ReplaceInput]))];
    let js_rewrite = make_source("qoder-js-rewrite", Js, "qoder", &hooks_rw);

    let err = resolve_configuration(&config, "qoder", &[native_block, js_rewrite]).unwrap_err();
    assert!(matches!(
        err,
        ResolveError::UnsatisfiedRequired {
            ref event,
            reason: CoverageGapReason::UnsupportedEffect,
            ref missing_effects,
            ..
        } if event == "tool.before" && missing_effects == &[ReplaceInput]
    ));
}

#[test]
fn optional_missing_capabilities_and_candidate_source_limits_are_enforced() {
    let mut doc = base_document();
    let native_step =
        json!([{"id": "native-before", "provider": "native", "on_error": "report", "native": {}}]);
    enable_event(&mut doc, "tool.before", false, native_step);
    let withhold_step = json!([step(
        "withhold-step",
        "policy",
        &["observe"],
        "withhold_result"
    )]);
    enable_event(&mut doc, "tool.after", false, withhold_step);
    enable_event(
        &mut doc,
        "session.start",
        false,
        json!([step("s", "policy", &["observe"], "report")]),
    );
    enable_event(
        &mut doc,
        "input.submit",
        true,
        json!([step("i", "policy", &["observe", "block"], "block")]),
    );
    let config = parse_config(&doc);

    let h_qoder = [
        ("input.submit", ctrl(&[Observe, Block])),
        ("tool.after", ctrl(&[Observe, ReplaceResult])),
    ];
    let native = make_source("qoder-native", Native, "qoder", &h_qoder);
    let h_probe = [("tool.before", ctrl(&[Observe, Block]))];
    let python = make_source("python-before-probe", Python, "qoder", &h_probe);

    let plan = resolve_configuration(&config, "qoder", &[native, python]).unwrap();
    assert!(!plan.coverage().is_complete());
    assert_eq!(plan.hooks().len(), 1);
    assert!(plan.hook("input.submit").is_some());

    let gaps = &plan.coverage().gaps;
    assert_eq!(gaps.len(), 3);
    let (uncov, deg) = (CoverageGapStatus::Uncovered, CoverageGapStatus::Degraded);
    let (r_evt, r_act, r_nat) = (
        CoverageGapReason::UnsupportedEvent,
        CoverageGapReason::UnsupportedFailureAction,
        CoverageGapReason::UnsupportedNativeStep,
    );
    assert_eq!(
        (&gaps[0].event[..], gaps[0].status, gaps[0].reason),
        ("session.start", uncov, r_evt)
    );
    assert_eq!(
        (&gaps[1].event[..], gaps[1].status, gaps[1].reason),
        ("tool.after", deg, r_act)
    );
    assert_eq!(
        gaps[1].missing_failure_actions,
        vec![FailureAction::WithholdResult]
    );
    assert_eq!(gaps[1].fallback_sources, vec!["qoder-native"]);
    assert_eq!(
        (&gaps[2].event[..], gaps[2].status, gaps[2].reason),
        ("tool.before", deg, r_nat)
    );
    assert_eq!(gaps[2].fallback_sources, vec!["python-before-probe"]);

    let obs = hook_cap(ObserveMode, &[Observe], &[]);
    let h_nat0 = [
        ("tool.before", ctrl(&[Observe, Block])),
        ("tool.after", ctrl(&[Observe])),
    ];
    let mut max_sources = vec![make_source("native-0", Native, "qoder", &h_nat0)];
    for index in 1..MAX_CANDIDATE_SOURCES {
        let h_s = [("tool.before", obs.clone()), ("tool.after", obs.clone())];
        max_sources.push(make_source(
            &format!("sight-{index:02}"),
            Sight,
            "qoder",
            &h_s,
        ));
    }
    let min_config = parse_config(&base_document());
    let at_limit = resolve_configuration(&min_config, "qoder", &max_sources).unwrap();
    let obs_len = at_limit
        .hook("tool.before")
        .unwrap()
        .observation_sources
        .len();
    assert_eq!(obs_len, MAX_CANDIDATE_SOURCES - 1);
    max_sources.push(make_source(
        "sight-overflow",
        Sight,
        "qoder",
        &[("tool.before", obs)],
    ));
    assert!(matches!(
        resolve_configuration(&min_config, "qoder", &max_sources),
        Err(ResolveError::SourceSet(
            "candidate source set exceeds maximum capacity"
        ))
    ));
}

#[test]
fn shadow_plan_matches_preflight_across_all_registered_framework_profiles() {
    for (adapter, caps) in matrix_capabilities() {
        let source = source_from_adapter(&caps).unwrap();
        let cells = project_capability_cells(&adapter, &[source]).unwrap();
        assert_eq!(cells.len(), 16);
        for event in HOOK_EVENTS {
            assert_eq!(
                cells[event].status == CapabilityStatus::Supported,
                caps.events.contains_key(event),
                "{adapter}/{event}"
            );
        }

        let base = base_document();
        assert_shadow_equivalence(&base, &adapter, &caps);

        let mut with_input = base.clone();
        enable_event(
            &mut with_input,
            "input.submit",
            true,
            json!([step("in", "policy", &["observe", "block"], "block")]),
        );
        assert_shadow_equivalence(&with_input, &adapter, &caps);

        let rewrite = step("rw", "policy", &["replace_input"], "block");
        let scan = base["spec"]["events"]["tool.before"]["steps"][0].clone();
        let mixed = step("mx", "policy", &["replace_input", "block"], "block");
        for required in [true, false] {
            for steps in [
                json!([rewrite.clone(), scan.clone()]),
                json!([scan.clone(), rewrite.clone()]),
                json!([mixed.clone()]),
            ] {
                let mut doc = base.clone();
                doc["spec"]["events"]["tool.before"]["required"] = json!(required);
                doc["spec"]["events"]["tool.before"]["steps"] = steps;
                assert_shadow_equivalence(&doc, &adapter, &caps);
            }
        }

        let replace_after = step("cmp", "policy", &["replace_result"], "report");
        let observe_after = base["spec"]["events"]["tool.after"]["steps"][0].clone();
        for steps in [
            json!([observe_after.clone(), replace_after.clone()]),
            json!([replace_after, observe_after]),
        ] {
            let mut doc = base.clone();
            doc["spec"]["events"]["tool.after"]["steps"] = steps;
            assert_shadow_equivalence(&doc, &adapter, &caps);
        }

        let mut withhold_doc = base.clone();
        withhold_doc["spec"]["events"]["tool.after"]["steps"][0]["on_error"] =
            json!("withhold_result");
        assert_shadow_equivalence(&withhold_doc, &adapter, &caps);
        let mut withhold_caps = caps.clone();
        withhold_caps
            .failure_actions
            .insert("tool.after".into(), vec!["withhold_result".into()]);
        assert_shadow_equivalence(&withhold_doc, &adapter, &withhold_caps);

        let mut security_doc = base.clone();
        enable_event(
            &mut security_doc,
            "security.violation",
            true,
            json!([step("fg", "policy", &["observe", "block"], "block")]),
        );
        assert_shadow_equivalence(&security_doc, &adapter, &caps);

        let mut hook_doc = base.clone();
        hook_doc["spec"]["events"]["tool.before"]["steps"] = json!([
            {"id": "bool-custom", "provider": "boolean", "effects": ["observe", "block"], "on_error": "block", "reason_code": "policy.blocked"},
            {"id": "bool-default", "provider": "boolean", "effects": ["observe"], "on_error": "report"},
            {"id": "native-step", "provider": "native", "on_error": "report", "native": {}}
        ]);
        assert_shadow_equivalence(&hook_doc, &adapter, &caps);

        let mut empty_event_doc = base.clone();
        empty_event_doc["spec"]["events"]["input.submit"] =
            json!({"enabled": true, "required": false, "steps": []});
        assert_shadow_equivalence(&empty_event_doc, &adapter, &caps);

        if adapter != "sdk" {
            let mut unicode_selector_doc = base.clone();
            let own_unicode = format!("native:{adapter}:one\u{2028}two\u{2029}three");
            unicode_selector_doc["spec"]["events"]["tool.before"]["steps"][0]["match"] =
                json!({"tools": [own_unicode]});
            assert_shadow_equivalence(&unicode_selector_doc, &adapter, &caps);
        }

        let mut foreign_selector_doc = base;
        let foreign = if adapter == "openclaw" {
            "native:qoder:custom"
        } else {
            "native:openclaw:custom"
        };
        foreign_selector_doc["spec"]["events"]["tool.before"]["steps"][0]["match"] =
            json!({"tools": [foreign]});
        assert_shadow_equivalence(&foreign_selector_doc, &adapter, &caps);
    }
}

#[test]
fn shadow_plan_matches_preflight_on_sdk_deployment_profiles_and_error_surfaces() {
    let base = base_document();
    let mut with_input = base.clone();
    let in_steps = json!([step("in", "policy", &["observe", "block"], "block")]);
    enable_event(&mut with_input, "input.submit", true, in_steps);

    let specs = [
        (
            "qoder-sdk",
            "public-hooks/v1",
            "sdk-worker",
            true,
            true,
            false,
        ),
        (
            "qwenpaw",
            "installed-native-hooks/v1",
            "app",
            true,
            false,
            true,
        ),
        (
            "claude-sdk",
            "resident-hooks/v1",
            "sdk-worker",
            false,
            false,
            false,
        ),
        ("pi", "worker-hooks/v1", "sdk", false, false, true),
    ];
    for spec in specs {
        for input_enabled in [true, false] {
            let profile = deployment_profile(spec, input_enabled);
            let target = profile.adapter.clone();
            assert_shadow_equivalence(&base, &target, &profile);
            assert_shadow_equivalence(&with_input, &target, &profile);

            let mut replace_result_doc = base.clone();
            replace_result_doc["spec"]["events"]["tool.after"]["steps"][0]["effects"] =
                json!(["replace_result"]);
            assert_shadow_equivalence(&replace_result_doc, &target, &profile);

            let mut withhold_doc = base.clone();
            withhold_doc["spec"]["events"]["tool.after"]["steps"][0]["on_error"] =
                json!("withhold_result");
            assert_shadow_equivalence(&withhold_doc, &target, &profile);
        }
    }

    let qoder_caps = catalog_capabilities("qoder");
    assert_shadow_equivalence(&base, "missing", &qoder_caps);
    assert_shadow_equivalence(&base, "openclaw", &qoder_caps);

    let mut blank_version = qoder_caps.clone();
    blank_version.version = " ".into();
    assert_shadow_equivalence(&base, "qoder", &blank_version);

    let mut guarded = base.clone();
    guarded["spec"]["events"]["tool.before"]["guard"] = json!("security.violation");
    guarded["spec"]["events"]["security.violation"] = json!({"enabled": true, "steps": []});
    assert_shadow_equivalence(&guarded, "qoder", &qoder_caps);

    let mut unsupported_event = base.clone();
    unsupported_event["spec"]["events"]["permission.request"] =
        json!({"enabled": true, "required": false, "steps": []});
    assert_shadow_equivalence(&unsupported_event, "qoder", &qoder_caps);

    let mut bool_replace = base;
    bool_replace["spec"]["events"]["tool.after"]["steps"][0] = json!({
        "id": "record", "provider": "boolean", "effects": ["replace_result"], "on_error": "report"
    });
    assert_shadow_equivalence(&bool_replace, "qoder", &qoder_caps);

    let source = source_from_adapter(&qoder_caps).unwrap();
    assert!(matches!(
        resolve("bad target", "qoder", &[], std::slice::from_ref(&source)),
        Err(ResolveError::Target(_))
    ));
    assert!(matches!(
        resolve("qoder", "openclaw", &[], std::slice::from_ref(&source)),
        Err(ResolveError::SourceSet(_))
    ));
    assert!(matches!(
        resolve("qoder", "qoder", &[], &[source.clone(), source]),
        Err(ResolveError::SourceSet(_))
    ));
}
