#[derive(Serialize)]
struct InitializeInput<'a> {
    #[serde(rename = "type")]
    message_type: &'static str,
    request_id: &'a str,
    request: InitializeRequest<'a>,
}

#[derive(Serialize)]
struct InitializeRequest<'a> {
    subtype: &'static str,
    fire_session_start: bool,
    protocol_version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    execution_profile: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    capability_profile: Option<&'a GatewayCapabilityProfileIdentity>,
}

#[derive(Serialize)]
struct ApprovalReceiptInput<'a> {
    #[serde(rename = "type")]
    message_type: &'static str,
    request_id: &'a str,
}

#[derive(Serialize)]
struct BrokeredControlResponseInput<'a> {
    #[serde(rename = "type")]
    message_type: &'static str,
    response: BrokeredControlResponse<'a>,
}

#[derive(Serialize)]
struct BrokeredControlResponse<'a> {
    subtype: &'static str,
    request_id: &'a str,
    response: BrokeredControlResponseBody<'a>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum BrokeredControlResponseBody<'a> {
    Allow {
        behavior: &'static str,
    },
    Deny {
        behavior: &'static str,
        message: &'a str,
    },
    Answer {
        behavior: &'static str,
        answer: &'a str,
    },
}

#[derive(Serialize)]
struct BrokeredCheckpointControlResponseInput<'a> {
    #[serde(rename = "type")]
    message_type: &'static str,
    response: BrokeredCheckpointControlResponse<'a>,
}

#[derive(Serialize)]
struct BrokeredCheckpointControlResponse<'a> {
    subtype: &'static str,
    request_id: &'a str,
    response: BrokeredCheckpointControlResponseBody<'a>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum BrokeredCheckpointControlResponseBody<'a> {
    Deny {
        behavior: &'static str,
        message: &'a str,
    },
    Created {
        behavior: &'static str,
        #[serde(rename = "checkpointResult")]
        checkpoint_result: &'a cosh_gateway_contracts::runtime::WorkspaceCheckpointCreateV1Result,
    },
    Error {
        behavior: &'static str,
        #[serde(rename = "checkpointError")]
        checkpoint_error: BrokeredCheckpointError<'a>,
    },
}

#[derive(Serialize)]
struct BrokeredCheckpointError<'a> {
    outcome: &'static str,
    code: &'a str,
    message: &'a str,
}

#[derive(Serialize)]
struct SimpleControlInput<'a> {
    #[serde(rename = "type")]
    message_type: &'static str,
    request_id: &'a str,
    request: SimpleControlRequest,
}

#[derive(Serialize)]
struct SimpleControlRequest {
    subtype: &'static str,
}

#[derive(Serialize)]
struct UserInput<'a> {
    #[serde(rename = "type")]
    message_type: &'static str,
    message: UserInputBody<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    shell_context: Option<&'a CoshCoreShellContext>,
}

#[derive(Serialize)]
struct UserInputBody<'a> {
    role: &'static str,
    content: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    raw_user_input: Option<&'a str>,
}

#[derive(Deserialize)]
struct WireControlResponseEnvelope {
    response: WireInitializeResponse,
}

#[derive(Deserialize)]
struct WireInitializeResponse {
    subtype: String,
    request_id: String,
    response: WireInitializeBody,
}

#[derive(Deserialize)]
struct WireInitializeBody {
    subtype: String,
    #[serde(default)]
    protocol_version: Option<u32>,
    #[serde(default)]
    execution_profile: Option<String>,
    #[serde(default)]
    capability_profile: Option<GatewayCapabilityProfileIdentity>,
    #[serde(default)]
    runtime_tools: Option<Vec<String>>,
    #[serde(default)]
    capabilities: Option<CoshCoreCapabilities>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Deserialize)]
struct WireStreamEnvelope {
    event: CoshCoreStreamEvent,
}

#[derive(Deserialize)]
struct WireUserOutput {
    #[serde(rename = "session_id")]
    provider_session_id: String,
    message: WireUserBody,
}

#[derive(Deserialize)]
struct WireUserBody {
    content: Vec<WireUserContent>,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum WireUserContent {
    #[serde(rename = "tool_result")]
    ToolResult(CoshCoreToolResult),
}

#[derive(Deserialize)]
struct WireControlRequestEnvelope {
    request_id: String,
    request: CoshCoreControlRequest,
}

#[derive(Deserialize)]
struct WireGenericControlResponseEnvelope {
    response: WireGenericControlResponse,
}

#[derive(Deserialize)]
struct WireGenericControlResponse {
    subtype: String,
    request_id: String,
    response: Value,
}

#[derive(Deserialize)]
struct WireRegistryResponse {
    request_id: String,
    success: bool,
    #[serde(default)]
    data: Option<Value>,
    #[serde(default)]
    error: Option<String>,
}
