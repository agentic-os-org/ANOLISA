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
/// Fail closed: an undecodable `Origin`, the `null` origin (sandboxed frames),
/// or a missing `Host` all count as cross-origin. A request without an
/// `Origin` header is not a browser call and is never cross-origin.
#[must_use]
pub fn is_cross_origin(origin: Option<&HeaderValue>, host: Option<&HeaderValue>) -> bool {
    let Some(origin) = origin else {
        return false;
    };
    let Some(origin) = origin.to_str().ok() else {
        return true;
    };
    let Some(origin_authority) = origin_authority(origin) else {
        return true;
    };
    let Some(host) = host.and_then(|host| host.to_str().ok()) else {
        return true;
    };
    normalize_authority(origin_authority) != normalize_authority(host)
}

/// The authority (host[:port]) named by an `Origin` header value.
///
/// Returns `None` for values without a scheme, including the serialized
/// `null` origin, so callers fail closed on them.
fn origin_authority(origin: &str) -> Option<&str> {
    let rest = origin.split_once("://")?.1;
    Some(rest.split(['/', '?', '#']).next().unwrap_or(rest))
}

/// Normalizes one authority for comparison: trims surrounding whitespace,
/// drops default HTTP/HTTPS ports, and lowercases (the port is digits, so
/// lowercasing the whole authority only affects the host).
fn normalize_authority(authority: &str) -> String {
    let authority = authority.trim();
    let authority = authority.strip_suffix(":80").unwrap_or(authority);
    let authority = authority.strip_suffix(":443").unwrap_or(authority);
    authority.to_ascii_lowercase()
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
    fn origin_authority_extracts_host_and_port() {
        assert_eq!(origin_authority("https://example.com"), Some("example.com"));
        assert_eq!(
            origin_authority("http://127.0.0.1:7396/api/x?q=1"),
            Some("127.0.0.1:7396")
        );
        assert_eq!(origin_authority("http://[::1]:7396"), Some("[::1]:7396"));
        // The serialized null origin, and values without a scheme, have no
        // authority: callers fail closed on them.
        assert_eq!(origin_authority("null"), None);
        assert_eq!(origin_authority("example.com"), None);
        assert_eq!(origin_authority(""), None);
    }

    #[test]
    fn authority_normalization_ignores_case_and_default_ports() {
        assert_eq!(normalize_authority("Example.COM"), "example.com");
        assert_eq!(normalize_authority("example.com:80"), "example.com");
        assert_eq!(normalize_authority("example.com:443"), "example.com");
        assert_eq!(normalize_authority("example.com:7396"), "example.com:7396");
        assert_eq!(normalize_authority("[::1]:7396"), "[::1]:7396");
    }

    #[test]
    fn cross_origin_classification_fails_closed() {
        // No Origin header: not a browser call.
        assert!(!is_cross_origin(None, Some(&header("127.0.0.1:7396"))));
        // Same authority in the usual spellings: not cross-origin.
        assert!(!is_cross_origin(
            Some(&header("http://127.0.0.1:7396")),
            Some(&header("127.0.0.1:7396"))
        ));
        assert!(!is_cross_origin(
            Some(&header("http://localhost")),
            Some(&header("LOCALHOST:80"))
        ));
        // A different host, port, scheme-carrying host, the null origin, a
        // missing Host, or an undecodable Origin all count as cross-origin.
        assert!(is_cross_origin(
            Some(&header("https://attacker.example")),
            Some(&header("127.0.0.1:7396"))
        ));
        assert!(is_cross_origin(
            Some(&header("http://127.0.0.1:8080")),
            Some(&header("127.0.0.1:7396"))
        ));
        assert!(is_cross_origin(
            Some(&header("null")),
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
