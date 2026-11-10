//! Official adjudication boundary for native Cedar candidates; synthetic requests execute no real
//! file, program or network operation.

use asc_intent::{
    CANDIDATE_VERSION, CEDAR_SCHEMA, CandidatePolicy, DRAFT_VERSION, INTENT_BUDGET,
    MAX_POLICY_BYTES, PROMPT_VERSION, PolicyError, ProposalContext, ProposalError, ProposalOutcome,
    TaskInput, prepare_policy, propose_task, validate_policy_response,
};
use asc_model_client::provider::{FakeProvider, ModelProvider, ProviderError};
use cedar_policy::{
    Authorizer, Context, Decision, Entities, EntityId, EntityTypeName, EntityUid, PolicySet,
    Request, Schema, ValidationMode, Validator,
};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::str::FromStr as _;
use std::time::{Duration, Instant};

fn context() -> ProposalContext {
    ProposalContext::new("task-1".into(), 2_000_000_000, 1800).unwrap()
}
fn propose(response: &Value) -> Result<ProposalOutcome, ProposalError> {
    propose_task(
        &FakeProvider::new(response.to_string()),
        "fixture",
        &TaskInput {
            source_id: "source".into(),
            text: "Acceptance task".into(),
        },
        &context(),
        Instant::now() + INTENT_BUDGET,
    )
}
fn candidate(policy: &str) -> Box<CandidatePolicy> {
    match propose(&json!({"policy":policy,"unresolved":[]})).unwrap() {
        ProposalOutcome::Candidate { candidate } => candidate,
        other @ ProposalOutcome::NeedsClarification { .. } => {
            panic!("{}", serde_json::to_string(&other).unwrap());
        }
    }
}
fn uid(kind: &str, id: &str) -> EntityUid {
    EntityUid::from_type_name_and_id(EntityTypeName::from_str(kind).unwrap(), EntityId::new(id))
}
fn write_context(operation: &str) -> Value {
    json!({"input":{"program":"/bin/test","operation":{"__entity":{"type":"SecCore::WriteOperation","id":operation}}}})
}
fn exec_context(cwd: &str, args: &[&str]) -> Value {
    let args: Vec<_> = args
        .iter()
        .enumerate()
        .map(|(i, value)| json!({"index":i,"value":value}))
        .collect();
    json!({"input":{"cwd":cwd,"arg_count":args.len(),"args":args}})
}
struct Judge {
    schema: Schema,
    policies: PolicySet,
}
impl Judge {
    fn new(policy: &str) -> Self {
        let (schema, _) = Schema::from_cedarschema_str(CEDAR_SCHEMA).unwrap();
        let policies = PolicySet::from_str(policy).unwrap();
        let result = Validator::new(schema.clone()).validate(&policies, ValidationMode::Strict);
        assert!(
            result.validation_passed(),
            "{:?}",
            result.validation_errors().collect::<Vec<_>>()
        );
        Self { schema, policies }
    }
    fn combined(policy: &str) -> Self {
        Self::new(&format!("{policy}\npermit(principal, action, resource);"))
    }
    fn decide(&self, action: &str, path: &str, context: Value) -> Decision {
        let resource = if action == "network.use" {
            uid("SecCore::Network", "all")
        } else {
            uid("SecCore::File", "target")
        };
        let action = uid("SecCore::Action", action);
        let entities = Entities::from_json_value(
            json!([
                {"uid":{"type":"SecCore::Subject","id":"process"},"attrs":{},"parents":[]},
                {"uid":{"type":"SecCore::File","id":"target"},"attrs":{"path":path},"parents":[]}
            ]),
            Some(&self.schema),
        )
        .unwrap();
        let request = Request::new(
            uid("SecCore::Subject", "process"),
            action.clone(),
            resource,
            Context::from_json_value(context, Some((&self.schema, &action))).unwrap(),
            Some(&self.schema),
        )
        .unwrap();
        let result = Authorizer::new().is_authorized(&request, &self.policies, &entities);
        assert_eq!(
            result.diagnostics().errors().count(),
            0,
            "an execution error must not masquerade as a normal Deny"
        );
        result.decision()
    }
    fn check(&self, action: &str, path: &str, context: Value, expected: Decision) {
        let request_description = format!("{action} {path} {context}");
        assert_eq!(
            self.decide(action, path, context),
            expected,
            "{request_description}"
        );
    }
    fn read(&self, path: &str, expected: Decision) {
        self.check(
            "file.read",
            path,
            json!({"input":{"program":"/bin/test"}}),
            expected,
        );
    }
    fn write(&self, path: &str, op: &str, expected: Decision) {
        self.check("file.write", path, write_context(op), expected);
    }
}
fn json_hash(value: &Value) -> String {
    fn sort(value: &mut Value) {
        match value {
            Value::Object(map) => {
                map.sort_keys();
                map.values_mut().for_each(sort);
            }
            Value::Array(values) => values.iter_mut().for_each(sort),
            _ => {}
        }
    }
    let mut value = value.clone();
    sort(&mut value);
    format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(&value).unwrap())
    )
}
fn generic_candidate(value: &Value) {
    assert_eq!(value["format_version"], CANDIDATE_VERSION);
    assert_eq!(value["draft"]["format_version"], DRAFT_VERSION);
    assert_eq!(value["draft"]["prompt_version"], PROMPT_VERSION);
    assert_eq!(value["status"], "CANDIDATE");
    assert_eq!(value["authorization_required"], true);
    assert!(value.get("rules").is_none() && value.get("mapping").is_none());
    // Path metadata and program protection are gone: the candidate keeps only model output and its
    // source.
    assert!(value.get("bindings").is_none() && value.get("protected_files").is_none());
    assert!(value.get("binding_digest").is_none());
    assert_eq!(
        value["artifact"]["schema_sha256"],
        format!("sha256:{:x}", Sha256::digest(CEDAR_SCHEMA.as_bytes()))
    );
    let mut content = value.clone();
    content.as_object_mut().unwrap().remove("candidate_hash");
    assert_eq!(value["candidate_hash"], json_hash(&content));
    let ttl = value["expires_at"].as_i64().unwrap() - value["created_at"].as_i64().unwrap();
    assert!((1..=1800).contains(&ttl));
    Judge::new(value["artifact"]["text"].as_str().unwrap());
}

