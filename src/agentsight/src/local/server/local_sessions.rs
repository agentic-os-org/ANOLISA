//! Local agent session discovery and ATIF conversion handlers
//!
//! These endpoints scan well-known directories on the local machine for agent
//! session files (Claude Code, Qoder, QoderWork, Codex, Cursor) and convert
//! them to ATIF format for trajectory display. They work without eBPF and
//! without a populated SQLite database — pure filesystem scanning.

use actix_web::{HttpResponse, Responder, get, web};
use serde::Deserialize;

use crate::local::collector::{
    LocalSessionsResponse, convert_jsonl_to_atif, discover_local_sessions,
};

/// List all discovered local agent session files.
#[get("/api/local-sessions")]
pub async fn list_local_sessions() -> impl Responder {
    let sessions = web::block(discover_local_sessions).await;

    match sessions {
        Ok(sessions) => {
            let total = sessions.len();
            let scanned_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let response = LocalSessionsResponse {
                sessions,
                total,
                scanned_at,
            };
            HttpResponse::Ok().json(response)
        }
        Err(e) => {
            log::error!("Failed to discover local sessions: {}", e);
            HttpResponse::InternalServerError().body(format!("Discovery failed: {}", e))
        }
    }
}

/// Query parameters for ATIF conversion
#[derive(Deserialize)]
pub struct AtifQuery {
    /// Absolute path to the JSONL session file
    pub path: String,
}

/// Convert a local JSONL session file to an ATIF document.
///
/// Query parameter: `path` — absolute path to the `.jsonl` file
#[get("/api/local-session/atif")]
pub async fn convert_local_to_atif(query: web::Query<AtifQuery>) -> impl Responder {
    let path = query.path.clone();

    if !is_safe_session_path(&path) {
        return HttpResponse::BadRequest().body(
            "Invalid file path: must be a .jsonl/.json file under a known session directory",
        );
    }

    let path_for_log = path.clone();
    let result = web::block(move || convert_jsonl_to_atif(std::path::Path::new(&path))).await;

    match result {
        Ok(Ok(doc)) => HttpResponse::Ok().json(doc),
        Ok(Err(e)) => {
            log::error!("Failed to convert {} to ATIF: {}", path_for_log, e);
            HttpResponse::InternalServerError().body(format!("Conversion failed: {}", e))
        }
        Err(e) => {
            log::error!("Thread pool error converting {}: {}", path_for_log, e);
            HttpResponse::InternalServerError().body(format!("Internal error: {}", e))
        }
    }
}

/// Query parameters for file reading
#[derive(Deserialize)]
pub struct FileQuery {
    /// Absolute path to the JSONL session file
    pub path: String,
}

/// Read the raw content of a local session file.
///
/// Query parameter: `path` — absolute path to the `.jsonl` file
#[get("/api/local-session/file")]
pub async fn read_local_session_file(query: web::Query<FileQuery>) -> impl Responder {
    let path = query.path.as_str();

    if !is_safe_session_path(path) {
        return HttpResponse::BadRequest().body(
            "Invalid file path: must be a .jsonl/.json file under a known session directory",
        );
    }

    // Block on file I/O to avoid blocking the async runtime.
    let path_owned = path.to_string();
    match web::block(move || std::fs::read_to_string(&path_owned)).await {
        Ok(Ok(content)) => HttpResponse::Ok()
            .content_type("application/jsonl; charset=utf-8")
            .body(content),
        Ok(Err(e)) => {
            log::error!("Failed to read {}: {}", path, e);
            HttpResponse::NotFound().body(format!("File not found: {}", e))
        }
        Err(e) => {
            log::error!("File read task failed: {}", e);
            HttpResponse::InternalServerError().body("Internal error")
        }
    }
}

/// Validate that the path is a .jsonl/.json file under a known session directory.
///
/// Canonicalizes the path and ensures it starts with the user's home directory
/// joined with one of the known session root subdirs (e.g. `.claude/projects`).
fn is_safe_session_path(path: &str) -> bool {
    let p = std::path::Path::new(path);

    // Must have .jsonl or .json extension
    if !p
        .extension()
        .is_some_and(|ext| ext == "jsonl" || ext == "json")
    {
        return false;
    }

    // Canonicalize to resolve symlinks and `..` traversal
    let canonical = match p.canonicalize() {
        Ok(c) => c,
        Err(_) => return false,
    };

    // Must be under $HOME/<known-session-root>
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return false,
    };

    let known_roots = [
        ".claude/projects",
        ".qoder/projects",
        ".qoderwork/projects",
        ".codex/sessions",
        ".codex/archived_sessions",
        ".cursor/projects",
    ];

    for root in &known_roots {
        let base = home.join(root);
        if canonical.starts_with(&base) {
            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_safe_session_path_rejects_bad_ext() {
        assert!(!is_safe_session_path("/tmp/session.txt"));
        assert!(!is_safe_session_path("/tmp/session.csv"));
        assert!(!is_safe_session_path("/tmp/noext"));
        assert!(!is_safe_session_path("/tmp/session"));
        assert!(!is_safe_session_path(""));
    }

    #[test]
    fn test_is_safe_session_path_rejects_non_session_dir() {
        // /tmp is not under any known session root
        assert!(!is_safe_session_path("/tmp/session.jsonl"));
        assert!(!is_safe_session_path("/etc/passwd.json"));
    }

    #[test]
    fn test_is_safe_session_path_rejects_relative() {
        assert!(!is_safe_session_path("relative/path.jsonl"));
        assert!(!is_safe_session_path("../../etc/secrets.jsonl"));
    }

    #[actix_web::test]
    async fn test_list_local_sessions_endpoint() {
        let app =
            actix_web::test::init_service(actix_web::App::new().service(list_local_sessions)).await;
        let req = actix_web::test::TestRequest::get()
            .uri("/api/local-sessions")
            .to_request();
        let resp = actix_web::test::call_service(&app, req).await;
        assert!(resp.status().is_success());
    }

    #[actix_web::test]
    async fn test_convert_local_to_atif_bad_path() {
        let app =
            actix_web::test::init_service(actix_web::App::new().service(convert_local_to_atif))
                .await;
        let req = actix_web::test::TestRequest::get()
            .uri("/api/local-session/atif?path=/tmp/notjson.txt")
            .to_request();
        let resp = actix_web::test::call_service(&app, req).await;
        assert_eq!(resp.status(), actix_web::http::StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn test_read_local_session_file_not_found() {
        let app =
            actix_web::test::init_service(actix_web::App::new().service(read_local_session_file))
                .await;
        let req = actix_web::test::TestRequest::get()
            .uri("/api/local-session/file?path=/tmp/nonexistent_file.jsonl")
            .to_request();
        let resp = actix_web::test::call_service(&app, req).await;
        // /tmp is not under a known session root → BadRequest (not NotFound)
        assert_eq!(resp.status(), actix_web::http::StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn test_read_local_session_file_bad_ext() {
        let app =
            actix_web::test::init_service(actix_web::App::new().service(read_local_session_file))
                .await;
        let req = actix_web::test::TestRequest::get()
            .uri("/api/local-session/file?path=/tmp/file.exe")
            .to_request();
        let resp = actix_web::test::call_service(&app, req).await;
        assert_eq!(resp.status(), actix_web::http::StatusCode::BAD_REQUEST);
    }
}
