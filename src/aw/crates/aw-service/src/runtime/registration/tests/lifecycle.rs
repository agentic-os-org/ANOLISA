//! Service lifecycle dispatch with an acknowledged semantic SDK Scope.

use super::*;
use crate::Availability;

fn lifecycle_scope(provider: Value, events: Value) -> (Fixture, RegistrationHandle, String) {
    let mut f = Fixture::new();
    let mut document: Value = serde_json::from_slice(&f.runtime.bytes).unwrap();
    document["spec"]["agents"]["target"] = json!({"adapter":"sdk","argv":["/bin/true"]});
    document["spec"]["providers"]["gate"] = provider;
    document["spec"]["events"] = events;
    f.runtime = Runtime::new(
        serde_json::to_vec(&document).unwrap(),
        &f.directory.path().join("audit"),
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    let mut registration =
        serde_json::to_value(super::sdk::command(&f, "lifecycle", Availability::Required)).unwrap();
    registration["capabilities"] = json!({
        "adapter":"sdk","version":"aw-hook-source/v1","entrypoint":"application",
        "events":{"session.start":["observe"],"turn.stop":["observe","block"],"session.end":["observe"]}
    });
    let ready = f
        .call(serde_json::from_value(registration).unwrap())
        .unwrap();
    let h = handle(&ready);
    f.call(RegistrationCommand::Ready {
        handle: h.clone(),
        policy_revision: ready["policy_revision"].as_str().unwrap().into(),
    })
    .unwrap();
    let scope = f.open(&h.binding.instance_id).unwrap();
    let id = scope["instance_id"].as_str().unwrap().to_owned();
    f.runtime.scope_policy(&id).unwrap();
    let deadline = || Instant::now() + Duration::from_secs(5);
    f.runtime.acknowledge_scope(&id, deadline()).unwrap();
    f.runtime.enable_invocations(&id).unwrap();
    (f, h, id)
}

#[test]
fn semantic_lifecycle_dispatch_retains_actual_reasons_and_scope_identity() {
    let (f, h, id) = lifecycle_scope(
        json!({
            "transport":{"type":"stdio","location":"agent",
                "argv":["/usr/bin/python3","-c",
                    "import json, sys; value = json.load(sys.stdin); print('true' if value['native'].get('reason') == 'timeout' else 'false')"]},
            "timeout_ms":3000,"max_output_bytes":32
        }),
        json!({
            "session.start":{"enabled":true,"steps":[
                {"id":"start","provider":"gate","effects":["observe"],"on_error":"report"}]},
            "turn.stop":{"enabled":true,"steps":[
                {"id":"inspect","provider":"gate","effects":["observe"],"on_error":"report"},
                {"id":"stop","provider":"gate","effects":["block"],"on_error":"block"}]},
            "session.end":{"enabled":true,"steps":[
                {"id":"end","provider":"gate","effects":["observe"],"on_error":"report"}]}
        }),
    );
    let deadline = || Instant::now() + Duration::from_secs(5);
    let invoke = |event: &str, step: &str, reason: Option<&str>, attempt: Option<&str>| {
        let mut native =
            json!({"api_version":"aw-hook-source/v1","event":event,"session_id":"session"});
        if let Some(reason) = reason {
            native["reason"] = json!(reason);
        }
        if let Some(attempt) = attempt {
            native["turn_id"] = json!("turn");
            native["attempt_id"] = json!(attempt);
        }
        let operation = Operation::SdkCallback {
            instance_id: id.clone(),
            event: event.into(),
            step_id: step.into(),
            native_input: native,
            close: false,
        };
        f.runtime
            .authorize_registration_peer(
                &operation,
                Some(&Process::read(std::process::id()).unwrap()),
            )
            .unwrap();
        f.runtime
            .handle(operation, deadline(), &AtomicBool::new(false))
    };

    assert_eq!(
        invoke("session.start", "start", None, None).unwrap()["code"],
        0
    );
    for (reason, attempt, expected) in [
        ("completed", "one", 0),
        ("denied", "two", 0),
        ("error", "three", 0),
        ("timeout", "four", 2),
    ] {
        assert_eq!(
            invoke("turn.stop", "inspect", Some(reason), Some(attempt)).unwrap()["code"],
            0
        );
        assert_eq!(
            invoke("turn.stop", "stop", Some(reason), Some(attempt)).unwrap()["code"],
            expected
        );
    }
    assert!(invoke("turn.stop", "stop", Some("timeout"), Some("four")).is_err());
    assert_eq!(
        invoke("session.end", "end", Some("cancelled"), None).unwrap()["code"],
        0
    );
    f.call(RegistrationCommand::Release { handle: h.clone() })
        .unwrap();

    let plans: Vec<_> = fs::read_dir(f.directory.path().join("audit"))
        .unwrap()
        .filter_map(|entry| fs::read_to_string(entry.unwrap().path()).ok())
        .flat_map(|data| data.lines().map(str::to_owned).collect::<Vec<_>>())
        .filter_map(|line| serde_json::from_str::<Value>(&line).ok())
        .filter(|line| line["record"]["plan"]["kind"] == "event")
        .map(|line| line["record"]["plan"]["event"].clone())
        .collect();
    assert_eq!(plans.len(), 6, "{plans:?}");
    for event in &plans {
        assert_eq!(event["session_id"], "session");
        assert_eq!(event["task_id"], id);
        assert_eq!(event["parent_instance_id"], h.binding.instance_id);
        assert_eq!(event["process"]["pid"], std::process::id());
    }
    for reason in ["completed", "denied", "error", "timeout", "cancelled"] {
        assert!(
            plans.iter().any(|event| event["reason"] == reason),
            "{reason}"
        );
    }
}

#[test]
fn a_long_lived_scope_retires_more_than_one_thousand_stops_and_fences_replays() {
    let (fixture, registration, id) = lifecycle_scope(
        json!({
            "transport":{"type":"stdio","location":"agent","argv":["/bin/sh","-c","cat >/dev/null; printf false"]},
            "timeout_ms":3000,"max_output_bytes":32
        }),
        json!({"turn.stop":{"enabled":true,"steps":[
            {"id":"stop","provider":"gate","effects":["block"],"on_error":"block"}
        ]}}),
    );
    let call = |turn: u32, close: bool| {
        let native = json!({
            "api_version":"aw-hook-source/v1","event":"turn.stop","session_id":"session",
            "turn_id":turn.to_string(),"reason":"completed"
        });
        fixture.runtime.sdk_callback(
            &id,
            "turn.stop",
            "stop",
            native,
            close,
            Instant::now() + Duration::from_secs(5),
            None,
        )
    };
    assert_eq!(call(0, true).unwrap()["code"], 0);
    for sequence in 1..=1026 {
        assert_eq!(call(sequence, false).unwrap()["code"], 0, "{sequence}");
    }

    // Sequence 1's lease is gone. Its original frame still cannot replay,
    // and cancellation before the first open also reserves the identity.
    for turn in [0, 1] {
        assert!(matches!(
            call(turn, false),
            Err(crate::Error::Rejected(code)) if code == "native_event_closed"
        ));
    }
    assert_eq!(call(1027, false).unwrap()["code"], 0);
    fixture
        .call(RegistrationCommand::Release {
            handle: registration,
        })
        .unwrap();

    let completed = fs::read_dir(fixture.directory.path().join("audit"))
        .unwrap()
        .filter_map(|entry| fs::read_to_string(entry.unwrap().path()).ok())
        .filter(|journal| {
            journal.lines().any(|line| {
                serde_json::from_str::<Value>(line).is_ok_and(|record| {
                    record["record"]["phase"] == "completed"
                        && record["record"]["call"]["step_id"] == "stop"
                })
            })
        })
        .count();
    assert_eq!(completed, 1027);
}
