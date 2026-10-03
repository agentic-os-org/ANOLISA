use super::resolve_candidate_identities;
use crate::security::ContainmentCandidate;

#[test]
fn candidate_identity_resolution_preserves_distinct_processes_for_one_agent() {
    let candidates = resolve_candidate_identities(vec![
        candidate("agent-a", 20, 200, "Zulu"),
        candidate("agent-a", 10, 100, "Alpha"),
        candidate("agent-a", 10, 100, "Duplicate"),
    ]);

    assert_eq!(candidates.len(), 2);
    assert_eq!(candidates[0], candidate("agent-a", 10, 100, "Alpha"));
    assert_eq!(candidates[1], candidate("agent-a", 20, 200, "Zulu"));
}

#[test]
fn candidate_identity_resolution_rejects_inconsistent_reuse_of_one_pid() {
    let candidates = resolve_candidate_identities(vec![
        candidate("agent-a", 10, 100, "Agent A"),
        candidate("agent-b", 10, 100, "Agent B"),
        candidate("agent-c", 20, 200, "Agent C"),
        candidate("agent-c", 30, 300, "Agent C"),
        candidate("agent-c", 30, 301, "Agent C reused"),
    ]);

    assert_eq!(candidates, vec![candidate("agent-c", 20, 200, "Agent C")]);
}

fn candidate(
    agent_id: &str,
    root_pid: i32,
    process_start_time: u64,
    display_name: &str,
) -> ContainmentCandidate {
    ContainmentCandidate {
        agent_id: agent_id.into(),
        root_pid,
        process_start_time,
        display_name: display_name.into(),
    }
}
