//! Query Tokenless with a service credential and save results for AgentSight views.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use actix_web::{HttpResponse, get, post, web};
use serde::Deserialize;
use serde_json::{Value, json};

use super::AppState;

const MAX_BYTES: u64 = 8 * 1024 * 1024;

/// Configured query client failures; no response includes the service credential.
#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    /// Configuration or query filters are unusable.
    #[error("invalid Tokenless query configuration or filters")]
    Invalid,
    /// Credential or response I/O failed.
    #[error("Tokenless query I/O failed: {0}")]
    Io(#[from] std::io::Error),
    /// The query endpoint returned an error or violated the bounded contract.
    #[error("Tokenless query endpoint failed or returned an unsupported response")]
    Endpoint,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    url: String,
    credential_file: PathBuf,
}

/// Backend-only credential and HTTP client; never serialized into browser responses.
pub(super) struct QueryClient {
    endpoint: String,
    credential: String,
    agent: ureq::Agent,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Filter {
    session_id: Option<String>,
    tool_use_id: Option<String>,
}

impl QueryClient {
    /// Load the optional server-only configuration and its credential file.
    ///
    /// # Errors
    /// Rejects unsafe endpoints, invalid credentials, and configuration/file failures.
    pub(super) fn from_config_path(path: Option<&Path>) -> Result<Option<Self>, QueryError> {
        let Some(path) = path else {
            return Ok(None);
        };
        if !path.try_exists()? {
            return Ok(None);
        }
        let config: Value =
            serde_json::from_slice(&std::fs::read(path)?).map_err(|_| QueryError::Invalid)?;
        let Some(config) = config.get("tokenless_api").filter(|value| !value.is_null()) else {
            return Ok(None);
        };
        let config: Config =
            serde_json::from_value(config.clone()).map_err(|_| QueryError::Invalid)?;
        let url = url::Url::parse(&config.url).map_err(|_| QueryError::Invalid)?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
            || !config.credential_file.is_absolute()
        {
            return Err(QueryError::Invalid);
        }
        let mut bytes = Vec::new();
        std::fs::File::open(config.credential_file)?
            .take(4097)
            .read_to_end(&mut bytes)?;
        let credential = std::str::from_utf8(&bytes)
            .map_err(|_| QueryError::Invalid)?
            .trim_end_matches(['\r', '\n']);
        if credential.is_empty()
            || bytes.len() > 4096
            || !credential.bytes().all(|byte| byte.is_ascii_graphic())
        {
            return Err(QueryError::Invalid);
        }
        Ok(Some(Self {
            endpoint: url.as_str().trim_end_matches('/').into(),
            credential: credential.into(),
            agent: ureq::AgentBuilder::new()
                .timeout(Duration::from_secs(5))
                .redirects(0)
                .build(),
        }))
    }

    fn request(&self, request: ureq::Request) -> Result<(Value, usize), QueryError> {
        let response = request
            .set("Authorization", &format!("Bearer {}", self.credential))
            .call()
            .map_err(|_| QueryError::Endpoint)?;
        let mut bytes = Vec::new();
        response
            .into_reader()
            .take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(QueryError::Endpoint);
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| QueryError::Endpoint)?;
        if value["schema_version"] != 1 {
            return Err(QueryError::Endpoint);
        }
        Ok((value, bytes.len()))
    }

