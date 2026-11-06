//! Bundled, offline schema registry. Schema files are the authoritative shapes.

use crate::{canonical, Error};
use serde_json::Value;
use std::collections::BTreeMap;

/// Exact bundled schema resources, identified by their local catalogue name.
pub const SCHEMAS: &[(&str, &str)] = &[
    (
        "object-meta-v1alpha1",
        include_str!("../schemas/object-meta-v1alpha1.schema.json"),
    ),
    (
        "execution-event-v1alpha1",
        include_str!("../schemas/execution-event-v1alpha1.schema.json"),
    ),
    (
        "audit-record-v1alpha1",
        include_str!("../schemas/audit-record-v1alpha1.schema.json"),
    ),
    (
        "evidence-v1",
        include_str!("../schemas/evidence-v1.schema.json"),
    ),
    (
        "evidence-context-v1",
        include_str!("../schemas/evidence-context-v1.schema.json"),
    ),
    (
        "capability-plan-v1",
        include_str!("../schemas/capability-plan-v1.schema.json"),
    ),
    (
        "plan-execution-v1",
        include_str!("../schemas/plan-execution-v1.schema.json"),
    ),
    (
        "os-protection-binding-v1",
        include_str!("../schemas/os-protection-binding-v1.schema.json"),
    ),
    (
        "boundary-descriptor-v1",
        include_str!("../schemas/boundary-descriptor-v1.schema.json"),
    ),
    (
        "capability-invocation-v1",
        include_str!("../schemas/capability-invocation-v1.schema.json"),
    ),
    (
        "common-v1",
        include_str!("../schemas/common-v1.schema.json"),
    ),
    (
        "context-adoption-v1",
        include_str!("../schemas/context-adoption-v1.schema.json"),
    ),
    (
        "context-projection-prepare-input-v2",
        include_str!("../schemas/context-projection-prepare-input-v2.schema.json"),
    ),
    (
        "context-projection-prepare-output-v2",
        include_str!("../schemas/context-projection-prepare-output-v2.schema.json"),
    ),
    (
        "control-grant-v1",
        include_str!("../schemas/control-grant-v1.schema.json"),
    ),
    (
        "execution-intent-v1",
        include_str!("../schemas/execution-intent-v1.schema.json"),
    ),
    (
        "operation-record-v1",
        include_str!("../schemas/operation-record-v1.schema.json"),
    ),
    (
        "provider-descriptor-v1",
        include_str!("../schemas/provider-descriptor-v1.schema.json"),
    ),
    (
        "provider-receipt-v1",
        include_str!("../schemas/provider-receipt-v1.schema.json"),
    ),
    (
        "runtime-binding-v1",
        include_str!("../schemas/runtime-binding-v1.schema.json"),
    ),
    (
        "security-code-inspect-input-v2",
        include_str!("../schemas/security-code-inspect-input-v2.schema.json"),
    ),
    (
        "security-code-inspect-output-v2",
        include_str!("../schemas/security-code-inspect-output-v2.schema.json"),
    ),
    (
        "security-command-inspect-input-v2",
        include_str!("../schemas/security-command-inspect-input-v2.schema.json"),
    ),
    (
        "security-command-inspect-output-v2",
        include_str!("../schemas/security-command-inspect-output-v2.schema.json"),
    ),
    (
        "security-content-inspect-input-v2",
        include_str!("../schemas/security-content-inspect-input-v2.schema.json"),
    ),
    (
        "security-content-inspect-output-v2",
        include_str!("../schemas/security-content-inspect-output-v2.schema.json"),
    ),
];

/// Reusable validators compiled exclusively from the bundled resources.
pub struct Registry {
    validators: BTreeMap<String, jsonschema::Validator>,
    references: BTreeMap<String, (String, String)>,
    objects: BTreeMap<(String, String), String>,
    evidence: jsonschema::Validator,
}

