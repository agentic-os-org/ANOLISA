//! Correlate external responses without granting execution or adoption authority.

use crate::{parsing, Error, EventGate, REQUEST_SCHEMA, RESPONSE_SCHEMA};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, sync::OnceLock};

/// A structurally checked request; it does not grant the requested effects.
pub struct Request(Value);

impl Request {
    /// Borrows the request to serialize it on a Host-owned transport.
    pub fn as_value(&self) -> &Value {
        &self.0
    }
}

/// Unauthenticated Provider claims, distinct from a Core provider descriptor.
#[derive(Clone)]
pub struct Description(pub(crate) Value);

impl Description {
    /// Borrows the checked description response, including its operations.
    pub fn as_value(&self) -> &Value {
        &self.0
    }
}

/// Private configuration acknowledged by a correlated Provider response.
///
/// The Host must associate this response with the configured Provider process
/// and invalidate it when that process identity or configuration changes.
#[derive(Clone)]
pub struct ValidatedConfig(pub(crate) Value);

impl ValidatedConfig {
    /// Borrows the exact JSON configuration from the acknowledged request.
    pub fn as_value(&self) -> &Value {
        &self.0
    }
}

/// Protocol-checked evidence supplied by the caller for one configured Provider.
///
/// The caller owns discovery and must associate both responses with the same
/// configured Provider. These values do not establish process identity or prove
/// that an Agent has installed a hook.
#[derive(Clone)]
pub struct ProviderEvidence {
    /// The operations, events and effects declared by the Provider.
    pub description: Description,
    /// The exact private configuration accepted by that Provider.
    pub configuration: ValidatedConfig,
}

/// Checked candidate effects or an executor acknowledgement from one invocation.
pub struct Outcome(Value);

impl Outcome {
    /// Borrows the validated response; reason codes remain Provider-owned data.
    pub fn as_value(&self) -> &Value {
        &self.0
    }

    /// Retains only observations, for a step configured to run in shadow.
    pub fn observe_only(&mut self) {
        if let Some(effects) = self.0["effects"].as_array_mut() {
            effects.retain(|effect| effect["type"] == "observe");
        }
    }

    /// Projects a validated decision into the existing native block effect.
    ///
    /// The Host calls this only after block capability admission and completion
    /// of any required actions. An approval request blocks until approval is supported.
    pub fn apply_decision(&mut self) {
        let reason = match self.0["decision"].as_str() {
            Some("block") => "provider_decision_block",
            Some("ask") => "provider_decision_approval_required",
            _ => return,
        };
        if let Some(effects) = self.0["effects"].as_array_mut() {
            effects.push(json!({"type":"block","reason_code":reason}));
        }
    }

    /// Whether the Provider requested blocking at this decision or pre-tool gate.
    ///
    /// False adds no restriction; it does not authorize native execution or delivery.
    pub fn requests_block(&self) -> bool {
        self.0["effects"]
            .as_array()
            .is_some_and(|effects| effects.iter().any(|effect| effect["type"] == "block"))
    }
}

/// Successful response classified by the method of its correlated request.
pub enum Reply {
    /// Provider operation and effect declarations.
    Description(Description),
    /// Acknowledged private configuration.
    Configuration(ValidatedConfig),
    /// Candidate effects for an evaluation, or the correlated acknowledgement of an action.
    Invocation(Outcome),
}

/// Reusable, offline schema and cross-message validator.
pub struct Protocol {
    schemas: &'static Schemas,
}

struct Schemas {
    request: jsonschema::Validator,
    response: jsonschema::Validator,
    event: jsonschema::Validator,
}

impl Protocol {
    /// Reuses process-wide bundled schemas without resolving external resources.
    /// Message correlation and validation still run for every request and reply.
    ///
    /// # Errors
    /// Returns [`Error::InvalidSchema`] if a bundled schema is invalid.
    pub fn new() -> Result<Self, Error> {
        static SCHEMAS: OnceLock<Result<Schemas, ()>> = OnceLock::new();
        let schemas = SCHEMAS
            .get_or_init(|| Self::compile().map_err(|_| ()))
            .as_ref()
            .map_err(|_| Error::InvalidSchema)?;
        Ok(Self { schemas })
    }

    fn compile() -> Result<Schemas, Error> {
        fn compile(schema: &Value) -> Result<jsonschema::Validator, Error> {
            jsonschema::draft202012::options()
                .offline()
                .build(schema)
                .map_err(|_| Error::InvalidSchema)
        }
        let request: Value =
            serde_json::from_str(REQUEST_SCHEMA).map_err(|_| Error::InvalidSchema)?;
        let response: Value =
            serde_json::from_str(RESPONSE_SCHEMA).map_err(|_| Error::InvalidSchema)?;
        Ok(Schemas {
            request: compile(&request)?,
            response: compile(&response)?,
            event: compile(
                request
                    .pointer("/oneOf/2/properties/event")
                    .ok_or(Error::InvalidSchema)?,
            )?,
        })
    }

