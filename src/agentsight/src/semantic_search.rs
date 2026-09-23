//! Semantic session search shared by the Linux and macOS (local) servers.
//!
//! Both `server::optimize` and `local::server::optimize` expose
//! `POST /api/sessions/search`. This module owns the request/response contract
//! and the LLM ranking call so the two handlers cannot drift apart. It lives at
//! the crate root rather than under `server` because the latter is gated to
//! Linux, which would put it out of reach of the macOS handler.

use actix_web::HttpResponse;
use agentsight_opt::LlmClient;
use agentsight_opt::llm::ChatMessage;
use serde::{Deserialize, Serialize};

/// System prompt instructing the LLM to rank candidate sessions by relevance.
const SEMANTIC_SEARCH_SYSTEM_PROMPT: &str = "你是一个会话搜索助手。用户想查找与其查询语义相关的 Agent 会话。\
请返回与用户查询语义相关的会话，按相关性从高到低排序。\
仅返回 JSON 数组: [{\"session_id\": \"...\", \"relevance\": \"high|medium\", \"reason\": \"一句话理由\"}]。\
如果没有相关会话，返回空数组 []。";

/// Maximum candidate sessions accepted per request (bounds deserialization).
pub const MAX_CANDIDATES: usize = 200;
/// Maximum characters of the user query forwarded to the LLM.
pub const MAX_QUERY_CHARS: usize = 500;
/// Maximum characters per message preview forwarded to the LLM.
const MAX_MESSAGE_CHARS: usize = 100;
/// Number of candidates actually included in the LLM prompt.
const RANKED_CANDIDATES: usize = 50;
/// Default deadline for one semantic-search LLM ranking request.
pub const DEFAULT_SEMANTIC_SEARCH_TIMEOUT_SECS: u64 = 5;

/// A candidate session summary sent by the dashboard for semantic ranking.
#[derive(Debug, Deserialize)]
pub struct SemanticSearchCandidate {
    /// Session id echoed back in the ranked results.
    pub session_id: String,
    /// First user message preview (`None` when unknown).
    pub first_message: Option<String>,
    /// Latest user message preview (`None` when unknown).
    pub last_message: Option<String>,
    /// Project label, when available.
    pub project: Option<String>,
}

/// Body of `POST /api/sessions/search`: the user query plus candidate sessions.
#[derive(Debug, Deserialize)]
pub struct SemanticSearchRequest {
    /// Natural-language query typed by the user.
    pub query: String,
    /// Candidate sessions to rank.
    pub candidates: Vec<SemanticSearchCandidate>,
}

/// One LLM-ranked semantic match.
#[derive(Debug, Deserialize, Serialize)]
pub struct SemanticSearchResult {
    /// Session id the LLM deemed relevant.
    pub session_id: String,
    /// Relevance bucket: `high` or `medium` (normalized server-side).
    pub relevance: String,
    /// Short human-readable justification for the match.
    pub reason: String,
}

/// Response of `POST /api/sessions/search`.
#[derive(Debug, Serialize)]
pub struct SemanticSearchResponse {
    /// Ranked results; empty when nothing matched or the LLM is unavailable.
    pub results: Vec<SemanticSearchResult>,
}

fn truncate_for_llm(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        value.to_string()
    } else {
        let mut out: String = value.chars().take(max_chars).collect();
        out.push_str("...");
        out
    }
}

