//! Bound protocol admission and retirement races with held runtime resources.

use super::*;

use crate::runtime::{EventEntry, Instance};
use aw_host::{Host, ProcessContext};
use aw_provider::admission::AdapterCapabilities;
use std::{
    collections::BTreeMap,
    fs,
    sync::{
        atomic::{AtomicBool, AtomicU64},
        mpsc, Mutex,
    },
};

struct Fixture {
    runtime: Arc<Runtime>,
    id: String,
    _directory: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::Builder::new()
            .prefix("invocation-races-")
            .tempdir_in(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"))
            .unwrap();
        let runtime = Runtime::new(
            include_bytes!("../../../../aw-config/examples/aw.minimal.yaml").to_vec(),
            &directory.path().join("audit"),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        let binding = runtime
            .bind(
                "qoder".into(),
                crate::Capabilities {
                    failure_actions: Default::default(),
                    adapter: "qoder".into(),
                    version: "fixture".into(),
                    entrypoint: "fixture".into(),
                    events: BTreeMap::from([
                        ("tool.before".into(), vec!["observe".into()]),
                        ("tool.after".into(), vec!["observe".into()]),
                    ]),
                },
                ProcessContext {
                    cwd: directory.path().to_owned(),
                    environment: BTreeMap::new(),
                    stderr_bytes: 1024,
                },
                deadline(),
                &AtomicBool::new(false),
            )
            .unwrap();
        Self {
            runtime,
            id: binding["instance_id"].as_str().unwrap().into(),
            _directory: directory,
        }
    }

    fn event(&self) -> Value {
        json!({"name":"tool.before", "agent":{"adapter":"qoder", "binding_id":"qoder", "instance_id":self.id},
               "session_id":"session", "tool":{"name":"tool", "native_name":"tool", "call_id":"call", "input":{}},
               "native":{}})
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.runtime.maintain(true).unwrap();
    }
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(5)
}

fn wait(mut ready: impl FnMut() -> bool) {
    let until = deadline();
    while !ready() {
        assert!(Instant::now() < until, "runtime barrier was not reached");
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn legacy_hook_admission_atomically_excludes_protocol_enable() {
    let fixture = Fixture::new();
    let (hooks, state) = {
        let instances = fixture.runtime.instances.lock().unwrap();
        let instance = instances[&fixture.id].as_ref().unwrap();
        (
            Arc::clone(&instance.hooks),
            Arc::clone(&instance.invocations),
        )
    };
    let held = hooks.lock().unwrap();
    thread::scope(|threads| {
        let opening = threads.spawn(|| {
            fixture.runtime.open_hook(
                fixture.id.clone(),
                fixture.event(),
                b"{}".to_vec(),
                deadline(),
            )
        });
        wait(|| state.lock().unwrap().legacy_used);
        let enabled = fixture.runtime.enable_invocations(&fixture.id);
        drop(held);
        // This zero-step automatic event can complete before open returns.
        if let Err(error) = opening.join().unwrap() {
            assert!(matches!(error, crate::Error::Rejected(code) if code == "native_event_closed"));
        }
        assert!(matches!(enabled, Err(crate::Error::Rejected(code))
            if code == "invocation_protocol_requires_unused_binding"));
    });
}

#[test]
fn completed_uncorrelated_event_cannot_switch_protocol() {
    let fixture = Fixture::new();
    let handle = fixture
        .runtime
        .open(
            fixture.id.clone(),
            fixture.event(),
            None,
            deadline(),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
    fixture
        .runtime
        .handle(
            crate::Operation::CloseEvent {
                instance_id: fixture.id.clone(),
                event_id: handle["event_id"].as_str().unwrap().into(),
            },
            deadline(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert!(fixture.runtime.events.lock().unwrap().is_empty());
    assert!(matches!(fixture.runtime.enable_invocations(&fixture.id),
        Err(crate::Error::Rejected(code)) if code == "invocation_protocol_requires_unused_binding"));
}

#[test]
fn late_close_during_second_phase_drain_leaves_no_retired_lease() {
    let fixture = Fixture::new();
    fixture.runtime.enable_invocations(&fixture.id).unwrap();
    fixture
        .runtime
        .register_invocation(&fixture.id, 1, "call", "tool", json!({}))
        .unwrap();
    let (hooks, state) = {
        let instances = fixture.runtime.instances.lock().unwrap();
        let instance = instances[&fixture.id].as_ref().unwrap();
        (
            Arc::clone(&instance.hooks),
            Arc::clone(&instance.invocations),
        )
    };
    state
        .lock()
        .unwrap()
        .ledger
        .as_mut()
        .unwrap()
        .check(&fixture.event())
        .unwrap();
    fixture
        .runtime
        .close_hook(&fixture.id, "tool.after", "session", "call", deadline())
        .unwrap();
    let cancelled = Arc::clone(
        &hooks.lock().unwrap()[&correlation("tool.after", "session", "call").unwrap()].cancelled,
    );
    cancelled.store(false, Ordering::Release);
    let (sender, _receiver) = mpsc::sync_channel(1);
    fixture.runtime.events.lock().unwrap().insert(
        "held-after".into(),
        EventEntry {
            instance_id: fixture.id.clone(),
            sender,
            cancelled: Arc::clone(&cancelled),
            context: Arc::new(crate::runtime::evidence::Cell::default()),
        },
    );
    thread::scope(|threads| {
        let finishing = threads.spawn(|| {
            fixture
                .runtime
                .finish_invocation(&fixture.id, 1, "call", deadline())
        });
        wait(|| cancelled.load(Ordering::Acquire));
        let late =
            fixture
                .runtime
                .close_hook(&fixture.id, "tool.before", "session", "call", deadline());
        fixture.runtime.events.lock().unwrap().remove("held-after");
        finishing.join().unwrap().unwrap();
        late.unwrap();
    });
    assert!(hooks.lock().unwrap().is_empty());
    assert!(state
        .lock()
        .unwrap()
        .ledger
        .as_ref()
        .unwrap()
        .active
        .is_empty());
    assert!(fixture
        .runtime
        .close_hook(&fixture.id, "tool.before", "session", "call", deadline())
        .is_err());
    assert!(hooks.lock().unwrap().is_empty());
}

#[test]
fn release_timeout_keeps_instance_draining_until_cleanup_can_finish() {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/service-release-tests")
        .join(format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
    fs::create_dir_all(directory.parent().unwrap()).unwrap();
    fs::create_dir(&directory).unwrap();
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }
    let _cleanup = Cleanup(directory.clone());
    let bytes = include_bytes!("../../../../aw-config/examples/aw.minimal.yaml");
    let stopped = Arc::new(AtomicBool::new(false));
    let runtime = Runtime::new(bytes.to_vec(), &directory.join("audit"), stopped).unwrap();
    let host = Host::prepare(
        bytes,
        "qoder",
        AdapterCapabilities {
            failure_actions: Default::default(),
            adapter: "qoder".into(),
            version: "fixture".into(),
            entrypoint: "fixture".into(),
            events: BTreeMap::from([
                ("tool.before".into(), vec!["observe".into(), "block".into()]),
                ("tool.after".into(), vec!["observe".into()]),
            ]),
        },
        ProcessContext {
            cwd: directory.clone(),
            environment: BTreeMap::new(),
            stderr_bytes: 1024,
        },
        Instant::now() + Duration::from_secs(5),
        &AtomicBool::new(false),
    )
    .unwrap();
    runtime.instances.lock().unwrap().insert(
        "fixture".into(),
        Some(Instance {
            bytes: bytes.to_vec(),
            context: ProcessContext {
                cwd: directory.clone(),
                environment: BTreeMap::new(),
                stderr_bytes: 1024,
            },
            reload: None,
            policy_update: Arc::new(Mutex::new(())),
            release: Arc::new(Mutex::new(())),
            host: Arc::new(host),
            closing: false,
            observation: None,
            hooks: Arc::new(Mutex::new(BTreeMap::new())),
            hook_open: Arc::new(Mutex::new(())),
            invocations: Arc::default(),
        }),
    );
    let hooks = Arc::clone(
        &runtime.instances.lock().unwrap()["fixture"]
            .as_ref()
            .unwrap()
            .hooks,
    );
    let guard = hooks.lock().unwrap();
    let locked = runtime.open_hook(
        "fixture".into(),
        json!({
            "name": "tool.before", "session_id": "session", "tool": {"call_id": "call"}
        }),
        vec![],
        Instant::now() + Duration::from_millis(10),
    );
    assert!(matches!(locked, Err(crate::Error::Rejected(code)) if code == "event_open_timeout"));
    drop(guard);
    let cancelled = Arc::new(AtomicBool::new(false));
    let (sender, _receiver) = mpsc::sync_channel(1);
    runtime.events.lock().unwrap().insert(
        "busy".into(),
        EventEntry {
            instance_id: "fixture".into(),
            sender,
            cancelled: Arc::clone(&cancelled),
            context: Arc::new(super::super::evidence::Cell::default()),
        },
    );
    assert!(
        matches!(runtime.release_instance("fixture", Instant::now() + Duration::from_millis(10)),
        Err(crate::Error::Rejected(code)) if code == "instance_release_timeout")
    );
    assert!(cancelled.load(Ordering::Acquire));
    assert!(
        runtime.instances.lock().unwrap()["fixture"]
            .as_ref()
            .unwrap()
            .closing
    );
    assert!(runtime
        .open(
            "fixture".into(),
            json!({}),
            None,
            Instant::now() + Duration::from_secs(1),
            Arc::new(AtomicBool::new(false)),
        )
        .is_err());
    runtime.events.lock().unwrap().remove("busy");
    assert_eq!(
        runtime
            .release_instance("fixture", Instant::now() + Duration::from_secs(1))
            .unwrap()["released"],
        true
    );
    assert!(runtime.instances.lock().unwrap().is_empty());
}