    /// Validate a normalized event without constructing or invoking a Provider request.
    ///
    /// # Errors
    /// Rejects size/depth overflow, unknown or missing fields, malformed identities
    /// and a tool result supplied before execution. Bare-event limits apply.
    pub fn validate_event(&self, event: &Value) -> Result<(), Error> {
        encode(event)?;
        if !self.schemas.event.is_valid(event) {
            return Err(Error::Invalid("event schema mismatch"));
        }
        check_event_result(event)
    }

    /// Parses one JSON request, rejecting ambiguity and unsupported invocation effects.
    ///
    /// # Errors
    /// Rejects size/depth overflow, duplicates, unknown fields, malformed identities,
    /// unsupported effects and a mismatched invocation binding. This checks protocol
    /// consistency; callers still perform configuration and Adapter admission.
    pub fn parse_request(&self, bytes: &[u8]) -> Result<Request, Error> {
        let value = parsing::parse(bytes)?;
        self.check_request_shape(&value)?;
        if matches!(
            value["method"].as_str(),
            Some("invoke" | "execute_obligation")
        ) && value["input_digest"] != binding(&value)?
        {
            return Err(Error::Invalid("invocation binding mismatch"));
        }
        Ok(Request(value))
    }

    fn check_request_shape(&self, value: &Value) -> Result<(), Error> {
        if !self.schemas.request.is_valid(value) {
            return Err(Error::Invalid("request schema mismatch"));
        }
        if value["method"] == "invoke" {
            let gate = value["event"]["name"]
                .as_str()
                .and_then(EventGate::for_event)
                .ok_or(Error::Invalid("invocation event is not implemented"))?;
            for effect in array(&value["allowed_effects"])? {
                if !effect
                    .as_str()
                    .is_some_and(|effect| gate.allows_effect(effect))
                {
                    return Err(Error::Invalid("invocation effect is not implemented"));
                }
            }
            check_event_result(&value["event"])?;
            if value["decision"] == true && !gate.allows_effect("block") {
                return Err(Error::Invalid("event does not support Provider decisions"));
            }
        }
        if value["method"] == "execute_obligation" {
            if !value["event_name"]
                .as_str()
                .and_then(EventGate::for_event)
                .is_some_and(|gate| gate.allows_effect("block"))
            {
                return Err(Error::Invalid(
                    "event does not support Provider obligations",
                ));
            }
            check_obligation(&value["obligation"])?;
        }
        if let Some(context) = value.get("binding") {
            if context["config_revision"] != value["config_revision"]
                || (value["method"] == "invoke"
                    && context["instance_id"] != value["event"]["agent"]["instance_id"])
            {
                return Err(Error::Invalid("authoritative context mismatch"));
            }
        }
        Ok(())
    }

    /// Binds a locally constructed invoke request before its digest field is added.
    ///
    /// The digest is an opaque external protocol binding, not a canonical Core
    /// input digest. Providers echo it without re-encoding or hashing the payload.
    ///
    /// # Errors
    /// Rejects an existing digest, a different method or an invalid request. Use
    /// [`Self::parse_request`] for untrusted bytes so duplicate fields remain visible.
    pub fn bind_invocation(&self, value: Value) -> Result<Request, Error> {
        self.bind_request(value, "invoke")
    }

    /// Binds a locally authorized action to its event identity and exact request bytes.
    ///
    /// # Errors
    /// Rejects incorrect methods, malformed obligations and invalid or oversized context.
    pub fn bind_obligation(&self, value: Value) -> Result<Request, Error> {
        self.bind_request(value, "execute_obligation")
    }

    fn bind_request(&self, mut value: Value, method: &str) -> Result<Request, Error> {
        if value["method"] != method || value.get("input_digest").is_some() {
            return Err(Error::Invalid("expected an unbound Provider request"));
        }
        let digest = binding(&value)?;
        let object = value
            .as_object_mut()
            .ok_or(Error::Invalid("expected request object"))?;
        object.insert("input_digest".into(), Value::String(digest));
        self.parse_request(&encode(&value)?)
    }

