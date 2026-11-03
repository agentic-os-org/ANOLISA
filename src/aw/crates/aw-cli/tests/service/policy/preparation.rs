//! Instance release cancels and drains candidate preparation through real RPCs.

use super::*;
use std::time::Duration;

#[test]
fn release_cancels_and_reaps_preparation_before_acknowledging() {
    for method in ["describe", "validate_config"] {
        let fixture = Fixture::new();
        let mut baseline = fixture.document();
        baseline["spec"]["providers"] = json!({});
        baseline["spec"]["events"] = json!({});
        let service = Running::start(&fixture, &baseline);
        let binding = bind(&fixture, &service.client, "qoder");
        let original = revision(&service.client, &binding);
        let mut candidate = fixture.document();
        candidate["spec"]["providers"]["policy"]["config"]["prepare_gate"] = json!(true);
        if method == "describe" {
            fs::write(fixture.0.join("describe-gate"), b"gate").unwrap();
        }
        thread::scope(|scope| {
            let client = service.client.clone();
            let operation = update(&binding, &original, &candidate);
            let worker = scope.spawn(move || call(&client, operation));
            fixture.wait_for_calls(method, 1);
            let released = service
                .client
                .call(
                    Operation::ReleaseInstance {
                        instance_id: binding.instance_id.clone(),
                    },
                    Instant::now() + Duration::from_secs(2),
                )
                .unwrap();
            // Keep the update connection open and the Provider gate closed:
            // release itself must cancel and reap the child before acknowledging.
            fixture.assert_reaped();
            assert_eq!(released["released"], true);
            assert_eq!(
                call(&service.client, Operation::Status).unwrap()["bindings"],
                0
            );
            let failure = worker.join().unwrap().unwrap_err();
            let aw_service::Error::Attempt { code, audit_key } = failure else {
                panic!("missing audit key: {failure:?}");
            };
            assert_eq!(code, "preparation_failed");
            let audit = common::audit(&service.client, &audit_key);
            let terminal = &audit["records"].as_array().unwrap().last().unwrap()["record"];
            let preparation = common::audit(
                &service.client,
                terminal["preparation_audit"].as_str().unwrap(),
            );
            assert_eq!(preparation["terminal"], true);
            let prepared = &preparation["records"].as_array().unwrap().last().unwrap()["record"];
            assert_eq!(prepared["status"], "error");
            assert_eq!(prepared["calls"]["failed"]["method"], method);
            assert!(prepared["calls"]["failed"]["process"].is_null());
        });
        fixture.assert_reaped();
    }
}
