fn prompt_text(input: Vec<ContentPart>) -> Result<String, AgentRuntimePortError> {
    let mut output = String::new();
    for part in input {
        match part {
            ContentPart::Text { text } => {
                let separator = usize::from(!output.is_empty());
                let next_len = output
                    .len()
                    .checked_add(separator)
                    .and_then(|length| length.checked_add(text.as_str().len()))
                    .ok_or(AgentRuntimePortError::Protocol)?;
                if next_len > MAX_TEXT_BYTES {
                    return Err(AgentRuntimePortError::Protocol);
                }
                if separator == 1 {
                    output.push('\n');
                }
                output.push_str(text.as_str());
            }
            ContentPart::ResourceLink { .. } => {
                return Err(AgentRuntimePortError::Unsupported {
                    operation: "resource prompt",
                })
            }
        }
    }
    if output.is_empty() {
        Err(AgentRuntimePortError::Protocol)
    } else {
        Ok(output)
    }
}

fn provider_permission_callback(
    request: &AcpV1PermissionRequest,
    normalized: &CapabilityRequest,
) -> Result<ProviderPermissionCallbackV2, AgentRuntimePortError> {
    let tool_call_id = request
        .tool_call
        .get("toolCallId")
        .and_then(serde_json::Value::as_str)
        .ok_or(AgentRuntimePortError::Protocol)?;
    let provider_request_id_digest = match &request.request_id {
        AcpV1RequestId::Number(value) => {
            digest_parts(&[b"cosh.acp.request-id.v2", b"number", value.to_string().as_bytes()])
        }
        AcpV1RequestId::String(value) => {
            digest_parts(&[b"cosh.acp.request-id.v2", b"string", value.as_bytes()])
        }
    };
    let ordered_option_set_digest = permission_options_digest(&request.options);
    Ok(ProviderPermissionCallbackV2 {
        provider_session_digest: digest_parts(&[
            b"cosh.acp.session-id.v2",
            request.session_id.as_bytes(),
        ]),
        provider_request_id_digest,
        provider_tool_call_id_digest: digest_parts(&[
            b"cosh.acp.tool-call-id.v2",
            tool_call_id.as_bytes(),
        ]),
        ordered_option_set_digest,
        callback_payload_digest: request.callback_payload_digest.clone(),
        normalized_operation_digest: normalized.operation_digest.clone(),
    })
}

fn permission_carrier_matches_snapshot(
    carrier: &serde_json::Value,
    snapshot: &serde_json::Value,
) -> bool {
    let (Some(carrier), Some(snapshot)) = (carrier.as_object(), snapshot.as_object()) else {
        return false;
    };
    carrier.iter().all(|(field, value)| {
        if field == "status" {
            value.as_str() == Some("pending") || snapshot.get(field) == Some(value)
        } else {
            snapshot.get(field) == Some(value)
        }
    })
}

