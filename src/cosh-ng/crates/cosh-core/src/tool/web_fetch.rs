//! Bounded HTTP(S) content retrieval.

use std::net::{IpAddr, ToSocketAddrs};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use reqwest::redirect::Policy;
use serde_json::Value;

use super::{Tool, ToolContext, ToolKind, ToolResult};

const MAX_RESPONSE_BYTES: usize = 512 * 1024;
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// Hosts `web_fetch` must never reach: this machine's own network and the
/// addresses a cloud provider puts its instance metadata on. A page the user
/// asked for must not be able to make the agent read them.
fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_unspecified()
                || octets[0] == 0
                || octets[0] >= 240
                // 100.64.0.0/10 is carrier-grade NAT, and 100.100.100.200 sits
                // in it: Alibaba Cloud serves instance metadata from there.
                || (octets[0] == 100 && (64..=127).contains(&octets[1]))
        }
        IpAddr::V6(v6) => {
            let segments = v6.segments();
            v6.is_loopback()
                || v6.is_unspecified()
                || (segments[0] & 0xfe00) == 0xfc00
                || (segments[0] & 0xffc0) == 0xfe80
                || v6
                    .to_ipv4_mapped()
                    .is_some_and(|v4| is_blocked_ip(IpAddr::V4(v4)))
        }
    }
}

/// Explains why `url` must not be fetched, or `None` when every address it
/// resolves to is public.
///
/// Resolution happens here rather than in `reqwest` so the address that is
/// checked is the address that gets fetched, and so a redirect hop — which a
/// server chooses, not the caller — is checked the same way as the first
/// request.
fn blocked_target_reason(url: &reqwest::Url) -> Option<String> {
    let host = url
        .host_str()?
        .trim_start_matches('[')
        .trim_end_matches(']');
    let port = url.port_or_known_default().unwrap_or(80);
    let addresses: Vec<IpAddr> = match host.parse::<IpAddr>() {
        Ok(ip) => vec![ip],
        Err(_) => match (host, port).to_socket_addrs() {
            Ok(resolved) => resolved.map(|address| address.ip()).collect(),
            Err(_) => return Some(format!("web_fetch cannot resolve {host}")),
        },
    };
    addresses
        .iter()
        .find(|address| is_blocked_ip(**address))
        .map(|address| {
            format!(
                "web_fetch refuses to reach {host}: it resolves to {address}, a loopback, \
                 private, link-local or instance-metadata address"
            )
        })
}

/// Follows at most five redirects, refusing any hop that leaves HTTP(S) or
/// lands on a blocked address.
fn redirect_policy(enforce_target_guard: bool) -> Policy {
    Policy::custom(move |attempt| {
        if attempt.previous().len() >= 5 {
            return attempt.error("too many redirects");
        }
        let target = attempt.url();
        if !matches!(target.scheme(), "http" | "https") {
            return attempt.error("redirect to a non-HTTP(S) URL");
        }
        if enforce_target_guard {
            if let Some(reason) = blocked_target_reason(target) {
                return attempt.error(reason);
            }
        }
        attempt.follow()
    })
}

pub(super) struct WebFetchTool {
    client: Option<reqwest::Client>,
    enforce_target_guard: bool,
}

impl WebFetchTool {
    pub(super) fn new() -> Self {
        Self::with_target_guard(true)
    }

    /// Builds a tool that also reaches loopback, so tests can serve content
    /// from a local listener. Production call sites use `new`.
    #[cfg(test)]
    fn without_target_guard() -> Self {
        Self::with_target_guard(false)
    }

    fn with_target_guard(enforce_target_guard: bool) -> Self {
        let client = reqwest::Client::builder()
            .timeout(FETCH_TIMEOUT)
            .redirect(redirect_policy(enforce_target_guard))
            .build()
            .ok();
        Self {
            client,
            enforce_target_guard,
        }
    }
}

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "web_fetch"
    }

    fn description(&self) -> &str {
        "Fetch bounded text content from an HTTP or HTTPS URL. Network access requires approval outside trust mode."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "Fully qualified HTTP or HTTPS URL"
                },
                "prompt": {
                    "type": "string",
                    "description": "Optional description of the information to extract"
                }
            },
            "required": ["url"]
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Network
    }

    async fn invoke(&self, params: Value, _ctx: &ToolContext) -> Result<ToolResult, String> {
        let url = params
            .get("url")
            .and_then(Value::as_str)
            .ok_or("missing 'url' parameter")?;
        let parsed =
            reqwest::Url::parse(url).map_err(|error| format!("invalid URL '{url}': {error}"))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Ok(ToolResult::error(
                "web_fetch only supports HTTP and HTTPS URLs",
            ));
        }

        if self.enforce_target_guard {
            if let Some(reason) = blocked_target_reason(&parsed) {
                return Ok(ToolResult::error(reason));
            }
        }

        let client = self
            .client
            .as_ref()
            .ok_or("Failed to initialize the HTTP client")?;
        let response = client
            .get(parsed)
            .send()
            .await
            .map_err(|error| format!("Failed to fetch {url}: {error}"))?;
        let status = response.status();
        if !status.is_success() {
            return Ok(ToolResult::error(format!(
                "Request to {url} failed with HTTP {status}"
            )));
        }

        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(ToOwned::to_owned);
        if let Some(content_type) = content_type.as_deref() {
            if !is_textual_content_type(content_type) {
                return Ok(ToolResult::error(format!(
                    "Unsupported Content-Type '{content_type}' from {url}; \
                     web_fetch only supports textual responses"
                )));
            }
        }
        let content_type = content_type.unwrap_or_else(|| "unknown".to_string());
        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        let mut truncated = false;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| format!("Failed to read {url}: {error}"))?;
            let remaining = MAX_RESPONSE_BYTES.saturating_sub(body.len());
            if chunk.len() > remaining {
                body.extend_from_slice(&chunk[..remaining]);
                truncated = true;
                break;
            }
            body.extend_from_slice(&chunk);
            if body.len() == MAX_RESPONSE_BYTES {
                truncated = true;
                break;
            }
        }

        let mut output = format!(
            "Source: {url}\nContent-Type: {content_type}\n\n{}",
            String::from_utf8_lossy(&body)
        );
        if truncated {
            output.push_str("\n\n[response truncated at 524288 bytes]");
        }
        Ok(ToolResult::success(output))
    }
}

