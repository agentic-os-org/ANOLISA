//! Direct view queries hold installed files through cancellation and process cleanup.

use super::*;
use std::{fs, fs::File, os::unix::fs::PermissionsExt, thread};

#[test]
fn direct_view_queries_hold_files_until_cancelled_children_are_reaped() {
    let directory = tempfile::tempdir_in("/tmp").unwrap();
    let prefix = directory.path().join("installed");
    fs::create_dir_all(prefix.join(".aw-packages")).unwrap();
    for path in [directory.path(), &prefix, &prefix.join(".aw-packages")] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let lock = prefix.join(".aw-install.lock");
    fs::write(&lock, []).unwrap();
    fs::set_permissions(&lock, fs::Permissions::from_mode(0o644)).unwrap();
    let script = prefix.join("view.sh");
    let ready = directory.path().join("ready");
    fs::write(&script, b"printf ready > \"$1\"\nwhile :; do :; done\n").unwrap();
    let registration = json!({"transport":{"argv":["/bin/sh",script,ready]}});
    let context = ProcessContext {
        cwd: directory.path().into(),
        environment: Default::default(),
        stderr_bytes: 65536,
    };
    let protocol = view::Protocol::new().unwrap();
    let cancelled = AtomicBool::new(false);
    let deadline = Instant::now() + Duration::from_secs(5);
    thread::scope(|scope| {
        let worker = scope.spawn(|| {
            query(
                &protocol,
                &registration,
                "fixture",
                &"a".repeat(64),
                &context,
                deadline,
                &cancelled,
            )
        });
        while !ready.exists() && !worker.is_finished() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(2));
        }
        let started = ready.exists();
        let held = File::open(&lock).unwrap().try_lock().is_err();
        cancelled.store(true, std::sync::atomic::Ordering::Release);
        assert_eq!(worker.join().unwrap(), Err("query_cancelled"));
        assert!(started && held);
    });
    File::open(&lock).unwrap().try_lock().unwrap();

    let writer = File::open(&lock).unwrap();
    writer.try_lock().unwrap();
    fs::remove_file(&ready).unwrap();
    assert_eq!(
        query(
            &protocol,
            &registration,
            "fixture",
            &"a".repeat(64),
            &context,
            Instant::now() + Duration::from_secs(3),
            &AtomicBool::new(false),
        ),
        Err("query_failed")
    );
    assert!(!ready.exists());
}
