use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use asc_daemon::{BootstrapConfig, serve};
use asc_daemon_core::{
    PeerCredentials, PolicyAdministration, PolicyAdministrationError, Principal, PrincipalPolicy,
    PrincipalRole, ResourcePage,
};
use asc_daemon_handler::{DaemonDispatcher, JsonRejectionEncoder};
use asc_daemon_protocol::{DaemonRequest, DaemonResponse, RequestId, error_code};
use asc_foundation_types::{ResourceId, Revision};
use asc_pap::PapService;
use asc_pap_repository_memory::ProcessLocalPapRepository;
use asc_policy_engine::PolicyTemplateCompiler;
use asc_policy_repository::{BindingStateRepository, BindingStateWrite, WriteResult};
use asc_policy_types::authoring::PolicyTemplate;
use asc_policy_types::binding::{BindingStatus, BindingView, PreparedBinding};
use asc_policy_types::policy::PreparedPolicy;
use asc_policy_types::scope::{PreparedScope, ScopeSelector};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::UnixStream;
use uuid::Uuid;

mod support;

static DIRECTORY_ID: AtomicU64 = AtomicU64::new(0);

fn transition_binding(
    repository: &ProcessLocalPapRepository,
    id: &ResourceId,
    status: BindingStatus,
) {
    let current = repository.get_binding_state(id).unwrap().unwrap();
    current
        .binding
        .status
        .phase
        .validate_successor(status)
        .unwrap();
    let mut next = current.clone();
    next.binding.status.phase = status;
    assert_eq!(
        repository.compare_exchange_binding_state(&current, &BindingStateWrite::new(next)),
        Ok(WriteResult::Applied)
    );
}

struct RunningPapDaemon {
    directory: PathBuf,
    socket_path: PathBuf,
    shutdown: asc_daemon_service::ShutdownToken,
    task: tokio::task::JoinHandle<()>,
}

impl RunningPapDaemon {
    async fn start(role: PrincipalRole) -> Self {
        Self::start_with_request_limit(role, 4 * 1024 * 1024).await
    }

    async fn start_with_request_limit(role: PrincipalRole, max_request_frame_bytes: usize) -> Self {
        Self::start_with_repository(
            role,
            max_request_frame_bytes,
            Arc::new(ProcessLocalPapRepository::default()),
        )
        .await
    }

    async fn start_with_repository(
        role: PrincipalRole,
        max_request_frame_bytes: usize,
        repository: Arc<ProcessLocalPapRepository>,
    ) -> Self {
        let application = PapService::new(repository, Arc::new(PolicyTemplateCompiler));
        Self::start_with_application(role, max_request_frame_bytes, application).await
    }

    async fn start_with_application(
        role: PrincipalRole,
        max_request_frame_bytes: usize,
        application: impl PolicyAdministration + 'static,
    ) -> Self {
        let directory = unique_directory();
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        let socket_path = directory.join("daemon.sock");
        let dispatcher = Arc::new(DaemonDispatcher::new(
            application,
            Arc::new(FixedRolePolicy(role)),
            asc_daemon::scan_application(
                asc_action_runtime::testing::discarding_finalizer(),
                Arc::new(asc_capability_pii_scan::PiiRuleSet::builtin().unwrap()),
            ),
        ));
        let shutdown = asc_daemon_service::ShutdownToken::new();
        let service_shutdown = shutdown.clone();
        let mut config = BootstrapConfig::new(&socket_path);
        config.service.max_request_frame_bytes = max_request_frame_bytes;
        config.service.request_read_timeout = Duration::from_millis(50);
        let task = tokio::spawn(async move {
            serve(
                config,
                dispatcher,
                Arc::new(JsonRejectionEncoder),
                service_shutdown,
            )
            .await
            .unwrap();
        });

        wait_for_socket(&socket_path).await;
        Self {
            directory,
            socket_path,
            shutdown,
            task,
        }
    }

    async fn stop(self) {
        self.shutdown.request();
        self.task.await.unwrap();
        assert!(!self.socket_path.exists());
        std::fs::remove_dir(self.directory).unwrap();
    }
}

#[derive(Clone, Copy)]
struct FixedRolePolicy(PrincipalRole);

impl PrincipalPolicy for FixedRolePolicy {
    fn role_for(&self, _peer: PeerCredentials) -> PrincipalRole {
        self.0
    }
}

#[derive(Clone)]
struct RecordingAdministration {
    binding: BindingView,
    calls: Arc<Mutex<Vec<&'static str>>>,
}

