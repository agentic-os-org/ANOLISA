//! Public record schema projected from the same hook contracts used by ingestion.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

use crate::{MetadataShape, OBSERVABILITY_HOOKS};

/// Generates the public CLI record JSON Schema from the Rust hook contracts.
///
/// Preserves V1's schema vocabulary and canonical field aliases. Ingestion also
/// applies normalization and custom validation (for example, at least one known
/// metric must remain after filtering); JSON Schema alone does not model those.
#[must_use]
pub fn observability_record_json_schema() -> Value {
    let mut definitions = BTreeMap::new();
    let mut mapping = BTreeMap::new();
    let mut variants = Vec::new();
    for hook in OBSERVABILITY_HOOKS {
        let stem = title(hook.as_str()).replace(' ', "");
        let record_name = format!("{stem}Record");
        let metrics_name = format!("{stem}Metrics");
        let metadata_name = match hook.metadata_shape() {
            MetadataShape::Common => "ObservabilityMetadata",
            MetadataShape::ModelCall => "ModelCallMetadata",
            MetadataShape::ToolCall => "ToolCallMetadata",
        };
        definitions
            .entry(metadata_name.into())
            .or_insert_with(|| metadata_schema(metadata_name, hook.metadata_shape()));
        let properties: Map<String, Value> = hook
            .metric_names()
            .iter()
            .map(|name| ((*name).into(), json!({"default":null,"title":title(name)})))
            .collect();
        definitions.insert(
            metrics_name.clone(),
            json!({
                "minProperties":1,"properties":properties,"title":metrics_name,"type":"object"
            }),
        );
        definitions.insert(
            record_name.clone(),
            json!({
                "properties":{
                    "hook":{"const":hook.as_str(),"title":"Hook","type":"string"},
                    "observedAt":{"format":"date-time","title":"Observedat","type":"string"},
                    "metadata":{"$ref":format!("#/$defs/{metadata_name}")},
                    "metrics":{"$ref":format!("#/$defs/{metrics_name}")}
                },
                "required":["hook","observedAt","metadata","metrics"],
                "title":record_name,"type":"object"
            }),
        );
        let reference = format!("#/$defs/{record_name}");
        mapping.insert(hook.as_str(), reference.clone());
        variants.push(json!({"$ref":reference}));
    }
    json!({
        "$defs":definitions,
        "discriminator":{"mapping":mapping,"propertyName":"hook"},
        "oneOf":variants,
        "$schema":"https://json-schema.org/draft/2020-12/schema"
    })
}

fn metadata_schema(name: &str, shape: MetadataShape) -> Value {
    let mut properties = Map::new();
    let mut required = Vec::new();
    for &(field, optional) in shape.record_fields() {
        let definition = if optional {
            json!({"anyOf":[{"type":"string"},{"type":"null"}],"default":null,"title":title(field)})
        } else {
            required.push(field);
            json!({"title":title(field),"type":"string"})
        };
        properties.insert(field.into(), definition);
    }
    let description = match shape {
        MetadataShape::Common => "Correlation metadata required on every observability record.",
        MetadataShape::ModelCall => "Correlation metadata for model API call records.",
        MetadataShape::ToolCall => "Correlation metadata required on tool call records.",
    };
    json!({"description":description,"properties":properties,"required":required,"title":name,"type":"object"})
}

// Match V1's generated titles without maintaining a second list of metric names.
fn title(name: &str) -> String {
    name.split('_')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => {
                    first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase()
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ObservabilityRecord;

    #[test]
    fn generated_schema_matches_complete_v1_oracle() {
        let expected: Value = serde_json::from_str(include_str!(
            "../../../fixtures/query/v1-record-schema.json"
        ))
        .unwrap();
        assert_eq!(observability_record_json_schema(), expected);
    }

    #[test]
    fn metadata_requirements_and_metric_names_agree_with_ingestion() {
        let schema = observability_record_json_schema();
        for hook in OBSERVABILITY_HOOKS {
            let reference = schema["discriminator"]["mapping"][hook.as_str()]
                .as_str()
                .unwrap();
            let definition = &schema["$defs"][reference.rsplit('/').next().unwrap()];
            let metadata_ref = definition["properties"]["metadata"]["$ref"]
                .as_str()
                .unwrap();
            let metadata = &schema["$defs"][metadata_ref.rsplit('/').next().unwrap()];
            let mut fields = Map::new();
            for name in metadata["required"].as_array().unwrap() {
                fields.insert(name.as_str().unwrap().into(), json!("id"));
            }
            let mut record = json!({"hook":hook.as_str(),"observedAt":"2030-01-01T00:00:00Z","metadata":fields,"metrics":{hook.metric_names()[0]:null}});
            assert!(ObservabilityRecord::from_json_value(&record).is_ok());
            for name in metadata["required"].as_array().unwrap() {
                let mut missing = record.clone();
                missing["metadata"]
                    .as_object_mut()
                    .unwrap()
                    .remove(name.as_str().unwrap());
                assert!(ObservabilityRecord::from_json_value(&missing).is_err());
            }
            for name in hook.metric_names() {
                record["metrics"] = json!({*name:null});
                let parsed = ObservabilityRecord::from_json_value(&record).unwrap();
                assert_eq!(parsed.metrics().get(name), Some(&Value::Null));
            }
            record["metrics"] = json!({"unknown_metric":1});
            assert!(ObservabilityRecord::from_json_value(&record).is_err());
        }
    }
}
