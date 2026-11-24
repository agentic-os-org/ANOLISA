use super::*;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

const OTEL: &str = "opentelemetry/instrumentation/auto_instrumentation";

fn command(cwd: &Path, paths: &str) -> CommandSpec {
    CommandSpec {
        program: "python3".into(),
        args: vec!["worker.py".into()],
        cwd: cwd.to_owned(),
        environment: [(OsString::from("PYTHONPATH"), paths.into())].into(),
    }
}

#[test]
fn filters_exact_suffixes_and_keeps_other_entries_in_order() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("sitecustomize.py"), "").unwrap();
    let kept = format!(
        ":/a/{OTEL}/child:{}:instrumentation/auto_instrumentation:/b/opentelemetry/instrumentation/my_auto_instrumentation:relative/./path/:",
        dir.path().display()
    );
    let path = format!("/opt/{OTEL}:{kept}:{OTEL}/:middle:/other/{OTEL}/./");
    let mut cmd = command(dir.path(), &path);
    isolate_python_environment(&mut cmd).unwrap();
    assert_eq!(
        cmd.environment[OsStr::new("PYTHONPATH")],
        OsStr::new(&format!("{kept}:middle"))
    );
    assert_eq!(cmd.environment[OsStr::new("OTEL_SDK_DISABLED")], "true");
}

#[test]
fn child_sees_no_pythonpath_when_all_entries_are_filtered() {
    let dir = tempfile::tempdir().unwrap();
    let original = command(dir.path(), &format!("{OTEL}:/another/{OTEL}/"));
    let script = r#"import os, sys
from pathlib import Path
assert 'PYTHONPATH' not in os.environ
Path(sys.argv[1]).write_text(os.environ['OTEL_SDK_DISABLED'], encoding='utf-8')
"#;
    assert_eq!(
        Probe(&AtomicBool::new(false))
            .python_output(
                &original,
                &["-c".into(), script.into()],
                PYTHON_PROBE_TIMEOUT,
            )
            .unwrap(),
        "true"
    );
    assert!(!original
        .environment
        .contains_key(OsStr::new("OTEL_SDK_DISABLED")));
}

#[test]
fn private_result_is_bounded_and_removed_on_success_and_failure() {
    let dir = tempfile::tempdir().unwrap();
    let cmd = command(dir.path(), "");
    let script = r#"import sys
from pathlib import Path
result = Path(sys.argv[2])
assert result.stat().st_mode & 0o777 == 0o600
Path('result-path').write_text(str(result))
print('stdout is independent of the result')
exec(sys.argv[1])
"#;
    for (action, expected) in [
        ("pass", Err("empty result")),
        (
            "result.write_text('x' * 4097)",
            Err("Result exceeded byte limit 4096"),
        ),
        ("result.write_bytes(b'\\xff')", Err("utf-8")),
        (
            "result.write_text('valid'); sys.exit(1)",
            Err("exited unsuccessfully"),
        ),
        ("result.write_text('x' * 4096)", Ok("x".repeat(4096))),
    ] {
        let output = Probe(&AtomicBool::new(false)).python_output(
            &cmd,
            &["-c".into(), script.into(), action.into()],
            PYTHON_PROBE_TIMEOUT,
        );
        match expected {
            Ok(value) => assert_eq!(output.unwrap(), value),
            Err(message) => assert!(
                output.unwrap_err().to_string().contains(message),
                "{action}"
            ),
        }
        let path = PathBuf::from(fs::read_to_string(dir.path().join("result-path")).unwrap());
        assert!(!path.exists(), "{action}");
    }
}

#[test]
fn result_overflow_stops_a_live_writer_without_growing_a_disk_file() {
    let dir = tempfile::tempdir().unwrap();
    let cmd = command(dir.path(), "");
    let script = r#"import os, stat, sys, time
from pathlib import Path
with open(sys.argv[1], 'wb', buffering=0) as result:
    result.write(b'x' * 4096)
    info = os.fstat(result.fileno())
    assert stat.S_ISFIFO(info.st_mode) and info.st_size == info.st_blocks == 0
    Path('writer').write_text(str(os.getpid()))
    for _ in range(20):
        result.write(b'x' * 4096)
        time.sleep(0.1)
    Path('continued').touch()
    time.sleep(10)
"#;
    let error = Probe(&AtomicBool::new(false))
        .python_output(&cmd, &["-c".into(), script.into()], Duration::from_secs(5))
        .unwrap_err();
    assert!(
        matches!(
            error,
            crate::Error::Exec(aw_exec::Error::OutputLimit {
                stream: aw_exec::Stream::Result,
                limit: 4096
            })
        ),
        "{error}"
    );
    assert!(!dir.path().join("continued").exists());
    let pid = fs::read_to_string(dir.path().join("writer")).unwrap();
    assert!(!Path::new("/proc").join(pid).exists());
}