impl RecordingAdministration {
    fn new() -> Self {
        let spec: PreparedBinding = serde_json::from_str(include_str!(
            "../../../crates/asc-policy-types/tests/fixtures/prepared-binding.json"
        ))
        .unwrap();
        Self {
            binding: BindingView {
                spec,
                status: (BindingStatus::PendingApply).into(),
            },
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn record(
        &self,
        principal: &Principal,
        method: &'static str,
    ) -> Result<(), PolicyAdministrationError> {
        if principal.role() != PrincipalRole::PolicyAdministrator {
            return Err(PolicyAdministrationError::Forbidden);
        }
        self.calls.lock().unwrap().push(method);
        Ok(())
    }

    fn policy(&self) -> PreparedPolicy {
        self.binding.spec.policy.clone()
    }

    fn scope(&self) -> PreparedScope {
        self.binding.spec.scope.clone()
    }
}

impl PolicyAdministration for RecordingAdministration {
    fn create_policy(
        &self,
        principal: &Principal,
        _policy_name: &str,
        _template: &PolicyTemplate,
    ) -> Result<PreparedPolicy, PolicyAdministrationError> {
        self.record(principal, "policy.templates.create")?;
        Ok(self.policy())
    }

    fn update_policy(
        &self,
        principal: &Principal,
        _policy_id: &ResourceId,
        _policy_name: &str,
        _template: &PolicyTemplate,
    ) -> Result<PreparedPolicy, PolicyAdministrationError> {
        self.record(principal, "policy.templates.update")?;
        Ok(self.policy())
    }

    fn get_policy(
        &self,
        principal: &Principal,
        _id: &ResourceId,
        _revision: Revision,
    ) -> Result<PreparedPolicy, PolicyAdministrationError> {
        self.record(principal, "policy.templates.get")?;
        Ok(self.policy())
    }

    fn list_policies(
        &self,
        principal: &Principal,
        _limit: u32,
        _offset: u32,
    ) -> Result<ResourcePage<PreparedPolicy>, PolicyAdministrationError> {
        self.record(principal, "policy.templates.list")?;
        Ok(ResourcePage {
            items: vec![self.policy()],
            total: 1,
        })
    }

    fn delete_policy_revision(
        &self,
        principal: &Principal,
        _id: &ResourceId,
        _revision: Revision,
    ) -> Result<PreparedPolicy, PolicyAdministrationError> {
        self.record(principal, "policy.templates.delete")?;
        Ok(self.policy())
    }

    fn create_scope(
        &self,
        principal: &Principal,
        _selector: &ScopeSelector,
    ) -> Result<PreparedScope, PolicyAdministrationError> {
        self.record(principal, "policy.scopes.create")?;
        Ok(self.scope())
    }

    fn update_scope(
        &self,
        principal: &Principal,
        _scope_id: &ResourceId,
        _selector: &ScopeSelector,
    ) -> Result<PreparedScope, PolicyAdministrationError> {
        self.record(principal, "policy.scopes.update")?;
        Ok(self.scope())
    }

    fn get_scope(
        &self,
        principal: &Principal,
        _id: &ResourceId,
        _revision: Revision,
    ) -> Result<PreparedScope, PolicyAdministrationError> {
        self.record(principal, "policy.scopes.get")?;
        Ok(self.scope())
    }

    fn list_scopes(
        &self,
        principal: &Principal,
        _limit: u32,
        _offset: u32,
    ) -> Result<ResourcePage<PreparedScope>, PolicyAdministrationError> {
        self.record(principal, "policy.scopes.list")?;
        Ok(ResourcePage {
            items: vec![self.scope()],
            total: 1,
        })
    }

    fn delete_scope_revision(
        &self,
        principal: &Principal,
        _id: &ResourceId,
        _revision: Revision,
    ) -> Result<PreparedScope, PolicyAdministrationError> {
        self.record(principal, "policy.scopes.delete")?;
        Ok(self.scope())
    }

    fn create_binding(
        &self,
        principal: &Principal,
        _policy_id: &ResourceId,
        _policy_revision: Revision,
        _scope_id: &ResourceId,
        _scope_revision: Revision,
    ) -> Result<BindingView, PolicyAdministrationError> {
        self.record(principal, "policy.bindings.create")?;
        Ok(self.binding.clone())
    }

    fn update_binding(
        &self,
        principal: &Principal,
        _binding_id: &ResourceId,
        _policy_id: &ResourceId,
        _policy_revision: Revision,
        _scope_id: &ResourceId,
        _scope_revision: Revision,
    ) -> Result<BindingView, PolicyAdministrationError> {
        self.record(principal, "policy.bindings.update")?;
        Ok(self.binding.clone())
    }

    fn get_binding(
        &self,
        principal: &Principal,
        _id: &ResourceId,
    ) -> Result<BindingView, PolicyAdministrationError> {
        self.record(principal, "policy.bindings.get")?;
        Ok(self.binding.clone())
    }

    fn list_bindings(
        &self,
        principal: &Principal,
        _limit: u32,
        _offset: u32,
    ) -> Result<ResourcePage<BindingView>, PolicyAdministrationError> {
        self.record(principal, "policy.bindings.list")?;
        Ok(ResourcePage {
            items: vec![self.binding.clone()],
            total: 1,
        })
    }

    fn delete_binding(
        &self,
        principal: &Principal,
        _id: &ResourceId,
    ) -> Result<BindingView, PolicyAdministrationError> {
        self.record(principal, "policy.bindings.delete")?;
        Ok(self.binding.clone())
    }
}

#[test]
fn all_frozen_methods_route_once_and_return_domain_values_directly() {
    let application = RecordingAdministration::new();
    let calls = Arc::clone(&application.calls);
    let expected_policy = serde_json::to_value(application.policy()).unwrap();
    let expected_scope = serde_json::to_value(application.scope()).unwrap();
    let expected_binding = serde_json::to_value(&application.binding).unwrap();
    let handler = DaemonDispatcher::new(
        application,
        Arc::new(FixedRolePolicy(PrincipalRole::PolicyAdministrator)),
        asc_daemon::scan_application(
            asc_action_runtime::testing::discarding_finalizer(),
            Arc::new(asc_capability_pii_scan::PiiRuleSet::builtin().unwrap()),
        ),
    );
    let fixtures: Vec<Value> = serde_json::from_str(include_str!(
        "../../../crates/asc-daemon-protocol/tests/fixtures/pap-methods.json"
    ))
    .unwrap();

    for (index, fixture) in fixtures.iter().enumerate() {
        let method = fixture["method"].as_str().unwrap();
        let response = handler.handle(
            RequestId::new(format!("request-{index}")).unwrap(),
            PeerCredentials::new(1000, 100, 4242),
            DaemonRequest {
                trace_context: None,
                compatibility: None,
                method: method.to_owned(),
                params: fixture["params"].clone(),
            },
        );
        let DaemonResponse::Success(success) = response else {
            panic!("{method} should succeed");
        };
        let expected = match fixture["resultType"].as_str().unwrap() {
            "PreparedPolicy" => expected_policy.clone(),
            "PreparedScope" => expected_scope.clone(),
            "BindingView" => expected_binding.clone(),
            "ListResult<PreparedPolicy>" => {
                json!({"items": [expected_policy.clone()], "total": 1})
            }
            "ListResult<PreparedScope>" => {
                json!({"items": [expected_scope.clone()], "total": 1})
            }
            "ListResult<BindingView>" => {
                json!({"items": [expected_binding.clone()], "total": 1})
            }
            unexpected => panic!("unexpected result type {unexpected}"),
        };
        assert_eq!(success.result, expected, "wrong direct result for {method}");
    }

    assert_eq!(
        *calls.lock().unwrap(),
        fixtures
            .iter()
            .map(|fixture| fixture["method"].as_str().unwrap())
            .collect::<Vec<_>>()
    );
}

#[test]
fn server_assigned_non_admin_role_is_not_overridden_by_request_data() {
    let application = RecordingAdministration::new();
    let calls = Arc::clone(&application.calls);
    let handler = DaemonDispatcher::new(
        application,
        Arc::new(FixedRolePolicy(PrincipalRole::LocalUser)),
        asc_daemon::scan_application(
            asc_action_runtime::testing::discarding_finalizer(),
            Arc::new(asc_capability_pii_scan::PiiRuleSet::builtin().unwrap()),
        ),
    );
    let response = handler.handle(
        RequestId::new("request-denied").unwrap(),
        PeerCredentials::new(0, 0, 1),
        serde_json::from_value(json!({
            "method": "policy.templates.get",
            "params": {"id": "policy-1", "revision": 1}
        }))
        .unwrap(),
    );

    let DaemonResponse::Error(error) = response else {
        panic!("a local user must not administer Policy");
    };
    assert_eq!(error.error.code.as_str(), error_code::PERMISSION_DENIED);
    assert!(calls.lock().unwrap().is_empty());
}

#[test]
fn unknown_methods_and_invalid_method_params_use_distinct_errors() {
    let handler = DaemonDispatcher::new(
        RecordingAdministration::new(),
        Arc::new(FixedRolePolicy(PrincipalRole::PolicyAdministrator)),
        asc_daemon::scan_application(
            asc_action_runtime::testing::discarding_finalizer(),
            Arc::new(asc_capability_pii_scan::PiiRuleSet::builtin().unwrap()),
        ),
    );
    for (method, params, expected_code, expected_message) in [
        (
            "policy.unknown",
            json!({}),
            error_code::UNKNOWN_METHOD,
            "daemon method is not implemented",
        ),
        (
            "policy.templates.get",
            json!({"id": "policy-1"}),
            error_code::INVALID_REQUEST,
            "missing field `revision`",
        ),
    ] {
        let response = handler.handle(
            RequestId::new(format!("request-{method}")).unwrap(),
            PeerCredentials::new(1000, 100, 4242),
            DaemonRequest {
                trace_context: None,
                compatibility: None,
                method: method.to_owned(),
                params,
            },
        );
        let DaemonResponse::Error(error) = response else {
            panic!("{method} should fail");
        };
        assert_eq!(error.error.code.as_str(), expected_code);
        assert_eq!(error.error.message(), expected_message);
    }
}

fn unique_directory() -> PathBuf {
    std::env::temp_dir().join(format!(
        "asc-daemon-pap-protocol-{}-{}",
        std::process::id(),
        DIRECTORY_ID.fetch_add(1, Ordering::Relaxed)
    ))
}

/// Waits until the endpoint accepts a connection, not merely until it appears.
///
/// Binding publishes the socket file slightly before the listener starts
/// accepting, so a path that already exists can still refuse connections. The
/// window was measured at roughly 10ms on macOS and is narrower but present on
/// Linux, which made the first request after startup fail intermittently. A
/// successful probe connect is the only signal that proves reachability; it is
/// closed at once and costs one of the 64 default connection slots.
async fn wait_for_socket(path: &Path) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Ok(probe) = UnixStream::connect(path).await {
                drop(probe);
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("daemon should accept connections on its socket");
}

async fn uds_request(path: &Path, payload: &[u8]) -> Value {
    let mut stream = UnixStream::connect(path).await.unwrap();
    stream.write_all(payload).await.unwrap();
    let mut response = Vec::new();
    BufReader::new(stream)
        .read_until(b'\n', &mut response)
        .await
        .unwrap();
    assert_eq!(response.pop(), Some(b'\n'));
    serde_json::from_slice(&response).unwrap()
}

fn record_error_response(
    failures: &mut Vec<String>,
    name: &str,
    response: &Value,
    expected_error: &Value,
) {
    let Some(request_id) = response["requestId"].as_str() else {
        failures.push(format!(
            "{name}: response has no string requestId: {response}"
        ));
        return;
    };
    if let Err(error) = Uuid::parse_str(request_id) {
        failures.push(format!("{name}: requestId is not a UUID: {error}"));
    }
    if response.as_object().is_none_or(|fields| fields.len() != 2)
        || response.get("result").is_some()
    {
        failures.push(format!(
            "{name}: error response must contain only requestId and error: {response}"
        ));
    }
    if response["error"] != *expected_error {
        failures.push(format!(
            "{name}: expected error {expected_error}, received {}",
            response["error"]
        ));
    }
}

fn generated_fixture_value(name: &str) -> Option<String> {
    match name {
        "resource_id_129" => Some("a".repeat(129)),
        "policy_name_257" => Some("n".repeat(257)),
        "path_4097" => Some(format!("/{}", "p".repeat(4_096))),
        _ => None,
    }
}

fn expand_generated_fixture_values(value: &Value) -> Value {
    match value {
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, value)| (key.clone(), expand_generated_fixture_values(value)))
                .collect(),
        ),
        Value::Array(items) => {
            Value::Array(items.iter().map(expand_generated_fixture_values).collect())
        }
        Value::String(candidate) if candidate.starts_with("${") && candidate.ends_with('}') => {
            let name = &candidate[2..candidate.len() - 1];
            generated_fixture_value(name).map_or_else(|| value.clone(), Value::String)
        }
        _ => value.clone(),
    }
}

