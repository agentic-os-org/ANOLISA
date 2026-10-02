use std::{
    sync::{Arc, Barrier},
    thread,
};

use cosh_gateway_contracts::{
    capability::{
        CapabilityDecision, CapabilityRequest, CapabilityScope, DenialCode, OperationDescriptor,
        RuntimeExecutionFence,
    },
    common::{
        ActorKind, ActorRef, AuthAssurance, BoundedName, BoundedOpaque, BoundedText, Digest,
        TargetRef,
    },
    ids::{ActorId, ExecutionId, RequestId, RunId, RuntimeBindingId, TaskId},
};

use super::super::{
    AuthoritativeRequestBinding, BrokerError, CapabilityBroker, MemoryPermitStore, ParentBinding,
    PermitExpectation, PermitStoreError, PolicyDecision, PolicyError, PolicyPort, RequestContext,
};

#[derive(Debug, Clone)]
struct FixedPolicy(PolicyDecision);

impl PolicyPort for FixedPolicy {
    fn evaluate(&self, _request: &CapabilityRequest) -> Result<PolicyDecision, PolicyError> {
        Ok(self.0.clone())
    }
}

#[derive(Debug, Clone)]
struct UnavailablePolicy;

impl PolicyPort for UnavailablePolicy {
    fn evaluate(&self, _request: &CapabilityRequest) -> Result<PolicyDecision, PolicyError> {
        Err(PolicyError::Unavailable)
    }
}

fn digest(byte: char) -> Digest {
    Digest::parse(byte.to_string().repeat(64)).expect("test digest is canonical")
}

fn target(value: &str) -> TargetRef {
    TargetRef {
        kind: BoundedName::new("ecs").expect("test target kind is bounded"),
        authority: BoundedName::new("local").expect("test authority is bounded"),
        identifier: BoundedOpaque::new(value).expect("test target ID is bounded"),
    }
}

fn runtime_fence() -> RuntimeExecutionFence {
    RuntimeExecutionFence {
        binding_id: RuntimeBindingId::new(),
        runtime_generation: 3,
        lease_generation: 4,
        lease_revision: 5,
    }
}

fn request() -> CapabilityRequest {
    CapabilityRequest {
        request_id: RequestId::new(),
        task_id: TaskId::new(),
        run_id: RunId::new(),
        actor: ActorRef {
            actor_id: ActorId::new(),
            actor_kind: ActorKind::Human,
            issuer: BoundedName::new("local-os").expect("test issuer is bounded"),
            assurance: AuthAssurance::LocalOs,
        },
        target: target("instance-1"),
        operation: OperationDescriptor {
            namespace: BoundedName::new("service").expect("test namespace is bounded"),
            name: BoundedName::new("restart").expect("test operation is bounded"),
            arguments_digest: digest('a'),
        },
        operation_digest: digest('c'),
        requested_scope: CapabilityScope {
            resource: BoundedName::new("systemd-unit").expect("test resource is bounded"),
            access: BoundedName::new("mutate").expect("test access is bounded"),
        },
        input_digest: digest('b'),
        expires_at_ms: 2_000,
    }
}

fn context(request: &CapabilityRequest, now_ms: u64) -> RequestContext {
    RequestContext {
        now_ms,
        parent: ParentBinding {
            task_id: request.task_id.clone(),
            run_id: request.run_id.clone(),
            actor: request.actor.clone(),
        },
        binding: AuthoritativeRequestBinding {
            target: request.target.clone(),
            target_identity_digest: digest('d'),
            runtime_fence: runtime_fence(),
            operation: request.operation.clone(),
            operation_digest: request.operation_digest.clone(),
            requested_scope: request.requested_scope.clone(),
            input_digest: request.input_digest.clone(),
        },
    }
}

fn allow_broker() -> CapabilityBroker<FixedPolicy, MemoryPermitStore> {
    CapabilityBroker::new(
        FixedPolicy(PolicyDecision::Allow {
            policy_revision: 7,
            valid_until_ms: 1_500,
        }),
        MemoryPermitStore::default(),
    )
}

fn issued_permit(
    broker: &CapabilityBroker<FixedPolicy, MemoryPermitStore>,
    request: &CapabilityRequest,
) -> cosh_gateway_contracts::capability::ExecutionPermit {
    match broker
        .authorize(request, &context(request, 1_000))
        .expect("valid request is authorized")
    {
        CapabilityDecision::Permit { permit } => permit,
        decision => panic!("expected permit, got {decision:?}"),
    }
}

