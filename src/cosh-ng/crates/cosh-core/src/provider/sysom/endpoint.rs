//! Endpoint resolution for the SysOM API, preferring the VPC proxy.
//!
//! ECS instances without public egress cannot reach the public SysOM endpoint,
//! but Alibaba Cloud exposes the same service inside every VPC at
//! `sysom.vpc-proxy.aliyuncs.com`. That name resolves only inside a VPC, and
//! the VPC's private DNS maps it to the POP of the instance's own region, so the
//! client needs no region handling of its own.
//!
//! Reachability is decided by a single bounded TCP connect rather than by
//! probing the ECS metadata service: metadata returns 403 on hardened instances,
//! which would misclassify exactly the hosts that most need the internal route.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::OnceCell;

/// Public endpoint, used when the VPC proxy is not reachable.
pub const PUBLIC_HOST: &str = "sysom.cn-hangzhou.aliyuncs.com";

/// VPC-internal endpoint. Region-less: private DNS resolves it per region.
pub const VPC_PROXY_HOST: &str = "sysom.vpc-proxy.aliyuncs.com";

/// Overrides the whole resolution, as an escape hatch and for debugging.
const ENV_ENDPOINT: &str = "COSH_SYSOM_ENDPOINT";
/// Redirects the probe target so tests can point it at a local listener.
const ENV_PROBE_HOST: &str = "COSH_SYSOM_VPC_PROXY_HOST";
/// Shortens the probe deadline so tests need not wait the production value.
const ENV_PROBE_TIMEOUT_MS: &str = "COSH_SYSOM_PROBE_TIMEOUT_MS";

/// Absolute deadline for the reachability probe.
///
/// Inside a VPC the proxy answers in tens of milliseconds. Outside one the name
/// does not resolve, and an NXDOMAIN costs single-digit to ~90ms against a
/// healthy resolver, so this deadline is the ceiling for a resolver that stalls
/// rather than the routine cost of running on a non-VPC host.
const PROBE_DEADLINE_MS: u64 = 500;

/// Maximum length of a DNS name.
const MAX_HOST_LEN: usize = 253;

/// Scheme assumed for a bare host and used for the probed and fallback hosts.
const DEFAULT_SCHEME: &str = "https";

/// How the effective host was chosen. Surfaced in logs so an operator can tell
/// which route an instance took without reproducing the probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointOrigin {
    /// Explicitly configured, via env var or `sysom_endpoint`.
    Configured,
    /// VPC proxy: the probe reached it.
    VpcProxy,
    /// Public endpoint: the probe failed, timed out, or this is not a VPC host.
    Public,
}

impl EndpointOrigin {
    /// Stable identifier for logs.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Configured => "configured",
            Self::VpcProxy => "vpc_proxy",
            Self::Public => "public",
        }
    }
}

/// The chosen SysOM endpoint and how it was chosen.
pub struct ResolvedEndpoint {
    /// Bare `host` or `host:port`.
    ///
    /// Invariant: carries no scheme, slash or path. Callers use it verbatim as
    /// the signed `host` header, so any extra character would desynchronise the
    /// signature from the wire Host and the gateway would answer
    /// `SignatureDoesNotMatch`.
    pub host: String,
    /// `http` or `https`.
    ///
    /// Carried separately rather than assumed, because a configured endpoint may
    /// legitimately be plain HTTP — a local proxy or a test server — and forcing
    /// TLS onto it turns a working configuration into a handshake failure.
    pub scheme: String,
    pub origin: EndpointOrigin,
}

impl ResolvedEndpoint {
    /// Scheme and host joined, without a trailing slash or path.
    pub fn base_url(&self) -> String {
        format!("{}://{}", self.scheme, self.host)
    }

    /// Keep automatic VPC requests on the same direct path as the probe.
    pub fn configure_client(&self, builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
        match self.origin {
            EndpointOrigin::VpcProxy => builder.no_proxy(),
            EndpointOrigin::Configured | EndpointOrigin::Public => builder,
        }
    }
}

