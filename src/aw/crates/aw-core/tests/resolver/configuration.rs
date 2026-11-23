//! Configuration-level regressions for selectors and command Hook protocols.

use super::{base_document, catalog_capabilities, parse_config, CONFIGURATION};
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
