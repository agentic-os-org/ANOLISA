//! Parent/guard framing and lifecycle over real subprocess pipes.

use super::*;
use crate::backend_tests::{Fixture, fixture};

// Use real pipes and a separate process so closing stdin and waiting for the
// helper are observable, including when its final message precedes its exit.
fn peer(script: &str) -> Command {
    let mut command = Command::new("/usr/bin/python3");
    command
        .arg("-c")
        .arg(format!("import signal\nsignal.alarm(10)\n{script}"));
    command
}

fn request(fixture: &Fixture) -> Request {
    Request {
        config: fixture.config(),
        spec: fixture.spec(),
        name: format!("asc-sandbox-{}", uuid::Uuid::new_v4()),
    }
}

#[test]
fn parent_reports_attach_status_and_requires_confirmed_cleanup() {
    let fixture = fixture();
    let request = request(&fixture);
    let script = r#"
import json, sys
request = json.loads(input())
assert request["name"].startswith("asc-sandbox-")
host = request["config"]["host"]
directory = request["spec"]["launch"]["cwd"]
print(json.dumps({"Owned": {"directory": directory}}), flush=True)
print(json.dumps({"Ready": {"directory": directory, "host": host}}), flush=True)
assert json.loads(input()) == {"Finished": {"code": 7}}
print(json.dumps({"Done": {"result": {"installed": True, "started": True, "exit_code": 7}}}), flush=True)
assert not sys.stdin.read()
"#;
    let result = supervise(peer(script), &request).unwrap();
    assert!(result.installed && result.started);
    assert_eq!(result.exit_code, Some(7));
    assert_eq!(
        fixture.read_calls("start.args"),
        ["start", "--attach", "--interactive", &request.name]
    );
    assert_eq!(
        fixture.read_calls("client.calls"),
        [
            "--config",
            &request.spec.launch.cwd,
            "--host",
            &request.config.host
        ]
    );
    let error = supervise(peer(&format!("{script}\nsys.exit(1)")), &request).unwrap_err();
    assert!(matches!(error, DockerError::Protocol(s) if s.contains("did not confirm cleanup")));
}

#[test]
fn lost_guard_reports_owned_directory_and_container_identity() {
    let request = request(&fixture());
    let error = supervise(
        peer("input(); print('{\"Owned\":{\"directory\":\"/recovery/location\"}}')"),
        &request,
    )
    .unwrap_err();
    let DockerError::Protocol(message) = error else {
        panic!("expected unconfirmed cleanup");
    };
    for detail in [&request.name, &request.config.host, "/recovery/location"] {
        assert!(message.contains(detail), "{message}");
    }
}

#[test]
fn parent_preserves_guard_error_class_and_rejects_out_of_turn_finish() {
    let request = request(&fixture());
    for unsupported in [false, true] {
        let reply =
            serde_json::json!({"Failed": {"error": "placement", "unsupported": unsupported}});
        let error = supervise(peer(&format!("input(); print('{reply}')")), &request).unwrap_err();
        let message = match error {
            DockerError::Unsupported(message) if unsupported => message,
            DockerError::Protocol(message) if !unsupported => message,
            other => panic!("unexpected error: {other}"),
        };
        assert_eq!(message, "placement");
    }
    let reply = serde_json::json!({"Finished": {"code": 0}});
    let error = supervise(peer(&format!("input(); print('{reply}')")), &request).unwrap_err();
    assert!(matches!(error, DockerError::Protocol(s) if s.contains("unexpected guard message")));
}

#[test]
fn framing_rejects_partial_oversize_and_malformed_messages() {
    let mut wire = Vec::new();
    send(&mut wire, &"ok").unwrap();
    assert_eq!(receive::<String>(&mut wire.as_slice()).unwrap(), "ok");
    assert!(matches!(
        receive::<String>(&mut &wire[..wire.len() - 1]),
        Err(DockerError::Protocol(_))
    ));
    assert!(matches!(
        receive::<Message>(&mut &b"{}\n"[..]),
        Err(DockerError::Json(_))
    ));
    let limit = usize::try_from(MAX_MESSAGE).unwrap();
    let oversized = "x".repeat(limit);
    assert!(matches!(
        send(&mut wire, &oversized),
        Err(DockerError::Protocol(_))
    ));
    assert!(matches!(
        receive::<String>(&mut vec![b'x'; limit + 1].as_slice()),
        Err(DockerError::Protocol(_))
    ));
}

#[test]
fn guard_rejects_an_invalid_container_identity_before_placement() {
    let mut request = request(&fixture());
    request.name = "invalid".into();
    let mut output = Vec::new();
    let error = serve(request, &mut &b""[..], &mut output).unwrap_err();
    assert!(matches!(error, DockerError::Protocol(s) if s.contains("container identity")));
    assert!(output.is_empty());
}
