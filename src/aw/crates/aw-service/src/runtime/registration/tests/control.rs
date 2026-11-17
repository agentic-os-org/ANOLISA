//! Cooperative requests retain application identity and require native Ready proof.

use super::*;
use crate::{Availability, SdkControl, SdkIntent};

fn key(name: &str) -> RegistrationKey {
    let process = Process::read(std::process::id()).unwrap().metadata();
    RegistrationKey {
        request_id: name.into(),
        owner: RegistrationOwner {
            pid: std::process::id(),
            start_ticks: process["start_ticks"].as_str().unwrap().into(),
        },
    }
}
fn call(f: &Fixture, command: SdkControl) -> Result<Value> {
    f.runtime.sdk_control(
        command,
        &Process::read(std::process::id()).unwrap(),
        Instant::now() + Duration::from_secs(5),
    )
}
fn poll(f: &Fixture, key: RegistrationKey, handle: Option<RegistrationHandle>) -> Value {
    call(
        f,
        SdkControl::Poll {
            key,
            target: "target".into(),
            availability: Availability::Required,
            handle,
        },
    )
    .unwrap()
}
fn request(f: &Fixture, id: &str, name: &str, intent: SdkIntent, timeout_ms: u64) -> Result<Value> {
    call(
        f,
        SdkControl::Request {
            application_id: id.into(),
            request_id: name.into(),
            intent,
            timeout_ms,
        },
    )
}

#[test]
fn sdk_control_requires_owner_and_ready_and_isolates_application_keys() {
    let f = Fixture::new();
    let first = key("control-one");
    let second = key("control-two");
    let one = poll(&f, first.clone(), None);
    let two = poll(&f, second.clone(), None);
    assert_ne!(one["application_id"], two["application_id"]);
    let id = one["application_id"].as_str().unwrap();
    let mut forged = first.clone();
    forged.owner.start_ticks.push('0');
    assert!(call(
        &f,
        SdkControl::Poll {
            key: forged,
            target: "target".into(),
            availability: Availability::Required,
            handle: None
        }
    )
    .is_err());
    assert_eq!(
        request(&f, id, "attach", SdkIntent::Attach, 1000).unwrap()["state"],
        "pending"
    );
    assert_eq!(
        request(&f, id, "attach", SdkIntent::Attach, 1000).unwrap()["state"],
        "pending"
    );
    code(
        request(&f, id, "conflict", SdkIntent::Detach, 1000),
        "sdk_control_busy",
    );
    code(
        call(
            &f,
            SdkControl::Acknowledge {
                key: first.clone(),
                request_id: "attach".into(),
                handle: None,
                error: None,
            },
        ),
        "sdk_control_result_invalid",
    );
    let (handle, scope) = super::sdk::task(&f, "control-registration");
    f.runtime
        .release_instance(&scope, Instant::now() + Duration::from_secs(5))
        .unwrap();
    // The fixture's target comes from its configured registration binding.
    let target = handle.binding.target.clone();
    assert_eq!(target, "target");
    assert_eq!(
        call(
            &f,
            SdkControl::Acknowledge {
                key: first.clone(),
                request_id: "attach".into(),
                handle: Some(handle.clone()),
                error: None
            }
        )
        .unwrap()["state"],
        "completed"
    );
    assert_eq!(
        poll(&f, first.clone(), Some(handle.clone()))["connected"],
        true
    );
    request(&f, id, "detach", SdkIntent::Detach, 1000).unwrap();
    poll(&f, first.clone(), None);
    code(
        call(
            &f,
            SdkControl::Acknowledge {
                key: first.clone(),
                request_id: "detach".into(),
                handle: None,
                error: None,
            },
        ),
        "sdk_control_still_attached",
    );
    f.call(RegistrationCommand::Release { handle }).unwrap();
    assert_eq!(
        call(
            &f,
            SdkControl::Acknowledge {
                key: first.clone(),
                request_id: "detach".into(),
                handle: None,
                error: None
            }
        )
        .unwrap()["state"],
        "completed"
    );
    call(&f, SdkControl::Withdraw { key: first }).unwrap();
    assert_eq!(
        call(&f, SdkControl::List).unwrap()["applications"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn sdk_control_expiration_does_not_accept_a_late_application_reply() {
    let f = Fixture::new();
    let owner = key("expires");
    let announced = poll(&f, owner.clone(), None);
    let id = announced["application_id"].as_str().unwrap();
    request(&f, id, "expires", SdkIntent::Attach, 100).unwrap();
    std::thread::sleep(Duration::from_millis(120));
    code(
        call(
            &f,
            SdkControl::Acknowledge {
                key: owner,
                request_id: "expires".into(),
                handle: None,
                error: Some("sdk_control_busy".into()),
            },
        ),
        "sdk_control_request_expired",
    );
    assert_eq!(
        call(
            &f,
            SdkControl::Query {
                application_id: id.into(),
                request_id: "expires".into()
            }
        )
        .unwrap()["state"],
        "expired"
    );
}
