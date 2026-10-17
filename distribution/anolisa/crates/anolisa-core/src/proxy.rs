//! Environment-proxy resolution shared by the bootstrap and download paths.
//!
//! ureq 2's global `try_proxy_from_env` switch is not URL-specific and does
//! not honor `no_proxy`, so both call sites resolve the proxy per URL through
//! [`env_proxy`] — the same rule the RPM transport already applies. Proxy
//! resolution is fail-closed: a proxy the environment configured but ureq
//! cannot honor is an error, never a silent direct fallback. Credentials are
//! never echoed in error messages.

use std::fmt;
use std::time::Duration;

use base64::Engine as _;

/// Error from resolving or honoring an environment-configured proxy, or from
/// resolving a redirect target. Credentials are never embedded in messages.
#[derive(Debug)]
pub struct ProxyError(pub String);

impl fmt::Display for ProxyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ProxyError {}

/// A proxy resolved from the environment for one URL.
#[derive(Debug)]
struct ProxySpec {
    /// Normalized `http://` URL with percent-decoded credentials embedded so
    /// ureq can authenticate an HTTPS CONNECT, or a placeholder host when the
    /// real host is an IPv6 literal.
    url: String,
    /// Percent-decoded `user:pass` for the `Proxy-Authorization` header on
    /// `http://` targets; `None` when the proxy is unauthenticated.
    http_authorization: Option<String>,
    /// IPv6 literal host plus port, when present. ureq 2 splits proxy hosts at
    /// `:`, so the literal is routed through a placeholder host plus a
    /// resolver instead of being embedded in the proxy URL.
    ipv6: Option<(std::net::Ipv6Addr, u16)>,
}

fn resolve_proxy(url: &str) -> Result<Option<ProxySpec>, ProxyError> {
    let Some(raw) = env_proxy::for_url_str(url).raw_value() else {
        return Ok(None);
    };
    let value = if raw.contains("://") {
        raw
    } else {
        format!("http://{raw}")
    };
    let parsed =
        url::Url::parse(&value).map_err(|err| ProxyError(format!("invalid proxy URL: {err}")))?;
    if parsed.scheme() != "http" {
        return Err(ProxyError(format!(
            "unsupported proxy scheme '{}'",
            parsed.scheme()
        )));
    }
    let port = parsed
        .port_or_known_default()
        .ok_or_else(|| ProxyError("proxy URL has no port".to_string()))?;
    let credentials = if !parsed.username().is_empty() || parsed.password().is_some() {
        let username = percent_encoding::percent_decode_str(parsed.username())
            .decode_utf8()
            .map_err(|_| ProxyError("proxy username is not UTF-8".to_string()))?;
        let password = percent_encoding::percent_decode_str(parsed.password().unwrap_or(""))
            .decode_utf8()
            .map_err(|_| ProxyError("proxy password is not UTF-8".to_string()))?;
        Some(format!("{username}:{password}"))
    } else {
        None
    };
    let authority = credentials
        .as_ref()
        .map(|credentials| format!("{credentials}@"))
        .unwrap_or_default();
    let (host, ipv6) = match parsed.host() {
        Some(url::Host::Ipv6(address)) => {
            ("anolisa-proxy.invalid".to_string(), Some((address, port)))
        }
        Some(host) => (host.to_string(), None),
        None => return Err(ProxyError("proxy URL has no host".to_string())),
    };
    Ok(Some(ProxySpec {
        url: format!("http://{authority}{host}:{port}"),
        http_authorization: credentials,
        ipv6,
    }))
}

