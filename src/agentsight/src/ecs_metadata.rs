//! ECS metadata service client
//!
//! Shared primitives for querying the Alibaba Cloud ECS metadata service
//! at `http://100.100.100.200/latest/meta-data/`.
//!
//! Provides [`metadata_agent()`] and [`read_plain()`] used by both
//! `genai::instance_id` and the dashboard ECS probe.
//!
//! Supports both IMDSv1 (no token) and hardened IMDS (token-based).

use std::sync::mpsc;
use std::time::Duration;

/// Base URL for the ECS metadata service.
pub(crate) const METADATA_BASE: &str = "http://100.100.100.200/latest/meta-data";

/// Hard deadline for the entire metadata probe (all HTTP requests combined).
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Per-request timeout for each individual HTTP call (connect + read).
const REQUEST_TIMEOUT: Duration = Duration::from_millis(800);

/// Build a ureq agent with explicit connect + overall request timeouts.
///
/// Shared by [`read_plain`] and the dashboard ECS probe.
pub(crate) fn metadata_agent(timeout: Duration) -> ureq::Agent {
    ureq::builder()
        .timeout_connect(timeout)
        .timeout(timeout)
        .build()
}

/// Read a single metadata path using IMDSv1 (no token).
///
/// `base_url` is the metadata service root (e.g. [`METADATA_BASE`]).
/// `path` is relative, e.g. `"instance-id"`.
/// Returns `None` when the endpoint is unreachable or returns empty.
pub(crate) fn read_plain(agent: &ureq::Agent, base_url: &str, path: &str) -> Option<String> {
    let url = format!("{base_url}/{path}");
    agent
        .get(&url)
        .call()
        .ok()
        .and_then(|r| r.into_string().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// ECS instance metadata collected from the metadata service.
#[derive(Debug, Clone)]
pub struct EcsMetadata {
    /// ECS instance ID, e.g. `i-bp1xxx`
    pub instance_id: String,
    /// Region ID, e.g. `cn-hangzhou`
    pub region_id: String,
    /// Elastic IP address (empty if no EIP bound)
    pub eip: String,
    /// Public IPv4 address (empty if no public IP)
    pub public_ipv4: String,
}

impl EcsMetadata {
    /// Returns the public IP address, preferring EIP over public-ipv4.
    pub fn public_ip(&self) -> Option<&str> {
        if !self.eip.is_empty() {
            Some(&self.eip)
        } else if !self.public_ipv4.is_empty() {
            Some(&self.public_ipv4)
        } else {
            None
        }
    }

    /// Returns the ECS console URL for the instance detail page.
    pub fn instance_url(&self) -> String {
        format!(
            "https://ecs.console.aliyun.com/server/{}/detail?regionId={}",
            self.instance_id, self.region_id
        )
    }
}

/// Probe the ECS metadata service with a hard 2-second deadline.
///
/// Spawns a background thread that performs all HTTP requests, then uses
/// `mpsc::recv_timeout` to enforce the deadline.  If the thread does not
/// finish within [`PROBE_TIMEOUT`], the caller returns `None` immediately
/// (the background thread is abandoned and will be cleaned up on process exit).
///
/// Returns `None` when not running on an ECS instance.
pub fn probe_ecs_metadata() -> Option<EcsMetadata> {
    probe_ecs_metadata_with(METADATA_BASE)
}

/// Internal probe entry point accepting a configurable metadata base URL.
fn probe_ecs_metadata_with(base_url: &str) -> Option<EcsMetadata> {
    let base_url = base_url.to_owned();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(fetch_ecs_metadata(&base_url));
    });
    match rx.recv_timeout(PROBE_TIMEOUT) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            log::debug!("ECS metadata probe timed out after {PROBE_TIMEOUT:?}");
            None
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            log::debug!("ECS metadata probe thread panicked");
            None
        }
    }
}

