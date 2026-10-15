//! Cross-origin (browser) request classification shared by both HTTP servers.
//!
//! Both the Linux serve path and the local viewer bind to loopback by default
//! and answer permissive CORS, so a visited web page can `fetch()` them as
//! easily as a local process can — a loopback address is not an authorization
//! boundary for web content. The helpers here recognize browser cross-origin
//! calls by their `Origin` header (browsers always attach one to cross-origin
//! requests; CLI tools and same-origin dashboard calls never carry a *foreign*
//! one) so each server can keep its local-caller conveniences without also
//! becoming a cross-origin data source.

use actix_web::body::EitherBody;
use actix_web::dev::{RequestHead, ServiceRequest, Transform};
use actix_web::http::header::{HOST, HeaderValue, ORIGIN};
use actix_web::{Error, HttpMessage, HttpResponse};
use std::future::{Ready, ready};
use std::pin::Pin;
use std::task::{Context, Poll};

/// Whether a request carrying these header values is a browser cross-origin
/// call to this server.
///
/// Fail closed: an undecodable `Origin`, a non-HTTP(S) scheme, the `null`
/// origin (sandboxed frames), a missing `Host`, or a `Host` that does not
/// name this server's own loopback endpoint all count as cross-origin. A
/// request without an `Origin` header is not a browser call and is never
/// cross-origin.
///
/// Matching alone is not trust: a DNS-rebound page sends an `Origin` and a
/// `Host` that both name the attacker's domain, so the `Host` itself must be
/// a loopback authority (localhost, 127.0.0.0/8, [::1]) — a name this server
/// answers on and an attacker's public name never is. The comparison then
/// uses the complete externally visible endpoint: the origin's scheme
/// defaults its effective port, and a `Host` without a port can only match
/// the origin scheme's default port, because a real same-origin browser call
/// always names a non-default server port explicitly.
#[must_use]
pub fn is_cross_origin(origin: Option<&HeaderValue>, host: Option<&HeaderValue>) -> bool {
    let Some(origin) = origin else {
        return false;
    };
    let Some(origin) = origin.to_str().ok() else {
        return true;
    };
    let Some((scheme, origin_authority)) = origin_endpoint(origin) else {
        return true;
    };
    let Some(host) = host.and_then(|host| host.to_str().ok()) else {
        return true;
    };
    let (host_name, host_port) = split_authority(host.trim());
    if !is_loopback_name(host_name) {
        return true;
    }
    let (origin_host, origin_port) = split_authority(origin_authority);
    let origin_effective_port = origin_port.unwrap_or(if scheme == "https" { 443 } else { 80 });
    let same_endpoint = match host_port {
        Some(host_port) => host_port == origin_effective_port,
        // A port-less Host can only match the origin scheme's default port.
        None => origin_effective_port == if scheme == "https" { 443 } else { 80 },
    };
    !(same_endpoint && origin_host.eq_ignore_ascii_case(host_name))
}

/// The scheme and authority named by an `Origin` header value.
///
/// Returns `None` for values without a scheme, non-HTTP(S) schemes, and the
/// serialized `null` origin, so callers fail closed on them.
fn origin_endpoint(origin: &str) -> Option<(&str, &str)> {
    let (scheme, rest) = origin.split_once("://")?;
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    Some((scheme, authority))
}

/// Splits one authority (an `Origin` value or `Host` header) into its host
/// and port. The port is `None` when absent or not a bare decimal below
/// 65536; a bracketed IPv6 literal keeps its brackets.
fn split_authority(authority: &str) -> (&str, Option<u16>) {
    match authority.rsplit_once(':') {
        Some((host, port))
            if port.parse::<u16>().is_ok()
                && (host.starts_with('[') || !host.contains(':')) =>
        {
            (host, port.parse().ok())
        }
        _ => (authority, None),
    }
}

/// Whether a host names this server's loopback interface.
fn is_loopback_name(host: &str) -> bool {
    let host = host.strip_prefix('[').unwrap_or(host);
    let host = host.strip_suffix(']').unwrap_or(host);
    if host.eq_ignore_ascii_case("localhost") || host == "::1" {
        return true;
    }
    host.strip_prefix("127.")
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit() || b == b'.'))
}

/// Whether one live request is a browser cross-origin call.
pub fn request_is_cross_origin<T>(req: &T) -> bool
where
    T: HttpMessage,
{
    let headers = req.headers();
    is_cross_origin(headers.get(&ORIGIN), headers.get(&HOST))
}

