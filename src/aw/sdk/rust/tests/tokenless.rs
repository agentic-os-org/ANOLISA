//! Exercise real component state and compare facade results with the Runtime.

#![cfg(feature = "tokenless")]

use aw_sdk::{CallContext, Client, Config, Error, tokenless::*};
use serde_json::json;
use tempfile::TempDir;
use tokenless_protocol::Attribution;
use tokenless_runtime::TokenlessRuntime;

fn directory() -> TempDir {
    let root = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"));
    std::fs::create_dir_all(root).unwrap();
    // Cargo may retain ".." from target-dir configuration; component state
    // policy correctly rejects parent traversal even in an absolute path.
    tempfile::tempdir_in(root.canonicalize().unwrap()).unwrap()
}

fn client(directory: &TempDir, rtk_path: Option<std::path::PathBuf>) -> Client {
    Client::new(Config {
        tokenless: Some(aw_sdk::tokenless::Config {
            rtk_path,
            ..aw_sdk::tokenless::Config::new(directory.path().join("state"))
        }),
        #[cfg(feature = "sec-core")]
        sec_core_socket: None,
    })
    .unwrap()
}

fn post_request() -> PostToolRequest {
    PostToolRequest {
        result_kind: ResultKind::Tool,
        tool_name: "api".into(),
        content: serde_json::to_string(
            &(0..100)
                .map(|i| {
                    json!({
                        "id": i, "message": format!("record-{i}-{}", "x".repeat(80))
                    })
                })
                .collect::<Vec<_>>(),
        )
        .unwrap(),
        status: ToolResultStatus::Success,
        content_origin: ContentOrigin::ApiResponse,
        command: None,
        output_optimization: OutputOptimization::None,
        capabilities: PostToolCapabilities {
            replace_output: true,
            replace_with_text: false,
            recovery: RecoveryMethod::tool("recover").unwrap(),
        },
    }
}

#[test]
fn real_runtime_matches_sdk_and_requires_visible_marker_for_recovery() {
    let sdk_directory = directory();
    let direct_directory = directory();
    let sdk = client(&sdk_directory, None);
    let component = sdk.tokenless().unwrap();
    let direct = TokenlessRuntime::new(RuntimeConfig {
        data_dir: Some(direct_directory.path().join("state")),
        ..RuntimeConfig::default()
    })
    .unwrap();
    let context = CallContext::new("sdk-test");
    let request = post_request();
    let expected = direct
        .post_tool(&request, &Attribution::new("sdk-test"))
        .unwrap();
    let result = component.post_tool(&request, &context).unwrap();
    assert_eq!(result, expected);
    assert!(result.is_applied());
    assert!(!result.stash_keys.is_empty());
    assert!(component.stash_error().is_none());
    assert!(component.stats_error().is_none());
    let marker = format!("<<tokenless:{}>>", result.stash_keys[0]);
    let mut retrieve = RetrieveRequest {
        hash_or_marker: marker.clone(),
        visible_markers: vec![],
    };
    assert!(matches!(
        component.retrieve(&retrieve, &context),
        Err(Error::Tokenless(RuntimeError::RetrieveUnauthorized { .. }))
    ));
    let before = BeforeModelRequest {
        tools: vec![],
        visible_context: json!({"tool_result": result.output}),
        capabilities: BeforeModelCapabilities {
            replace_tools: true,
            recovery: RecoveryMethod::tool("recover").unwrap(),
        },
    };
    let visible = component.before_model(&before, &context).unwrap();
    assert_eq!(
        visible,
        direct
            .before_model(&before, &Attribution::new("sdk-test"))
            .unwrap()
    );
    assert!(visible.visible_markers.contains(&result.stash_keys[0]));
    retrieve.visible_markers = visible.visible_markers;
    assert_eq!(
        component.retrieve(&retrieve, &context).unwrap().payload,
        request.content
    );
    // Persistent recovery survives dropping handles, without installing a service.
    drop(sdk);
    let reopened = client(&sdk_directory, None);
    assert_eq!(
        reopened
            .tokenless()
            .unwrap()
            .retrieve(&retrieve, &context)
            .unwrap()
            .payload,
        request.content
    );
}

#[test]
fn failed_and_rtk_owned_results_are_preserved() {
    let directory = directory();
    let sdk = client(&directory, None);
    for (status, optimization) in [
        (ToolResultStatus::Error, OutputOptimization::None),
        (ToolResultStatus::Success, OutputOptimization::Rtk),
    ] {
        let request = PostToolRequest {
            status,
            output_optimization: optimization,
            ..post_request()
        };
        let result = sdk
            .tokenless()
            .unwrap()
            .post_tool(&request, &CallContext::new("test"))
            .unwrap();
        assert_eq!(result.output, request.content);
        assert!(!result.is_applied());
    }
}

#[test]
fn pre_tool_uses_explicit_rtk_and_retains_host_adoption_choice() {
    use std::os::unix::fs::PermissionsExt;
    let directory = directory();
    let rtk = directory.path().join("rtk");
    std::fs::write(&rtk, "#!/bin/sh\n[ \"$1\" = rewrite ] || exit 9\n[ \"$2\" = 'git status' ] || exit 8\nprintf 'rtk git status\\n'\n").unwrap();
    std::fs::set_permissions(&rtk, std::fs::Permissions::from_mode(0o700)).unwrap();
    let request = PreToolRequest {
        tool_name: "Bash".into(),
        arguments: json!({"command": "git status", "other": "keep"}),
        command_field: "command".into(),
        capabilities: PreToolCapabilities {
            replace_arguments: false,
            block_and_suggest: true,
        },
    };
    let context = CallContext::new("sdk-test");
    let missing = client(&directory, None);
    assert!(matches!(
        missing.tokenless().unwrap().pre_tool(&request, &context),
        Err(Error::Tokenless(RuntimeError::RtkUnavailable))
    ));
    let sdk = client(&directory, Some(rtk));
    let result = sdk
        .tokenless()
        .unwrap()
        .pre_tool(&request, &context)
        .unwrap();
    assert_eq!(result.action, PreToolAction::BlockAndSuggest);
    assert_eq!(result.output_optimization, OutputOptimization::Rtk);
    assert_eq!(
        result.arguments,
        json!({
            "command": format!(
                "env TOKENLESS_AGENT_ID=sdk-test TOKENLESS_SESSION_ID= TOKENLESS_TOOL_USE_ID= TOKENLESS_DATA_DIR={} {} git status",
                directory.path().join("state").display(), directory.path().join("rtk").display()
            ),
            "other": "keep"
        })
    );
    std::fs::remove_file(directory.path().join("rtk")).unwrap();
    assert!(matches!(
        sdk.tokenless().unwrap().pre_tool(&request, &context),
        Err(Error::Tokenless(RuntimeError::RtkSpawn { .. }))
    ));
}
