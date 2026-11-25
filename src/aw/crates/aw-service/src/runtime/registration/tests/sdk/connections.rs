//! SDK close ordering across connection admission, execution and reply.

use super::*;
use crate::runtime::limits::Admission;
use std::os::unix::net::UnixStream;

#[test]
fn sdk_close_waits_for_the_matching_rpc_to_reach_its_reply() {
    let f = Fixture::new();
    let (_, id) = task(&f, "drain-rpc");
    let slots = &f.runtime.resources.connections;
    let (stream, peer) = UnixStream::pair().unwrap();
    let rpc = slots.track(stream).unwrap();
    assert!(rpc.admit(Admission::SdkCallback, 1).unwrap());
    let deadline = || Instant::now() + Duration::from_secs(5);
    let cancelled = AtomicBool::new(false);
    f.runtime
        .handle_connected(callback(&f, &id), deadline(), &cancelled, Some(&rpc))
        .unwrap();
    drop(peer);

    let close = |call: &str, limit| {
        let mut op = callback(&f, &id);
        if let Operation::SdkCallback {
            close,
            native_input,
            ..
        } = &mut op
        {
            *close = true;
            native_input["tool_use_id"] = json!(call);
        }
        f.runtime.handle(op, limit, &cancelled)
    };
    // Hold the worker between event retirement and begin_reply. Another
    // correlation in the same scope must still be able to close.
    f.runtime
        .close_hook(&id, "tool.before", "sdk-session", "sdk-call", deadline())
        .unwrap();
    assert!(f.runtime.events.lock().unwrap().is_empty());
    close("other-call", deadline()).unwrap();
    code(
        close("sdk-call", Instant::now() + Duration::from_millis(100)),
        "event_close_timeout",
    );
    rpc.begin_reply().unwrap();
    close("sdk-call", deadline()).unwrap();
    let (next, _peer) = UnixStream::pair().unwrap();
    assert!(slots
        .track(next)
        .unwrap()
        .admit(Admission::Ordinary, 1)
        .unwrap());
}

#[test]
fn sdk_close_reclaims_disconnected_callbacks_before_registration() {
    let f = Fixture::new();
    let (_, id) = task(&f, "early-cancel");
    let slots = &f.runtime.resources.connections;
    let (stream, peer) = UnixStream::pair().unwrap();
    let old = slots.track(stream).unwrap();
    assert!(old.admit(Admission::SdkCallback, 1).unwrap());
    drop(peer);

    let deadline = || Instant::now() + Duration::from_secs(5);
    let cancelled = AtomicBool::new(false);
    let mut close = callback(&f, &id);
    if let Operation::SdkCallback { close, .. } = &mut close {
        *close = true;
    }
    f.runtime.handle(close, deadline(), &cancelled).unwrap();

    let (next, _peer) = UnixStream::pair().unwrap();
    let next = slots.track(next).unwrap();
    assert!(next.admit(Admission::SdkCallback, 1).unwrap());
    let mut fresh = callback(&f, &id);
    if let Operation::SdkCallback { native_input, .. } = &mut fresh {
        native_input["tool_use_id"] = json!("fresh-after-early-cancel");
    }
    f.runtime
        .handle_connected(fresh, deadline(), &cancelled, Some(&next))
        .unwrap();
    // Resume the old worker only after a new callback has used its slot.
    code(
        f.runtime
            .handle_connected(callback(&f, &id), deadline(), &cancelled, Some(&old)),
        "event_cancelled",
    );
    assert!(old.begin_reply().is_err());
}
