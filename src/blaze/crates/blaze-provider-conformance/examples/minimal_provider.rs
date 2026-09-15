// SPDX-License-Identifier: Apache-2.0
//! Run the base provider lifecycle against an isolated example implementation.

use blaze_provider_api::{PrepareRequest, PrepareSource, RequestContext};
use blaze_provider_conformance::{ExampleFileProvider, exercise_create_delete};
use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!("blaze-provider-example-{}", Uuid::new_v4()));
    tokio::fs::create_dir(&root).await?;
    let provider = ExampleFileProvider::new(root.clone());
    let request = PrepareRequest {
        context: RequestContext {
            instance_id: Uuid::new_v4(),
            request_id: Uuid::new_v4(),
            operation_id: Uuid::new_v4(),
            lease_id: Uuid::new_v4(),
            generation: 1,
        },
        source: PrepareSource::Image {
            image_digest: "sha256:example".to_string(),
        },
        root_filesystem_bytes: 4096,
        guest_memory_bytes: 4096,
    };

    exercise_create_delete(&provider, request).await?;
    tokio::fs::remove_dir(root).await?;
    Ok(())
}
