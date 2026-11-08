//! Read-only workspace snapshot listing, Step association, and detail endpoints.

use actix_web::{HttpResponse, get, post, web};
use agentsight_atif::{AtifTrajectory, StepSource};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

use super::AppState;
use super::workspace_evidence::{WorkspaceCandidate, step_start, workspace_candidates};
use super::workspace_snapshot_client::{
    ChangeType, DiffEntry, VerifiedWorkspace, WorkspaceSnapshot, WsCkptClient,
};

const DEFAULT_PAGE_LIMIT: u32 = 20;
const MAX_PAGE_LIMIT: u32 = 100;
const MAX_CURSOR_BYTES: usize = 16 * 1024;
const MAX_PATH_DEPTH: usize = 32;
const MAX_DETAIL_PAGES: usize = 20;
const MAX_ANOMALIES: usize = 50;

/// An explicit timestamp or verification problem that limits association.
#[derive(Debug, Serialize)]
pub(crate) struct SnapshotAnomaly {
    code: &'static str,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    timestamp: Option<String>,
}

/// Snapshot entries grouped into one complete Agent Step interval.
#[derive(Debug, Serialize)]
pub(crate) struct StepSnapshotGroup {
    step_index: usize,
    step_id: usize,
    interval_start: String,
    interval_end: String,
    snapshots: Vec<WorkspaceSnapshot>,
}

/// Successful operation that supplied the verified default workspace.
#[derive(Debug, Serialize)]
pub(crate) struct WorkspaceEvidence {
    step_index: usize,
    observed_at: String,
    kind: &'static str,
}

/// Fixed-query-time snapshot page for the active ATIF document.
#[derive(Debug, Serialize)]
pub(crate) struct SnapshotListResponse {
    document_digest: String,
    query_time: String,
    workspace: Option<VerifiedWorkspace>,
    evidence: Option<WorkspaceEvidence>,
    groups: Vec<StepSnapshotGroup>,
    next_cursor: Option<String>,
    anomalies: Vec<SnapshotAnomaly>,
}

/// Both file-level directions used by the read-only detail dialog.
#[derive(Debug, Serialize)]
pub(crate) struct SnapshotDetailResponse {
    workspace: VerifiedWorkspace,
    snapshot: WorkspaceSnapshot,
    step_index: usize,
    query_time: String,
    target_to_current: Vec<DiffEntry>,
    current_to_target: Vec<DiffEntry>,
    rollback_enabled: bool,
    rollback_disabled_reason: &'static str,
}

/// ATIF root and embedded trajectory identity used by the viewer.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DocumentTarget {
    session_id: Option<String>,
    conversation_id: Option<String>,
    /// JSON array of embedded trajectory keys, matching the dashboard node path.
    trajectory_path: Option<String>,
}

/// Query parameters for one snapshot page.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SnapshotListQuery {
    #[serde(flatten)]
    target: DocumentTarget,
    cursor: Option<String>,
    limit: Option<u32>,
}

/// Immutable detail identity returned by a previous list response.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SnapshotDetailRequest {
    #[serde(flatten)]
    target: DocumentTarget,
    document_digest: String,
    query_time: String,
    workspace: VerifiedWorkspace,
    snapshot_id: String,
    step_index: usize,
}

#[derive(Debug, thiserror::Error)]
enum SnapshotApiError {
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Unavailable(String),
}

impl SnapshotApiError {
    fn response(&self) -> HttpResponse {
        let (code, message) = match self {
            Self::BadRequest(message) => ("bad_request", message),
            Self::NotFound(message) => ("not_found", message),
            Self::Conflict(message) => ("conflict", message),
            Self::Unavailable(message) => ("unavailable", message),
        };
        let mut response = match self {
            Self::BadRequest(_) => HttpResponse::BadRequest(),
            Self::NotFound(_) => HttpResponse::NotFound(),
            Self::Conflict(_) => HttpResponse::Conflict(),
            Self::Unavailable(_) => HttpResponse::ServiceUnavailable(),
        };
        response.json(serde_json::json!({"error": code, "message": message}))
    }
}

