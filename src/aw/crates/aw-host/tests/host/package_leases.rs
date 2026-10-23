//! Installed Provider files remain protected through prepared snapshot ownership.

use crate::{capabilities, Fixture, Host, TIMEOUT};
use serde_json::{json, Value};
use std::{
    fs::{self, File},
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::Instant,
};

fn installed_provider(fixture: &Fixture) -> (Value, PathBuf) {
    let prefix = fixture.0.join("installed");
    fs::create_dir_all(prefix.join(".aw-packages")).unwrap();
    for path in [&prefix, &prefix.join(".aw-packages")] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    fs::write(prefix.join(".aw-install.lock"), []).unwrap();
    fs::set_permissions(
        prefix.join(".aw-install.lock"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    let script = prefix.join("provider.py");
    fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/provider.py"),
        &script,
    )
    .unwrap();
    let mut document = fixture.document();
    document["spec"]["providers"]["policy"]["transport"]["argv"][2] = json!(script);
    (document, prefix)
}

#[test]
fn prepared_hosts_retain_managed_files_until_the_last_snapshot_drops() {
    let fixture = Fixture::private();
    let (document, prefix) = installed_provider(&fixture);
    let host = Arc::new(fixture.prepare(&document));
    let in_flight_snapshot = Arc::clone(&host);
    let writer = File::open(prefix.join(".aw-install.lock")).unwrap();
    assert!(writer.try_lock().is_err());
    drop(host);
    assert!(writer.try_lock().is_err());
    drop(in_flight_snapshot);
    writer.try_lock().unwrap();
    fixture.assert_reaped();
}

#[test]
fn failed_and_cancelled_preparation_release_managed_files() {
    for cancel_during_discovery in [false, true] {
        let fixture = Fixture::private();
        let (mut document, prefix) = installed_provider(&fixture);
        document["spec"]["providers"]["policy"]["transport"]["argv"][4] =
            json!(if cancel_during_discovery {
                "slow-preparation"
            } else {
                "describe-nonzero"
            });
        let cancelled = AtomicBool::new(false);
        let bytes = serde_json::to_vec(&document).unwrap();
        let result = thread::scope(|scope| {
            let worker = scope.spawn(|| {
                Host::prepare(
                    &bytes,
                    "target",
                    capabilities(),
                    fixture.context(),
                    Instant::now() + TIMEOUT,
                    &cancelled,
                )
            });
            if cancel_during_discovery {
                let ready = fixture.wait_for_calls("describe", 1, Instant::now() + TIMEOUT);
                let writer = File::open(prefix.join(".aw-install.lock")).unwrap();
                let held = writer.try_lock().is_err();
                cancelled.store(true, Ordering::Release);
                let result = worker.join().unwrap();
                assert!(ready && held);
                result
            } else {
                worker.join().unwrap()
            }
        });
        assert!(result.is_err());
        File::open(prefix.join(".aw-install.lock"))
            .unwrap()
            .try_lock()
            .unwrap();
        fixture.assert_reaped();
    }
}

#[test]
fn mutation_lock_prevents_any_provider_discovery_process() {
    let fixture = Fixture::private();
    let (document, prefix) = installed_provider(&fixture);
    let writer = File::open(prefix.join(".aw-install.lock")).unwrap();
    writer.try_lock().unwrap();
    let result = Host::prepare(
        &serde_json::to_vec(&document).unwrap(),
        "target",
        capabilities(),
        fixture.context(),
        Instant::now() + TIMEOUT,
        &AtomicBool::new(false),
    );
    assert!(matches!(
        result,
        Err(aw_host::Error::PackageLease(aw_exec::LeaseError::Busy(_)))
    ));
    assert!(fixture.calls("describe").is_empty());
    fixture.assert_reaped();
}
