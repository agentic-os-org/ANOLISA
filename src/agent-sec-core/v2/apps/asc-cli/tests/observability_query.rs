//! Real CLI and UDS acceptance, including paged reports and terminal restoration.

use std::{
    path::Path,
    process::{Command, Stdio},
    sync::Arc,
    time::Duration,
};

use asc_daemon_core::{
    ObservabilityService, ObservabilitySink, ObservabilityWriteError, RootManagedPrincipalPolicy,
    query::ObservabilityQueryService,
};
use asc_daemon_handler::{DaemonDispatcher, JsonRejectionEncoder, QueryHandler};
use asc_daemon_protocol::{DaemonRequest, DaemonResponse};
use asc_daemon_service::ShutdownToken;
use asc_observability::ObservabilityRecord;
use asc_persistence_sqlite::{
    observability::owned::OwnedObservabilityWriter,
    query::{SqliteObservabilityQueries, SqliteSecurityQueries},
    security_events::writer::SqliteEventWriter,
};
use serde_json::{Value, json};

mod common;

struct Writer(OwnedObservabilityWriter);
impl ObservabilitySink for Writer {
    fn write(
        &self,
        record: &ObservabilityRecord,
        owner: u32,
    ) -> Result<(), ObservabilityWriteError> {
        self.0
            .write(record, owner)
            .map_err(|_| ObservabilityWriteError::Storage)
    }
}

fn cli(socket: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_agent-sec-cli"))
        .arg("--socket")
        .arg(socket)
        .args(["observability"])
        .args(args)
        .env("RUST_LOG", "off")
        .output()
        .unwrap()
}

fn call(socket: &Path, method: &str, params: Value) -> DaemonResponse {
    asc_daemon_client::call(
        socket,
        &DaemonRequest {
            method: method.into(),
            params,
            trace_context: None,
            compatibility: None,
        },
        Duration::from_secs(5),
    )
    .unwrap()
}

