//! Correlate external responses without granting execution or adoption authority.

use crate::{parsing, Error, REQUEST_SCHEMA, RESPONSE_SCHEMA};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

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

/// Candidate effects from one invocation, never a receipt or adoption record.
pub struct Outcome(Value);

impl Outcome {
    /// Borrows the validated response; reason codes remain Provider-owned data.
    pub fn as_value(&self) -> &Value {
        &self.0
    }

    /// Whether the Provider requested blocking this tool attempt.
    ///
    /// False adds no restriction; it does not authorize the native tool.
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
    /// Candidate effects for this invocation.
    Invocation(Outcome),
}

/// Reusable, offline schema and cross-message validator.
pub struct Protocol {
    request: jsonschema::Validator,
    response: jsonschema::Validator,
}

impl Protocol {
    /// Compiles the bundled schemas without resolving external resources.
    ///
    /// # Errors
    /// Returns [`Error::InvalidSchema`] if a bundled schema is invalid.
    pub fn new() -> Result<Self, Error> {
        fn compile(schema: &str) -> Result<jsonschema::Validator, Error> {
            let schema: Value = serde_json::from_str(schema).map_err(|_| Error::InvalidSchema)?;
            jsonschema::draft202012::options()
                .offline()
                .build(&schema)
                .map_err(|_| Error::InvalidSchema)
        }
        Ok(Self {
            request: compile(REQUEST_SCHEMA)?,
            response: compile(RESPONSE_SCHEMA)?,
        })
    }

    /// Parses one JSON request, rejecting ambiguity and unsupported invocation effects.
    ///
    /// # Errors
    /// Rejects size/depth overflow, duplicates, unknown fields, malformed identities,
    /// unsupported effects and a mismatched invocation binding. This checks protocol
    /// consistency; callers still perform configuration and Adapter admission.
    pub fn parse_request(&self, bytes: &[u8]) -> Result<Request, Error> {
        let value = parsing::parse(bytes)?;
        if !self.request.is_valid(&value) {
            return Err(Error::Invalid("request schema mismatch"));
        }
        if value["method"] == "invoke" {
            let before = value["event"]["name"] == "tool.before";
            for effect in array(
                &value["allowed_effects"],
                "invoke request missing allowed_effects",
            )? {
                if effect != "observe" && !(before && effect == "block") {
                    return Err(Error::Invalid("invocation effect is not implemented"));
                }
            }
            if before && !value["event"]["tool"]["result"].is_null() {
                return Err(Error::Invalid("tool.before cannot supply a tool result"));
            }
            if value["input_digest"] != binding(&value)? {
                return Err(Error::Invalid("invocation binding mismatch"));
            }
        }
        Ok(Request(value))
    }

    /// Binds a locally constructed invoke request before its digest field is added.
    ///
    /// The digest is an opaque external protocol binding, not a canonical Core
    /// input digest. Providers echo it without re-encoding or hashing the payload.
    ///
    /// # Errors
    /// Rejects an existing digest, a different method or an invalid request. Use
    /// [`Self::parse_request`] for untrusted bytes so duplicate fields remain visible.
    pub fn bind_invocation(&self, mut value: Value) -> Result<Request, Error> {
        if value["method"] != "invoke" || value.get("input_digest").is_some() {
            return Err(Error::Invalid("expected an unbound invoke request"));
        }
        let digest = binding(&value)?;
        let object = value
            .as_object_mut()
            .ok_or(Error::Invalid("expected request object"))?;
        object.insert("input_digest".into(), Value::String(digest));
        self.parse_request(&encode(&value)?)
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
        if !self.response.is_valid(&value) {
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
                for operation in
                    array(&value["operations"], "describe response missing operations")?
                {
                    if !names.insert(operation["name"].as_str()) {
                        return Err(Error::Invalid("duplicate operation name"));
                    }
                }
                Ok(Reply::Description(Description(value)))
            }
            Some("validate_config") => {
                if value.get("operations").is_some() || value.get("effects").is_some() {
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
                for effect in array(&value["effects"], "invoke response missing effects")? {
                    if !array(
                        &request.0["allowed_effects"],
                        "invoke request missing allowed_effects",
                    )?
                    .contains(&effect["type"])
                    {
                        return Err(Error::Invalid("response effect was not admitted"));
                    }
                }
                Ok(Reply::Invocation(Outcome(value)))
            }
            _ => Err(Error::Invalid("unknown request method")),
        }
    }
}

/// Borrow `value` as an array, or fail naming the absent protocol field.
/// The response schema's bare-ok branch is method-agnostic, so a describe
/// reply without payload reaches callers as `Value::Null`; the failure must
/// then say which field the method requires, not just the expected shape.
fn array<'a>(value: &'a Value, missing: &'static str) -> Result<&'a Vec<Value>, Error> {
    value.as_array().ok_or(Error::Invalid(missing))
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
