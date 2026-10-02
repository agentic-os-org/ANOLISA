//! Auth control-protocol exchange with deterministic validation and retry.

use std::io::Write;
use std::time::Duration;

use tokio::io::AsyncBufReadExt;

use crate::config::CoreConfig;
use crate::protocol::{
    AuthReason, ControlResponseBody, InputMessage, OutputMessage, ShellControlRequest,
    CONTROL_PROTOCOL_VERSION,
};

use super::{builtin_auth_providers, prepare_auth_candidate, AuthConfigureError, AuthResponse};

const AUTH_TIMEOUT: Duration = Duration::from_secs(300);

struct AuthWaitResult {
    response: Option<AuthResponse>,
    buffered_lines: Vec<String>,
}

pub(crate) struct ValidatedAuth {
    pub(crate) candidate: CoreConfig,
}

pub(crate) async fn request_validated_auth<W, R>(
    config: &mut CoreConfig,
    reader: &mut tokio::io::Lines<R>,
    writer: &mut W,
    request_id: &str,
    initial_reason: AuthReason,
    initial_error: Option<String>,
    buffered_lines: &mut Vec<String>,
) -> Option<ValidatedAuth>
where
    W: Write,
    R: AsyncBufReadExt + Unpin,
{
    let mut attempt = 0_u32;
    let mut reason = initial_reason;
    let mut error_message = initial_error;

    loop {
        let current_request_id = if attempt == 0 {
            request_id.to_string()
        } else {
            format!("{request_id}-retry-{attempt}")
        };
        let message = OutputMessage::auth_required(
            &current_request_id,
            reason.clone(),
            error_message.take(),
            builtin_auth_providers(),
        );
        emit(writer, &message);

        let auth_result = wait_for_auth_response(&current_request_id, reader).await;
        buffered_lines.extend(auth_result.buffered_lines);
        let response = auth_result.response?;

        match prepare_auth_candidate(config, &response).await {
            Ok(candidate) => {
                if response.persist && crate::config::persist_config(&candidate).is_err() {
                    let error = AuthConfigureError::Persistence;
                    tracing::warn!("auth candidate could not be persisted");
                    reason = AuthReason::Invalid;
                    error_message = Some(error.to_string());
                    attempt = attempt.saturating_add(1);
                    continue;
                }
                return Some(ValidatedAuth { candidate });
            }
            Err(error) => {
                tracing::warn!("invalid auth response: {error}");
                reason = AuthReason::Invalid;
                error_message = Some(error.to_string());
                attempt = attempt.saturating_add(1);
            }
        }
    }
}

async fn wait_for_auth_response<R: AsyncBufReadExt + Unpin>(
    expected_request_id: &str,
    reader: &mut tokio::io::Lines<R>,
) -> AuthWaitResult {
    let mut buffered_lines = Vec::new();
    let result = tokio::time::timeout(AUTH_TIMEOUT, async {
        while let Ok(Some(line)) = reader.next_line().await {
            let line = line.trim().to_string();
            if line.is_empty() {
                continue;
            }
            let message: InputMessage = match serde_json::from_str(&line) {
                Ok(message) => message,
                Err(_) => {
                    // Let the headless input loop emit the protocol error.
                    buffered_lines.push(line);
                    continue;
                }
            };
            match message {
                InputMessage::ControlResponse { response } => {
                    if response.request_id == expected_request_id {
                        return parse_auth_response(&response.response);
                    }
                }
                InputMessage::ControlRequest { request, .. } => {
                    if matches!(
                        &request,
                        ShellControlRequest::Initialize {
                            protocol_version: Some(version),
                            ..
                        } if *version != CONTROL_PROTOCOL_VERSION
                    ) {
                        // Replay through the headless dispatcher immediately;
                        // it emits the version error and terminates before any
                        // buffered user message can start a provider turn.
                        buffered_lines.push(line);
                        return None;
                    }
                    if matches!(request, ShellControlRequest::Interrupt) {
                        return None;
                    }
                    buffered_lines.push(line);
                }
                _ => buffered_lines.push(line),
            }
        }
        None
    })
    .await;

    match result {
        Ok(response) => AuthWaitResult {
            response,
            buffered_lines,
        },
        Err(_) => {
            tracing::warn!("Auth timeout after {}s", AUTH_TIMEOUT.as_secs());
            AuthWaitResult {
                response: None,
                buffered_lines,
            }
        }
    }
}

fn parse_auth_response(body: &ControlResponseBody) -> Option<AuthResponse> {
    if body.behavior.as_deref() == Some("deny") {
        return None;
    }

    Some(AuthResponse {
        provider_id: body.provider_id.clone()?,
        provider_type: body.provider_type.clone(),
        values: body.values.clone().unwrap_or_default(),
        persist: body.persist.unwrap_or(true),
    })
}

