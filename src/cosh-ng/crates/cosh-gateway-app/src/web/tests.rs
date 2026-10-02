use std::fs;
use std::os::unix::fs::PermissionsExt;

use tempfile::tempdir;

use super::*;

fn request(headers: &[(&str, &str)]) -> HttpRequest {
    HttpRequest {
        method: "GET".to_owned(),
        target: "/api/v1/tasks".to_owned(),
        headers: headers
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect(),
        body: Vec::new(),
    }
}

fn private_tempdir() -> tempfile::TempDir {
    let directory = tempdir().unwrap();
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
    directory
}

#[test]
fn web_attests_workspace_and_authority_before_binding_http() {
    let directory = private_tempdir();
    let admitted = directory.path().join("admitted");
    let declared = directory.path().join("declared");
    fs::create_dir(&admitted).unwrap();
    fs::create_dir(&declared).unwrap();
    fs::set_permissions(&admitted, fs::Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(&declared, fs::Permissions::from_mode(0o700)).unwrap();
    let token = admitted.join("token");
    fs::write(&token, "0123456789abcdef0123456789abcdef").unwrap();
    fs::set_permissions(&token, fs::Permissions::from_mode(0o600)).unwrap();
    let resolver = TrustedWorkspaceResolver::new(
        GatewayCapabilityProfile::task_only_v1().governed_target(),
        &admitted,
    )
    .unwrap();
    let socket = directory.path().join("gateway.sock");
    let mut daemon = GatewayDaemon::bind(GatewayDaemonConfig {
        socket_path: socket.clone(),
        database_path: directory.path().join("gateway.db"),
        installation_id: None,
        launch_catalog: TaskLaunchCatalog::new(
            resolver.workspace_ref().clone(),
            LaunchReadiness::ready(),
            LaunchReadiness::ready(),
            LaunchReadiness::ready(),
        ),
    })
    .unwrap();
    // An occupied port makes a missing admission check fail immediately, not hang.
    let occupied = TcpListener::bind("127.0.0.1:0").unwrap();
    let args = WebArgs {
        bind: occupied.local_addr().unwrap(),
        socket: Some(socket.clone()),
        workspace: declared,
        token_file: token.clone(),
        output: Output::Jsonl,
    };
    let external_token = directory.path().join("token");
    fs::copy(token, &external_token).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let daemon_stop = Arc::clone(&stop);
    let server = std::thread::spawn(move || daemon.serve_until(&daemon_stop));
    let mismatch = web(
        args.clone(),
        &Reporter {
            output: Output::Jsonl,
        },
    );
    let authority = web(
        WebArgs {
            workspace: admitted,
            token_file: external_token,
            ..args.clone()
        },
        &Reporter {
            output: Output::Jsonl,
        },
    );
    stop.store(true, Ordering::Relaxed);
    server.join().unwrap().unwrap();
    let mismatch = mismatch.unwrap_err().to_string();
    assert!(
        mismatch.contains("admitted workspace does not match"),
        "{mismatch}"
    );
    let authority = authority.unwrap_err().to_string();
    assert!(
        authority.contains("brokered-only token boundary"),
        "{authority}"
    );
    let unavailable = web(
        WebArgs {
            token_file: directory.path().join("token"),
            ..args
        },
        &Reporter {
            output: Output::Jsonl,
        },
    );
    assert!(unavailable
        .unwrap_err()
        .to_string()
        .contains("cannot attest Gateway capabilities"));
}

#[test]
fn token_file_requires_exact_private_mode_and_owner() {
    let directory = private_tempdir();
    let path = directory.path().join("token");
    fs::write(&path, "0123456789abcdef0123456789abcdef\n").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        read_token(&path).unwrap().bytes,
        b"0123456789abcdef0123456789abcdef"
    );
    fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
    assert!(read_token(&path).is_err());
}