const DENY_ETC: &str = r#"@id("read-boundary")
forbid(principal, action == SecCore::Action::"file.read", resource is SecCore::File)
when { resource.path == "/etc" || resource.path like "/etc/*" };"#;
fn deny_etc_matrix(policy: &str) {
    let original = Judge::new(policy);
    for path in ["/etc", "/etc/hosts", "/work/report.txt"] {
        original.read(path, Decision::Deny);
    }
    let combined = Judge::combined(policy);
    for (path, expected) in [
        ("/etc", Decision::Deny),
        ("/etc/hosts", Decision::Deny),
        ("/etc/ssh/sshd_config", Decision::Deny),
        ("/etc-backup/hosts", Decision::Allow),
        ("/etcd/config", Decision::Allow),
        ("/work/report.txt", Decision::Allow),
    ] {
        combined.read(path, expected);
    }
    combined.write("/etc/hosts", "write", Decision::Allow);
    combined.check("network.use", "", json!({}), Decision::Allow);
}

#[test]
fn direct_cedar_generation_and_model_boundaries() {
    let proposed = candidate(DENY_ETC);
    assert_eq!(proposed.policy_text(), DENY_ETC);
    deny_etc_matrix(proposed.policy_text());
    generic_candidate(&serde_json::to_value(&proposed).unwrap());
    model_policy_is_authoritative();
    literal_and_root_paths();
    condition_and_exclusive_boundaries();
    invalid_and_unsupported();
    process_program_attribute();
    unresolved_is_not_a_completeness_proof();
    exact_delete_matrix();
    input_budgets_and_deadlines();
    prompt_and_wrapper_boundaries();
}

