//! Maps typed OS requests into schema-checked Cedar request and entity data.

use asc_policy_types::authorization::AuthorizationRequest;
use cedar_policy::{Context, Entities, EntityUid, Request, Schema};
use serde_json::{Value, json};

use crate::PdpError;

pub(crate) struct Query {
    pub request: Request,
    pub entities: Entities,
}

pub(crate) fn invocation(
    request: &AuthorizationRequest,
    schema: &Schema,
    command: bool,
) -> Result<Query, PdpError> {
    let executable = canonical_fact(request, &request.launch.resolved_program)?;
    let cwd = canonical_fact(request, &request.launch.cwd)?;
    let args: Vec<_> = request
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
            &request.launch.program,
            &json!({"program": request.launch.program, "resolved_program": executable}),
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
    request: &AuthorizationRequest,
    schema: &Schema,
    path: &str,
    write: bool,
) -> Result<Query, PdpError> {
    let context = if write {
        json!({"input": {"program": request.launch.resolved_program,
            "operation": {"__entity": {"type": "SecCore::WriteOperation", "id": "write"}}}})
    } else {
        json!({"input": {"program": request.launch.resolved_program}})
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

pub(crate) fn network(request: &AuthorizationRequest, schema: &Schema) -> Result<Query, PdpError> {
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
    request: &AuthorizationRequest,
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
    let entities = Entities::from_json_value(
        json!([
            {"uid": {"type": "SecCore::Subject", "id": request.principal.agent_id},
             "attrs": {"agent_type": request.principal.agent_type}, "parents": []},
            {"uid": {"type": format!("SecCore::{resource_type}"), "id": resource_id},
             "attrs": attributes, "parents": []}
        ]),
        Some(schema),
    )
    .map_err(|error| PdpError::Evaluation(error.to_string()))?;
    let request = Request::new(
        uid("Subject", &request.principal.agent_id)?,
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

fn canonical_fact<'a>(request: &'a AuthorizationRequest, path: &str) -> Result<&'a str, PdpError> {
    request
        .path_facts
        .iter()
        .find(|fact| fact.path == path)
        .map(|fact| fact.canonical_path.as_str())
        .ok_or_else(|| PdpError::MissingFacts(vec![path.to_owned()]))
}
