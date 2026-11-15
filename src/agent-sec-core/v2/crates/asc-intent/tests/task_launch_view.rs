//! Task candidates retry once when a permit tests `context`, which the launch authorizer cannot
//! read, or when a forbid carries annotations, which the runtime protector refuses; the retry
//! never costs the valid first draft. A point grant on a link is reported, never retried.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use asc_intent::{
    LAUNCH_VIEW_NOTE, POINT_ALIAS_NOTE, ProposalContext, ProposalOutcome, RUNTIME_VIEW_NOTE,
    TaskInput, annotated_forbids, launch_view_gaps, point_aliases, propose_task,
};
use asc_model_client::provider::{ModelProvider, ProviderError};
use serde_json::{Value, json};

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

fn propose(provider: &Scripted) -> Value {
    let outcome = propose_task(
        provider,
        "fixture",
        &TaskInput {
            source_id: "source".into(),
            text: "Build locally".into(),
        },
        &ProposalContext::new("task-1".into(), 2_000_000_000, 1800).unwrap(),
        Instant::now() + Duration::from_secs(5),
    )
    .unwrap();
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

#[test]
fn gaps_name_context_permits_only() {
    assert_eq!(
        launch_view_gaps(CWD_SPAWN).unwrap(),
        [r#"permit #1 @id("spawn") (process.spawn) tests context.input.cwd"#]
    );
    // A forbid on `context.input.operation` is the runtime protector's input, not a gap.
    assert!(launch_view_gaps(PLAIN_SPAWN).unwrap().is_empty());
    let operation_permit = r#"permit(principal, action == SecCore::Action::"file.write", resource is SecCore::File)
when { resource.path == "/srv/demo/out" && context.input.operation == SecCore::WriteOperation::"create" };"#;
    assert_eq!(
        launch_view_gaps(operation_permit).unwrap(),
        ["permit #1 (file.write) tests context.input.operation"]
    );
}

#[test]
fn a_repaired_retry_replaces_the_first_draft() {
    let provider = Scripted::new(vec![Ok(body(CWD_SPAWN)), Ok(body(PLAIN_SPAWN))]);
    let candidate = propose(&provider);
    assert_eq!(candidate["artifact"]["text"], PLAIN_SPAWN);
    assert_eq!(candidate["draft"]["policy"], PLAIN_SPAWN);
    assert!(launch_notes(&candidate).is_empty());
    let payloads = provider.payloads();
    assert_eq!(payloads.len(), 2);
    assert!(!payloads[0].contains("上次输出被本地校验拒绝"));
    assert!(payloads[1].contains("context.input.cwd"));
}

#[test]
fn a_failed_retry_keeps_the_first_draft_and_reports_the_gap() {
    for retry in [
        Err(ProviderError::Transport),
        Ok("not json".to_owned()),
        Ok(body("permit(principal action, resource);")),
        Ok(body(CWD_SPAWN)),
    ] {
        let provider = Scripted::new(vec![Ok(body(CWD_SPAWN)), retry]);
        let candidate = propose(&provider);
        assert_eq!(candidate["artifact"]["text"], CWD_SPAWN);
        let notes = launch_notes(&candidate);
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("context.input.cwd"), "{notes:?}");
    }
}

#[test]
fn a_clean_draft_is_not_retried() {
    let provider = Scripted::new(vec![Ok(body(PLAIN_SPAWN))]);
    let candidate = propose(&provider);
    assert_eq!(candidate["artifact"]["text"], PLAIN_SPAWN);
    assert_eq!(provider.payloads().len(), 1);
}

#[test]
fn point_aliases_name_links_outside_granted_subtrees() {
    let text = r#"permit(principal, action == SecCore::Action::"file.read", resource is SecCore::File)
when { resource.path == "/lib" || resource.path like "/lib/*" ||
    resource.path == "/usr/bin/bash" || resource.path == "/usr/bin/python3" };
permit(principal, action == SecCore::Action::"file.exec", resource is SecCore::File)
when { resource.path == "/usr/bin/python3" || resource.path like "/usr/*" };
forbid(principal, action == SecCore::Action::"file.read", resource is SecCore::File)
when { resource.path == "/usr/bin/python3" };"#;
    let resolve = |path: &str| match path {
        "/usr/bin/python3" => Some("/usr/bin/python3.11".to_owned()),
        "/lib" => Some("/usr/lib".to_owned()),
        other => Some(other.to_owned()),
    };
    // The `/lib` pair is a portable subtree grant, the exec point sits inside `/usr/*`, and a
    // forbid is the runtime protector's input: only the read point is a dead grant.
    assert_eq!(
        point_aliases(text, &resolve).unwrap(),
        ["permit #1 (file.read) grants /usr/bin/python3, which resolves to /usr/bin/python3.11"]
    );
    assert!(point_aliases(text, &|_| None).unwrap().is_empty());
}

#[test]
fn a_point_alias_is_reported_without_a_retry() {
    let root = std::env::temp_dir().join(format!("asc-intent-alias-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
    let (target, link) = (root.join("python3.11"), root.join("python3"));
    std::fs::write(&target, b"").unwrap();
    let _ = std::fs::remove_file(&link);
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let policy = format!(
        r#"// [S1] interpreter
permit(principal, action == SecCore::Action::"file.read", resource is SecCore::File)
when {{ resource.path == "{}" }};"#,
        link.display()
    );
    let provider = Scripted::new(vec![Ok(body(&policy))]);
    let candidate = propose(&provider);
    std::fs::remove_dir_all(&root).unwrap();
    assert_eq!(candidate["artifact"]["text"], policy);
    assert_eq!(provider.payloads().len(), 1);
    let aliases = notes(&candidate, POINT_ALIAS_NOTE);
    assert_eq!(aliases.len(), 1);
    assert!(
        aliases[0].contains(&format!("which resolves to {}", target.display())),
        "{aliases:?}"
    );
}

#[test]
fn annotated_forbids_are_retried_and_reported_when_they_stay() {
    let annotated = PLAIN_SPAWN.replace("// [H1] keep the rollback package\n", "@id(\"keep\")\n");
    // `@id` on a permit stays a description; only the forbid fails the runtime protector.
    assert!(annotated_forbids(CWD_SPAWN).unwrap().is_empty());
    assert_eq!(
        annotated_forbids(&annotated).unwrap(),
        ["forbid #2 carries @id"]
    );

    let provider = Scripted::new(vec![Ok(body(&annotated)), Ok(body(PLAIN_SPAWN))]);
    let candidate = propose(&provider);
    assert_eq!(candidate["artifact"]["text"], PLAIN_SPAWN);
    assert!(notes(&candidate, RUNTIME_VIEW_NOTE).is_empty());
    assert!(provider.payloads()[1].contains("forbid #2 carries @id"));

    let provider = Scripted::new(vec![Ok(body(&annotated)), Ok(body(&annotated))]);
    let candidate = propose(&provider);
    assert_eq!(candidate["artifact"]["text"], annotated);
    let runtime = notes(&candidate, RUNTIME_VIEW_NOTE);
    assert_eq!(runtime.len(), 1);
    assert!(runtime[0].contains("UNSUPPORTED_ANNOTATION"), "{runtime:?}");
}