fn expectation(
    permit: &cosh_gateway_contracts::capability::ExecutionPermit,
    now_ms: u64,
) -> PermitExpectation {
    PermitExpectation {
        permit_id: permit.permit_id.clone(),
        actor_id: permit.actor_id.clone(),
        task_id: permit.task_id.clone(),
        run_id: permit.run_id.clone(),
        execution_id: permit.execution_id.clone(),
        target: permit.target.clone(),
        target_identity_digest: permit.target_identity_digest.clone(),
        runtime_fence: permit.runtime_fence.clone(),
        operation_digest: permit.operation_digest.clone(),
        input_digest: permit.input_digest.clone(),
        policy_revision: permit.policy_revision,
        now_ms,
    }
}

#[test]
fn authorize_fails_closed_for_expiry_and_parent_mismatch() {
    let request = request();
    let broker = allow_broker();

    assert_eq!(
        broker.authorize(&request, &context(&request, request.expires_at_ms)),
        Err(BrokerError::RequestExpired)
    );

    let mut wrong = context(&request, 1_000);
    wrong.parent.task_id = TaskId::new();
    assert_eq!(
        broker.authorize(&request, &wrong),
        Err(BrokerError::RequestTaskMismatch)
    );
    wrong = context(&request, 1_000);
    wrong.parent.run_id = RunId::new();
    assert_eq!(
        broker.authorize(&request, &wrong),
        Err(BrokerError::RequestRunMismatch)
    );
    wrong = context(&request, 1_000);
    wrong.parent.actor.actor_id = ActorId::new();
    assert_eq!(
        broker.authorize(&request, &wrong),
        Err(BrokerError::RequestActorMismatch)
    );
    wrong = context(&request, 1_000);
    wrong.parent.actor.assurance = AuthAssurance::RemoteVerified;
    assert_eq!(
        broker.authorize(&request, &wrong),
        Err(BrokerError::RequestActorMismatch)
    );
    wrong = context(&request, 1_000);
    wrong.parent.actor.issuer =
        BoundedName::new("substituted-issuer").expect("test issuer is bounded");
    assert_eq!(
        broker.authorize(&request, &wrong),
        Err(BrokerError::RequestActorMismatch)
    );
    wrong = context(&request, 1_000);
    wrong.binding.target = target("instance-2");
    assert_eq!(
        broker.authorize(&request, &wrong),
        Err(BrokerError::RequestContentMismatch)
    );
    wrong = context(&request, 1_000);
    wrong.binding.operation.name = BoundedName::new("stop").expect("test operation is bounded");
    assert_eq!(
        broker.authorize(&request, &wrong),
        Err(BrokerError::RequestContentMismatch)
    );
    wrong = context(&request, 1_000);
    wrong.binding.operation_digest = digest('d');
    assert_eq!(
        broker.authorize(&request, &wrong),
        Err(BrokerError::RequestContentMismatch)
    );
    wrong = context(&request, 1_000);
    wrong.binding.requested_scope.access =
        BoundedName::new("observe").expect("test access is bounded");
    assert_eq!(
        broker.authorize(&request, &wrong),
        Err(BrokerError::RequestContentMismatch)
    );
    wrong = context(&request, 1_000);
    wrong.binding.input_digest = digest('d');
    assert_eq!(
        broker.authorize(&request, &wrong),
        Err(BrokerError::RequestContentMismatch)
    );
}

#[test]
fn policy_deny_and_approval_never_issue_a_permit() {
    let request = request();
    let deny = CapabilityBroker::new(
        FixedPolicy(PolicyDecision::Deny {
            code: DenialCode::PolicyDenied,
            safe_message: BoundedText::new("blocked by policy").expect("message is bounded"),
        }),
        MemoryPermitStore::default(),
    );
    assert!(matches!(
        deny.authorize(&request, &context(&request, 1_000)),
        Ok(CapabilityDecision::Deny {
            code: DenialCode::PolicyDenied,
            ..
        })
    ));

    let approval = CapabilityBroker::new(
        FixedPolicy(PolicyDecision::RequireApproval {
            summary: BoundedText::new("restart host service").expect("message is bounded"),
            policy_revision: 7,
            valid_until_ms: 1_500,
        }),
        MemoryPermitStore::default(),
    );
    match approval
        .authorize(&request, &context(&request, 1_000))
        .expect("approval decision is valid")
    {
        CapabilityDecision::RequireApproval { approval } => {
            assert_eq!(approval.request_id, request.request_id);
            assert_eq!(approval.task_id, request.task_id);
            assert_eq!(approval.run_id, request.run_id);
            assert_eq!(approval.expires_at_ms, 1_500);
        }
        decision => panic!("expected approval, got {decision:?}"),
    }
}

