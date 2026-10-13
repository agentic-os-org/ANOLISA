//! Query Tokenless with a service credential and save results for AgentSight views.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

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
    /// The query exceeds the bounded recording budget.
    #[error("Tokenless query exceeds 1000 records/requests or 8 MiB; narrow the filters")]
    TooLarge,
    /// The query endpoint returned an error or violated the bounded contract.
    #[error("Tokenless query endpoint failed or returned an unsupported response")]
    Endpoint,
}

struct QueryBudget {
    started: Instant,
    bytes: usize,
    records: usize,
    requests: usize,
}

impl Default for QueryBudget {
    fn default() -> Self {
        Self {
            started: Instant::now(),
            bytes: 0,
            records: 0,
            requests: 0,
        }
    }
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
    before_id: Option<String>,
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

    fn request(
        &self,
        request: ureq::Request,
        body: Option<Value>,
    ) -> Result<(Value, usize), QueryError> {
        let request = request.set("Authorization", &format!("Bearer {}", self.credential));
        let response = if let Some(body) = body {
            request
                .set("Content-Type", "application/json")
                .send_string(&body.to_string())
        } else {
            request.call()
        }
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

    fn fetch(&self, filter: &Filter) -> Result<(Vec<Value>, Option<i64>), QueryError> {
        self.fetch_bounded(filter, &mut QueryBudget::default(), None)
    }

    fn fetch_tools(&self, ids: &[&str]) -> Result<Vec<Value>, QueryError> {
        let ids: BTreeSet<_> = ids.iter().copied().collect();
        if ids.iter().any(|id| id.is_empty() || id.len() > 1024) {
            return Err(QueryError::Invalid);
        }
        let ids: Vec<_> = ids.into_iter().collect();
        let mut budget = QueryBudget::default();
        let mut records = Vec::new();
        for batch in ids.chunks(128) {
            let (rows, _) = self.fetch_bounded(&Filter::default(), &mut budget, Some(batch))?;
            if rows.iter().any(|row| {
                !row["tool_use_id"]
                    .as_str()
                    .is_some_and(|id| batch.contains(&id))
            }) {
                return Err(QueryError::Endpoint);
            }
            records.extend(rows);
        }
        Ok(records)
    }

    fn fetch_bounded(
        &self,
        filter: &Filter,
        budget: &mut QueryBudget,
        tool_ids: Option<&[&str]>,
    ) -> Result<(Vec<Value>, Option<i64>), QueryError> {
        if [&filter.session_id, &filter.tool_use_id]
            .into_iter()
            .flatten()
            .any(|value| value.is_empty() || value.len() > 1024)
        {
            return Err(QueryError::Invalid);
        }
        let unfiltered =
            filter.session_id.is_none() && filter.tool_use_id.is_none() && tool_ids.is_none();
        if !unfiltered && filter.before_id.is_some() {
            return Err(QueryError::Invalid);
        }
        let mut records = Vec::new();
        let mut before_id = filter
            .before_id
            .as_deref()
            .map(|id| id.parse::<i64>())
            .transpose()
            .map_err(|_| QueryError::Invalid)?;
        if before_id.is_some_and(|id| id <= 0) {
            return Err(QueryError::Invalid);
        }
        loop {
            budget.requests += 1;
            if budget.requests > 1000 {
                return Err(QueryError::TooLarge);
            }
            let remaining = Duration::from_secs(5)
                .checked_sub(budget.started.elapsed())
                .filter(|remaining| !remaining.is_zero())
                .ok_or(QueryError::Endpoint)?;
            let mut request = if tool_ids.is_some() {
                self.agent
                    .post(&format!("{}/v1/stats/records/query", self.endpoint))
            } else {
                self.agent
                    .get(&format!("{}/v1/stats/records", self.endpoint))
                    .query("limit", "100")
            };
            if let Some(value) = &filter.session_id {
                request = request.query("session_id", value);
            }
            if let Some(value) = &filter.tool_use_id {
                request = request.query("tool_use_id", value);
            }
            if let Some(id) = before_id.filter(|_| tool_ids.is_none()) {
                request = request.query("before_id", &i64::to_string(&id));
            }
            let body =
                tool_ids.map(|ids| json!({"tool_use_ids":ids,"limit":100,"before_id":before_id}));
            let (page, bytes) = self.request(request.timeout(remaining), body)?;
            budget.bytes += bytes;
            let rows = page["records"].as_array().ok_or(QueryError::Endpoint)?;
            if rows.len() > 100 {
                return Err(QueryError::Endpoint);
            }
            budget.records += rows.len();
            if budget.records > 1000 || budget.bytes as u64 > MAX_BYTES {
                return Err(QueryError::TooLarge);
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
                return Ok((records, None));
            }
            let next = page["next_before_id"]
                .as_i64()
                .filter(|id| !rows.is_empty() && Some(*id) == last)
                .ok_or(QueryError::Endpoint)?;
            if unfiltered {
                return Ok((records, Some(next)));
            }
            before_id = Some(next);
        }
    }
}

/// Fetch and save a bounded query atomically, with optional attribution filters.
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
        let (records, next) = client.fetch(&filter)?;
        store
            .save_tokenless_records(&client.endpoint, &records)
            .map_err(|_| QueryError::Endpoint)?;
        Ok::<_, QueryError>((records.len(), next))
    })
    .await;
    match result {
        Ok(Ok((count, next))) => HttpResponse::Ok()
            .json(json!({"saved":count,"next_before_id":next.map(|id| id.to_string())})),
        Ok(Err(QueryError::Invalid)) => HttpResponse::BadRequest()
            .json(json!({"error":"Use nonempty attribution IDs and a valid pagination cursor"})),
        Ok(Err(QueryError::TooLarge)) => HttpResponse::PayloadTooLarge()
            .json(json!({"error":"Query exceeds 1000 records or 8 MiB; narrow the filters"})),
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
    before_cache_id: Option<i64>,
}