#[test]
fn token_inside_the_admitted_workspace_is_rejected() {
    let directory = private_tempdir();
    let workspace = directory.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    fs::set_permissions(&workspace, fs::Permissions::from_mode(0o700)).unwrap();
    let token = workspace.join("token");
    fs::write(&token, "0123456789abcdef0123456789abcdef").unwrap();
    fs::set_permissions(&token, fs::Permissions::from_mode(0o600)).unwrap();
    let loaded = read_token(&token).unwrap();
    assert!(validate_token_scope(&loaded.path, &workspace).is_err());
}

#[test]
fn token_file_rejects_symlinks_and_hard_links() {
    let directory = private_tempdir();
    let path = directory.path().join("token");
    let alias = directory.path().join("alias");
    fs::write(&path, "0123456789abcdef0123456789abcdef").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    std::os::unix::fs::symlink(&path, &alias).unwrap();
    assert!(read_token(&alias).is_err());
    fs::remove_file(&alias).unwrap();
    fs::hard_link(&path, &alias).unwrap();
    assert!(read_token(&path).is_err());
}

#[test]
fn drip_fed_request_cannot_extend_the_absolute_deadline() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let writer = std::thread::spawn(move || {
        let mut stream = TcpStream::connect(address).unwrap();
        for byte in b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n" {
            if stream.write_all(&[*byte]).is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    });
    let (mut stream, _) = listener.accept().unwrap();
    let started = std::time::Instant::now();
    assert!(read_request_with_deadline(&mut stream, Duration::from_millis(80)).is_err());
    assert!(started.elapsed() < Duration::from_millis(300));
    drop(stream);
    writer.join().unwrap();
}

#[test]
fn one_incomplete_request_does_not_block_another_client() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let workers = HttpWorkers::new(
        address,
        b"0123456789abcdef0123456789abcdef".to_vec(),
        PathBuf::from("/not-used"),
    )
    .unwrap();

    let mut incomplete = TcpStream::connect(address).unwrap();
    let (server, _) = listener.accept().unwrap();
    workers.submit(server).unwrap();
    incomplete.write_all(b"G").unwrap();

    let mut complete = TcpStream::connect(address).unwrap();
    complete
        .write_all(format!("GET / HTTP/1.1\r\nHost: {address}\r\n\r\n").as_bytes())
        .unwrap();
    let (server, _) = listener.accept().unwrap();
    workers.submit(server).unwrap();
    complete
        .set_read_timeout(Some(Duration::from_millis(300)))
        .unwrap();
    let mut response = String::new();
    complete.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200 OK"));

    drop(incomplete);
}

#[test]
fn every_api_get_requires_exact_bearer_authentication() {
    let token = b"0123456789abcdef0123456789abcdef";
    assert!(validate_api_auth(&request(&[]), token, "limit=64").is_err());
    assert!(validate_api_auth(
        &request(&[("authorization", "Bearer wrong")]),
        token,
        "limit=64"
    )
    .is_err());
    assert!(validate_api_auth(
        &request(&[("authorization", "Bearer 0123456789abcdef0123456789abcdef")]),
        token,
        "limit=64"
    )
    .is_ok());
}

#[test]
fn cookie_and_query_tokens_are_rejected() {
    let token = b"0123456789abcdef0123456789abcdef";
    let authorized = request(&[
        ("authorization", "Bearer 0123456789abcdef0123456789abcdef"),
        ("cookie", "token=0123456789abcdef0123456789abcdef"),
    ]);
    assert!(validate_api_auth(&authorized, token, "").is_err());
    let authorized = request(&[("authorization", "Bearer 0123456789abcdef0123456789abcdef")]);
    assert!(validate_api_auth(&authorized, token, "token=secret").is_err());
}

#[test]
fn query_contract_rejects_unknown_and_duplicate_fields() {
    assert!(validate_query("limit=64", &["limit"]).is_ok());
    assert!(validate_query("limit=64&limit=1", &["limit"]).is_err());
    assert!(validate_query("credential=secret", &["limit"]).is_err());
    assert!(split_target("/api/v1/tasks?%74oken=secret").is_err());
}

