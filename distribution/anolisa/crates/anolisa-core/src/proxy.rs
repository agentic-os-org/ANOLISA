//! Environment-proxy resolution shared by the bootstrap and download paths.
//!
//! ureq 2's global `try_proxy_from_env` switch is not URL-specific and does
//! not honor `no_proxy`, so both call sites resolve the proxy per URL through
//! [`env_proxy`] — the same rule the RPM transport already applies. Proxy
//! resolution is fail-closed: a proxy the environment configured but ureq
//! cannot honor is an error, never a silent direct fallback.

use std::fmt;
use std::time::Duration;

/// Error from resolving or honoring an environment-configured proxy, or from
/// resolving a redirect target while following one.
#[derive(Debug)]
pub struct ProxyError(pub String);

impl fmt::Display for ProxyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ProxyError {}

/// Resolve the proxy for `url` from the environment and build a [`ureq::Proxy`].
///
/// `Ok(None)` means no proxy applies (unset or the host matches `no_proxy`).
/// An `Err` means a proxy was configured but ureq cannot honor it (for example
/// an unsupported `https://` or `socks5://` scheme), so callers must fail
/// closed instead of silently connecting directly and bypassing the operator's
/// proxy boundary.
pub fn env_proxy_for(url: &str) -> Result<Option<ureq::Proxy>, ProxyError> {
    let Some(raw) = env_proxy::for_url_str(url).raw_value() else {
        return Ok(None);
    };
    let value = if raw.contains("://") {
        raw
    } else {
        format!("http://{raw}")
    };
    let proxy_url = url::Url::parse(&value)
        .map_err(|err| ProxyError(format!("invalid proxy URL '{value}': {err}")))?;
    if proxy_url.scheme() != "http" {
        return Err(ProxyError(format!(
            "unsupported proxy scheme '{}' in '{value}'",
            proxy_url.scheme()
        )));
    }
    ureq::Proxy::new(value.clone())
        .map(Some)
        .map_err(|err| ProxyError(format!("cannot honor configured proxy '{value}': {err}")))
}

/// Build a redirect-disabled agent for `url` whose proxy is re-resolved from
/// the environment for this exact URL.
pub fn agent_for(
    url: &str,
    connect_timeout: Duration,
    read_timeout: Duration,
) -> Result<ureq::Agent, ProxyError> {
    let mut builder = ureq::AgentBuilder::new()
        .redirects(0)
        .timeout_connect(connect_timeout)
        .timeout_read(read_timeout);
    if let Some(proxy) = env_proxy_for(url)? {
        builder = builder.proxy(proxy);
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

    #[test]
    fn env_proxy_for_rejects_unsupported_scheme() {
        let keys = ["https_proxy", "HTTPS_PROXY"];
        let previous: Vec<Option<String>> =
            keys.iter().map(|key| std::env::var(key).ok()).collect();
        unsafe {
            std::env::set_var("https_proxy", "https://proxy.example:8443");
            std::env::set_var("HTTPS_PROXY", "https://proxy.example:8443");
        }
        let result = env_proxy_for("https://example.com/repo.toml");
        for (index, key) in keys.iter().enumerate() {
            match &previous[index] {
                Some(value) => unsafe { std::env::set_var(key, value) },
                None => unsafe { std::env::remove_var(key) },
            }
        }
        assert!(result.is_err());
    }

    #[test]
    fn resolve_redirect_resolves_relative_location() {
        let next = resolve_redirect("https://example.com/a/b.toml", "../c.toml").unwrap();
        assert_eq!(next, "https://example.com/c.toml");
    }
}
