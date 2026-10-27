//! Explicit in-memory evidence collection followed by deterministic checking.

use std::future::Future;
use std::pin::Pin;

use semantic_arbiter::check::check;
use semantic_arbiter::contract::{
    Binding, Budget, Candidate, EvidenceFact, Operation, Provenance, RebuildResult, Scenario, Task,
    TaskOrigin,
};
use semantic_arbiter::deadline::Ingress;
use semantic_arbiter::evidence::{
    EvidenceRequest, EvidenceSource, EvidenceSourceDescriptor, EvidenceSourceError,
    EvidenceSourceOutput, EvidenceSourceRegistry, SourceEvidence,
};
use semantic_arbiter::protocol::{
    CheckMode, CheckRequest, CHECK_API_VERSION, MAX_EVIDENCE_ITEMS, MAX_MESSAGE_BYTES,
};
use tokio::time::Instant;

struct InMemorySource {
    descriptor: EvidenceSourceDescriptor,
    output: EvidenceSourceOutput,
}

impl EvidenceSource for InMemorySource {
    fn descriptor(&self) -> EvidenceSourceDescriptor {
        self.descriptor.clone()
    }

    fn collect<'a>(
        &'a self,
        _request: EvidenceRequest<'a>,
        _deadline: Instant,
    ) -> Pin<Box<dyn Future<Output = Result<EvidenceSourceOutput, EvidenceSourceError>> + Send + 'a>>
    {
        Box::pin(async move { Ok(self.output.clone()) })
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let ingress = Ingress::capture()?;
    let now_ms = ingress.now_ms();
    let mut request = CheckRequest {
        api_version: CHECK_API_VERSION.into(),
        request_id: "evidence-source-example".into(),
        mode: CheckMode::DeterministicCheck,
        binding: Binding {
            session_id: "session-example".into(),
            instance_id: "instance-example".into(),
            workspace_id: "workspace-example".into(),
            task_version: "task-v1".into(),
            config_revision: "policy-v1".into(),
            candidate_id: "candidate-example".into(),
            candidate_revision: "candidate-v1".into(),
        },
        budget: Budget {
            opened_at_ms: now_ms.saturating_sub(1),
            deadline_ms: now_ms + 5_000,
            max_evidence_items: MAX_EVIDENCE_ITEMS,
            max_evidence_bytes: MAX_MESSAGE_BYTES,
        },
        check_at_ms: now_ms,
        scenarios: vec![Scenario::Preservation],
        task: Some(Task {
            session_id: "session-example".into(),
            version: "task-v1".into(),
            origin: TaskOrigin::Operator,
            text: "Replace the generated resource".into(),
            allowed_resource_ids: vec!["resource-example".into()],
            preserve_resource_ids: vec![],
        }),
        candidate: Candidate {
            operation: Operation::Replace,
            resource_id: "resource-example".into(),
            native_tool: None,
        },
        evidence: vec![],
        gaps: vec![],
    };

    let source = InMemorySource {
        descriptor: EvidenceSourceDescriptor {
            source_id: "in-memory-source".into(),
            source_revision: "source-v1".into(),
            provenance: Provenance::SourceReported,
            supported_scenarios: vec![Scenario::Preservation],
            max_evidence_items: 1,
            max_evidence_bytes: MAX_MESSAGE_BYTES,
            max_age_ms: 5_000,
            timeout_ms: 100,
        },
        output: EvidenceSourceOutput {
            evidence: vec![SourceEvidence {
                id: "synthetic-rebuild".into(),
                captured_at_ms: now_ms,
                valid_until_ms: now_ms + 2_000,
                fact: EvidenceFact::Rebuild {
                    resource_id: "resource-example".into(),
                    result: RebuildResult::Identical,
                },
            }],
            gaps: vec![],
        },
    };
    let mut registry = EvidenceSourceRegistry::new();
    registry.register(source)?;
    let collection = registry
        .collect(
            &["in-memory-source".into()],
            EvidenceRequest::from(&request),
            ingress,
        )
        .await?;
    request.check_at_ms = collection.collected_at_ms;
    request.evidence = collection.evidence;
    request.gaps = collection.gaps;

    let response = check(&request)?;
    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
}