fn model_policy_is_authoritative() {
    // Nothing narrows the candidate when the model permits everything.
    let proposed = candidate("permit(principal, action, resource); // model trailing comment");
    assert_eq!(
        proposed.policy_text(),
        "permit(principal, action, resource); // model trailing comment"
    );
    let judge = Judge::new(proposed.policy_text());
    for path in ["/", "/etc", "/work/config/keep.txt"] {
        for op in ["write", "create", "delete", "rename", "setattr"] {
            judge.write(path, op, Decision::Allow);
        }
        judge.read(path, Decision::Allow);
    }
    // An empty policy stays empty; Cedar itself supplies the default deny.
    let empty = candidate("");
    assert_eq!(empty.policy_text(), "");
    let original = Judge::new(empty.policy_text());
    for path in ["/", "/etc", "/work", "/work/report.csv"] {
        original.read(path, Decision::Deny);
        for op in ["write", "create", "delete", "rename", "setattr"] {
            original.write(path, op, Decision::Deny);
        }
    }
    let combined = Judge::combined(empty.policy_text());
    combined.read("/work/report.csv", Decision::Allow);
    combined.write("/etc/hosts", "delete", Decision::Allow);
    generic_candidate(&serde_json::to_value(&empty).unwrap());
    assert_ne!(
        empty.candidate_hash(),
        candidate("permit(principal, action, resource);").candidate_hash()
    );
    assert_eq!(empty.candidate_hash(), candidate("").candidate_hash());
}

fn literal_and_root_paths() {
    for path in [
        "/work/project space",
        "/work/a*b",
        "/work/a\"; permit(principal, action, resource);",
    ] {
        let policy = format!(
            "permit(principal, action == SecCore::Action::\"file.read\", resource is SecCore::File) when {{ resource.path == {} }};",
            json!(path)
        );
        let proposed = candidate(&policy);
        let judge = Judge::new(proposed.policy_text());
        judge.read(path, Decision::Allow);
        judge.read(&format!("{path}/child"), Decision::Deny);
        judge.read(&format!("{path}-neighbor/file"), Decision::Deny);
        judge.read("/work/aZZb/file", Decision::Deny);
        prepare_policy(proposed.policy_text()).unwrap();
    }
    let root = r#"permit(principal, action == SecCore::Action::"file.read", resource is SecCore::File)
when { resource.path == "/" || resource.path like "/*" };"#;
    let proposed = candidate(root);
    let judge = Judge::new(proposed.policy_text());
    judge.read("/", Decision::Allow);
    judge.read("/etc/hosts", Decision::Allow);
    judge.write("/etc/hosts", "create", Decision::Deny);
}

