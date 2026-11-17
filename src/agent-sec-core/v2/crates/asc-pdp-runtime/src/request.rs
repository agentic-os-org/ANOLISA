//! Maps typed OS requests into schema-checked Cedar request and entity data.

use asc_daemon_protocol::authorization::OsAuthorizationRequest;
use cedar_policy::{Context, Entities, EntityUid, Request, Schema};
use serde_json::{Value, json};

use crate::PdpError;

// shortcut: use one synthetic subject until authenticated Agent identities are supported.
const SUBJECT_ID: &str = "request";

pub(crate) struct Query {
    pub request: Request,
    pub entities: Entities,
}

pub(crate) fn invocation(
    request: &OsAuthorizationRequest,
    schema: &Schema,
    command: bool,
) -> Result<Query, PdpError> {
    let executable = request.context.launch.resolved_program.as_str();
    let cwd = request.context.launch.cwd.as_str();
    let args: Vec<_> = request
        .context
        .launch
        .args
        .iter()
        .enumerate()
        .map(|(index, value)| json!({"index": index, "value": value}))
        .collect();
    let context = json!({"input": {"cwd": cwd, "arg_count": args.len(), "args": args}});
    if command {
        query(
            request,
            schema,
            "process.spawn",
            "Process",
            &request.context.launch.program,
            &json!({"program": request.context.launch.program, "resolved_program": executable}),
            context,
        )
    } else {
        query(
            request,
            schema,
            "file.exec",
            "File",
            executable,
            &json!({"path": executable}),
            context,
        )
    }
}

pub(crate) fn file(
    request: &OsAuthorizationRequest,
    schema: &Schema,
    path: &str,
    write: bool,
) -> Result<Query, PdpError> {
    let context = if write {
        json!({"input": {"program": request.context.launch.resolved_program,
            "operation": {"__entity": {"type": "SecCore::WriteOperation", "id": "write"}}}})
    } else {
        json!({"input": {"program": request.context.launch.resolved_program}})
    };
    query(
        request,
        schema,
        if write { "file.write" } else { "file.read" },
        "File",
        path,
        &json!({"path": path}),
        context,
    )
}

pub(crate) fn network(
    request: &OsAuthorizationRequest,
    schema: &Schema,
) -> Result<Query, PdpError> {
    query(
        request,
        schema,
        "network.use",
        "Network",
        "all",
        &json!({}),
        json!({}),
    )
}

fn query(
    request: &OsAuthorizationRequest,
    schema: &Schema,
    action: &str,
    resource_type: &str,
    resource_id: &str,
    attributes: &Value,
    context: Value,
) -> Result<Query, PdpError> {
    let action = uid("Action", action)?;
    let context = Context::from_json_value(context, Some((schema, &action)))
        .map_err(|error| PdpError::Evaluation(error.to_string()))?;
    let mut subject_attributes = serde_json::Map::new();
    if let Some(agent_type) = &request.context.agent.agent_type {
        subject_attributes.insert("agent_type".into(), json!(agent_type));
    }
    let entities = Entities::from_json_value(
        json!([
            {"uid": {"type": "SecCore::Subject", "id": SUBJECT_ID},
             "attrs": subject_attributes, "parents": []},
            {"uid": {"type": format!("SecCore::{resource_type}"), "id": resource_id},
             "attrs": attributes, "parents": []}
        ]),
        Some(schema),
    )
    .map_err(|error| PdpError::Evaluation(error.to_string()))?;
    let request = Request::new(
        uid("Subject", SUBJECT_ID)?,
        action,
        uid(resource_type, resource_id)?,
        context,
        Some(schema),
    )
    .map_err(|error| PdpError::Evaluation(error.to_string()))?;
    Ok(Query { request, entities })
}

fn uid(kind: &str, id: &str) -> Result<EntityUid, PdpError> {
    let quoted =
        serde_json::to_string(id).map_err(|error| PdpError::Evaluation(error.to_string()))?;
    format!("SecCore::{kind}::{quoted}")
        .parse()
        .map_err(|error: cedar_policy::ParseErrors| PdpError::Evaluation(error.to_string()))
}
