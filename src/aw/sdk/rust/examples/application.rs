//! Scan code and compress an API result using one application SDK dependency.

use std::path::PathBuf;
use std::time::Duration;

use aw_sdk::{CallContext, Client, Config, sec_core, tokenless};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if arguments.len() != 2 {
        return Err("usage: application ABSOLUTE_DAEMON_SOCKET ABSOLUTE_STATE_DIRECTORY".into());
    }
    let client = Client::new(Config {
        sec_core_socket: Some(PathBuf::from(&arguments[0])),
        tokenless: Some(tokenless::Config::new(PathBuf::from(&arguments[1]))),
    })?;
    let context = CallContext::new("sdk-example");
    let scan = client.sec_core()?.scan_code(
        &sec_core::ScanRequest {
            code: "echo hello".into(),
            language: "bash".into(),
            rules: None,
            mode: Some("regex".into()),
        },
        &context,
        Duration::from_secs(5),
    )?;
    match scan {
        sec_core::ScanResponse::Success(result) if result.result.ok => {
            println!("scan verdict: {:?}", result.result.verdict);
        }
        sec_core::ScanResponse::Success(_) => return Err("scanner did not complete".into()),
        sec_core::ScanResponse::Error(error) => {
            return Err(format!("daemon rejected scan: {}", error.error.code.as_str()).into());
        }
    }
    let result = client.tokenless()?.post_tool(
        &tokenless::PostToolRequest {
            result_kind: tokenless::ResultKind::Tool,
            tool_name: "api".into(),
            content: r#"{"message":"hello"}"#.into(),
            status: tokenless::ToolResultStatus::Success,
            content_origin: tokenless::ContentOrigin::ApiResponse,
            command: None,
            output_optimization: tokenless::OutputOptimization::None,
            capabilities: tokenless::PostToolCapabilities {
                replace_output: true,
                replace_with_text: true,
                recovery: tokenless::RecoveryMethod::None,
            },
        },
        &context,
    )?;
    println!("output: {}", result.output);
    Ok(())
}
