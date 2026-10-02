//! Loopback-only presentation adapter for durable local Gateway Tasks.

mod assets;
mod security;

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::Mutex;
use std::time::Duration;

use serde::Deserialize;

use super::*;
use security::{attest_gateway, canonical_workspace, read_token, validate_token_scope};

const MAX_HTTP_HEAD_BYTES: usize = 16 * 1024;
const MAX_HTTP_BODY_BYTES: usize = 64 * 1024;
const MAX_TOKEN_BYTES: usize = 256;
const HTTP_REQUEST_DEADLINE: Duration = Duration::from_secs(2);
const HTTP_WORKERS: usize = 4;
const HTTP_QUEUE_CAPACITY: usize = 16;

#[derive(Debug, Clone, Args)]
pub(super) struct WebArgs {
    /// Loopback address for the local browser beta.
    #[arg(long, default_value = "127.0.0.1:8765")]
    bind: SocketAddr,
    /// Absolute Unix socket path; defaults below the user runtime directory.
    #[arg(long, value_name = "PATH")]
    socket: Option<PathBuf>,
    /// Canonical workspace admitted by the paired Gateway daemon.
    #[arg(long, value_name = "PATH")]
    workspace: PathBuf,
    /// Absolute 0600 root- or current-user-owned Bearer token file outside the admitted workspace.
    #[arg(long, value_name = "PATH")]
    token_file: PathBuf,
    /// Presentation format for startup and stable errors.
    #[arg(long, value_enum, default_value_t = Output::Human)]
    pub(super) output: Output,
}

pub(super) fn web(args: WebArgs, reporter: &Reporter) -> Result<u8, CliError> {
    validate_bind(args.bind)?;
    let workspace = canonical_workspace(&args.workspace)?;
    let token = read_token(&args.token_file)?;
    validate_token_scope(&token.path, &workspace)?;
    let socket = daemon_socket_path(args.socket.as_ref())?;
    attest_gateway(&LocalGatewayClient::new(socket.clone()), &workspace)?;
    let listener =
        TcpListener::bind(args.bind).map_err(|error| CliError::Web(error.to_string()))?;
    listener
        .set_nonblocking(true)
        .map_err(|error| CliError::Web(error.to_string()))?;
    let address = listener
        .local_addr()
        .map_err(|error| CliError::Web(error.to_string()))?;
    let workers = HttpWorkers::new(address, token.bytes, socket)?;
    let interrupted = install_interrupt_handler()?;
    reporter.event(
        "web_ready",
        json!({
            "url": format!("http://{address}/"),
        }),
    )?;
    while !interrupted.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, peer)) => {
                if !peer.ip().is_loopback() {
                    continue;
                }
                workers.submit(stream)?;
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(error) => return Err(CliError::Web(error.to_string())),
        }
    }
    Ok(0)
}

struct HttpWorkers {
    sender: Option<SyncSender<TcpStream>>,
    handles: Vec<std::thread::JoinHandle<()>>,
}

impl HttpWorkers {
    fn new(address: SocketAddr, token: Vec<u8>, socket: PathBuf) -> Result<Self, CliError> {
        let (sender, receiver) = sync_channel::<TcpStream>(HTTP_QUEUE_CAPACITY);
        let receiver = Arc::new(Mutex::new(receiver));
        let token = Arc::new(token);
        let socket = Arc::new(socket);
        let mut handles = Vec::with_capacity(HTTP_WORKERS);
        for index in 0..HTTP_WORKERS {
            let receiver = Arc::clone(&receiver);
            let token = Arc::clone(&token);
            let socket = Arc::clone(&socket);
            handles.push(
                std::thread::Builder::new()
                    .name(format!("cosh-web-{index}"))
                    .spawn(move || worker_loop(&receiver, address, &token, &socket))
                    .map_err(|error| CliError::Web(error.to_string()))?,
            );
        }
        Ok(Self {
            sender: Some(sender),
            handles,
        })
    }

    fn submit(&self, stream: TcpStream) -> Result<(), CliError> {
        let Some(sender) = &self.sender else {
            return Err(CliError::Web("Web worker pool is closed".to_owned()));
        };
        match sender.try_send(stream) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_stream)) => Ok(()),
            Err(TrySendError::Disconnected(_)) => {
                Err(CliError::Web("Web worker pool stopped".to_owned()))
            }
        }
    }
}

impl Drop for HttpWorkers {
    fn drop(&mut self) {
        self.sender.take();
        for handle in self.handles.drain(..) {
            let _ = handle.join();
        }
    }
}