fn build_candidate_text(candidates: &[SemanticSearchCandidate]) -> String {
    candidates
        .iter()
        .take(RANKED_CANDIDATES)
        .enumerate()
        .map(|(idx, c)| {
            let first =
                truncate_for_llm(c.first_message.as_deref().unwrap_or(""), MAX_MESSAGE_CHARS);
            let last = truncate_for_llm(c.last_message.as_deref().unwrap_or(""), MAX_MESSAGE_CHARS);
            let project = c.project.as_deref().unwrap_or("").to_string();
            format!(
                "{}. [session_id={}] 首条消息: \"{}\" | 最后消息: \"{}\" | 项目: {}",
                idx + 1,
                c.session_id,
                first,
                last,
                project
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Build the system + user messages used for the ranking completion.
fn build_ranking_messages(query: &str, candidates: &[SemanticSearchCandidate]) -> Vec<ChatMessage> {
    let query = truncate_for_llm(query, MAX_QUERY_CHARS);
    vec![
        ChatMessage::system(SEMANTIC_SEARCH_SYSTEM_PROMPT),
        ChatMessage::user(format!(
            "用户查询: \"{}\"\n\n候选会话列表:\n{}",
            query,
            build_candidate_text(candidates)
        )),
    ]
}

/// Clamp `relevance` to the documented `high` / `medium` buckets.
fn normalize_results(mut items: Vec<SemanticSearchResult>) -> Vec<SemanticSearchResult> {
    for item in &mut items {
        if item.relevance != "high" && item.relevance != "medium" {
            item.relevance = "medium".to_string();
        }
    }
    items
}

/// Ask the configured LLM to rank `candidates` by semantic relevance to `query`.
///
/// Returns the normalized, relevance-ordered results, or an empty vector on
/// timeout, LLM error, or unparseable output so callers preserve the existing
/// HTTP response contract while the server records why it degraded.
pub async fn rank_sessions(
    client: &LlmClient,
    query: &str,
    candidates: &[SemanticSearchCandidate],
    timeout_secs: u64,
) -> Vec<SemanticSearchResult> {
    let messages = build_ranking_messages(query, candidates);

    let parsed = actix_web::rt::time::timeout(
        std::time::Duration::from_secs(timeout_secs),
        client.chat_json_parsed_labeled::<Vec<SemanticSearchResult>>(
            messages,
            Some("semantic-search"),
        ),
    )
    .await;

    match parsed {
        Ok(Ok(items)) => normalize_results(items),
        Err(_) => {
            log::warn!(
                "Semantic search ranking timed out after {timeout_secs}s; returning no results"
            );
            vec![]
        }
        Ok(Err(error)) => {
            let failure = if error
                .chain()
                .any(|cause| cause.to_string().contains("JSON parse failed after"))
            {
                "response parse failure"
            } else {
                "LLM request failure"
            };
            log::warn!("Semantic search ranking {failure}; returning no results: {error:#}");
            vec![]
        }
    }
}

/// Drops candidates whose session is on an exclusion list.
///
/// The list comes from the caller — the reuse label store — because which
/// sessions are out of scope is a labelling policy, not a ranking concern, and
/// this module must not depend on the label store to stay a shared,
/// platform-independent contract. Excluding after the minimum-count check would
/// let a full request sink below the LLM threshold, so filtering happens before
/// it: a search whose candidates are all excluded is genuinely empty, not a
/// request that shrank.
pub fn filter_excluded(
    request: SemanticSearchRequest,
    excluded: &std::collections::HashSet<String>,
) -> SemanticSearchRequest {
    if excluded.is_empty() {
        return request;
    }
    SemanticSearchRequest {
        query: request.query,
        candidates: request
            .candidates
            .into_iter()
            .filter(|candidate| !excluded.contains(&candidate.session_id))
            .collect(),
    }
}

/// Validate the request and, when eligible, rank candidates via the LLM.
///
/// Rejects oversized requests, skips the LLM call below the minimum candidate
/// count, and degrades to an empty result set on any LLM failure.
pub async fn handle_semantic_search(
    client: &LlmClient,
    request: &SemanticSearchRequest,
    timeout_secs: u64,
) -> HttpResponse {
    if request.candidates.len() > MAX_CANDIDATES {
        return HttpResponse::BadRequest().json(serde_json::json!({
            "error": "too_many_candidates",
            "message": format!("at most {MAX_CANDIDATES} candidates allowed")
        }));
    }
    // Below this size the candidate list is small enough to scan manually;
    // return no semantic results instead of spending an LLM call.
    if request.candidates.len() <= 5 {
        return HttpResponse::Ok().json(SemanticSearchResponse { results: vec![] });
    }
    let results = rank_sessions(client, &request.query, &request.candidates, timeout_secs).await;
    HttpResponse::Ok().json(SemanticSearchResponse { results })
}

#[cfg(test)]
mod tests {

    #[test]
    fn excluded_sessions_are_dropped_from_candidates() {
        use super::*;
        let request = SemanticSearchRequest {
            query: "版本".to_string(),
            candidates: vec![
                SemanticSearchCandidate {
                    session_id: "keep-1".to_string(),
                    first_message: None,
                    last_message: None,
                    project: None,
                },
                SemanticSearchCandidate {
                    session_id: "drop-1".to_string(),
                    first_message: None,
                    last_message: None,
                    project: None,
                },
            ],
        };
        let mut excluded = std::collections::HashSet::new();
        excluded.insert("drop-1".to_string());
        let filtered = filter_excluded(request, &excluded);
        assert_eq!(filtered.candidates.len(), 1);
        assert_eq!(filtered.candidates[0].session_id, "keep-1");
    }

    #[test]
    fn an_empty_exclusion_list_leaves_the_request_untouched() {
        use super::*;
        let request = SemanticSearchRequest {
            query: "q".to_string(),
            candidates: vec![SemanticSearchCandidate {
                session_id: "s".to_string(),
                first_message: None,
                last_message: None,
                project: None,
            }],
        };
        let filtered = filter_excluded(request, &std::collections::HashSet::new());
        assert_eq!(filtered.candidates.len(), 1);
    }

    use super::*;

    #[test]
    fn truncate_for_llm_leaves_short_values_untouched() {
        assert_eq!(truncate_for_llm("short", 100), "short");
    }

    #[test]
    fn truncate_for_llm_truncates_long_values_with_ellipsis() {
        assert_eq!(truncate_for_llm("abcdefghij", 5), "abcde...");
    }

    #[test]
    fn build_candidate_text_numbers_and_renders_candidates() {
        let candidates = vec![SemanticSearchCandidate {
            session_id: "sess-1".into(),
            first_message: Some("first".into()),
            last_message: None,
            project: Some("web".into()),
        }];
        let text = build_candidate_text(&candidates);
        assert!(text.contains("[session_id=sess-1]"), "{text}");
        assert!(text.contains("首条消息: \"first\""), "{text}");
        assert!(text.contains("项目: web"), "{text}");
    }

    #[test]
    fn build_ranking_messages_truncates_query_and_embeds_candidates() {
        let long_query = "x".repeat(600);
        let candidates = vec![SemanticSearchCandidate {
            session_id: "sess-1".into(),
            first_message: Some("hello".into()),
            last_message: None,
            project: None,
        }];
        let messages = build_ranking_messages(&long_query, &candidates);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "system");
        assert!(!messages[1].content.contains(&"x".repeat(600)));
        assert!(messages[1].content.contains(&"x".repeat(500)));
        assert!(messages[1].content.contains("[session_id=sess-1]"));
    }

    #[test]
    fn normalize_results_clamps_relevance_and_preserves_known_buckets() {
        let results = vec![
            SemanticSearchResult {
                session_id: "a".into(),
                relevance: "high".into(),
                reason: "high".into(),
            },
            SemanticSearchResult {
                session_id: "b".into(),
                relevance: "low".into(),
                reason: "low".into(),
            },
            SemanticSearchResult {
                session_id: "c".into(),
                relevance: "medium".into(),
                reason: "medium".into(),
            },
        ];
        let normalized = normalize_results(results);
        assert_eq!(normalized[0].relevance, "high");
        assert_eq!(normalized[1].relevance, "medium");
        assert_eq!(normalized[2].relevance, "medium");
    }

    fn dummy_client() -> LlmClient {
        LlmClient::with_config("http://127.0.0.1:1/v1", "test-key", "test-model")
    }

    fn ranking_candidates() -> Vec<SemanticSearchCandidate> {
        (0..6)
            .map(|i| SemanticSearchCandidate {
                session_id: format!("sess-{i}"),
                first_message: None,
                last_message: None,
                project: None,
            })
            .collect()
    }

    fn spawn_mock_llm(
        response_body: &'static str,
        delay: std::time::Duration,
        requests: usize,
    ) -> String {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for _ in 0..requests {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0; 4096];
                let _ = stream.read(&mut request);
                std::thread::sleep(delay);
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response_body}",
                    response_body.len()
                )
                .unwrap();
            }
        });
        format!("http://{address}/v1")
    }

    #[test]
    fn handle_semantic_search_degrades_after_llm_request_failure() {
        let request = SemanticSearchRequest {
            query: "test".into(),
            candidates: ranking_candidates(),
        };
        let response = actix_web::rt::System::new().block_on(handle_semantic_search(
            &dummy_client(),
            &request,
            DEFAULT_SEMANTIC_SEARCH_TIMEOUT_SECS,
        ));
        assert_eq!(response.status(), 200);
    }

    #[test]
    fn rank_sessions_degrades_after_timeout() {
        let base_url = spawn_mock_llm("[]", std::time::Duration::from_millis(50), 1);
        let client = LlmClient::with_config(base_url, "test-key", "test-model");
        let results = actix_web::rt::System::new().block_on(rank_sessions(
            &client,
            "test",
            &ranking_candidates(),
            0,
        ));
        assert!(results.is_empty());
    }

    #[test]
    fn rank_sessions_degrades_after_unparseable_response() {
        let base_url = spawn_mock_llm("not-json", std::time::Duration::ZERO, 3);
        let client = LlmClient::with_config(base_url, "test-key", "test-model");
        let results = actix_web::rt::System::new().block_on(rank_sessions(
            &client,
            "test",
            &ranking_candidates(),
            DEFAULT_SEMANTIC_SEARCH_TIMEOUT_SECS,
        ));
        assert!(results.is_empty());
    }

    #[test]
    fn handle_semantic_search_rejects_too_many_candidates() {
        let client = dummy_client();
        let request = SemanticSearchRequest {
            query: "test".into(),
            candidates: (0..=MAX_CANDIDATES)
                .map(|i| SemanticSearchCandidate {
                    session_id: format!("sess-{i}"),
                    first_message: None,
                    last_message: None,
                    project: None,
                })
                .collect(),
        };
        let response = actix_web::rt::System::new().block_on(handle_semantic_search(
            &client,
            &request,
            DEFAULT_SEMANTIC_SEARCH_TIMEOUT_SECS,
        ));
        assert_eq!(response.status(), 400);
    }

    #[test]
    fn handle_semantic_search_skips_small_candidate_lists() {
        let client = dummy_client();
        let request = SemanticSearchRequest {
            query: "test".into(),
            candidates: (0..5)
                .map(|i| SemanticSearchCandidate {
                    session_id: format!("sess-{i}"),
                    first_message: None,
                    last_message: None,
                    project: None,
                })
                .collect(),
        };
        let response = actix_web::rt::System::new().block_on(handle_semantic_search(
            &client,
            &request,
            DEFAULT_SEMANTIC_SEARCH_TIMEOUT_SECS,
        ));
        assert_eq!(response.status(), 200);
    }
}