fn condition_and_exclusive_boundaries() {
    let policy = r#"
permit(principal, action == SecCore::Action::"file.read", resource is SecCore::File)
when { resource.path == "/work/data" && context.input.program == "/usr/bin/cat" };
permit(principal, action == SecCore::Action::"file.write", resource is SecCore::File)
when { resource.path == "/work/report" && context.input.operation == SecCore::WriteOperation::"setattr"
 && context.input has mode && context.input.mode == 420
 && context.input has ownerUid && context.input.ownerUid == 1000
 && context.input has ownerGid && context.input.ownerGid == 1001 };
permit(principal, action == SecCore::Action::"file.exec", resource is SecCore::File)
when { resource.path == "/usr/bin/git" && context.input.cwd == "/work"
 && context.input.arg_count == 4
 && context.input.args == [{index:0,value:"a"},{index:1,value:"a"},{index:2,value:""},{index:3,value:"quote\"\\*"}] };
forbid(principal, action == SecCore::Action::"file.exec", resource is SecCore::File)
unless { resource.path == "/usr/bin/git" && context.input.cwd == "/work"
 && context.input.arg_count == 4
 && context.input.args == [{index:0,value:"a"},{index:1,value:"a"},{index:2,value:""},{index:3,value:"quote\"\\*"}] };
"#;
    let proposed = candidate(policy);
    let judge = Judge::new(proposed.policy_text());
    judge.read("/work/data", Decision::Deny);
    judge.check(
        "file.read",
        "/work/data",
        json!({"input":{"program":"/usr/bin/cat"}}),
        Decision::Allow,
    );
    let mut attrs = write_context("setattr");
    attrs["input"]["mode"] = json!(420);
    attrs["input"]["ownerUid"] = json!(1000);
    attrs["input"]["ownerGid"] = json!(1001);
    judge.check("file.write", "/work/report", attrs.clone(), Decision::Allow);
    for field in ["mode", "ownerUid", "ownerGid"] {
        let mut missing = attrs.clone();
        missing["input"].as_object_mut().unwrap().remove(field);
        judge.check("file.write", "/work/report", missing, Decision::Deny);
        let mut wrong = attrs.clone();
        wrong["input"][field] = json!(0);
        judge.check("file.write", "/work/report", wrong, Decision::Deny);
    }
    let combined = Judge::combined(proposed.policy_text());
    let args = ["a", "a", "", "quote\"\\*"];
    for judge in [&judge, &combined] {
        judge.check(
            "file.exec",
            "/usr/bin/git",
            exec_context("/work", &args),
            Decision::Allow,
        );
        for other in [
            &["a", "a"][..],
            &["a", "", "a", "quote\"\\*"][..],
            &["a", "a", "", "other"][..],
        ] {
            judge.check(
                "file.exec",
                "/usr/bin/git",
                exec_context("/work", other),
                Decision::Deny,
            );
        }
        judge.check(
            "file.exec",
            "/bin/sh",
            exec_context("/work", &args),
            Decision::Deny,
        );
        judge.check(
            "file.exec",
            "/usr/bin/git",
            exec_context("/outside", &args),
            Decision::Deny,
        );
        let mut forged = exec_context("/work", &args);
        forged["input"]["arg_count"] = json!(5);
        judge.check("file.exec", "/usr/bin/git", forged, Decision::Deny);
    }
    combined.read("/outside", Decision::Allow);
    let exclusive = r#"
permit(principal, action == SecCore::Action::"file.write", resource is SecCore::File)
when { resource.path == "/work/report" && context.input.operation == SecCore::WriteOperation::"write" };
forbid(principal, action == SecCore::Action::"file.write", resource is SecCore::File)
when { context.input.operation == SecCore::WriteOperation::"write" && resource.path != "/work/report" };"#;
    let proposed = candidate(exclusive);
    let judge = Judge::combined(proposed.policy_text());
    judge.write("/work/report", "write", Decision::Allow);
    judge.write("/other", "write", Decision::Deny);
    for op in ["create", "delete", "rename", "setattr"] {
        judge.write("/other", op, Decision::Allow);
    }
}