#[derive(Clone, Copy)]
enum ParamKind {
    ResourceId,
    Revision,
    String,
    PolicyTemplate,
    ScopeSelector,
    Limit,
    Offset,
}

struct ParamSpec {
    name: &'static str,
    kind: ParamKind,
    required: bool,
}

fn method_param_specs(method: &str) -> Vec<ParamSpec> {
    let id = |name| ParamSpec {
        name,
        kind: ParamKind::ResourceId,
        required: true,
    };
    let revision = |name| ParamSpec {
        name,
        kind: ParamKind::Revision,
        required: true,
    };
    match method {
        "policy.templates.create" => vec![
            ParamSpec {
                name: "policyName",
                kind: ParamKind::String,
                required: true,
            },
            ParamSpec {
                name: "template",
                kind: ParamKind::PolicyTemplate,
                required: true,
            },
        ],
        "policy.templates.update" => vec![
            id("policyId"),
            ParamSpec {
                name: "policyName",
                kind: ParamKind::String,
                required: true,
            },
            ParamSpec {
                name: "template",
                kind: ParamKind::PolicyTemplate,
                required: true,
            },
        ],
        "policy.templates.get"
        | "policy.templates.delete"
        | "policy.scopes.get"
        | "policy.scopes.delete" => vec![id("id"), revision("revision")],
        "policy.templates.list" | "policy.scopes.list" | "policy.bindings.list" => vec![
            ParamSpec {
                name: "limit",
                kind: ParamKind::Limit,
                required: false,
            },
            ParamSpec {
                name: "offset",
                kind: ParamKind::Offset,
                required: false,
            },
        ],
        "policy.scopes.create" => vec![ParamSpec {
            name: "selector",
            kind: ParamKind::ScopeSelector,
            required: true,
        }],
        "policy.scopes.update" => vec![
            id("scopeId"),
            ParamSpec {
                name: "selector",
                kind: ParamKind::ScopeSelector,
                required: true,
            },
        ],
        "policy.bindings.create" => vec![
            id("policyId"),
            revision("policyRevision"),
            id("scopeId"),
            revision("scopeRevision"),
        ],
        "policy.bindings.update" => vec![
            id("bindingId"),
            id("policyId"),
            revision("policyRevision"),
            id("scopeId"),
            revision("scopeRevision"),
        ],
        "policy.bindings.get" | "policy.bindings.delete" => vec![id("id")],
        unexpected => panic!("unregistered PAP method {unexpected}"),
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the table keeps every bounded scalar error class reviewable in one place"
)]
fn invalid_param_values(kind: ParamKind) -> Vec<(&'static str, Value, &'static str)> {
    match kind {
        ParamKind::ResourceId => vec![
            (
                "wrong type",
                json!(1),
                "invalid type: integer `1`, expected a string",
            ),
            ("null", Value::Null, "invalid type: null, expected a string"),
            (
                "empty",
                json!(""),
                "identifier must be 1..=128 ASCII letters, digits, '.', ':', '_' or '-'",
            ),
            (
                "oversized",
                json!("${resource_id_129}"),
                "identifier must be 1..=128 ASCII letters, digits, '.', ':', '_' or '-'",
            ),
            (
                "unsafe ASCII",
                json!("invalid/id"),
                "identifier must be 1..=128 ASCII letters, digits, '.', ':', '_' or '-'",
            ),
            (
                "non-ASCII",
                json!("非ascii"),
                "identifier must be 1..=128 ASCII letters, digits, '.', ':', '_' or '-'",
            ),
        ],
        ParamKind::Revision => vec![
            (
                "zero",
                json!(0),
                "invalid value: integer `0`, expected a nonzero u32",
            ),
            (
                "negative",
                json!(-1),
                "invalid value: integer `-1`, expected a nonzero u32",
            ),
            (
                "overflow",
                json!(4_294_967_296_u64),
                "invalid value: integer `4294967296`, expected a nonzero u32",
            ),
            (
                "fractional",
                json!(1.5),
                "invalid type: floating point `1.5`, expected a nonzero u32",
            ),
            (
                "wrong type",
                json!("one"),
                "invalid type: string \"one\", expected a nonzero u32",
            ),
            (
                "null",
                Value::Null,
                "invalid type: null, expected a nonzero u32",
            ),
        ],
        ParamKind::String => vec![
            (
                "wrong type",
                json!(1),
                "invalid type: integer `1`, expected a string",
            ),
            ("null", Value::Null, "invalid type: null, expected a string"),
        ],
        ParamKind::PolicyTemplate => vec![(
            "wrong type",
            Value::Null,
            "invalid type: null, expected internally tagged enum PolicyTemplate",
        )],
        ParamKind::ScopeSelector => vec![(
            "wrong type",
            Value::Null,
            "invalid type: null, expected internally tagged enum ScopeSelector",
        )],
        ParamKind::Limit => vec![
            ("zero", json!(0), "limit must be between 1 and 1000"),
            (
                "above maximum",
                json!(1001),
                "limit must be between 1 and 1000",
            ),
            (
                "negative",
                json!(-1),
                "invalid value: integer `-1`, expected u32",
            ),
            (
                "u32 overflow",
                json!(4_294_967_296_u64),
                "invalid value: integer `4294967296`, expected u32",
            ),
            (
                "fractional",
                json!(1.5),
                "invalid type: floating point `1.5`, expected u32",
            ),
            (
                "wrong type",
                json!("one"),
                "invalid type: string \"one\", expected u32",
            ),
            ("null", Value::Null, "invalid type: null, expected u32"),
        ],
        ParamKind::Offset => vec![
            (
                "negative",
                json!(-1),
                "invalid value: integer `-1`, expected u32",
            ),
            (
                "u32 overflow",
                json!(4_294_967_296_u64),
                "invalid value: integer `4294967296`, expected u32",
            ),
            (
                "fractional",
                json!(1.5),
                "invalid type: floating point `1.5`, expected u32",
            ),
            (
                "wrong type",
                json!("one"),
                "invalid type: string \"one\", expected u32",
            ),
            ("null", Value::Null, "invalid type: null, expected u32"),
        ],
    }
}

