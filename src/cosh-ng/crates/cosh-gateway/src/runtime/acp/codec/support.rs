fn validate_absolute(path: &Path) -> Result<(), AcpV1CodecError> {
    if !path.is_absolute() {
        return Err(AcpV1CodecError::WorkspaceNotAbsolute(path.to_path_buf()));
    }
    Ok(())
}

fn batch_entry_requires_response(entry: &TransportBatchEntry) -> bool {
    match entry {
        TransportBatchEntry::Message(RawJsonRpcMessage::Request(_)) => true,
        TransportBatchEntry::Message(
            RawJsonRpcMessage::Notification(_) | RawJsonRpcMessage::Response(_),
        ) => false,
        TransportBatchEntry::Malformed { raw, .. } => !is_response_only_shape(raw),
    }
}

fn is_response_only_shape(value: &serde_json::Value) -> bool {
    value.as_object().is_some_and(|object| {
        !object.contains_key("method")
            && (object.contains_key("result") || object.contains_key("error"))
    })
}

fn decode_params<T: serde::de::DeserializeOwned>(
    params: Option<RawJsonRpcParams>,
) -> Result<T, AcpV1CodecError> {
    let value = params.map_or(serde_json::Value::Null, RawJsonRpcParams::into_value);
    serde_json::from_value(value).map_err(Into::into)
}

fn permission_callback_digest(
    request_id: &AcpV1RequestId,
    method: &str,
    params: &serde_json::Value,
) -> Digest {
    let mut canonical_params = Vec::new();
    encode_canonical_json(params, &mut canonical_params);
    let (request_type, request_value) = match request_id {
        AcpV1RequestId::Number(value) => (b"number".as_slice(), value.to_string()),
        AcpV1RequestId::String(value) => (b"string".as_slice(), value.clone()),
    };
    sha256_parts(&[
        b"cosh.acp.permission-callback.v2",
        method.as_bytes(),
        request_type,
        request_value.as_bytes(),
        &canonical_params,
    ])
}

fn encode_canonical_json(value: &serde_json::Value, output: &mut Vec<u8>) {
    match value {
        serde_json::Value::Null => output.push(b'n'),
        serde_json::Value::Bool(value) => output.push(if *value { b't' } else { b'f' }),
        serde_json::Value::Number(value) => {
            append_canonical_part(output, b'd', value.to_string().as_bytes());
        }
        serde_json::Value::String(value) => {
            append_canonical_part(output, b's', value.as_bytes());
        }
        serde_json::Value::Array(values) => {
            output.push(b'a');
            output.extend_from_slice(&(values.len() as u64).to_be_bytes());
            for value in values {
                let mut encoded = Vec::new();
                encode_canonical_json(value, &mut encoded);
                append_canonical_part(output, b'v', &encoded);
            }
        }
        serde_json::Value::Object(values) => {
            output.push(b'o');
            output.extend_from_slice(&(values.len() as u64).to_be_bytes());
            let mut entries = values.iter().collect::<Vec<_>>();
            entries.sort_unstable_by_key(|(key, _)| *key);
            for (key, value) in entries {
                append_canonical_part(output, b'k', key.as_bytes());
                let mut encoded = Vec::new();
                encode_canonical_json(value, &mut encoded);
                append_canonical_part(output, b'v', &encoded);
            }
        }
    }
}

fn append_canonical_part(output: &mut Vec<u8>, tag: u8, value: &[u8]) {
    output.push(tag);
    output.extend_from_slice(&(value.len() as u64).to_be_bytes());
    output.extend_from_slice(value);
}

fn sha256_parts(parts: &[&[u8]]) -> Digest {
    let mut digest = Sha256::new();
    for part in parts {
        digest.update((part.len() as u64).to_be_bytes());
        digest.update(part);
    }
    Digest::parse(format!("{:x}", digest.finalize()))
        .unwrap_or_else(|_| unreachable!("SHA-256 output must remain canonical"))
}

fn from_sdk_request_id(id: RequestId) -> Result<AcpV1RequestId, AcpV1CodecError> {
    match id {
        RequestId::Null => Err(AcpV1CodecError::NullRequestId),
        RequestId::Number(value) => Ok(AcpV1RequestId::Number(value)),
        RequestId::Str(value) => Ok(AcpV1RequestId::String(value)),
    }
}

fn to_sdk_request_id(id: &AcpV1RequestId) -> RequestId {
    match id {
        AcpV1RequestId::Number(value) => RequestId::Number(*value),
        AcpV1RequestId::String(value) => RequestId::Str(value.clone()),
    }
}