#[test]
fn host_and_origin_must_name_the_loopback_listener() {
    let address = "127.0.0.1:8765".parse().unwrap();
    assert!(validate_host_origin(
        &request(&[
            ("host", "localhost:8765"),
            ("origin", "http://localhost:8765")
        ]),
        address
    )
    .is_ok());
    assert!(validate_host_origin(
        &request(&[
            ("host", "attacker.example"),
            ("origin", "http://attacker.example")
        ]),
        address
    )
    .is_err());
    assert!(validate_host_origin(
        &request(&[
            ("host", "localhost:8765"),
            ("origin", "http://attacker.example")
        ]),
        address
    )
    .is_err());
}

#[test]
fn every_mutation_requires_an_idempotency_key() {
    let mut request = request(&[("content-type", "application/json")]);
    request.method = "POST".to_owned();
    assert!(require_json_mutation(&request).is_err());
    request.headers.insert(
        "idempotency-key".to_owned(),
        "one-browser-action".to_owned(),
    );
    assert!(require_json_mutation(&request).is_ok());
}

#[test]
fn non_loopback_cli_bind_is_visible_to_admission() {
    let parsed = Cli::try_parse_from([
        "cosh-gateway",
        "web",
        "--bind",
        "0.0.0.0:8765",
        "--token-file",
        "/tmp/token",
        "--workspace",
        "/tmp",
    ])
    .unwrap();
    let Command::Web(args) = parsed.command else {
        panic!("web command must parse")
    };
    assert!(validate_bind(args.bind).is_err());
}

#[test]
fn browser_uses_task_bound_cancel_retry_and_fresh_keys() {
    assert!(assets::APP_JS.contains("/cancel`"));
    assert!(assets::APP_JS.contains("/retry`"));
    assert!(assets::APP_JS.contains("crypto.randomUUID()"));
    assert!(assets::APP_JS.contains("expected_revision: task.revision"));
}

#[test]
fn browser_ignores_stale_selection_responses_and_polls_single_flight() {
    let script = assets::APP_JS;
    assert!(script.contains("const generation = ++selectionGeneration"));
    assert!(script.contains("if (!current(state)) return;"));
    assert!(script.contains("if (!current(state) || state.polling) return;"));
    assert!(script.contains("state.cursor = page.next_revision"));
    assert!(!script.contains("setInterval("));
}

#[test]
fn browser_only_offers_controls_for_current_pending_interactions() {
    let script = assets::APP_JS;
    assert!(script.contains("state.pendingApprovals.delete(event.approval_id)"));
    assert!(script.contains("state.pendingInputs.delete(event.request_id)"));
    assert!(script.contains("state.pendingApprovals.has(event.approval.approval_id)"));
    assert!(script.contains("state.pendingInputs.has(event.request.request_id)"));
    assert!(script.contains("while (hasMore && current(state))"));
    assert!(script.contains("new Map()"));
    assert!(script.contains("event.approval.run_id"));
    assert!(script.contains("event.request.run_id"));
    assert!(script.contains("clearRunInteractions(state, event.run_id)"));
    assert!(script.contains("clearRunInteractions(state, event.previous_run_id)"));
    assert!(script.contains("selectTask(task.task_id).catch(showError)"));
}

#[test]
fn cancel_and_retry_routes_enforce_mutation_contract_before_io() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let peer = std::thread::spawn(move || TcpStream::connect(address).unwrap());
    let (mut stream, _) = listener.accept().unwrap();
    let _peer = peer.join().unwrap();
    let client = LocalGatewayClient::new(PathBuf::from("/not-used"));
    for suffix in ["cancel", "retry"] {
        let target = format!("/api/v1/tasks/tsk_00000000-0000-0000-0000-000000000000/{suffix}");
        let request = HttpRequest {
            method: "POST".to_owned(),
            target: target.clone(),
            headers: [("content-type".to_owned(), "application/json".to_owned())]
                .into_iter()
                .collect(),
            body: b"{}".to_vec(),
        };
        let error = route_api(&mut stream, &request, &target, "", &client).unwrap_err();
        assert!(error.contains("Idempotency-Key"));
    }
}