#[allow(clippy::too_many_lines)] // One process scenario verifies ingestion, queries, terminal cleanup and shutdown together.
/// QRY-001/004/007/009/010: real peer, report pagination, PTY cleanup and no fallback.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uds_peer_owns_ingestion_and_cli_pages_reports_without_local_fallback() {
    let directory = common::Directory::new();
    let socket = directory.0.join("daemon.sock");
    let obs = directory.0.join("obs.db");
    let sec = directory.0.join("sec.db");
    let uid = rustix::process::getuid().as_raw();
    let writer = OwnedObservabilityWriter::new(&obs).unwrap();
    for owner in [uid, uid + 1] {
        for _ in 0..205 {
            let record = ObservabilityRecord::from_json_value(&json!({
                "hook":"after_llm_call","observedAt":if owner == uid { "2030-01-01T00:00:00Z" } else { "2029-01-01T00:00:00Z" },
                "metadata":{"sessionId":if owner == uid { "same" } else { "foreign" },"runId":"run","callId":"call"},
                "metrics":{"request_payload_bytes":10,"response_stream_bytes":20}
            }))
            .unwrap();
            writer.write(&record, owner).unwrap();
        }
    }
    let security_writer = SqliteEventWriter::new(&sec).unwrap();
    security_writer.probe().unwrap();
    for owner in [uid, uid + 1] {
        let mut event = asc_security_events::SecurityEvent::new(
            "scan",
            "prompt_scan",
            json!({"result":{"verdict": if owner == uid { "deny" } else { "FOREIGN-SECRET" }}})
                .as_object()
                .unwrap()
                .clone(),
        );
        event.uid = owner;
        event.session_id = Some("new".into());
        event.run_id = Some("run".into());
        event.set_timestamp("2030-01-02T00:00:00Z").unwrap();
        security_writer.write(&event);
    }
    security_writer.close();
    drop(security_writer);
    let query = ObservabilityQueryService::new(
        SqliteObservabilityQueries::new(obs.clone()),
        SqliteSecurityQueries::new(sec.clone()),
    );
    let dispatcher = DaemonDispatcher::new(
        asc_pap::PapService::new(Arc::new(
            asc_pap_repository_memory::ProcessLocalPapRepository::default(),
        )),
        // Policy administration must never widen a non-root query's owner scope.
        Arc::new(RootManagedPrincipalPolicy::with_admin_uids([uid])),
        asc_daemon::scan_application(
            asc_action_runtime::testing::discarding_finalizer(),
            Arc::new(asc_capability_pii_scan::PiiRuleSet::builtin().unwrap()),
        ),
    )
    .with_observability(ObservabilityService::new(Arc::new(Writer(writer))))
    .with_queries(QueryHandler::default().with_observability_queries(query));
    let shutdown = ShutdownToken::new();
    let stop = shutdown.clone();
    let config = asc_daemon::BootstrapConfig::new(&socket);
    let task = tokio::spawn(async move {
        asc_daemon::serve(
            config,
            Arc::new(dispatcher),
            Arc::new(JsonRejectionEncoder),
            stop,
        )
        .await
        .unwrap();
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if tokio::net::UnixStream::connect(&socket).await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();

    let result = cli(&socket, &["report", "--last", "--format", "json"]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let report: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert!(report.get("uid").is_none());
    assert_eq!(report["llm_calls"], 205);
    assert_eq!(report["request_bytes"], 2050);
    assert_eq!(report["response_bytes"], 4100);
    assert!(
        cli(&socket, &["report", "--session-id", "same"])
            .status
            .success()
    );
    for field in ["uid", "owner_uid"] {
        let response = call(&socket, "obs.sessions.list", json!({field:uid}));
        let DaemonResponse::Error(response) = response else {
            panic!("caller-supplied UID accepted");
        };
        assert_eq!(response.error.code.as_str(), "invalid_argument");
    }
    let input = json!({"hook":"before_agent_run","observedAt":"2030-01-02T00:00:00Z","metadata":{"sessionId":"new","runId":"run"},"metrics":{"user_input":"hello"}});
    let mut child = Command::new(env!("CARGO_BIN_EXE_agent-sec-cli"))
        .arg("--socket")
        .arg(&socket)
        .args([
            "--otel-context",
            r#"{"version":1,"baggage":"uid=0,role=root,owner_uid=0"}"#,
            "observability",
            "record",
            "--stdin",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    std::io::Write::write_all(
        &mut child.stdin.take().unwrap(),
        input.to_string().as_bytes(),
    )
    .unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let result = cli(&socket, &["report", "--last", "--format", "json"]);
    let report: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert!(report.get("uid").is_none());
    assert_eq!(report["session_id"], "new");

    // Non-TTY review fails explicitly instead of silently becoming a batch listing.
    let result = cli(&socket, &["review"]);
    assert_eq!(result.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&result.stderr).contains("interactive terminal"));
    let tty = Command::new("python3")
        .args([
            "-c",
            PTY_REVIEW,
            env!("CARGO_BIN_EXE_agent-sec-cli"),
            socket.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        tty.status.success(),
        "{}",
        String::from_utf8_lossy(&tty.stderr)
    );

    let empty = Command::new("python3")
        .args([
            "-c",
            PTY_REVIEW,
            env!("CARGO_BIN_EXE_agent-sec-cli"),
            socket.to_str().unwrap(),
            obs.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        empty.status.success(),
        "{}",
        String::from_utf8_lossy(&empty.stderr)
    );
    let writer = OwnedObservabilityWriter::new(&obs).unwrap();
    writer
        .write(&ObservabilityRecord::from_json_value(&input).unwrap(), uid)
        .unwrap();

    // Corrupted required security storage must fail the complete report.
    std::fs::write(&sec, b"corrupt").unwrap();
    let result = cli(&socket, &["report", "--last", "--format", "json"]);
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    assert!(String::from_utf8_lossy(&result.stderr).contains("internal"));
    assert!(matches!(
        call(&socket, "obs.sessions.list", json!({"uid":0})),
        DaemonResponse::Error(_)
    ));
    shutdown.request();
    task.await.unwrap();
    let result = cli(&socket, &["report", "--last", "--format", "json"]);
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
}

const PTY_REVIEW: &str = r"
import fcntl, os, pty, select, signal, sqlite3, struct, subprocess, sys, termios, time
if len(sys.argv)>3:
    with sqlite3.connect(sys.argv[3]) as store: store.execute('DELETE FROM observability_events')
master, slave = pty.openpty()
fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 40, 160, 0, 0))
before = termios.tcgetattr(slave)
child = subprocess.Popen([sys.argv[1], '--socket', sys.argv[2], 'observability', 'review'], stdin=slave, stdout=slave, stderr=slave, env=dict(os.environ, TZ='Asia/Shanghai'))
data = b''
def wait_for(needle):
    global data
    deadline = time.monotonic()+10
    while needle not in data:
        assert child.poll() is None, data
        assert time.monotonic()<deadline, data
        if select.select([master], [], [], .1)[0]: data += os.read(master, 65536)
    for hidden in [b'FOREIGN-SECRET', b'Observability review |', b'UID:', b'Enter/click:', b'q/Esc:', b'Left/Right:']:
        assert hidden not in data, data
def press(key, needle):
    global data
    data = b''
    os.write(master,key)
    wait_for(needle)
try:
    if len(sys.argv)>3:
        wait_for(b'No observability records found.')
        os.write(master,b'q')
        assert child.wait(timeout=5)==0
        assert termios.tcgetattr(slave)==before
        sys.exit(0)
    wait_for(b'Last seen')
    wait_for(b'2030-01-02 08:00:00')
    # A row click performs the same drill-down as Enter.
    press(b'\x1b[<0;3;2M\x1b[<0;3;2m', b'Started')
    press(b'\r', b'Security Result')
    wait_for(b'prompt_scan:deny')
    press(b'\r', b'Security Events:')
    for marker in [b'Metadata:', b'Metrics:', b'match=run_id', b'deny', b'2030-01-02T00:00:00+00:00']:
        wait_for(marker)
    # Resize while idle must redraw without a key, and details remain scrollable.
    data = b''
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 12, 50, 0, 0))
    os.kill(child.pid, signal.SIGWINCH)
    wait_for(b'Hook:')
    press(b'n', b'Metrics:')
    press(b'q', b'Hook')
    press(b'\x1b', b'Started')
    press(b'q', b'Last seen')
    os.write(master,b'q')
    assert child.wait(timeout=5)==0
    assert termios.tcgetattr(slave)==before, 'terminal attributes were not restored'
    # A fresh PTY prevents buffered output from the old child satisfying readiness.
    os.close(master); os.close(slave)
    master, slave = pty.openpty()
    before = termios.tcgetattr(slave)
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 40, 160, 0, 0))
    data = b''
    child = subprocess.Popen([sys.argv[1], '--socket', sys.argv[2], 'observability', 'review'], stdin=slave, stdout=slave, stderr=slave)
    wait_for(b'Last seen')
    press(b'j', b'Last seen')
    press(b'\r', b'Started')
    press(b'\r', b'1-100 / 205')
    press(b'n', b'101-200 / 205')
    press(b'n', b'201-205 / 205')
    press(b'\x1b[B', b'201-205 / 205')
    press(b'\r', b'Hook:')
    press(b'q', b'201-205 / 205')
    press(b'p', b'101-200 / 205')
    press(b'q', b'Started')
    press(b'\x1b', b'Last seen')
    os.write(master,b'q')
    assert child.wait(timeout=5)==0
    assert termios.tcgetattr(slave)==before
    child = subprocess.Popen([sys.argv[1], '--socket', sys.argv[2]+'.missing', 'observability', 'review'], stdin=slave, stdout=slave, stderr=slave)
    assert child.wait(timeout=5)==1
    assert termios.tcgetattr(slave)==before, 'query failure must restore terminal'
    for sig in [signal.SIGINT, signal.SIGTERM, signal.SIGHUP]:
        for detail in [False, True]:
            os.close(master); os.close(slave)
            master, slave = pty.openpty()
            before = termios.tcgetattr(slave)
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 40, 160, 0, 0))
            data = b''
            child = subprocess.Popen([sys.argv[1], '--socket', sys.argv[2], 'observability', 'review'], stdin=slave, stdout=slave, stderr=slave)
            wait_for(b'Last seen')
            if detail:
                press(b'\r', b'Started')
                press(b'\r', b'Security Result')
                press(b'\r', b'Security Events:')
            data = b''
            os.kill(child.pid, sig)
            assert child.wait(timeout=5) == 128 + sig, (sig, child.returncode)
            assert termios.tcgetattr(slave) == before, ('signal must restore terminal', sig)
            while select.select([master], [], [], .1)[0]:
                data += os.read(master, 65536)
            for cleanup in [b'\x1b[?1049l', b'\x1b[?25h', b'\x1b[?1000l', b'\x1b[?1006l']:
                assert cleanup in data, (sig, cleanup, data)
