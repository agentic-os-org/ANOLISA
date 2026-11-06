//! Protocol-level PEP checks; real isolation is covered by the Linux helper suite.
#![cfg(target_os = "linux")]

use asc_daemon_protocol::authorization::{
    AgentPrincipal, AuthorizationRequest, Decision, DecisionVerdict, ExecutionReceipt, LaunchSpec,
    OsPermissionSet, ReceiptAck,
};
use asc_pep_runtime::{
    ApprovalHandler, AuthorizationClient, FixedPermissions, PepError, PepRuntime,
    PreparedInvocation, SandboxBackend, prepare_launch,
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
            approval_requirements: vec![],
            warnings: vec![],
            require_files: vec![],
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
  echo '{"strict_filesystem":true,"noexec":true,"network_off":true}'
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
fn request(launch: LaunchSpec, permissions: OsPermissionSet) -> PreparedInvocation {
    PreparedInvocation::new(&principal(), launch, &FixedPermissions(permissions)).unwrap()
}
#[test]
fn pushed_decisions_reject_replay_changed_argv_and_expiry() {
    let (_directory, mut runtime, launch, permissions) = fixture();
    let client = client(DecisionVerdict::Allow);
    let request = request(launch, permissions.0);
    let decision = client.evaluate(&request.request).unwrap();
    let mut changed = request.clone();
    changed.request.launch.args.push("changed".into());
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
    let decision = client.evaluate(&request.request).unwrap();
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
    let mut perms = permissions.0;
    let mut rule = perms.filesystem.rules[1].clone();
    rule.path = path.to_str().unwrap().into();
    perms.filesystem.rules.push(rule);
    let request = request(launch, perms);
    let decision = client.evaluate(&request.request).unwrap();
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
    let mut decision = client.evaluate(&request.request).unwrap();
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
fn defer_is_terminal_and_reports_once() {
    let (_directory, mut runtime, launch, permissions) = fixture();
    let client = client(DecisionVerdict::Defer);
    let outcome = runtime
        .run(&principal(), launch, &permissions, &client, None)
        .unwrap();
    assert!(!outcome.result.started);
    assert_eq!(client.calls.get(), 1);
    assert_eq!(client.receipts.borrow().len(), 1);
}

#[test]
fn modified_decision_cannot_expand_the_requested_permissions() {
    let (_directory, mut runtime, launch, permissions) = fixture();
    let client = client(DecisionVerdict::Modify);
    let request = request(launch, permissions.0);
    let mut decision = client.evaluate(&request.request).unwrap();
    decision.launch.args.push("authorized".into());
    decision
        .effective_permissions
        .as_mut()
        .unwrap()
        .network
        .mode = asc_daemon_protocol::authorization::NetworkMode::Full;
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
            assert!(
                request
                    .requested_permissions
                    .filesystem
                    .rules
                    .iter()
                    .any(|r| r.path == self.1)
            );
            decision.launch.cwd.clone_from(&self.1);
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
        target.to_str().unwrap().to_owned(),
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
    let mut perms = permissions.0;
    let mut rule = perms.filesystem.rules[1].clone();
    rule.path = alias.to_str().unwrap().into();
    perms.filesystem.rules.push(rule);
    let request = request(launch, perms);
    let client = client(DecisionVerdict::StepUp);
    let decision = client.evaluate(&request.request).unwrap();
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

#[test]
fn impossible_point_grant_fails_before_approval_and_is_reported() {
    struct NeverAsk;
    impl ApprovalHandler for NeverAsk {
        fn ask(&mut self, _: &Decision) -> Result<bool, PepError> {
            panic!("unimplementable grant reached approval")
        }
    }
    let (dir, mut runtime, launch, permissions) = fixture();
    let prepared = request(launch, permissions.0);
    let client = client(DecisionVerdict::StepUp);
    let mut decision = client.evaluate(&prepared.request).unwrap();
    decision.require_files = vec![dir.path().to_str().unwrap().into()];
    let outcome = runtime
        .apply(&prepared, decision, &client, Some(&mut NeverAsk))
        .unwrap();
    assert!(!outcome.result.started);
    assert!(outcome.result.error.unwrap().contains("point-only"));
    assert_eq!(client.receipts.borrow()[0].approval, None);
}