    fn fetch(&self, filter: &Filter) -> Result<Vec<Value>, QueryError> {
        if filter.session_id.is_none() && filter.tool_use_id.is_none() {
            return Err(QueryError::Invalid);
        }
        if [&filter.session_id, &filter.tool_use_id]
            .into_iter()
            .flatten()
            .any(|value| value.is_empty() || value.len() > 1024)
        {
            return Err(QueryError::Invalid);
        }
        let mut records = Vec::new();
        let mut before_id = None;
        let mut size = 0;
        loop {
            let mut request = self
                .agent
                .get(&format!("{}/v1/stats/records", self.endpoint))
                .query("limit", "100");
            if let Some(value) = &filter.session_id {
                request = request.query("session_id", value);
            }
            if let Some(value) = &filter.tool_use_id {
                request = request.query("tool_use_id", value);
            }
            if let Some(id) = before_id {
                request = request.query("before_id", &i64::to_string(&id));
            }
            let (page, bytes) = self.request(request)?;
            size += bytes;
            let rows = page["records"].as_array().ok_or(QueryError::Endpoint)?;
            if records.len() + rows.len() > 1000 || size as u64 > MAX_BYTES {
                return Err(QueryError::Endpoint);
            }
            let mut last = before_id;
            for row in rows {
                let id = row["id"]
                    .as_i64()
                    .filter(|id| *id > 0 && last.is_none_or(|previous| *id < previous))
                    .ok_or(QueryError::Endpoint)?;
                if row["content_included"] != false {
                    return Err(QueryError::Endpoint);
                }
                last = Some(id);
            }
            records.extend_from_slice(rows);
            if page["next_before_id"].is_null() {
                return Ok(records);
            }
            let next = page["next_before_id"]
                .as_i64()
                .filter(|id| !rows.is_empty() && Some(*id) == last)
                .ok_or(QueryError::Endpoint)?;
            before_id = Some(next);
        }
    }
}

/// Fetch and save one session or tool-call query atomically.
#[post("/tokenless/refresh")]
async fn refresh(
    data: web::Data<AppState>,
    client: Option<web::Data<QueryClient>>,
    filter: web::Json<Filter>,
) -> HttpResponse {
    let Some(client) = client else {
        return HttpResponse::ServiceUnavailable()
            .json(json!({"error":"Tokenless API is not configured"}));
    };
    let Some(store) = data.genai_store.clone() else {
        return HttpResponse::ServiceUnavailable()
            .json(json!({"error":"Statistics storage unavailable"}));
    };
    let result = web::block(move || {
        let records = client.fetch(&filter)?;
        store
            .save_tokenless_records(&client.endpoint, &records)
            .map_err(|_| QueryError::Endpoint)?;
        Ok::<_, QueryError>(records.len())
    })
    .await;
    match result {
        Ok(Ok(count)) => HttpResponse::Ok().json(json!({"saved":count})),
        Ok(Err(QueryError::Invalid)) => {
            HttpResponse::BadRequest().json(json!({"error":"Supply a session_id or tool_use_id"}))
        }
        _ => HttpResponse::BadGateway()
            .json(json!({"error":"Tokenless query or persistence failed"})),
    }
}

#[derive(Deserialize)]
struct ListQuery {
    start_ns: Option<i64>,
    end_ns: Option<i64>,
    session_id: Option<String>,
    tool_use_id: Option<String>,
    limit: Option<usize>,
}

/// Return saved metadata even when the original query service is offline.
#[get("/tokenless/records")]
async fn get_records(data: web::Data<AppState>, filter: web::Query<ListQuery>) -> HttpResponse {
    let Some(store) = data.genai_store.clone() else {
        return HttpResponse::ServiceUnavailable().finish();
    };
    let end = filter
        .end_ns
        .unwrap_or_else(|| chrono::Utc::now().timestamp_nanos_opt().unwrap_or(i64::MAX));
    let start = filter
        .start_ns
        .unwrap_or(end.saturating_sub(86_400_000_000_000));
    let filter = filter.into_inner();
    match web::block(move || {
        store.list_tokenless_records(
            start,
            end,
            filter.session_id.as_deref(),
            filter.tool_use_id.as_deref(),
            filter.limit.unwrap_or(100),
        )
    })
    .await
    {
        Ok(Ok(records)) => HttpResponse::Ok().json(json!({"records":records})),
        Ok(Err(crate::storage::sqlite::genai::TokenlessRecordError::Invalid(_))) => {
            HttpResponse::BadRequest().json(json!({"error":"Invalid statistics range or limit"}))
        }
        _ => HttpResponse::ServiceUnavailable()
            .json(json!({"error":"Statistics storage unavailable"})),
    }
}

#[cfg(test)]
#[path = "tokenless_tests.rs"]
mod tests;