fn canonicalize_self_contained_permission_carrier(
    carrier: &serde_json::Value,
) -> Result<serde_json::Value, AgentRuntimePortError> {
    let object = carrier
        .as_object()
        .ok_or(AgentRuntimePortError::Protocol)?;
    if object.keys().any(|field| {
        !matches!(
            field.as_str(),
            "toolCallId" | "kind" | "status" | "title" | "content" | "rawInput" | "_meta"
        )
    })
    {
        return Err(AgentRuntimePortError::Protocol);
    }

    let tool_call_id = required_bounded_carrier_text(object, "toolCallId")?;
    if tool_call_id.len() > DEFAULT_MAX_TOOL_IDENTIFIER_BYTES {
        return Err(AgentRuntimePortError::Protocol);
    }
    let kind = required_bounded_carrier_text(object, "kind")?;
    match kind {
        "read" | "edit" | "delete" | "move" | "search" | "execute" | "think" | "fetch"
        | "switch_mode" | "other" => {}
        _ => return Err(AgentRuntimePortError::Protocol),
    }
    if object.get("status").and_then(serde_json::Value::as_str) != Some("pending") {
        return Err(AgentRuntimePortError::Protocol);
    }

    let canonical = if object.get("title").is_some() {
        carrier.clone()
    } else {
        canonicalize_titleless_execute_carrier(object, kind)?
    };
    let canonical_object = canonical
        .as_object()
        .ok_or(AgentRuntimePortError::Protocol)?;
    let title = required_bounded_carrier_text(canonical_object, "title")?;
    let summary = title
        .chars()
        .filter(|character| !character.is_control())
        .collect::<String>();
    if summary.trim().is_empty() || summary.len() > MAX_TEXT_BYTES {
        return Err(AgentRuntimePortError::Protocol);
    }
    let raw_input_is_sufficient = canonical_object
        .get("rawInput")
        .is_some_and(nonempty_json_value);
    let content_is_sufficient = canonical_object.get("content").is_some_and(|content| {
        content
            .as_array()
            .is_some_and(|items| !items.is_empty() && items.iter().all(valid_tool_content))
    });
    if !raw_input_is_sufficient && !content_is_sufficient {
        return Err(AgentRuntimePortError::Protocol);
    }

    Ok(canonical)
}

fn canonicalize_codex_command_permission_refinement(
    carrier: &serde_json::Value,
    snapshot: &serde_json::Value,
) -> Result<serde_json::Value, AgentRuntimePortError> {
    let carrier = carrier
        .as_object()
        .ok_or(AgentRuntimePortError::Protocol)?;
    let snapshot = snapshot
        .as_object()
        .ok_or(AgentRuntimePortError::Protocol)?;
    if carrier.contains_key("title")
        || carrier.contains_key("content")
        || carrier.get("toolCallId") != snapshot.get("toolCallId")
        || snapshot.get("status").and_then(serde_json::Value::as_str) != Some("in_progress")
        || !matches!(
            snapshot.get("kind").and_then(serde_json::Value::as_str),
            Some("read" | "search" | "execute")
        )
    {
        return Err(AgentRuntimePortError::Protocol);
    }
    canonicalize_self_contained_permission_carrier(&serde_json::Value::Object(carrier.clone()))
}

fn canonicalize_titleless_execute_carrier(
    object: &serde_json::Map<String, serde_json::Value>,
    kind: &str,
) -> Result<serde_json::Value, AgentRuntimePortError> {
    if kind != "execute" || object.contains_key("content") {
        return Err(AgentRuntimePortError::Protocol);
    }
    let raw_input = object
        .get("rawInput")
        .and_then(serde_json::Value::as_object)
        .ok_or(AgentRuntimePortError::Protocol)?;
    if raw_input
        .keys()
        .any(|field| !matches!(field.as_str(), "command" | "cwd"))
    {
        return Err(AgentRuntimePortError::Protocol);
    }
    let command = required_bounded_carrier_text(raw_input, "command")?;
    if command.trim().is_empty() {
        return Err(AgentRuntimePortError::Protocol);
    }
    let cwd = if let Some(cwd) = raw_input.get("cwd") {
        let cwd = cwd.as_str().ok_or(AgentRuntimePortError::Protocol)?;
        if cwd.is_empty()
            || cwd.len() > MAX_TEXT_BYTES
            || cwd.chars().any(char::is_control)
            || !std::path::Path::new(cwd).is_absolute()
        {
            return Err(AgentRuntimePortError::Protocol);
        }
        Some(cwd)
    } else {
        None
    };
    let quoted_command =
        serde_json::to_string(command).map_err(|_| AgentRuntimePortError::Protocol)?;
    let title = if let Some(cwd) = cwd {
        let quoted_cwd =
            serde_json::to_string(cwd).map_err(|_| AgentRuntimePortError::Protocol)?;
        format!("Run cwd={quoted_cwd}, command={quoted_command}")
    } else {
        format!("Run command={quoted_command}")
    };
    if title.len() > MAX_TEXT_BYTES {
        return Err(AgentRuntimePortError::Protocol);
    }

    let mut canonical = object.clone();
    canonical.insert("title".into(), serde_json::Value::String(title));
    Ok(serde_json::Value::Object(canonical))
}

