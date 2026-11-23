//! Command proposals retry once when the model returns Cedar the official parser rejects.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use asc_intent::{CommandInput, CommandPolicyOutcome, PolicyError, ProposalError, propose_command};
use asc_model_client::provider::{ModelProvider, ProviderError};
use serde_json::json;

const VALID: &str = "permit(principal, action == SecCore::Action::\"file.read\", resource is SecCore::File) when { resource.path == \"/tmp/a.csv\" };";
const INVALID: &str = "permit(principal action == SecCore::Action::\"file.read\", resource is SecCore::File) when { resource.path == \"/tmp/a.csv\" };";

fn body(policy: &str) -> String {
    json!({"policy": policy, "unresolved": []}).to_string()
}

fn input() -> CommandInput {
    CommandInput {
        command: "cat a.csv".into(),
        cwd: "/tmp".into(),
    }
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(5)
}

struct Scripted {
    responses: Mutex<VecDeque<String>>,
    payloads: Mutex<Vec<String>>,
    calls: AtomicUsize,
}

impl Scripted {
    fn new(responses: &[String]) -> Self {
        Self {
            responses: Mutex::new(responses.iter().cloned().collect()),
            payloads: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
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
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.payloads.lock().unwrap().push(payload.to_owned());
        let mut responses = self.responses.lock().unwrap();
        responses.pop_front().ok_or(ProviderError::Transport)
    }
}

#[test]
fn command_prompt_includes_schema_and_a_valid_example() {
    struct PromptExample;
    impl ModelProvider for PromptExample {
        fn complete(
            &self,
            _: &str,
            system: &str,
            payload: &str,
            _: Instant,
        ) -> Result<String, ProviderError> {
            assert!(system.contains(asc_intent::CEDAR_SCHEMA));
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(payload).unwrap(),
                json!({"command": "read -r line < report.csv", "cwd": "/tmp"})
            );
            // Check the example actually sent, so the test copy cannot drift from the prompt.
            Ok(system
                .lines()
                .find(|line| line.starts_with("{\"policy\":"))
                .expect("提示词应包含 JSON 输出示例")
                .to_owned())
        }
    }

    let input = CommandInput {
        command: "read -r line < report.csv".into(),
        cwd: "/tmp".into(),
    };
    let outcome = propose_command(&PromptExample, "fixture", &input, deadline()).unwrap();
    let CommandPolicyOutcome::Candidate { policy, .. } = outcome else {
        panic!("示例应通过 Cedar 校验并返回候选");
    };
    assert_eq!(policy, VALID.replace("a.csv", "report.csv"));
}

#[test]
fn syntax_failures_are_repaired_by_one_retry() {
    let provider = Scripted::new(&[body(INVALID), body(VALID)]);
    let outcome = propose_command(&provider, "fixture", &input(), deadline()).unwrap();
    let CommandPolicyOutcome::Candidate { policy, .. } = outcome else {
        panic!("expected a candidate after the retry");
    };
    assert_eq!(policy, VALID);
    assert_eq!(provider.calls(), 2);
}

#[test]
fn a_second_failure_surfaces_the_validation_error() {
    let provider = Scripted::new(&[body(INVALID), body(INVALID)]);
    let error = propose_command(&provider, "fixture", &input(), deadline()).unwrap_err();
    assert!(matches!(error, ProposalError::Policy(PolicyError::Syntax)));
    assert_eq!(provider.calls(), 2);
}

#[test]
fn caveats_accompany_the_candidate() {
    let provider = Scripted::new(&[json!({
        "policy": VALID,
        "unresolved": [],
        "caveats": ["python3 resolved through PATH, exec target unknown"],
    })
    .to_string()]);
    let outcome = propose_command(&provider, "fixture", &input(), deadline()).unwrap();
    let CommandPolicyOutcome::Candidate { policy, caveats } = outcome else {
        panic!("caveats must not turn a candidate into a clarification");
    };
    assert_eq!(policy, VALID);
    assert_eq!(caveats.len(), 1);
    assert_eq!(provider.calls(), 1);
}

#[test]
fn the_retry_replays_the_local_rejection() {
    let provider = Scripted::new(&[body(INVALID), body(VALID)]);
    propose_command(&provider, "fixture", &input(), deadline()).unwrap();
    let payloads = provider.payloads();
    assert_eq!(payloads.len(), 2);
    assert!(!payloads[0].contains("上次输出被本地校验拒绝"));
    assert!(payloads[1].contains("上次输出被本地校验拒绝"));
}

#[test]
fn an_empty_policy_is_admitted_with_its_caveats() {
    let provider = Scripted::new(&[json!({
        "policy": "  ",
        "unresolved": [],
        "caveats": ["the command has no file, process or network action"],
    })
    .to_string()]);
    let outcome = propose_command(&provider, "fixture", &input(), deadline()).unwrap();
    let CommandPolicyOutcome::Candidate { policy, caveats } = outcome else {
        panic!("a command with no file, process or network action yields an empty candidate");
    };
    assert!(policy.trim().is_empty());
    assert_eq!(
        caveats,
        vec!["the command has no file, process or network action".to_owned()]
    );
    assert_eq!(provider.calls(), 1);
}

#[test]
fn clarification_is_not_retried() {
    let clarification = json!({"policy": "", "unresolved": ["$DIR 未定义"]}).to_string();
    let provider = Scripted::new(&[clarification]);
    let outcome = propose_command(&provider, "fixture", &input(), deadline()).unwrap();
    assert!(matches!(
        outcome,
        CommandPolicyOutcome::NeedsClarification { .. }
    ));
    assert_eq!(provider.calls(), 1);
}

#[test]
fn a_persistent_path_rejection_becomes_a_clarification() {
    let relative = "permit(principal, action == SecCore::Action::\"file.read\", resource is SecCore::File) when { resource.path == \"a.csv\" };";
    let provider = Scripted::new(&[body(relative), body(relative)]);
    let outcome = propose_command(&provider, "fixture", &input(), deadline()).unwrap();
    assert!(matches!(
        outcome,
        CommandPolicyOutcome::NeedsClarification { .. }
    ));
    assert_eq!(provider.calls(), 2);
}