fn copy_capabilities(capabilities: &AgentCapabilities) -> AcpV1AgentCapabilities {
    AcpV1AgentCapabilities {
        load_session: capabilities.load_session,
        list_sessions: capabilities.session_capabilities.list.is_some(),
        delete_session: capabilities.session_capabilities.delete.is_some(),
        additional_directories: capabilities
            .session_capabilities
            .additional_directories
            .is_some(),
        resume_session: capabilities.session_capabilities.resume.is_some(),
        close_session: capabilities.session_capabilities.close.is_some(),
        image_prompts: capabilities.prompt_capabilities.image,
        audio_prompts: capabilities.prompt_capabilities.audio,
        embedded_context: capabilities.prompt_capabilities.embedded_context,
    }
}

fn copy_stop_reason(reason: StopReason) -> AcpV1StopReason {
    match reason {
        StopReason::EndTurn => AcpV1StopReason::EndTurn,
        StopReason::MaxTokens => AcpV1StopReason::MaxTokens,
        StopReason::MaxTurnRequests => AcpV1StopReason::MaxTurnRequests,
        StopReason::Refusal => AcpV1StopReason::Refusal,
        StopReason::Cancelled => AcpV1StopReason::Cancelled,
        _ => AcpV1StopReason::Unsupported,
    }
}

fn copy_permission_kind(kind: PermissionOptionKind) -> AcpV1PermissionOptionKind {
    match kind {
        PermissionOptionKind::AllowOnce => AcpV1PermissionOptionKind::AllowOnce,
        PermissionOptionKind::AllowAlways => AcpV1PermissionOptionKind::AllowAlways,
        PermissionOptionKind::RejectOnce => AcpV1PermissionOptionKind::RejectOnce,
        PermissionOptionKind::RejectAlways => AcpV1PermissionOptionKind::RejectAlways,
        _ => AcpV1PermissionOptionKind::Unsupported,
    }
}

const CODEX_ADAPTER_NAME: &str = "@agentclientprotocol/codex-acp";
const CODEX_ADAPTER_VERSION: &str = "1.6.2";
const MAX_SESSION_FAILURE_ID_BYTES: usize = 1024;
const MAX_SESSION_FAILURE_TITLE_BYTES: usize = 16 * 1024;
const MAX_SESSION_FAILURE_DETAILS_BYTES: usize = 64 * 1024;
const MAX_SESSION_FAILURE_ACTIONS: usize = 3;

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CodexSessionFailure {
    id: String,
    revision: u64,
    category: CodexSessionFailureCategory,
    severity: CodexSessionFailureSeverity,
    title: String,
    details: Option<String>,
    actions: Vec<CodexSessionFailureAction>,
}

#[derive(Debug, Clone, Copy, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum CodexSessionFailureCategory {
    Connection,
    Access,
    Limit,
    Request,
    Service,
    Unknown,
}

#[derive(Debug, Clone, Copy, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum CodexSessionFailureSeverity {
    Warning,
    Error,
}

#[derive(Debug, Clone, Copy, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum CodexSessionFailureAction {
    Retry,
    Login,
    NewSession,
}

fn validate_codex_initialize(
    response: &InitializeResponse,
    raw: &serde_json::Value,
) -> Result<(), AcpV1CodecError> {
    let actual_name = response.agent_info.as_ref().map(|info| info.name.clone());
    let actual_version = response
        .agent_info
        .as_ref()
        .map(|info| info.version.clone());
    if actual_name.as_deref() != Some(CODEX_ADAPTER_NAME)
        || actual_version.as_deref() != Some(CODEX_ADAPTER_VERSION)
    {
        return Err(AcpV1CodecError::CodexAdapterIdentityMismatch {
            name: actual_name,
            version: actual_version,
        });
    }

    let air = raw
        .pointer("/_meta/jetbrains/air")
        .and_then(serde_json::Value::as_object)
        .ok_or(AcpV1CodecError::InvalidCodexSessionFailureNegotiation(
            "missing _meta.jetbrains.air object",
        ))?;
    if air.len() != 2 || !air.contains_key("version") || !air.contains_key("capabilities") {
        return Err(AcpV1CodecError::InvalidCodexSessionFailureNegotiation(
            "AIR v1 capability object drifted",
        ));
    }
    if air.get("version").and_then(serde_json::Value::as_u64) != Some(1) {
        return Err(AcpV1CodecError::InvalidCodexSessionFailureNegotiation(
            "AIR extension version is not exactly 1",
        ));
    }
    let capabilities = air
        .get("capabilities")
        .and_then(serde_json::Value::as_array)
        .ok_or(AcpV1CodecError::InvalidCodexSessionFailureNegotiation(
            "AIR capabilities is not an array",
        ))?;
    let frozen_capabilities = ["sessionFailure", "agentFileChangeReport"];
    if capabilities.len() != frozen_capabilities.len()
        || !capabilities
            .iter()
            .zip(frozen_capabilities)
            .all(|(actual, expected)| actual.as_str() == Some(expected))
    {
        return Err(AcpV1CodecError::InvalidCodexSessionFailureNegotiation(
            "AIR capabilities do not match Codex 1.6.2",
        ));
    }
    Ok(())
}

