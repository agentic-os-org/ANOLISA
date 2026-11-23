use super::*;
use std::fs;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn scratch_collisions_leave_existing_files_and_links_untouched() {
    const CHILD: &str = "KTUNER_SCRATCH_COLLISION_CHILD";
    if let Ok(mode) = std::env::var(CHILD) {
        let cwd = std::env::current_dir().unwrap();
        // Never run the fixture against a fallback directory outside this sandbox.
        assert_eq!(bench_io_dir(), cwd.to_string_lossy());
        let throughput = mode.starts_with("throughput");
        let name = if throughput {
            "ktuner_io_tput_bench"
        } else {
            "ktuner_io_bench"
        };
        let path = bench_io_path(cwd.to_str().unwrap(), name);
        if mode.ends_with("fresh") {
            let result = if throughput {
                bench_io_throughput()
            } else {
                bench_io_latency()
            }
            .unwrap();
            assert!(result.value.is_finite());
            assert!(result.value > 0.0);
            assert!(!std::path::Path::new(&path).exists());
            return;
        }
        let original = b"existing file must survive a scratch-name collision";
        let target = cwd.join("fixture-target");
        if mode.ends_with("link") {
            fs::write(&target, original).unwrap();
            std::os::unix::fs::symlink(&target, &path).unwrap();
        } else {
            fs::write(&path, original).unwrap();
        }
        let result = if throughput {
            bench_io_throughput()
        } else {
            bench_io_latency()
        };
        assert!(
            result.is_err(),
            "an existing scratch path must not be opened for truncation"
        );
        assert_eq!(fs::read(&path).unwrap(), original);
        if mode.ends_with("link") {
            assert!(fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink());
            assert_eq!(fs::read(target).unwrap(), original);
        }
        return;
    }
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let base = bench_io_dir();
    if is_tmpfs(&base) {
        // No disk-backed candidate exists. Running a child would select the
        // shared /tmp fallback instead of its private fixture directory.
        eprintln!("skipping scratch fixtures: no disk-backed benchmark directory");
        return;
    }
    let root = std::path::Path::new(&base)
        .join(format!(".scratch-fixture-{}-{unique}", std::process::id()));
    fs::create_dir(&root).unwrap();
    let mut failures = Vec::new();
    for mode in [
        "latency-file",
        "latency-link",
        "throughput-file",
        "throughput-link",
        "latency-fresh",
        "throughput-fresh",
    ] {
        let dir = root.join(mode);
        fs::create_dir(&dir).unwrap();
        let result = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "bench::collision_tests::scratch_collisions_leave_existing_files_and_links_untouched", "--nocapture"])
            .current_dir(&dir).env(CHILD, mode).output().unwrap();
        if !result.status.success() {
            failures.push(format!(
                "{mode}: {}\n{}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            ));
        }
    }
    fs::remove_dir_all(root).unwrap();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