fn unknown_param_message(specs: &[ParamSpec]) -> String {
    let expected = specs
        .iter()
        .map(|spec| format!("`{}`", spec.name))
        .collect::<Vec<_>>();
    match expected.as_slice() {
        [only] => format!("unknown field `unexpected`, expected {only}"),
        [first, second] => {
            format!("unknown field `unexpected`, expected {first} or {second}")
        }
        _ => format!(
            "unknown field `unexpected`, expected one of {}",
            expected.join(", ")
        ),
    }
}

async fn run_method_param_error_matrix(path: &Path) -> Vec<String> {
    let methods: Vec<Value> = serde_json::from_str(include_str!(
        "../../../crates/asc-daemon-protocol/tests/fixtures/pap-methods.json"
    ))
    .unwrap();
    let mut failures = Vec::new();
    for fixture in methods {
        let method = fixture["method"].as_str().unwrap();
        let base = fixture["params"].as_object().unwrap();
        let specs = method_param_specs(method);
        for spec in &specs {
            if spec.required {
                let mut params = base.clone();
                params.remove(spec.name);
                let response =
                    support::request_json(path, &json!({"method": method, "params": params})).await;
                record_error_response(
                    &mut failures,
                    &format!("{method}: missing {}", spec.name),
                    &response,
                    &json!({
                        "code": "invalid_request",
                        "message": format!("missing field `{}`", spec.name)
                    }),
                );
            }
            for (label, invalid, message) in invalid_param_values(spec.kind) {
                let mut params = base.clone();
                params.insert(
                    spec.name.to_owned(),
                    expand_generated_fixture_values(&invalid),
                );
                let response =
                    support::request_json(path, &json!({"method": method, "params": params})).await;
                record_error_response(
                    &mut failures,
                    &format!("{method}: {label} {}", spec.name),
                    &response,
                    &json!({"code": "invalid_request", "message": message}),
                );
            }
        }

        let mut params = base.clone();
        params.insert("unexpected".to_owned(), json!(true));
        let response =
            support::request_json(path, &json!({"method": method, "params": params})).await;
        record_error_response(
            &mut failures,
            &format!("{method}: unknown parameter"),
            &response,
            &json!({
                "code": "invalid_request",
                "message": unknown_param_message(&specs)
            }),
        );
    }

    let oversized_field = "x".repeat(512);
    let response = support::request_json(
        path,
        &json!({
            "method": "policy.templates.list",
            "params": {oversized_field.clone(): true}
        }),
    )
    .await;
    record_error_response(
        &mut failures,
        "oversized parameter error does not reflect caller input",
        &response,
        &json!({"code": "invalid_request", "message": "request parameters are invalid"}),
    );
    failures
}

