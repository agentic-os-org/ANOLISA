//! Narrow HTTP transport with one deadline, explicit credentials and redacted failures.

use super::Error;
use reqwest::{
    blocking::{Client, RequestBuilder},
    header::{HeaderValue, CONTENT_TYPE},
    Url,
};
use serde_json::Value;
use std::{
    fs::OpenOptions,
    io::Read,
    net::IpAddr,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
    time::Instant,
};

pub(super) struct Transport {
    client: Client,
    authorization: Option<HeaderValue>,
}

pub(super) fn endpoint(input: &str) -> Result<Url, Error> {
    let invalid =
        || Error::Configuration("endpoint must be an HTTPS origin or literal HTTP loopback");
    if input.len() > 2048
        || input
            .bytes()
            .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
        || input.contains(['\\', '@', '?', '#'])
        || !(input.starts_with("https://") || input.starts_with("http://"))
    {
        return Err(invalid());
    }
    let url = Url::parse(input).map_err(|_| invalid())?;
    if url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || url.port() == Some(0)
    {
        return Err(invalid());
    }
    if url.scheme() == "http" {
        let host = url.host_str().ok_or_else(invalid)?.trim_matches(['[', ']']);
        if !host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback()) {
            return Err(invalid());
        }
        // Require the literal spelling too; URL parsers normalize shorthand/encoded hosts.
        let authority = input
            .strip_prefix("http://")
            .ok_or_else(invalid)?
            .trim_end_matches('/');
        let literal = if let Some(rest) = authority.strip_prefix('[') {
            rest.split(']').next().ok_or_else(invalid)?
        } else {
            authority.split(':').next().ok_or_else(invalid)?
        };
        if !literal.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback()) {
            return Err(invalid());
        }
    }
    Ok(url)
}

impl Transport {
    pub(super) fn new(endpoint: &Url, path: &Path) -> Result<Self, Error> {
        // A loopback listener has no authenticated peer identity. Never load its token.
        let authorization = if endpoint.scheme() == "https" {
            Some(read_authorization(path)?)
        } else {
            None
        };
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .pool_max_idle_per_host(0)
            .build()
            .map_err(|_| Error::Unavailable)?;
        Ok(Self {
            client,
            authorization,
        })
    }

    pub(super) fn authentication(&self) -> &'static str {
        if self.authorization.is_some() {
            "bearer_supplied"
        } else {
            "loopback_unauthenticated"
        }
    }

    fn request(&self, url: Url) -> RequestBuilder {
        let authorization = self
            .authorization
            .as_ref()
            .filter(|_| url.scheme() == "https");
        let request = self.client.get(url).header("Accept", "application/json");
        match authorization {
            Some(value) => request.header("Authorization", value.clone()),
            None => request,
        }
    }

    pub(super) fn get(
        &self,
        url: Url,
        deadline: Instant,
        remaining: &mut usize,
    ) -> Result<(u16, Value), Error> {
        let timeout = deadline
            .checked_duration_since(Instant::now())
            .ok_or(Error::Deadline)?;
        if timeout.is_zero() {
            return Err(Error::Deadline);
        }
        let mut response = self.request(url).timeout(timeout).send().map_err(|error| {
            if error.is_timeout() {
                Error::Deadline
            } else {
                Error::Unavailable
            }
        })?;
        let status = response.status().as_u16();
        match status {
            200 | 404 => {}
            401 | 403 => return Err(Error::Authentication),
            300..=399 => return Err(Error::Redirect),
            _ => return Err(Error::Http(status)),
        }
        if response
            .content_length()
            .is_some_and(|len| len > *remaining as u64)
        {
            return Err(Error::Limit);
        }
        if response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(';').next())
            .is_none_or(|v| !v.trim().eq_ignore_ascii_case("application/json"))
        {
            return Err(Error::InvalidResponse);
        }
        let mut bytes = Vec::new();
        (&mut response)
            .take(*remaining as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::TimedOut || Instant::now() >= deadline {
                    Error::Deadline
                } else {
                    Error::Unavailable
                }
            })?;
        if bytes.len() > *remaining {
            return Err(Error::Limit);
        }
        *remaining -= bytes.len();
        // A 404 from this API conflates source absence and missing data. Its body is not evidence.
        let value = if status == 404 {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).map_err(|_| Error::InvalidResponse)?
        };
        Ok((status, value))
    }
}

fn read_authorization(path: &Path) -> Result<HeaderValue, Error> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| Error::Credential)?;
    let meta = file.metadata().map_err(|_| Error::Credential)?;
    // SAFETY: geteuid has no preconditions or pointer arguments.
    let owner = unsafe { libc::geteuid() };
    if !meta.is_file() || meta.uid() != owner || meta.mode() & 0o077 != 0 || meta.len() > 4096 {
        return Err(Error::Credential);
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(4097)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::Credential)?;
    if bytes.len() > 4096 {
        return Err(Error::Credential);
    }
    let token = std::str::from_utf8(&bytes).map_err(|_| Error::Credential)?;
    let token = token
        .strip_suffix("\r\n")
        .or_else(|| token.strip_suffix('\n'))
        .unwrap_or(token);
    if token.is_empty() || !token.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(Error::Credential);
    }
    let mut authorization =
        HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| Error::Credential)?;
    authorization.set_sensitive(true);
    Ok(authorization)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt};

    #[test]
    fn credentials_are_loaded_and_attached_only_for_https() {
        let directory = tempfile::tempdir().unwrap();
        let token = directory.path().join("token");
        fs::write(&token, "fixture-token").unwrap();
        fs::set_permissions(&token, fs::Permissions::from_mode(0o600)).unwrap();
        for origin in ["https://agentsight.invalid", "https://127.0.0.1:7396"] {
            let url = endpoint(origin).unwrap();
            let transport = Transport::new(&url, &token).unwrap();
            assert_eq!(transport.authentication(), "bearer_supplied");
            let request = transport.request(url).build().unwrap();
            let header = &request.headers()["Authorization"];
            assert_eq!(header, "Bearer fixture-token");
            assert!(header.is_sensitive());
            // Keep the no-plaintext invariant at the shared request boundary too.
            let request = transport
                .request(endpoint("http://127.0.0.1").unwrap())
                .build()
                .unwrap();
            assert!(!request.headers().contains_key("Authorization"));
        }
        for origin in ["http://127.0.0.1:7396", "http://[::1]:7396"] {
            let url = endpoint(origin).unwrap();
            let transport = Transport::new(&url, &directory.path().join("missing")).unwrap();
            assert_eq!(transport.authentication(), "loopback_unauthenticated");
            let request = transport.request(url).build().unwrap();
            assert!(!request.headers().contains_key("Authorization"));
        }
    }
}
