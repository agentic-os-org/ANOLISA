//! Display-projection regressions for the Governance panel (issue #2197).
//!
//! Every test here drives the real governance pass first, so the projection is
//! exercised against the same `GovernedEvent` shapes `finish` renders.

use super::governance::{
    govern_agent_events_with_language, project_hook_notifications_for_display,
};
use crate::config::Language;
use crate::i18n::I18n;
use crate::types::{AgentEvent, GovernedEvent, Policy};

const PII_ALLOW_MESSAGE: &str = "[pii-checker] detected 2 sensitive items; type: credit_card; \
     severity: warn; masked sample: [REDACTED_CARD:2603]; the request continues.";

fn hook_event(hook_name: &str, message: &str, decision: Option<&str>) -> AgentEvent {
    AgentEvent::HookNotification {
        run_id: "run-1".to_string(),
        hook_name: hook_name.to_string(),
        message: message.to_string(),
        tool_use_id: Some("tool-1".to_string()),
        decision: decision.map(ToString::to_string),
    }
}

/// Governs `events`, then projects them for display. Returns the governed input
/// (untouched) alongside the projection so tests can assert on both.
fn govern_and_project(
    events: &[AgentEvent],
    language: Language,
) -> (Vec<GovernedEvent>, Vec<GovernedEvent>) {
    let governed = govern_agent_events_with_language(events, &Policy::default(), language);
    let projected = project_hook_notifications_for_display(&governed.events, &I18n::new(language));
    (governed.events, projected)
}

fn display_texts(events: &[GovernedEvent]) -> Vec<&str> {
    events
        .iter()
        .map(|event| event.display_text.as_str())
        .collect()
}

#[test]
fn hook_notification_projection_collapses_identical_allow_notices_with_hit_count() {
    let events = vec![
        hook_event("pii-checker", PII_ALLOW_MESSAGE, Some("allow")),
        hook_event("pii-checker", PII_ALLOW_MESSAGE, Some("allow")),
        hook_event("pii-checker", PII_ALLOW_MESSAGE, Some("allow")),
    ];

    let (_, projected) = govern_and_project(&events, Language::EnUs);

    assert_eq!(projected.len(), 1);
    let line = &projected[0].display_text;
    assert!(line.ends_with(" ×3"), "{line}");
    // Collapsed notices are single-line weak hints: no `Decision: allow` row,
    // and no duplicated `[pii-checker]` prefix beside the hook name.
    assert_eq!(line.lines().count(), 1, "{line}");
    assert!(
        line.starts_with("• pii-checker: detected 2 sensitive items;"),
        "{line}"
    );
    assert!(!line.contains("Decision"), "{line}");
    assert!(!line.contains("[pii-checker]"), "{line}");
    // Digits still distinguish the underlying security hit.
    assert!(line.contains("[REDACTED_CARD:2603]"), "{line}");
}

#[test]
fn hook_notification_projection_omits_hit_count_for_a_single_allow_notice() {
    let events = vec![hook_event("pii-checker", PII_ALLOW_MESSAGE, Some("allow"))];

    let (_, projected) = govern_and_project(&events, Language::EnUs);

    assert_eq!(projected.len(), 1);
    assert!(
        !projected[0].display_text.contains('×'),
        "{:?}",
        projected[0]
    );
}

#[test]
fn hook_notification_projection_keeps_distinct_detection_counts_as_separate_summaries() {
    let two_items = PII_ALLOW_MESSAGE;
    let one_item = "[pii-checker] detected 1 sensitive item; type: credit_card; \
         severity: warn; masked sample: [REDACTED_CARD:2603]; the request continues.";
    let mut events = vec![hook_event("pii-checker", two_items, Some("allow")); 3];
    events.extend(vec![hook_event("pii-checker", one_item, Some("allow")); 9]);

    let (_, projected) = govern_and_project(&events, Language::EnUs);

    assert_eq!(projected.len(), 2);
    let lines = display_texts(&projected);
    assert!(
        lines[0].contains("detected 2 sensitive items") && lines[0].ends_with(" ×3"),
        "{lines:?}"
    );
    assert!(
        lines[1].contains("detected 1 sensitive item") && lines[1].ends_with(" ×9"),
        "{lines:?}"
    );
}

// Redacted samples carry the digits that tell two security hits apart; merging
// them would report one event where two happened.
#[test]
fn hook_notification_projection_keeps_distinct_redacted_numbers_apart() {
    let events = vec![
        hook_event(
            "pii-checker",
            "masked sample: [REDACTED_CARD:2603]",
            Some("allow"),
        ),
        hook_event(
            "pii-checker",
            "masked sample: [REDACTED_CARD:9471]",
            Some("allow"),
        ),
        hook_event(
            "pii-checker",
            "masked sample: [REDACTED_CARD:2603]",
            Some("allow"),
        ),
    ];

    let (_, projected) = govern_and_project(&events, Language::EnUs);

    assert_eq!(
        display_texts(&projected),
        [
            "• pii-checker: masked sample: [REDACTED_CARD:2603] ×2",
            "• pii-checker: masked sample: [REDACTED_CARD:9471]",
        ]
    );
}

#[test]
fn hook_notification_projection_keeps_different_hooks_apart() {
    let events = vec![
        hook_event("pii-checker", "same message", Some("allow")),
        hook_event("secret-scanner", "same message", Some("allow")),
        hook_event("pii-checker", "same message", Some("allow")),
    ];

    let (_, projected) = govern_and_project(&events, Language::EnUs);

    assert_eq!(
        display_texts(&projected),
        [
            "• pii-checker: same message ×2",
            "• secret-scanner: same message",
        ]
    );
}

