use crate::runtime::prelude::{
    ProviderCancellationArtifactKind, ProviderCancellationArtifactStore,
};

#[derive(Default)]
pub(crate) struct ProviderCancellationArtifactState {
    records: Vec<RuntimeProviderCancellationArtifactRecord>,
}

pub(crate) struct RuntimeProviderCancellationArtifactRecord {
    pub(crate) id: String,
    pub(crate) audit_ref: Option<String>,
    pub(crate) run_id: String,
    pub(crate) provider: &'static str,
    pub(crate) pending_session_id: Option<String>,
    pub(crate) store: ProviderCancellationArtifactStore,
}

impl ProviderCancellationArtifactState {
    pub(crate) fn record_cancelled_run(
        &mut self,
        run_id: String,
        audit_ref: Option<String>,
        provider: &'static str,
        pending_session_id: Option<String>,
        store: ProviderCancellationArtifactStore,
    ) -> String {
        let id = format!("provider-cancel-{}", self.records.len() + 1);
        self.records
            .push(RuntimeProviderCancellationArtifactRecord {
                id: id.clone(),
                audit_ref,
                run_id,
                provider,
                pending_session_id,
                store,
            });
        id
    }

    pub(crate) fn by_id(&self, id: &str) -> Option<&RuntimeProviderCancellationArtifactRecord> {
        self.records.iter().find(|record| record.id == id)
    }
}

impl RuntimeProviderCancellationArtifactRecord {
    pub(crate) fn detail_lines(&self) -> Vec<String> {
        let mut lines = vec![
            format!("artifact_id: {}", self.id),
            format!(
                "audit_ref: {}",
                self.audit_ref.as_deref().unwrap_or("<none>")
            ),
            format!("run_id: {}", self.run_id),
            format!("provider: {}", self.provider),
            format!(
                "pending_session_id: {}",
                self.pending_session_id.as_deref().unwrap_or("<none>")
            ),
        ];

        let artifacts = self.store.snapshot();
        if artifacts.is_empty() {
            lines.push("artifacts: <none captured yet>".to_string());
            return lines;
        }

        lines.push("artifacts:".to_string());
        for artifact in artifacts {
            let kind = match artifact.kind {
                ProviderCancellationArtifactKind::StdoutLine => "stdout_line",
                ProviderCancellationArtifactKind::StderrTail => "stderr_tail",
                ProviderCancellationArtifactKind::PendingSession => "pending_session",
            };
            lines.push(format!("- kind: {kind}"));
            lines.push(format!("  provider: {}", artifact.provider));
            lines.push(format!("  run_id: {}", artifact.run_id));
            lines.push("  text:".to_string());
            lines.extend(artifact.text.lines().map(|line| format!("    {line}")));
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_details_render_only_a_supplied_audit_reference() {
        let mut state = ProviderCancellationArtifactState::default();
        let id = state.record_cancelled_run(
            "run-1".to_string(),
            Some("audit-event-1".to_string()),
            "cosh-core",
            None,
            ProviderCancellationArtifactStore::default(),
        );
        let lines = state.by_id(&id).unwrap().detail_lines();
        assert!(lines.iter().any(|line| line == "audit_ref: audit-event-1"));
    }
}