/// Return saved metadata even when the original query service is offline.
#[get("/tokenless/records")]
async fn get_records(
    data: web::Data<AppState>,
    client: Option<web::Data<QueryClient>>,
    filter: web::Query<ListQuery>,
) -> HttpResponse {
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
    let limit = filter.limit.unwrap_or(100);
    match web::block(move || {
        store.list_tokenless_records(
            start,
            end,
            filter.session_id.as_deref(),
            filter.tool_use_id.as_deref(),
            filter.limit.unwrap_or(100),
            filter.before_cache_id,
        )
    })
    .await
    {
        Ok(Ok(records)) => {
            let next = (records.len() == limit)
                .then(|| records.last().map(|record| record.cache_id))
                .flatten();
            HttpResponse::Ok().json(json!({"records":records,"next_before_cache_id":next,"api_configured":client.is_some()}))
        }
        Ok(Err(crate::storage::sqlite::genai::TokenlessRecordError::Invalid(_))) => {
            HttpResponse::BadRequest().json(json!({"error":"Invalid statistics range or limit"}))
        }
        _ => HttpResponse::ServiceUnavailable()
            .json(json!({"error":"Statistics storage unavailable"})),
    }
}

#[derive(Deserialize)]
struct DetailQuery {
    #[serde(default)]
    include_content: bool,
}