/// Probe result, computed at most once per process.
///
/// Wrapped in a `Mutex<Option<..>>` so tests can swap in a fresh cell.
static VPC_REACHABLE: Mutex<Option<Arc<OnceCell<bool>>>> = Mutex::new(None);

/// Return the shared probe cache, creating it if necessary.
fn reachability_cache() -> Arc<OnceCell<bool>> {
    VPC_REACHABLE
        .lock()
        .expect("vpc reachability cache lock")
        .get_or_insert_with(Arc::default)
        .clone()
}

/// Resolve the SysOM host, preferring the VPC proxy when it is reachable.
///
/// Priority, short-circuiting on the first match so an explicit choice never
/// pays for the probe:
///
/// 1. `COSH_SYSOM_ENDPOINT`
/// 2. `configured_endpoint` (the provider's `sysom_endpoint` setting)
/// 3. VPC proxy, if the probe reaches it
/// 4. Public endpoint
///
/// Step 2 deliberately does not read `base_url`: that setting is
/// OpenAI-compatible and defaults to a DashScope address, so honouring it here
/// sent ACS3-signed SysOM traffic to DashScope.
///
/// A *failed* probe lands on the public endpoint, which is the behaviour before
/// VPC support existed, so an unreachable proxy degrades to "no improvement"
/// rather than to a new outage.
///
/// A *successful* probe commits the process: the verdict is cached for the
/// process lifetime and requests carry no fallback of their own. A proxy that
/// completes the TCP handshake but cannot serve the API, or a route that
/// disappears after the probe, therefore fails hard instead of reverting to
/// public. Recovering from that would mean re-signing against a second host
/// mid-request, which is out of scope here.
pub async fn resolve(configured_endpoint: &str) -> ResolvedEndpoint {
    if let Some((scheme, host)) = env_endpoint(ENV_ENDPOINT) {
        tracing::debug!(host = %host, origin = "configured", "sysom endpoint from env");
        return ResolvedEndpoint {
            host,
            scheme,
            origin: EndpointOrigin::Configured,
        };
    }
    if let Some((scheme, host)) = split_endpoint(configured_endpoint) {
        tracing::debug!(host = %host, origin = "configured", "sysom endpoint from config");
        return ResolvedEndpoint {
            host,
            scheme,
            origin: EndpointOrigin::Configured,
        };
    }
    if probe_vpc_proxy().await {
        return ResolvedEndpoint {
            host: VPC_PROXY_HOST.to_string(),
            scheme: DEFAULT_SCHEME.to_string(),
            origin: EndpointOrigin::VpcProxy,
        };
    }
    ResolvedEndpoint {
        host: PUBLIC_HOST.to_string(),
        scheme: DEFAULT_SCHEME.to_string(),
        origin: EndpointOrigin::Public,
    }
}

/// Read an env var as an endpoint, ignoring values that are empty or malformed.
///
/// Ignoring beats failing here: a typo in an override must not take the provider
/// down, and must not let an arbitrary string reach the signed `host` header.
fn env_endpoint(key: &str) -> Option<(String, String)> {
    split_endpoint(&std::env::var(key).ok()?)
}

/// Split a configured endpoint into its scheme and bare `host` or `host:port`.
///
/// Accepts what the existing config shapes allow — a bare host, or a URL with a
/// scheme, port and path — and returns `None` for anything that fails
/// validation. A bare host is assumed to be HTTPS.
///
/// Only `http` and `https` are admitted; anything else (`file`, `ftp`, …) is
/// rejected rather than silently reduced to its host.
fn split_endpoint(input: &str) -> Option<(String, String)> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let url = if trimmed.contains("://") {
        reqwest::Url::parse(trimmed).ok()?
    } else {
        if !is_valid_host(trimmed) {
            return None;
        }
        reqwest::Url::parse(&format!("{DEFAULT_SCHEME}://{trimmed}")).ok()?
    };
    let scheme = url.scheme();
    if !matches!(scheme, "http" | "https") {
        return None;
    }
    let host = url.host_str()?;
    let host = match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    };
    is_valid_host(&host).then(|| (scheme.to_string(), host))
}