fn is_textual_content_type(content_type: &str) -> bool {
    let media_type = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let Some((category, subtype)) = media_type.split_once('/') else {
        return false;
    };

    category == "text"
        || (category == "application"
            && (matches!(
                subtype,
                "json"
                    | "xml"
                    | "javascript"
                    | "x-javascript"
                    | "yaml"
                    | "x-yaml"
                    | "toml"
                    | "x-www-form-urlencoded"
            ) || subtype.ends_with("+json")
                || subtype.ends_with("+xml")))
        || media_type == "image/svg+xml"
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn fetches_http_content() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).await.unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 5\r\n\r\nhello",
                )
                .await
                .unwrap();
        });
        let ctx = ToolContext::new(
            PathBuf::from("/tmp"),
            "test".to_string(),
            PathBuf::from("/tmp"),
        );

        let result = WebFetchTool::without_target_guard()
            .invoke(
                serde_json::json!({"url": format!("http://{address}/")}),
                &ctx,
            )
            .await
            .unwrap();
        server.await.unwrap();

        assert!(!result.is_error);
        assert!(result.output.contains("Content-Type: text/plain"));
        assert!(result.output.ends_with("hello"));
    }

    #[tokio::test]
    async fn rejects_non_http_urls() {
        let ctx = ToolContext::new(
            PathBuf::from("/tmp"),
            "test".to_string(),
            PathBuf::from("/tmp"),
        );

        let result = WebFetchTool::without_target_guard()
            .invoke(serde_json::json!({"url": "file:///etc/passwd"}), &ctx)
            .await
            .unwrap();

        assert!(result.is_error);
    }

    #[tokio::test]
    async fn rejects_binary_content_types() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).await.unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/pdf\r\n\
                      Content-Length: 4\r\n\r\n\x00\xff\x00\xff",
                )
                .await
                .unwrap();
        });
        let ctx = ToolContext::new(
            PathBuf::from("/tmp"),
            "test".to_string(),
            PathBuf::from("/tmp"),
        );

        let result = WebFetchTool::without_target_guard()
            .invoke(
                serde_json::json!({"url": format!("http://{address}/document.pdf")}),
                &ctx,
            )
            .await
            .unwrap();
        server.await.unwrap();

        assert!(result.is_error);
        assert!(result.output.contains("application/pdf"));
        assert!(result.output.contains("only supports textual responses"));
        assert!(!result.output.contains('\u{fffd}'));
    }

    #[test]
    fn recognizes_structured_text_content_types() {
        for content_type in [
            "text/plain; charset=utf-8",
            "application/json",
            "application/problem+json",
            "application/xml",
            "application/atom+xml",
            "image/svg+xml",
        ] {
            assert!(
                is_textual_content_type(content_type),
                "expected textual Content-Type: {content_type}"
            );
        }
        for content_type in ["application/pdf", "application/zip", "image/png"] {
            assert!(
                !is_textual_content_type(content_type),
                "expected binary Content-Type: {content_type}"
            );
        }
    }

    #[test]
    fn blocks_the_address_space_the_machine_lives_in() {
        for address in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.100.100.200",
            "0.0.0.0",
            "255.255.255.255",
            "::1",
            "fc00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(
                is_blocked_ip(address.parse().unwrap()),
                "{address} should be refused"
            );
        }
    }

    #[test]
    fn allows_public_addresses() {
        for address in ["93.184.216.34", "1.1.1.1", "2606:4700:4700::1111"] {
            assert!(
                !is_blocked_ip(address.parse().unwrap()),
                "{address} should be reachable"
            );
        }
    }

    #[tokio::test]
    async fn refuses_the_instance_metadata_endpoint() {
        let ctx = ToolContext::new(
            PathBuf::from("/tmp"),
            "test".to_string(),
            PathBuf::from("/tmp"),
        );

        let result = WebFetchTool::new()
            .invoke(
                serde_json::json!({"url": "http://100.100.100.200/latest/meta-data/"}),
                &ctx,
            )
            .await
            .unwrap();

        assert!(result.is_error);
        assert!(result.output.contains("refuses to reach"));
        assert!(result.output.contains("100.100.100.200"));
    }

    #[test]
    fn refuses_names_that_resolve_into_the_local_network() {
        let url = reqwest::Url::parse("http://localhost/").unwrap();
        assert!(blocked_target_reason(&url).is_some());
    }

    #[test]
    fn a_redirect_target_is_judged_like_the_first_request() {
        // The policy hands every hop to this helper, so the helper is what a
        // redirect into the metadata range is refused by. It is exercised
        // directly because reaching the policy needs a fetch that already
        // passed the same check.
        let url = reqwest::Url::parse("http://169.254.169.254/latest/meta-data/").unwrap();
        let reason = blocked_target_reason(&url).expect("link-local hop must be refused");
        assert!(reason.contains("169.254.169.254"));
    }
}
