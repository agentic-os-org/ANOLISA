//! Exact HTTP authority grants in a validated Agent configuration.

use crate::Error;
use serde_json::Value;
use std::net::{Ipv4Addr, Ipv6Addr};

/// Returns the selected Agent's validated grants, defaulting to an empty list.
pub fn plaintext_http_hosts(agent: &Value) -> &[Value] {
    agent["claude_sdk"]["allow_plaintext_http_hosts"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
}

/// Whether a validated target explicitly trusts its CLI for model transport.
pub fn direct_model_transport(agent: &Value) -> bool {
    agent["claude_sdk"]["model_transport"] == "direct"
}

pub(crate) fn validate(target: &str, agent: &Value) -> Result<(), Error> {
    if agent.get("claude_sdk").is_some() && agent["adapter"] != "claude-sdk" {
        return Err(Error::Invariant {
            path: format!("/spec/agents/{target}/claude_sdk"),
            reason: "Claude SDK settings require the Claude SDK Adapter",
        });
    }
    for (i, entry) in plaintext_http_hosts(agent).iter().enumerate() {
        if !entry.as_str().is_some_and(valid_authority) {
            return Err(Error::Invariant {
                path: format!("/spec/agents/{target}/claude_sdk/allow_plaintext_http_hosts/{i}"),
                reason: "expected an exact hostname or IP address, optionally with port 1..65535 (bracket IPv6 with a port)",
            });
        }
    }
    Ok(())
}

fn valid_port(port: &str) -> bool {
    port.bytes().all(|b| b.is_ascii_digit()) && port.parse::<u16>().is_ok_and(|p| p > 0)
}

fn valid_authority(entry: &str) -> bool {
    if let Some(bracketed) = entry.strip_prefix('[') {
        return bracketed.split_once(']').is_some_and(|(host, suffix)| {
            host.parse::<Ipv6Addr>().is_ok()
                && (suffix.is_empty() || suffix.strip_prefix(':').is_some_and(valid_port))
        });
    }
    if entry.parse::<Ipv6Addr>().is_ok() {
        return true;
    }
    let host = match entry.split_once(':') {
        Some((host, port)) if valid_port(port) => host,
        Some(_) => return false,
        None => entry,
    };
    if host.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return host.parse::<Ipv4Addr>().is_ok();
    }
    // The final root label is valid only on DNS names. Keep it in the grant.
    let labels = host.strip_suffix('.').unwrap_or(host);
    labels.len() <= 253
        && labels.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}
