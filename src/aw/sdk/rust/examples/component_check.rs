//! Acceptance against an existing daemon, managed RTK and real Tokenless state.

use std::{path::PathBuf, time::Duration};

use aw_sdk::{CallContext, Client, Config, Error, sec_core, tokenless::*};
use serde_json::json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if arguments.len() != 3 {
        return Err(
            "usage: component_check ABSOLUTE_DAEMON_SOCKET ABSOLUTE_STATE_DIRECTORY ABSOLUTE_RTK"
                .into(),
        );
    }
    let rtk = PathBuf::from(&arguments[2]);
    let client = Client::new(Config {
        sec_core_socket: Some(PathBuf::from(&arguments[0])),
        tokenless: Some(aw_sdk::tokenless::Config {
            rtk_path: Some(rtk.clone()),
            ..aw_sdk::tokenless::Config::new(PathBuf::from(&arguments[1]))
        }),
    })?;
    let mut context = CallContext::new("sdk-component-check");
    context.session_id = Some("acceptance".into());
    context.tool_use_id = Some("call-1".into());
    for (code, expected) in [
        ("echo hello", sec_core::Verdict::Pass),
        // Source data only: neither the SDK nor this check executes the snippet.
        ("rm -rf /", sec_core::Verdict::Warn),
    ] {
        let response = client.sec_core()?.scan_code(
            &sec_core::ScanRequest {
                code: code.into(),
                language: "bash".into(),
                rules: None,
                mode: Some("regex".into()),
            },
            &context,
            Duration::from_secs(5),
        )?;
        let sec_core::ScanResponse::Success(response) = response else {
            return Err("daemon rejected the acceptance scan".into());
        };
        if !response.result.ok || response.result.verdict != expected {
            return Err(format!(
                "expected {expected:?}, received {:?} (completed={})",
                response.result.verdict, response.result.ok
            )
            .into());
        }
        if expected == sec_core::Verdict::Warn && response.result.findings.is_empty() {
            return Err("warning lost its typed findings".into());
        }
    }
    let tokenless = client.tokenless()?;
    if tokenless.stash_error().is_some() || tokenless.stats_error().is_some() {
        return Err("Tokenless state is not available".into());
    }
    let post = PostToolRequest {
        result_kind: ResultKind::Tool,
        tool_name: "api".into(),
        content: serde_json::to_string(
            &(0..100)
                .map(|id| json!({"id": id, "message": format!("record-{id}-{}", "x".repeat(80))}))
                .collect::<Vec<_>>(),
        )?,
        status: ToolResultStatus::Success,
        content_origin: ContentOrigin::ApiResponse,
        command: None,
        output_optimization: OutputOptimization::None,
        capabilities: PostToolCapabilities {
            replace_output: true,
            replace_with_text: false,
            recovery: RecoveryMethod::tool("recover")?,
        },
    };
    let compressed = tokenless.post_tool(&post, &context)?;
    if !compressed.is_applied() || compressed.output.len() >= post.content.len() {
        return Err("real Runtime did not compress the acceptance input".into());
    }
    let key = compressed
        .stash_keys
        .first()
        .ok_or("compression lost the recovery marker")?;
    let mut retrieve = RetrieveRequest {
        hash_or_marker: key.clone(),
        visible_markers: vec![],
    };
    if !matches!(
        tokenless.retrieve(&retrieve, &context),
        Err(Error::Tokenless(RuntimeError::RetrieveUnauthorized { .. }))
    ) {
        return Err("recovery accepted an invisible marker".into());
    }
    let visible = tokenless.before_model(
        &BeforeModelRequest {
            tools: vec![],
            visible_context: json!({"tool_result": compressed.output}),
            capabilities: BeforeModelCapabilities {
                replace_tools: true,
                recovery: RecoveryMethod::tool("recover")?,
            },
        },
        &context,
    )?;
    retrieve.visible_markers = visible.visible_markers;
    if tokenless.retrieve(&retrieve, &context)?.payload != post.content {
        return Err("authorized recovery changed the original content".into());
    }
    let rewrite = tokenless.pre_tool(
        &PreToolRequest {
            tool_name: "Bash".into(),
            arguments: json!({"command": "git status", "cwd": "/application"}),
            command_field: "command".into(),
            capabilities: PreToolCapabilities {
                replace_arguments: false,
                block_and_suggest: true,
            },
        },
        &context,
    )?;
    if rewrite.action != PreToolAction::BlockAndSuggest
        || rewrite.output_optimization != OutputOptimization::Rtk
        || rewrite.arguments["cwd"] != "/application"
        || !rewrite.arguments["command"]
            .as_str()
            .is_some_and(|command| command.contains(rtk.to_string_lossy().as_ref()))
    {
        return Err("managed RTK did not produce an attributed retry suggestion".into());
    }
    for (status, output_optimization) in [
        (ToolResultStatus::Error, OutputOptimization::None),
        (ToolResultStatus::Success, OutputOptimization::Rtk),
    ] {
        let preserved = tokenless.post_tool(
            &PostToolRequest {
                status,
                output_optimization,
                ..post.clone()
            },
            &context,
        )?;
        if preserved.is_applied() || preserved.output != post.content {
            return Err("failed or RTK-owned output was recompressed".into());
        }
    }
    println!(
        "{}",
        json!({
            "sec_core_pass_and_warn": true,
            "tokenless_post_tool": true,
            "tokenless_before_model": true,
            "tokenless_authorized_retrieve": true,
            "tokenless_managed_rtk_pre_tool": true,
            "failed_and_rtk_owned_results_preserved": true,
        })
    );
    Ok(())
}
