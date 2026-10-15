//! HTTP handlers for SQLite lifecycle status and the storage settings panel.

use actix_web::{HttpResponse, Responder, get, post, web};
use serde::{Deserialize, Serialize};

use super::AppState;
use crate::config::StorageConfig;
use crate::database::DatabaseManager;
use crate::storage_budget::write_total_size_limit;

/// Returns effective storage policies and current SQLite allocation.
#[get("/storage/status")]
pub async fn get_storage_status(
    state: web::Data<AppState>,
    config: web::Data<StorageConfig>,
    manager: Option<web::Data<DatabaseManager>>,
) -> impl Responder {
    HttpResponse::Ok().json(crate::storage_status::collect_storage_status(
        &state.storage_path,
        &config,
        manager.as_ref().map(|manager| manager.get_ref()),
        state.storage_budget.cap_mb(),
    ))
}

/// Request body for updating the combined SQLite storage limit.
#[derive(Debug, Deserialize)]
pub struct StorageConfigRequest {
    /// Combined limit in MiB; zero disables it, otherwise the minimum is 9.
    pub max_total_size_mb: u64,
}

/// Echo of the persisted storage setting.
#[derive(Debug, Serialize)]
struct StorageConfigResponse {
    max_total_size_mb: u64,
}

/// Persists the combined storage limit into the runtime configuration file.
///
/// Maintenance jobs re-read the value before every run, so the new limit
/// takes effect without restarting trace or serve processes.
#[post("/storage/config")]
pub async fn post_storage_config(
    state: web::Data<AppState>,
    body: web::Json<StorageConfigRequest>,
) -> impl Responder {
    if let Err(error) = StorageConfig::validate_total_size_mb(body.max_total_size_mb) {
        return HttpResponse::BadRequest().body(error);
    }
    let Some(config_path) = state.config_path.as_deref() else {
        return HttpResponse::ServiceUnavailable()
            .body("configuration file unavailable: server started without --config");
    };
    if let Err(error) = write_total_size_limit(config_path, body.max_total_size_mb) {
        log::warn!("Failed to persist storage limit to {config_path:?}: {error}");
        return HttpResponse::InternalServerError()
            .body(format!("failed to persist configuration: {error}"));
    }
    HttpResponse::Ok().json(StorageConfigResponse {
        max_total_size_mb: body.max_total_size_mb,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_config_path(tag: &str) -> std::path::PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "agentsight-storage-config-{tag}-{}-{nonce}.json",
            std::process::id()
        ))
    }

    #[test]
    fn merge_preserves_other_sections_and_survives_reload() {
        let path = unique_config_path("merge");
        std::fs::write(
            &path,
            r#"{"schema_version": 4, "features": {"audit": true}, "storage": {"primary": {"retention_days": 7}}}"#,
        )
        .unwrap();

        write_total_size_limit(&path, 1234).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(value["storage"]["max_total_size_mb"], 1234);
        assert_eq!(value["storage"]["primary"]["retention_days"], 7);
        assert_eq!(value["features"]["audit"], true);
        assert_eq!(value["schema_version"], 4);

        // The budget probe must observe the persisted value.
        let storage = StorageConfig {
            max_total_size_mb: 1,
            ..StorageConfig::default()
        };
        let budget = crate::storage_budget::StorageBudget::new(Some(path.clone()), &storage);
        assert_eq!(budget.cap_mb(), 1234);

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn merge_creates_missing_file_with_schema_version() {
        let path = unique_config_path("create");
        write_total_size_limit(&path, 900).unwrap();
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["storage"]["max_total_size_mb"], 900);
        assert_eq!(
            value["schema_version"],
            crate::config::CURRENT_SCHEMA_VERSION
        );
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn merge_rejects_non_json_config() {
        let path = unique_config_path("invalid");
        std::fs::write(&path, "{ not json").unwrap();
        assert!(write_total_size_limit(&path, 100).is_err());
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn merge_rejects_unsafe_nonzero_limit() {
        let path = unique_config_path("too-small");
        let error = write_total_size_limit(&path, 8).unwrap_err();
        assert!(error.contains("must be zero or at least 9"));
        assert!(!path.exists());
    }
}