fn worker_loop(
    receiver: &Mutex<Receiver<TcpStream>>,
    address: SocketAddr,
    token: &[u8],
    socket: &Path,
) {
    loop {
        let stream = receiver
            .lock()
            .ok()
            .and_then(|receiver| receiver.recv().ok());
        let Some(mut stream) = stream else {
            return;
        };
        if stream
            .set_write_timeout(Some(HTTP_REQUEST_DEADLINE))
            .is_err()
        {
            continue;
        }
        if let Err(error) = handle_connection(&mut stream, address, token, socket) {
            let _ = write_error(&mut stream, 400, &error);
        }
    }
}

fn validate_bind(address: SocketAddr) -> Result<(), CliError> {
    if address.ip().is_loopback() {
        Ok(())
    } else {
        Err(CliError::Web(
            "Web beta binds only to an IPv4 or IPv6 loopback address".to_owned(),
        ))
    }
}

#[derive(Debug)]
struct HttpRequest {
    method: String,
    target: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

fn handle_connection(
    stream: &mut TcpStream,
    address: SocketAddr,
    token: &[u8],
    socket: &Path,
) -> Result<(), String> {
    let request = read_request(stream)?;
    validate_host_origin(&request, address)?;
    let (path, query) = split_target(&request.target)?;
    if path.starts_with("/api/") {
        validate_api_auth(&request, token, query)?;
        let client = LocalGatewayClient::new(socket.to_path_buf());
        return route_api(stream, &request, path, query, &client);
    }
    if request.method != "GET" {
        return write_json(
            stream,
            405,
            &json!({"ok": false, "error": "method not allowed"}),
        );
    }
    match path {
        "/" => write_response(stream, 200, "text/html; charset=utf-8", assets::INDEX_HTML),
        "/app.js" => write_response(
            stream,
            200,
            "text/javascript; charset=utf-8",
            assets::APP_JS,
        ),
        "/app.css" => write_response(stream, 200, "text/css; charset=utf-8", assets::APP_CSS),
        _ => write_json(stream, 404, &json!({"ok": false, "error": "not found"})),
    }
}

fn read_request(stream: &mut TcpStream) -> Result<HttpRequest, String> {
    read_request_with_deadline(stream, HTTP_REQUEST_DEADLINE)
}

fn read_request_with_deadline(
    stream: &mut TcpStream,
    duration: Duration,
) -> Result<HttpRequest, String> {
    let deadline = std::time::Instant::now() + duration;
    let mut bytes = Vec::new();
    let head_end = loop {
        if bytes.len() >= MAX_HTTP_HEAD_BYTES {
            return Err("HTTP header exceeds limit".to_owned());
        }
        let mut chunk = [0_u8; 1024];
        let count = read_before(stream, &mut chunk, deadline)?;
        if count == 0 {
            return Err("HTTP request ended before headers".to_owned());
        }
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(offset) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break offset + 4;
        }
    };
    let head = std::str::from_utf8(&bytes[..head_end]).map_err(|_| "HTTP header is not UTF-8")?;
    let mut lines = head[..head.len() - 4].split("\r\n");
    let mut request_line = lines.next().ok_or("missing HTTP request line")?.split(' ');
    let method = request_line.next().ok_or("missing HTTP method")?.to_owned();
    let target = request_line.next().ok_or("missing HTTP target")?.to_owned();
    if request_line.next() != Some("HTTP/1.1") || request_line.next().is_some() {
        return Err("only HTTP/1.1 origin-form requests are supported".to_owned());
    }
    let mut headers = BTreeMap::new();
    for line in lines {
        let (name, value) = line.split_once(':').ok_or("malformed HTTP header")?;
        let name = name.trim().to_ascii_lowercase();
        if name.is_empty() || headers.insert(name, value.trim().to_owned()).is_some() {
            return Err("duplicate or empty HTTP header".to_owned());
        }
    }
    if headers.contains_key("transfer-encoding") {
        return Err("transfer-encoded requests are not supported".to_owned());
    }
    let content_length = headers
        .get("content-length")
        .map(|value| value.parse::<usize>().map_err(|_| "invalid Content-Length"))
        .transpose()?
        .unwrap_or(0);
    if content_length > MAX_HTTP_BODY_BYTES {
        return Err("HTTP body exceeds limit".to_owned());
    }
    while bytes.len().saturating_sub(head_end) < content_length {
        let mut chunk = [0_u8; 4096];
        let count = read_before(stream, &mut chunk, deadline)?;
        if count == 0 {
            return Err("HTTP body ended early".to_owned());
        }
        bytes.extend_from_slice(&chunk[..count]);
        if bytes.len().saturating_sub(head_end) > MAX_HTTP_BODY_BYTES {
            return Err("HTTP body exceeds limit".to_owned());
        }
    }
    Ok(HttpRequest {
        method,
        target,
        headers,
        body: bytes[head_end..head_end + content_length].to_vec(),
    })
}

