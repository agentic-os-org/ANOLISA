//! Task proposal admission, repair and review notes with a scripted model.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use asc_intent::{
    COVERAGE_NOTE, HUMAN_GATE_NOTE, LAUNCH_VIEW_NOTE, PolicyError, ProposalContext, ProposalError,
    ProposalOutcome, RUNTIME_VIEW_NOTE, TaskInput, confirmation_gaps, human_gates,
    launch_view_gaps, prepare_policy, prepare_task_policy, propose_task, requirement_coverage,
    runtime_view_gaps,
};
use asc_model_client::provider::{ModelProvider, ProviderError};
use serde_json::{Value, json};

fn body(policy: &str) -> String {
    json!({"policy": policy, "unresolved": []}).to_string()
}

struct Scripted {
    responses: Mutex<VecDeque<Result<String, ProviderError>>>,
    payloads: Mutex<Vec<String>>,
}

impl Scripted {
    fn new(responses: Vec<Result<String, ProviderError>>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            payloads: Mutex::new(Vec::new()),
        }
    }

    fn payloads(&self) -> Vec<String> {
        self.payloads.lock().unwrap().clone()
    }
}

impl ModelProvider for Scripted {
    fn complete(
        &self,
        _: &str,
        _: &str,
        payload: &str,
        _: Instant,
    ) -> Result<String, ProviderError> {
        self.payloads.lock().unwrap().push(payload.to_owned());
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Err(ProviderError::Transport))
    }
}

fn run(provider: &Scripted) -> Result<ProposalOutcome, ProposalError> {
    propose_task(
        provider,
        "fixture",
        &TaskInput {
            source_id: "source".into(),
            text: "Build locally".into(),
        },
        &ProposalContext::new("task-1".into(), 2_000_000_000, 1800).unwrap(),
        Instant::now() + Duration::from_secs(5),
    )
}

fn proposed_candidate(provider: &Scripted) -> Value {
    let outcome = run(provider).unwrap();
    assert!(matches!(outcome, ProposalOutcome::Candidate { .. }));
    serde_json::to_value(outcome).unwrap()["candidate"].clone()
}

fn launch_notes(candidate: &Value) -> Vec<String> {
    notes(candidate, LAUNCH_VIEW_NOTE)
}

