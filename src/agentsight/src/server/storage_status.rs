//! HTTP handler for read-only SQLite lifecycle status.

use actix_web::{HttpResponse, Responder, get, web};

use super::AppState;
use crate::config::StorageConfig;

/// Returns effective storage policies and current SQLite allocation.
#[get("/storage/status")]
pub async fn get_storage_status(
    state: web::Data<AppState>,
    config: web::Data<StorageConfig>,
) -> impl Responder {
    HttpResponse::Ok().json(crate::storage_status::collect_storage_status(
        &state.storage_path,
        &config,
    ))
}
