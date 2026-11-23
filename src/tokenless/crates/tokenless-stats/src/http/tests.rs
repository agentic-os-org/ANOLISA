use super::*;
use crate::{OperationType, StatsRecord, StatsRecorder};
use actix_web::{http::StatusCode, test};

#[actix_web::test]
async fn authentication_query_and_failure_boundaries() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stats.db");
    let writer = StatsRecorder::new(&path).unwrap();
    writer
        .record(
            &StatsRecord::new(OperationType::RewriteCommand, "fixture".into(), 12, 6, 4, 2)
                .with_session_id("session & value")
                .with_tool_use_id("tool' OR 1=1 --")
                .with_text("before".into(), "after".into()),
        )
        .unwrap();
    drop(writer);
    let state = web::Data::new(State {
        database: path.clone(),
        credential: blake3::hash(b"fixture-token"),
    });
    let app = test::init_service(
        App::new()
            .app_data(state)
            .app_data(web::PayloadConfig::new(MAX_QUERY_BYTES))
            .default_service(web::route().to(query)),
    )
    .await;
    for uri in [
        "/v1/stats/records",
        "/v1/stats/records/1?include_content=true",
    ] {
        for token in [None, Some("Bearer wrong"), Some("Basic fixture-token")] {
            let mut request = test::TestRequest::get().uri(uri);
            if let Some(token) = token {
                request = request.insert_header((header::AUTHORIZATION, token));
            }
            let response = test::call_service(&app, request.to_request()).await;
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            assert_eq!(
                response.headers().get(header::WWW_AUTHENTICATE).unwrap(),
                "Bearer"
            );
        }
    }
    let duplicate = test::TestRequest::get()
        .uri("/v1/stats/records")
        .append_header((header::AUTHORIZATION, "Bearer fixture-token"))
        .append_header((header::AUTHORIZATION, "Bearer fixture-token"))
        .to_request();
    assert_eq!(
        test::call_service(&app, duplicate).await.status(),
        StatusCode::UNAUTHORIZED
    );
    let request = test::TestRequest::get()
        .uri("/v1/stats/records?session_id=session%20%26%20value")
        .insert_header((header::AUTHORIZATION, "bearer fixture-token"))
        .to_request();
    let value: Value = test::call_and_read_body_json(&app, request).await;
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["records"][0]["session_id"], "session & value");
    assert_eq!(value["records"][0]["before_text"], Value::Null);
    let request = test::TestRequest::post()
        .uri("/v1/stats/records/query")
        .set_json(json!({"tool_use_ids":["tool' OR 1=1 --"],"limit":100}))
        .to_request();
    assert_eq!(
        test::call_service(&app, request).await.status(),
        StatusCode::UNAUTHORIZED
    );
    let before = std::fs::read(&path).unwrap();
    let request = test::TestRequest::post()
        .uri("/v1/stats/records/query")
        .insert_header((header::AUTHORIZATION, "Bearer fixture-token"))
        .set_json(
            json!({"tool_use_ids":["missing","tool' OR 1=1 --","tool' OR 1=1 --"],"limit":100}),
        )
        .to_request();
    let value: Value = test::call_and_read_body_json(&app, request).await;
    assert_eq!(value["records"].as_array().unwrap().len(), 1);
    assert_eq!(value["records"][0]["before_text"], Value::Null);
    assert_eq!(value["next_before_id"], Value::Null);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    for body in [
        json!({"tool_use_ids":[],"limit":100}),
        json!({"tool_use_ids":["t"],"limit":101}),
        json!({"tool_use_ids":["t"],"limit":100,"include_content":true}),
        json!({"tool_use_ids":["t"],"limit":100,"before_id":0}),
    ] {
        let request = test::TestRequest::post()
            .uri("/v1/stats/records/query")
            .insert_header((header::AUTHORIZATION, "Bearer fixture-token"))
            .set_json(body)
            .to_request();
        assert_eq!(
            test::call_service(&app, request).await.status(),
            StatusCode::BAD_REQUEST
        );
    }
    let request = test::TestRequest::post()
        .uri("/v1/stats/records/query")
        .insert_header((header::AUTHORIZATION, "Bearer fixture-token"))
        .set_payload("x".repeat(MAX_QUERY_BYTES + 1))
        .to_request();
    assert_eq!(
        test::call_service(&app, request).await.status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
    for (uri, expected) in [
        ("/v1/stats/records?limit=0", StatusCode::BAD_REQUEST),
        ("/v1/stats/records?unknown=1", StatusCode::BAD_REQUEST),
        ("/v1/stats/records/999", StatusCode::NOT_FOUND),
        (
            "/v1/stats/records/1?include_content=wrong",
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let request = test::TestRequest::get()
            .uri(uri)
            .insert_header((header::AUTHORIZATION, "Bearer fixture-token"))
            .to_request();
        assert_eq!(test::call_service(&app, request).await.status(), expected);
    }
    let request = test::TestRequest::get()
        .uri("/v1/stats/records/1?include_content=true")
        .insert_header((header::AUTHORIZATION, "Bearer fixture-token"))
        .to_request();
    let value: Value = test::call_and_read_body_json(&app, request).await;
    assert_eq!(value["record"]["before_text"], "before");
    assert_eq!(value["record"]["content_included"], true);
    let request = test::TestRequest::post()
        .uri("/v1/stats/records")
        .insert_header((header::AUTHORIZATION, "Bearer fixture-token"))
        .to_request();
    let response = test::call_service(&app, request).await;
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(response.headers().get(header::ALLOW).unwrap(), "GET, HEAD");
    let request = test::TestRequest::get()
        .uri("/v1/stats/records/query")
        .insert_header((header::AUTHORIZATION, "Bearer fixture-token"))
        .to_request();
    let response = test::call_service(&app, request).await;
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(response.headers().get(header::ALLOW).unwrap(), "POST");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute(
        "UPDATE stats SET timestamp='private malformed timestamp'",
        [],
    )
    .unwrap();
    let request = test::TestRequest::get()
        .uri("/v1/stats/records")
        .insert_header((header::AUTHORIZATION, "Bearer fixture-token"))
        .to_request();
    let response = test::call_service(&app, request).await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let value: Value = test::read_body_json(response).await;
    assert_eq!(
        value["error"],
        json!({"code":"invalid_record","record_id":1,"field":"timestamp"})
    );
    assert!(!value.to_string().contains("private malformed"));
    drop(conn);
    std::fs::remove_file(path).unwrap();
    let request = test::TestRequest::get()
        .uri("/v1/stats/records")
        .insert_header((header::AUTHORIZATION, "Bearer fixture-token"))
        .to_request();
    assert_eq!(
        test::call_service(&app, request).await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[actix_web::test]
async fn route_selection_precedes_method_and_database_checks() {
    let dir = tempfile::tempdir().unwrap();
    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(State {
                database: dir.path().join("missing.db"),
                credential: blake3::hash(b"fixture-token"),
            }))
            .default_service(web::route().to(query)),
    )
    .await;
    for uri in [
        "/totally/unknown",
        "/v1/stats/records/",
        "/v1/stats/records/query/",
        "/v1/stats/records/not-an-id",
        "/v1/stats/records/1/extra",
    ] {
        for method in [
            actix_web::http::Method::GET,
            actix_web::http::Method::HEAD,
            actix_web::http::Method::POST,
        ] {
            for authenticated in [false, true] {
                let mut request = test::TestRequest::default().method(method.clone()).uri(uri);
                if authenticated {
                    request =
                        request.insert_header((header::AUTHORIZATION, "Bearer fixture-token"));
                }
                let response = test::call_service(&app, request.to_request()).await;
                assert_eq!(
                    response.status(),
                    if authenticated {
                        StatusCode::NOT_FOUND
                    } else {
                        StatusCode::UNAUTHORIZED
                    },
                    "{method} {uri}"
                );
                assert!(!response.headers().contains_key(header::ALLOW));
            }
        }
    }
    for (uri, method, allow) in [
        (
            "/v1/stats/records",
            actix_web::http::Method::POST,
            "GET, HEAD",
        ),
        (
            "/v1/stats/records/1",
            actix_web::http::Method::POST,
            "GET, HEAD",
        ),
        (
            "/v1/stats/records/query",
            actix_web::http::Method::HEAD,
            "POST",
        ),
    ] {
        let request = test::TestRequest::default()
            .method(method)
            .uri(uri)
            .insert_header((header::AUTHORIZATION, "Bearer fixture-token"))
            .to_request();
        let response = test::call_service(&app, request).await;
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(response.headers().get(header::ALLOW).unwrap(), allow);
    }
    assert!(!dir.path().join("missing.db").exists());
}

#[actix_web::test]
async fn credential_files_and_response_limits() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("credential");
    for bytes in [
        Vec::new(),
        b"has space".to_vec(),
        vec![b'x'; 4097],
        vec![0xff],
    ] {
        std::fs::write(&path, bytes).unwrap();
        assert!(matches!(
            load_credential(&path),
            Err(ServerError::Credential)
        ));
    }
    std::fs::write(&path, b"fixture-token\n").unwrap();
    assert_eq!(
        load_credential(&path).unwrap(),
        blake3::hash(b"fixture-token")
    );
    assert_eq!(
        response(json!({"text":"x".repeat(MAX_RESPONSE_BYTES)})).status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
    assert!(load_credential(&dir.path().join("missing")).is_err());
}