/// Build a redirect-disabled agent for `url` whose proxy is re-resolved from
/// the environment for this exact URL.
///
/// HTTP proxies authenticate `http://` targets with a `Proxy-Authorization`
/// header (ureq 2 only authenticates HTTPS CONNECT automatically), and IPv6
/// proxy hosts are routed through a placeholder host plus a resolver — both
/// mirroring the RPM transport.
pub fn agent_for(
    url: &str,
    connect_timeout: Duration,
    read_timeout: Duration,
) -> Result<ureq::Agent, ProxyError> {
    let mut builder = ureq::AgentBuilder::new()
        .redirects(0)
        .timeout_connect(connect_timeout)
        .timeout_read(read_timeout);
    if let Some(spec) = resolve_proxy(url)? {
        if let Some((address, port)) = spec.ipv6 {
            builder = builder
                .resolver(move |_: &str| Ok(vec![std::net::SocketAddr::new(address.into(), port)]));
        }
        let proxy = ureq::Proxy::new(spec.url).map_err(|_| {
            // Discard ureq's error: it may echo the proxy URL, which embeds the
            // percent-decoded credentials. `spec.url` has already passed scheme,
            // host, and port validation above, so any failure here is a ureq
            // parser limitation, not a detail worth exposing.
            ProxyError("cannot honor configured proxy URL".to_string())
        })?;
        builder = builder.proxy(proxy);
        if url.starts_with("http://")
            && let Some(credentials) = spec.http_authorization
        {
            let authorization = format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode(credentials)
            );
            builder = builder.middleware(
                move |request: ureq::Request, next: ureq::MiddlewareNext<'_>| {
                    next.handle(request.set("Proxy-Authorization", &authorization))
                },
            );
        }
    }
    Ok(builder.build())
}

/// Resolve a redirect `Location` header against the current URL.
pub fn resolve_redirect(base: &str, location: &str) -> Result<String, ProxyError> {
    url::Url::parse(base)
        .and_then(|current| current.join(location))
        .map(|next| next.to_string())
        .map_err(|err| ProxyError(format!("invalid redirect location '{location}': {err}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Serialize the environment-manipulating tests: `env_proxy` reads the
    /// process environment, which is shared across parallel test threads.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_proxy_env(value: &str, test: impl FnOnce()) {
        let _guard = ENV_LOCK.lock().unwrap();
        let keys = ["https_proxy", "HTTPS_PROXY"];
        let previous: Vec<Option<String>> =
            keys.iter().map(|key| std::env::var(key).ok()).collect();
        unsafe {
            std::env::set_var("https_proxy", value);
            std::env::set_var("HTTPS_PROXY", value);
        }
        test();
        for (index, key) in keys.iter().enumerate() {
            match &previous[index] {
                Some(value) => unsafe { std::env::set_var(key, value) },
                None => unsafe { std::env::remove_var(key) },
            }
        }
    }

    #[test]
    fn proxy_percent_decodes_credentials() {
        with_proxy_env("http://user%40dom:p%40ss@proxy.example:8080", || {
            let spec = resolve_proxy("https://example.com/x").unwrap().unwrap();
            assert_eq!(spec.http_authorization.as_deref(), Some("user@dom:p@ss"));
            assert!(spec.url.contains("user@dom:p@ss@proxy.example:8080"));
        });
    }

    #[test]
    fn proxy_rejects_unsupported_scheme_without_leaking_password() {
        with_proxy_env("socks5://user:secret@proxy.example:8080", || {
            let err = resolve_proxy("https://example.com/x").unwrap_err();
            assert!(!err.0.contains("secret"));
            assert!(err.0.contains("unsupported proxy scheme"));
        });
    }

    #[test]
    fn proxy_routes_ipv6_host_through_placeholder() {
        with_proxy_env("http://[::1]:8080", || {
            let spec = resolve_proxy("https://example.com/x").unwrap().unwrap();
            assert_eq!(spec.ipv6, Some(("::1".parse().unwrap(), 8080)));
            assert!(spec.url.contains("anolisa-proxy.invalid:8080"));
        });
    }

    #[test]
    fn resolve_redirect_resolves_relative_location() {
        let next = resolve_redirect("https://example.com/a/b.toml", "../c.toml").unwrap();
        assert_eq!(next, "https://example.com/c.toml");
    }
}
