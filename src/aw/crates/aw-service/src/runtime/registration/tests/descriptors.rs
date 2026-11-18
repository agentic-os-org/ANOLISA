//! Replay history must not retain process descriptors after owner cleanup.

use super::*;
use std::process::{Child, Command};

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn descriptors() -> (usize, usize) {
    let mut counts = (0, 0);
    for entry in fs::read_dir("/proc/self/fd").unwrap() {
        let Ok(target) = fs::read_link(entry.unwrap().path()) else {
            continue;
        };
        let target = target.to_string_lossy();
        if target == "anon_inode:[pidfd]" {
            counts.0 += 1;
        } else if target.starts_with("/proc/") && !target.ends_with("/fd") {
            counts.1 += 1;
        }
    }
    counts
}

#[test]
fn replay_history_releases_process_descriptors() {
    // Isolate descriptor counts from concurrently running libtest cases.
    if std::env::var_os("AW_REGISTRATION_FD_TEST").is_none() {
        let mut child = ChildGuard(
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", thread::current().name().unwrap(), "--nocapture"])
                .env("AW_REGISTRATION_FD_TEST", "1")
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                assert!(status.success());
                return;
            }
            assert!(Instant::now() < deadline, "descriptor regression timed out");
            thread::sleep(Duration::from_millis(10));
        }
    }

    let f = Fixture::new();
    let baseline = descriptors();
    for index in 0..3 {
        let request = format!("released-{index}");
        let snapshot = f.call(f.command(&request)).unwrap();
        assert_eq!(descriptors(), (baseline.0 + 1, baseline.1 + 1));
        let h = handle(&snapshot);
        f.call(RegistrationCommand::Ready {
            handle: h.clone(),
            policy_revision: snapshot["policy_revision"].as_str().unwrap().into(),
        })
        .unwrap();
        f.call(RegistrationCommand::Release { handle: h }).unwrap();
        assert_eq!(f.query(&request)["state"], "released");
        assert_eq!(f.call(f.command(&request)).unwrap()["state"], "released");
        assert_eq!(
            descriptors(),
            baseline,
            "released owner retained descriptors"
        );
    }

    fs::write(f.directory.path().join("hold"), "").unwrap();
    let cancel = AtomicBool::new(false);
    thread::scope(|scope| {
        let preparation = scope.spawn(|| {
            f.runtime.registration(
                f.command("failed"),
                &Process::read(std::process::id()).unwrap(),
                Instant::now() + Duration::from_secs(10),
                &cancel,
            )
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while !f.directory.path().join("preparing").exists() {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(descriptors(), (baseline.0 + 1, baseline.1 + 1));
        cancel.store(true, Ordering::Release);
        assert!(preparation.join().unwrap().is_err());
    });
    assert_eq!(f.query("failed")["state"], "failed");
    assert_eq!(f.call(f.command("failed")).unwrap()["state"], "failed");
    assert_eq!(
        descriptors(),
        baseline,
        "failed preparation retained descriptors"
    );
    fs::remove_file(f.directory.path().join("hold")).unwrap();

    let mut child = ChildGuard(Command::new("sleep").arg("20").spawn().unwrap());
    let peer = Process::read(child.0.id()).unwrap();
    let mut command = f.command("exited");
    if let RegistrationCommand::Register { key, .. } = &mut command {
        key.owner.pid = child.0.id();
        key.owner.start_ticks = peer.metadata()["start_ticks"].as_str().unwrap().into();
    }
    let snapshot = f
        .runtime
        .registration(
            command,
            &peer,
            Instant::now() + Duration::from_secs(10),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(descriptors(), (baseline.0 + 1, baseline.1 + 1));
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    f.runtime.maintain_scopes().unwrap();
    assert!(!f
        .runtime
        .instances
        .lock()
        .unwrap()
        .contains_key(&handle(&snapshot).binding.instance_id));
    assert!(f.runtime.owners.lock().unwrap().is_empty());
    assert_eq!(f.runtime.registrations.lock().unwrap().len(), 5);
    assert_eq!(descriptors(), baseline, "exited owner retained descriptors");
}