async fn run_frozen_error_cases(path: &Path) -> Vec<String> {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../crates/asc-daemon-protocol/tests/fixtures/pap-invalid-requests.json"
    ))
    .unwrap();
    assert_eq!(fixture["schemaVersion"], 1);
    let seed = support::request_json(
        path,
        &json!({
            "method": "policy.templates.create",
            "params": {
                "policyName": "existing-policy",
                "template": {"kind": "prevent_file_deletion", "files": ["/existing"]}
            }
        }),
    )
    .await;
    let existing_policy_id = seed["result"]["policyId"]
        .as_str()
        .expect("error fixture should seed one valid Policy");
    let mut failures = Vec::new();
    for case in fixture["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let mut request = expand_generated_fixture_values(&case["request"]);
        replace_fixture_string(&mut request, "${existing_policy_id}", existing_policy_id);
        let response = support::request_json(path, &request).await;
        record_error_response(&mut failures, name, &response, &case["expectedError"]);
    }
    failures
}

fn replace_fixture_string(value: &mut Value, target: &str, replacement: &str) {
    match value {
        Value::Object(fields) => {
            for value in fields.values_mut() {
                replace_fixture_string(value, target, replacement);
            }
        }
        Value::Array(items) => {
            for value in items {
                replace_fixture_string(value, target, replacement);
            }
        }
        Value::String(value) if value == target => replacement.clone_into(value),
        _ => {}
    }
}

