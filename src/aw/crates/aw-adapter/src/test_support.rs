//! Private launch-file ownership for native integration unit tests.

pub(crate) struct Artifacts(pub(crate) std::path::PathBuf);

impl crate::LaunchFiles for Artifacts {
    fn directory(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for Artifacts {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(crate) fn final_gate_document(adapter: &str, steps: serde_json::Value) -> serde_json::Value {
    use serde_json::json;
    let mut document = aw_config::Validator::new()
        .unwrap()
        .parse(include_bytes!("../../aw-config/examples/aw.minimal.yaml"))
        .unwrap()
        .as_value()
        .clone();
    document["spec"]["agents"] = json!({"sdk":{"adapter":adapter,"argv":["/bin/true"]}});
    document["spec"]["providers"] = json!({"policy":{
        "protocol":"aw-provider/v1alpha1","config":{},"timeout_ms":1000,"max_output_bytes":4096,
        "transport":{"type":"stdio","location":"agent","argv":["/bin/true"]}
    }});
    document["spec"]["events"] =
        json!({"security.violation":{"enabled":true,"required":false,"steps":steps}});
    document
}

pub(crate) fn final_gate_step(mode: &str, on_error: &str) -> serde_json::Value {
    serde_json::json!({
        "id":"check","provider":"policy","operation":"check","mode":mode,
        "effects":["observe","block"],"on_error":on_error
    })
}