fn notes(candidate: &Value, prefix: &str) -> Vec<String> {
    candidate["notes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .filter(|note| note.starts_with(prefix))
        .map(str::to_owned)
        .collect()
}

const CWD_SPAWN: &str = r#"// [S3] launch
@id("spawn")
permit(principal, action == SecCore::Action::"process.spawn", resource is SecCore::Process)
when { context.input.cwd == "/srv/demo" && resource.program == "/usr/bin/bash" };
// [H1] keep the rollback package
forbid(principal, action == SecCore::Action::"file.write", resource is SecCore::File)
when { resource.path == "/srv/demo/previous.zip"
    && context.input.operation == SecCore::WriteOperation::"delete" };"#;
const PLAIN_SPAWN: &str = r#"// [S3] launch
permit(principal, action == SecCore::Action::"process.spawn", resource is SecCore::Process)
when { resource.program == "/usr/bin/bash" };
// [H1] keep the rollback package
forbid(principal, action == SecCore::Action::"file.write", resource is SecCore::File)
when { resource.path == "/srv/demo/previous.zip"
    && context.input.operation == SecCore::WriteOperation::"delete" };"#;

#[test]
fn invocation_context_and_file_operation_context_use_different_pdp_contracts() {
    let gate = r#"@advice("{\"verdict\":\"escalate\",\"message\":\"Confirm this command\"}")
permit(principal is SecCore::Subject, action == SecCore::Action::"process.spawn", resource)
when { context.input.cwd == "/work" };"#;
    for action in ["process.spawn", "file.exec"] {
        let rule = gate.replace("process.spawn", action);
        assert!(launch_view_gaps(&rule).unwrap().is_empty());
        let candidate = proposed_candidate(&Scripted::new(vec![Ok(body(&rule))]));
        assert!(notes(&candidate, HUMAN_GATE_NOTE)[0].contains(action));
        assert!(notes(&candidate, COVERAGE_NOTE)[0].contains("PDP (launch approval)"));
    }
    let operation = PLAIN_SPAWN.replace("(principal,", "(principal is SecCore::Subject,");
    assert!(!launch_view_gaps(&operation).unwrap().is_empty());
}

#[test]
fn compatibility_findings_preserve_the_cwd_restriction_without_a_retry() {
    let provider = Scripted::new(vec![Ok(body(CWD_SPAWN)), Ok(body(PLAIN_SPAWN))]);
    let candidate = proposed_candidate(&provider);
    assert_eq!(candidate["draft"]["policy"], CWD_SPAWN);
    assert_eq!(candidate["artifact"]["text"], CWD_SPAWN);
    assert!(launch_notes(&candidate).is_empty());
    assert_eq!(provider.payloads().len(), 1);
}

#[test]
fn a_clean_draft_is_not_retried() {
    let provider = Scripted::new(vec![Ok(body(PLAIN_SPAWN))]);
    let candidate = proposed_candidate(&provider);
    assert_eq!(candidate["artifact"]["text"], PLAIN_SPAWN);
    assert_eq!(provider.payloads().len(), 1);
}

#[test]
fn task_candidate_is_independent_of_host_symlink_targets() {
    let root = std::env::temp_dir().join(format!("asc-intent-alias-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
    let (target, link) = (root.join("python3.11"), root.join("python3"));
    std::fs::write(&target, b"").unwrap();
    let _ = std::fs::remove_file(&link);
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let policy = format!(
        r#"// [S1] report link
permit(principal, action == SecCore::Action::"file.write", resource is SecCore::File)
when {{ resource.path == "{}" }};"#,
        link.display()
    );
    let provider = Scripted::new(vec![Ok(body(&policy))]);
    let linked = proposed_candidate(&provider);
    std::fs::remove_dir_all(&root).unwrap();
    let absent = proposed_candidate(&Scripted::new(vec![Ok(body(&policy))]));
    assert_eq!(linked["artifact"]["text"], policy);
    assert_eq!(linked, absent);
    assert!(!linked.to_string().contains(target.to_str().unwrap()));
}

#[test]
fn agentsight_checks_the_whole_projected_file_view() {
    let file = r#"forbid(principal, action == SecCore::Action::"file.read", resource is SecCore::File)
when { resource.path == "/config" };"#;
    assert!(runtime_view_gaps(file).unwrap().is_empty());
    assert!(requirement_coverage(file).unwrap()[0].contains("AgentSight file protection"));
    let annotated = format!("@id(\"R3\") {file}");
    let network = r#"forbid(principal, action == SecCore::Action::"network.use", resource);"#;
    let policy = format!("{network}\n{annotated}");
    let provider = Scripted::new(vec![Ok(body(&policy))]);
    let candidate = proposed_candidate(&provider);
    assert_eq!(provider.payloads().len(), 1);
    assert_eq!(candidate["artifact"]["text"], policy);
    assert!(notes(&candidate, RUNTIME_VIEW_NOTE)[0].contains("UNSUPPORTED_ANNOTATION"));
    assert!(!notes(&candidate, COVERAGE_NOTE)[0].contains("AgentSight file protection"));
    assert!(runtime_view_gaps(network).unwrap().is_empty());
}

#[test]
fn mixed_principal_headers_are_repaired_or_returned_for_clarification() {
    let mixed = format!("{PLAIN_SPAWN}\n{GATED_NETWORK}");
    let (result, calls) = try_responses(vec![Ok(body(&mixed)), Ok(body(GATED_NETWORK))]);
    assert!(matches!(result, Ok(ProposalOutcome::Candidate { .. })));
    assert!(calls[1].contains("consistent principal headers"));
    let (result, _) = try_responses(vec![Ok(body(&mixed)), Ok(body(&mixed))]);
    assert!(matches!(
        result,
        Ok(ProposalOutcome::NeedsClarification { .. })
    ));
}

#[test]
fn coverage_groups_statements_by_requirement_and_names_their_consumers() {
    let text = r#"@id("base")
permit(principal, action == SecCore::Action::"process.spawn", resource is SecCore::Process)
when { resource.program == "/usr/bin/bash" };
@id("R1.1")
permit(principal, action == SecCore::Action::"file.write", resource is SecCore::File)
when { resource.path == "/srv/demo/out" || resource.path like "/srv/demo/out/*" };
@id("R2")
forbid(principal, action == SecCore::Action::"file.write", resource is SecCore::File)
when { resource.path == "/srv/demo/previous.zip"
    && context.input.operation == SecCore::WriteOperation::"delete" };
@id("R1.2")
permit(principal, action == SecCore::Action::"file.write", resource is SecCore::File)
when { resource.path == "/srv/demo/tmp" && context.input.operation == SecCore::WriteOperation::"create" };
permit(principal, action == SecCore::Action::"file.read", resource is SecCore::File)
when { resource.path like "/*" };"#;
    let coverage = requirement_coverage(text).unwrap();
    assert!(
        coverage
            .iter()
            .any(|row| row.contains("R1") && row.contains("#2 #4"))
    );
    assert!(coverage.iter().all(|row| row.contains("sandbox input")));
    assert!(
        coverage
            .iter()
            .all(|row| !row.contains("AgentSight") && !row.contains("PDP"))
    );
}

#[test]
fn network_coverage_distinguishes_sandbox_projection_from_launch_approval() {
    for effect in ["permit", "forbid"] {
        let rule =
            format!("{effect}(principal, action == SecCore::Action::\"network.use\", resource);");
        assert_eq!(
            requirement_coverage(&rule).unwrap(),
            ["no @id  #1  sandbox input (network mode)"]
        );
        let list = rule.replace(
            "== SecCore::Action::\"network.use\"",
            "in [SecCore::Action::\"network.use\"]",
        );
        assert_eq!(
            requirement_coverage(&list).unwrap(),
            requirement_coverage(&rule).unwrap()
        );
        let eligible = rule.replace("(principal,", "(principal is SecCore::Subject,");
        assert_eq!(
            requirement_coverage(&eligible).unwrap(),
            ["no @id  #1  sandbox input (network mode), PDP"]
        );
    }
    let bare = GATED_NETWORK.replace("principal is SecCore::Subject,", "principal,");
    assert!(matches!(
        prepare_task_policy(&bare),
        Err(PolicyError::PrincipalHeaders)
    ));
}

/// Writes everything under `out/`, as a task that only names `out/` in its goal would.
const GOAL: &str = r#"@id("goal")
permit(principal, action == SecCore::Action::"file.write", resource is SecCore::File)
when { resource.path == "/srv/demo/out" || resource.path like "/srv/demo/out/*" };"#;

/// Asks before writing `out/`, which the goal already grants: friction, not a wider grant.
const GATED_OUT: &str = r#"@id("R2")
@advice("{\"verdict\":\"escalate\",\"message\":\"Ask before writing to out/\"}")
permit(principal, action == SecCore::Action::"file.write", resource is SecCore::File)
when { resource.path == "/srv/demo/out" || resource.path like "/srv/demo/out/*" };"#;

/// Asks before writing a directory no other permit grants.
const GATED_CONFIG: &str = r#"@id("R2")
@advice("{\"verdict\":\"escalate\",\"message\":\"Confirm before changing the manually maintained files in config/\"}")
permit(principal, action == SecCore::Action::"file.write", resource is SecCore::File)
when { resource.path == "/srv/demo/config" || resource.path like "/srv/demo/config/*" };"#;

const PLAIN_CONFIG: &str = r#"@id("goal")
permit(principal, action == SecCore::Action::"file.write", resource is SecCore::File)
when { resource.path == "/srv/demo/config" || resource.path like "/srv/demo/config/*" };"#;

fn gate(text: &str) -> String {
    format!("{GOAL}\n{text}").replace("(principal,", "(principal is SecCore::Subject,")
}

#[test]
fn a_gate_on_granted_paths_is_admitted_and_named() {
    let text = gate(GATED_OUT);
    let artifact = prepare_task_policy(&text).unwrap();
    assert!(confirmation_gaps(&artifact.text).unwrap().is_empty());
    let gates = human_gates(&artifact.text).unwrap();
    assert_eq!(gates.len(), 1);
    assert_eq!(gates[0].requirement, "R2");
    // A directory pair names one location, and that is what the prompt shows.
    assert_eq!(gates[0].paths, ["/srv/demo/out"]);
    assert_eq!(gates[0].message, "Ask before writing to out/");
    assert_eq!(gates[0].position, 2);
    // A command proposal grants one launch and keeps its own approval flow: no gate there.
    assert!(matches!(
        prepare_policy(&text),
        Err(PolicyError::UnsupportedPolicy)
    ));
}

#[test]
fn a_gate_outside_every_other_grant_is_widening() {
    let text = gate(GATED_CONFIG);
    let artifact = prepare_task_policy(&text).unwrap();
    assert_eq!(
        confirmation_gaps(&artifact.text).unwrap(),
        [
            r#"permit #2 @id("R2") asks before acting on /srv/demo/config, which no other permit grants (file.write), so the confirmation would widen authorization"#,
        ]
    );
    // Granting the same location first turns the same statement into a confirmation.
    assert!(
        confirmation_gaps(&gate(&format!("{PLAIN_CONFIG}\n{GATED_CONFIG}")))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn conditional_grants_do_not_establish_path_coverage() {
    for restricted in [
        GOAL.replace("when {", "unless {"),
        GOAL.replace("when {", "when { false && (")
            .replace(" };", ") };"),
    ] {
        assert!(
            !confirmation_gaps(&format!("{restricted}\n{GATED_OUT}"))
                .unwrap()
                .is_empty()
        );
    }
    assert!(
        !confirmation_gaps(&gate(&GATED_OUT.replace("when {", "unless {")))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_gate_that_tests_context_is_never_shown() {
    let text = gate(
        r#"@id("R2")
@advice("{\"verdict\":\"escalate\",\"message\":\"Ask each time\"}")
permit(principal, action == SecCore::Action::"file.write", resource is SecCore::File)
when { resource.path == "/srv/demo/out"
    && context.input.operation == SecCore::WriteOperation::"create" };"#,
    );
    let gaps = confirmation_gaps(&text).unwrap();
    assert_eq!(gaps.len(), 1);
    assert!(gaps[0].contains("cannot project"), "{gaps:?}");
}

#[test]
fn a_gate_must_name_exactly_one_action_and_a_path() {
    // One action and one path are what an approval prompt can show; anything else is not a gate.
    let many = gate(
        r#"@id("R2")
@advice("{\"verdict\":\"escalate\",\"message\":\"Ask first\"}")
permit(principal,
  action in [SecCore::Action::"file.write", SecCore::Action::"file.read"],
  resource is SecCore::File)
when { resource.path == "/srv/demo/out" };"#,
    );
    let mut gaps = confirmation_gaps(&many).unwrap();
    assert_eq!(gaps.len(), 1);
    assert!(
        gaps[0].contains("without naming exactly one action"),
        "{gaps:?}"
    );

    let pathless = gate(
        r#"@id("R2")
@advice("{\"verdict\":\"escalate\",\"message\":\"Ask first\"}")
permit(principal, action == SecCore::Action::"file.write", resource is SecCore::File);"#,
    );
    gaps = confirmation_gaps(&pathless).unwrap();
    assert_eq!(gaps.len(), 1);
    assert!(gaps[0].contains("without naming a path"), "{gaps:?}");
}

#[test]
fn only_one_escalation_contract_is_admitted() {
    for rejected in [
        // A forbid cannot carry a confirmation: it is already the stricter answer.
        r#"@id("R1")
@advice("{\"verdict\":\"escalate\",\"message\":\"Ask first\"}")
forbid(principal, action == SecCore::Action::"file.write", resource is SecCore::File)
when { resource.path == "/srv/demo/out" };"#,
        // No decision surface reads `warn` back to a human on this path.
        r#"@id("R1")
@advice("{\"verdict\":\"warn\",\"message\":\"Ask first\"}")
permit(principal, action == SecCore::Action::"file.write", resource is SecCore::File)
when { resource.path == "/srv/demo/out" };"#,
        // A prompt with no reason explains nothing, so the gate is not admitted.
        r#"@id("R1")
@advice("{\"verdict\":\"escalate\"}")
permit(principal, action == SecCore::Action::"file.write", resource is SecCore::File)
when { resource.path == "/srv/demo/out" };"#,
        // A task may not choose how its command is rewritten.
        r#"@id("R1")
@advice("{\"verdict\":\"escalate\",\"message\":\"Ask first\",\"transform\":\"$target.x\"}")
permit(principal, action == SecCore::Action::"file.write", resource is SecCore::File)
when { resource.path == "/srv/demo/out" };"#,
    ] {
        assert!(matches!(
            prepare_task_policy(&gate(rejected)),
            Err(PolicyError::UnsupportedPolicy | PolicyError::Advice)
        ));
    }
}

#[test]
fn a_widening_gate_requires_clarification_without_replacing_the_original_draft() {
    let original = gate(GATED_CONFIG);
    let provider = Scripted::new(vec![Ok(body(&original)), Ok(body(GOAL))]);
    let ProposalOutcome::NeedsClarification { draft, reasons } = run(&provider).unwrap() else {
        panic!("a widening gate must be reviewed");
    };
    assert_eq!(draft.policy(), original);
    assert!(
        reasons
            .iter()
            .any(|reason| reason.contains("would widen authorization"))
    );
    assert_eq!(provider.payloads().len(), 1);
}

#[test]
fn an_admitted_gate_travels_with_its_paths_and_message() {
    let provider = Scripted::new(vec![Ok(body(&gate(GATED_OUT)))]);
    let candidate = proposed_candidate(&provider);
    assert_eq!(provider.payloads().len(), 1);
    let gates = notes(&candidate, HUMAN_GATE_NOTE);
    assert!(gates[0].contains("before launching a command"));
    assert!(gates[0].contains(r#"#2 @id("R2") /srv/demo/out -- Ask before writing to out/"#));
}

/// Requests approval before launching a command with network access.
const GATED_NETWORK: &str = r#"@id("R2")
@advice("{\"verdict\":\"escalate\",\"message\":\"Allow network access for this command?\"}")
permit(principal is SecCore::Subject, action == SecCore::Action::"network.use",
       resource == SecCore::Network::"all");"#;

#[test]
fn a_whole_network_gate_needs_no_other_grant() {
    // Network has no path or endpoint dimension, so nothing narrower can grant it first; the gate
    // is a request for approval at command launch.
    let artifact = prepare_task_policy(GATED_NETWORK).unwrap();
    assert!(confirmation_gaps(&artifact.text).unwrap().is_empty());
    let gates = human_gates(&artifact.text).unwrap();
    assert_eq!(gates.len(), 1);
    assert_eq!(gates[0].requirement, "R2");
    assert!(gates[0].paths.is_empty());
    assert_eq!(gates[0].message, "Allow network access for this command?");

    let provider = Scripted::new(vec![Ok(body(GATED_NETWORK))]);
    let candidate = proposed_candidate(&provider);
    let gates = notes(&candidate, HUMAN_GATE_NOTE);
    assert_eq!(gates.len(), 1);
    assert!(
        gates[0].contains("#1 @id(\"R2\") network.use -- Allow network access for this command?"),
        "{gates:?}"
    );
    let coverage = notes(&candidate, "COVERAGE:");
    assert_eq!(coverage.len(), 1);
    assert!(
        coverage[0].ends_with("  R2  #1  PDP (launch approval)"),
        "{coverage:?}"
    );
}

const VALID: &str = r#"permit(principal, action == SecCore::Action::"file.write", resource is SecCore::File)
when { resource.path == "/srv/demo/out" || resource.path like "/srv/demo/out/*" };"#;
const SYNTAX: &str = r#"permit(principal action == SecCore::Action::"file.write", resource);"#;
const UNTYPED: &str = r#"permit(principal, action == SecCore::Action::"file.write", resource is SecCore::File)
when { resource.missing == "/srv/demo/out" };"#;
const WILDCARD: &str = r#"permit(principal, action == SecCore::Action::"file.write", resource is SecCore::File)
when { resource.path == "/srv/demo/out/*" };"#;
const UNSUPPORTED: &str = r#"@reason("demo")
permit(principal, action == SecCore::Action::"file.write", resource is SecCore::File)
when { resource.path == "/srv/demo/out" };"#;

fn try_responses(
    responses: Vec<Result<String, ProviderError>>,
) -> (Result<ProposalOutcome, ProposalError>, Vec<String>) {
    let provider = Scripted::new(responses);
    (run(&provider), provider.payloads())
}

#[test]
fn a_rejected_draft_is_repaired_by_one_retry() {
    for (first, reason) in [
        (SYNTAX, "syntax is invalid"),
        (UNTYPED, "failed Cedar validation"),
        (WILDCARD, "equality compares a concrete path"),
    ] {
        let (outcome, payloads) = try_responses(vec![Ok(body(first)), Ok(body(VALID))]);
        let Ok(ProposalOutcome::Candidate { candidate }) = outcome else {
            panic!("expected a candidate after the retry of {first}");
        };
        assert_eq!(candidate.policy_text(), VALID);
        assert_eq!(payloads.len(), 2);
        assert!(!payloads[0].contains("The previous response failed local validation"));
        assert!(payloads[1].contains("The previous response failed local validation"));
        assert!(payloads[1].contains(reason), "{}", payloads[1]);
    }
}

#[test]
fn a_persistent_rejection_keeps_its_outcome_after_the_retry() {
    let (outcome, payloads) = try_responses(vec![Ok(body(SYNTAX)), Ok(body(SYNTAX))]);
    assert!(matches!(
        outcome,
        Err(ProposalError::Policy(PolicyError::Syntax))
    ));
    assert_eq!(payloads.len(), 2);

    let (outcome, payloads) = try_responses(vec![Ok(body(UNTYPED)), Ok(body(UNTYPED))]);
    assert!(matches!(
        outcome,
        Err(ProposalError::Policy(PolicyError::Validation))
    ));
    assert_eq!(payloads.len(), 2);

    let (outcome, payloads) = try_responses(vec![Ok(body(WILDCARD)), Ok(body(WILDCARD))]);
    assert!(matches!(
        outcome,
        Ok(ProposalOutcome::NeedsClarification { .. })
    ));
    assert_eq!(payloads.len(), 2);
}

#[test]
fn a_failed_retry_call_fails_the_proposal() {
    let (outcome, payloads) = try_responses(vec![Ok(body(SYNTAX)), Err(ProviderError::Transport)]);
    assert!(matches!(outcome, Err(ProposalError::Intent(_))));
    assert_eq!(payloads.len(), 2);
}

#[test]
fn unsupported_features_and_valid_drafts_are_not_retried() {
    let (outcome, payloads) = try_responses(vec![Ok(body(UNSUPPORTED)), Ok(body(VALID))]);
    assert!(matches!(
        outcome,
        Ok(ProposalOutcome::NeedsClarification { .. })
    ));
    assert_eq!(payloads.len(), 1);

    let (outcome, payloads) = try_responses(vec![Ok(body(VALID))]);
    assert!(matches!(outcome, Ok(ProposalOutcome::Candidate { .. })));
    assert_eq!(payloads.len(), 1);
}

#[test]
fn malformed_task_response_and_invalid_cedar_share_one_admission_retry() {
    for malformed in ["not json", r#"{"policy":"","unresolved":null}"#] {
        let (result, calls) = try_responses(vec![Ok(malformed.into()), Ok(body(VALID))]);
        assert!(matches!(result, Ok(ProposalOutcome::Candidate { .. })));
        assert_eq!(calls.len(), 2);
        assert!(calls[1].contains("JSON object"));
        for retry in [malformed.to_owned(), body(SYNTAX)] {
            let (result, calls) = try_responses(vec![Ok(malformed.into()), Ok(retry)]);
            assert!(result.is_err());
            assert_eq!(calls.len(), 2);
        }
    }
}

#[test]
fn review_describes_admitted_rules_and_preserves_untranslatable_conditions() {
    let policy = format!(
        r#"{PLAIN_SPAWN}
{GATED_NETWORK}
@id("R3") forbid(principal, action == SecCore::Action::"file.read", resource is SecCore::File)
unless {{ resource.path == "/allowed" }};"#
    );
    let policy = policy.replace("(principal,", "(principal is SecCore::Subject,");
    let candidate = proposed_candidate(&Scripted::new(vec![Ok(body(&policy))]));
    let rows = notes(&candidate, "Rule review");
    let review = &rows[0];
    for expected in [
        "Allow starting processes",
        "the target program is \"/usr/bin/bash\"",
        "Forbid modifying files",
        "the path is \"/srv/demo/previous.zip\"",
        "the operation is deletion",
        "Allow network access for SecCore subjects. Request confirmation before command launch",
        "Allow network access for this command?",
        "Review the full Cedar statement",
        "unless",
        "/allowed",
    ] {
        assert!(review.contains(expected), "{review}");
    }
    assert_eq!(review.matches("Review the full Cedar statement").count(), 1);
}

#[test]
fn review_lists_directory_unions_without_broadening_exact_points_or_unpaired_subtrees() {
    let text = r#"permit(principal, action == SecCore::Action::"file.read", resource is SecCore::File)
when { resource.path == "/a" || resource.path like "/a/*" ||
       resource.path == "/b" || resource.path like "/b/*" ||
       resource.path == "/file" || resource.path like "/children/*" };"#;
    let candidate = proposed_candidate(&Scripted::new(vec![Ok(body(text))]));
    let review = notes(&candidate, "Rule review");
    assert!(review[0].contains(
        r#"["/a" or a descendant, "/b" or a descendant, "/file", a descendant of "/children"]"#
    ));
}
