//! Token Savings API handlers
//!
//! Provides endpoints that cross-reference genai_events.db with the external
//! ~/.tokenless/stats.db to compute token savings metrics.

use actix_web::{HttpResponse, Responder, get, web};
use serde::{Deserialize, Serialize};

use super::AppState;
use crate::storage::sqlite::GenAISqliteStore;
use crate::storage::sqlite::tokenless::{self, TokenlessStatsStore};

// ─── Query parameters ────────────────────────────────────────────────────────

/// Query parameters for /api/token-savings
#[derive(Debug, Deserialize)]
pub struct TokenSavingsQuery {
    pub start_ns: Option<i64>,
    pub end_ns: Option<i64>,
    pub agent_name: Option<String>,
}

// ─── Response DTOs ───────────────────────────────────────────────────────────

/// Per-strategy saved amounts
#[derive(Debug, Serialize)]
pub struct StrategyBreakdown {
    pub strategy: String,
    pub label: String,
    pub saved: i64,
    pub compounded_saved: i64,
}

/// Overall savings summary
#[derive(Debug, Serialize)]
pub struct SavingsSummary {
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub total_tokens: i64,
    pub baseline_tokens: i64,
    pub total_saved_tokens: i64,
    pub total_compounded_saved: i64,
    /// Fraction of tokens saved: `total_saved_tokens / total_tokens`, in [0.0, 1.0].
    pub savings_rate: f64,
    /// Fraction saved including the compounding effect, in [0.0, 1.0].
    pub compounded_savings_rate: f64,
    pub total_tool_saved: i64,
    pub total_mcp_saved: i64,
    pub total_compounded_tool_saved: i64,
    pub total_compounded_mcp_saved: i64,
    pub strategy_breakdown: Vec<StrategyBreakdown>,
}

/// A single optimization item within a session
#[derive(Debug, Serialize, Clone)]
pub struct OptimizationItemDto {
    pub id: String,
    pub category: String,
    pub title: String,
    pub strategy: String,
    pub strategy_label: String,
    pub before_tokens: i64,
    pub after_tokens: i64,
    pub saved_tokens: i64,
    pub compounded_saved: i64,
    pub compounding_turns: i64,
    pub compression_ratio: f64,
    pub explanation: String,
    pub before_summary: String,
    pub after_summary: String,
    pub optimization_reason: String,
    pub before_text: Option<String>,
    pub after_text: Option<String>,
    pub diff_lines: Vec<DiffLineDto>,
}

/// A single diff line
#[derive(Debug, Serialize, Clone)]
pub struct DiffLineDto {
    #[serde(rename = "type")]
    pub line_type: String,
    pub content: String,
}

/// Per-session savings data
#[derive(Debug, Serialize, Clone)]
pub struct SessionSavingsDto {
    pub session_id: String,
    pub agent_name: String,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub total_tokens: i64,
    pub baseline_tokens: i64,
    pub saved_tokens: i64,
    pub compounded_saved: i64,
    /// Fraction of tokens saved: `saved_tokens / total_tokens`, in [0.0, 1.0].
    pub savings_rate: f64,
    /// Fraction saved including the compounding effect, in [0.0, 1.0].
    pub compounded_savings_rate: f64,
    pub request_count: i64,
    pub tool_saved: i64,
    pub mcp_saved: i64,
    pub optimization_items: Vec<OptimizationItemDto>,
}

/// An actionable optimization tip
#[derive(Debug, Serialize, Clone, PartialEq)]
pub struct OptimizationTip {
    pub level: String,
    pub title: String,
    pub description: String,
}

/// Full response for /api/token-savings
#[derive(Debug, Serialize)]
pub struct TokenSavingsResponse {
    pub stats_available: bool,
    pub summary: SavingsSummary,
    pub sessions: Vec<SessionSavingsDto>,
    pub optimization_tips: Vec<OptimizationTip>,
}

/// Response for /api/token-savings/session/{session_id}
#[derive(Debug, Serialize)]
pub struct SessionSavingsDetail {
    pub session_id: String,
    pub stats_available: bool,
    pub total_actual_tokens: i64,
    pub total_compounded_saved: i64,
    pub total_original_tokens: i64,
    /// Fraction of tokens saved: `total_compounded_saved / total_actual_tokens`, in [0.0, 1.0].
    pub savings_rate: f64,
    pub items: Vec<OptimizationItemDto>,
}

// ─── Mapping helpers ─────────────────────────────────────────────────────────

/// Map stats.db `operation` field to frontend category.
///
/// Classification rationale:
/// - `compress-response` / `compress-toon`: both compress MCP server responses
///   (toon uses a structured encoding variant), hence `mcp_response`.
/// - `rewrite-command` / `compress-schema`: both reduce tool-definition /
///   invocation payloads sent to the LLM, hence `tool_output`.
fn map_operation_to_category(operation: &str) -> &str {
    match operation {
        // MCP response compression strategies
        "compress-response" | "compress-toon" => "mcp_response",
        // Tool definition / invocation compression strategies
        "rewrite-command" | "compress-schema" => "tool_output",
        _ => "tool_output",
    }
}

/// Map operation to a human-readable title.
fn map_operation_to_title(operation: &str) -> &str {
    match operation {
        "compress-response" => "MCP Response Compression",
        "rewrite-command" => "Tool Output Optimization",
        "compress-schema" => "Schema Compression",
        "compress-toon" => "TOON Encoding",
        _ => "Other Optimization",
    }
}

/// Map operation to a human-readable strategy label.
///
/// Note: unknown operations all map to "Other Optimization", and the aggregation logic
/// uses this label as the grouping key to avoid duplicate pie chart slices.
fn map_operation_to_strategy_label(operation: &str) -> &str {
    match operation {
        "compress-schema" => "Schema Compression",
        "compress-response" => "Response Compression",
        "rewrite-command" => "Command Rewrite",
        "compress-toon" => "TOON Encoding",
        _ => "Other Optimization",
    }
}

/// Generate a human-readable optimization reason for a given operation.
fn generate_optimization_reason(operation: &str, before_tokens: i64, after_tokens: i64) -> String {
    let saved = before_tokens - after_tokens;
    let pct = if before_tokens > 0 {
        (saved as f64 / before_tokens as f64 * 100.0).round() as i64
    } else {
        0
    };
    match operation {
        "compress-response" => format!(
            "MCP server responses were compressed, removing redundant fields and duplicates, saving {pct}% ({saved} tokens)"
        ),
        "rewrite-command" => format!(
            "Tool call outputs were concisely rewritten, preserving key semantics while cutting token cost, saving {pct}% ({saved} tokens)"
        ),
        "compress-schema" => format!(
            "Tool JSON Schema definitions were compressed, removing descriptive text and optional fields, saving {pct}% ({saved} tokens)"
        ),
        "compress-toon" => format!(
            "TOON structured encoding replaced raw JSON, greatly shrinking token usage, saving {pct}% ({saved} tokens)"
        ),
        _ => format!("Content was optimized, saving {pct}% ({saved} tokens)"),
    }
}

