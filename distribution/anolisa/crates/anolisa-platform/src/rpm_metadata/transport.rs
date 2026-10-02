//! Host network settings for RPM metadata; Raw downloads have their own transport.

use base64::Engine;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use ureq::rustls::{
    ClientConfig, RootCertStore, pki_types::CertificateDer, pki_types::pem::PemObject,
};

pub(super) struct Transport {
    main: ini::Properties,
}

impl Transport {
    pub(super) fn system() -> Result<Self, String> {
        Self::load(&[Path::new("/etc/yum.conf"), Path::new("/etc/dnf/dnf.conf")])
    }

    fn load(paths: &[&Path]) -> Result<Self, String> {
        for path in paths {
            let content = match std::fs::read_to_string(path) {
                Ok(content) => content,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => {
                    return Err(format!(
                        "read RPM network configuration {}: {e}",
                        path.display()
                    ));
                }
            };
            // Do not include parser input in diagnostics: it can contain credentials.
            let config = ini::Ini::load_from_str_noescape(&content)
                .map_err(|_| format!("invalid RPM network configuration {}", path.display()))?;
            return Ok(Self {
                main: config.section(Some("main")).cloned().unwrap_or_default(),
            });
        }
        Ok(Self {
            main: ini::Properties::new(),
        })
    }

    pub(super) fn get(&self, url: &url::Url) -> Result<ureq::Response, String> {
        let mut current = url.clone();
        let mut authorization = None;
        for _ in 0..=5 {
            let mut request = self.agent(&current)?.get(current.as_str());
            if !current.username().is_empty() || current.password().is_some() {
                // ureq 2 encodes URL userinfo verbatim. Decode once before Basic
                // encoding; explicit credentials replace any inherited header.
                let mut credentials =
                    percent_encoding::percent_decode_str(current.username()).collect::<Vec<_>>();
                credentials.push(b':');
                credentials.extend(percent_encoding::percent_decode_str(
                    current.password().unwrap_or(""),
                ));
                authorization = Some(format!(
                    "Basic {}",
                    base64::engine::general_purpose::STANDARD.encode(credentials)
                ));
            }
            if let Some(authorization) = &authorization {
                request = request.set("Authorization", authorization);
            }
            let response = request.call().map_err(|e| {
                let origin = super::diagnostic_origin(current.as_str());
                match e {
                    ureq::Error::Status(status, _) => format!("fetch {origin}: HTTP {status}"),
                    ureq::Error::Transport(error) => format!("fetch {origin}: {:?}", error.kind()),
                }
            })?;
            if !matches!(response.status(), 301 | 302 | 303 | 307 | 308) {
                return Ok(response);
            }
            let next = current
                .join(
                    response
                        .header("Location")
                        .ok_or("metadata redirect has no Location")?,
                )
                .map_err(|e| format!("invalid metadata redirect: {e}"))?;
            if !matches!(next.scheme(), "http" | "https")
                || (current.scheme() == "https" && next.scheme() != "https")
            {
                return Err(
                    "metadata redirect changes to an insecure or unsupported scheme".into(),
                );
            }
            // Absolute same-origin Locations omit userinfo but retain authentication.
            // Once we leave that origin, even a later return must not restore it.
            if next.origin() != current.origin() {
                authorization = None;
            }
            current = next;
        }
        Err("too many metadata redirects".into())
    }