    /// Binds a local boolean Hook invocation, applying limits to its bare event.
    ///
    /// The validated envelope stays inside AW; only the event is sent to the
    /// command. Envelope fields must not consume the event's size/depth quota.
    /// Structured Provider wire requests retain their full-message limits.
    ///
    /// # Errors
    /// Rejects an existing digest, an operation other than invoke, nonempty
    /// configuration, invalid request fields or an event exceeding its limits.
    pub fn bind_hook_invocation(&self, mut value: Value) -> Result<Request, Error> {
        if value["method"] != "invoke"
            || value["operation"] != "invoke"
            || value.get("binding").is_some()
            || value.get("decision").is_some()
            || value.get("input_digest").is_some()
            || !value["config"]
                .as_object()
                .is_some_and(|config| config.is_empty())
        {
            return Err(Error::Invalid(
                "expected an unbound boolean Hook invocation",
            ));
        }
        encode(&value["event"])?;
        let object = value
            .as_object_mut()
            .ok_or(Error::Invalid("expected request object"))?;
        object.insert(
            "input_digest".into(),
            Value::String(format!("sha256:{}", "0".repeat(64))),
        );
        self.check_request_shape(&value)?;
        value
            .as_object_mut()
            .ok_or(Error::Invalid("expected request object"))?
            .remove("input_digest");
        // Event limits were checked above; the schema bounds the remaining
        // envelope fields, and Hook configuration must be empty before encoding.
        let bytes =
            serde_json::to_vec(&value).map_err(|_| Error::Invalid("request encoding failed"))?;
        value["input_digest"] = Value::String(format!("sha256:{:x}", Sha256::digest(bytes)));
        Ok(Request(value))
    }

    /// Checks one stdout response against its exact request after a successful exit.
    ///
    /// # Errors
    /// Rejects invalid shape, wrong method payload, identity/binding mismatches,
    /// duplicate operations and effects outside the request. A correlated error
    /// response returns [`Error::ProviderFailure`], never a successful block.
    /// Hosts must first enforce process exit, deadline and output limits; this
    /// function does not see process state, authenticate origin or record a receipt.
    pub fn check_response(&self, request: &Request, bytes: &[u8]) -> Result<Reply, Error> {
        let value = parsing::parse(bytes)?;
        if !self.schemas.response.is_valid(&value) {
            return Err(Error::Invalid("response schema mismatch"));
        }
        if value["request_id"] != request.0["request_id"] {
            return Err(Error::Invalid("response identity mismatch"));
        }
        if value["status"] == "error" {
            return Err(Error::ProviderFailure {
                code: value["error_code"]
                    .as_str()
                    .ok_or(Error::Invalid("missing error code"))?
                    .into(),
            });
        }
        match request.0["method"].as_str() {
            Some("describe") => {
                let mut names = BTreeSet::new();
                for operation in array(&value["operations"])? {
                    if !names.insert(operation["name"].as_str()) {
                        return Err(Error::Invalid("duplicate operation name"));
                    }
                    if operation["decision"] == true && operation.get("obligation_kinds").is_some()
                    {
                        return Err(Error::Invalid(
                            "operation cannot decide and execute obligations",
                        ));
                    }
                    if let Some(constraints) = operation["event_adapters"].as_object() {
                        let events = array(&operation["events"])?;
                        if constraints
                            .keys()
                            .any(|event| !events.iter().any(|v| v == event))
                        {
                            return Err(Error::Invalid(
                                "Adapter constraint references an undeclared event",
                            ));
                        }
                    }
                }
                Ok(Reply::Description(Description(value)))
            }
            Some("validate_config") => {
                if value.get("operations").is_some() || value.get("input_digest").is_some() {
                    return Err(Error::Invalid("unexpected validation response payload"));
                }
                Ok(Reply::Configuration(ValidatedConfig(
                    request.0["config"].clone(),
                )))
            }
            Some("invoke") => {
                if value["input_digest"] != request.0["input_digest"] {
                    return Err(Error::Invalid("response binding mismatch"));
                }
                let gate = request.0["event"]["name"]
                    .as_str()
                    .and_then(EventGate::for_event)
                    .ok_or(Error::Invalid("invocation event is not implemented"))?;
                let effects = array(&value["effects"])?;
                if request.0["decision"] == true {
                    if value.get("decision").is_none()
                        || effects.iter().any(|effect| effect["type"] != "observe")
                    {
                        return Err(Error::Invalid("expected an advisory decision response"));
                    }
                    if let Some(obligations) = value.get("obligations") {
                        let mut kinds = BTreeSet::new();
                        for obligation in array(obligations)? {
                            check_obligation(obligation)?;
                            if !kinds.insert(obligation["kind"].as_str()) {
                                return Err(Error::Invalid("duplicate obligation kind"));
                            }
                        }
                        if value["decision"] == "abstain" {
                            return Err(Error::Invalid("abstention cannot request obligations"));
                        }
                    }
                } else if value.get("decision").is_some() || value.get("obligations").is_some() {
                    return Err(Error::Invalid("unsolicited decision response"));
                }
                let mut replacements = 0;
                for effect in effects {
                    let Some(kind) = effect["type"].as_str() else {
                        return Err(Error::Invalid("expected effect type"));
                    };
                    if !gate.allows_effect(kind) {
                        return Err(Error::Invalid("response effect violates event gate"));
                    }
                    if kind == "replace_input"
                        && value["effects"].as_array().map(Vec::len) != Some(1)
                    {
                        return Err(Error::Invalid(
                            "input replacement must be a sole pre-tool effect",
                        ));
                    }
                    if kind == "replace_input"
                        && request.0["event"]["tool"]["input"]
                            .as_object()
                            .zip(effect["input"].as_object())
                            .is_none_or(|(original, candidate)| {
                                original.keys().any(|key| !candidate.contains_key(key))
                            })
                    {
                        return Err(Error::Invalid(
                            "input replacement must retain original argument keys",
                        ));
                    }
                    if kind == "replace_result" {
                        replacements += 1;
                        if replacements > 1 {
                            return Err(Error::Invalid("multiple result replacements"));
                        }
                        if request.0["event"]["tool"]
                            .get("result_projection")
                            .is_none()
                        {
                            return Err(Error::Invalid(
                                "replacement response requires a result projection",
                            ));
                        }
                    }
                    if !array(&request.0["allowed_effects"])?.contains(&effect["type"]) {
                        return Err(Error::Invalid("response effect was not admitted"));
                    }
                }
                Ok(Reply::Invocation(Outcome(value)))
            }
            Some("execute_obligation") => {
                if value["input_digest"] != request.0["input_digest"]
                    || value.get("obligation_status").is_none()
                {
                    return Err(Error::Invalid(
                        "expected a correlated obligation acknowledgement",
                    ));
                }
                Ok(Reply::Invocation(Outcome(value)))
            }
            _ => Err(Error::Invalid("unknown request method")),
        }
    }
}