/// Compute line-level diff between before/after text.
///
/// Uses LCS (longest common subsequence) to identify unchanged, added, and
/// removed lines. Only a window of CONTEXT_LINES around each change is shown;
/// hidden regions are represented by a separator line ("...").
/// For very large inputs (n+m > 2000), falls back to a simple before/after listing.
fn compute_diff_lines(before: Option<&str>, after: Option<&str>) -> Vec<DiffLineDto> {
    const CONTEXT_LINES: usize = 3;
    const MAX_DIFF_LINES: usize = 200;

    let before_text = before.unwrap_or("");
    let after_text = after.unwrap_or("");

    if before_text.is_empty() && after_text.is_empty() {
        return Vec::new();
    }

    let before_lines: Vec<&str> = before_text.lines().collect();
    let after_lines: Vec<&str> = after_text.lines().collect();
    let n = before_lines.len();
    let m = after_lines.len();

    // Fallback for very large inputs to avoid O(n*m) memory/time
    if n + m > 2000 {
        let mut result = Vec::new();
        for line in before_lines.iter().take(MAX_DIFF_LINES / 2) {
            result.push(DiffLineDto {
                line_type: "remove".to_string(),
                content: line.to_string(),
            });
        }
        for line in after_lines.iter().take(MAX_DIFF_LINES / 2) {
            result.push(DiffLineDto {
                line_type: "add".to_string(),
                content: line.to_string(),
            });
        }
        return result;
    }

    // LCS DP
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in 1..=n {
        for j in 1..=m {
            dp[i][j] = if before_lines[i - 1] == after_lines[j - 1] {
                dp[i - 1][j - 1] + 1
            } else {
                dp[i - 1][j].max(dp[i][j - 1])
            };
        }
    }

    // Backtrack to produce diff entries
    let mut diff: Vec<(char, &str)> = Vec::new();
    let (mut i, mut j) = (n, m);
    while i > 0 || j > 0 {
        if i > 0 && j > 0 && before_lines[i - 1] == after_lines[j - 1] {
            diff.push((' ', before_lines[i - 1]));
            i -= 1;
            j -= 1;
        } else if j > 0 && (i == 0 || dp[i][j - 1] >= dp[i - 1][j]) {
            diff.push(('+', after_lines[j - 1]));
            j -= 1;
        } else {
            diff.push(('-', before_lines[i - 1]));
            i -= 1;
        }
    }
    diff.reverse();

    // Determine which lines are visible (within CONTEXT_LINES of a change)
    let change_indices: Vec<usize> = diff
        .iter()
        .enumerate()
        .filter(|(_, (t, _))| *t != ' ')
        .map(|(idx, _)| idx)
        .collect();

    if change_indices.is_empty() {
        return Vec::new();
    }

    let total = diff.len();
    let mut visible = vec![false; total];
    for &ci in &change_indices {
        let start = ci.saturating_sub(CONTEXT_LINES);
        let end = (ci + CONTEXT_LINES + 1).min(total);
        for v in visible[start..end].iter_mut() {
            *v = true;
        }
    }

    // Build output with separator for hidden regions
    let mut result: Vec<DiffLineDto> = Vec::new();
    let mut last_visible = false;
    for (idx, &vis) in visible.iter().enumerate() {
        if vis {
            let (t, content) = &diff[idx];
            let line_type = match t {
                '+' => "add",
                '-' => "remove",
                _ => "context",
            };
            result.push(DiffLineDto {
                line_type: line_type.to_string(),
                content: content.to_string(),
            });
            last_visible = true;
        } else if last_visible && visible[idx + 1..].iter().any(|v| *v) {
            result.push(DiffLineDto {
                line_type: "separator".to_string(),
                content: "...".to_string(),
            });
            last_visible = false;
        }
    }

    result.truncate(MAX_DIFF_LINES);
    result
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Current UNIX time in nanoseconds
fn now_ns() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
}

/// Compute compression ratio clamped to [0.0, 100.0].
/// Returns 0.0 when before_tokens is 0 or after >= before (no real compression).
pub(crate) fn compute_compression_ratio(before_tokens: i64, after_tokens: i64) -> f64 {
    if before_tokens > 0 {
        let raw = (1.0 - after_tokens as f64 / before_tokens as f64) * 100.0;
        raw.clamp(0.0, 100.0)
    } else {
        0.0
    }
}

/// Build a human-readable explanation string for an optimization item.
pub(crate) fn build_explanation(
    category: &str,
    before_tokens: i64,
    after_tokens: i64,
    compression_ratio: f64,
    compounding_turns: i64,
    compounded: i64,
) -> String {
    if category == "mcp_response" {
        format!(
            "MCP Response Compression: original {} tokens → {} tokens, ratio {:.1}%. The following {} LLM calls benefit, compounding to {} tokens saved.",
            before_tokens, after_tokens, compression_ratio, compounding_turns, compounded
        )
    } else {
        format!(
            "Tool Output Optimization: original {} tokens → {} tokens, ratio {:.1}%. The following {} LLM calls benefit, compounding to {} tokens saved.",
            before_tokens, after_tokens, compression_ratio, compounding_turns, compounded
        )
    }
}

