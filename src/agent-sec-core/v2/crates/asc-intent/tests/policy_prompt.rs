//! Direct Cedar response wrapper, provenance and failure isolation; the fixed provider never calls
//! the cloud.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use asc_intent::{
    CEDAR_SCHEMA, DRAFT_VERSION, INTENT_BUDGET, IntentError, PROMPT_VERSION, PolicyError,
    TaskInput, generate_policy_draft, prepare_policy, validate_policy_response,
};
use asc_model_client::provider::{ModelProvider, OpenAiCompatibleProvider, ProviderError};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

const TASK: &str = include_str!("../../../fixtures/intent/protect-delete/task.json");
const POLICY: &str = include_str!("../../../fixtures/intent/protect-delete/policy.cedar");

fn draft() -> String {
    json!({"policy": POLICY, "unresolved": []}).to_string()
}

struct FakeClient {
    response: String,
    calls: AtomicUsize,
    fail: bool,
    delay: Duration,
}
impl FakeClient {
    fn new(content: &str) -> Self {
        Self {
            response: content.into(),
            calls: AtomicUsize::new(0),
            fail: false,
            delay: Duration::ZERO,
        }
    }
}
impl ModelProvider for FakeClient {
    fn complete(
        &self,
        model: &str,
        system: &str,
        input: &str,
        deadline: Instant,
    ) -> Result<String, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(model, "test-model");
        assert!(system.starts_with(include_str!("../prompts/policy-generation.md")));
        assert_eq!(system.matches(CEDAR_SCHEMA).count(), 1);
        let input: Value = serde_json::from_str(input).unwrap();
        assert_eq!(input.as_object().unwrap().len(), 1);
        assert_eq!(input["task"].as_object().unwrap().len(), 2);
        assert!(input.get("paths").is_none());
        assert!(deadline.saturating_duration_since(Instant::now()) <= INTENT_BUDGET);
        std::thread::sleep(self.delay);
        if self.fail {
            Err(ProviderError::Transport)
        } else {
            Ok(self.response.clone())
        }
    }
}
fn task() -> TaskInput {
    serde_json::from_str(TASK).unwrap()
}
fn extract(client: &FakeClient, task: &TaskInput) -> Result<Value, IntentError> {
    Ok(serde_json::to_value(generate_policy_draft(
        client,
        "test-model",
        task,
        Instant::now() + INTENT_BUDGET,
    )?)
    .unwrap())
}

#[test]
fn direct_response_preserves_original_policy_and_traceable_source() {
    let client = FakeClient::new(&draft());
    let output = extract(&client, &task()).unwrap();
    assert_eq!(output["status"], "DRAFT");
    assert_eq!(output["format_version"], DRAFT_VERSION);
    assert_eq!(output["prompt_version"], PROMPT_VERSION);
    assert_eq!(output["policy"], POLICY);
    assert_eq!(output["unresolved"], json!([]));
    assert_eq!(output["source"]["byte_length"], task().text.len());
    assert_eq!(
        output["source"]["sha256"],
        format!("sha256:{:x}", Sha256::digest(task().text.as_bytes()))
    );
    assert!(output.get("intent").is_none());
    assert!(output.get("rules").is_none());
    assert_eq!(extract(&client, &task()).unwrap(), output);
    assert_eq!(client.calls.load(Ordering::SeqCst), 2);
    prepare_policy(output["policy"].as_str().unwrap()).unwrap();
}

#[test]
fn response_and_task_reject_authority_duplicate_missing_null_and_extra_fields() {
    for content in [
        "",
        "not JSON",
        "{}",
        "{\"policy\":\"\"}",
        "{\"policy\":null,\"unresolved\":[]}",
        "{\"policy\":\"\",\"unresolved\":null}",
        "{\"policy\":\"\",\"policy\":\"duplicate\",\"unresolved\":[]}",
        "{\"policy\":\"\",\"unresolved\":[],\"unresolved\":[]}",
        "{\"policy\":\"\",\"unresolved\":[],\"status\":\"APPROVED\"}",
    ] {
        assert!(validate_policy_response(content).is_err(), "{content}");
    }
    for field in [
        "principal",
        "trusted",
        "verified",
        "authority",
        "rules",
        "status",
        "confirmed",
    ] {
        let mut invalid = json!({"policy":"", "unresolved":[]});
        invalid[field] = json!("SECRET_IDENTITY");
        let error = validate_policy_response(&invalid.to_string()).unwrap_err();
        assert!(!format!("{error}: {error:?}").contains("SECRET_IDENTITY"));
        let mut input: Value = serde_json::from_str(TASK).unwrap();
        input[field] = json!(true);
        assert!(serde_json::from_value::<TaskInput>(input).is_err());
    }
    for reasons in [
        json!([" "]),
        json!(["control\n"]),
        json!(["x".repeat(4097)]),
        json!(vec!["gap"; 129]),
    ] {
        assert!(
            validate_policy_response(&json!({"policy":"", "unresolved":reasons}).to_string())
                .is_err()
        );
    }
    assert!(validate_policy_response(&"x".repeat(16 * 1024 + 1)).is_err());
    assert!(validate_policy_response("{\"policy\":\"\",\"unresolved\":[]}").is_ok());
}

#[test]
fn invalid_input_expiry_and_model_failures_never_return_an_old_draft() {
    let mut client = FakeClient::new(&draft());
    assert!(generate_policy_draft(&client, "test-model", &task(), Instant::now()).is_err());
    for text in [String::new(), " ".into(), "x".repeat(64 * 1024 + 1)] {
        assert!(matches!(
            extract(
                &client,
                &TaskInput {
                    source_id: "source".into(),
                    text
                }
            ),
            Err(IntentError::InvalidInput)
        ));
    }
    assert_eq!(client.calls.load(Ordering::SeqCst), 0);
    client.fail = true;
    let error = extract(&client, &task()).unwrap_err();
    assert!(matches!(
        error,
        IntentError::Provider(ProviderError::Transport)
    ));
    assert!(!format!("{error:?}").contains("SECRET_"));
    client.fail = false;
    client.delay = Duration::from_millis(20);
    assert!(matches!(
        generate_policy_draft(
            &client,
            "test-model",
            &task(),
            Instant::now() + Duration::from_millis(5)
        ),
        Err(IntentError::DeadlineExceeded)
    ));
}

#[test]
#[ignore = "requires DASHSCOPE_API_KEY; makes one paid call"]
fn dashscope_exact_file_protection_fixture() {
    // Reasoning tokens share `max_tokens` with the answer, so disable thinking as the daemon does.
    let provider = OpenAiCompatibleProvider::new(
        "https://dashscope.aliyuncs.com/compatible-mode/v1",
        std::env::var("DASHSCOPE_API_KEY").expect("DASHSCOPE_API_KEY is required"),
    )
    .unwrap()
    .without_thinking();
    let draft = generate_policy_draft(
        &provider,
        "deepseek-v4.1-flash",
        &task(),
        Instant::now() + INTENT_BUDGET,
    )
    .unwrap();
    // The fixture names relative targets, so a model that follows the prompt reports them instead
    // of inventing an absolute directory; whatever Cedar it does emit must still be valid.
    match prepare_policy(draft.policy()) {
        Ok(_) => {}
        Err(PolicyError::InvalidPath | PolicyError::UnsupportedPolicy) => {
            assert!(!draft.unresolved().is_empty());
        }
        Err(error) => panic!("unexpected policy failure: {error}"),
    }
    // This check covers types and the wrapper only; task semantics are covered by the independent
    // adjudication matrix in policy.rs.
}