#[test]
fn unavailable_policy_propagates_without_issuing_authority() {
    let request = request();
    let broker = CapabilityBroker::new(UnavailablePolicy, MemoryPermitStore::default());
    assert_eq!(
        broker.authorize(&request, &context(&request, 1_000)),
        Err(BrokerError::Policy(PolicyError::Unavailable))
    );
}

#[test]
fn invalid_policy_authority_fails_closed() {
    let request = request();
    let zero_revision = CapabilityBroker::new(
        FixedPolicy(PolicyDecision::Allow {
            policy_revision: 0,
            valid_until_ms: 1_500,
        }),
        MemoryPermitStore::default(),
    );
    assert_eq!(
        zero_revision.authorize(&request, &context(&request, 1_000)),
        Err(BrokerError::InvalidPolicyRevision)
    );

    let expired = CapabilityBroker::new(
        FixedPolicy(PolicyDecision::Allow {
            policy_revision: 7,
            valid_until_ms: 1_000,
        }),
        MemoryPermitStore::default(),
    );
    assert_eq!(
        expired.authorize(&request, &context(&request, 1_000)),
        Err(BrokerError::PolicyDecisionExpired)
    );
}

#[test]
fn issued_permit_binds_every_execution_authority_field() {
    let request = request();
    let broker = allow_broker();
    let permit = issued_permit(&broker, &request);

    assert_eq!(permit.actor_id, request.actor.actor_id);
    assert_eq!(permit.task_id, request.task_id);
    assert_eq!(permit.run_id, request.run_id);
    assert_eq!(permit.target, request.target);
    assert_eq!(permit.operation_digest, request.operation_digest);
    assert_eq!(permit.input_digest, request.input_digest);
    assert_eq!(permit.policy_revision, 7);
    assert_eq!(permit.valid_until_ms, 1_500);
    assert!(permit.single_use);
}

#[test]
fn wrong_permit_bindings_and_full_operation_digest_fail_without_consuming_authority() {
    let request = request();
    let broker = allow_broker();
    let permit = issued_permit(&broker, &request);
    let correct = expectation(&permit, 1_100);

    let mut wrong = correct.clone();
    wrong.actor_id = ActorId::new();
    assert_eq!(
        broker.claim(&wrong),
        Err(BrokerError::Permit(PermitStoreError::ActorMismatch))
    );
    wrong = correct.clone();
    wrong.task_id = TaskId::new();
    assert_eq!(
        broker.claim(&wrong),
        Err(BrokerError::Permit(PermitStoreError::TaskMismatch))
    );
    wrong = correct.clone();
    wrong.run_id = RunId::new();
    assert_eq!(
        broker.claim(&wrong),
        Err(BrokerError::Permit(PermitStoreError::RunMismatch))
    );
    wrong = correct.clone();
    wrong.execution_id = ExecutionId::new();
    assert_eq!(
        broker.claim(&wrong),
        Err(BrokerError::Permit(PermitStoreError::ExecutionMismatch))
    );
    wrong = correct.clone();
    wrong.target = target("instance-2");
    assert_eq!(
        broker.claim(&wrong),
        Err(BrokerError::Permit(PermitStoreError::TargetMismatch))
    );
    wrong = correct.clone();
    wrong.operation_digest = digest('d');
    assert_eq!(
        broker.claim(&wrong),
        Err(BrokerError::Permit(PermitStoreError::OperationMismatch))
    );
    wrong = correct.clone();
    wrong.input_digest = digest('d');
    assert_eq!(
        broker.claim(&wrong),
        Err(BrokerError::Permit(PermitStoreError::InputMismatch))
    );
    wrong = correct.clone();
    wrong.policy_revision += 1;
    assert_eq!(
        broker.claim(&wrong),
        Err(BrokerError::Permit(
            PermitStoreError::PolicyRevisionMismatch
        ))
    );

    assert_eq!(
        broker
            .claim(&correct)
            .expect("mismatches did not consume permit")
            .permit(),
        &permit
    );
    assert_eq!(
        broker.claim(&correct),
        Err(BrokerError::Permit(PermitStoreError::AlreadyConsumed))
    );
}