/// GET /api/atif/workspace-snapshots
#[get("/atif/workspace-snapshots")]
pub(crate) async fn list_workspace_snapshots(
    data: web::Data<AppState>,
    query: web::Query<SnapshotListQuery>,
    client: Option<web::Data<WsCkptClient>>,
) -> HttpResponse {
    let query = query.into_inner();
    let client = client.unwrap_or_else(|| web::Data::new(WsCkptClient::default()));
    match web::block(move || list_page(&data, &query, client.get_ref())).await {
        Ok(Ok(response)) => HttpResponse::Ok().json(response),
        Ok(Err(error)) => error.response(),
        Err(error) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": "blocking_task_failed", "message": error.to_string()
        })),
    }
}

/// POST /api/atif/workspace-snapshots/detail
#[post("/atif/workspace-snapshots/detail")]
pub(crate) async fn workspace_snapshot_detail(
    data: web::Data<AppState>,
    request: web::Json<SnapshotDetailRequest>,
    client: Option<web::Data<WsCkptClient>>,
) -> HttpResponse {
    let request = request.into_inner();
    let client = client.unwrap_or_else(|| web::Data::new(WsCkptClient::default()));
    match web::block(move || detail(&data, &request, client.get_ref())).await {
        Ok(Ok(response)) => HttpResponse::Ok().json(response),
        Ok(Err(error)) => error.response(),
        Err(error) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": "blocking_task_failed", "message": error.to_string()
        })),
    }
}