fn read_before(
    stream: &mut TcpStream,
    buffer: &mut [u8],
    deadline: std::time::Instant,
) -> Result<usize, String> {
    let remaining = deadline
        .checked_duration_since(std::time::Instant::now())
        .ok_or("HTTP request exceeded its absolute deadline")?;
    stream
        .set_read_timeout(Some(remaining))
        .map_err(|error| error.to_string())?;
    stream.read(buffer).map_err(|error| {
        if matches!(
            error.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
        ) {
            "HTTP request exceeded its absolute deadline".to_owned()
        } else {
            error.to_string()
        }
    })
}

fn validate_host_origin(request: &HttpRequest, address: SocketAddr) -> Result<(), String> {
    let host = request
        .headers
        .get("host")
        .ok_or("Host header is required")?;
    let port = address.port();
    let allowed = [
        address.to_string(),
        format!("127.0.0.1:{port}"),
        format!("[::1]:{port}"),
        format!("localhost:{port}"),
    ];
    if !allowed.iter().any(|candidate| candidate == host) {
        return Err("Host is not the configured loopback listener".to_owned());
    }
    if let Some(origin) = request.headers.get("origin") {
        if origin != &format!("http://{host}") {
            return Err("Origin does not match the loopback Host".to_owned());
        }
    }
    Ok(())
}

fn validate_api_auth(request: &HttpRequest, token: &[u8], query: &str) -> Result<(), String> {
    if request.headers.contains_key("cookie")
        || query
            .split('&')
            .filter_map(|part| part.split_once('='))
            .any(|(name, _)| {
                matches!(
                    name.to_ascii_lowercase().as_str(),
                    "token" | "access_token" | "bearer"
                )
            })
    {
        return Err("tokens in cookies or query parameters are rejected".to_owned());
    }
    let authorization = request
        .headers
        .get("authorization")
        .ok_or("Bearer authorization is required")?;
    let supplied = authorization
        .strip_prefix("Bearer ")
        .ok_or("Bearer authorization is required")?
        .as_bytes();
    if !constant_time_eq(supplied, token) {
        return Err("Bearer authorization failed".to_owned());
    }
    Ok(())
}

fn route_api(
    stream: &mut TcpStream,
    request: &HttpRequest,
    path: &str,
    query: &str,
    client: &LocalGatewayClient,
) -> Result<(), String> {
    let segments = path.trim_matches('/').split('/').collect::<Vec<_>>();
    let result = match (request.method.as_str(), segments.as_slice()) {
        ("GET", ["api", "v1", "tasks"]) => {
            validate_query(query, &["limit"])?;
            client.list(RequestId::new(), query_u16(query, "limit", 64, 1, 64)?)
        }
        ("GET", ["api", "v1", "tasks", task]) => {
            validate_query(query, &[])?;
            client.get(RequestId::new(), parse_task_id(task)?)
        }
        ("GET", ["api", "v1", "tasks", task, "events"]) => {
            validate_query(query, &["after", "limit"])?;
            client.events(
                RequestId::new(),
                parse_task_id(task)?,
                query_u64(query, "after")?,
                query_u16(query, "limit", 64, 1, 64)?,
            )
        }
        ("POST", ["api", "v1", "tasks", task, "approvals", approval]) => {
            validate_query(query, &[])?;
            require_json_mutation(request)?;
            let body: ApprovalBody = decode_body(request)?;
            client.resolve_approval_for_task(cosh_gateway::daemon::ResolveApprovalForTask {
                request_id: RequestId::new(),
                idempotency_key: mutation_key(request)?,
                task_id: parse_task_id(task)?,
                approval_id: ApprovalId::parse(approval).map_err(|error| error.to_string())?,
                decision: body.decision,
            })
        }
        ("POST", ["api", "v1", "tasks", task, "inputs", input]) => {
            validate_query(query, &[])?;
            require_json_mutation(request)?;
            let body: InputBody = decode_body(request)?;
            client.append_input(AppendTaskInput {
                request_id: RequestId::new(),
                idempotency_key: mutation_key(request)?,
                task_id: parse_task_id(task)?,
                input_request_id: InputRequestId::parse(input)
                    .map_err(|error| error.to_string())?,
                response: RuntimeInputResponse::Text {
                    text: BoundedText::new(body.text).map_err(|error| error.to_string())?,
                },
                expected_revision: body.expected_revision,
            })
        }
        ("POST", ["api", "v1", "tasks", task, "cancel"]) => {
            validate_query(query, &[])?;
            require_json_mutation(request)?;
            let body: CancelBody = decode_body(request)?;
            client.cancel(CancelTask {
                request_id: RequestId::new(),
                idempotency_key: mutation_key(request)?,
                task_id: parse_task_id(task)?,
                run_id: RunId::parse(&body.run_id).map_err(|error| error.to_string())?,
                expected_revision: body.expected_revision,
            })
        }
        ("POST", ["api", "v1", "tasks", task, "retry"]) => {
            validate_query(query, &[])?;
            require_json_mutation(request)?;
            let body: RetryBody = decode_body(request)?;
            client.retry(RetryTask {
                request_id: RequestId::new(),
                idempotency_key: mutation_key(request)?,
                task_id: parse_task_id(task)?,
                previous_run_id: RunId::parse(&body.previous_run_id)
                    .map_err(|error| error.to_string())?,
                expected_revision: body.expected_revision,
            })
        }
        _ => return write_json(stream, 404, &json!({"ok": false, "error": "not found"})),
    };
    match result {
        Ok(result) => write_json(stream, 200, &json!({"ok": true, "data": result})),
        Err(error) => write_json(
            stream,
            409,
            &json!({"ok": false, "error": error.to_string()}),
        ),
    }
}

