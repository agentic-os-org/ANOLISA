use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use asc_daemon::{BootstrapConfig, serve};
use asc_daemon_core::{PeerCredentials, PrincipalPolicy, PrincipalRole};
use asc_daemon_handler::{DaemonDispatcher, JsonRejectionEncoder};
use asc_daemon_service::{
    DispatchError, DispatchRequest, RequestDispatcher, ResponseDisposition, ShutdownToken,
};
use asc_pap::PapService;
use asc_pap_repository_memory::ProcessLocalPapRepository;
use asc_policy_engine::PolicyTemplateCompiler;
use serde_json::{Value, json};
use uuid::Uuid;

mod common;

struct TestPolicy(PrincipalRole);
impl PrincipalPolicy for TestPolicy {
    fn role_for(&self, _peer: PeerCredentials) -> PrincipalRole {
        self.0
    }
}

struct RecordingDispatcher {
    inner: Arc<dyn RequestDispatcher>,
    requests: Arc<Mutex<Vec<Value>>>,
}
impl RequestDispatcher for RecordingDispatcher {
    fn dispatch(
        &self,
        request: DispatchRequest,
        response: &mut dyn std::io::Write,
    ) -> Result<ResponseDisposition, DispatchError> {
        self.requests
            .lock()
            .unwrap()
            .push(serde_json::from_slice(&request.payload).unwrap());
        self.inner.dispatch(request, response)
    }
}