#[derive(Debug)]
enum Selector<'a> {
    Session(&'a str),
    Conversation(&'a str),
}

impl DocumentTarget {
    fn selector(&self) -> Result<Selector<'_>, SnapshotApiError> {
        match (self.session_id.as_deref(), self.conversation_id.as_deref()) {
            (Some(session), None) if !session.is_empty() => Ok(Selector::Session(session)),
            (None, Some(conversation)) if !conversation.is_empty() => {
                Ok(Selector::Conversation(conversation))
            }
            _ => Err(SnapshotApiError::BadRequest(
                "provide exactly one nonempty session_id or conversation_id".to_string(),
            )),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct PageCursor {
    document_digest: String,
    workspace_id: String,
    query_time: String,
    inner: String,
}

fn list_page(
    data: &AppState,
    query: &SnapshotListQuery,
    client: &WsCkptClient,
) -> Result<SnapshotListResponse, SnapshotApiError> {
    let root = load_root(data, query.target.selector()?)?;
    let document = select_document(&root, query.target.trajectory_path.as_deref())?;
    let digest = document_digest(document)?;
    let cursor = query.cursor.as_deref().map(decode_cursor).transpose()?;
    if cursor
        .as_ref()
        .is_some_and(|cursor| cursor.document_digest != digest)
    {
        return Err(SnapshotApiError::Conflict(
            "the trajectory changed; refresh snapshots before continuing".to_string(),
        ));
    }
    let query_time = match cursor.as_ref() {
        Some(cursor) => parse_time(&cursor.query_time)?,
        None => Utc::now(),
    };
    let query_time_text = format_time(query_time);
    let limit = query
        .limit
        .unwrap_or(DEFAULT_PAGE_LIMIT)
        .clamp(1, MAX_PAGE_LIMIT);
    let candidates = workspace_candidates(document);
    let (workspace, evidence, mut anomalies) = verify_latest(client, candidates);
    if cursor.as_ref().is_some_and(|cursor| {
        Some(cursor.workspace_id.as_str()) != workspace.as_ref().map(|ws| ws.ws_id.as_str())
    }) {
        return Err(SnapshotApiError::Conflict(
            "the verified workspace changed; refresh snapshots before continuing".to_string(),
        ));
    }
    let Some(workspace) = workspace else {
        anomalies.push(anomaly(
            "workspace_unverified",
            "no successful explicit ws-ckpt operation could be verified",
            None,
        ));
        cap_anomalies(&mut anomalies);
        return Ok(SnapshotListResponse {
            document_digest: digest,
            query_time: query_time_text,
            workspace: None,
            evidence: None,
            groups: vec![],
            next_cursor: None,
            anomalies,
        });
    };
    let page = client
        .list(
            &workspace,
            limit,
            cursor.as_ref().map(|cursor| cursor.inner.as_str()),
        )
        .map_err(|error| SnapshotApiError::Unavailable(error.to_string()))?;
    let mut snapshots = Vec::with_capacity(page.snapshots.len());
    for snapshot in page.snapshots {
        if snapshot.workspace == workspace.path || snapshot.workspace == workspace.ws_id {
            snapshots.push(snapshot);
        } else {
            anomalies.push(anomaly(
                "snapshot_workspace_mismatch",
                format!(
                    "snapshot {} belongs to {}, not verified workspace {}",
                    snapshot.id, snapshot.workspace, workspace.ws_id
                ),
                Some(snapshot.meta.created_at.clone()),
            ));
        }
    }
    let (groups, mut association_anomalies) = associate(document, &snapshots, query_time);
    anomalies.append(&mut association_anomalies);
    cap_anomalies(&mut anomalies);
    let next_cursor = page
        .next_cursor
        .map(|inner| {
            encode_cursor(&PageCursor {
                document_digest: digest.clone(),
                workspace_id: workspace.ws_id.clone(),
                query_time: query_time_text.clone(),
                inner,
            })
        })
        .transpose()?;
    Ok(SnapshotListResponse {
        document_digest: digest,
        query_time: query_time_text,
        workspace: Some(workspace),
        evidence,
        groups,
        next_cursor,
        anomalies,
    })
}

fn detail(
    data: &AppState,
    request: &SnapshotDetailRequest,
    client: &WsCkptClient,
) -> Result<SnapshotDetailResponse, SnapshotApiError> {
    let root = load_root(data, request.target.selector()?)?;
    let document = select_document(&root, request.target.trajectory_path.as_deref())?;
    if document_digest(document)? != request.document_digest {
        return Err(SnapshotApiError::Conflict(
            "the trajectory changed; reopen the snapshot from a refreshed list".to_string(),
        ));
    }
    let query_time = parse_time(&request.query_time)?;
    let workspace = client
        .status(&request.workspace.ws_id)
        .map_err(|error| SnapshotApiError::Unavailable(error.to_string()))?;
    if workspace != request.workspace {
        return Err(SnapshotApiError::Conflict(
            "the registered workspace identity changed".to_string(),
        ));
    }
    let snapshot = find_snapshot(client, &workspace, &request.snapshot_id)?
        .ok_or_else(|| SnapshotApiError::NotFound("snapshot no longer exists".to_string()))?;
    let (groups, _) = associate(document, std::slice::from_ref(&snapshot), query_time);
    if groups.len() != 1 || groups[0].step_index != request.step_index {
        return Err(SnapshotApiError::Conflict(
            "snapshot is outside the requested Agent Step interval".to_string(),
        ));
    }
    let target_to_current = client
        .diff_to_current(&workspace, &snapshot.id)
        .map_err(|error| SnapshotApiError::Unavailable(error.to_string()))?;
    let current_to_target = invert_diff(&target_to_current)?;
    Ok(SnapshotDetailResponse {
        workspace,
        snapshot,
        step_index: request.step_index,
        query_time: request.query_time.clone(),
        target_to_current,
        current_to_target,
        rollback_enabled: false,
        rollback_disabled_reason: "rollback requires verified stopped writers, a protection snapshot, guarded restoration, and privileged authorization",
    })
}

fn verify_latest(
    client: &WsCkptClient,
    candidates: Vec<WorkspaceCandidate>,
) -> (
    Option<VerifiedWorkspace>,
    Option<WorkspaceEvidence>,
    Vec<SnapshotAnomaly>,
) {
    let mut anomalies = Vec::new();
    for candidate in candidates.iter().rev() {
        match client.status(&candidate.path) {
            Ok(workspace) => {
                return (
                    Some(workspace),
                    Some(WorkspaceEvidence {
                        step_index: candidate.step_index,
                        observed_at: format_time(candidate.observed_at),
                        kind: candidate.evidence,
                    }),
                    anomalies,
                );
            }
            Err(error) => anomalies.push(anomaly(
                "workspace_verification_failed",
                format!(
                    "workspace candidate at Step {} failed status: {error}",
                    candidate.step_index
                ),
                Some(format_time(candidate.observed_at)),
            )),
        }
    }
    (None, None, anomalies)
}

fn invert_diff(changes: &[DiffEntry]) -> Result<Vec<DiffEntry>, SnapshotApiError> {
    changes
        .iter()
        .map(|entry| {
            let change_type = match entry.change_type {
                ChangeType::Added => ChangeType::Deleted,
                ChangeType::Deleted => ChangeType::Added,
                ChangeType::Modified => ChangeType::Modified,
                ChangeType::Renamed => ChangeType::Renamed,
            };
            if entry.change_type != ChangeType::Renamed {
                return Ok(DiffEntry {
                    path: entry.path.clone(),
                    change_type,
                    detail: entry.detail.clone(),
                });
            }
            let detail = entry.detail.as_deref().ok_or_else(|| {
                SnapshotApiError::Unavailable("rename diff omitted its source path".to_string())
            })?;
            let suffix = format!(" → {}", entry.path);
            let old_path = detail
                .strip_suffix(&suffix)
                .filter(|path| !path.is_empty())
                .ok_or_else(|| {
                    SnapshotApiError::Unavailable(
                        "rename diff did not match the ws-ckpt source → destination contract"
                            .to_string(),
                    )
                })?;
            Ok(DiffEntry {
                path: old_path.to_string(),
                change_type,
                detail: Some(format!("{} → {old_path}", entry.path)),
            })
        })
        .collect()
}

fn find_snapshot(
    client: &WsCkptClient,
    workspace: &VerifiedWorkspace,
    snapshot_id: &str,
) -> Result<Option<WorkspaceSnapshot>, SnapshotApiError> {
    if snapshot_id.is_empty() || snapshot_id.contains('/') || snapshot_id.contains('\0') {
        return Err(SnapshotApiError::BadRequest(
            "snapshot_id must be one nonempty path component".to_string(),
        ));
    }
    let mut cursor = None;
    for _ in 0..MAX_DETAIL_PAGES {
        let page = client
            .list(workspace, MAX_PAGE_LIMIT, cursor.as_deref())
            .map_err(|error| SnapshotApiError::Unavailable(error.to_string()))?;
        if let Some(snapshot) = page.snapshots.into_iter().find(|snapshot| {
            snapshot.id == snapshot_id
                && (snapshot.workspace == workspace.path || snapshot.workspace == workspace.ws_id)
        }) {
            return Ok(Some(snapshot));
        }
        match page.next_cursor {
            Some(next) if next.is_empty() || cursor.as_ref() == Some(&next) => {
                return Err(SnapshotApiError::Unavailable(
                    "ws-ckpt returned a non-advancing snapshot cursor".to_string(),
                ));
            }
            Some(next) => cursor = Some(next),
            None => return Ok(None),
        }
    }
    Err(SnapshotApiError::Unavailable(
        "snapshot lookup exceeded the bounded page budget".to_string(),
    ))
}

#[derive(Clone, Debug)]
struct Interval {
    step_index: usize,
    step_id: usize,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
}

fn associate(
    document: &AtifTrajectory,
    snapshots: &[WorkspaceSnapshot],
    query_time: DateTime<Utc>,
) -> (Vec<StepSnapshotGroup>, Vec<SnapshotAnomaly>) {
    let mut intervals = Vec::new();
    let mut anomalies = Vec::new();
    for (step_index, step) in document.steps.iter().enumerate() {
        if step.source != StepSource::Agent {
            continue;
        }
        match step_start(step) {
            Some(start) => intervals.push(Interval {
                step_index,
                step_id: step.step_id,
                start,
                end: query_time,
            }),
            None => anomalies.push(anomaly(
                "step_timestamp_missing",
                format!("Agent Step {step_index} has no valid start timestamp"),
                step.timestamp.clone(),
            )),
        }
    }
    intervals.sort_by_key(|interval| (interval.start, interval.step_index));
    for index in 1..intervals.len() {
        intervals[index - 1].end = intervals[index].start;
    }

    let mut groups: BTreeMap<usize, (Interval, Vec<(DateTime<Utc>, WorkspaceSnapshot)>)> =
        BTreeMap::new();
    for snapshot in snapshots {
        let created = match DateTime::parse_from_rfc3339(&snapshot.meta.created_at) {
            Ok(value) => value.with_timezone(&Utc),
            Err(_) => {
                anomalies.push(anomaly(
                    "snapshot_timestamp_invalid",
                    format!("snapshot {} has an invalid created_at", snapshot.id),
                    Some(snapshot.meta.created_at.clone()),
                ));
                continue;
            }
        };
        if created >= query_time {
            anomalies.push(anomaly(
                "snapshot_after_query_time",
                format!(
                    "snapshot {} is not before the fixed query time",
                    snapshot.id
                ),
                Some(snapshot.meta.created_at.clone()),
            ));
            continue;
        }
        let position = intervals.partition_point(|interval| interval.start <= created);
        let matched = position
            .checked_sub(1)
            .and_then(|index| intervals.get(index))
            .filter(|interval| created < interval.end);
        let Some(interval) = matched else {
            anomalies.push(anomaly(
                "snapshot_outside_agent_steps",
                format!(
                    "snapshot {} is outside complete Agent Step intervals",
                    snapshot.id
                ),
                Some(snapshot.meta.created_at.clone()),
            ));
            continue;
        };
        groups
            .entry(interval.step_index)
            .or_insert_with(|| ((*interval).clone(), Vec::new()))
            .1
            .push((created, snapshot.clone()));
    }
    let groups = groups
        .into_values()
        .map(|(interval, mut snapshots)| {
            snapshots.sort_by(|(left_created, left), (right_created, right)| {
                right_created.cmp(left_created).then(left.id.cmp(&right.id))
            });
            StepSnapshotGroup {
                step_index: interval.step_index,
                step_id: interval.step_id,
                interval_start: format_time(interval.start),
                interval_end: format_time(interval.end),
                snapshots: snapshots
                    .into_iter()
                    .map(|(_, snapshot)| snapshot)
                    .collect(),
            }
        })
        .collect();
    (groups, anomalies)
}

fn load_root(data: &AppState, selector: Selector<'_>) -> Result<AtifTrajectory, SnapshotApiError> {
    match selector {
        Selector::Conversation(id) => captured_conversation(data, id)?,
        Selector::Session(id) => {
            if let Some(document) = captured_session(data, id)? {
                Some(document)
            } else {
                collected_session(data, id)?
            }
        }
    }
    .ok_or_else(|| SnapshotApiError::NotFound("ATIF document not found".to_string()))
}

fn captured_conversation(
    data: &AppState,
    id: &str,
) -> Result<Option<AtifTrajectory>, SnapshotApiError> {
    let Some(store) = data.genai_store.as_deref() else {
        return Ok(None);
    };
    let events = store
        .get_events_by_conversation(id)
        .map_err(|error| SnapshotApiError::Unavailable(error.to_string()))?;
    if events.is_empty() {
        return Ok(None);
    }
    crate::atif::convert_trace_to_atif(id, events)
        .map(Some)
        .map_err(|error| SnapshotApiError::Unavailable(error.to_string()))
}

fn captured_session(data: &AppState, id: &str) -> Result<Option<AtifTrajectory>, SnapshotApiError> {
    let Some(store) = data.genai_store.as_deref() else {
        return Ok(None);
    };
    let events = store
        .get_events_by_session(id)
        .map_err(|error| SnapshotApiError::Unavailable(error.to_string()))?;
    if events.is_empty() {
        return Ok(None);
    }
    let mut document = crate::atif::convert_session_to_atif(id, events)
        .map_err(|error| SnapshotApiError::Unavailable(error.to_string()))?;
    if let Some(store) = data.trajectory_store() {
        match store.get_captured_session_subagents(id) {
            Ok(children) => {
                let children: Vec<_> = children
                    .iter()
                    .filter_map(|json| match serde_json::from_str(json) {
                        Ok(child) => Some(child),
                        Err(error) => {
                            log::warn!("invalid collected child for {id}: {error}");
                            None
                        }
                    })
                    .collect();
                if !children.is_empty() {
                    document.subagent_trajectories = Some(children);
                }
            }
            Err(error) => log::warn!("collected children unavailable for {id}: {error}"),
        }
    }
    Ok(Some(document))
}

fn collected_session(
    data: &AppState,
    id: &str,
) -> Result<Option<AtifTrajectory>, SnapshotApiError> {
    let Some(store) = data.trajectory_store() else {
        return Ok(None);
    };
    let Some(json) = store
        .get_atif_json(id)
        .map_err(|error| SnapshotApiError::Unavailable(error.to_string()))?
    else {
        return Ok(None);
    };
    let mut document: AtifTrajectory = serde_json::from_str(&json)
        .map_err(|error| SnapshotApiError::Unavailable(format!("invalid stored ATIF: {error}")))?;
    let children = store
        .get_subagent_atif_jsons(id)
        .map_err(|error| SnapshotApiError::Unavailable(error.to_string()))?;
    if !children.is_empty() {
        let children = children
            .iter()
            .map(|json| {
                serde_json::from_str(json).map_err(|error| {
                    SnapshotApiError::Unavailable(format!("invalid child ATIF: {error}"))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        document.subagent_trajectories = Some(children);
    }
    Ok(Some(document))
}

fn select_document<'a>(
    root: &'a AtifTrajectory,
    encoded_path: Option<&str>,
) -> Result<&'a AtifTrajectory, SnapshotApiError> {
    let path: Vec<String> = match encoded_path {
        Some(path) => serde_json::from_str(path).map_err(|_| {
            SnapshotApiError::BadRequest("trajectory_path must be a JSON string array".to_string())
        })?,
        None => vec![],
    };
    if path.len() > MAX_PATH_DEPTH || path.iter().any(|segment| segment.is_empty()) {
        return Err(SnapshotApiError::BadRequest(
            "trajectory_path is empty or exceeds the depth limit".to_string(),
        ));
    }
    let mut document = root;
    for segment in path {
        let children = document.subagent_trajectories.as_deref().unwrap_or(&[]);
        document = children
            .iter()
            .enumerate()
            .find(|(index, child)| {
                child.trajectory_id.as_deref().filter(|id| !id.is_empty()) == Some(segment.as_str())
                    || (child.trajectory_id.as_deref().is_none_or(str::is_empty)
                        && child.session_id.as_deref().filter(|id| !id.is_empty())
                            == Some(segment.as_str()))
                    || (child.trajectory_id.as_deref().is_none_or(str::is_empty)
                        && child.session_id.as_deref().is_none_or(str::is_empty)
                        && *segment == format!("#{index}"))
            })
            .map(|(_, child)| child)
            .ok_or_else(|| {
                SnapshotApiError::Conflict(
                    "the embedded trajectory path no longer resolves".to_string(),
                )
            })?;
    }
    Ok(document)
}

fn document_digest(document: &AtifTrajectory) -> Result<String, SnapshotApiError> {
    let bytes = serde_json::to_vec(document)
        .map_err(|error| SnapshotApiError::Unavailable(error.to_string()))?;
    Ok(hex_encode(&Sha256::digest(bytes)))
}

fn parse_time(value: &str) -> Result<DateTime<Utc>, SnapshotApiError> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|_| SnapshotApiError::BadRequest("query_time must be RFC 3339".to_string()))
}

fn format_time(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Nanos, true)
}

fn anomaly(
    code: &'static str,
    message: impl Into<String>,
    timestamp: Option<String>,
) -> SnapshotAnomaly {
    SnapshotAnomaly {
        code,
        message: message.into(),
        timestamp,
    }
}

fn cap_anomalies(anomalies: &mut Vec<SnapshotAnomaly>) {
    if anomalies.len() > MAX_ANOMALIES {
        let omitted = anomalies.len() - MAX_ANOMALIES;
        anomalies.truncate(MAX_ANOMALIES);
        anomalies.push(anomaly(
            "anomalies_truncated",
            format!("{omitted} additional anomalies omitted"),
            None,
        ));
    }
}

fn encode_cursor(cursor: &PageCursor) -> Result<String, SnapshotApiError> {
    let bytes = serde_json::to_vec(cursor)
        .map_err(|error| SnapshotApiError::Unavailable(error.to_string()))?;
    let encoded = hex_encode(&bytes);
    if encoded.len() > MAX_CURSOR_BYTES {
        return Err(SnapshotApiError::Unavailable(
            "ws-ckpt continuation cursor exceeds the bounded size".to_string(),
        ));
    }
    Ok(encoded)
}

fn decode_cursor(encoded: &str) -> Result<PageCursor, SnapshotApiError> {
    if encoded.is_empty() || encoded.len() > MAX_CURSOR_BYTES || encoded.len() % 2 != 0 {
        return Err(SnapshotApiError::BadRequest(
            "invalid page cursor".to_string(),
        ));
    }
    let bytes = hex_decode(encoded).ok_or_else(|| {
        SnapshotApiError::BadRequest("page cursor is not valid hexadecimal".to_string())
    })?;
    serde_json::from_slice(&bytes)
        .map_err(|_| SnapshotApiError::BadRequest("invalid page cursor payload".to_string()))
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn hex_decode(encoded: &str) -> Option<Vec<u8>> {
    encoded
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = (pair[0] as char).to_digit(16)?;
            let low = (pair[1] as char).to_digit(16)?;
            Some(((high << 4) | low) as u8)
        })
        .collect()
}

#[cfg(test)]
#[path = "workspace_snapshot_tests.rs"]
mod tests;