fn invalid_and_unsupported() {
    for expression in [
        "resource.path == \"relative\"",
        "resource.path == \"/a/../b\"",
        "resource.path == \"/a//b\"",
        "resource.path like \"/a*\"",
        "resource.path like \"/a/*/b\"",
        "resource.path == context.input.program",
        "[\"/a\"].contains(resource.path)",
        "resource.path == \"/a/\"",
        "resource.path == \"/a\\\\b\"",
        // A record projection reads the same string without the canonical receiver.
        "{r: resource}.r.path like \"/a/*\"",
        "{r: resource}.r.path == \"/a\"",
    ] {
        let policy = format!(
            "permit(principal, action == SecCore::Action::\"file.read\", resource is SecCore::File) when {{ {expression} }};"
        );
        assert!(
            matches!(prepare_policy(&policy), Err(PolicyError::InvalidPath)),
            "{policy}"
        );
        let response = json!({"policy":policy,"unresolved":[]});
        let result = serde_json::to_value(propose(&response).unwrap()).unwrap();
        assert_eq!(result["outcome"], "needs_clarification");
        assert_eq!(result["draft"]["policy"], response["policy"]);
        assert!(!result["reasons"].as_array().unwrap().is_empty());
        assert!(result.get("candidate").is_none());
    }
    for policy in [
        "permit(principal == ?principal, action, resource);",
        "permit(principal == SecCore::Subject::\"A\", action, resource);",
        "permit(principal, action, resource == SecCore::File::\"/a\");",
        "permit(principal, action, resource) when { principal == SecCore::Subject::\"A\" };",
        "@advice(\"audit\") permit(principal, action, resource);",
        "@trusted(\"yes\") permit(principal, action, resource);",
    ] {
        assert!(
            matches!(prepare_policy(policy), Err(PolicyError::UnsupportedPolicy)),
            "{policy}"
        );
    }
    assert!(matches!(
        prepare_policy("not Cedar"),
        Err(PolicyError::Syntax)
    ));
    assert!(matches!(
        prepare_policy("permit(principal, action == SecCore::Action::\"tool.call\", resource);"),
        Err(PolicyError::Validation)
    ));
    // Policy admission rejects these as well: the action can never apply to the resource type, so
    // the forbid restricts nothing.
    assert!(matches!(
        prepare_policy(
            "forbid(principal, action == SecCore::Action::\"network.use\", resource is SecCore::File);"
        ),
        Err(PolicyError::Validation)
    ));
    assert!(matches!(
        prepare_policy(
            "permit(principal, action == SecCore::Action::\"file.read\", resource is SecCore::Network);"
        ),
        Err(PolicyError::Validation)
    ));
    assert!(matches!(
        prepare_policy(
            "permit(principal, action, resource) when { context.input.nonexistent == 0 };"
        ),
        Err(PolicyError::Validation)
    ));
    assert!(matches!(
        prepare_policy(&" ".repeat(MAX_POLICY_BYTES + 1)),
        Err(PolicyError::TooLarge)
    ));
    assert!(matches!(
        prepare_policy(&"permit(principal, action, resource);\n".repeat(1025)),
        Err(PolicyError::TooLarge)
    ));
}

fn process_program_attribute() {
    // A `like` pattern written as an equality literal names no concrete path and would not
    // project, so it gets its own reason the model can act on.
    for expression in ["resource.path == \"/a/*\"", "resource.path != \"/a/*\""] {
        let policy = format!(
            "permit(principal, action == SecCore::Action::\"file.read\", resource is SecCore::File) when {{ {expression} }};"
        );
        assert!(
            matches!(prepare_policy(&policy), Err(PolicyError::WildcardLiteral)),
            "{policy}"
        );
    }
    // `SecCore::Process.program` is a command name supplied by the request, not a path, so it
    // keeps no absolute-literal rule; only a projection through a record hides the receiver.
    for policy in [
        "permit(principal, action == SecCore::Action::\"process.spawn\", resource is SecCore::Process)\nwhen { resource.program == \"git\" };"
            .to_owned(),
        "permit(principal, action == SecCore::Action::\"process.spawn\", resource is SecCore::Process)\nwhen { resource.program != \"git\" };".to_owned(),
        "permit(principal, action == SecCore::Action::\"process.spawn\", resource is SecCore::Process)\nwhen { resource.resolved_program == \"/usr/bin/git\" };".to_owned(),
    ] {
        assert!(prepare_policy(&policy).is_ok(), "{policy}");
    }
    let projected = "{r: resource}.r.program == \"git\"";
    let policy = format!(
        "permit(principal, action == SecCore::Action::\"process.spawn\", resource is SecCore::Process) when {{ {projected} }};"
    );
    assert!(
        matches!(prepare_policy(&policy), Err(PolicyError::InvalidPath)),
        "{policy}"
    );
    // The request context program stays an executable path and keeps the absolute-literal rule.
    let context_program = "permit(principal, action == SecCore::Action::\"file.read\", resource is SecCore::File)\nwhen { context.input.program == \"git\" };";
    assert!(matches!(
        prepare_policy(context_program),
        Err(PolicyError::InvalidPath)
    ));
}