/// actix-cors predicate: allow an origin only when it names this server
/// itself, so a visited web page cannot read responses cross-origin.
///
/// The closure shape is `Fn(&HeaderValue, &RequestHead)` — actix-cors passes
/// the request's `Origin` header value first and only calls this predicate
/// when the header is present.
#[must_use]
pub fn cors_allows_origin(origin: &HeaderValue, req: &RequestHead) -> bool {
    !is_cross_origin(Some(origin), req.headers().get(&HOST))
}

/// Rejects browser cross-origin calls before any endpoint runs.
///
/// The local viewer has no token layer; this gate is what stops a visited web
/// page from reading the served data through the browser. Requests that are
/// not browser cross-origin calls (CLI tools, the embedded dashboard's own
/// same-origin calls) pass through untouched.
pub struct CrossOriginGate;

impl<S, B> Transform<S, ServiceRequest> for CrossOriginGate
where
    S: actix_web::dev::Service<
            ServiceRequest,
            Response = actix_web::dev::ServiceResponse<B>,
            Error = Error,
        > + 'static,
    B: 'static,
{
    type Response = actix_web::dev::ServiceResponse<EitherBody<B>>;
    type Error = Error;
    type Transform = CrossOriginGateService<S>;
    type InitError = ();
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ready(Ok(CrossOriginGateService { service }))
    }
}

/// The middleware service created by [`CrossOriginGate`].
///
/// Wraps the inner service so that every request classified as a browser
/// cross-origin call by [`request_is_cross_origin`] is answered with a 403
/// JSON error before any endpoint runs; all other requests (CLI tools,
/// the dashboard's own same-origin calls) reach the inner service unchanged.
pub struct CrossOriginGateService<S> {
    service: S,
}