fn assert_error_matrix(failures: &[String]) {
    assert!(
        failures.is_empty(),
        "error contract mismatches:\n{}",
        failures.join("\n")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_uds_executes_the_complete_pap_crud_fixture() {
    let daemon = RunningPapDaemon::start(PrincipalRole::PolicyAdministrator).await;
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../crates/asc-daemon-protocol/tests/fixtures/pap-crud-e2e.json"
    ))
    .unwrap();
    support::run_frozen_pap_crud_scenario(&daemon.socket_path, &fixture).await;
    daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_uds_rejects_every_invalid_crud_parameter_class() {
    let daemon = RunningPapDaemon::start(PrincipalRole::PolicyAdministrator).await;
    let mut failures = run_method_param_error_matrix(&daemon.socket_path).await;
    failures.extend(run_frozen_error_cases(&daemon.socket_path).await);
    daemon.stop().await;
    assert_error_matrix(&failures);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::too_many_lines,
    reason = "the transport error sequence is intentionally kept as one real UDS lifecycle"
)]
async fn real_uds_returns_exact_errors_for_invalid_envelopes_and_transport_frames() {
    let daemon = RunningPapDaemon::start(PrincipalRole::PolicyAdministrator).await;
    let invalid_envelope = json!({
        "code": "invalid_request",
        "message": "request envelope is invalid"
    });
    let mut failures = Vec::new();
    let raw_cases = [
        ("malformed JSON", b"not-json\n".as_slice(), invalid_envelope.clone()),
        ("trailing JSON data", b"{}{}\n".as_slice(), invalid_envelope.clone()),
        ("invalid UTF-8", &[0xff, b'\n'], invalid_envelope.clone()),
        (
            "duplicate envelope method",
            b"{\"method\":\"policy.templates.list\",\"method\":\"policy.scopes.list\",\"params\":{}}\n".as_slice(),
            invalid_envelope.clone(),
        ),
        (
            "duplicate method parameter",
            b"{\"method\":\"policy.templates.list\",\"params\":{\"limit\":0,\"limit\":1}}\n".as_slice(),
            invalid_envelope.clone(),
        ),
    ];
    for (name, payload, expected) in raw_cases {
        let response = uds_request(&daemon.socket_path, payload).await;
        record_error_response(&mut failures, name, &response, &expected);
    }

    let mut empty = UnixStream::connect(&daemon.socket_path).await.unwrap();
    empty.shutdown().await.unwrap();
    let mut empty_response = Vec::new();
    BufReader::new(empty)
        .read_until(b'\n', &mut empty_response)
        .await
        .unwrap();
    assert_eq!(empty_response.pop(), Some(b'\n'));
    let empty_response: Value = serde_json::from_slice(&empty_response).unwrap();
    record_error_response(
        &mut failures,
        "empty frame",
        &empty_response,
        &json!({
            "code": "invalid_request",
            "message": "request frame is empty"
        }),
    );

    for (name, request, expected) in [
        (
            "missing method",
            json!({"params": {}}),
            invalid_envelope.clone(),
        ),
        (
            "non-string method",
            json!({"method": 1, "params": {}}),
            invalid_envelope.clone(),
        ),
        (
            "blank method",
            json!({"method": "  ", "params": {}}),
            invalid_envelope.clone(),
        ),
        (
            "non-object params",
            json!({"method": "policy.templates.list", "params": []}),
            invalid_envelope.clone(),
        ),
        (
            "unknown envelope field",
            json!({"method": "policy.templates.list", "params": {}, "callerUid": 0}),
            invalid_envelope.clone(),
        ),
        (
            "unknown method",
            json!({"method": "policy.unknown", "params": {}}),
            json!({
                "code": "unknown_method",
                "message": "daemon method is not implemented"
            }),
        ),
    ] {
        let response = support::request_json(&daemon.socket_path, &request).await;
        record_error_response(&mut failures, name, &response, &expected);
    }

    let idle = UnixStream::connect(&daemon.socket_path).await.unwrap();
    let mut rejection = Vec::new();
    tokio::time::timeout(
        Duration::from_secs(1),
        BufReader::new(idle).read_until(b'\n', &mut rejection),
    )
    .await
    .expect("idle connection should receive a bounded rejection")
    .unwrap();
    assert_eq!(rejection.pop(), Some(b'\n'));
    let response: Value = serde_json::from_slice(&rejection).unwrap();
    record_error_response(
        &mut failures,
        "request read timeout",
        &response,
        &json!({
            "code": "deadline_exceeded",
            "message": "request read deadline expired"
        }),
    );
    daemon.stop().await;

    let bounded =
        RunningPapDaemon::start_with_request_limit(PrincipalRole::PolicyAdministrator, 64).await;
    let oversized = vec![b'x'; 65];
    let response = uds_request(&bounded.socket_path, &oversized).await;
    record_error_response(
        &mut failures,
        "oversized request frame",
        &response,
        &json!({
            "code": "resource_exhausted",
            "message": "request frame exceeds the configured limit"
        }),
    );
    bounded.stop().await;
    assert_error_matrix(&failures);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_uds_denies_every_crud_method_with_the_exact_public_error() {
    let daemon = RunningPapDaemon::start(PrincipalRole::LocalUser).await;
    let fixtures: Vec<Value> = serde_json::from_str(include_str!(
        "../../../crates/asc-daemon-protocol/tests/fixtures/pap-methods.json"
    ))
    .unwrap();
    let expected = json!({
        "code": "permission_denied",
        "message": "principal is not authorized to administer policy"
    });
    let mut failures = Vec::new();
    for fixture in fixtures {
        let method = fixture["method"].as_str().unwrap();
        let response = support::request_json(
            &daemon.socket_path,
            &json!({"method": method, "params": fixture["params"].clone()}),
        )
        .await;
        record_error_response(&mut failures, method, &response, &expected);
    }
    daemon.stop().await;
    assert_error_matrix(&failures);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::too_many_lines,
    reason = "all legal boundary requests share one daemon and one reviewable sequence"
)]
async fn real_uds_accepts_domain_and_pagination_boundary_values() {
    let daemon = RunningPapDaemon::start(PrincipalRole::PolicyAdministrator).await;
    let maximum_path = format!("/{}", "p".repeat(4_095));
    for (name, request) in [
        (
            "256-byte policy name and 4096-byte path",
            json!({
                "method": "policy.templates.create",
                "params": {
                    "policyName": "n".repeat(256),
                    "template": {
                        "kind": "prevent_file_deletion",
                        "files": [maximum_path]
                    }
                }
            }),
        ),
        (
            "maximum PID",
            json!({
                "method": "policy.scopes.create",
                "params": {"selector": {"kind": "pid", "pid": u32::MAX}}
            }),
        ),
        (
            "maximum cgroup ID",
            json!({
                "method": "policy.scopes.create",
                "params": {"selector": {"kind": "cgroup_id", "cgroupId": u64::MAX}}
            }),
        ),
    ] {
        let response = support::request_json(&daemon.socket_path, &request).await;
        assert!(response.get("result").is_some(), "{name}: {response}");
    }

    for method in [
        "policy.templates.list",
        "policy.scopes.list",
        "policy.bindings.list",
    ] {
        for params in [
            json!({}),
            json!({"limit": 1, "offset": 0}),
            json!({"limit": 1000, "offset": u32::MAX}),
        ] {
            let response = support::request_json(
                &daemon.socket_path,
                &json!({"method": method, "params": params}),
            )
            .await;
            assert!(response.get("result").is_some(), "{method}: {response}");
        }
    }
    daemon.stop().await;

    let daemon = RunningPapDaemon::start_with_application(
        PrincipalRole::PolicyAdministrator,
        4 * 1024 * 1024,
        RecordingAdministration::new(),
    )
    .await;
    let maximum_id = "a".repeat(128);
    let boundary_requests = [
        json!({"method": "policy.templates.update", "params": {
            "policyId": maximum_id,
            "policyName": "valid",
            "template": {"kind": "prevent_file_deletion", "files": ["/"]}
        }}),
        json!({"method": "policy.templates.get", "params": {
            "id": "a", "revision": u32::MAX
        }}),
        json!({"method": "policy.templates.delete", "params": {
            "id": maximum_id, "revision": u32::MAX
        }}),
        json!({"method": "policy.scopes.update", "params": {
            "scopeId": maximum_id,
            "selector": {"kind": "pid", "pid": u32::MAX}
        }}),
        json!({"method": "policy.scopes.get", "params": {
            "id": "a-_.:0", "revision": u32::MAX
        }}),
        json!({"method": "policy.scopes.delete", "params": {
            "id": maximum_id, "revision": u32::MAX
        }}),
        json!({"method": "policy.bindings.create", "params": {
            "policyId": maximum_id,
            "policyRevision": u32::MAX,
            "scopeId": "a",
            "scopeRevision": u32::MAX
        }}),
        json!({"method": "policy.bindings.update", "params": {
            "bindingId": maximum_id,
            "policyId": "a",
            "policyRevision": u32::MAX,
            "scopeId": maximum_id,
            "scopeRevision": u32::MAX
        }}),
        json!({"method": "policy.bindings.get", "params": {"id": maximum_id}}),
        json!({"method": "policy.bindings.delete", "params": {"id": "a"}}),
    ];
    for request in boundary_requests {
        let method = request["method"].as_str().unwrap();
        let response = support::request_json(&daemon.socket_path, &request).await;
        assert!(response.get("result").is_some(), "{method}: {response}");
    }
    daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::too_many_lines,
    reason = "the stateful CRUD error sequence must retain its captured resource identities"
)]
async fn real_uds_distinguishes_stale_references_and_binding_state_conflicts() {
    let repository = Arc::new(ProcessLocalPapRepository::default());
    let daemon = RunningPapDaemon::start_with_repository(
        PrincipalRole::PolicyAdministrator,
        4 * 1024 * 1024,
        Arc::clone(&repository),
    )
    .await;

    let policy = support::request_json(
        &daemon.socket_path,
        &json!({
            "method": "policy.templates.create",
            "params": {
                "policyName": "policy-v1",
                "template": {"kind": "prevent_file_deletion", "files": ["/v1"]}
            }
        }),
    )
    .await;
    let policy_id = policy["result"]["policyId"].as_str().unwrap().to_owned();
    let policy = support::request_json(
        &daemon.socket_path,
        &json!({
            "method": "policy.templates.update",
            "params": {
                "policyId": policy_id,
                "policyName": "policy-v2",
                "template": {"kind": "prevent_file_deletion", "files": ["/v2"]}
            }
        }),
    )
    .await;
    assert_eq!(policy["result"]["revision"], 2);

    let scope = support::request_json(
        &daemon.socket_path,
        &json!({
            "method": "policy.scopes.create",
            "params": {"selector": {"kind": "pid", "pid": 1}}
        }),
    )
    .await;
    let scope_id = scope["result"]["scopeId"].as_str().unwrap().to_owned();
    let scope = support::request_json(
        &daemon.socket_path,
        &json!({
            "method": "policy.scopes.update",
            "params": {
                "scopeId": scope_id,
                "selector": {"kind": "cgroup_id", "cgroupId": 2}
            }
        }),
    )
    .await;
    assert_eq!(scope["result"]["revision"], 2);

    let mut failures = Vec::new();
    for (name, request, expected) in [
        (
            "get stale policy revision",
            json!({"method": "policy.templates.get", "params": {"id": policy_id, "revision": 1}}),
            json!({"code": "not_found", "message": "policy revision was not found"}),
        ),
        (
            "delete stale policy revision",
            json!({"method": "policy.templates.delete", "params": {"id": policy_id, "revision": 1}}),
            json!({"code": "not_found", "message": "policy revision was not found"}),
        ),
        (
            "get stale scope revision",
            json!({"method": "policy.scopes.get", "params": {"id": scope_id, "revision": 1}}),
            json!({"code": "not_found", "message": "scope revision was not found"}),
        ),
        (
            "delete stale scope revision",
            json!({"method": "policy.scopes.delete", "params": {"id": scope_id, "revision": 1}}),
            json!({"code": "not_found", "message": "scope revision was not found"}),
        ),
        (
            "create binding with stale policy revision",
            json!({"method": "policy.bindings.create", "params": {
                "policyId": policy_id,
                "policyRevision": 1,
                "scopeId": scope_id,
                "scopeRevision": 2
            }}),
            json!({"code": "not_found", "message": "referenced policy revision was not found"}),
        ),
        (
            "create binding with stale scope revision",
            json!({"method": "policy.bindings.create", "params": {
                "policyId": policy_id,
                "policyRevision": 2,
                "scopeId": scope_id,
                "scopeRevision": 1
            }}),
            json!({"code": "not_found", "message": "referenced scope revision was not found"}),
        ),
        (
            "create binding with missing scope revision",
            json!({"method": "policy.bindings.create", "params": {
                "policyId": policy_id,
                "policyRevision": 2,
                "scopeId": "missing-scope",
                "scopeRevision": 1
            }}),
            json!({"code": "not_found", "message": "referenced scope revision was not found"}),
        ),
    ] {
        let response = support::request_json(&daemon.socket_path, &request).await;
        record_error_response(&mut failures, name, &response, &expected);
    }

    let binding = support::request_json(
        &daemon.socket_path,
        &json!({"method": "policy.bindings.create", "params": {
            "policyId": policy_id,
            "policyRevision": 2,
            "scopeId": scope_id,
            "scopeRevision": 2
        }}),
    )
    .await;
    let binding_id = binding["result"]["spec"]["bindingId"]
        .as_str()
        .unwrap()
        .to_owned();
    let binding_resource_id = ResourceId::new(binding_id.clone()).unwrap();

    for (name, request, expected) in [
        (
            "update binding with stale policy revision",
            json!({"method": "policy.bindings.update", "params": {
                "bindingId": binding_id,
                "policyId": policy_id,
                "policyRevision": 1,
                "scopeId": scope_id,
                "scopeRevision": 2
            }}),
            json!({"code": "not_found", "message": "referenced policy revision was not found"}),
        ),
        (
            "update binding with stale scope revision",
            json!({"method": "policy.bindings.update", "params": {
                "bindingId": binding_id,
                "policyId": policy_id,
                "policyRevision": 2,
                "scopeId": scope_id,
                "scopeRevision": 1
            }}),
            json!({"code": "not_found", "message": "referenced scope revision was not found"}),
        ),
    ] {
        let response = support::request_json(&daemon.socket_path, &request).await;
        record_error_response(&mut failures, name, &response, &expected);
    }

    transition_binding(&repository, &binding_resource_id, BindingStatus::Applying);
    let identical_apply = support::request_json(
        &daemon.socket_path,
        &json!({"method": "policy.bindings.update", "params": {
            "bindingId": binding_id,
            "policyId": policy_id,
            "policyRevision": 2,
            "scopeId": scope_id,
            "scopeRevision": 2
        }}),
    )
    .await;
    assert_eq!(identical_apply["result"]["status"]["phase"], "APPLYING");
    assert_eq!(identical_apply["result"]["spec"]["bindingRevision"], 1);

    let conflict = json!({
        "code": "conflict",
        "message": "binding reconciliation operation is in progress"
    });
    let response = support::request_json(
        &daemon.socket_path,
        &json!({"method": "policy.bindings.update", "params": {
            "bindingId": binding_id, "policyId": policy_id, "policyRevision": 1,
            "scopeId": scope_id, "scopeRevision": 2
        }}),
    )
    .await;
    record_error_response(
        &mut failures,
        "changed binding update while apply is running",
        &response,
        &conflict,
    );

    let deletion = support::request_json(
        &daemon.socket_path,
        &json!({"method": "policy.bindings.delete", "params": {"id": binding_id}}),
    )
    .await;
    assert_eq!(deletion["result"]["status"]["phase"], "PENDING_DELETE");
    let repeated_pending_deletion = support::request_json(
        &daemon.socket_path,
        &json!({"method": "policy.bindings.delete", "params": {"id": binding_id}}),
    )
    .await;
    assert_eq!(
        repeated_pending_deletion["result"]["status"]["phase"],
        "PENDING_DELETE"
    );
    assert_eq!(
        repeated_pending_deletion["result"]["spec"]["bindingRevision"],
        1
    );
    assert_eq!(
        deletion["result"]["spec"],
        identical_apply["result"]["spec"]
    );
    let response = support::request_json(
        &daemon.socket_path,
        &json!({"method": "policy.bindings.update", "params": {
            "bindingId": binding_id, "policyId": policy_id, "policyRevision": 2,
            "scopeId": scope_id, "scopeRevision": 2
        }}),
    )
    .await;
    record_error_response(
        &mut failures,
        "pending delete cannot be cancelled",
        &response,
        &conflict,
    );
    transition_binding(&repository, &binding_resource_id, BindingStatus::Deleting);
    let repeated_running_deletion = support::request_json(
        &daemon.socket_path,
        &json!({"method": "policy.bindings.delete", "params": {"id": binding_id}}),
    )
    .await;
    assert_eq!(
        repeated_running_deletion["result"]["status"]["phase"],
        "DELETING"
    );
    assert_eq!(
        repeated_running_deletion["result"]["spec"]["bindingRevision"],
        1
    );
    let response = support::request_json(
        &daemon.socket_path,
        &json!({"method": "policy.bindings.update", "params": {
            "bindingId": binding_id,
            "policyId": policy_id,
            "policyRevision": 2,
            "scopeId": scope_id,
            "scopeRevision": 2
        }}),
    )
    .await;
    record_error_response(
        &mut failures,
        "binding update while delete is running",
        &response,
        &conflict,
    );

    transition_binding(
        &repository,
        &binding_resource_id,
        BindingStatus::DeleteFailed,
    );
    let response = support::request_json(
        &daemon.socket_path,
        &json!({"method": "policy.bindings.update", "params": {
            "bindingId": binding_id, "policyId": policy_id, "policyRevision": 2,
            "scopeId": scope_id, "scopeRevision": 2
        }}),
    )
    .await;
    record_error_response(
        &mut failures,
        "failed delete cannot be cancelled",
        &response,
        &conflict,
    );
    let retried = support::request_json(
        &daemon.socket_path,
        &json!({"method": "policy.bindings.delete", "params": {"id": binding_id}}),
    )
    .await;
    assert_eq!(retried["result"], deletion["result"]);

    daemon.stop().await;
    assert_error_matrix(&failures);
}

