//! Episodic memory extraction — identifies tool-call chains that form
//! coherent task episodes from session audit logs.
//!
//! An "episode" is a sequence of tool calls that together describe:
//! "what the agent tried → what happened → what was the outcome".

use serde::{Deserialize, Serialize};

use super::fact::{ConsolidatedFact, FactCategory};
use super::heuristics::OwnedAuditEntry;

/// A single step within an episode.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpisodeStep {
    pub step: usize,
    pub tool: String,
    /// Brief description of the input (query, path, etc.).
    pub input: String,
    /// Brief description of the result.
    pub result: String,
}

/// Outcome of an episode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EpisodeOutcome {
    Success,
    Partial,
    Failed,
    Promoted,
    Recovered,
}

impl std::fmt::Display for EpisodeOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EpisodeOutcome::Success => write!(f, "success"),
            EpisodeOutcome::Partial => write!(f, "partial"),
            EpisodeOutcome::Failed => write!(f, "failed"),
            EpisodeOutcome::Promoted => write!(f, "promoted"),
            EpisodeOutcome::Recovered => write!(f, "recovered"),
        }
    }
}

/// A complete episode — a coherent task sequence from the session log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Episode {
    pub id: String,
    pub session_id: String,
    /// What triggered this episode (first tool call description).
    pub trigger: String,
    pub chain: Vec<EpisodeStep>,
    pub outcome: EpisodeOutcome,
    /// Duration in seconds from first to last step.
    pub duration_secs: u64,
    pub error_count: usize,
    pub created_at: String,
}

impl Episode {
    pub fn new(
        session_id: &str,
        trigger: String,
        chain: Vec<EpisodeStep>,
        outcome: EpisodeOutcome,
        error_count: usize,
        duration_secs: u64,
    ) -> Self {
        let id = ulid::Ulid::new().to_string();
        let now = chrono::Utc::now().to_rfc3339();

        Self {
            id,
            session_id: session_id.to_string(),
            trigger,
            chain,
            outcome,
            duration_secs,
            error_count,
            created_at: now,
        }
    }

    /// Serialize as markdown with YAML frontmatter.
    pub fn to_markdown(&self) -> String {
        let mut out = String::from("---\n");
        out.push_str(&format!("id: {}\n", self.id));
        out.push_str(&format!("session_id: {}\n", self.session_id));
        out.push_str(&format!("trigger: {}\n", self.sanitize(&self.trigger)));
        out.push_str(&format!("outcome: {}\n", self.outcome));
        out.push_str(&format!("error_count: {}\n", self.error_count));
        out.push_str(&format!("duration_secs: {}\n", self.duration_secs));
        out.push_str(&format!("created_at: {}\n", self.created_at));
        out.push_str("---\n\n");

        for step in &self.chain {
            out.push_str(&format!("## Step {}: {}\n", step.step, step.tool));
            if !step.input.is_empty() {
                out.push_str(&format!("- Input: `{}`\n", step.input));
            }
            if !step.result.is_empty() {
                out.push_str(&format!("- Result: {}\n", step.result));
            }
            out.push('\n');
        }

        out
    }

    /// Serialize as a single JSONL line.
    pub fn to_jsonl(&self) -> std::result::Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// Convert to a ConsolidatedFact for integration with consolidation.
    pub fn to_fact(&self) -> ConsolidatedFact {
        let chain_summary = self
            .chain
            .iter()
            .map(|s| format!("{}: {}", s.tool, s.input))
            .collect::<Vec<_>>()
            .join(" → ");

        let content = format!(
            "Episode ({} steps, {}): {}\n\n{}\n\nChain: {}",
            self.chain.len(),
            self.outcome,
            self.trigger,
            self.chain
                .iter()
                .map(|s| format!("Step {}: {} → {}", s.step, s.tool, s.result))
                .collect::<Vec<_>>()
                .join("\n"),
            chain_summary,
        );

        ConsolidatedFact::new(
            &self.session_id,
            FactCategory::Episodic,
            format!("情景: {}", self.trigger),
            content,
            "episodic".into(),
            self.chain
                .iter()
                .filter_map(|s| {
                    if s.input.starts_with('/') || s.input.contains('/') {
                        Some(s.input.clone())
                    } else {
                        None
                    }
                })
                .collect(),
            match self.outcome {
                EpisodeOutcome::Success | EpisodeOutcome::Promoted | EpisodeOutcome::Recovered => {
                    0.9
                }
                EpisodeOutcome::Partial => 0.7,
                EpisodeOutcome::Failed => 0.5,
            },
        )
    }