fn required_bounded_carrier_text<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<&'a str, AgentRuntimePortError> {
    let value = object
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or(AgentRuntimePortError::Protocol)?;
    if value.is_empty() || value.len() > MAX_TEXT_BYTES {
        return Err(AgentRuntimePortError::Protocol);
    }
    Ok(value)
}

fn nonempty_json_value(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => false,
        serde_json::Value::String(value) => !value.is_empty(),
        serde_json::Value::Array(value) => !value.is_empty(),
        serde_json::Value::Object(value) => !value.is_empty(),
        serde_json::Value::Bool(_) | serde_json::Value::Number(_) => true,
    }
}

fn valid_tool_content(value: &serde_json::Value) -> bool {
    if serde_json::from_value::<agent_client_protocol::schema::v1::ToolCallContent>(value.clone())
        .is_err()
    {
        return false;
    }
    let Some(object) = value.as_object() else {
        return false;
    };
    match object.get("type").and_then(serde_json::Value::as_str) {
        Some("content") => object.get("content").is_some_and(nonempty_json_value),
        Some("diff") => {
            object
                .get("path")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|path| !path.is_empty())
                && object
                    .get("newText")
                    .and_then(serde_json::Value::as_str)
                    .is_some()
        }
        Some("terminal") => object
            .get("terminalId")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|terminal_id| !terminal_id.is_empty()),
        _ => false,
    }
}

fn permission_options_digest(options: &[super::AcpV1PermissionOption]) -> Digest {
    let mut parts = vec![b"cosh.acp.permission-options.v2".as_slice()];
    let kind_names = options
        .iter()
        .map(|option| match option.kind {
            AcpV1PermissionOptionKind::AllowOnce => b"allow_once".as_slice(),
            AcpV1PermissionOptionKind::AllowAlways => b"allow_always".as_slice(),
            AcpV1PermissionOptionKind::RejectOnce => b"reject_once".as_slice(),
            AcpV1PermissionOptionKind::RejectAlways => b"reject_always".as_slice(),
            AcpV1PermissionOptionKind::Unsupported => b"unsupported".as_slice(),
        })
        .collect::<Vec<_>>();
    for (option, kind) in options.iter().zip(&kind_names) {
        parts.push(option.option_id.as_bytes());
        parts.push(option.name.as_bytes());
        parts.push(kind);
    }
    digest_parts(&parts)
}

fn digest_parts(parts: &[&[u8]]) -> Digest {
    let mut digest = Sha256::new();
    for part in parts {
        digest.update((part.len() as u64).to_be_bytes());
        digest.update(part);
    }
    Digest::parse(format!("{:x}", digest.finalize()))
        .unwrap_or_else(|_| unreachable!("SHA-256 output must remain canonical"))
}
fn map_driver_error(error: AcpSessionDriverError) -> AgentRuntimePortError {
    match error {
        AcpSessionDriverError::Deadline { operation } => {
            AgentRuntimePortError::Deadline { operation }
        }
        AcpSessionDriverError::InvalidState { operation, state } => {
            AgentRuntimePortError::InvalidState { operation, state }
        }
        AcpSessionDriverError::Bridge(_)
        | AcpSessionDriverError::ActorUnavailable
        | AcpSessionDriverError::CancellationPending
        | AcpSessionDriverError::ObservationBackpressure
        | AcpSessionDriverError::Cancelled => AgentRuntimePortError::Transport,
        AcpSessionDriverError::InvalidDeadlineConfiguration => AgentRuntimePortError::Protocol,
    }
}
fn safe_error(
    code: &'static str,
    category: ErrorCategory,
    retryable: bool,
    message: &'static str,
) -> ContractError {
    ContractError::new(code, category, retryable, message)
        .unwrap_or_else(|_| unreachable!("static contract error must remain valid"))
}
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}