/// Generate optimization tips based on aggregated savings data.
///
/// `grand_compounded_rate` is a fraction in [0.0, 1.0] (not a percentage).
pub(crate) fn generate_optimization_tips(
    stats_available: bool,
    grand_total: i64,
    grand_compounded_rate: f64,
    grand_compounded_tool_saved: i64,
    grand_compounded_mcp_saved: i64,
    sessions: &[SessionSavingsDto],
) -> Vec<OptimizationTip> {
    let mut tips: Vec<OptimizationTip> = Vec::new();

    if !stats_available {
        tips.push(OptimizationTip {
            level: "warning".to_string(),
            title: "Tokenless component not detected".to_string(),
            description: "stats.db not found. Make sure the tokenless component is installed and enabled; it automatically compresses tool output and MCP responses, significantly cutting token consumption.".to_string(),
        });
    } else if grand_compounded_rate < 0.15 && grand_total > 0 {
        tips.push(OptimizationTip {
            level: "warning".to_string(),
            title: "Savings rate is low".to_string(),
            description: "The compounded savings rate is below 15%. Check that tokenless is enabled for all agents and that both tool-output and MCP-response compression are on.".to_string(),
        });
    }

    if grand_compounded_tool_saved > 0 && grand_compounded_mcp_saved == 0 && grand_total > 0 {
        tips.push(OptimizationTip {
            level: "info".to_string(),
            title: "Consider enabling MCP response compression".to_string(),
            description:
                "Only tool-output optimization is active; MCP response compression was not detected. Enabling it would cut token consumption further."
                    .to_string(),
        });
    }

    if grand_compounded_mcp_saved > 0 && grand_compounded_tool_saved == 0 && grand_total > 0 {
        tips.push(OptimizationTip {
            level: "info".to_string(),
            title: "Consider enabling tool-output optimization".to_string(),
            description:
                "Only MCP response compression is active; tool-output optimization was not detected. Enabling it would cut token consumption further."
                    .to_string(),
        });
    }

    let zero_savings_sessions = sessions
        .iter()
        .filter(|s| s.compounded_saved == 0 && s.total_tokens > 1000)
        .count();
    if zero_savings_sessions > 0 {
        tips.push(OptimizationTip {
            level: "info".to_string(),
            title: format!("Found {} unoptimized sessions", zero_savings_sessions),
            description: "Some sessions have high consumption but no optimization records; the corresponding agents may not have tokenless enabled or make few tool calls. Check the agent configuration for these sessions.".to_string(),
        });
    }

    if grand_compounded_rate >= 0.30 {
        tips.push(OptimizationTip {
            level: "success".to_string(),
            title: "Excellent savings".to_string(),
            description: format!(
                "Current compounded savings rate is {:.1}% — excellent! Keep the current configuration.",
                grand_compounded_rate * 100.0
            ),
        });
    } else if grand_compounded_rate >= 0.15 {
        tips.push(OptimizationTip {
            level: "success".to_string(),
            title: "Good savings".to_string(),
            description: format!(
                "Current compounded savings rate is {:.1}% — a good level. Try tuning compression strategies to improve further.",
                grand_compounded_rate * 100.0
            ),
        });
    }

    tips
}

// ─── GET /api/token-savings ──────────────────────────────────────────────────