    /// Sanitize a value for safe frontmatter inclusion.
    /// Replaces newlines and ASCII control chars with spaces.
    /// Does NOT apply YAML double-quoting or backslash escaping:
    /// the hand-rolled frontmatter readers do not interpret those.
    fn sanitize(&self, s: &str) -> String {
        s.chars()
            .map(|c| match c {
                '\n' | '\r' => ' ',
                c if c.is_ascii_control() => ' ',
                other => other,
            })
            .collect()
    }
}

/// Extract episodes from a sequence of audit entries.
/// Returns up to `max_episodes` episodes.
/// `session_id` is the actual session identifier, propagated to every
/// episode so that facts derived from them carry the correct ownership.
pub fn extract_episodes(
    entries: &[OwnedAuditEntry],
    session_id: &str,
    min_steps: usize,
    max_episodes: usize,
) -> Vec<Episode> {
    let mut episodes = Vec::new();

    // Pattern 1: search → read → edit → read (edit-verify cycle)
    episodes.extend(extract_edit_cycle(entries, session_id));

    // Pattern 2: read → write(scratch) → promote
    episodes.extend(extract_promote_chain(entries, session_id));

    // Pattern 3: error → retry → success
    episodes.extend(extract_error_recovery(entries, session_id));

    // Pattern 4: general task chains (≥min_steps consecutive diverse calls)
    episodes.extend(extract_general_chains(entries, session_id, min_steps));

    // Deduplicate overlapping episodes (prefer longer ones)
    episodes.sort_by_key(|e| std::cmp::Reverse(e.chain.len()));
    episodes.truncate(max_episodes);

    episodes
}

// ── Pattern 1: Edit-Verify Cycle ──────────────────────────────────
// search/grep → read → edit → read

fn extract_edit_cycle(entries: &[OwnedAuditEntry], session_id: &str) -> Vec<Episode> {
    let mut episodes = Vec::new();
    let mut i = 0;

    while i + 3 < entries.len() {
        // Look for search/grep
        let is_search = entries[i].tool == "memory_search" || entries[i].tool == "mem_grep";
        if !is_search {
            i += 1;
            continue;
        }

        // Check subsequent: read → edit → read
        let j = i + 1;
        if j >= entries.len() || !is_read(&entries[j]) {
            i += 1;
            continue;
        }

        let k = j + 1;
        if k >= entries.len() || entries[k].tool != "mem_edit" {
            i += 1;
            continue;
        }

        let l = k + 1;
        if l >= entries.len() || !is_read(&entries[l]) {
            i += 1;
            continue;
        }

        // Found a full cycle: search → read → edit → read
        let chain = vec![
            make_step(1, &entries[i]),
            make_step(2, &entries[j]),
            make_step(3, &entries[k]),
            make_step(4, &entries[l]),
        ];

        let outcome = if entries[l].ok {
            EpisodeOutcome::Success
        } else {
            EpisodeOutcome::Partial
        };

        let error_count = entries[i..=l].iter().filter(|e| !e.ok).count();
        let trigger = format!("{} {}", entries[i].tool, entries[i].path);
        let duration = duration_between(&entries[i].ts, &entries[l].ts);

        episodes.push(Episode::new(
            session_id,
            trigger,
            chain,
            outcome,
            error_count,
            duration,
        ));

        i = l + 1; // Skip past this episode
    }

    episodes
}

// ── Pattern 2: Promote Chain ──────────────────────────────────────
// read → write(scratch/) → promote

fn extract_promote_chain(entries: &[OwnedAuditEntry], session_id: &str) -> Vec<Episode> {
    let mut episodes = Vec::new();
    let mut i = 0;

    while i < entries.len() {
        if entries[i].tool != "mem_promote" {
            i += 1;
            continue;
        }

        // Look backward for a scratch write and a prior read
        let mut read_idx = None;
        let mut write_idx = None;
        for j in (0..i).rev() {
            if entries[j].path.starts_with("scratch/")
                && entries[j].tool == "mem_write"
                && write_idx.is_none()
            {
                write_idx = Some(j);
            } else if is_read(&entries[j]) && write_idx.is_some() && read_idx.is_none() {
                // Read must be BEFORE the write
                read_idx = Some(j);
                break;
            }
        }

        if let (Some(ri), Some(wi)) = (read_idx, write_idx) {
            let chain = vec![
                make_step(1, &entries[ri]),
                make_step(2, &entries[wi]),
                make_step(3, &entries[i]),
            ];

            let trigger = format!("promote: {}", entries[i].path);
            let duration = duration_between(&entries[ri].ts, &entries[i].ts);
            episodes.push(Episode::new(
                session_id,
                trigger,
                chain,
                EpisodeOutcome::Promoted,
                0,
                duration,
            ));
        }

        i += 1;
    }

    episodes
}

