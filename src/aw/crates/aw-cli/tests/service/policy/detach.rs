//! Unbind uses the same complete native idle gate as explicit policy activation.

use super::*;

#[test]
fn unbind_keeps_busy_requests_and_native_cleanup_protected() {
    for adapter in ["qoder", "qoder-sdk", "qwenpaw"] {
        let fixture = Fixture::new();
        let mut baseline = fixture.document();
        baseline["spec"]["agents"]["target"]["adapter"] = json!(adapter);
        let service = Running::start(&fixture, &baseline);
        let binding = bind(&fixture, &service.client, adapter);
        let remove = || {
            call(
                &service.client,
                Operation::Unbind {
                    instance_id: binding.instance_id.clone(),
                },
            )
        };
        lifecycle(
            &service.client,
            &binding,
            LifecyclePhase::RequestStart,
            "request",
        );
        let original_revision = revision(&service.client, &binding);
        for _ in 0..16 {
            rejected(remove(), "busy");
        }
        assert_eq!(revision(&service.client, &binding), original_revision);
        lifecycle(&service.client, &binding, LifecyclePhase::ToolStart, "tool");
        lifecycle(
            &service.client,
            &binding,
            LifecyclePhase::RequestEnd,
            "request",
        );
        rejected(remove(), "busy");
        assert_eq!(
            call(&service.client, Operation::Status).unwrap()["bindings"],
            1
        );
        lifecycle(&service.client, &binding, LifecyclePhase::ToolEnd, "tool");
        call(
            &service.client,
            update(&binding, &original_revision, &baseline),
        )
        .unwrap();
        remove().unwrap();
        assert_eq!(
            call(&service.client, Operation::Status).unwrap()["bindings"],
            0
        );
        assert!(call(
            &service.client,
            Operation::BridgeLifecycle {
                instance_id: binding.instance_id,
                pid: std::process::id(),
                phase: LifecyclePhase::RequestStart,
                key: "late".into()
            }
        )
        .is_err());
    }
}