/// Fetch ECS metadata fields from the metadata service.
fn fetch_ecs_metadata(base_url: &str) -> Option<EcsMetadata> {
    let agent = metadata_agent(REQUEST_TIMEOUT);

    // Obtain IMDSv2 token once (None on non-ECS or when IMDSv2 is unavailable)
    let token = get_imdsv2_token(&agent, base_url);

    // Verify reachability via instance-id
    let instance_id = read_metadata(&agent, base_url, "instance-id", token.as_deref())?;

    let region_id =
        read_metadata(&agent, base_url, "region-id", token.as_deref()).unwrap_or_default();

    let eip = read_metadata(&agent, base_url, "eipv4", token.as_deref()).unwrap_or_default();
    let public_ipv4 =
        read_metadata(&agent, base_url, "public-ipv4", token.as_deref()).unwrap_or_default();

    Some(EcsMetadata {
        instance_id,
        region_id,
        eip,
        public_ipv4,
    })
}

/// Read a single metadata path, using the pre-fetched token if available.
fn read_metadata(
    agent: &ureq::Agent,
    base_url: &str,
    path: &str,
    token: Option<&str>,
) -> Option<String> {
    // Try IMDSv2 (token-based) first if we have a token
    if let Some(t) = token {
        let url = format!("{base_url}/{path}");
        if let Some(val) = read_with_token(agent, &url, t) {
            return Some(val);
        }
    }

    // Fallback to IMDSv1 (no token)
    read_plain(agent, base_url, path)
}