// ── Pattern 3: Error Recovery ─────────────────────────────────────
// tool(error) → tool(ok) or different_tool(ok)

fn extract_error_recovery(entries: &[OwnedAuditEntry], session_id: &str) -> Vec<Episode> {
    let mut episodes = Vec::new();

    for i in 0..entries.len().saturating_sub(1) {
        if entries[i].ok || entries[i].error.is_none() {
            continue;
        }

        // Look for a successful retry within next 3 entries
        for j in (i + 1)..(i + 4).min(entries.len()) {
            if entries[j].ok {
                let chain = vec![make_step(1, &entries[i]), make_step(2, &entries[j])];
                let trigger = format!("error recovery: {}", entries[i].tool);
                let duration = duration_between(&entries[i].ts, &entries[j].ts);
                episodes.push(Episode::new(
                    session_id,
                    trigger,
                    chain,
                    EpisodeOutcome::Recovered,
                    1,
                    duration,
                ));
                break;
            }
        }
    }

    episodes
}

// ── Pattern 4: General Task Chains ────────────────────────────────
// ≥min_steps consecutive diverse tool calls

fn extract_general_chains(
    entries: &[OwnedAuditEntry],
    session_id: &str,
    min_steps: usize,
) -> Vec<Episode> {
    let mut episodes = Vec::new();

    // Find contiguous blocks of "interesting" tools (not mem_list, not repeated reads)
    let mut chain_start = None;
    let mut current_chain: Vec<usize> = Vec::new();
    let mut last_tool = String::new();
    let mut last_path = String::new();

    for (i, e) in entries.iter().enumerate() {
        let is_noise = e.tool == "mem_list"
            || (e.tool == "mem_read" && e.path == last_path && last_tool == "mem_read");

        if is_noise {
            // End current chain if long enough
            if current_chain.len() >= min_steps && chain_start.is_some() {
                let chain: Vec<_> = current_chain
                    .iter()
                    .enumerate()
                    .map(|(step, &idx)| make_step(step + 1, &entries[idx]))
                    .collect();
                let outcome = if current_chain.iter().all(|&idx| entries[idx].ok) {
                    EpisodeOutcome::Success
                } else {
                    EpisodeOutcome::Partial
                };
                let error_count = current_chain
                    .iter()
                    .filter(|&&idx| !entries[idx].ok)
                    .count();
                let trigger = format!("task chain: {} tools", current_chain.len());
                let start_idx: usize = *current_chain.first().unwrap();
                let end_idx: usize = *current_chain.last().unwrap();
                let duration = duration_between(&entries[start_idx].ts, &entries[end_idx].ts);
                episodes.push(Episode::new(
                    session_id,
                    trigger,
                    chain,
                    outcome,
                    error_count,
                    duration,
                ));
            }
            current_chain.clear();
            chain_start = None;
            continue;
        }

        if chain_start.is_none() {
            chain_start = Some(i);
        }
        current_chain.push(i);
        last_tool = e.tool.clone();
        last_path = e.path.clone();
    }

    // Final chain
    if current_chain.len() >= min_steps && chain_start.is_some() {
        let chain: Vec<_> = current_chain
            .iter()
            .enumerate()
            .map(|(step, &idx)| make_step(step + 1, &entries[idx]))
            .collect();
        let outcome = if current_chain.iter().all(|&idx| entries[idx].ok) {
            EpisodeOutcome::Success
        } else {
            EpisodeOutcome::Partial
        };
        let error_count = current_chain
            .iter()
            .filter(|&&idx| !entries[idx].ok)
            .count();
        let trigger = format!("task chain: {} tools", current_chain.len());
        let start_idx: usize = *current_chain.first().unwrap();
        let end_idx: usize = *current_chain.last().unwrap();
        let duration = duration_between(&entries[start_idx].ts, &entries[end_idx].ts);
        episodes.push(Episode::new(
            session_id,
            trigger,
            chain,
            outcome,
            error_count,
            duration,
        ));
    }

    episodes
}

fn is_read(e: &OwnedAuditEntry) -> bool {
    e.tool == "mem_read"
}

