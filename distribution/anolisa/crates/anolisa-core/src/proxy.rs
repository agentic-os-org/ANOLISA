//! Environment-proxy resolution shared by the bootstrap and download paths.
//!
//! ureq 2's global `try_proxy_from_env` switch is not URL-specific and does
//! not honor `no_proxy`, so both call sites resolve the proxy per URL through
//! [`env_proxy`] — the same rule the RPM transport already applies.

/// Resolve the proxy for `url` from the environment and build a [`ureq::Proxy`].
///
/// Returns `None` when no proxy applies (unset or the host matches `no_proxy`)
/// or when the resolved scheme is unsupported by ureq, so callers fall back to
/// a direct connection instead of failing the fetch.
pub fn env_proxy_for(url: &str) -> Option<ureq::Proxy> {
    let raw = env_proxy::for_url_str(url).raw_value()?;
    let value = if raw.contains("://") {
        raw
    } else {
        format!("http://{raw}")
    };
    ureq::Proxy::new(value).ok()
}