fn unresolved_is_not_a_completeness_proof() {
    let policy = "permit(principal, action == SecCore::Action::\"file.read\", resource is SecCore::File) when { resource.path == \"/read\" };";
    let gaps = [
        "Agent must not call git tool",
        "Process A identity is unavailable",
        "Approval required",
        "Audit obligation cannot be expressed",
        "Only example.com:443",
    ];
    let result = propose(&json!({"policy":policy,"unresolved":gaps})).unwrap();
    let ProposalOutcome::Candidate { candidate } = result else {
        panic!("expected candidate");
    };
    assert_eq!(candidate.unresolved(), gaps);
    let judge = Judge::new(candidate.policy_text());
    judge.read("/read", Decision::Allow);
    judge.read("/audited", Decision::Deny);
    judge.check("network.use", "", json!({}), Decision::Deny);
    let combined = Judge::combined(candidate.policy_text());
    combined.check(
        "file.exec",
        "/usr/bin/git",
        exec_context("/work", &[]),
        Decision::Allow,
    );
    generic_candidate(&serde_json::to_value(&candidate).unwrap());
    let empty = propose(&json!({"policy":"","unresolved":["Only list names"]})).unwrap();
    assert_eq!(
        serde_json::to_value(empty).unwrap()["candidate"]["artifact"]["text"],
        ""
    );
}

fn exact_delete_matrix() {
    let response = include_str!("../../../fixtures/intent/protect-delete/policy.cedar");
    let proposed = candidate(response);
    let original = Judge::new(proposed.policy_text());
    let combined = Judge::combined(proposed.policy_text());
    for judge in [&original, &combined] {
        judge.write("/work/config/important.txt", "delete", Decision::Deny);
        for path in [
            "/work/config/other.txt",
            "/work/config/important.txt.bak",
            "/work/config/important.txt/child",
            "/work/config",
        ] {
            judge.write(path, "delete", Decision::Allow);
        }
        judge.check("network.use", "", json!({}), Decision::Deny);
        judge.check(
            "file.exec",
            "/bin/sh",
            exec_context("/work", &[]),
            Decision::Deny,
        );
    }
    for op in ["write", "create", "rename", "setattr"] {
        original.write("/work/config/important.txt", op, Decision::Deny);
        combined.write("/work/config/important.txt", op, Decision::Allow);
    }
    original.write("/outside/file", "delete", Decision::Deny);
    combined.write("/outside/file", "delete", Decision::Allow);
}

fn input_budgets_and_deadlines() {
    struct Slow;
    impl ModelProvider for Slow {
        fn complete(&self, _: &str, _: &str, _: &str, _: Instant) -> Result<String, ProviderError> {
            std::thread::sleep(Duration::from_millis(20));
            Ok("{\"policy\":\"\",\"unresolved\":[]}".into())
        }
    }
    for ttl in [0, 1801] {
        assert!(ProposalContext::new("id".into(), 1, ttl).is_err());
    }
    assert!(ProposalContext::new("bad/id".into(), 1, 30).is_err());
    assert!(ProposalContext::new("id".into(), i64::MAX, 1).is_err());
    let task = TaskInput {
        source_id: "source".into(),
        text: "Review".into(),
    };
    assert!(matches!(
        propose_task(
            &FakeProvider::new("{}".into()),
            "fixture",
            &task,
            &context(),
            Instant::now()
        ),
        Err(ProposalError::DeadlineExceeded)
    ));
    assert!(
        propose_task(
            &Slow,
            "fixture",
            &task,
            &context(),
            Instant::now() + Duration::from_millis(1)
        )
        .is_err()
    );
}

