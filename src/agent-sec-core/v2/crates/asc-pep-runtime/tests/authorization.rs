//! Protocol-level PEP checks; real isolation is covered by the Linux helper suite.
#![cfg(target_os = "linux")]

use asc_pep_runtime::{
    ApprovalHandler, AuthorizationClient, FixedPermissions, PepError, PepRuntime, SandboxBackend,
    prepare_launch, prepare_path_facts,
};
use asc_policy_types::{
    AgentPrincipal, AuthorizationRequest, Decision, DecisionVerdict, ExecutionReceipt, LaunchSpec,
    OsPermissionSet, PepCapabilities, ReceiptAck, RequestType,
};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt as _;
use std::time::{SystemTime, UNIX_EPOCH};

struct Client {
    verdict: DecisionVerdict,
    calls: Cell<usize>,
    receipts: RefCell<Vec<ExecutionReceipt>>,
    fail_report: bool,
}
impl AuthorizationClient for Client {
    fn evaluate(&self, request: &AuthorizationRequest) -> Result<Decision, PepError> {
        self.calls.set(self.calls.get() + 1);
        Ok(Decision {
            decision_id: "decision-1".into(),
            request_id: request.request_id.clone(),
            execution_id: request.execution_id.clone(),
            request_digest: request.digest()?,
            policy_version: "version-1".into(),
            expires_at: now() + 60,
            principal: request.principal.clone(),
            launch: request.launch.clone(),
            verdict: self.verdict,
            effective_permissions: Some(request.requested_permissions.clone()),
            reason: "test".into(),
            warnings: vec![],
            candidate_launch: None,
            missing_paths: vec![],
            continuation: None,
        })
    }
    fn report(&self, receipt: &ExecutionReceipt) -> Result<ReceiptAck, PepError> {
        self.receipts.borrow_mut().push(receipt.clone());
        if self.fail_report {
            return Err(PepError::Protocol("storage unavailable".into()));
        }
        Ok(ReceiptAck {
            decision_id: receipt.decision_id.clone(),
            recorded: true,
        })
    }
}
struct Approval(bool);
impl ApprovalHandler for Approval {
    fn ask(&mut self, _: &Decision) -> Result<bool, PepError> {
        Ok(self.0)
    }
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}
fn principal() -> AgentPrincipal {
    AgentPrincipal {
        agent_id: "test-agent".into(),
        agent_type: "codex".into(),
    }
}
fn permissions(path: &std::path::Path) -> OsPermissionSet {
    serde_json::from_value(serde_json::json!({
        "version":1,"filesystem":{"default":"deny","rules":[
            {"effect":"allow","path":"/usr","rights":["read"]},
            {"effect":"allow","path":path,"rights":["read","write"]}
        ]},"execution":{"deny_paths":[]},"network":{"mode":"off"}
    }))
    .unwrap()
}
fn fixture() -> (tempfile::TempDir, PepRuntime, LaunchSpec, FixedPermissions) {
    let directory = tempfile::tempdir().unwrap();
    let helper = directory.path().join("helper");
    std::fs::write(&helper, r#"#!/bin/bash
if [ "$1" = --capabilities ]; then
  echo '{"strict_filesystem":true,"noexec":true,"network_off":true,"ask":false}'
else
  eval "printf '%s\\n' '{\"stage\":\"installed\"}' '{\"stage\":\"started\"}' '{\"stage\":\"exited\",\"exit_code\":7,\"signal\":null}' >&$4"
fi
"#).unwrap();
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
    let launch =
        prepare_launch("/usr/bin/true", vec![], directory.path(), BTreeMap::new()).unwrap();
    let provider = FixedPermissions(permissions(directory.path()));
    (
        directory,
        PepRuntime::new(SandboxBackend::new(helper)),
        launch,
        provider,
    )
}
fn client(verdict: DecisionVerdict) -> Client {
    Client {
        verdict,
        calls: Cell::new(0),
        receipts: RefCell::new(vec![]),
        fail_report: false,
    }
}
#[test]
fn approval_continues_without_evaluating_twice() {
    let (_directory, mut runtime, launch, permissions) = fixture();
    let client = client(DecisionVerdict::StepUp);
    let result = runtime
        .run(
            &principal(),
            launch,
            &permissions,
            &client,
            Some(&mut Approval(true)),
        )
        .unwrap();
    assert!(result.result.installed, "{:?}", result.result.error);
    assert!(result.result.started, "{:?}", result.result.error);
    assert_eq!(result.result.exit_code, Some(7));
    assert_eq!(client.calls.get(), 1);
    assert_eq!(client.receipts.borrow()[0].approval, Some(true));
}
#[test]
fn absent_or_declined_approval_never_starts_target() {
    for approved in [None, Some(false)] {
        let (_directory, mut runtime, launch, permissions) = fixture();
        let client = client(DecisionVerdict::StepUp);
        let mut channel = Approval(false);
        let approval = approved.map(|_| &mut channel as &mut dyn ApprovalHandler);
        let result = runtime
            .run(&principal(), launch, &permissions, &client, approval)
            .unwrap();
        assert!(!result.result.started);
        assert_eq!(client.receipts.borrow().len(), 1);
        assert_eq!(client.receipts.borrow()[0].approval, approved);
    }
}
#[test]
fn receipt_failure_preserves_completed_execution_without_replay() {
    let (_directory, mut runtime, launch, permissions) = fixture();
    let mut client = client(DecisionVerdict::Allow);
    client.fail_report = true;
    let result = runtime
        .run(&principal(), launch, &permissions, &client, None)
        .unwrap();
    assert!(result.result.started, "{:?}", result.result.error);
    assert_eq!(result.result.exit_code, Some(7));
    assert!(result.receipt_error.is_some());
    assert_eq!(client.calls.get(), 1);
    assert_eq!(client.receipts.borrow().len(), 1);
}
fn request(launch: LaunchSpec, permissions: OsPermissionSet) -> AuthorizationRequest {
    AuthorizationRequest {
        request_type: RequestType::Os,
        request_id: "req".into(),
        execution_id: "exec".into(),
        principal: principal(),
        path_facts: prepare_path_facts(&launch, &permissions, &[]).unwrap(),
        launch,
        requested_permissions: permissions,
        capabilities: PepCapabilities {
            strict_filesystem: true,
            noexec: true,
            network_off: true,
            ask: true,
        },
        continuation: None,
    }
}
#[test]
fn pushed_decisions_reject_replay_changed_argv_and_expiry() {
    let (_directory, mut runtime, launch, permissions) = fixture();
    let client = client(DecisionVerdict::Allow);
    let request = request(launch, permissions.0);
    let decision = client.evaluate(&request).unwrap();
    let mut changed = request.clone();
    changed.launch.args.push("changed".into());
    assert!(
        runtime
            .apply(&changed, decision.clone(), &client, None)
            .is_err()
    );
    let mut expired = decision.clone();
    expired.expires_at = now();
    assert!(runtime.apply(&request, expired, &client, None).is_err());
    assert!(
        runtime
            .apply(&request, decision.clone(), &client, None)
            .unwrap()
            .result
            .started
    );
    assert!(runtime.apply(&request, decision, &client, None).is_err());
    assert_eq!(client.receipts.borrow().len(), 1);
}

#[test]
fn declined_approval_consumes_the_decision() {
    let (_directory, mut runtime, launch, permissions) = fixture();
    let client = client(DecisionVerdict::StepUp);
    let request = request(launch, permissions.0);
    let decision = client.evaluate(&request).unwrap();
    let outcome = runtime
        .apply(
            &request,
            decision.clone(),
            &client,
            Some(&mut Approval(false)),
        )
        .unwrap();
    assert!(!outcome.result.started);
    assert_eq!(client.receipts.borrow()[0].approval, Some(false));
    assert!(
        runtime
            .apply(&request, decision, &client, Some(&mut Approval(true)))
            .is_err()
    );
    assert_eq!(client.calls.get(), 1);
    assert_eq!(client.receipts.borrow().len(), 1);
}

#[test]
fn approval_cannot_refresh_changed_path_identity() {
    struct ReplacingApproval(std::path::PathBuf);
    impl ApprovalHandler for ReplacingApproval {
        fn ask(&mut self, _: &Decision) -> Result<bool, PepError> {
            std::fs::rename(&self.0, self.0.with_extension("previous"))?;
            std::fs::write(&self.0, "replacement")?;
            Ok(true)
        }
    }
    let (directory, mut runtime, launch, permissions) = fixture();
    let path = directory.path().join("bound-file");
    std::fs::write(&path, "original").unwrap();
    let client = client(DecisionVerdict::StepUp);
    let mut request = request(launch, permissions.0);
    request.path_facts = prepare_path_facts(
        &request.launch,
        &request.requested_permissions,
        &[path.to_str().unwrap().to_owned()],
    )
    .unwrap();
    let decision = client.evaluate(&request).unwrap();
    let outcome = runtime
        .apply(
            &request,
            decision.clone(),
            &client,
            Some(&mut ReplacingApproval(path)),
        )
        .unwrap();
    assert!(!outcome.result.started);
    assert!(
        outcome
            .result
            .error
            .unwrap()
            .contains("path identity changed")
    );
    assert_eq!(client.receipts.borrow()[0].approval, Some(true));
    assert!(
        runtime
            .apply(&request, decision, &client, Some(&mut Approval(true)))
            .is_err()
    );
    assert_eq!(client.calls.get(), 1);
    assert_eq!(client.receipts.borrow().len(), 1);
}

#[test]
fn sandbox_preserves_default_host_signals() {
    use rustix::process::{Signal, getpid, kill_process};
    use std::os::unix::process::ExitStatusExt as _;

    let signals = [
        ("INT", Signal::INT),
        ("TERM", Signal::TERM),
        ("HUP", Signal::HUP),
    ];
    if let Ok(selected) = std::env::var("ASC_PEP_TEST_HOST_SIGNAL") {
        let (directory, _runtime, launch, permissions) = fixture();
        let missing = std::env::var_os("ASC_PEP_TEST_MISSING_HELPER").is_some();
        let helper = directory
            .path()
            .join(if missing { "missing" } else { "helper" });
        let result = SandboxBackend::new(helper).execute(launch, permissions.0);
        if missing {
            assert!(result.is_err());
        } else {
            assert_eq!(result.unwrap().exit_code, Some(7));
        }
        let signal = signals
            .into_iter()
            .find(|(name, _)| *name == selected)
            .unwrap()
            .1;
        kill_process(getpid(), signal).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));
        panic!("sandbox call swallowed the host's default {selected} action");
    }
    for (name, signal) in signals {
        for missing in [false, true] {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "sandbox_preserves_default_host_signals",
                    "--nocapture",
                ])
                .env("ASC_PEP_TEST_HOST_SIGNAL", name)
                .env_remove("ASC_PEP_TEST_MISSING_HELPER");
            if missing {
                command.env("ASC_PEP_TEST_MISSING_HELPER", "1");
            }
            let output = command.output().unwrap();
            assert_eq!(
                output.status.signal(),
                Some(signal.as_raw()),
                "missing={missing}: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}

#[test]
fn authorized_modify_and_approval_execute_without_a_second_evaluation() {
    struct TransformClient(Client);
    impl AuthorizationClient for TransformClient {
        fn evaluate(&self, request: &AuthorizationRequest) -> Result<Decision, PepError> {
            let mut decision = self.0.evaluate(request)?;
            decision.launch.args = vec!["authorized".into()];
            Ok(decision)
        }
        fn report(&self, receipt: &ExecutionReceipt) -> Result<ReceiptAck, PepError> {
            self.0.report(receipt)
        }
    }
    struct FinalApproval;
    impl ApprovalHandler for FinalApproval {
        fn ask(&mut self, decision: &Decision) -> Result<bool, PepError> {
            assert_eq!(decision.launch.args, vec!["authorized"]);
            Ok(true)
        }
    }
    for verdict in [DecisionVerdict::Modify, DecisionVerdict::StepUp] {
        let (_directory, mut runtime, launch, permissions) = fixture();
        let client = TransformClient(client(verdict));
        let result = runtime
            .run(
                &principal(),
                launch,
                &permissions,
                &client,
                Some(&mut FinalApproval),
            )
            .unwrap();
        assert!(result.result.started, "{:?}", result.result.error);
        assert_eq!(result.decision.launch.args, vec!["authorized"]);
        assert_eq!(client.0.calls.get(), 1);
        assert_eq!(client.0.receipts.borrow().len(), 1);
    }
}

#[test]
fn modified_resolution_must_match_execution_side_lookup() {
    let (_directory, mut runtime, launch, permissions) = fixture();
    let client = client(DecisionVerdict::Modify);
    let request = request(launch, permissions.0);
    let mut decision = client.evaluate(&request).unwrap();
    decision.launch.resolved_program = "/usr/bin/false".into();
    let result = runtime.apply(&request, decision, &client, None).unwrap();
    assert!(!result.result.started);
    assert!(
        result
            .result
            .error
            .unwrap()
            .contains("authorized resolution")
    );
}

#[test]
fn deferred_preparation_keeps_the_original_permission_request() {
    struct Provider(Cell<usize>, OsPermissionSet);
    impl asc_pep_runtime::PermissionProvider for Provider {
        fn permissions(&self, _: &LaunchSpec) -> Result<OsPermissionSet, PepError> {
            self.0.set(self.0.get() + 1);
            assert_eq!(
                self.0.get(),
                1,
                "permission provider must not widen a continuation"
            );
            Ok(self.1.clone())
        }
    }
    struct DeferredClient(Client);
    impl AuthorizationClient for DeferredClient {
        fn evaluate(&self, request: &AuthorizationRequest) -> Result<Decision, PepError> {
            let mut decision = self.0.evaluate(request)?;
            decision.decision_id = format!("decision-{}", self.0.calls.get());
            if self.0.calls.get() == 1 {
                decision.verdict = DecisionVerdict::Defer;
                let mut candidate = request.launch.clone();
                candidate.args.push("prepared".into());
                decision.candidate_launch = Some(candidate);
                decision.continuation = Some("prepared-token".into());
            }
            Ok(decision)
        }
        fn report(&self, receipt: &ExecutionReceipt) -> Result<ReceiptAck, PepError> {
            self.0.report(receipt)
        }
    }
    let (_directory, mut runtime, launch, permissions) = fixture();
    let provider = Provider(Cell::new(0), permissions.0);
    let client = DeferredClient(client(DecisionVerdict::Modify));
    let result = runtime
        .run(&principal(), launch, &provider, &client, None)
        .unwrap();
    assert!(result.result.started, "{:?}", result.result.error);
    assert_eq!(provider.0.get(), 1);
    assert_eq!(client.0.calls.get(), 2);
}

#[test]
fn modified_decision_cannot_expand_the_requested_permissions() {
    let (_directory, mut runtime, launch, permissions) = fixture();
    let client = client(DecisionVerdict::Modify);
    let request = request(launch, permissions.0);
    let mut decision = client.evaluate(&request).unwrap();
    decision.launch.args.push("authorized".into());
    decision
        .effective_permissions
        .as_mut()
        .unwrap()
        .network
        .mode = asc_policy_types::NetworkMode::Full;
    let outcome = runtime.apply(&request, decision, &client, None).unwrap();
    assert!(!outcome.result.started);
    assert!(
        outcome
            .result
            .error
            .unwrap()
            .contains("effective network exceeds request")
    );
}

#[test]
fn prepared_alias_keeps_both_identities_for_direct_modified_launch() {
    struct AliasClient(Client, String);
    impl AuthorizationClient for AliasClient {
        fn evaluate(&self, request: &AuthorizationRequest) -> Result<Decision, PepError> {
            let mut decision = self.0.evaluate(request)?;
            let alias = request
                .path_facts
                .iter()
                .find(|fact| fact.path == self.1)
                .unwrap();
            let canonical = request
                .path_facts
                .iter()
                .find(|fact| fact.path == alias.canonical_path)
                .unwrap();
            assert_eq!(alias.device, canonical.device);
            assert_eq!(alias.inode, canonical.inode);
            assert_eq!(canonical.path, canonical.canonical_path);
            decision.launch.cwd.clone_from(&canonical.path);
            Ok(decision)
        }
        fn report(&self, receipt: &ExecutionReceipt) -> Result<ReceiptAck, PepError> {
            self.0.report(receipt)
        }
    }
    let (directory, mut runtime, launch, mut permissions) = fixture();
    let target = directory.path().join("real");
    let alias = directory.path().join("alias");
    std::fs::create_dir(&target).unwrap();
    std::os::unix::fs::symlink(&target, &alias).unwrap();
    let mut rule = permissions.0.filesystem.rules[1].clone();
    rule.path = alias.to_str().unwrap().to_owned();
    permissions.0.filesystem.rules.push(rule);
    let client = AliasClient(
        client(DecisionVerdict::Modify),
        alias.to_str().unwrap().to_owned(),
    );
    let outcome = runtime
        .run(&principal(), launch, &permissions, &client, None)
        .unwrap();
    assert!(outcome.result.started, "{:?}", outcome.result.error);
    assert_eq!(outcome.decision.launch.cwd, target.to_str().unwrap());
    assert_eq!(client.0.calls.get(), 1);
}

#[test]
fn canonical_facts_do_not_hide_alias_retargeting_after_approval() {
    struct RetargetApproval(std::path::PathBuf, std::path::PathBuf);
    impl ApprovalHandler for RetargetApproval {
        fn ask(&mut self, _: &Decision) -> Result<bool, PepError> {
            std::fs::remove_file(&self.0)?;
            std::os::unix::fs::symlink(&self.1, &self.0)?;
            Ok(true)
        }
    }
    let (directory, mut runtime, launch, permissions) = fixture();
    let target = directory.path().join("real");
    let other = directory.path().join("other");
    let alias = directory.path().join("alias");
    std::fs::create_dir(&target).unwrap();
    std::fs::create_dir(&other).unwrap();
    std::os::unix::fs::symlink(&target, &alias).unwrap();
    let mut request = request(launch, permissions.0);
    request.path_facts = prepare_path_facts(
        &request.launch,
        &request.requested_permissions,
        &[alias.to_str().unwrap().to_owned()],
    )
    .unwrap();
    assert!(
        request
            .path_facts
            .iter()
            .any(|fact| fact.path == target.to_str().unwrap())
    );
    let client = client(DecisionVerdict::StepUp);
    let decision = client.evaluate(&request).unwrap();
    let outcome = runtime
        .apply(
            &request,
            decision,
            &client,
            Some(&mut RetargetApproval(alias, other)),
        )
        .unwrap();
    assert!(!outcome.result.started);
    assert!(
        outcome
            .result
            .error
            .unwrap()
            .contains("path identity changed")
    );
}