// Decisions that changed or gated the run must stay equally prominent even when
// the text repeats verbatim: collapsing them would hide how often a command was
// actually blocked.
#[test]
fn hook_notification_projection_keeps_blocking_hook_notification_decisions_expanded() {
    let mut events = Vec::new();
    for decision in ["block", "deny", "reject", "ask"] {
        events.push(hook_event(
            "sandbox-guard",
            "blocked reboot",
            Some(decision),
        ));
        events.push(hook_event(
            "sandbox-guard",
            "blocked reboot",
            Some(decision),
        ));
    }

    let (governed, projected) = govern_and_project(&events, Language::EnUs);

    assert_eq!(projected.len(), 8);
    assert_eq!(display_texts(&governed), display_texts(&projected));
    assert_eq!(
        projected[0].display_text,
        "Hook: sandbox-guard\nMessage: blocked reboot\nDecision: block"
    );
    assert!(projected
        .iter()
        .all(|event| !event.display_text.contains('×')));
}

// `passthrough` is not a permissive verdict the panel may weaken, and an
// unknown or absent decision must never be guessed at.
#[test]
fn hook_notification_projection_keeps_unrecognized_hook_notification_decisions_expanded() {
    let events = vec![
        hook_event("hook-a", "same message", Some("passthrough")),
        hook_event("hook-a", "same message", Some("passthrough")),
        hook_event("hook-b", "same message", Some("   ")),
        hook_event("hook-b", "same message", Some("   ")),
        hook_event("hook-c", "same message", None),
        hook_event("hook-c", "same message", None),
        hook_event("hook-d", "same message", Some("quarantine")),
        hook_event("hook-d", "same message", Some("quarantine")),
    ];

    let (governed, projected) = govern_and_project(&events, Language::EnUs);

    assert_eq!(display_texts(&governed), display_texts(&projected));
    assert_eq!(
        projected[2].display_text,
        "Hook: hook-b\nMessage: same message\nDecision: unspecified"
    );
    assert_eq!(
        projected[6].display_text,
        "Hook: hook-d\nMessage: same message\nDecision: quarantine"
    );
}

// Only an exact `[hook_name]` prefix is redundant with the hook column; any
// other bracketed lead-in is real message content.
#[test]
fn hook_notification_projection_strips_only_the_exact_hook_notification_prefix() {
    let events = vec![
        hook_event("pii-checker", "[pii-checker] body", Some("allow")),
        hook_event("pii-checker", "[pii-checker-v2] body", Some("allow")),
        hook_event("pii-checker", "[other] body", Some("allow")),
    ];

    let (governed, projected) = govern_and_project(&events, Language::EnUs);

    assert_eq!(
        display_texts(&projected),
        [
            "• pii-checker: body",
            "• pii-checker: [pii-checker-v2] body",
            "• pii-checker: [other] body",
        ]
    );
    // The stored event keeps the provider's original text verbatim.
    assert!(matches!(
        &governed[0].event,
        AgentEvent::HookNotification { hook_name, message, decision, .. }
            if hook_name == "pii-checker"
                && message == "[pii-checker] body"
                && decision.as_deref() == Some("allow")
    ));
}

#[test]
fn hook_notification_projection_reuses_localized_fallbacks_for_empty_hook_and_message() {
    let events = vec![
        hook_event("  ", "   ", Some(" ALLOW ")),
        hook_event("  ", "   ", Some("Approve")),
    ];

    let (_, en) = govern_and_project(&events, Language::EnUs);
    let (_, zh) = govern_and_project(&events, Language::ZhCn);

    // Decision matching trims and ignores ASCII case, so both spellings land in
    // the same summary.
    assert_eq!(
        display_texts(&en),
        ["• unknown hook: no message provided ×2"]
    );
    assert_eq!(display_texts(&zh), ["• 未知 Hook: 未提供消息 ×2"]);
}

#[test]
fn hook_notification_projection_preserves_first_seen_order_and_other_events() {
    let events = vec![
        AgentEvent::TextDelta {
            run_id: "run-1".to_string(),
            text: "answer".to_string(),
        },
        hook_event("pii-checker", "hit", Some("allow")),
        AgentEvent::AgentCompleted {
            run_id: "run-1".to_string(),
            summary: "done".to_string(),
        },
        hook_event("pii-checker", "hit", Some("allow")),
    ];

    let (_, projected) = govern_and_project(&events, Language::EnUs);

    assert_eq!(
        display_texts(&projected),
        ["answer", "• pii-checker: hit ×2", "done"]
    );
}

// The projection is a temporary view: it must not replace the governed source
// events or change the governance audit cardinality derived from them.
#[test]
fn hook_notification_projection_preserves_source_events_and_audit_cardinality() {
    let events = vec![
        hook_event("pii-checker", PII_ALLOW_MESSAGE, Some("allow")),
        hook_event("pii-checker", PII_ALLOW_MESSAGE, Some("allow")),
        hook_event("sandbox-guard", "blocked reboot", Some("block")),
    ];

    let governed = govern_agent_events_with_language(&events, &Policy::default(), Language::EnUs);
    let projected =
        project_hook_notifications_for_display(&governed.events, &I18n::new(Language::EnUs));

    assert_eq!(projected.len(), 2);
    assert_eq!(governed.events.len(), 3);
    assert_eq!(governed.audit.len(), 3);
    assert_eq!(
        governed.events[0].display_text,
        format!("Hook: pii-checker\nMessage: {PII_ALLOW_MESSAGE}\nDecision: allow")
    );
    assert!(matches!(
        &governed.events[0].event,
        AgentEvent::HookNotification { message, decision, .. }
            if message == PII_ALLOW_MESSAGE && decision.as_deref() == Some("allow")
    ));
}