fn prompt_and_wrapper_boundaries() {
    let prompt = include_str!("../prompts/policy-generation.md");
    let (_, constraints) = prompt.split_once("## schema 与生成约束\n").unwrap();
    let (constraints, _) = constraints.split_once("## 路径与输入边界\n").unwrap();
    assert!(constraints.contains("seccore.cedarschema"));
    for duplicated_term in [
        "file.read",
        "file.write",
        "file.exec",
        "WriteOperation",
        "arg_count",
    ] {
        assert!(!constraints.contains(duplicated_term));
    }
    assert!(prompt.len() <= 6000);
    for line in prompt.lines() {
        assert!(line.len() <= 100, "prompt line exceeds 100 bytes: {line}");
    }
    // The prompt accepts no path metadata input.
    for removed in ["paths.workspace", "paths.attachments", "protected_files"] {
        assert!(!prompt.contains(removed), "{removed}");
    }
    let (_, rest) = prompt.split_once("```json\n").unwrap();
    let (example, _) = rest.split_once("\n```").unwrap();
    let response = validate_policy_response(example).unwrap();
    assert!(response.policy.is_empty() && !response.unresolved.is_empty());
    for content in [
        "{}",
        "{\"policy\":null,\"unresolved\":[]}",
        "{\"policy\":\"\",\"policy\":\"\",\"unresolved\":[]}",
        "{\"policy\":\"\",\"unresolved\":[],\"approved\":true}",
    ] {
        assert!(validate_policy_response(content).is_err());
    }
}

#[test]
fn deny_etc_cedar_validate_and_authorize() {
    let policy = std::env::var_os("ASC_INTENT_POLICY").map_or_else(
        || DENY_ETC.into(),
        |path| std::fs::read_to_string(path).unwrap(),
    );
    deny_etc_matrix(&policy);
}
#[test]
fn exact_file_delete_fixture_denies_the_protected_file() {
    exact_delete_matrix();
}
/// Combined task: exact reads, exact create/write, subtree delete denied, argument-less exec,
/// network denied.
fn combination_matrix(policy: &str, root: &str) {
    assert!(!root.is_empty(), "ASC_INTENT_WORKSPACE is required");
    let judge = Judge::new(policy);
    judge.read(&format!("{root}/config/app.yaml"), Decision::Allow);
    judge.read(&format!("{root}/config/important.txt"), Decision::Deny);
    judge.read("/etc/passwd", Decision::Deny);
    judge.write(&format!("{root}/report.txt"), "create", Decision::Allow);
    judge.write(&format!("{root}/report.txt"), "write", Decision::Allow);
    judge.write(&format!("{root}/report.txt"), "delete", Decision::Deny);
    judge.write(&format!("{root}/src/main.rs"), "delete", Decision::Deny);
    judge.check(
        "file.exec",
        "/usr/bin/hostname",
        exec_context(root, &[]),
        Decision::Allow,
    );
    judge.check(
        "file.exec",
        "/usr/bin/hostname",
        exec_context(root, &["-f"]),
        Decision::Deny,
    );
    judge.check("network.use", "", json!({}), Decision::Deny);
}

/// Subtree deletion is allowed while the exact file stays undeletable.
fn protect_delete_matrix(policy: &str, root: &str) {
    assert!(!root.is_empty(), "ASC_INTENT_WORKSPACE is required");
    let judge = Judge::new(policy);
    for path in ["src/main.rs", "config/app.yaml"] {
        judge.write(&format!("{root}/{path}"), "delete", Decision::Allow);
    }
    judge.write(
        &format!("{root}/config/important.txt"),
        "delete",
        Decision::Deny,
    );
    judge.check(
        "file.exec",
        "/usr/bin/hostname",
        exec_context(root, &[]),
        Decision::Deny,
    );
    judge.check("network.use", "", json!({}), Decision::Deny);
}