    fn agent(&self, url: &url::Url) -> Result<ureq::Agent, String> {
        let mut builder = ureq::AgentBuilder::new()
            .try_proxy_from_env(false)
            .redirects(0)
            .timeout_connect(Duration::from_secs(10))
            .timeout_read(Duration::from_secs(60));
        let configured = self.main.get("proxy");
        // _none_ and an empty proxy disable inherited settings. Environment
        // selection is URL-specific and honors no_proxy, unlike ureq 2's switch.
        let proxy = match configured {
            Some("" | "_none_") => None,
            Some(value) => Some(value.to_string()),
            None => env_proxy::for_url(url).raw_value(),
        };
        if let Some(value) = proxy {
            let value = if value.contains("://") {
                value
            } else {
                format!("http://{value}")
            };
            let proxy_url = url::Url::parse(&value).map_err(|_| "invalid RPM proxy URL")?;
            if proxy_url.scheme() != "http" {
                return Err("unsupported RPM proxy scheme".into());
            }
            let credentials =
                if let Some(username) = configured.and_then(|_| self.main.get("proxy_username")) {
                    Some(format!(
                        "{username}:{}",
                        self.main.get("proxy_password").unwrap_or("")
                    ))
                } else if !proxy_url.username().is_empty() || proxy_url.password().is_some() {
                    let username = percent_encoding::percent_decode_str(proxy_url.username())
                        .decode_utf8()
                        .map_err(|_| "non-UTF-8 RPM proxy username")?;
                    let password =
                        percent_encoding::percent_decode_str(proxy_url.password().unwrap_or(""))
                            .decode_utf8()
                            .map_err(|_| "non-UTF-8 RPM proxy password")?;
                    Some(format!("{username}:{password}"))
                } else {
                    None
                };
            let port = proxy_url
                .port_or_known_default()
                .ok_or("RPM proxy URL has no port")?;
            let host = match proxy_url.host().ok_or("RPM proxy URL has no host")? {
                url::Host::Ipv6(address) => {
                    // ureq 2 splits proxy hosts at ':'. Its resolver only resolves
                    // the proxy endpoint, so map an internal alias to the literal.
                    builder = builder.resolver(move |_: &str| {
                        Ok(vec![std::net::SocketAddr::new(address.into(), port)])
                    });
                    "anolisa-proxy.invalid".to_string()
                }
                host => host.to_string(),
            };
            // ureq 2 expects raw credentials rather than URL-encoded userinfo.
            let authority = credentials
                .as_ref()
                .map(|s| format!("{s}@"))
                .unwrap_or_default();
            let proxy = ureq::Proxy::new(format!("http://{authority}{host}:{port}"))
                .map_err(|_| "invalid or unsupported RPM proxy URL")?;
            // ureq 2 only authenticates HTTPS CONNECT. HTTP needs a header;
            // manual redirects prevent forwarding it into a TLS origin request.
            if url.scheme() == "http"
                && let Some(credentials) = credentials
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
            builder = builder.proxy(proxy);
        }
        if url.scheme() == "https" {
            let mut roots = RootCertStore::empty();
            if let Some(path) = self.main.get("sslcacert").filter(|s| !s.is_empty()) {
                let certs = CertificateDer::pem_file_iter(path)
                    .map_err(|e| format!("read RPM sslcacert {path}: {e}"))?;
                for cert in certs {
                    roots
                        .add(cert.map_err(|e| format!("parse RPM sslcacert {path}: {e}"))?)
                        .map_err(|e| format!("invalid RPM sslcacert {path}: {e}"))?;
                }
            } else {
                let native = rustls_native_certs::load_native_certs();
                roots.add_parsable_certificates(native.certs);
                if roots.is_empty() {
                    return Err(format!(
                        "no system CA certificates available: {:?}",
                        native.errors
                    ));
                }
            }
            if roots.is_empty() {
                return Err("RPM sslcacert contains no CA certificates".into());
            }
            let tls = ClientConfig::builder_with_provider(
                ureq::rustls::crypto::ring::default_provider().into(),
            )
            .with_safe_default_protocol_versions()
            .map_err(|e| e.to_string())?
            .with_root_certificates(roots)
            .with_no_client_auth();
            builder = builder.tls_config(Arc::new(tls));
        }
        Ok(builder.build())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};
    use std::process::Command;
    use std::thread;
    use ureq::rustls::{ServerConfig, ServerConnection, StreamOwned, pki_types::PrivateKeyDer};

    fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/rpm-network")
            .join(name)
    }

    fn accept(listener: &TcpListener) -> TcpStream {
        listener.set_nonblocking(true).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    stream
                        .set_write_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    return stream;
                }
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        && std::time::Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(e) => panic!("test server did not receive a connection: {e}"),
            }
        }
    }

    fn request(input: &mut impl BufRead) -> std::io::Result<String> {
        let mut headers = String::new();
        loop {
            let mut line = String::new();
            if input.read_line(&mut line)? == 0 || line == "\r\n" {
                break;
            }
            headers.push_str(&line);
        }
        Ok(headers)
    }

    fn tls_config() -> Arc<ServerConfig> {
        let certificates = CertificateDer::pem_file_iter(fixture("server.pem"))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let key = PrivateKeyDer::from_pem_file(fixture("server.key")).unwrap();
        let config = ServerConfig::builder_with_provider(
            ureq::rustls::crypto::ring::default_provider().into(),
        )
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(certificates, key)
        .unwrap();
        Arc::new(config)
    }

    fn tls_server() -> (url::Url, thread::JoinHandle<bool>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = url::Url::parse(&format!(
            "https://localhost:{}/metadata",
            listener.local_addr().unwrap().port()
        ))
        .unwrap();
        let server = thread::spawn(move || {
            let mut stream = BufReader::new(StreamOwned::new(
                ServerConnection::new(tls_config()).unwrap(),
                accept(&listener),
            ));
            let Ok(headers) = request(&mut stream) else {
                return false;
            };
            if !headers.starts_with("GET /metadata ") {
                return false;
            }
            stream
                .get_mut()
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\nmetadata",
                )
                .unwrap();
            stream.get_mut().flush().unwrap();
            true
        });
        (url, server)
    }

    #[test]
    fn repository_userinfo_is_decoded_once_for_basic_authentication() {
        for (userinfo, credentials) in [
            ("user:p%2Fss", "user:p/ss"),
            ("u%40ser:p%40ss%23%25", "u@ser:p@ss#%"),
            ("user:literal%252F", "user:literal%2F"),
            ("user", "user:"),
            (":p%2Fss", ":p/ss"),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = url::Url::parse(&format!(
                "http://{userinfo}@{}/metadata",
                listener.local_addr().unwrap()
            ))
            .unwrap();
            let expected = format!(
                "Authorization: Basic {}",
                base64::engine::general_purpose::STANDARD.encode(credentials)
            );
            let server = thread::spawn(move || {
                let mut stream = BufReader::new(accept(&listener));
                let headers = request(&mut stream).unwrap();
                let authenticated = headers
                    .lines()
                    .filter(|line| line.starts_with("Authorization:"))
                    .collect::<Vec<_>>()
                    == [expected];
                let status = if authenticated {
                    "200 OK"
                } else {
                    "401 Unauthorized"
                };
                write!(
                    stream.get_mut(),
                    "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )
                .unwrap();
                authenticated
            });
            let mut main = ini::Properties::new();
            main.insert("proxy", "_none_");
            let result = Transport { main }.get(&url);
            assert!(
                server.join().unwrap(),
                "server received wrong Basic credentials for {userinfo}"
            );
            assert!(result.is_ok(), "{result:?}");
        }
    }

    #[test]
    fn repository_authentication_survives_same_origin_absolute_redirects() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let url = url::Url::parse(&format!("http://user:p%2Fss@{address}/metadata")).unwrap();
        let server = thread::spawn(move || {
            for (path, credentials, location) in [
                (
                    "/metadata",
                    "user:p/ss",
                    Some(format!("http://{address}/absolute")),
                ),
                (
                    "/absolute",
                    "user:p/ss",
                    Some(format!("//{address}/network")),
                ),
                (
                    "/network",
                    "user:p/ss",
                    Some(format!("http://other:next%2Fpass@{address}/override")),
                ),
                ("/override", "other:next/pass", Some("/relative".into())),
                ("/relative", "other:next/pass", None),
            ] {
                let mut stream = BufReader::new(accept(&listener));
                let headers = request(&mut stream).unwrap();
                assert!(headers.starts_with(&format!("GET {path} ")));
                let expected = format!(
                    "Authorization: Basic {}",
                    base64::engine::general_purpose::STANDARD.encode(credentials)
                );
                if !headers.lines().any(|line| line == expected) {
                    stream.get_mut().write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
                    return false;
                }
                let response = match location {
                    Some(location) => format!(
                        "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    ),
                    None => {
                        "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into()
                    }
                };
                stream.get_mut().write_all(response.as_bytes()).unwrap();
            }
            true
        });
        let mut main = ini::Properties::new();
        main.insert("proxy", "_none_");
        let result = Transport { main }.get(&url);
        assert!(
            server.join().unwrap(),
            "same-origin redirect lost or replaced repository credentials"
        );
        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    fn repository_authentication_follows_relative_but_not_cross_origin_redirects() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let other = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = url::Url::parse(&format!(
            "http://user:p%2Fss@{}/metadata",
            listener.local_addr().unwrap()
        ))
        .unwrap();
        let return_url = format!("http://{}/return", listener.local_addr().unwrap());
        let next_origin = format!("http://{}/primary", other.local_addr().unwrap());
        let source = thread::spawn(move || {
            for (path, location) in [
                ("/metadata", "/redirect"),
                ("/redirect", next_origin.as_str()),
            ] {
                let mut stream = BufReader::new(accept(&listener));
                let headers = request(&mut stream).unwrap();
                assert!(headers.starts_with(&format!("GET {path} ")));
                assert!(headers.lines().any(|line| line
                    == format!(
                        "Authorization: Basic {}",
                        base64::engine::general_purpose::STANDARD.encode("user:p/ss")
                    )));
                write!(stream.get_mut(), "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
            }
            let mut stream = BufReader::new(accept(&listener));
            let headers = request(&mut stream).unwrap();
            assert!(headers.starts_with("GET /return "));
            assert!(!headers.to_ascii_lowercase().contains("authorization:"));
            stream
                .get_mut()
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .unwrap();
        });
        let destination = thread::spawn(move || {
            let mut stream = BufReader::new(accept(&other));
            let headers = request(&mut stream).unwrap();
            assert!(headers.starts_with("GET /primary "));
            assert!(!headers.to_ascii_lowercase().contains("authorization:"));
            write!(stream.get_mut(), "HTTP/1.1 302 Found\r\nLocation: {return_url}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        });
        let mut main = ini::Properties::new();
        main.insert("proxy", "_none_");
        Transport { main }.get(&url).unwrap();
        source.join().unwrap();
        destination.join().unwrap();
    }

    #[test]
    fn configured_proxy_is_used_for_metadata_and_authentication_is_preserved() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("yum.conf");
        std::fs::write(
            &config,
            format!(
                "[main]\nproxy=http://{}\nproxy_username=user\nproxy_password=secret\n",
                listener.local_addr().unwrap()
            ),
        )
        .unwrap();
        let server = thread::spawn(move || {
            let mut stream = BufReader::new(accept(&listener));
            let headers = request(&mut stream).unwrap();
            assert!(
                headers.starts_with("GET http://repository.invalid/metadata "),
                "{headers}"
            );
            assert!(
                headers
                    .to_ascii_lowercase()
                    .contains("proxy-authorization: basic dxnlcjpzzwnyzxq=")
            );
            stream
                .get_mut()
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\nmetadata",
                )
                .unwrap();
        });
        let transport = Transport::load(&[&config]).unwrap();
        let dest = dir.path().join("result");
        super::super::fetch(
            &url::Url::parse("http://repository.invalid/metadata").unwrap(),
            &dest,
            Some(&transport),
            super::super::INDEX_LIMIT,
        )
        .unwrap();
        assert_eq!(std::fs::read(dest).unwrap(), b"metadata");
        server.join().unwrap();
    }

    #[test]
    fn redirect_rebuilds_tls_and_keeps_proxy_credentials_out_of_origin_headers() {
        let listener = TcpListener::bind("[::1]:0").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("yum.conf");
        std::fs::write(&config, format!("[main]\nproxy=http://{}\nproxy_username=user\nproxy_password=secret/#:@%\nsslcacert={}\n", listener.local_addr().unwrap(), fixture("ca.pem").display())).unwrap();
        let server = thread::spawn(move || {
            let mut first = BufReader::new(accept(&listener));
            assert!(
                request(&mut first)
                    .unwrap()
                    .starts_with("GET http://repository.invalid/metadata ")
            );
            first.get_mut().write_all(b"HTTP/1.1 302 Found\r\nLocation: https://localhost/metadata\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
            drop(first);
            let mut tunnel = BufReader::new(accept(&listener));
            let headers = request(&mut tunnel).unwrap();
            assert!(headers.starts_with("CONNECT localhost:443 "));
            assert!(headers.to_ascii_lowercase().contains(&format!(
                    "proxy-authorization: basic {}",
                    base64::engine::general_purpose::STANDARD
                        .encode("user:secret/#:@%")
                        .to_ascii_lowercase()
                )));
            tunnel
                .get_mut()
                .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                .unwrap();
            let mut origin = BufReader::new(StreamOwned::new(
                ServerConnection::new(tls_config()).unwrap(),
                tunnel.into_inner(),
            ));
            let headers = request(&mut origin).unwrap();
            assert!(headers.starts_with("GET "));
            assert!(!headers.to_ascii_lowercase().contains("proxy-authorization"));
            origin
                .get_mut()
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\nmetadata",
                )
                .unwrap();
            origin.get_mut().flush().unwrap();
        });
        let transport = Transport::load(&[&config]).unwrap();
        let destination = dir.path().join("result");
        super::super::fetch(
            &url::Url::parse("http://repository.invalid/metadata").unwrap(),
            &destination,
            Some(&transport),
            super::super::INDEX_LIMIT,
        )
        .unwrap();
        assert_eq!(std::fs::read(destination).unwrap(), b"metadata");
        server.join().unwrap();
    }

    #[test]
    fn bad_config_and_missing_ca_fail_without_exposing_proxy_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("yum.conf");
        std::fs::write(
            &config,
            "[main]\nproxy=unsupported://user:secret@example.invalid\n",
        )
        .unwrap();
        let transport = Transport::load(&[&config]).unwrap();
        let err = transport
            .agent(&url::Url::parse("https://localhost/").unwrap())
            .unwrap_err();
        assert!(err.contains("proxy"));
        assert!(!err.contains("secret"));
        std::fs::write(
            &config,
            format!(
                "[main]\nproxy=_none_\nsslcacert={}\n",
                dir.path().join("missing.pem").display()
            ),
        )
        .unwrap();
        let transport = Transport::load(&[&config]).unwrap();
        assert!(
            transport
                .agent(&url::Url::parse("https://localhost/").unwrap())
                .unwrap_err()
                .contains("sslcacert")
        );
    }

    #[test]
    fn proxy_credentials_preserve_reserved_characters_and_ipv6() {
        for bind in ["127.0.0.1:0", "[::1]:0"] {
            for embedded in [false, true] {
                for password in ["secret/#:@%", "encoded%2Fpassword"] {
                    let listener = TcpListener::bind(bind).unwrap();
                    let dir = tempfile::tempdir().unwrap();
                    let config = dir.path().join("yum.conf");
                    let mut proxy =
                        url::Url::parse(&format!("http://{}", listener.local_addr().unwrap()))
                            .unwrap();
                    let settings = if embedded {
                        // Encode percent signs too: Url setters accept existing escapes.
                        proxy.set_username("user").unwrap();
                        proxy
                            .set_password(Some(
                                &percent_encoding::utf8_percent_encode(
                                    password,
                                    percent_encoding::NON_ALPHANUMERIC,
                                )
                                .to_string(),
                            ))
                            .unwrap();
                        format!("proxy={proxy}\n")
                    } else {
                        format!("proxy={proxy}\nproxy_username=user\nproxy_password={password}\n")
                    };
                    std::fs::write(&config, format!("[main]\n{settings}")).unwrap();
                    let expected = format!(
                        "Proxy-Authorization: Basic {}",
                        base64::engine::general_purpose::STANDARD
                            .encode(format!("user:{password}"))
                    );
                    let server = thread::spawn(move || {
                        let mut stream = BufReader::new(accept(&listener));
                        let headers = request(&mut stream).unwrap();
                        assert!(headers.contains(&expected), "{headers}");
                        stream.get_mut().write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
                    });
                    let transport = Transport::load(&[&config]).unwrap();
                    let result = transport
                        .get(&url::Url::parse("http://repository.invalid/metadata").unwrap());
                    assert!(result.is_ok(), "{bind}: {result:?}");
                    server.join().unwrap();
                }
            }
        }
    }

    #[test]
    fn download_limit_rejects_declared_and_streamed_oversize_responses() {
        for framing in [
            "Content-Length: 65\r\n",
            "Transfer-Encoding: chunked\r\n",
            "",
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = url::Url::parse(&format!(
                "http://{}/metadata",
                listener.local_addr().unwrap()
            ))
            .unwrap();
            let server = thread::spawn(move || {
                let mut stream = BufReader::new(accept(&listener));
                request(&mut stream).unwrap();
                let body = if framing.contains("chunked") {
                    format!("41\r\n{}\r\n0\r\n\r\n", "x".repeat(65))
                } else {
                    "x".repeat(65)
                };
                stream
                    .get_mut()
                    .write_all(
                        format!("HTTP/1.1 200 OK\r\n{framing}Connection: close\r\n\r\n{body}")
                            .as_bytes(),
                    )
                    .unwrap();
            });
            let mut main = ini::Properties::new();
            main.insert("proxy", "_none_");
            let transport = Transport { main };
            let dir = tempfile::tempdir().unwrap();
            let dest = dir.path().join("download");
            let error = super::super::fetch(&url, &dest, Some(&transport), 64).unwrap_err();
            assert!(error.contains("64-byte limit"), "{error}");
            if framing.starts_with("Content-Length") {
                assert!(!dest.exists());
            } else {
                assert_eq!(std::fs::metadata(&dest).unwrap().len(), 64);
            }
            server.join().unwrap();
        }
    }

    #[test]
    fn configured_ca_is_required_for_private_https_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("dnf.conf");
        for trusted in [false, true] {
            let ca = if trusted {
                format!("sslcacert={}\n", fixture("ca.pem").display())
            } else {
                String::new()
            };
            std::fs::write(&config, format!("[main]\nproxy=_none_\n{ca}")).unwrap();
            let transport =
                Transport::load(&[&dir.path().join("absent-yum.conf"), &config]).unwrap();
            let (url, server) = tls_server();
            let result = super::super::fetch(
                &url,
                &dir.path().join("result"),
                Some(&transport),
                super::super::INDEX_LIMIT,
            );
            assert_eq!(result.is_ok(), trusted, "{result:?}");
            assert_eq!(server.join().unwrap(), trusted);
        }
    }

    #[test]
    fn system_trust_store_accepts_private_ca() {
        const CHILD: &str = "ANOLISA_TEST_RPM_SYSTEM_CA";
        if std::env::var_os(CHILD).is_none() {
            let dir = tempfile::tempdir().unwrap();
            let result = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "rpm_metadata::transport::tests::system_trust_store_accepts_private_ca",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .env("SSL_CERT_FILE", fixture("ca.pem"))
                .env("SSL_CERT_DIR", dir.path())
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}{}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
            return;
        }
        let mut main = ini::Properties::new();
        main.insert("proxy", "_none_");
        let transport = Transport { main };
        let (url, server) = tls_server();
        let dir = tempfile::tempdir().unwrap();
        super::super::fetch(
            &url,
            &dir.path().join("result"),
            Some(&transport),
            super::super::INDEX_LIMIT,
        )
        .unwrap();
        assert!(server.join().unwrap());
    }

    #[test]
    fn environment_proxy_and_no_proxy_are_honored() {
        const CHILD: &str = "ANOLISA_TEST_RPM_ENV_PROXY";
        if std::env::var_os(CHILD).is_none() {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let proxy = format!("http://{}", listener.local_addr().unwrap());
            let server = thread::spawn(move || {
                let mut stream = BufReader::new(accept(&listener));
                assert!(
                    request(&mut stream)
                        .unwrap()
                        .starts_with("GET http://repository.invalid/metadata ")
                );
                stream
                    .get_mut()
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .unwrap();
            });
            let result = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "rpm_metadata::transport::tests::environment_proxy_and_no_proxy_are_honored",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .env("http_proxy", proxy)
                .env("no_proxy", "127.0.0.1")
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}{}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
            server.join().unwrap();
            return;
        }
        let transport = Transport {
            main: ini::Properties::new(),
        };
        let url = url::Url::parse("http://repository.invalid/metadata").unwrap();
        transport
            .agent(&url)
            .unwrap()
            .get(url.as_str())
            .call()
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = url::Url::parse(&format!(
            "http://{}/metadata",
            listener.local_addr().unwrap()
        ))
        .unwrap();
        let server = thread::spawn(move || {
            let mut stream = BufReader::new(accept(&listener));
            assert!(request(&mut stream).unwrap().starts_with("GET /metadata "));
            stream
                .get_mut()
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .unwrap();
        });
        transport
            .agent(&url)
            .unwrap()
            .get(url.as_str())
            .call()
            .unwrap();
        server.join().unwrap();
    }
}