#[tokio::test]
async fn real_uds_scheduling_rejection_and_get_list_match_frozen_wire() {
    struct Reject(asc_pap::EnqueueError);
    impl asc_pap::BindingReconcileEnqueuer for Reject {
        fn check_ready(&self) -> Result<(), asc_pap::PapError> {
            Ok(())
        }
        fn enqueue(&self, _: &ResourceId) -> Result<(), asc_pap::EnqueueError> {
            Err(self.0)
        }
    }
    let cases: Vec<Value> = serde_json::from_str(include_str!(
        "../../../fixtures/reconciliation/admission-wire.json"
    ))
    .unwrap();
    for case in cases {
        let mut binding: BindingView = serde_json::from_value(case["binding"].clone()).unwrap();
        binding.status = BindingStatus::PendingApply.into();
        binding.status.error = None;
        let spec = binding.spec.clone();
        let repo = Arc::new(
            ProcessLocalPapRepository::with_binding_states(vec![
                asc_policy_repository::BindingStateSnapshot {
                    binding,
                    deployments: vec![],
                },
            ])
            .unwrap(),
        );
        let reason = if case["reason"] == "full" {
            asc_pap::EnqueueError::Full
        } else {
            asc_pap::EnqueueError::Stopped
        };
        let pap = PapService::new(repo, Arc::new(PolicyTemplateCompiler))
            .with_reconcile_enqueuer(Arc::new(Reject(reason)));
        let daemon = RunningPapDaemon::start_with_application(
            PrincipalRole::PolicyAdministrator,
            4 * 1024 * 1024,
            pap,
        )
        .await;
        let operation = case["operation"].as_str().unwrap();
        let params = if operation == "delete" {
            json!({"id": spec.binding_id})
        } else {
            json!({"bindingId": spec.binding_id, "policyId": spec.policy.policy_id, "policyRevision": spec.policy.revision, "scopeId": spec.scope.scope_id, "scopeRevision": spec.scope.revision})
        };
        let request = format!(
            "{}\n",
            json!({"method": format!("policy.bindings.{operation}"), "params": params})
        );
        let response = uds_request(&daemon.socket_path, request.as_bytes()).await;
        assert!(response.get("error").is_none());
        assert_eq!(response["result"], case["binding"]);
        let get = format!(
            "{}\n",
            json!({"method":"policy.bindings.get", "params":{"id":spec.binding_id}})
        );
        let response = uds_request(&daemon.socket_path, get.as_bytes()).await;
        assert_eq!(response["result"], case["binding"]);
        let response = uds_request(
            &daemon.socket_path,
            b"{\"method\":\"policy.bindings.list\",\"params\":{}}\n",
        )
        .await;
        assert_eq!(response["result"]["items"], json!([case["binding"]]));
        daemon.stop().await;
    }
}