/// Source subtree read only; no modification, execution or network.
fn code_review_matrix(policy: &str, root: &str) {
    assert!(!root.is_empty(), "ASC_INTENT_WORKSPACE is required");
    let judge = Judge::new(policy);
    judge.read(&format!("{root}/src"), Decision::Allow);
    judge.read(&format!("{root}/src/main.rs"), Decision::Allow);
    judge.read(&format!("{root}/config/app.yaml"), Decision::Deny);
    judge.write(&format!("{root}/report.txt"), "create", Decision::Deny);
    judge.check(
        "file.exec",
        "/bin/ls",
        exec_context(root, &["-l"]),
        Decision::Deny,
    );
    judge.check("network.use", "", json!({}), Decision::Deny);
}

/// Two exact inputs and one exact output; every other action is denied by default.
fn exclusive_report_matrix(policy: &str, root: &str) {
    assert!(!root.is_empty(), "ASC_INTENT_WORKSPACE is required");
    let judge = Judge::new(policy);
    judge.read(&format!("{root}/config/app.yaml"), Decision::Allow);
    judge.read(&format!("{root}/docs/service.txt"), Decision::Allow);
    judge.read(&format!("{root}/src/main.rs"), Decision::Deny);
    judge.write(&format!("{root}/report.txt"), "create", Decision::Allow);
    judge.write(&format!("{root}/report.txt"), "write", Decision::Allow);
    judge.write(&format!("{root}/src/main.rs"), "write", Decision::Deny);
    judge.check(
        "file.exec",
        "/usr/bin/date",
        exec_context(root, &[]),
        Decision::Deny,
    );
    judge.check("network.use", "", json!({}), Decision::Deny);
}

/// Exact argument sequence of a single command; other arguments must not pass.
fn exec_args_matrix(policy: &str, root: &str) {
    assert!(!root.is_empty(), "ASC_INTENT_WORKSPACE is required");
    let judge = Judge::new(policy);
    judge.check(
        "file.exec",
        "/usr/bin/printf",
        exec_context(root, &["%s %s", "hello", "world"]),
        Decision::Allow,
    );
    judge.check(
        "file.exec",
        "/usr/bin/printf",
        exec_context(root, &["%s"]),
        Decision::Deny,
    );
    judge.check(
        "file.exec",
        "/bin/ls",
        exec_context(root, &[]),
        Decision::Deny,
    );
    judge.read(&format!("{root}/config/app.yaml"), Decision::Deny);
    judge.write(&format!("{root}/report.txt"), "create", Decision::Deny);
    judge.check("network.use", "", json!({}), Decision::Deny);
}

#[test]
#[ignore = "verifies a saved v1 candidate via ASC_INTENT_CANDIDATE; no cloud call"]
fn real_model_candidate_case() {
    let path = std::env::var("ASC_INTENT_CANDIDATE").expect("ASC_INTENT_CANDIDATE is required");
    let result: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let case = std::env::var("ASC_INTENT_CASE").unwrap_or_default();
    let root = std::env::var("ASC_INTENT_WORKSPACE").unwrap_or_default();
    if case == "unsupported-network" {
        let policy = result
            .pointer("/candidate/artifact/text")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let draft = result.get("draft").unwrap_or(&result["candidate"]["draft"]);
        assert!(
            policy.is_empty(),
            "no Cedar when endpoints and protocol are inexpressible"
        );
        assert!(!draft["unresolved"].as_array().unwrap().is_empty());
        println!(
            "network endpoint limit is inexpressible: no Cedar generated, unresolved is not empty"
        );
        return;
    }
    assert_eq!(result["outcome"], "candidate");
    generic_candidate(&result["candidate"]);
    let policy = result["candidate"]["artifact"]["text"].as_str().unwrap();
    match case.as_str() {
        "deny-etc" => deny_etc_matrix(policy),
        "combination" => combination_matrix(policy, &root),
        "protect-delete" => protect_delete_matrix(policy, &root),
        "code-review" => code_review_matrix(policy, &root),
        "exclusive-report" => exclusive_report_matrix(policy, &root),
        "exec-args" => exec_args_matrix(policy, &root),
        "" => println!("structure and Strict pass; task semantics not checked"),
        other => panic!("no task assertion for scenario {other} in this adjudication entry"),
    }
}
