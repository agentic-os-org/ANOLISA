// SPDX-License-Identifier: Apache-2.0
//! Compile and run a daemon with an example build-time data-plane provider.

use std::path::PathBuf;

use blaze_provider_conformance::ExampleFileProvider;
use blazed::{BlazeDaemonBuilder, initialize_tracing};
use clap::Parser;

#[derive(Debug, Parser)]
struct Args {
    /// Standard Blaze daemon configuration.
    #[arg(long)]
    daemon_config: PathBuf,
    /// Empty absolute directory for the example provider's resources.
    #[arg(long)]
    resource_root: PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    tokio::fs::create_dir_all(&args.resource_root).await?;
    initialize_tracing();
    BlazeDaemonBuilder::new(ExampleFileProvider::new(args.resource_root))
        .run(&args.daemon_config)
        .await
}