impl<S, B> actix_web::dev::Service<ServiceRequest> for CrossOriginGateService<S>
where
    S: actix_web::dev::Service<
            ServiceRequest,
            Response = actix_web::dev::ServiceResponse<B>,
            Error = Error,
        > + 'static,
    B: 'static,
{
    type Response = actix_web::dev::ServiceResponse<EitherBody<B>>;
    type Error = Error;
    type Future = Pin<Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>>>>;

    fn poll_ready(&self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&self, req: ServiceRequest) -> Self::Future {
        if request_is_cross_origin(&req) {
            let response = req.into_response(
                HttpResponse::Forbidden()
                    .json(serde_json::json!({
                        "error": "cross_origin_forbidden",
                        "message": "cross-origin browser requests are not accepted"
                    }))
                    .map_into_right_body(),
            );
            return Box::pin(async move { Ok(response) });
        }
        let fut = self.service.call(req);
        Box::pin(async move { fut.await.map(|res| res.map_into_left_body()) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::test as awtest;
    use actix_web::{App, HttpResponse, web};

    fn header(value: &str) -> HeaderValue {
        HeaderValue::from_str(value).expect("test header value")
    }

    #[test]
    fn origin_endpoint_keeps_the_scheme_and_rejects_non_http_schemes() {
        assert_eq!(
            origin_endpoint("https://example.com"),
            Some(("https", "example.com"))
        );
        assert_eq!(
            origin_endpoint("http://127.0.0.1:7396/api/x?q=1"),
            Some(("http", "127.0.0.1:7396"))
        );
        // The serialized null origin, values without a scheme, and non-web
        // schemes have no trusted endpoint: callers fail closed on them.
        assert_eq!(origin_endpoint("null"), None);
        assert_eq!(origin_endpoint("example.com"), None);
        assert_eq!(origin_endpoint(""), None);
        assert_eq!(origin_endpoint("file://host"), None);
        assert_eq!(origin_endpoint("ftp://host"), None);
    }

    #[test]
    fn authority_split_handles_ports_and_ipv6_literals() {
        assert_eq!(split_authority("example.com"), ("example.com", None));
        assert_eq!(split_authority("example.com:8080"), ("example.com", Some(8080)));
        assert_eq!(split_authority("[::1]:7396"), ("[::1]", Some(7396)));
        // A bare IPv6 literal has no port suffix.
        assert_eq!(split_authority("::1"), ("::1", None));
    }

    #[test]
    fn loopback_name_recognizes_only_local_spellings() {
        assert!(is_loopback_name("localhost"));
        assert!(is_loopback_name("LOCALHOST"));
        assert!(is_loopback_name("127.0.0.1"));
        assert!(is_loopback_name("127.1.2.3"));
        assert!(is_loopback_name("[::1]"));
        assert!(is_loopback_name("::1"));
        assert!(!is_loopback_name("evil.example"));
        assert!(!is_loopback_name("127"));
        assert!(!is_loopback_name("1270.0.0.1"));
        assert!(!is_loopback_name("localhost.evil.example"));
    }

    #[test]
    fn cross_origin_classification_fails_closed() {
        // No Origin header: not a browser call.
        assert!(!is_cross_origin(None, Some(&header("127.0.0.1:7396"))));
        // Same loopback endpoint in the usual spellings: not cross-origin.
        assert!(!is_cross_origin(
            Some(&header("http://127.0.0.1:7396")),
            Some(&header("127.0.0.1:7396"))
        ));
        assert!(!is_cross_origin(
            Some(&header("http://localhost")),
            Some(&header("LOCALHOST"))
        ));
        assert!(!is_cross_origin(
            Some(&header("http://[::1]:7396")),
            Some(&header("[::1]:7396"))
        ));
        // A DNS-rebound page sends Origin and Host naming its own domain:
        // matching each other is not trust — the Host must be loopback.
        assert!(is_cross_origin(
            Some(&header("http://evil.example:7396")),
            Some(&header("evil.example:7396"))
        ));
        // A loopback server reached through a same-host reverse proxy keeps
        // its public name in the Host: not a loopback endpoint, refused.
        assert!(is_cross_origin(
            Some(&header("http://dash.example.com")),
            Some(&header("dash.example.com"))
        ));
        // A different host, a different port, the null origin, a non-web
        // scheme, a missing Host, or an undecodable Origin are cross-origin.
        assert!(is_cross_origin(
            Some(&header("https://attacker.example")),
            Some(&header("127.0.0.1:7396"))
        ));
        assert!(is_cross_origin(
            Some(&header("http://127.0.0.1:8080")),
            Some(&header("127.0.0.1:7396"))
        ));
        // A port-less Host can only match the origin scheme's default port:
        // a same-origin browser call names the server's port explicitly.
        assert!(is_cross_origin(
            Some(&header("http://127.0.0.1:7396")),
            Some(&header("127.0.0.1"))
        ));
        assert!(is_cross_origin(
            Some(&header("null")),
            Some(&header("127.0.0.1:7396"))
        ));
        assert!(is_cross_origin(
            Some(&header("file://127.0.0.1:7396")),
            Some(&header("127.0.0.1:7396"))
        ));
        assert!(is_cross_origin(
            Some(&header("https://attacker.example")),
            None
        ));
        assert!(is_cross_origin(
            Some(&HeaderValue::from_bytes(&[0xff, 0xfe]).unwrap()),
            Some(&header("127.0.0.1:7396"))
        ));
    }

    #[actix_web::test]
    async fn gate_rejects_foreign_origins_and_passes_local_callers() {
        let app = awtest::init_service(App::new().wrap(CrossOriginGate).route(
            "/api/data",
            web::get().to(|| async { HttpResponse::Ok().body("ok") }),
        ))
        .await;

        // A CLI-style request without an Origin header passes.
        let local = awtest::call_service(
            &app,
            awtest::TestRequest::get()
                .uri("/api/data")
                .insert_header(("Host", "127.0.0.1:7396"))
                .to_request(),
        )
        .await;
        assert_eq!(local.status(), 200);

        // The dashboard's own same-origin calls pass.
        let same_origin = awtest::call_service(
            &app,
            awtest::TestRequest::get()
                .uri("/api/data")
                .insert_header(("Host", "127.0.0.1:7396"))
                .insert_header(("Origin", "http://127.0.0.1:7396"))
                .to_request(),
        )
        .await;
        assert_eq!(same_origin.status(), 200);

        // A foreign web page is refused before the endpoint runs.
        let foreign = awtest::call_service(
            &app,
            awtest::TestRequest::get()
                .uri("/api/data")
                .insert_header(("Host", "127.0.0.1:7396"))
                .insert_header(("Origin", "https://attacker.example"))
                .to_request(),
        )
        .await;
        assert_eq!(foreign.status(), 403);
    }

    #[actix_web::test]
    async fn cors_predicate_allows_only_this_servers_origin() {
        let head = |host: Option<&str>| {
            let mut req = RequestHead::default();
            if let Some(host) = host {
                req.headers_mut().insert(HOST, header(host));
            }
            req
        };
        assert!(cors_allows_origin(
            &header("http://127.0.0.1:7396"),
            &head(Some("127.0.0.1:7396"))
        ));
        assert!(!cors_allows_origin(
            &header("https://attacker.example"),
            &head(Some("127.0.0.1:7396"))
        ));
        assert!(!cors_allows_origin(
            &header("https://attacker.example"),
            &head(None)
        ));
    }
}