impl Registry {
    /// Compiles all schemas without fetching external resources.
    ///
    /// # Errors
    /// Returns an error if a bundled schema or reference is invalid.
    pub fn new() -> Result<Self, Error> {
        let mut resources = jsonschema::Registry::new();
        let mut documents = Vec::new();
        let mut references = BTreeMap::new();
        let mut objects = BTreeMap::new();
        for (name, text) in SCHEMAS {
            let schema: Value =
                serde_json::from_str(text).map_err(|_| Error::InvalidSchema((*name).into()))?;
            let id = schema["$id"]
                .as_str()
                .ok_or_else(|| Error::InvalidSchema((*name).into()))?
                .to_owned();
            resources = resources
                .add(&id, schema.clone())
                .map_err(|_| Error::InvalidSchema((*name).into()))?;
            if let Some(version) = schema["properties"]["apiVersion"]["const"].as_str() {
                let kind = schema["properties"]["kind"]["const"]
                    .as_str()
                    .ok_or_else(|| Error::InvalidSchema((*name).into()))?;
                if objects
                    .insert((version.into(), kind.into()), (*name).into())
                    .is_some()
                {
                    return Err(Error::InvalidSchema("duplicate object identity".into()));
                }
            }
            references.insert((*name).into(), (id, canonical::digest(text.as_bytes())));
            documents.push((*name, schema));
        }
        let resources = resources
            .prepare()
            .map_err(|_| Error::InvalidSchema("registry".into()))?;
        let evidence = jsonschema::draft202012::options()
            .offline()
            .with_registry(&resources)
            .build(&serde_json::json!({
                "$ref": "https://agentic-os.sh/schemas/aw/common/v1#/$defs/evidence"
            }))
            .map_err(|_| Error::InvalidSchema("common-v1 evidence".into()))?;
        let mut validators = BTreeMap::new();
        for (name, schema) in documents {
            let validator = jsonschema::draft202012::options()
                .offline()
                .should_validate_formats(true)
                .with_registry(&resources)
                .build(&schema)
                .map_err(|_| Error::InvalidSchema(name.into()))?;
            validators.insert(name.into(), validator);
        }
        Ok(Self {
            validators,
            references,
            objects,
            evidence,
        })
    }

    /// Validates both the bounded encoding domain and a named schema shape.
    ///
    /// # Errors
    /// Rejects unknown schema names, invalid metadata or a shape mismatch.
    /// Cross-record invariants require the methods in `validation` as well.
    pub fn validate(&self, name: &str, value: &Value) -> Result<(), Error> {
        canonical::bytes(value)?;
        let validator = self
            .validators
            .get(name)
            .ok_or_else(|| Error::UnsupportedSchema(name.into()))?;
        validator
            .validate(value)
            .map_err(|_| Error::SchemaMismatch(name.into()))
    }

    /// Checks an acknowledgement against the existing common/v1 evidence shape.
    ///
    /// This validates the reference, not the referenced record or its durability.
    ///
    /// # Errors
    /// Rejects invalid encoding, missing fields or a malformed evidence reference.
    pub fn validate_evidence(&self, value: &Value) -> Result<(), Error> {
        canonical::bytes(value)?;
        self.evidence
            .validate(value)
            .map_err(|_| Error::SchemaMismatch("common-v1 evidence".into()))
    }

    /// Returns a schema URI and exact resource digest for negotiation.
    ///
    /// # Errors
    /// Rejects an unknown schema name.
    pub fn reference(&self, name: &str) -> Result<Value, Error> {
        let (id, digest) = self
            .references
            .get(name)
            .ok_or_else(|| Error::UnsupportedSchema(name.into()))?;
        Ok(serde_json::json!({"id": id, "digest": digest}))
    }

    /// Resolves an exact public object version and kind from bundled schemas.
    ///
    /// # Errors
    /// Rejects unregistered pairs without loading schemas from a caller's URI.
    pub fn object_schema(&self, api_version: &str, kind: &str) -> Result<&str, Error> {
        self.objects
            .get(&(api_version.into(), kind.into()))
            .map(String::as_str)
            .ok_or_else(|| Error::UnsupportedSchema("object version or kind".into()))
    }
}
