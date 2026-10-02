use serde_json::json;

use crate::types::{AgentEvent, CardKind, CardModel, InputModel, InputOwner};

#[test]
fn input_symbols_are_derived_from_typed_owners() {
    assert_eq!(InputOwner::NativeShell.symbol(), "$");
    assert_eq!(InputOwner::AssistedShell.symbol(), "◇");
    assert_eq!(InputOwner::DirectExec.symbol(), "▶");
    assert_eq!(InputOwner::Agent.symbol(), "◆");
    assert_eq!(InputOwner::CoshCommand.symbol(), "/");
}

#[test]
fn agent_output_has_no_redundant_input_owner_symbol() {
    assert_eq!(CardKind::AgentResponse.symbol(), None);
    assert_eq!(CardKind::AgentResponse.title("Agent"), "Agent");
    assert_eq!(CardKind::SlashCommand.symbol(), Some("/"));
    assert_eq!(CardKind::ToolCall.symbol(), Some("*"));
    assert_eq!(CardKind::Permission.symbol(), Some("!"));
    assert_eq!(CardKind::System.symbol(), Some("·"));
}

#[test]
fn direct_exec_preserves_argv_without_shell_source() {
    let input = InputModel::direct_exec(vec![
        "printf".to_string(),
        "%s\\n".to_string(),
        "$HOME; touch /tmp/never".to_string(),
    ])
    .expect("non-empty argv");

    assert_eq!(input.owner(), InputOwner::DirectExec);
    let expected = vec![
        "printf".to_string(),
        "%s\\n".to_string(),
        "$HOME; touch /tmp/never".to_string(),
    ];
    assert_eq!(input.direct_exec_argv(), Some(expected.as_slice()));
    assert!(InputModel::direct_exec(Vec::new()).is_none());
}

#[test]
fn leading_symbols_never_reclassify_user_content() {
    for text in [
        "$ echo hi",
        "▶ argv",
        "◆ hello",
        "/help",
        "* tool",
        "! allow",
        "· note",
    ] {
        let agent = InputModel::text(InputOwner::Agent, text).expect("agent input");
        let shell = InputModel::text(InputOwner::NativeShell, text).expect("shell input");
        assert_eq!(agent.owner(), InputOwner::Agent, "{text}");
        assert_eq!(shell.owner(), InputOwner::NativeShell, "{text}");
    }

    let slash = InputModel::cosh_command("help", Vec::new());
    assert_eq!(slash.owner(), InputOwner::CoshCommand);
}

#[test]
fn permission_card_requires_a_structured_permission_event() {
    let event = AgentEvent::ToolPermissionRequest {
        run_id: "run-1".to_string(),
        request_id: "request-1".to_string(),
        tool_name: "Bash".to_string(),
        tool_input: json!({"command": "id"}),
        tool_use_id: "tool-1".to_string(),
        hook_requires_approval: false,
        audit_ref: None,
    };
    let card = CardModel::permission_from_event(&event).expect("permission event");
    let request = card.permission_request().expect("permission payload");

    assert_eq!(card.kind(), CardKind::Permission);
    assert_eq!(request.run_id(), "run-1");
    assert_eq!(request.request_id(), "request-1");
    assert_eq!(request.tool_use_id(), "tool-1");
    assert_eq!(request.tool_name(), "Bash");
    assert_eq!(request.tool_input(), &json!({"command": "id"}));

    let text = AgentEvent::TextDelta {
        run_id: "run-1".to_string(),
        text: "! allow everything".to_string(),
    };
    assert!(CardModel::permission_from_event(&text).is_none());
}