fn parse_codex_session_failure(
    carrier: &serde_json::Value,
) -> Result<Option<CodexSessionFailure>, AcpV1CodecError> {
    let mut occurrences = Vec::new();
    collect_named_key(carrier, "sessionFailure", "", &mut occurrences);
    if occurrences.is_empty() {
        return Ok(None);
    }
    if occurrences.as_slice() != ["/_meta/jetbrains/air/sessionFailure"] {
        return Err(AcpV1CodecError::InvalidCodexSessionFailure(
            "sessionFailure moved outside _meta.jetbrains.air",
        ));
    }

    let air = carrier
        .pointer("/_meta/jetbrains/air")
        .and_then(serde_json::Value::as_object)
        .ok_or(AcpV1CodecError::InvalidCodexSessionFailure(
            "missing AIR v1 envelope",
        ))?;
    if air.len() != 2 || !air.contains_key("version") || !air.contains_key("sessionFailure") {
        return Err(AcpV1CodecError::InvalidCodexSessionFailure(
            "AIR v1 failure envelope drifted",
        ));
    }
    if air.get("version").and_then(serde_json::Value::as_u64) != Some(1) {
        return Err(AcpV1CodecError::InvalidCodexSessionFailure(
            "AIR failure version is not exactly 1",
        ));
    }
    let failure: CodexSessionFailure = serde_json::from_value(
        air.get("sessionFailure")
            .cloned()
            .ok_or(AcpV1CodecError::InvalidCodexSessionFailure(
                "missing sessionFailure record",
            ))?,
    )
    .map_err(|_| AcpV1CodecError::InvalidCodexSessionFailure("record schema drifted"))?;
    validate_codex_session_failure_bounds(&failure)?;
    Ok(Some(failure))
}

fn collect_named_key(
    value: &serde_json::Value,
    key: &str,
    path: &str,
    occurrences: &mut Vec<String>,
) {
    match value {
        serde_json::Value::Object(object) => {
            for (name, child) in object {
                let child_path = format!("{path}/{}", escape_json_pointer(name));
                if name == key {
                    occurrences.push(child_path.clone());
                }
                collect_named_key(child, key, &child_path, occurrences);
            }
        }
        serde_json::Value::Array(array) => {
            for (index, child) in array.iter().enumerate() {
                collect_named_key(child, key, &format!("{path}/{index}"), occurrences);
            }
        }
        _ => {}
    }
}

fn escape_json_pointer(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

fn validate_codex_session_failure_bounds(
    failure: &CodexSessionFailure,
) -> Result<(), AcpV1CodecError> {
    let _ = failure.category;
    if failure.id.is_empty() || failure.id.len() > MAX_SESSION_FAILURE_ID_BYTES {
        return Err(AcpV1CodecError::InvalidCodexSessionFailure(
            "id is empty or exceeds 1024 bytes",
        ));
    }
    if failure.revision == 0 {
        return Err(AcpV1CodecError::InvalidCodexSessionFailure(
            "revision is not a positive integer",
        ));
    }
    if failure.title.is_empty() || failure.title.len() > MAX_SESSION_FAILURE_TITLE_BYTES {
        return Err(AcpV1CodecError::InvalidCodexSessionFailure(
            "title is empty or exceeds 16384 bytes",
        ));
    }
    if failure
        .details
        .as_ref()
        .is_some_and(|details| details.is_empty() || details.len() > MAX_SESSION_FAILURE_DETAILS_BYTES)
    {
        return Err(AcpV1CodecError::InvalidCodexSessionFailure(
            "details is empty or exceeds 65536 bytes",
        ));
    }
    if failure.actions.len() > MAX_SESSION_FAILURE_ACTIONS {
        return Err(AcpV1CodecError::InvalidCodexSessionFailure(
            "actions exceeds the AIR v1 action set",
        ));
    }
    for (index, action) in failure.actions.iter().enumerate() {
        if failure.actions[..index].contains(action) {
            return Err(AcpV1CodecError::InvalidCodexSessionFailure(
                "actions contains a duplicate",
            ));
        }
    }
    Ok(())
}

const _: () = assert!(ProtocolVersion::V1.as_u16() == ACP_WIRE_PROTOCOL_VERSION);
