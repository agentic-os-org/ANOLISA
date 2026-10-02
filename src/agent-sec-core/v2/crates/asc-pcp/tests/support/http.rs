use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use asc_agentsight_client::{AgentSightHttpMethod, AgentSightHttpRequest, AgentSightHttpResponse};

pub struct Exchange {
    pub request: AgentSightHttpRequest,
    /// None closes the connection without confirming the remote outcome.
    pub response: Option<AgentSightHttpResponse>,
}

pub struct MockHttp {
    pub base_url: String,
    observed: Arc<Mutex<Vec<AgentSightHttpRequest>>>,
    worker: JoinHandle<()>,
}

impl MockHttp {
    pub fn start(
        script: Vec<Exchange>,
        inspect: impl Fn(&AgentSightHttpRequest) + Send + 'static,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base_url = format!("http://{}/api", listener.local_addr().unwrap());
        let observed = Arc::new(Mutex::new(vec![]));
        let captured = observed.clone();
        let worker = thread::spawn(move || {
            for expected in script {
                let deadline = Instant::now() + Duration::from_secs(5);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error)
                            if error.kind() == std::io::ErrorKind::WouldBlock
                                && Instant::now() < deadline =>
                        {
                            // Polling only bounds mock-server lifetime, not a race assertion.
                            thread::sleep(Duration::from_millis(1));
                        }
                        other => panic!("missing expected HTTP request: {other:?}"),
                    }
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let request = read_request(&mut stream);
                assert_eq!(request, expected.request);
                inspect(&request);
                captured.lock().unwrap().push(request);
                if let Some(response) = expected.response {
                    write!(stream, "HTTP/1.1 {} Mock\r\nContent-Length: {}\r\nConnection: close\r\nContent-Type: application/json\r\n\r\n", response.status, response.body.len()).unwrap();
                    stream.write_all(&response.body).unwrap();
                    stream.flush().unwrap();
                }
            }
        });
        Self {
            base_url,
            observed,
            worker,
        }
    }

    pub fn finish(self, expected_count: usize) {
        self.worker.join().unwrap();
        assert_eq!(self.observed.lock().unwrap().len(), expected_count);
    }
}

fn read_request(stream: &mut TcpStream) -> AgentSightHttpRequest {
    let mut reader = BufReader::new(stream);
    let mut first = String::new();
    reader.read_line(&mut first).unwrap();
    let fields: Vec<_> = first.split_whitespace().collect();
    assert_eq!(fields.len(), 3);
    let method = match fields[0] {
        "GET" => AgentSightHttpMethod::Get,
        "POST" => AgentSightHttpMethod::Post,
        "DELETE" => AgentSightHttpMethod::Delete,
        _ => panic!("unexpected HTTP method"),
    };
    let path = fields[1].strip_prefix("/api").unwrap().to_owned();
    let mut length = 0;
    let mut authorized = false;
    loop {
        let mut line = String::new();
        assert!(reader.read_line(&mut line).unwrap() > 0);
        if line == "\r\n" {
            break;
        }
        let (name, value) = line.trim().split_once(':').unwrap();
        if name.eq_ignore_ascii_case("content-length") {
            length = value.trim().parse::<usize>().unwrap();
        }
        if name.eq_ignore_ascii_case("authorization") {
            authorized = value.trim() == "Bearer test-local-only";
        }
    }
    assert!(
        authorized,
        "real transport must supply configured credential"
    );
    assert!(length <= 2 * 1024 * 1024);
    let body = if method == AgentSightHttpMethod::Post {
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        Some(body)
    } else {
        assert_eq!(length, 0);
        None
    };
    AgentSightHttpRequest { method, path, body }
}
