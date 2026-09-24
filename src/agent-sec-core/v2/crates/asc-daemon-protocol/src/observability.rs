//! Single-record ingestion wire contract; attribution travels in the `OTel` carrier.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Business fields of `obs.record`. Metadata is deliberately not a parameter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ObservabilityRecordParams {
    /// One supported observability hook.
    pub hook: String,
    /// Original timezone-aware observation timestamp, validated by the domain parser.
    pub observed_at: String,
    /// Hook-specific metric values; unknown metric names are filtered by the domain.
    pub metrics: Map<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::method::{AccessPolicy, MethodId, OBS_RECORD, resolve};
    use serde_json::json;

    #[test]
    fn ingestion_is_explicitly_local_user_and_metadata_is_not_a_business_field() {
        let method = resolve(OBS_RECORD).unwrap();
        assert_eq!(method, MethodId::ObservabilityRecord);
        assert_eq!(method.metadata().access, AccessPolicy::LocalUser);
        assert!(resolve("obs.record_batch").is_none());
        let params = json!({"hook":"before_agent_run", "observedAt":"2030-01-02T03:04:05Z", "metrics":{"user_input":"hello"}});
        assert!(serde_json::from_value::<ObservabilityRecordParams>(params.clone()).is_ok());
        for (key, value) in [
            ("metadata", json!({})),
            ("observedAt", json!(12)),
            ("metrics", json!([])),
        ] {
            let mut invalid = params.clone();
            invalid[key] = value;
            assert!(serde_json::from_value::<ObservabilityRecordParams>(invalid).is_err());
        }
    }
}