/// GET /api/token-savings?start_ns=<i64>&end_ns=<i64>&agent_name=<str>
///
/// Returns token savings data by cross-referencing genai_events.db
/// with the external ~/.tokenless/stats.db.
#[get("/token-savings")]
pub async fn get_token_savings(
    data: web::Data<AppState>,
    query: web::Query<TokenSavingsQuery>,
) -> impl Responder {
    let db_path = &data.storage_path;
    let end_ns = query.end_ns.unwrap_or_else(|| now_ns() as i64);
    let start_ns = query
        .start_ns
        .unwrap_or_else(|| end_ns - 86_400_000_000_000i64);
    let agent_name = query.agent_name.as_deref();

    // Step 1: Query sessions from genai_events.db
    let sessions = match GenAISqliteStore::new_with_path(
        db_path,
        crate::config::InsertStoragePolicy::default(),
    ) {
        Ok(store) => match store.list_sessions_for_savings(start_ns, end_ns, agent_name) {
            Ok(s) => s,
            Err(e) => {
                return HttpResponse::InternalServerError()
                    .json(serde_json::json!({"error": e.to_string()}));
            }
        },
        Err(e) => {
            return HttpResponse::InternalServerError()
                .json(serde_json::json!({"error": e.to_string()}));
        }
    };

    // Step 2: Open stats.db (read-only, graceful if absent)
    let stats_path = tokenless::default_stats_path();
    let stats_store = TokenlessStatsStore::open_if_exists(&stats_path);
    let stats_available = stats_store.is_some();

    // Step 3: Build tool_call_id → (turn_index, session_id) map from genai_events.
    // This gives us all known tool_use_ids and their session membership.
    let session_ids: Vec<&str> = sessions.iter().map(|s| s.session_id.as_str()).collect();
    let turn_indices = match GenAISqliteStore::new_with_path(
        db_path,
        crate::config::InsertStoragePolicy::default(),
    ) {
        Ok(store) => store
            .get_tool_call_turn_indices(&session_ids)
            .unwrap_or_default(),
        Err(_) => std::collections::HashMap::new(),
    };

    // Step 4: Query stats.db by tool_use_ids (instead of session_ids)
    let stats_by_session = if let Some(ref store) = stats_store {
        let tool_use_ids: Vec<&str> = turn_indices.keys().map(|s| s.as_str()).collect();
        let rows = store.get_stats_by_tool_use_ids(&tool_use_ids);
        // Group by session: use turn_indices to determine session, fallback to row.session_id
        let mut map: std::collections::HashMap<String, Vec<_>> = std::collections::HashMap::new();
        for row in rows {
            let sid = turn_indices
                .get(&row.tool_use_id)
                .map(|info| info.session_id.clone())
                .unwrap_or_else(|| row.session_id.clone());
            map.entry(sid).or_default().push(row);
        }
        map
    } else {
        std::collections::HashMap::new()
    };

    // Step 5: Build response
    let mut resp_sessions = Vec::with_capacity(sessions.len());
    let mut grand_input: i64 = 0;
    let mut grand_output: i64 = 0;
    let mut grand_saved: i64 = 0;
    let mut grand_compounded_saved: i64 = 0;
    let mut grand_tool_saved: i64 = 0;
    let mut grand_mcp_saved: i64 = 0;
    let mut grand_compounded_tool_saved: i64 = 0;
    let mut grand_compounded_mcp_saved: i64 = 0;
    // FIX(#2): aggregate by strategy *label* (not raw operation) so that
    // unknown operations merge into a single "Other Optimization" slice in the pie chart.
    let mut grand_strategy_map: std::collections::HashMap<String, (String, i64, i64)> =
        std::collections::HashMap::new();

    for session in &sessions {
        let total_tokens = session.total_input_tokens + session.total_output_tokens;
        let request_count = session.request_count;
        let mut session_saved: i64 = 0;
        let mut session_compounded_saved: i64 = 0;
        let mut session_tool_saved: i64 = 0;
        let mut session_mcp_saved: i64 = 0;
        let mut session_compounded_tool_saved: i64 = 0;
        let mut session_compounded_mcp_saved: i64 = 0;
        let mut items = Vec::new();

        if let Some(stat_rows) = stats_by_session.get(&session.session_id) {
            for row in stat_rows {
                let saved = row.before_tokens - row.after_tokens;
                let category = map_operation_to_category(&row.operation);
                let title = map_operation_to_title(&row.operation);

                // Compounding: the shortened tool output appears in the
                // context of all LLM calls AFTER the one that triggered the
                // tool use. If the tool was invoked at turn N (1-based) out
                // of M total turns, the savings persist for (M - N) turns.
                let turn_index = turn_indices
                    .get(&row.tool_use_id)
                    .map(|info| info.turn_index)
                    .unwrap_or(1) as i64;
                let compounding_turns = (request_count - turn_index).max(1);
                let compounded = saved * compounding_turns;

                if category == "mcp_response" {
                    session_mcp_saved += saved;
                    session_compounded_mcp_saved += compounded;
                } else {
                    session_tool_saved += saved;
                    session_compounded_tool_saved += compounded;
                }
                session_saved += saved;
                session_compounded_saved += compounded;

                let diff_lines =
                    compute_diff_lines(row.before_text.as_deref(), row.after_text.as_deref());
                let optimization_reason = generate_optimization_reason(
                    &row.operation,
                    row.before_tokens,
                    row.after_tokens,
                );

                let strategy = row.operation.clone();
                let strategy_label = map_operation_to_strategy_label(&row.operation).to_string();

                // FIX(#2): aggregate by strategy key so unknown ops merge into one slice.
                let strategy_key = match row.operation.as_str() {
                    "compress-response" | "compress-toon" | "rewrite-command"
                    | "compress-schema" => row.operation.clone(),
                    _ => "other".to_string(),
                };
                let entry = grand_strategy_map.entry(strategy_key).or_insert((
                    strategy_label.clone(),
                    0,
                    0,
                ));
                entry.1 += saved;
                entry.2 += compounded;

                let compression_ratio =
                    compute_compression_ratio(row.before_tokens, row.after_tokens);
                let explanation = build_explanation(
                    category,
                    row.before_tokens,
                    row.after_tokens,
                    compression_ratio,
                    compounding_turns,
                    compounded,
                );

                items.push(OptimizationItemDto {
                    id: row.tool_use_id.clone(),
                    category: category.to_string(),
                    title: title.to_string(),
                    strategy,
                    strategy_label,
                    before_tokens: row.before_tokens,
                    after_tokens: row.after_tokens,
                    saved_tokens: saved,
                    compounded_saved: compounded,
                    compounding_turns,
                    compression_ratio,
                    explanation,
                    before_summary: format!("Original {} tokens", row.before_tokens),
                    after_summary: format!("Optimized {} tokens", row.after_tokens),
                    optimization_reason,
                    before_text: row.before_text.clone(),
                    after_text: row.after_text.clone(),
                    diff_lines,
                });
            }
        }

        // FIX(#1): use compounded/total_tokens for both list and detail pages
        let savings_rate = if total_tokens > 0 {
            session_saved as f64 / total_tokens as f64
        } else {
            0.0
        };
        let compounded_savings_rate = if total_tokens > 0 {
            session_compounded_saved as f64 / total_tokens as f64
        } else {
            0.0
        };

        grand_input += session.total_input_tokens;
        grand_output += session.total_output_tokens;
        grand_saved += session_saved;
        grand_compounded_saved += session_compounded_saved;
        grand_tool_saved += session_tool_saved;
        grand_mcp_saved += session_mcp_saved;
        grand_compounded_tool_saved += session_compounded_tool_saved;
        grand_compounded_mcp_saved += session_compounded_mcp_saved;

        resp_sessions.push(SessionSavingsDto {
            session_id: session.session_id.clone(),
            agent_name: session.agent_name.clone().unwrap_or_default(),
            total_input_tokens: session.total_input_tokens,
            total_output_tokens: session.total_output_tokens,
            total_tokens,
            baseline_tokens: total_tokens + session_compounded_saved,
            saved_tokens: session_saved,
            compounded_saved: session_compounded_saved,
            savings_rate,
            compounded_savings_rate,
            request_count,
            tool_saved: session_tool_saved,
            mcp_saved: session_mcp_saved,
            optimization_items: items,
        });
    }

    let grand_total = grand_input + grand_output;
    let grand_rate = if grand_total > 0 {
        grand_saved as f64 / grand_total as f64
    } else {
        0.0
    };
    let grand_compounded_rate = if grand_total > 0 {
        grand_compounded_saved as f64 / grand_total as f64
    } else {
        0.0
    };

    // FIX(#2): strategy = operation key (for frontend color lookup),
    //           label = Chinese display name
    let strategy_breakdown: Vec<StrategyBreakdown> = grand_strategy_map
        .into_iter()
        .map(
            |(strategy_key, (label, saved, compounded_saved))| StrategyBreakdown {
                strategy: strategy_key,
                label,
                saved,
                compounded_saved,
            },
        )
        .collect();

    HttpResponse::Ok().json(TokenSavingsResponse {
        stats_available,
        summary: SavingsSummary {
            total_input_tokens: grand_input,
            total_output_tokens: grand_output,
            total_tokens: grand_total,
            baseline_tokens: grand_total + grand_compounded_saved,
            total_saved_tokens: grand_saved,
            total_compounded_saved: grand_compounded_saved,
            savings_rate: grand_rate,
            compounded_savings_rate: grand_compounded_rate,
            total_tool_saved: grand_tool_saved,
            total_mcp_saved: grand_mcp_saved,
            total_compounded_tool_saved: grand_compounded_tool_saved,
            total_compounded_mcp_saved: grand_compounded_mcp_saved,
            strategy_breakdown,
        },
        sessions: resp_sessions.clone(),
        optimization_tips: generate_optimization_tips(
            stats_available,
            grand_total,
            grand_compounded_rate,
            grand_compounded_tool_saved,
            grand_compounded_mcp_saved,
            &resp_sessions,
        ),
    })
}

// ─── GET /api/token-savings/session/{session_id} ─────────────────────────────

