use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::thread;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

mod common;

fn binary_path() -> std::path::PathBuf {
    let mut path = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    path.push("cosh-core");
    path
}

fn read_request(stream: &TcpStream) -> (String, String) {
    let cloned = stream.try_clone().unwrap();
    let mut reader = BufReader::new(cloned);
    let mut request_line = String::new();
    reader.read_line(&mut request_line).unwrap();
    let mut content_length = 0;
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).unwrap();
        if header.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().unwrap();
            }
        }
    }
    let mut body = vec![0; content_length];
    reader.read_exact(&mut body).unwrap();
    (
        request_line.split_whitespace().nth(1).unwrap().to_string(),
        String::from_utf8(body).unwrap(),
    )
}

fn respond(stream: &mut TcpStream, status: &str, headers: &[(&str, String)], body: &str) {
    write!(stream, "HTTP/1.1 {status}\r\nConnection: close\r\n").unwrap();
    for (name, value) in headers {
        write!(stream, "{name}: {value}\r\n").unwrap();
    }
    write!(stream, "Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
}

fn get(url: &reqwest::Url) -> String {
    let address = format!(
        "{}:{}",
        url.host_str().unwrap(),
        url.port_or_known_default().unwrap()
    );
    let mut stream = TcpStream::connect(address).unwrap();
    let path = match url.query() {
        Some(query) => format!("{}?{query}", url.path()),
        None => url.path().to_string(),
    };
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut response = String::new();
    BufReader::new(stream)
        .read_to_string(&mut response)
        .unwrap();
    response
}

#[test]
fn mcp_login_completes_discovery_registration_and_callback() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        for _ in 0..8 {
            let (mut stream, _) = listener.accept().unwrap();
            let (target, body) = read_request(&stream);
            match target.split('?').next().unwrap() {
                "/mcp" => {
                    let initialize: serde_json::Value = serde_json::from_str(&body).unwrap();
                    assert_eq!(initialize["method"], "initialize");
                    assert!(initialize["params"]["protocolVersion"].is_string());
                    assert!(initialize["params"]["capabilities"].is_object());
                    assert!(initialize["params"]["clientInfo"]["name"].is_string());
                    respond(
                        &mut stream,
                        "401 Unauthorized",
                        &[(
                            "WWW-Authenticate",
                            "Bearer scope=\"tools.challenge\"".to_string(),
                        )],
                        "",
                    );
                }
                "/.well-known/oauth-protected-resource/mcp" => {
                    respond(&mut stream, "404 Not Found", &[], "")
                }
                "/.well-known/oauth-protected-resource" => respond(
                    &mut stream,
                    "200 OK",
                    &[("Content-Type", "application/json".to_string())],
                    &format!(
                        r#"{{"resource":"http://{address}/mcp","authorization_servers":["http://{address}"],"scopes_supported":["tools.read"]}}"#
                    ),
                ),
                "/.well-known/oauth-authorization-server" => {
                    respond(&mut stream, "404 Not Found", &[], "")
                }
                "/.well-known/openid-configuration" => respond(
                    &mut stream,
                    "200 OK",
                    &[("Content-Type", "application/json".to_string())],
                    &format!(
                        r#"{{"issuer":"http://{address}","authorization_endpoint":"http://{address}/authorize","token_endpoint":"http://{address}/token","registration_endpoint":"http://{address}/register","code_challenge_methods_supported":["S256"]}}"#
                    ),
                ),
                "/register" => respond(
                    &mut stream,
                    "201 Created",
                    &[("Content-Type", "application/json".to_string())],
                    r#"{"client_id":"test-client"}"#,
                ),
                "/authorize" => {
                    let request =
                        reqwest::Url::parse(&format!("http://{address}{target}")).unwrap();
                    let values: std::collections::HashMap<_, _> =
                        request.query_pairs().into_owned().collect();
                    assert_eq!(
                        values.get("code_challenge_method"),
                        Some(&"S256".to_string())
                    );
                    assert_eq!(
                        values.get("resource"),
                        Some(&format!("http://{address}/mcp"))
                    );
                    assert_eq!(values.get("scope"), Some(&"tools.challenge".to_string()));
                    let mut callback =
                        reqwest::Url::parse(values.get("redirect_uri").unwrap()).unwrap();
                    callback
                        .query_pairs_mut()
                        .append_pair("code", "test-code")
                        .append_pair("state", values.get("state").unwrap());
                    respond(
                        &mut stream,
                        "302 Found",
                        &[("Location", callback.to_string())],
                        "",
                    );
                }
                "/token" => respond(
                    &mut stream,
                    "200 OK",
                    &[("Content-Type", "application/json".to_string())],
                    r#"{"access_token":"test-access-token","refresh_token":"test-refresh-token","expires_in":3600}"#,
                ),
                unexpected => panic!("unexpected OAuth request path: {unexpected}"),
            }
        }
    });

    let home = tempfile::tempdir().unwrap();
    let config_dir = home.path().join(".copilot-shell");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.toml"),
        "[mcp.servers.remote]\nurl = \"${MCP_OAUTH_TEST_URL}\"\n",
    )
    .unwrap();
    let mut command = Command::new(binary_path());
    command
        .args(["mcp", "login", "remote"])
        .env("HOME", home.path())
        .env("MCP_OAUTH_TEST_URL", format!("http://{address}/mcp"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    common::opt_out_telemetry(&mut command, home.path());
    let mut child = command.spawn().unwrap();
    let stderr = child.stderr.take().unwrap();
    let mut stderr = BufReader::new(stderr);
    let mut authorization_url = None;
    for _ in 0..20 {
        let mut line = String::new();
        stderr.read_line(&mut line).unwrap();
        if line.starts_with("http://") {
            authorization_url = Some(reqwest::Url::parse(line.trim()).unwrap());
            break;
        }
    }
    let authorization_url = authorization_url.expect("OAuth authorization URL");
    let authorization_response = get(&authorization_url);
    let location = authorization_response
        .lines()
        .find_map(|line| line.strip_prefix("Location: "))
        .expect("authorization redirect");
    let callback_response = get(&reqwest::Url::parse(location).unwrap());
    assert!(callback_response.starts_with("HTTP/1.1 200"));

    assert!(child.wait().unwrap().success());
    let mut remaining_stderr = String::new();
    stderr.read_to_string(&mut remaining_stderr).unwrap();
    assert!(!remaining_stderr.contains("test-access-token"));
    assert!(!remaining_stderr.contains("test-refresh-token"));
    server.join().unwrap();

    let credentials = std::fs::read_to_string(config_dir.join("mcp-oauth.json")).unwrap();
    assert!(credentials.contains("test-access-token"));
    assert!(credentials.contains(&format!(r#""endpoint":"http://{address}/mcp""#)));
    assert!(!std::fs::read_to_string(config_dir.join("config.toml"))
        .unwrap()
        .contains("test-access-token"));
    #[cfg(unix)]
    assert_eq!(
        std::fs::metadata(config_dir.join("mcp-oauth.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn mcp_inspect_does_not_reuse_credentials_after_endpoint_changes() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let cloned = stream.try_clone().unwrap();
        let mut reader = BufReader::new(cloned);
        let mut request_line = String::new();
        reader.read_line(&mut request_line).unwrap();
        assert!(request_line.starts_with("POST /mcp "));
        let mut authorization = None;
        loop {
            let mut header = String::new();
            reader.read_line(&mut header).unwrap();
            if header == "\r\n" {
                break;
            }
            if let Some((name, value)) = header.split_once(':') {
                if name.eq_ignore_ascii_case("authorization") {
                    authorization = Some(value.trim().to_string());
                }
            }
        }
        assert!(authorization.is_none(), "stale credentials were sent");
        respond(&mut stream, "401 Unauthorized", &[], "");
    });

    let home = tempfile::tempdir().unwrap();
    let config_dir = home.path().join(".copilot-shell");
    std::fs::create_dir_all(&config_dir).unwrap();
    let endpoint = format!("http://{address}/mcp");
    std::fs::write(
        config_dir.join("config.toml"),
        format!("[mcp.servers.remote]\nurl = \"{endpoint}\"\n"),
    )
    .unwrap();
    std::fs::write(
        config_dir.join("mcp-oauth.json"),
        format!(
            r#"{{"servers":{{"remote":{{"access_token":"stale-access-token","refresh_token":"stale-refresh-token","token_endpoint":"http://127.0.0.1:1/token","client_id":"client","client_secret":null,"resource":"{endpoint}","endpoint":"http://127.0.0.1:1/old-mcp","expires_at":null}}}}}}"#
        ),
    )
    .unwrap();

    let mut command = Command::new(binary_path());
    command
        .args(["mcp", "inspect", "remote"])
        .env("HOME", home.path());
    common::opt_out_telemetry(&mut command, home.path());
    let output = command.output().unwrap();

    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("stale-access-token"));
    server.join().unwrap();
}