/// Calculate the duration in seconds between two RFC3339 timestamp strings.
/// Returns 0 on parse failure (best-effort).
fn duration_between(before: &str, after: &str) -> u64 {
    if let (Ok(a), Ok(b)) = (
        chrono::DateTime::parse_from_rfc3339(before),
        chrono::DateTime::parse_from_rfc3339(after),
    ) {
        b.signed_duration_since(a).num_seconds().max(0) as u64
    } else {
        0
    }
}

fn make_step(step: usize, entry: &OwnedAuditEntry) -> EpisodeStep {
    let input = if !entry.path.is_empty() {
        entry.path.clone()
    } else {
        String::new()
    };

    let result = if entry.ok {
        if let Some(bytes) = entry.bytes {
            format!("{} bytes", bytes)
        } else {
            "ok".to_string()
        }
    } else {
        entry.error.clone().unwrap_or_else(|| "failed".to_string())
    };

    EpisodeStep {
        step,
        tool: entry.tool.clone(),
        input,
        result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn make_entry(
        tool: &str,
        path: &str,
        ok: bool,
        error: Option<&str>,
        bytes: Option<u64>,
    ) -> OwnedAuditEntry {
        OwnedAuditEntry {
            ts: Utc::now().to_rfc3339(),
            tool: tool.to_string(),
            path: path.to_string(),
            ok,
            bytes,
            error: error.map(String::from),
            trace_id: None,
        }
    }

    #[test]
    fn detects_edit_verify_cycle() {
        let entries = vec![
            make_entry("memory_search", "bm25:rust config", true, None, Some(3)),
            make_entry("mem_read", "src/config.rs", true, None, Some(200)),
            make_entry("mem_edit", "src/config.rs", true, None, Some(1)),
            make_entry("mem_read", "src/config.rs", true, None, Some(200)),
        ];
        let episodes = extract_edit_cycle(&entries, "test-sid");
        assert_eq!(episodes.len(), 1);
        assert_eq!(episodes[0].chain.len(), 4);
        assert_eq!(episodes[0].outcome, EpisodeOutcome::Success);
    }

    #[test]
    fn detects_promote_chain() {
        let entries = vec![
            make_entry("mem_read", "notes/template.md", true, None, Some(100)),
            make_entry("mem_write", "scratch/new.md", true, None, Some(150)),
            make_entry(
                "mem_promote",
                "scratch/new.md -> notes/new.md",
                true,
                None,
                Some(150),
            ),
        ];
        let episodes = extract_promote_chain(&entries, "test-sid");
        assert_eq!(episodes.len(), 1);
        assert_eq!(episodes[0].chain.len(), 3);
        assert_eq!(episodes[0].outcome, EpisodeOutcome::Promoted);
    }

    #[test]
    fn detects_error_recovery() {
        let entries = vec![
            make_entry(
                "mem_write",
                "notes/x.md",
                false,
                Some("already exists"),
                None,
            ),
            make_entry("mem_write", "notes/x.md", true, None, Some(100)),
        ];
        let episodes = extract_error_recovery(&entries, "test-sid");
        assert_eq!(episodes.len(), 1);
        assert_eq!(episodes[0].outcome, EpisodeOutcome::Recovered);
    }

    #[test]
    fn episode_to_fact() {
        let entries = vec![
            make_entry("memory_search", "bm25:rust config", true, None, Some(3)),
            make_entry("mem_read", "src/config.rs", true, None, Some(200)),
            make_entry("mem_edit", "src/config.rs", true, None, Some(1)),
            make_entry("mem_read", "src/config.rs", true, None, Some(200)),
        ];
        let episodes = extract_edit_cycle(&entries, "test-sid");
        assert_eq!(episodes.len(), 1);
        let fact = episodes[0].to_fact();
        assert_eq!(fact.category, FactCategory::Episodic);
        assert!(
            fact.title.contains("情景") || fact.content.contains("episodic"),
            "fact title: {} content: {}",
            fact.title,
            &fact.content[..100.min(fact.content.len())]
        );
        assert!(fact.confidence >= 0.9);
    }

    #[test]
    fn episode_markdown_has_frontmatter() {
        let entries = vec![
            make_entry("memory_search", "bm25:rust", true, None, Some(3)),
            make_entry("mem_read", "src/lib.rs", true, None, Some(100)),
            make_entry("mem_edit", "src/lib.rs", true, None, Some(1)),
            make_entry("mem_read", "src/lib.rs", true, None, Some(100)),
        ];
        let episodes = extract_edit_cycle(&entries, "test-sid");
        let md = episodes[0].to_markdown();
        assert!(md.starts_with("---\n"));
        assert!(md.contains("outcome: success"));
        assert!(md.contains("## Step 1: memory_search"));
    }
}
