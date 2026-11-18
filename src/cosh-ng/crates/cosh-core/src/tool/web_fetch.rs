//! Bounded HTTP(S) content retrieval.

use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use hyper_util::client::proxy::matcher::Matcher as ProxyMatcher;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::redirect::Policy;
use serde_json::Value;

use super::{Tool, ToolContext, ToolKind, ToolResult};

const MAX_RESPONSE_BYTES: usize = 512 * 1024;
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_REDIRECTS: usize = 5;

/// Addresses a fetched page must never reach: the machine's own network and
/// the ranges cloud providers serve instance metadata from.
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
                // 100.64.0.0/10 is CGNAT; Alibaba Cloud metadata lives at 100.100.100.200.
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

fn refusal_reason(host: &str, address: IpAddr) -> String {
    format!(
        "web_fetch refuses to reach {host}: it resolves to {address}, a loopback, \
         private, link-local or instance-metadata address"
    )
}

/// A target-policy refusal carried through the transport error chain so the
/// tool reports it as an actionable result, not a generic fetch failure.
#[derive(Debug)]
struct GuardRefusal(String);

impl std::fmt::Display for GuardRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for GuardRefusal {}

/// Checks IP literals without DNS; hostnames are checked by the guarded
/// resolver at connect time, inside the request deadline.
fn blocked_literal_reason(url: &reqwest::Url) -> Option<String> {
    let host = url
        .host_str()?
        .trim_start_matches('[')
        .trim_end_matches(']');
    let ip: IpAddr = host.parse().ok()?;
    if is_blocked_ip(ip) {
        Some(refusal_reason(host, ip))
    } else {
        None
    }
}

/// A proxied destination is resolved by the proxy, so the target guard cannot
/// pin the address actually fetched; refuse instead of fetching unguarded.
fn proxied_egress_reason(matcher: &ProxyMatcher, url: &reqwest::Url) -> Option<String> {
    let uri: http::Uri = match url.as_str().parse() {
        Ok(uri) => uri,
        Err(_) => return Some(format!("web_fetch cannot check proxy egress for {url}")),
    };
    matcher.intercept(&uri).map(|_| {
        format!(
            "web_fetch refuses to reach {} through a proxy: the proxy resolves the \
             destination, so the target guard cannot pin the fetched address; use \
             direct egress or exclude the host via NO_PROXY",
            url.host_str().unwrap_or("the target")
        )
    })
}

/// Resolves and checks at connect time so the address that was checked is the
/// address actually fetched, within the request's total deadline.
struct GuardedResolver;

impl Resolve for GuardedResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_string();
        let lookup_host = host.clone();
        Box::pin(async move {
            let addresses: Vec<SocketAddr> = tokio::task::spawn_blocking(move || {
                (lookup_host.as_str(), 0_u16).to_socket_addrs()
            })
            .await??
            .collect();
            if let Some(address) = addresses.iter().find(|address| is_blocked_ip(address.ip())) {
                return Err(Box::new(GuardRefusal(refusal_reason(&host, address.ip())))
                    as Box<dyn std::error::Error + Send + Sync>);
            }
            Ok(Box::new(addresses.into_iter()) as Addrs)
        })
    }
}

/// A redirect target is chosen by the server, so the approval card cannot
/// cover it; every hop is judged like the first request. `previous` includes
/// the initial URL, so the fifth redirect is still allowed (len 5 == max).
fn redirect_hop_error(
    previous: &[reqwest::Url],
    matcher: &ProxyMatcher,
    target: &reqwest::Url,
) -> Option<String> {
    if previous.len() > MAX_REDIRECTS {
        return Some("too many redirects".to_string());
    }
    if !matches!(target.scheme(), "http" | "https") {
        return Some("redirect to a non-HTTP(S) URL".to_string());
    }
    if let Some(reason) = proxied_egress_reason(matcher, target) {
        return Some(reason);
    }
    blocked_literal_reason(target)
}