/// Accept only characters legal in a host or `host:port`.
///
/// The allowlist is what upholds the `ResolvedEndpoint::host` invariant: it
/// admits no scheme separator, slash, whitespace or control character.
fn is_valid_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= MAX_HOST_LEN
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | ':'))
}

/// Probe target as `host:port`, honouring the test override.
///
/// Any scheme on the override is ignored: the probe is a bare TCP connect.
fn probe_target() -> String {
    match env_endpoint(ENV_PROBE_HOST).map(|(_, host)| host) {
        Some(host) if host.contains(':') => host,
        Some(host) => format!("{host}:443"),
        None => format!("{VPC_PROXY_HOST}:443"),
    }
}

/// Probe deadline, honouring the test override.
fn probe_deadline() -> Duration {
    let millis = std::env::var(ENV_PROBE_TIMEOUT_MS)
        .ok()
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(PROBE_DEADLINE_MS);
    Duration::from_millis(millis)
}

/// Whether the VPC proxy accepts a TCP connection, computed once per process.
///
/// One connect covers both DNS resolution and L4 reachability. DNS failure,
/// refusal and timeout are all treated the same: not reachable.
///
/// The deadline bounds the *caller*, not the resolver. `TcpStream::connect`
/// resolves a hostname on the blocking pool, and `timeout` can only detach that
/// task, never cancel it, so a wedged resolver keeps one blocking thread busy
/// after `resolve` has already returned. The per-process cache bounds that to a
/// single occurrence. `sls::fetch_region_id_from_metadata` has no such tail
/// because it dials a fixed `SocketAddr` and never resolves a name.
async fn probe_vpc_proxy() -> bool {
    let cell = reachability_cache();
    *cell
        .get_or_init(|| async {
            let target = probe_target();
            let deadline = probe_deadline();
            let started = std::time::Instant::now();
            let reachable = matches!(
                tokio::time::timeout(deadline, tokio::net::TcpStream::connect(&target)).await,
                Ok(Ok(_))
            );
            tracing::info!(
                target = %target,
                reachable,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "sysom vpc proxy probe"
            );
            reachable
        })
        .await
}