fn require_json_mutation(request: &HttpRequest) -> Result<(), String> {
    if request.headers.get("content-type").map(String::as_str) != Some("application/json") {
        return Err("mutations require Content-Type application/json".to_owned());
    }
    mutation_key(request).map(|_| ())
}

fn mutation_key(request: &HttpRequest) -> Result<IdempotencyKey, String> {
    let value = request
        .headers
        .get("idempotency-key")
        .ok_or("mutations require Idempotency-Key")?;
    IdempotencyKey::new(value.clone()).map_err(|error| error.to_string())
}

fn decode_body<T: for<'de> Deserialize<'de>>(request: &HttpRequest) -> Result<T, String> {
    serde_json::from_slice(&request.body).map_err(|error| format!("invalid JSON body: {error}"))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApprovalBody {
    decision: ApprovalDecision,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InputBody {
    text: String,
    expected_revision: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CancelBody {
    run_id: String,
    expected_revision: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RetryBody {
    previous_run_id: String,
    expected_revision: Option<u64>,
}

fn split_target(target: &str) -> Result<(&str, &str), String> {
    if !target.starts_with('/') || target.contains(['#', '%']) || target.contains("//") {
        return Err("HTTP target must use origin form".to_owned());
    }
    Ok(target.split_once('?').unwrap_or((target, "")))
}

fn parse_task_id(value: &str) -> Result<TaskId, String> {
    TaskId::parse(value).map_err(|error| error.to_string())
}

fn query_u16(query: &str, name: &str, default: u16, min: u16, max: u16) -> Result<u16, String> {
    let value = query_value(query, name)
        .map(|value| value.parse::<u16>().map_err(|_| format!("invalid {name}")))
        .transpose()?
        .unwrap_or(default);
    if value < min || value > max {
        return Err(format!("{name} is outside its bounded range"));
    }
    Ok(value)
}

fn query_u64(query: &str, name: &str) -> Result<Option<u64>, String> {
    query_value(query, name)
        .map(|value| value.parse::<u64>().map_err(|_| format!("invalid {name}")))
        .transpose()
}

fn query_value<'a>(query: &'a str, name: &str) -> Option<&'a str> {
    query
        .split('&')
        .filter_map(|part| part.split_once('='))
        .find_map(|(candidate, value)| (candidate == name).then_some(value))
}

fn validate_query(query: &str, allowed: &[&str]) -> Result<(), String> {
    let mut seen = std::collections::BTreeSet::new();
    for part in query.split('&').filter(|part| !part.is_empty()) {
        let (name, _) = part
            .split_once('=')
            .ok_or("query parameters require an explicit value")?;
        if !allowed.contains(&name) || !seen.insert(name) {
            return Err("unknown or duplicate query parameter".to_owned());
        }
    }
    Ok(())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    for index in 0..left.len().max(right.len()) {
        difference |= usize::from(
            left.get(index).copied().unwrap_or_default()
                ^ right.get(index).copied().unwrap_or_default(),
        );
    }
    difference == 0
}

fn write_error(stream: &mut TcpStream, status: u16, message: &str) -> Result<(), String> {
    write_json(stream, status, &json!({"ok": false, "error": message}))
}

fn write_json(stream: &mut TcpStream, status: u16, value: &Value) -> Result<(), String> {
    let body = serde_json::to_string(value).map_err(|error| error.to_string())?;
    write_response(stream, status, "application/json; charset=utf-8", &body)
}

fn write_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &str,
) -> Result<(), String> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        503 => "Service Unavailable",
        _ => "Error",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nContent-Security-Policy: default-src 'self'; connect-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests;