finally:
    if child.poll() is None: child.kill(); child.wait()
    os.close(master); os.close(slave)
";

#[test]
fn schema_and_report_usage_do_not_require_a_daemon() {
    let output = Command::new(env!("CARGO_BIN_EXE_agent-sec-cli"))
        .env("AGENT_SEC_DAEMON_SOCKET", "relative-invalid-socket")
        .args(["observability", "schema"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let schema: Value = serde_json::from_slice(&output.stdout).unwrap();
    let expected: Value = serde_json::from_str(include_str!(
        "../../../fixtures/query/v1-record-schema.json"
    ))
    .unwrap();
    assert_eq!(schema, expected);
    for command in ["report", "review"] {
        for flag in ["--since", "--until"] {
            let output = cli(
                Path::new("/nonexistent/obs-test.sock"),
                &[command, flag, "1000"],
            );
            assert_eq!(output.status.code(), Some(2));
            assert!(String::from_utf8_lossy(&output.stderr).contains("unexpected argument"));
        }
    }
    for flag in ["--session-id", "--run-id"] {
        let output = cli(
            Path::new("/nonexistent/obs-test.sock"),
            &["review", flag, "session"],
        );
        assert_eq!(output.status.code(), Some(2));
    }
    for args in [vec!["report"], vec!["report", "--last", "--format", "bad"]] {
        let output = cli(Path::new("/nonexistent/obs-test.sock"), &args);
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("connect"));
    }
}