/// Fetch content explicitly and retain it for subsequent offline views.
#[get("/tokenless/records/{cache_id}")]
async fn get_record(
    data: web::Data<AppState>,
    client: Option<web::Data<QueryClient>>,
    id: web::Path<i64>,
    query: web::Query<DetailQuery>,
) -> HttpResponse {
    let Some(store) = data.genai_store.clone() else {
        return HttpResponse::ServiceUnavailable().finish();
    };
    if *id <= 0 {
        return HttpResponse::BadRequest().finish();
    }
    let result = web::block(move || {
        let Some(mut saved) = store.tokenless_record(*id).map_err(|_| 503u16)? else {
            return Err(404u16);
        };
        if query.include_content && saved.record["content_included"] != true {
            let client = client.ok_or(503u16)?;
            if saved.source != client.endpoint {
                return Err(409u16);
            }
            let source_id = saved.record["id"].as_i64().ok_or(502u16)?;
            let request = client
                .agent
                .get(&format!("{}/v1/stats/records/{source_id}", client.endpoint))
                .query("include_content", "true");
            let (response, _) = client.request(request, None).map_err(|_| 502u16)?;
            let record = &response["record"];
            if record["id"] != saved.record["id"]
                || record["timestamp"] != saved.record["timestamp"]
                || record["content_included"] != true
            {
                return Err(409u16);
            }
            store
                .save_tokenless_records(&saved.source, std::slice::from_ref(record))
                .map_err(|_| 502u16)?;
            saved = store
                .tokenless_record(*id)
                .map_err(|_| 503u16)?
                .ok_or(503u16)?;
        }
        if !query.include_content {
            for field in ["before_text", "after_text", "before_output", "after_output"] {
                saved.record[field] = Value::Null;
            }
            saved.record["content_included"] = json!(false);
        }
        Ok(saved)
    })
    .await;
    match result {
        Ok(Ok(saved)) => HttpResponse::Ok().json(json!({"record":saved})),
        Ok(Err(status)) => HttpResponse::build(
            actix_web::http::StatusCode::from_u16(status)
                .unwrap_or(actix_web::http::StatusCode::BAD_GATEWAY),
        )
        .json(json!({"error":"Saved record unavailable, source changed, or content query failed"})),
        _ => HttpResponse::ServiceUnavailable().finish(),
    }
}

/// Refresh source statistics before exposing applied savings through existing views.
///
/// # Errors
/// Returns an HTTP error on refresh/storage failure instead of substituting stale totals.
pub(super) async fn savings_rows(
    data: &AppState,
    client: Option<web::Data<QueryClient>>,
    ids: &[&str],
) -> Result<
    (
        bool,
        Vec<crate::storage::sqlite::tokenless::TokenlessStatRow>,
    ),
    HttpResponse,
> {
    if let Some(client) = client {
        let Some(store) = data.genai_store.clone() else {
            return Err(HttpResponse::ServiceUnavailable().finish());
        };
        let ids: Vec<String> = ids.iter().map(|id| (*id).to_owned()).collect();
        let result = web::block(move || {
            let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
            let records = client.fetch_tools(&ids)?;
            store
                .save_tokenless_records(&client.endpoint, &records)
                .map_err(|_| QueryError::Endpoint)?;
            store
                .tokenless_applied_stats(&client.endpoint, &ids, 0, i64::MAX, Some(&records))
                .map_err(|_| QueryError::Endpoint)
        })
        .await;
        return match result {
            Ok(Ok(rows)) => Ok((true, rows)),
            Ok(Err(QueryError::TooLarge)) => Err(HttpResponse::PayloadTooLarge().json(
                json!({"error":"Tokenless refresh exceeds its recording budget; narrow the query"}),
            )),
            _ => Err(HttpResponse::BadGateway()
                .json(json!({"error":"Unable to refresh current Tokenless statistics"}))),
        };
    }
    let store = crate::storage::sqlite::TokenlessStatsStore::open_if_exists(
        &crate::storage::sqlite::tokenless::default_stats_path(),
    );
    Ok((
        store.is_some(),
        store
            .map(|store| store.get_stats_by_tool_use_ids(ids))
            .unwrap_or_default(),
    ))
}

pub(super) fn capabilities(client: Option<&QueryClient>) -> Vec<&'static str> {
    let mut capabilities = super::capabilities::app_capabilities();
    if client.is_some() && !capabilities.contains(&"token_savings") {
        capabilities.push("token_savings");
    }
    capabilities
}

#[cfg(test)]
#[path = "tokenless_tests.rs"]
mod tests;
