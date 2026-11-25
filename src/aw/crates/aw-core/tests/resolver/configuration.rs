//! Configuration-level regressions for selectors and command Hook protocols.

use super::{base_document, catalog_capabilities, enable_event, parse_config, CONFIGURATION};
use aw_core::{
    admission::preflight,
    resolver::{
        resolve_configuration, resolve_shadow_preflight, source_from_adapter, ResolveError,
    },
};
use serde_json::json;

#[test]
fn qoder_selectors_at_the_unicode_character_limit_preserve_admission() {
    let caps = catalog_capabilities("qoder");
    let selector = format!("native:qoder:{}", "汉".repeat(256 - "native:qoder:".len()));
    for pointer in [
        "/spec/events/tool.before",
        "/spec/events/tool.before/steps/0",
    ] {
        let mut doc = base_document();
        let tools = json!({"tools": [selector]});
        doc.pointer_mut(pointer).unwrap()["match"] = tools;
        let config = parse_config(&doc);
        let expected = preflight(&config, "qoder", &caps).unwrap();
        let (plan, actual) = resolve_shadow_preflight(&config, "qoder", &caps).unwrap();
        assert_eq!(actual, expected);
        assert_eq!(plan.hook("tool.before").unwrap().steps.len(), 1);

        doc.pointer_mut(pointer).unwrap()["match"]["tools"][0] = json!(format!("{selector}汉"));
        assert!(CONFIGURATION
            .parse(&serde_json::to_vec(&doc).unwrap())
            .is_err());
    }
}

#[test]
fn boolean_hook_replacements_are_invalid_even_with_a_capable_source() {
    let caps = catalog_capabilities("qoder");
    let source = source_from_adapter(&caps).unwrap();
    for (event, effect, on_error) in [
        ("tool.after", "replace_result", "report"),
        ("tool.before", "replace_input", "block"),
    ] {
        assert!(caps.events[event].iter().any(|name| name == effect));
        let mut doc = base_document();
        doc["spec"]["events"][event]["steps"] = json!([{
            "id": "replace", "provider": "boolean", "effects": [effect], "on_error": on_error
        }]);
        for required in [true, false] {
            doc["spec"]["events"][event]["required"] = json!(required);
            for default_protocol in [false, true] {
                let mut policy = doc.clone();
                if default_protocol {
                    policy["spec"]["providers"]["boolean"]
                        .as_object_mut()
                        .unwrap()
                        .remove("protocol");
                }
                let config = parse_config(&policy);
                assert!(matches!(
                    resolve_configuration(&config, "qoder", std::slice::from_ref(&source)),
                    Err(ResolveError::Requirement(
                        "boolean Hooks cannot replace input or results"
                    ))
                ));
            }
        }
    }
}

#[test]
fn mode_off_step_with_tool_selector_on_non_tool_event_matches_preflight() {
    let caps = catalog_capabilities("qoder");
    for explicit_enabled in [None, Some(true)] {
        let mut doc = base_document();
        let mut step = json!({
            "id": "disabled-by-mode",
            "provider": "policy",
            "operation": "check",
            "effects": ["observe"],
            "mode": "off",
            "on_error": "report",
            "match": {"tools": ["native:qoder:read"]},
        });
        if let Some(enabled) = explicit_enabled {
            step["enabled"] = json!(enabled);
        }
        enable_event(&mut doc, "session.start", false, json!([step]));

        let config = parse_config(&doc);
        let expected = preflight(&config, "qoder", &caps).unwrap();
        let (_, actual) = resolve_shadow_preflight(&config, "qoder", &caps).unwrap();
        assert_eq!(actual, expected);

        // Turning the step back on with a tool selector on a non-tool event is
        // rejected at configuration validation.
        for active_mode in ["shadow", "enforce"] {
            doc["spec"]["events"]["session.start"]["steps"][0]["mode"] = json!(active_mode);
            assert!(CONFIGURATION
                .parse(&serde_json::to_vec(&doc).unwrap())
                .is_err());
        }
    }
}