#[cfg(test)]
fn reset_reachability_cache() {
    *VPC_REACHABLE.lock().expect("vpc reachability cache lock") = None;
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;

    use super::*;

    /// Serialize env-var-dependent tests to avoid cross-test interference.
    static ENV_TEST_MUTEX: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// RAII guard that restores an env var on drop so tests never leak state
    /// even if the test body panics.
    struct EnvVarGuard {
        key: &'static str,
        old_value: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let old_value = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, old_value }
        }

        fn remove(key: &'static str) -> Self {
            let old_value = std::env::var(key).ok();
            std::env::remove_var(key);
            Self { key, old_value }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.old_value {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    /// Address that swallows packets without answering, so the probe can only
    /// end by hitting its deadline. TEST-NET-1 (RFC 5737) is reserved for
    /// documentation and is not routable, so no real host is contacted.
    const BLACKHOLE: &str = "192.0.2.1:443";

    /// Bind a listener that accepts connections, standing in for the proxy.
    fn live_listener() -> TcpListener {
        TcpListener::bind("127.0.0.1:0").expect("bind probe listener")
    }

    /// Bind then drop, yielding an address that refuses connections.
    fn closed_port() -> String {
        let listener = live_listener();
        let addr = listener.local_addr().expect("listener addr").to_string();
        drop(listener);
        addr
    }

    fn assert_host_invariant(host: &str) {
        assert!(
            !host.contains("://"),
            "host must not carry a scheme: {host}"
        );
        assert!(!host.contains('/'), "host must not carry a path: {host}");
    }

    async fn http_responder(body: &'static str) -> (String, tokio::task::JoinHandle<()>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            assert!(stream.read(&mut request).await.unwrap() > 0);
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });
        (address, task)
    }

    #[tokio::test]
    async fn only_automatic_vpc_routes_bypass_proxy() {
        for (origin, expected) in [
            (EndpointOrigin::VpcProxy, "direct"),
            (EndpointOrigin::Configured, "proxy"),
            (EndpointOrigin::Public, "proxy"),
        ] {
            let (host, direct) = http_responder("direct").await;
            let (proxy_host, proxy) = http_responder("proxy").await;
            let resolved = ResolvedEndpoint {
                host,
                scheme: "http".to_string(),
                origin,
            };
            let builder = reqwest::Client::builder()
                .proxy(reqwest::Proxy::all(format!("http://{proxy_host}")).unwrap())
                .timeout(Duration::from_secs(3));
            let client = resolved.configure_client(builder).build().unwrap();
            let response = client.get(resolved.base_url()).send().await;
            let body = match response {
                Ok(response) => response.text().await,
                Err(err) => Err(err),
            };
            direct.abort();
            proxy.abort();
            let _ = direct.await;
            let _ = proxy.await;
            assert_eq!(body.unwrap(), expected, "origin={origin:?}");
        }
    }

    #[tokio::test]
    async fn env_override_wins_and_skips_probe() {
        let _lock = ENV_TEST_MUTEX.lock().await;
        reset_reachability_cache();
        let listener = live_listener();
        let probe_addr = listener.local_addr().expect("listener addr").to_string();
        let _endpoint = EnvVarGuard::set(ENV_ENDPOINT, "sysom.cn-shanghai.aliyuncs.com");
        let _probe = EnvVarGuard::set(ENV_PROBE_HOST, &probe_addr);

        let resolved = resolve("").await;

        assert_eq!(resolved.origin, EndpointOrigin::Configured);
        assert_eq!(resolved.host, "sysom.cn-shanghai.aliyuncs.com");
        assert_host_invariant(&resolved.host);

        // Nothing must have dialed the probe target.
        listener
            .set_nonblocking(true)
            .expect("listener nonblocking");
        assert!(
            listener.accept().is_err(),
            "probe ran despite an explicit endpoint override"
        );
    }

    #[tokio::test]
    async fn configured_endpoint_wins_and_skips_probe() {
        let _lock = ENV_TEST_MUTEX.lock().await;
        reset_reachability_cache();
        let listener = live_listener();
        let probe_addr = listener.local_addr().expect("listener addr").to_string();
        let _endpoint = EnvVarGuard::remove(ENV_ENDPOINT);
        let _probe = EnvVarGuard::set(ENV_PROBE_HOST, &probe_addr);

        let resolved = resolve("http://127.0.0.1:8080/v1").await;

        assert_eq!(resolved.origin, EndpointOrigin::Configured);
        assert_eq!(resolved.host, "127.0.0.1:8080");
        assert_host_invariant(&resolved.host);

        listener
            .set_nonblocking(true)
            .expect("listener nonblocking");
        assert!(
            listener.accept().is_err(),
            "probe ran despite an explicit configured endpoint"
        );
    }

    /// Pins step 1 above step 2. Without this, inverting the two leaves every
    /// other test green: each one sets at most one of the pair, so nothing
    /// observes which of them outranks the other.
    #[tokio::test]
    async fn env_override_outranks_configured_endpoint() {
        let _lock = ENV_TEST_MUTEX.lock().await;
        reset_reachability_cache();
        let _endpoint = EnvVarGuard::set(ENV_ENDPOINT, "from-env.example.com");

        let resolved = resolve("https://from-config.example.com").await;
        assert_eq!(resolved.host, "from-env.example.com");

        drop(_endpoint);
        let _absent = EnvVarGuard::remove(ENV_ENDPOINT);
        reset_reachability_cache();

        let resolved = resolve("https://from-config.example.com").await;
        assert_eq!(resolved.host, "from-config.example.com");
    }

    #[tokio::test]
    async fn reachable_probe_selects_vpc_proxy() {
        let _lock = ENV_TEST_MUTEX.lock().await;
        reset_reachability_cache();
        let listener = live_listener();
        let probe_addr = listener.local_addr().expect("listener addr").to_string();
        let _endpoint = EnvVarGuard::remove(ENV_ENDPOINT);
        let _probe = EnvVarGuard::set(ENV_PROBE_HOST, &probe_addr);

        let resolved = resolve("").await;

        assert_eq!(resolved.origin, EndpointOrigin::VpcProxy);
        assert_eq!(resolved.host, VPC_PROXY_HOST);
        assert_host_invariant(&resolved.host);
    }

    #[tokio::test]
    async fn unresolvable_probe_falls_back_to_public() {
        let _lock = ENV_TEST_MUTEX.lock().await;
        reset_reachability_cache();
        let _endpoint = EnvVarGuard::remove(ENV_ENDPOINT);
        let _probe = EnvVarGuard::set(ENV_PROBE_HOST, "no-such-host.invalid:443");

        let resolved = resolve("").await;

        assert_eq!(resolved.origin, EndpointOrigin::Public);
        assert_eq!(resolved.host, PUBLIC_HOST);
        assert_host_invariant(&resolved.host);
    }

    #[tokio::test]
    async fn refused_probe_falls_back_to_public() {
        let _lock = ENV_TEST_MUTEX.lock().await;
        reset_reachability_cache();
        let _endpoint = EnvVarGuard::remove(ENV_ENDPOINT);
        let _probe = EnvVarGuard::set(ENV_PROBE_HOST, &closed_port());

        let resolved = resolve("").await;

        assert_eq!(resolved.origin, EndpointOrigin::Public);
        assert_eq!(resolved.host, PUBLIC_HOST);
    }

    /// The load-bearing test for "a failed probe must not look like a hang":
    /// against a black hole, resolution must still return within the deadline.
    ///
    /// Weak on networks that answer TEST-NET-1 with `ENETUNREACH` instead of
    /// dropping the packets: there the connect fails immediately and this
    /// degrades into another refusal test, passing without exercising the
    /// deadline at all. A green run is therefore not by itself proof that the
    /// deadline works.
    #[tokio::test]
    async fn blackholed_probe_respects_deadline() {
        let _lock = ENV_TEST_MUTEX.lock().await;
        reset_reachability_cache();
        let _endpoint = EnvVarGuard::remove(ENV_ENDPOINT);
        let _probe = EnvVarGuard::set(ENV_PROBE_HOST, BLACKHOLE);
        let _timeout = EnvVarGuard::set(ENV_PROBE_TIMEOUT_MS, "200");

        let started = std::time::Instant::now();
        let resolved = resolve("").await;
        let elapsed = started.elapsed();

        assert_eq!(resolved.origin, EndpointOrigin::Public);
        assert!(
            elapsed < Duration::from_millis(1_000),
            "resolution took {elapsed:?}, deadline was 200ms"
        );
    }

    #[tokio::test]
    async fn malformed_env_override_is_ignored() {
        let _lock = ENV_TEST_MUTEX.lock().await;
        reset_reachability_cache();
        let _probe = EnvVarGuard::set(ENV_PROBE_HOST, &closed_port());

        for bad in ["evil.com/../path", "host with space", "", "host\u{7f}name"] {
            reset_reachability_cache();
            let _endpoint = EnvVarGuard::set(ENV_ENDPOINT, bad);
            let resolved = resolve("").await;
            assert_eq!(
                resolved.origin,
                EndpointOrigin::Public,
                "malformed override {bad:?} was not ignored"
            );
            assert_host_invariant(&resolved.host);
        }
    }

    #[test]
    fn split_endpoint_accepts_supported_shapes() {
        assert_eq!(
            split_endpoint("sysom.cn-hangzhou.aliyuncs.com"),
            Some((
                "https".to_string(),
                "sysom.cn-hangzhou.aliyuncs.com".to_string()
            ))
        );
        assert_eq!(
            split_endpoint("https://sysom.cn-hangzhou.aliyuncs.com"),
            Some((
                "https".to_string(),
                "sysom.cn-hangzhou.aliyuncs.com".to_string()
            ))
        );
        assert_eq!(
            split_endpoint("http://127.0.0.1:8080/v1"),
            Some(("http".to_string(), "127.0.0.1:8080".to_string()))
        );
        assert_eq!(
            split_endpoint("  spaced.example.com  "),
            Some(("https".to_string(), "spaced.example.com".to_string()))
        );
    }

    #[test]
    fn bare_endpoints_follow_the_same_url_validation() {
        for host in ["proxy:65536", "proxy:invalid", "proxy:80:90", ":443"] {
            assert_eq!(split_endpoint(host), None, "accepted {host}");
            assert_eq!(split_endpoint(&format!("https://{host}")), None);
        }
        for host in ["EXAMPLE.COM:443", "example.com:8443", "127.0.0.1:8080"] {
            assert_eq!(
                split_endpoint(host),
                split_endpoint(&format!("https://{host}"))
            );
        }
    }

    #[test]
    fn split_endpoint_rejects_unsupported_shapes() {
        assert_eq!(split_endpoint(""), None);
        assert_eq!(split_endpoint("   "), None);
        assert_eq!(split_endpoint("host/path"), None);
        assert_eq!(split_endpoint("host with space"), None);
        assert_eq!(split_endpoint(&"a".repeat(MAX_HOST_LEN + 1)), None);
        assert_eq!(split_endpoint("file:///etc/passwd"), None);
        assert_eq!(split_endpoint("ftp://example.com"), None);
    }

    /// IPv6 is unsupported, and must stay *rejected* rather than half-accepted:
    /// `host_str` keeps the brackets, which the host allowlist excludes, so the
    /// value never reaches the signed `host` header. Admitting IPv6 later means
    /// deciding whether the brackets belong in the signature, so this pins the
    /// boundary instead of leaving it to chance.
    #[test]
    fn split_endpoint_rejects_ipv6() {
        assert_eq!(split_endpoint("http://[::1]:8080/v1"), None);
        assert_eq!(split_endpoint("https://[2400:3200::1]"), None);
        assert_eq!(split_endpoint("[::1]:8080"), None);
    }

    /// A plain-HTTP endpoint must stay plain HTTP. Upgrading it to HTTPS turns a
    /// working local proxy or test server into a TLS handshake failure.
    #[tokio::test]
    async fn configured_http_scheme_is_preserved() {
        let _lock = ENV_TEST_MUTEX.lock().await;
        reset_reachability_cache();
        let _endpoint = EnvVarGuard::remove(ENV_ENDPOINT);

        let resolved = resolve("http://127.0.0.1:9999/v1").await;

        assert_eq!(resolved.scheme, "http");
        assert_eq!(resolved.host, "127.0.0.1:9999");
        assert_eq!(resolved.base_url(), "http://127.0.0.1:9999");
        assert_host_invariant(&resolved.host);
    }

    #[tokio::test]
    async fn probed_and_fallback_hosts_use_https() {
        let _lock = ENV_TEST_MUTEX.lock().await;
        reset_reachability_cache();
        let _endpoint = EnvVarGuard::remove(ENV_ENDPOINT);
        let _probe = EnvVarGuard::set(ENV_PROBE_HOST, "no-such-host.invalid:443");

        let resolved = resolve("").await;

        assert_eq!(resolved.scheme, "https");
        assert_eq!(resolved.base_url(), format!("https://{PUBLIC_HOST}"));
    }
}