/// GET /api/token-savings/session/{session_id}
///
/// Returns token savings detail for a single session.
#[get("/token-savings/session/{session_id}")]
pub async fn get_session_savings(
    data: web::Data<AppState>,
    path: web::Path<String>,
) -> impl Responder {
    let session_id = path.into_inner();
    let db_path = &data.storage_path;

    // FIX(#3): query single session by id instead of full-table scan
    let store = match GenAISqliteStore::new_with_path(
        db_path,
        crate::config::InsertStoragePolicy::default(),
    ) {
        Ok(s) => s,
        Err(e) => {
            return HttpResponse::InternalServerError()
                .json(serde_json::json!({"error": e.to_string()}));
        }
    };

    let session = match store.get_session_for_savings(&session_id) {
        Ok(Some(s)) => s,
        Ok(None) => {
            return HttpResponse::Ok().json(SessionSavingsDetail {
                session_id,
                stats_available: false,
                total_actual_tokens: 0,
                total_compounded_saved: 0,
                total_original_tokens: 0,
                savings_rate: 0.0,
                items: Vec::new(),
            });
        }
        Err(e) => {
            return HttpResponse::InternalServerError()
                .json(serde_json::json!({"error": e.to_string()}));
        }
    };

    let total_tokens = session.total_input_tokens + session.total_output_tokens;
    let request_count = session.request_count;

    // Step 2: Get turn indices for tool_call_ids
    let session_ids = vec![session_id.as_str()];
    let turn_indices = match GenAISqliteStore::new_with_path(
        db_path,
        crate::config::InsertStoragePolicy::default(),
    ) {
        Ok(st) => st
            .get_tool_call_turn_indices(&session_ids)
            .unwrap_or_default(),
        Err(_) => std::collections::HashMap::new(),
    };

    // Step 3: Open stats.db
    let stats_path = tokenless::default_stats_path();
    let stats_store = TokenlessStatsStore::open_if_exists(&stats_path);
    let stats_available = stats_store.is_some();

    let mut items = Vec::new();
    let mut total_compounded_saved: i64 = 0;

    if let Some(ref store) = stats_store {
        let tool_use_ids: Vec<&str> = turn_indices.keys().map(|s| s.as_str()).collect();
        let rows = store.get_stats_by_tool_use_ids(&tool_use_ids);

        for row in &rows {
            // Only include rows belonging to this session
            let sid = turn_indices
                .get(&row.tool_use_id)
                .map(|info| info.session_id.as_str())
                .unwrap_or(&row.session_id);
            if sid != session_id {
                continue;
            }

            let saved = row.before_tokens - row.after_tokens;
            let category = map_operation_to_category(&row.operation);
            let title = map_operation_to_title(&row.operation);
            let strategy = row.operation.clone();
            let strategy_label = map_operation_to_strategy_label(&row.operation).to_string();

            let turn_index = turn_indices
                .get(&row.tool_use_id)
                .map(|info| info.turn_index)
                .unwrap_or(1) as i64;
            let compounding_turns = (request_count - turn_index).max(1);
            let compounded = saved * compounding_turns;
            total_compounded_saved += compounded;

            items.push(OptimizationItemDto {
                id: row.tool_use_id.clone(),
                category: category.to_string(),
                title: title.to_string(),
                strategy,
                strategy_label,
                before_tokens: row.before_tokens,
                after_tokens: row.after_tokens,
                saved_tokens: saved,
                compounded_saved: compounded,
                compounding_turns,
                compression_ratio: compute_compression_ratio(row.before_tokens, row.after_tokens),
                explanation: build_explanation(
                    category,
                    row.before_tokens,
                    row.after_tokens,
                    compute_compression_ratio(row.before_tokens, row.after_tokens),
                    compounding_turns,
                    compounded,
                ),
                before_summary: format!("Original {} tokens", row.before_tokens),
                after_summary: format!("Optimized {} tokens", row.after_tokens),
                optimization_reason: generate_optimization_reason(
                    &row.operation,
                    row.before_tokens,
                    row.after_tokens,
                ),
                before_text: row.before_text.clone(),
                after_text: row.after_text.clone(),
                diff_lines: compute_diff_lines(
                    row.before_text.as_deref(),
                    row.after_text.as_deref(),
                ),
            });
        }
    }

    // FIX(#1): use compounded/total_tokens — consistent with get_token_savings
    let savings_rate = if total_tokens > 0 {
        total_compounded_saved as f64 / total_tokens as f64
    } else {
        0.0
    };

    HttpResponse::Ok().json(SessionSavingsDetail {
        session_id,
        stats_available,
        total_actual_tokens: total_tokens,
        total_compounded_saved,
        total_original_tokens: total_tokens + total_compounded_saved,
        savings_rate,
        items,
    })
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grader::EvaluationStore;
    use actix_web::test as actix_test;
    use actix_web::{App, web};
    use std::sync::{Arc, Mutex, RwLock};
    use std::time::Instant;

    // Tests manipulate the HOME env var which is process-global.
    // Use a mutex to serialize tests that depend on it.
    // allow(clippy::await_holding_lock): intentional — we need the lock held
    // for the entire test to prevent parallel env var races.
    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    /// Create a temp genai_events.db with test data and return its path.
    fn setup_genai_db(dir: &std::path::Path) -> std::path::PathBuf {
        let db_path = dir.join("genai_events.db");
        // Use GenAISqliteStore to create proper schema
        let store = crate::storage::sqlite::GenAISqliteStore::new_with_path(
            &db_path,
            crate::config::InsertStoragePolicy::default(),
        )
        .unwrap();
        // Insert test data directly via raw connection
        drop(store);
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO genai_events (event_type, session_id, call_id, agent_name, model, input_tokens, output_tokens, start_timestamp_ns, event_json, tool_call_ids)
             VALUES ('llm_call', 'sess-1', 'call-1', 'test-agent', 'gpt-4', 1000, 500, 100000000, '{}', '[\"tc-1\"]')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO genai_events (event_type, session_id, call_id, agent_name, model, input_tokens, output_tokens, start_timestamp_ns, event_json, tool_call_ids)
             VALUES ('llm_call', 'sess-1', 'call-2', 'test-agent', 'gpt-4', 800, 400, 200000000, '{}', '[\"tc-2\"]')",
            [],
        ).unwrap();
        db_path
    }

    /// Create a temp stats.db with test data and return its path.
    fn setup_stats_db(dir: &std::path::Path) -> std::path::PathBuf {
        let stats_dir = dir.join(".tokenless");
        std::fs::create_dir_all(&stats_dir).unwrap();
        let stats_path = stats_dir.join("stats.db");
        let conn = rusqlite::Connection::open(&stats_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE stats (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT,
                tool_use_id TEXT,
                before_tokens INTEGER,
                after_tokens INTEGER,
                before_text TEXT,
                after_text TEXT,
                operation TEXT
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO stats (session_id, tool_use_id, before_tokens, after_tokens, before_text, after_text, operation)
             VALUES ('sess-1', 'tc-1', 2000, 500, 'long text', 'short', 'compress-response')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO stats (session_id, tool_use_id, before_tokens, after_tokens, before_text, after_text, operation)
             VALUES ('sess-1', 'tc-2', 1000, 300, 'schema text', 'mini', 'compress-schema')",
            [],
        ).unwrap();
        stats_path
    }

    fn make_app_state(db_path: std::path::PathBuf) -> AppState {
        AppState {
            reuse_store: None,
            reuse_llm_judge_enabled: false,
            causal_store: None,
            storage_path: db_path.clone(),
            start_time: Instant::now(),
            health_store: Arc::new(RwLock::new(crate::health::HealthStore::default())),
            interruption_store: None,
            evaluation_store: Arc::new(EvaluationStore::new_with_path(&db_path).unwrap()),
            enforcement: None,
            containment: None,
            audit_service: Arc::new(agentsight_audit::AuditService::new(
                crate::security::SecurityStore::open_in_memory()
                    .unwrap()
                    .audit_store(),
            )),
            security_observability: crate::server::SecurityObservabilityConfig::default(),
            auth: Arc::new(crate::server::auth::DashboardAuth::init(
                &crate::config::ServerAuthConfig { enabled: false },
                std::path::Path::new("/tmp"),
            )),
            optimize: None,
            trajectory_store: Arc::new(RwLock::new(None)),
        }
    }

    // ─── Unit tests for mapping functions ─────────────────────────────────

    #[test]
    fn test_map_operation_to_category() {
        assert_eq!(
            map_operation_to_category("compress-response"),
            "mcp_response"
        );
        assert_eq!(map_operation_to_category("compress-toon"), "mcp_response");
        assert_eq!(map_operation_to_category("rewrite-command"), "tool_output");
        assert_eq!(map_operation_to_category("compress-schema"), "tool_output");
        assert_eq!(map_operation_to_category("unknown-op"), "tool_output");
    }

    #[test]
    fn test_map_operation_to_title() {
        assert_eq!(
            map_operation_to_title("compress-response"),
            "MCP Response Compression"
        );
        assert_eq!(
            map_operation_to_title("rewrite-command"),
            "Tool Output Optimization"
        );
        assert_eq!(
            map_operation_to_title("compress-schema"),
            "Schema Compression"
        );
        assert_eq!(map_operation_to_title("compress-toon"), "TOON Encoding");
        assert_eq!(map_operation_to_title("other"), "Other Optimization");
    }

    #[test]
    fn test_map_operation_to_strategy_label() {
        assert_eq!(
            map_operation_to_strategy_label("compress-schema"),
            "Schema Compression"
        );
        assert_eq!(
            map_operation_to_strategy_label("compress-response"),
            "Response Compression"
        );
        assert_eq!(
            map_operation_to_strategy_label("rewrite-command"),
            "Command Rewrite"
        );
        assert_eq!(
            map_operation_to_strategy_label("compress-toon"),
            "TOON Encoding"
        );
        assert_eq!(
            map_operation_to_strategy_label("unknown"),
            "Other Optimization"
        );
    }

    // ─── Integration tests for handlers ───────────────────────────────────

    #[allow(clippy::await_holding_lock)]
    #[actix_web::test]
    async fn test_get_token_savings_no_stats_db() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let orig_home = std::env::var("HOME").ok();
        // When stats.db doesn't exist, handler should return stats_available=false
        let tmp = std::env::temp_dir().join(format!("agentsight_test_{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let db_path = setup_genai_db(&tmp);

        // Point HOME to a dir without .tokenless/stats.db
        let fake_home = tmp.join("fakehome");
        std::fs::create_dir_all(&fake_home).unwrap();
        unsafe { std::env::set_var("HOME", &fake_home) };

        let state = make_app_state(db_path);
        let app = actix_test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .service(get_token_savings),
        )
        .await;

        let req = actix_test::TestRequest::get()
            .uri("/token-savings?start_ns=0&end_ns=9999999999999999")
            .to_request();
        let resp = actix_test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200);

        let body: serde_json::Value = actix_test::read_body_json(resp).await;
        assert_eq!(body["stats_available"], false);
        assert!(!body["sessions"].as_array().unwrap().is_empty());

        // Restore HOME to avoid polluting other tests
        match orig_home {
            Some(v) => unsafe { std::env::set_var("HOME", v) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[allow(clippy::await_holding_lock)]
    #[actix_web::test]
    async fn test_get_token_savings_with_stats() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let orig_home = std::env::var("HOME").ok();
        let tmp =
            std::env::temp_dir().join(format!("agentsight_test_stats_{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let db_path = setup_genai_db(&tmp);
        let _stats_path = setup_stats_db(&tmp);

        // Point HOME to tmp so default_stats_path() finds .tokenless/stats.db
        unsafe { std::env::set_var("HOME", &tmp) };

        let state = make_app_state(db_path);
        let app = actix_test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .service(get_token_savings),
        )
        .await;

        let req = actix_test::TestRequest::get()
            .uri("/token-savings?start_ns=0&end_ns=9999999999999999")
            .to_request();
        let resp = actix_test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200);

        let body: serde_json::Value = actix_test::read_body_json(resp).await;
        assert_eq!(body["stats_available"], true);
        // Should have strategy_breakdown
        let breakdown = body["summary"]["strategy_breakdown"].as_array().unwrap();
        assert!(!breakdown.is_empty());
        // Check savings were computed
        let total_saved = body["summary"]["total_saved_tokens"].as_i64().unwrap();
        assert!(total_saved > 0);

        // Rates are fractions in [0.0, 1.0]: 2200 saved / 2700 total tokens
        let expected_rate = 2200.0 / 2700.0;
        let rate = body["summary"]["savings_rate"].as_f64().unwrap();
        assert!((rate - expected_rate).abs() < 1e-9, "got {rate}");
        let compounded_rate = body["summary"]["compounded_savings_rate"].as_f64().unwrap();
        assert!(
            (compounded_rate - expected_rate).abs() < 1e-9,
            "got {compounded_rate}"
        );
        let session_rate = body["sessions"][0]["savings_rate"].as_f64().unwrap();
        assert!(
            (session_rate - expected_rate).abs() < 1e-9,
            "got {session_rate}"
        );

        // Restore HOME
        match orig_home {
            Some(v) => unsafe { std::env::set_var("HOME", v) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[allow(clippy::await_holding_lock)]
    #[actix_web::test]
    async fn test_get_session_savings_not_found() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let orig_home = std::env::var("HOME").ok();
        let tmp = std::env::temp_dir().join(format!("agentsight_test_sess_{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let db_path = setup_genai_db(&tmp);

        let fake_home = tmp.join("fakehome2");
        std::fs::create_dir_all(&fake_home).unwrap();
        unsafe { std::env::set_var("HOME", &fake_home) };

        let state = make_app_state(db_path);
        let app = actix_test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .service(get_session_savings),
        )
        .await;

        let req = actix_test::TestRequest::get()
            .uri("/token-savings/session/nonexistent")
            .to_request();
        let resp = actix_test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200);

        let body: serde_json::Value = actix_test::read_body_json(resp).await;
        assert_eq!(body["stats_available"], false);
        assert_eq!(body["session_id"], "nonexistent");

        // Restore HOME
        match orig_home {
            Some(v) => unsafe { std::env::set_var("HOME", v) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[allow(clippy::await_holding_lock)]
    #[actix_web::test]
    async fn test_get_session_savings_with_data() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let orig_home = std::env::var("HOME").ok();
        let tmp =
            std::env::temp_dir().join(format!("agentsight_test_sess_data_{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let db_path = setup_genai_db(&tmp);
        let _stats_path = setup_stats_db(&tmp);

        unsafe { std::env::set_var("HOME", &tmp) };

        let state = make_app_state(db_path);
        let app = actix_test::init_service(
            App::new()
                .app_data(web::Data::new(state))
                .service(get_session_savings),
        )
        .await;

        let req = actix_test::TestRequest::get()
            .uri("/token-savings/session/sess-1")
            .to_request();
        let resp = actix_test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200);

        let body: serde_json::Value = actix_test::read_body_json(resp).await;
        assert_eq!(body["stats_available"], true);
        assert_eq!(body["session_id"], "sess-1");
        let items = body["items"].as_array().unwrap();
        assert!(!items.is_empty());
        // Verify strategy fields are present
        assert!(items[0]["strategy"].as_str().is_some());
        assert!(items[0]["strategy_label"].as_str().is_some());
        let compounded = body["total_compounded_saved"].as_i64().unwrap();
        assert!(compounded > 0);
        // Rate is a fraction in [0.0, 1.0]: 2200 compounded saved / 2700 total tokens
        let rate = body["savings_rate"].as_f64().unwrap();
        assert!((rate - 2200.0 / 2700.0).abs() < 1e-9, "got {rate}");

        // Restore HOME
        match orig_home {
            Some(v) => unsafe { std::env::set_var("HOME", v) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // ─── Unit tests for generate_optimization_reason ────────────────────

    #[test]
    fn test_generate_optimization_reason_compress_response() {
        let reason = generate_optimization_reason("compress-response", 1000, 400);
        assert!(reason.contains("MCP"));
        assert!(reason.contains("60%"));
        assert!(reason.contains("600 tokens"));
    }

    #[test]
    fn test_generate_optimization_reason_rewrite_command() {
        let reason = generate_optimization_reason("rewrite-command", 500, 200);
        assert!(reason.contains("Tool call"));
        assert!(reason.contains("60%"));
    }

    #[test]
    fn test_generate_optimization_reason_compress_schema() {
        let reason = generate_optimization_reason("compress-schema", 800, 300);
        assert!(reason.contains("Schema"));
        assert!(reason.contains("63%"));
    }

    #[test]
    fn test_generate_optimization_reason_compress_toon() {
        let reason = generate_optimization_reason("compress-toon", 2000, 500);
        assert!(reason.contains("TOON"));
        assert!(reason.contains("75%"));
    }

    #[test]
    fn test_generate_optimization_reason_unknown() {
        let reason = generate_optimization_reason("unknown-op", 100, 50);
        assert!(reason.contains("optimized"));
        assert!(reason.contains("50%"));
    }

    #[test]
    fn test_generate_optimization_reason_zero_before() {
        let reason = generate_optimization_reason("compress-response", 0, 0);
        assert!(reason.contains("0%"));
    }

    // ─── Unit tests for compute_diff_lines ────────────────────────────

    #[test]
    fn test_diff_empty_inputs() {
        let result = compute_diff_lines(None, None);
        assert!(result.is_empty());
        let result2 = compute_diff_lines(Some(""), Some(""));
        assert!(result2.is_empty());
    }

    #[test]
    fn test_diff_add_only() {
        let result = compute_diff_lines(Some(""), Some("new line"));
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].line_type, "add");
        assert_eq!(result[0].content, "new line");
    }

    #[test]
    fn test_diff_remove_only() {
        let result = compute_diff_lines(Some("old line"), Some(""));
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].line_type, "remove");
        assert_eq!(result[0].content, "old line");
    }

    #[test]
    fn test_diff_mixed_changes() {
        let before = "line1\nline2\nline3";
        let after = "line1\nmodified\nline3";
        let result = compute_diff_lines(Some(before), Some(after));
        let types: Vec<&str> = result.iter().map(|l| l.line_type.as_str()).collect();
        assert!(types.contains(&"remove"));
        assert!(types.contains(&"add"));
        assert!(types.contains(&"context"));
    }

    #[test]
    fn test_diff_identical_texts() {
        let text = "line1\nline2\nline3";
        let result = compute_diff_lines(Some(text), Some(text));
        assert!(result.is_empty());
    }

    #[test]
    fn test_diff_context_lines_limited() {
        // Two changes far apart should produce a separator in between
        let mut before_lines = vec!["common"; 20];
        before_lines[2] = "old_a";
        before_lines[17] = "old_b";
        let mut after_lines = vec!["common"; 20];
        after_lines[2] = "new_a";
        after_lines[17] = "new_b";
        let before = before_lines.join("\n");
        let after = after_lines.join("\n");
        let result = compute_diff_lines(Some(&before), Some(&after));
        let has_sep = result.iter().any(|l| l.line_type == "separator");
        assert!(has_sep, "should have separator between distant changes");
    }

    #[test]
    fn test_diff_no_dangling_separator_at_start_end() {
        let before = "changed_first\nA\nB\nC\nD\nE\nF\nG\nH\nchanged_last";
        let after = "NEW_first\nA\nB\nC\nD\nE\nF\nG\nH\nNEW_last";
        let result = compute_diff_lines(Some(before), Some(after));
        if let Some(first) = result.first() {
            assert_ne!(first.line_type, "separator");
        }
        if let Some(last) = result.last() {
            assert_ne!(last.line_type, "separator");
        }
    }

    // ─── Unit tests for helper functions ─────────────────────────────────

    fn make_session_for_tips(compounded_saved: i64, total_tokens: i64) -> SessionSavingsDto {
        SessionSavingsDto {
            session_id: "sess-1".to_string(),
            agent_name: "TestAgent".to_string(),
            total_input_tokens: total_tokens / 2,
            total_output_tokens: total_tokens / 2,
            total_tokens,
            baseline_tokens: total_tokens + compounded_saved,
            saved_tokens: compounded_saved,
            compounded_saved,
            savings_rate: 0.0,
            compounded_savings_rate: 0.0,
            request_count: 1,
            tool_saved: 0,
            mcp_saved: 0,
            optimization_items: vec![],
        }
    }

    #[test]
    fn test_diff_replace_remove_before_add() {
        let before = "old_line";
        let after = "new_line";
        let result = compute_diff_lines(Some(before), Some(after));
        let first_remove = result.iter().position(|l| l.line_type == "remove");
        let first_add = result.iter().position(|l| l.line_type == "add");
        assert!(
            first_remove < first_add || first_add.is_none(),
            "removes should come before adds"
        );
    }

    #[test]
    fn test_compute_compression_ratio_normal() {
        let ratio = compute_compression_ratio(1000, 250);
        assert!((ratio - 75.0).abs() < 0.01);
    }

    #[test]
    fn test_compute_compression_ratio_zero_before() {
        assert_eq!(compute_compression_ratio(0, 100), 0.0);
    }

    #[test]
    fn test_compute_compression_ratio_no_compression() {
        let ratio = compute_compression_ratio(500, 500);
        assert!((ratio - 0.0).abs() < 0.01);
    }

    #[test]
    fn test_compute_compression_ratio_negative_clamped() {
        let ratio = compute_compression_ratio(100, 200);
        assert_eq!(ratio, 0.0);
    }

    #[test]
    fn test_build_explanation_mcp_response() {
        let explanation = build_explanation("mcp_response", 1000, 200, 80.0, 3, 2400);
        assert!(explanation.contains("MCP"));
        assert!(explanation.contains("1000"));
        assert!(explanation.contains("200"));
        assert!(explanation.contains("80.0%"));
        assert!(explanation.contains("3"));
        assert!(explanation.contains("2400"));
    }

    #[test]
    fn test_build_explanation_tool_output() {
        let explanation = build_explanation("tool_output", 500, 100, 80.0, 2, 800);
        assert!(explanation.contains("Tool Output"));
        assert!(explanation.contains("500"));
        assert!(explanation.contains("100"));
    }

    #[test]
    fn test_tips_stats_unavailable() {
        let tips = generate_optimization_tips(false, 0, 0.0, 0, 0, &[]);
        assert_eq!(tips.len(), 1);
        assert_eq!(tips[0].level, "warning");
        assert!(tips[0].title.contains("Tokenless"));
    }

    #[test]
    fn test_tips_low_savings_rate() {
        let tips = generate_optimization_tips(true, 10000, 0.10, 100, 200, &[]);
        assert!(
            tips.iter()
                .any(|t| t.level == "warning" && t.title.contains("Savings rate"))
        );
    }

    #[test]
    fn test_diff_large_input_fallback() {
        let big_before = (0..1200)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let big_after = (0..1000)
            .map(|i| format!("new{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let result = compute_diff_lines(Some(&big_before), Some(&big_after));
        // Should use fallback (all remove then all add), not LCS
        assert!(!result.is_empty());
        assert!(result.len() <= 200);
        // All entries should be remove or add (no context in fallback)
        for line in &result {
            assert!(
                line.line_type == "remove" || line.line_type == "add",
                "fallback should only have remove/add"
            );
        }
    }

    #[test]
    fn test_diff_max_lines_cap() {
        // Create input that would generate many diff lines
        let before = (0..150)
            .map(|i| format!("old_{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let after = (0..150)
            .map(|i| format!("new_{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let result = compute_diff_lines(Some(&before), Some(&after));
        assert!(result.len() <= 200);
    }

    #[test]
    fn test_tips_boundary_at_15_no_warning() {
        let tips = generate_optimization_tips(true, 10000, 0.15, 1000, 500, &[]);
        assert!(
            !tips
                .iter()
                .any(|t| t.level == "warning" && t.title.contains("Savings rate"))
        );
        assert!(tips.iter().any(|t| t.level == "success"));
    }

    #[test]
    fn test_tips_boundary_at_30_excellent() {
        let tips = generate_optimization_tips(true, 10000, 0.30, 2000, 1000, &[]);
        assert!(
            tips.iter()
                .any(|t| t.level == "success" && t.title.contains("Excellent"))
        );
    }

    #[test]
    fn test_tips_boundary_just_below_15_warning() {
        let tips = generate_optimization_tips(true, 10000, 0.149, 500, 500, &[]);
        assert!(
            tips.iter()
                .any(|t| t.level == "warning" && t.title.contains("Savings rate"))
        );
    }

    #[test]
    fn test_tips_only_tool_saved_suggest_mcp() {
        let tips = generate_optimization_tips(true, 10000, 0.10, 500, 0, &[]);
        assert!(
            tips.iter()
                .any(|t| t.level == "info" && t.title.contains("MCP"))
        );
    }

    #[test]
    fn test_tips_only_mcp_saved_suggest_tool() {
        let tips = generate_optimization_tips(true, 10000, 0.10, 0, 500, &[]);
        assert!(
            tips.iter()
                .any(|t| t.level == "info" && t.title.contains("tool-output"))
        );
    }

    #[test]
    fn test_tips_zero_savings_sessions() {
        let sessions = vec![
            make_session_for_tips(0, 5000),
            make_session_for_tips(200, 3000),
        ];
        let tips = generate_optimization_tips(true, 8000, 0.10, 100, 100, &sessions);
        assert!(tips.iter().any(|t| t.title.contains("1")));
    }

    #[test]
    fn test_tips_excellent_rate() {
        let tips = generate_optimization_tips(true, 10000, 0.35, 2000, 1500, &[]);
        assert!(
            tips.iter()
                .any(|t| t.level == "success" && t.title.contains("Excellent"))
        );
    }

    #[test]
    fn test_tips_good_rate() {
        let tips = generate_optimization_tips(true, 10000, 0.20, 1000, 1000, &[]);
        assert!(
            tips.iter()
                .any(|t| t.level == "success" && t.title.contains("Good"))
        );
    }

    #[test]
    fn test_tips_empty_when_no_data() {
        let tips = generate_optimization_tips(true, 0, 0.0, 0, 0, &[]);
        assert!(tips.is_empty());
    }
}
