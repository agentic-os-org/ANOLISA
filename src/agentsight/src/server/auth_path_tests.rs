use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;

use actix_web::http::StatusCode;
use actix_web::{App, HttpResponse, test as awtest, web};

use super::{AuthMiddleware, DashboardAuth};
use crate::config::ServerAuthConfig;

const CONTAINMENT_ROUTE: &str = "/api/audit/cases/{case_id}/contain";
const CANONICAL: &str = "/api/audit/cases/case-1/contain";
const ENCODED_API: &str = "/%61pi/audit/cases/case-1/contain";
const ENCODED_CONTAIN: &str = "/api/audit/cases/case-1/%63ontain";
const REVIEW_ROUTE: &str = "/api/audit/cases/{case_id}/review";
const REVIEW: &str = "/api/audit/cases/case-1/review";

#[actix_web::test]
async fn containment_mutation_variants_fail_closed_without_credentials() {
    let fixture = AuthFixture::new();
    let app = awtest::init_service(
        App::new()
            .wrap(AuthMiddleware::new(Arc::clone(&fixture.auth)))
            .route(
                CONTAINMENT_ROUTE,
                web::post().to(|| async { HttpResponse::Ok().finish() }),
            ),
    )
    .await;
    let loopback = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 12345);

    for uri in [
        CANONICAL,
        ENCODED_API,
        ENCODED_CONTAIN,
        "/api/audit/cases/case-1/co%6Etain",
        "/api/audit/cases/case-1/co%6etain",
        "/api/audit/cases/case-1/contain/",
        "/%2561pi/audit/cases/case-1/contain",
        "/api/audit/cases/case-1/%",
    ] {
        let response = awtest::call_service(
            &app,
            awtest::TestRequest::post()
                .uri(uri)
                .peer_addr(loopback)
                .to_request(),
        )
        .await;
        assert_ne!(response.status(), StatusCode::OK, "{uri} bypassed auth");
    }
}

#[actix_web::test]
async fn case_review_mutation_fails_closed_without_credentials() {
    let fixture = AuthFixture::new();
    let app = awtest::init_service(
        App::new()
            .wrap(AuthMiddleware::new(Arc::clone(&fixture.auth)))
            .route(
                REVIEW_ROUTE,
                web::post().to(|| async { HttpResponse::Ok().finish() }),
            ),
    )
    .await;
    let loopback = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 12345);

    let response = awtest::call_service(
        &app,
        awtest::TestRequest::post()
            .uri(REVIEW)
            .peer_addr(loopback)
            .to_request(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[actix_web::test]
async fn authenticated_encoded_mutations_only_reach_matching_routes() {
    let fixture = AuthFixture::new();
    let token = fixture.auth.token().unwrap_or_default().to_string();
    let app = awtest::init_service(
        App::new()
            .wrap(AuthMiddleware::new(Arc::clone(&fixture.auth)))
            .route(
                CONTAINMENT_ROUTE,
                web::post().to(|| async { HttpResponse::Ok().finish() }),
            ),
    )
    .await;
    let loopback = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 12345);

    for uri in [CANONICAL, ENCODED_API, ENCODED_CONTAIN] {
        let response = awtest::call_service(
            &app,
            awtest::TestRequest::post()
                .uri(uri)
                .peer_addr(loopback)
                .insert_header(("Authorization", format!("Bearer {token}")))
                .to_request(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK, "{uri} did not route");
    }

    for uri in [
        "/api/audit/cases/case-1/contain/",
        "/%2561pi/audit/cases/case-1/contain",
        "/api/audit/cases/case-1/%",
    ] {
        let response = awtest::call_service(
            &app,
            awtest::TestRequest::post()
                .uri(uri)
                .peer_addr(loopback)
                .insert_header(("Authorization", format!("Bearer {token}")))
                .to_request(),
        )
        .await;
        assert_ne!(
            response.status(),
            StatusCode::OK,
            "{uri} unexpectedly routed"
        );
    }
}

#[actix_web::test]
async fn remote_encoded_api_paths_share_canonical_auth_classification() {
    let fixture = AuthFixture::new();
    let app = awtest::init_service(
        App::new()
            .wrap(AuthMiddleware::new(Arc::clone(&fixture.auth)))
            .default_service(web::to(|| async { HttpResponse::Ok().finish() })),
    )
    .await;
    let remote = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10)), 12345);

    for uri in ["/%61pi/unknown", "/api%2Funknown", "/api/unknown/"] {
        let response = awtest::call_service(
            &app,
            awtest::TestRequest::get()
                .uri(uri)
                .peer_addr(remote)
                .to_request(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{uri}");
    }
}

struct AuthFixture {
    auth: Arc<DashboardAuth>,
    directory: std::path::PathBuf,
}

impl AuthFixture {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!("auth-path-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).expect("auth fixture directory should exist");
        let auth = Arc::new(DashboardAuth::init(
            &ServerAuthConfig { enabled: true },
            Path::new(&directory),
        ));
        Self { auth, directory }
    }
}

impl Drop for AuthFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}