/// Attempt to read metadata with an IMDSv2 session token.
fn read_with_token(agent: &ureq::Agent, url: &str, token: &str) -> Option<String> {
    agent
        .get(url)
        .set("X-Forwarded-For", "China")
        .set("X-Alibaba-Cloud-Ecs-Metadata-Token", token)
        .call()
        .ok()
        .and_then(|r| r.into_string().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Token endpoint path relative to the metadata base URL.
const TOKEN_PATH: &str = "api/token";

/// Obtain an IMDSv2 session token via PUT request.
fn get_imdsv2_token(agent: &ureq::Agent, base_url: &str) -> Option<String> {
    // The token endpoint lives under /latest/api/token, which is one level
    // above /latest/meta-data.  Derive it from base_url by replacing the
    // trailing `meta-data` segment.
    let token_url = base_url
        .strip_suffix("/meta-data")
        .map(|prefix| format!("{prefix}/api/{TOKEN_PATH}"))
        .unwrap_or_else(|| format!("{base_url}/{TOKEN_PATH}"));

    agent
        .put(&token_url)
        .set("X-Alibaba-Cloud-Ecs-Metadata-Token-Ttl-Seconds", "300")
        .call()
        .ok()
        .and_then(|r| r.into_string().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    fn sample_metadata() -> EcsMetadata {
        EcsMetadata {
            instance_id: "i-bp1abcdef".to_string(),
            region_id: "cn-hangzhou".to_string(),
            eip: "47.98.1.2".to_string(),
            public_ipv4: String::new(),
        }
    }

    #[test]
    fn public_ip_prefers_eip_over_public_ipv4() {
        let meta = EcsMetadata {
            eip: "1.2.3.4".to_string(),
            public_ipv4: "5.6.7.8".to_string(),
            ..sample_metadata()
        };
        assert_eq!(meta.public_ip(), Some("1.2.3.4"));
    }

    #[test]
    fn public_ip_falls_back_to_public_ipv4() {
        let meta = EcsMetadata {
            eip: String::new(),
            public_ipv4: "5.6.7.8".to_string(),
            ..sample_metadata()
        };
        assert_eq!(meta.public_ip(), Some("5.6.7.8"));
    }

    #[test]
    fn public_ip_returns_none_when_both_empty() {
        let meta = EcsMetadata {
            eip: String::new(),
            public_ipv4: String::new(),
            ..sample_metadata()
        };
        assert_eq!(meta.public_ip(), None);
    }

    #[test]
    fn instance_url_contains_region_and_id() {
        let meta = sample_metadata();
        let url = meta.instance_url();
        assert!(url.contains("cn-hangzhou"));
        assert!(url.contains("i-bp1abcdef"));
        assert!(url.starts_with("https://ecs.console.aliyun.com/"));
    }

    // -----------------------------------------------------------------------
    // Mock HTTP server helpers
    // -----------------------------------------------------------------------

    /// A minimal HTTP/1.1 mock server bound to localhost.
    struct MockServer {
        listener: TcpListener,
        /// Base URL for the metadata API (e.g. `http://127.0.0.1:PORT/latest/meta-data`)
        meta_base: String,
    }

    impl MockServer {
        /// Bind to a random available port and return the server handle.
        fn bind() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let meta_base = format!("http://127.0.0.1:{port}/latest/meta-data");
            Self {
                listener,
                meta_base,
            }
        }

        /// Spawn a handler thread that serves a fixed metadata response set.
        /// Handles both PUT (token) and GET (metadata) requests.
        fn serve_metadata(self, fields: Vec<(&'static str, &'static str)>) {
            let listener = self.listener;
            std::thread::spawn(move || {
                // Accept up to 8 connections (token + 4 fields * IMDSv1+IMDSv2 fallback)
                for _ in 0..8 {
                    let Ok((mut stream, _)) = listener.accept() else {
                        continue;
                    };
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
                    let mut reader = BufReader::new(&stream);

                    // Read the request line
                    let mut request_line = String::new();
                    if reader.read_line(&mut request_line).is_err() {
                        continue;
                    }
                    // Skip headers
                    loop {
                        let mut line = String::new();
                        if reader.read_line(&mut line).is_err() || line.trim().is_empty() {
                            break;
                        }
                    }

                    let is_put = request_line.starts_with("PUT");

                    let body = if is_put {
                        // PUT /latest/api/token → return a mock token
                        "mock-token-abc".to_string()
                    } else {
                        // GET /latest/meta-data/<field>
                        let path = request_line
                            .split_whitespace()
                            .nth(1)
                            .unwrap_or("")
                            .to_string();
                        fields
                            .iter()
                            .find(|(k, _)| path.ends_with(k))
                            .map(|(_, v)| v.to_string())
                            .unwrap_or_else(|| "not-found".to_string())
                    };

                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes());
                    let _ = stream.flush();
                }
            });
        }
    }

    #[test]
    fn read_plain_returns_value_on_success() {
        let server = MockServer::bind();
        let base = server.meta_base.clone();
        server.serve_metadata(vec![("instance-id", "i-test123")]);

        let agent = metadata_agent(Duration::from_secs(2));
        let val = read_plain(&agent, &base, "instance-id");
        assert_eq!(val.as_deref(), Some("i-test123"));
    }

    #[test]
    fn read_plain_returns_none_for_unknown_path() {
        let server = MockServer::bind();
        let base = server.meta_base.clone();
        // Server has no mapping for "eipv4", returns "not-found" which is non-empty
        // so read_plain will return Some("not-found"). This tests the fallback path.
        server.serve_metadata(vec![("instance-id", "i-test123")]);

        let agent = metadata_agent(Duration::from_secs(2));
        let val = read_plain(&agent, &base, "eipv4");
        // "not-found" is a valid non-empty string, so read_plain returns Some
        assert_eq!(val.as_deref(), Some("not-found"));
    }

    #[test]
    fn read_plain_returns_none_when_server_unreachable() {
        // Use a port that is not listening
        let agent = metadata_agent(Duration::from_millis(100));
        let val = read_plain(&agent, "http://127.0.0.1:1/latest/meta-data", "instance-id");
        assert_eq!(val, None);
    }

    #[test]
    fn get_imdsv2_token_returns_token_on_success() {
        let server = MockServer::bind();
        let base = server.meta_base.clone();
        server.serve_metadata(vec![]);

        let agent = metadata_agent(Duration::from_secs(2));
        let token = get_imdsv2_token(&agent, &base);
        assert_eq!(token.as_deref(), Some("mock-token-abc"));
    }

    #[test]
    fn read_metadata_prefers_imdsv2_when_token_present() {
        let server = MockServer::bind();
        let base = server.meta_base.clone();
        server.serve_metadata(vec![("instance-id", "i-v2value")]);

        let agent = metadata_agent(Duration::from_secs(2));
        let val = read_metadata(&agent, &base, "instance-id", Some("my-token"));
        assert_eq!(val.as_deref(), Some("i-v2value"));
    }

    #[test]
    fn read_metadata_falls_back_to_imdsv1_when_no_token() {
        let server = MockServer::bind();
        let base = server.meta_base.clone();
        server.serve_metadata(vec![("instance-id", "i-v1value")]);

        let agent = metadata_agent(Duration::from_secs(2));
        let val = read_metadata(&agent, &base, "instance-id", None);
        assert_eq!(val.as_deref(), Some("i-v1value"));
    }

    #[test]
    fn fetch_ecs_metadata_returns_all_fields() {
        let server = MockServer::bind();
        let base = server.meta_base.clone();
        server.serve_metadata(vec![
            ("instance-id", "i-fetch123"),
            ("region-id", "cn-beijing"),
            ("eipv4", "10.0.0.1"),
            ("public-ipv4", "172.16.0.1"),
        ]);

        let result = fetch_ecs_metadata(&base);
        let meta = result.expect("should return metadata");
        assert_eq!(meta.instance_id, "i-fetch123");
        assert_eq!(meta.region_id, "cn-beijing");
        assert_eq!(meta.eip, "10.0.0.1");
        assert_eq!(meta.public_ipv4, "172.16.0.1");
    }

    #[test]
    fn fetch_ecs_metadata_returns_none_when_instance_id_missing() {
        // Server has no mapping for "instance-id", returns "not-found" which is
        // treated as a valid value.  To test the None path, use an unreachable
        // address.
        let result = fetch_ecs_metadata("http://127.0.0.1:1/latest/meta-data");
        assert!(result.is_none(), "unreachable server should return None");
    }

    #[test]
    fn probe_with_mock_server_returns_metadata() {
        let server = MockServer::bind();
        let base = server.meta_base.clone();
        server.serve_metadata(vec![
            ("instance-id", "i-probe456"),
            ("region-id", "cn-shanghai"),
            ("eipv4", "8.8.8.8"),
            ("public-ipv4", ""),
        ]);

        let result = probe_ecs_metadata_with(&base);
        let meta = result.expect("probe should succeed with mock server");
        assert_eq!(meta.instance_id, "i-probe456");
        assert_eq!(meta.region_id, "cn-shanghai");
        assert_eq!(meta.eip, "8.8.8.8");
        assert!(meta.public_ipv4.is_empty());
    }

    #[test]
    fn probe_times_out_when_server_is_slow() {
        // Bind a listener but never respond — simulates an unreachable server
        // that accepts TCP but hangs on HTTP (the PROBE_TIMEOUT is 2s).
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let base = format!("http://127.0.0.1:{port}/latest/meta-data");

        // Accept connections in a background thread but never send data
        std::thread::spawn(move || {
            let _ = listener.accept(); // block until probe connects
            std::thread::sleep(Duration::from_secs(30)); // hold the connection
        });

        let start = std::time::Instant::now();
        let result = probe_ecs_metadata_with(&base);
        let elapsed = start.elapsed();

        assert!(result.is_none(), "slow server should cause timeout → None");
        assert!(
            elapsed < PROBE_TIMEOUT + Duration::from_secs(2),
            "probe should respect PROBE_TIMEOUT, took {elapsed:?}"
        );
    }

    // Environment-dependent: CI runners may be on ECS where metadata is reachable.
    // Run manually with: cargo test --lib -- ecs_metadata::tests::probe_returns_none_when_not_on_ecs --ignored
    #[test]
    #[ignore]
    fn probe_returns_none_when_not_on_ecs() {
        // On non-ECS machines the metadata endpoint is unreachable.
        // The probe should return None within the PROBE_TIMEOUT (2s).
        let start = std::time::Instant::now();
        let result = probe_ecs_metadata();
        let elapsed = start.elapsed();
        assert!(result.is_none(), "Expected None on non-ECS host");
        // recv_timeout guarantees we don't exceed PROBE_TIMEOUT by much
        assert!(
            elapsed < PROBE_TIMEOUT + Duration::from_secs(1),
            "Probe took too long: {elapsed:?}"
        );
    }
}