async fn start(
    socket: &Path,
    role: PrincipalRole,
) -> (
    ShutdownToken,
    tokio::task::JoinHandle<()>,
    Arc<Mutex<Vec<Value>>>,
) {
    let application = PapService::new(
        Arc::new(ProcessLocalPapRepository::default()),
        Arc::new(PolicyTemplateCompiler),
    );
    let inner = Arc::new(DaemonDispatcher::new(
        application,
        Arc::new(TestPolicy(role)),
        asc_daemon::scan_application(asc_action_runtime::testing::discarding_finalizer()),
    ));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let dispatcher = Arc::new(RecordingDispatcher {
        inner,
        requests: Arc::clone(&requests),
    });
    let config = BootstrapConfig::new(socket);
    let shutdown = ShutdownToken::new();
    let service_shutdown = shutdown.clone();
    let task = tokio::spawn(async move {
        serve(
            config,
            dispatcher,
            Arc::new(JsonRejectionEncoder),
            service_shutdown,
        )
        .await
        .unwrap();
    });
    // Binding publishes the socket file slightly before the listener starts
    // accepting, so waiting for the path to exist can hand back an endpoint that
    // still refuses connections. Probing with a real connect is what makes the
    // readiness signal trustworthy; the probe closes immediately.
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Ok(probe) = tokio::net::UnixStream::connect(socket).await {
                drop(probe);
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    (shutdown, task, requests)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_cli_processes_execute_the_complete_frozen_pap_crud_scenario() {
    let directory = common::Directory::new();
    let socket = directory.0.join("daemon.sock");
    let (shutdown, task, requests) = start(&socket, PrincipalRole::PolicyAdministrator).await;
    let fixture: Value = serde_json::from_str(common::SCENARIO).unwrap();
    let mut variables = BTreeMap::new();
    let mut expected_requests = Vec::new();
    for step in fixture["steps"].as_array().unwrap() {
        let request = common::expand(&step["request"], &fixture["objects"], &variables);
        let args = common::args_for(&request, &directory.0, &socket);
        let output = tokio::task::spawn_blocking(move || common::run(&args))
            .await
            .unwrap();
        assert!(
            output.status.success(),
            "{}: {}",
            step["name"],
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        let wrapper = json!({"result":result});
        for capture in step["captures"].as_array().unwrap() {
            let value = wrapper
                .pointer(capture["pointer"].as_str().unwrap())
                .unwrap()
                .clone();
            Uuid::parse_str(value.as_str().unwrap()).unwrap();
            assert!(
                variables
                    .insert(capture["name"].as_str().unwrap().to_owned(), value)
                    .is_none()
            );
        }
        assert_eq!(
            result,
            common::expand(
                &step["expectedResponse"]["result"],
                &fixture["objects"],
                &variables
            ),
            "{}",
            step["name"]
        );
        expected_requests.push(request);
    }
    assert_ne!(variables["policy_id"], variables["scope_id"]);
    assert_ne!(variables["binding_id"], variables["scope_id"]);
    assert_ne!(variables["binding_id"], variables["policy_id"]);
    let actual: Vec<_> = requests
        .lock()
        .unwrap()
        .iter()
        .cloned()
        .map(|mut request| {
            let carrier: asc_daemon_protocol::TraceCarrierV1 = serde_json::from_value(
                request
                    .as_object_mut()
                    .unwrap()
                    .remove("traceContext")
                    .expect("CLI must propagate its SDK context"),
            )
            .unwrap();
            assert_eq!(carrier.version, 1);
            assert_eq!(carrier.traceparent.as_ref().unwrap().len(), 55);
            request
        })
        .collect();
    assert_eq!(actual, expected_requests);
    let request = json!({"method":"policy.templates.get","params":{"id":variables["policy_id"],"revision":2}});
    let args = common::args_for(&request, &directory.0, &socket);
    let output = tokio::task::spawn_blocking(move || common::run(&args))
        .await
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "not_found");
    Uuid::parse_str(error["requestId"].as_str().unwrap()).unwrap();
    shutdown.request();
    task.await.unwrap();
    assert!(!socket.exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_cli_processes_code_scan_through_the_daemon() {
    let directory = common::Directory::new();
    let socket = directory.0.join("daemon.sock");
    let (shutdown, task, requests) = start(&socket, PrincipalRole::LocalUser).await;
    let scan_args = vec![
        OsString::from("--socket"),
        socket.into_os_string(),
        OsString::from("scan-code"),
        OsString::from("--code"),
        OsString::from("rm -rf /tmp/test"),
    ];
    let output = tokio::task::spawn_blocking(move || common::run(&scan_args))
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["ok"], true);
    assert_eq!(result["verdict"], "warn");
    assert_eq!(result["language"], "bash");
    let mut actual = requests.lock().unwrap().clone();
    let carrier: asc_daemon_protocol::TraceCarrierV1 = serde_json::from_value(
        actual[0]
            .as_object_mut()
            .unwrap()
            .remove("traceContext")
            .unwrap(),
    )
    .unwrap();
    assert_eq!(carrier.version, 1);
    assert_eq!(carrier.traceparent.as_ref().unwrap().len(), 55);
    assert_eq!(
        actual,
        vec![json!({
            "method": "action.code_scan",
            "params": {
                "code": "rm -rf /tmp/test",
                "language": "bash",
                "rules": null,
                "mode": "regex"
            }
        })]
    );

    let bad_language = vec![
        OsString::from("--socket"),
        directory.0.join("daemon.sock").into_os_string(),
        OsString::from("scan-code"),
        OsString::from("--code"),
        OsString::from("puts 1"),
        OsString::from("--language"),
        OsString::from("ruby"),
    ];
    let output = tokio::task::spawn_blocking(move || common::run(&bad_language))
        .await
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(output.stderr, b"scan error: unsupported language: ruby\n");

    let empty = vec![
        OsString::from("--socket"),
        directory.0.join("daemon.sock").into_os_string(),
        OsString::from("scan-code"),
    ];
    let output = tokio::task::spawn_blocking(move || common::run(&empty))
        .await
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(
        output.stderr,
        b"Error: --code is required (use --code '<source>')\n"
    );
    assert_eq!(requests.lock().unwrap().len(), 2);

    shutdown.request();
    task.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unauthorized_cli_cannot_read_or_modify_any_pap_resource() {
    let directory = common::Directory::new();
    let socket = directory.0.join("daemon.sock");
    let (shutdown, task, _) = start(&socket, PrincipalRole::LocalUser).await;
    let methods: Value = serde_json::from_str(common::METHODS).unwrap();
    for row in methods.as_array().unwrap() {
        let args = common::args_for(row, &directory.0, &socket);
        let output = tokio::task::spawn_blocking(move || common::run(&args))
            .await
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["error"]["code"], "permission_denied");
        assert!(!error["error"]["message"].as_str().unwrap().is_empty());
        Uuid::parse_str(error["requestId"].as_str().unwrap()).unwrap();
    }
    shutdown.request();
    task.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn domain_validation_and_pagination_are_owned_by_the_daemon() {
    let directory = common::Directory::new();
    let socket = directory.0.join("daemon.sock");
    let (shutdown, task, requests) = start(&socket, PrincipalRole::PolicyAdministrator).await;
    let request = json!({"method":"policy.templates.create","params":{"policyName":"", "template":{"kind":"prevent_file_deletion","files":["/work"]}}});
    let args = common::args_for(&request, &directory.0, &socket);
    let output = tokio::task::spawn_blocking(move || common::run(&args))
        .await
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stderr).unwrap()["error"]["code"],
        "invalid_argument"
    );
    for pid in 1..=3 {
        let request =
            json!({"method":"policy.scopes.create","params":{"selector":{"kind":"pid","pid":pid}}});
        let args = common::args_for(&request, &directory.0, &socket);
        assert!(
            tokio::task::spawn_blocking(move || common::run(&args))
                .await
                .unwrap()
                .status
                .success()
        );
    }
    let request = json!({"method":"policy.scopes.list","params":{"limit":1,"offset":1}});
    let args = common::args_for(&request, &directory.0, &socket);
    let output = tokio::task::spawn_blocking(move || common::run(&args))
        .await
        .unwrap();
    assert!(output.status.success());
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["total"], 3);
    assert_eq!(result["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        requests.lock().unwrap().len(),
        5,
        "CLI must not auto-page or perform hidden reads"
    );
    shutdown.request();
    task.await.unwrap();
}

#[test]
fn failed_binding_results_preserve_cli_stdout_and_mutation_exit_status() {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;

    let cases: Vec<Value> = serde_json::from_str(include_str!(
        "../../../fixtures/reconciliation/admission-wire.json"
    ))
    .unwrap();
    let methods: Value = serde_json::from_str(common::METHODS).unwrap();
    for case in cases {
        for operation in ["create", "update", "delete", "get"] {
            let directory = common::Directory::new();
            let socket = directory.0.join("result.sock");
            let listener = UnixListener::bind(&socket).unwrap();
            let binding = case["binding"].clone();
            let method = format!("policy.bindings.{operation}");
            let expected_method = method.clone();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = String::new();
                BufReader::new(&stream).read_line(&mut request).unwrap();
                assert_eq!(
                    serde_json::from_str::<Value>(&request).unwrap()["method"],
                    expected_method
                );
                writeln!(
                    stream,
                    "{}",
                    json!({"requestId":"10000000-0000-4000-8000-000000000001", "result":binding})
                )
                .unwrap();
            });
            let row = methods
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["method"] == method)
                .unwrap();
            let output = std::process::Command::new(env!("CARGO_BIN_EXE_agent-sec-cli"))
                .args(common::args_for(row, &directory.0, &socket))
                .output()
                .unwrap();
            server.join().unwrap();
            assert_eq!(output.status.code(), Some(i32::from(operation != "get")));
            assert!(output.stderr.is_empty());
            assert_eq!(
                serde_json::from_slice::<Value>(&output.stdout).unwrap(),
                case["binding"]
            );
        }
    }
}