#[tokio::test]
async fn unavailable_preflight_returns_wire_error_without_creating_binding() {
    let repo = Arc::new(ProcessLocalPapRepository::default());
    let pap = PapService::new(repo, Arc::new(PolicyTemplateCompiler))
        .with_reconcile_enqueuer(Arc::new(asc_daemon::UnavailableReconciliation));
    let daemon = RunningPapDaemon::start_with_application(
        PrincipalRole::PolicyAdministrator,
        4 * 1024 * 1024,
        pap,
    )
    .await;
    let response = uds_request(&daemon.socket_path, b"{\"method\":\"policy.bindings.create\",\"params\":{\"policyId\":\"10000000-0000-4000-8000-000000000001\",\"policyRevision\":1,\"scopeId\":\"10000000-0000-4000-8000-000000000002\",\"scopeRevision\":1}}\n").await;
    assert_eq!(
        response["error"],
        json!({"code":"unavailable", "message":"reconciliation runtime is unavailable"})
    );
    assert!(response.get("result").is_none());
    let response = uds_request(
        &daemon.socket_path,
        b"{\"method\":\"policy.bindings.list\",\"params\":{}}\n",
    )
    .await;
    assert_eq!(response["result"], json!({"items":[], "total":0}));
    daemon.stop().await;
}