fn emit<W: Write>(writer: &mut W, message: &OutputMessage) {
    if let Ok(json) = serde_json::to_string(message) {
        let _ = writeln!(writer, "{json}");
        let _ = writer.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use tokio::io::AsyncWriteExt;

    fn model_server() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request);
            let body = r#"{"id":"test-model"}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });
        format!("http://{address}/v1")
    }

    #[tokio::test]
    async fn buffers_invalid_jsonl_during_auth_wait() {
        let (mut input, output) = tokio::io::duplex(1024);
        input
            .write_all(b"token=must-not-echo\n")
            .await
            .expect("write invalid JSONL");
        input
            .write_all(
                br#"{"type":"control_response","response":{"subtype":"auth","request_id":"auth-init","response":{"provider_id":"dashscope","values":{"api_key":"test-key"},"persist":false}}}"#,
            )
            .await
            .expect("write auth response");
        input
            .write_all(b"\n")
            .await
            .expect("terminate auth response");
        drop(input);

        let mut lines = tokio::io::BufReader::new(output).lines();
        let result = wait_for_auth_response("auth-init", &mut lines).await;

        assert_eq!(result.buffered_lines, vec!["token=must-not-echo"]);
        assert!(result.response.is_some(), "expected auth response");
    }

    #[tokio::test]
    async fn returns_unsupported_initialize_for_immediate_replay() {
        let (mut input, output) = tokio::io::duplex(1024);
        let line = r#"{"type":"control_request","request_id":"init-1","request":{"subtype":"initialize","protocol_version":9}}"#;
        input
            .write_all(format!("{line}\n").as_bytes())
            .await
            .expect("write initialize request");

        let mut lines = tokio::io::BufReader::new(output).lines();
        let result = wait_for_auth_response("auth-init", &mut lines).await;

        assert_eq!(result.buffered_lines, vec![line]);
        assert!(result.response.is_none());
    }

    #[tokio::test]
    async fn invalid_response_requests_retry_before_returning_success() {
        let (mut input, output) = tokio::io::duplex(4096);
        let valid_base_url = model_server();
        input
            .write_all(
                format!(
                    "{{\"type\":\"control_response\",\"response\":{{\"subtype\":\"auth\",\"request_id\":\"auth-test\",\"response\":{{\"provider_id\":\"test\",\"provider_type\":\"openai_compat\",\"values\":{{\"base_url\":\"bad-url\",\"api_key\":\"test-key\",\"model\":\"test-model\"}},\"persist\":false}}}}}}\n{{\"type\":\"control_response\",\"response\":{{\"subtype\":\"auth\",\"request_id\":\"auth-test-retry-1\",\"response\":{{\"provider_id\":\"test\",\"provider_type\":\"openai_compat\",\"values\":{{\"base_url\":{valid_base_url:?},\"api_key\":\"test-key\",\"model\":\"test-model\"}},\"persist\":false}}}}}}\n"
                )
                .as_bytes(),
            )
            .await
            .expect("write auth responses");
        drop(input);

        let mut lines = tokio::io::BufReader::new(output).lines();
        let mut protocol_output = Vec::new();
        let mut config = CoreConfig::default();
        let response = request_validated_auth(
            &mut config,
            &mut lines,
            &mut protocol_output,
            "auth-test",
            AuthReason::NotConfigured,
            None,
            &mut Vec::new(),
        )
        .await
        .expect("valid retry response");

        assert_eq!(
            response.candidate.ai.active_provider.as_deref(),
            Some("test")
        );
        let messages = String::from_utf8(protocol_output).expect("protocol output");
        let lines = messages.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains(r#""reason":"not_configured""#));
        assert!(lines[1].contains(r#""reason":"invalid""#));
        assert!(lines[1].contains("invalid base_url"));
        assert!(!config.ai.providers.contains_key("test"));
    }

    #[test]
    fn parse_auth_response_deny() {
        let body = ControlResponseBody {
            behavior: Some("deny".to_string()),
            message: None,
            result: None,
            checkpoint_result: None,
            checkpoint_error: None,
            tool_use_id: None,
            updated_permissions: None,
            answer: None,
            selected_options: None,
            provider_id: None,
            provider_type: None,
            values: None,
            persist: None,
            unknown_fields: HashMap::new(),
        };
        assert!(parse_auth_response(&body).is_none());
    }

    #[test]
    fn parse_auth_response_success() {
        let body = ControlResponseBody {
            behavior: None,
            message: None,
            result: None,
            checkpoint_result: None,
            checkpoint_error: None,
            tool_use_id: None,
            updated_permissions: None,
            answer: None,
            selected_options: None,
            provider_id: Some("dashscope".to_string()),
            provider_type: None,
            values: Some(HashMap::from([(
                "api_key".to_string(),
                "sk-xxx".to_string(),
            )])),
            persist: Some(true),
            unknown_fields: HashMap::new(),
        };
        let response = parse_auth_response(&body).expect("auth response");
        assert_eq!(response.provider_id, "dashscope");
        assert_eq!(response.provider_type, None);
        assert_eq!(response.values.get("api_key").unwrap(), "sk-xxx");
        assert!(response.persist);
    }
}
