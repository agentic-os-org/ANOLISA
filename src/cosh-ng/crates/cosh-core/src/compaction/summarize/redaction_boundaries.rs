use super::*;
use crate::provider::mock::MockProvider;

#[tokio::test]
async fn redaction_boundaries_generated_summary() {
    let input = "## Objective\n--password 'canary multi words'\nSECRET_KEY=canary-secret-material\nprivate_key=canary-private-material\naccess_key=canary-access-material\ntrace Cookie: sid=canary-cookie-material; mode=canary-cookie-tail\nregion=ordinary-value";
    let provider = MockProvider::text_only(input);
    let summary = generate_summary(&provider, "mock-model", "ordinary input")
        .await
        .expect("summary generated");
    let leaked = [
        "canary multi",
        "words",
        "canary-secret-material",
        "canary-private-material",
        "canary-access-material",
        "canary-cookie-material",
        "canary-cookie-tail",
    ]
    .iter()
    .filter(|value| summary.contains(**value))
    .count();
    assert!(
        leaked == 0,
        "generated summary masking failed: {leaked} canaries remain"
    );
    assert!(
        summary.contains("## Objective") && summary.contains("region=ordinary-value"),
        "ordinary summary changed"
    );
}