fn redirect_policy(enforce_target_guard: bool, proxy_matcher: Arc<ProxyMatcher>) -> Policy {
    Policy::custom(move |attempt| {
        if enforce_target_guard {
            if let Some(reason) =
                redirect_hop_error(attempt.previous(), &proxy_matcher, attempt.url())
            {
                return attempt.error(GuardRefusal(reason));
            }
        } else if attempt.previous().len() > MAX_REDIRECTS {
            return attempt.error("too many redirects");
        }
        attempt.follow()
    })
}

pub(super) struct WebFetchTool {
    client: Option<reqwest::Client>,
    enforce_target_guard: bool,
    proxy_matcher: Arc<ProxyMatcher>,
    first_hop_precheck: bool,
}

impl WebFetchTool {
    pub(super) fn new() -> Self {
        Self::build(true, true)
    }

    /// Test-only: lets the content tests serve from a loopback listener.
    #[cfg(test)]
    fn without_target_guard() -> Self {
        Self::build(false, false)
    }

    /// Test-only: keeps the guarded redirect policy and resolver but skips the
    /// invoke precheck, so a loopback 302 can witness the per-hop wiring.
    #[cfg(test)]
    fn without_first_hop_precheck() -> Self {
        Self::build(true, false)
    }

    fn build(enforce_target_guard: bool, first_hop_precheck: bool) -> Self {
        // Same system snapshot reqwest's own proxy matcher reads at build time.
        let proxy_matcher = Arc::new(ProxyMatcher::from_system());
        let mut builder =
            reqwest::Client::builder()
                .timeout(FETCH_TIMEOUT)
                .redirect(redirect_policy(
                    enforce_target_guard,
                    Arc::clone(&proxy_matcher),
                ));
        if enforce_target_guard {
            builder = builder.dns_resolver(Arc::new(GuardedResolver));
        }
        let client = builder.build().ok();
        Self {
            client,
            enforce_target_guard,
            proxy_matcher,
            first_hop_precheck,
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

        if self.enforce_target_guard && self.first_hop_precheck {
            if let Some(reason) = proxied_egress_reason(&self.proxy_matcher, &parsed) {
                return Ok(ToolResult::error(reason));
            }
            if let Some(reason) = blocked_literal_reason(&parsed) {
                return Ok(ToolResult::error(reason));
            }
        }

        let client = self
            .client
            .as_ref()
            .ok_or("Failed to initialize the HTTP client")?;
        let response = match client.get(parsed).send().await {
            Ok(response) => response,
            Err(error) => {
                let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(&error);
                while let Some(inner) = cause {
                    if let Some(refusal) = inner.downcast_ref::<GuardRefusal>() {
                        return Ok(ToolResult::error(refusal.0.clone()));
                    }
                    cause = inner.source();
                }
                return Err(format!("Failed to fetch {url}: {error}"));
            }
        };
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

    #[tokio::test]
    async fn refuses_loopback_content() {
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

        let result = WebFetchTool::new()
            .invoke(
                serde_json::json!({"url": format!("http://{address}/")}),
                &ctx,
            )
            .await
            .unwrap();
        server.abort();

        assert!(result.is_error);
        assert!(result.output.contains("refuses to reach"));
    }

    #[tokio::test]
    async fn refuses_names_that_resolve_into_the_local_network() {
        let ctx = ToolContext::new(
            PathBuf::from("/tmp"),
            "test".to_string(),
            PathBuf::from("/tmp"),
        );

        let result = WebFetchTool::new()
            .invoke(serde_json::json!({"url": "http://localhost/"}), &ctx)
            .await
            .unwrap();

        assert!(result.is_error);
        assert!(result.output.contains("refuses to reach"));
    }

    #[test]
    fn blocks_the_address_space_the_machine_lives_in() {
        for address in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.0",
            "100.100.100.200",
            "100.127.255.255",
            "0.0.0.0",
            "240.0.0.1",
            "255.255.255.255",
            "::1",
            "fc00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
            "::ffff:100.100.100.200",
        ] {
            assert!(
                is_blocked_ip(address.parse().unwrap()),
                "{address} should be refused"
            );
        }
    }

    #[test]
    fn allows_public_addresses() {
        for address in [
            "93.184.216.34",
            "1.1.1.1",
            "100.128.0.1",
            "2606:4700:4700::1111",
        ] {
            assert!(
                !is_blocked_ip(address.parse().unwrap()),
                "{address} should be reachable"
            );
        }
    }

    #[tokio::test]
    async fn guarded_resolver_refuses_blocked_answers() {
        let name: Name = "localhost".parse().unwrap();
        let error = GuardedResolver
            .resolve(name)
            .await
            .err()
            .expect("blocked answer must be refused");
        assert!(error.to_string().contains("refuses to reach"));
    }

    #[test]
    fn redirect_hops_are_judged_like_the_first_request() {
        let matcher = ProxyMatcher::builder().build();
        let metadata = reqwest::Url::parse("http://100.100.100.200/latest/meta-data/").unwrap();
        let reason =
            redirect_hop_error(&[], &matcher, &metadata).expect("metadata hop must be refused");
        assert!(reason.contains("100.100.100.200"));

        let link_local = reqwest::Url::parse("http://169.254.169.254/latest/meta-data/").unwrap();
        assert!(redirect_hop_error(&[], &matcher, &link_local).is_some());

        // Named hops are judged by the guarded resolver at connect time.
        let loopback_name = reqwest::Url::parse("http://localhost/").unwrap();
        assert!(redirect_hop_error(&[], &matcher, &loopback_name).is_none());

        let non_http = reqwest::Url::parse("file:///etc/passwd").unwrap();
        assert!(redirect_hop_error(&[], &matcher, &non_http).is_some());

        let public = reqwest::Url::parse("https://93.184.216.34/").unwrap();
        assert!(redirect_hop_error(&[], &matcher, &public).is_none());

        let fifth: Vec<reqwest::Url> = (0..MAX_REDIRECTS).map(|_| public.clone()).collect();
        assert!(redirect_hop_error(&fifth, &matcher, &public).is_none());

        let sixth: Vec<reqwest::Url> = (0..=MAX_REDIRECTS).map(|_| public.clone()).collect();
        assert!(redirect_hop_error(&sixth, &matcher, &public).is_some());
    }

    #[test]
    fn proxied_destinations_are_refused() {
        let http_proxy = ProxyMatcher::builder().http("http://127.0.0.1:9").build();
        let plain = reqwest::Url::parse("http://example.com/").unwrap();
        let reason =
            proxied_egress_reason(&http_proxy, &plain).expect("proxied target must be refused");
        assert!(reason.contains("proxy"));

        let tls = reqwest::Url::parse("https://example.com/").unwrap();
        assert!(proxied_egress_reason(&http_proxy, &tls).is_none());

        let excluded = ProxyMatcher::builder()
            .http("http://127.0.0.1:9")
            .no("example.com")
            .build();
        assert!(proxied_egress_reason(&excluded, &plain).is_none());

        let direct = ProxyMatcher::builder().build();
        assert!(proxied_egress_reason(&direct, &plain).is_none());
    }

    #[tokio::test]
    async fn redirect_refusal_surfaces_through_invoke() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).await.unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 302 Found\r\n\
                      Location: http://100.100.100.200/latest/meta-data/\r\n\
                      Content-Length: 0\r\n\r\n",
                )
                .await
                .unwrap();
        });
        let ctx = ToolContext::new(
            PathBuf::from("/tmp"),
            "test".to_string(),
            PathBuf::from("/tmp"),
        );

        let result = WebFetchTool::without_first_hop_precheck()
            .invoke(
                serde_json::json!({"url": format!("http://{address}/")}),
                &ctx,
            )
            .await
            .unwrap();
        server.abort();

        assert!(result.is_error);
        assert!(result.output.contains("refuses to reach"));
        assert!(result.output.contains("100.100.100.200"));
    }
}