#[test]
fn expired_permit_fails_closed() {
    let request = request();
    let broker = allow_broker();
    let permit = issued_permit(&broker, &request);
    assert_eq!(
        broker.claim(&expectation(&permit, permit.valid_until_ms)),
        Err(BrokerError::Permit(PermitStoreError::Expired))
    );
}

#[test]
fn exactly_one_concurrent_consumer_claims_a_permit() {
    let request = request();
    let broker = Arc::new(allow_broker());
    let permit = issued_permit(&broker, &request);
    let expected = expectation(&permit, 1_100);
    let barrier = Arc::new(Barrier::new(8));

    let handles: Vec<_> = (0..8)
        .map(|_| {
            let broker = Arc::clone(&broker);
            let expected = expected.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                broker.claim(&expected)
            })
        })
        .collect();

    let results: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().expect("claim thread does not panic"))
        .collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| {
                matches!(
                    result,
                    Err(BrokerError::Permit(PermitStoreError::AlreadyConsumed))
                )
            })
            .count(),
        7
    );
}

#[test]
fn repeated_authorization_replays_the_first_permit() {
    let request = request();
    let broker = allow_broker();

    let first = issued_permit(&broker, &request);
    let replayed = issued_permit(&broker, &request);

    assert_eq!(replayed, first);
}

#[test]
fn concurrent_authorization_issues_one_execution_identity() {
    let request = Arc::new(request());
    let broker = Arc::new(allow_broker());
    let barrier = Arc::new(Barrier::new(8));
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let request = Arc::clone(&request);
            let broker = Arc::clone(&broker);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                broker.authorize(&request, &context(&request, 1_000))
            })
        })
        .collect();

    let decisions = handles
        .into_iter()
        .map(|handle| handle.join().expect("authorization thread does not panic"))
        .collect::<Result<Vec<_>, _>>()
        .expect("equivalent retries are authorized");
    let permits = decisions
        .into_iter()
        .map(|decision| match decision {
            CapabilityDecision::Permit { permit } => permit,
            other => panic!("expected permit, got {other:?}"),
        })
        .collect::<Vec<_>>();

    assert!(permits.windows(2).all(|pair| pair[0] == pair[1]));
}

#[test]
fn reused_request_identity_cannot_change_input_authority() {
    let request = request();
    let broker = allow_broker();
    let _ = issued_permit(&broker, &request);
    let mut substituted = request.clone();
    substituted.input_digest = digest('d');

    assert_eq!(
        broker.authorize(&substituted, &context(&substituted, 1_000)),
        Err(BrokerError::Permit(PermitStoreError::RequestConflict))
    );
}

#[test]
fn first_non_permit_decision_is_replayed_across_policy_changes() {
    let request = request();
    let permits = MemoryPermitStore::default();
    let approval = CapabilityBroker::new(
        FixedPolicy(PolicyDecision::RequireApproval {
            summary: BoundedText::new("restart host service").expect("message is bounded"),
            policy_revision: 7,
            valid_until_ms: 1_500,
        }),
        permits.clone(),
    );
    let first = approval
        .authorize(&request, &context(&request, 1_000))
        .expect("approval decision is recorded");
    let allow = CapabilityBroker::new(
        FixedPolicy(PolicyDecision::Allow {
            policy_revision: 8,
            valid_until_ms: 1_600,
        }),
        permits,
    );

    assert_eq!(
        allow
            .authorize(&request, &context(&request, 1_000))
            .expect("equivalent retry replays the first decision"),
        first
    );

    let mut substituted = request.clone();
    substituted.input_digest = digest('d');
    assert_eq!(
        allow.authorize(&substituted, &context(&substituted, 1_000)),
        Err(BrokerError::Permit(PermitStoreError::RequestConflict))
    );
}
