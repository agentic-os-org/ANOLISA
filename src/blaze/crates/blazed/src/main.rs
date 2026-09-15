// SPDX-License-Identifier: Apache-2.0
//! Standard `blazed` binary entry point.

use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    blazed::main_entry().await
}