fn check_obligation(value: &Value) -> Result<(), Error> {
    if encode(value)?.len() > 4096 {
        return Err(Error::Invalid("obligation size limit"));
    }
    Ok(())
}

fn check_event_result(event: &Value) -> Result<(), Error> {
    if event["name"] == "tool.before"
        && (!event["tool"]["result"].is_null() || event["tool"].get("result_projection").is_some())
    {
        return Err(Error::Invalid("tool.before cannot supply a tool result"));
    }
    Ok(())
}

fn array(value: &Value) -> Result<&Vec<Value>, Error> {
    value.as_array().ok_or(Error::Invalid("expected array"))
}

fn encode(value: &Value) -> Result<Vec<u8>, Error> {
    // Value callers are local, but enforce the same recursion ceiling before
    // invoking serde's recursive encoder or cloning the request for hashing.
    let mut pending = vec![(value, 0)];
    while let Some((value, depth)) = pending.pop() {
        if depth > crate::MAX_DEPTH {
            return Err(Error::Invalid("message nesting limit"));
        }
        match value {
            Value::Object(values) => pending.extend(values.values().map(|v| (v, depth + 1))),
            Value::Array(values) => pending.extend(values.iter().map(|v| (v, depth + 1))),
            _ => {}
        }
    }
    let bytes = serde_json::to_vec(value).map_err(|_| Error::Invalid("invalid JSON value"))?;
    if bytes.len() > crate::MAX_MESSAGE_BYTES {
        return Err(Error::Invalid("message size limit"));
    }
    Ok(bytes)
}

fn binding(value: &Value) -> Result<String, Error> {
    encode(value)?;
    let mut value = value.clone();
    value
        .as_object_mut()
        .ok_or(Error::Invalid("expected request object"))?
        .remove("input_digest");
    Ok(format!("sha256:{:x}", Sha256::digest(encode(&value)?)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn independent_protocols_share_only_immutable_schemas() {
        let first = Protocol::new().unwrap();
        std::thread::scope(|scope| {
            let second = scope.spawn(|| Protocol::new().unwrap()).join().unwrap();
            assert!(std::ptr::eq(first.schemas, second.schemas));
            for protocol in [&first, &second] {
                assert!(protocol.parse_request(br#"{"method":"invoke"}"#).is_err());
                assert!(protocol.validate_event(&serde_json::json!({})).is_err());
            }
        });
    }
}
