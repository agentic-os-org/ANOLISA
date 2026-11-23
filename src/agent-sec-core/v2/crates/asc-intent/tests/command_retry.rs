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
                .expect("the prompt must include a JSON response example")
                .to_owned())
        }
    }

    let input = CommandInput {
        command: "read -r line < report.csv".into(),
        cwd: "/tmp".into(),
    };
    let outcome = propose_command(&PromptExample, "fixture", &input, deadline()).unwrap();
    let CommandPolicyOutcome::Candidate { policy, .. } = outcome else {
        panic!("the example must pass Cedar validation and produce a candidate");
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
    assert!(!payloads[0].contains("The previous response failed local validation"));
    assert!(payloads[1].contains("The previous response failed local validation"));
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
    let clarification = json!({"policy": "", "unresolved": ["$DIR is undefined"]}).to_string();
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

#[test]
fn literal_archive_and_glob_commands_request_only_their_data_scopes() {
    use asc_intent::summarize;
    let provider = Scripted::new(&[]);
    for (command, read, write) in [
        (
            "cat src/*.txt > out/all.txt",
            "/work/src/*",
            "/work/out/all.txt",
        ),
        (
            "cat '/source dir'/*.log > '/work/out/all logs'",
            "/source dir/*",
            "/work/out/all logs",
        ),
        ("tar -cf out/a.tar -C src .", "/work/src/*", "/work/out/*"),
        (
            "tar -czf /archives/a.tgz -C '/source dir' .",
            "/source dir/*",
            "/archives/*",
        ),
        (
            "tar --create --file='out dir/a.tar' --directory 'src dir' .",
            "/work/src dir/*",
            "/work/out dir/*",
        ),
        ("tar -c --file a.tar -C src .", "/work/src/*", "/work/a.tar"),
    ] {
        let data = CommandInput {
            command: command.to_owned(),
            cwd: "/work".into(),
        };
        let result = propose_command(&provider, "fixture", &data, deadline()).unwrap();
        let CommandPolicyOutcome::Candidate { policy, .. } = result else {
            panic!("{command}")
        };
        let summary = summarize(&policy).unwrap();
        let permissions: Vec<_> = summary
            .permits
            .iter()
            .map(|p| (p.action.label(), p.path.as_ref().unwrap().display()))
            .collect();
        assert_eq!(
            permissions,
            [("read", read.to_owned()), ("write", write.to_owned())],
            "{command}"
        );
    }
    assert_eq!(provider.calls(), 0);
}

#[test]
fn archive_in_an_ancestor_requests_only_the_output_file() {
    let provider = Scripted::new(&[]);
    let input = CommandInput {
        command: "tar -cf /work/bundle.tar -C src .".into(),
        cwd: "/work/project".into(),
    };
    let result = propose_command(&provider, "fixture", &input, deadline()).unwrap();
    let CommandPolicyOutcome::Candidate { policy, .. } = result else {
        panic!("expected a candidate for a directory archive");
    };
    let summary = asc_intent::summarize(&policy).unwrap();
    let writes: Vec<_> = summary
        .permits
        .iter()
        .filter(|permit| permit.action.label() == "write")
        .map(|permit| permit.path.as_ref().unwrap().display())
        .collect();
    assert_eq!(writes, ["/work/bundle.tar"]);
    assert_eq!(provider.calls(), 0);
}

#[test]
fn commands_with_ambiguous_operands_or_unsupported_syntax_use_the_model() {
    for command in [
        "cat 'src/*.txt' > out/all",
        "cat src/*/*.txt > out/all",
        "cat src/*.txt > out/all; pwd",
        "cat src/*.txt 2>out/errors",
        "tar -cf . -C src .",
        "tar -cf /work -C src .",
        "tar -cf out/a.tar README.md",
        "tar -cf out/a.tar .",
        "tar -cf out/a.tar -C src README.md",
        "tar -cf out/a.tar -C src . README.md",
        "tar -cf out/a.tar --files-from manifest",
        "tar -cf remote:archive -C src .",
        "tar -xf out/a.tar",
        "tar -cf out/a.tar -C ../unknown .",
    ] {
        let provider = Scripted::new(&[body(VALID)]);
        let data = CommandInput {
            command: command.to_owned(),
            cwd: "/work".into(),
        };
        propose_command(&provider, "fixture", &data, deadline()).unwrap();
        assert_eq!(provider.calls(), 1, "{command}");
    }
}
