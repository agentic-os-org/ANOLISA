use std::path::Path;
use std::time::Instant;

use clap::Subcommand;

use cosh_platform::checkpoint::CkptClient;
use cosh_platform::detect::Distro;
use cosh_types::checkpoint::DEFAULT_SOCKET_PATH;
use cosh_types::error::{CoshError, ErrorCode};

use crate::{build_meta, build_meta_with_warning, print_failure, print_success};

#[derive(Subcommand)]
pub enum CheckpointCommands {
    /// Initialize a workspace for checkpointing
    Init {
        /// Workspace path
        #[arg(long)]
        workspace: String,
        /// Custom daemon socket path
        #[arg(long, default_value = DEFAULT_SOCKET_PATH)]
        socket: String,
    },
    /// Recover a workspace
    Recover {
        /// Workspace path
        #[arg(long)]
        workspace: String,
        /// Custom daemon socket path
        #[arg(long, default_value = DEFAULT_SOCKET_PATH)]
        socket: String,
    },
    /// Create a workspace checkpoint
    Create {
        /// Workspace path
        #[arg(long)]
        workspace: String,
        /// Snapshot ID (required)
        #[arg(long, short = 'i')]
        id: String,
        /// Checkpoint message
        #[arg(long, short)]
        message: Option<String>,
        /// JSON metadata string
        #[arg(long)]
        metadata: Option<String>,
        /// Pin this snapshot (prevent auto-cleanup)
        #[arg(long, default_value_t = false)]
        pin: bool,
        /// Custom daemon socket path
        #[arg(long, default_value = DEFAULT_SOCKET_PATH)]
        socket: String,
    },
    /// List checkpoints for a workspace
    List {
        /// Workspace path (optional, lists all if omitted)
        #[arg(long)]
        workspace: Option<String>,
        /// Custom daemon socket path
        #[arg(long, default_value = DEFAULT_SOCKET_PATH)]
        socket: String,
    },
    /// Restore (rollback) to a checkpoint
    Restore {
        /// Snapshot ID to restore to
        id: String,
        /// Workspace path
        #[arg(long)]
        workspace: String,
        /// Custom daemon socket path
        #[arg(long, default_value = DEFAULT_SOCKET_PATH)]
        socket: String,
    },
    /// Show checkpoint status
    Status {
        /// Workspace path (optional, shows all if omitted)
        #[arg(long)]
        workspace: Option<String>,
        /// Custom daemon socket path
        #[arg(long, default_value = DEFAULT_SOCKET_PATH)]
        socket: String,
    },
    /// Delete a snapshot
    Delete {
        /// Snapshot ID to delete
        #[arg(long, short = 's')]
        snapshot: String,
        /// Workspace path (optional)
        #[arg(long)]
        workspace: Option<String>,
        /// Force deletion without confirmation
        #[arg(long, default_value_t = false)]
        force: bool,
        /// Custom daemon socket path
        #[arg(long, default_value = DEFAULT_SOCKET_PATH)]
        socket: String,
    },
    /// Show diff between two snapshots
    Diff {
        /// Workspace path
        #[arg(long)]
        workspace: String,
        /// Source snapshot ID
        #[arg(long, short = 'f')]
        from: String,
        /// Target snapshot ID
        #[arg(long, short = 't')]
        to: String,
        /// Custom daemon socket path
        #[arg(long, default_value = DEFAULT_SOCKET_PATH)]
        socket: String,
    },
    /// Cleanup old snapshots
    Cleanup {
        /// Workspace path
        #[arg(long)]
        workspace: String,
        /// Number of snapshots to keep
        #[arg(long)]
        keep: Option<u32>,
        /// Custom daemon socket path
        #[arg(long, default_value = DEFAULT_SOCKET_PATH)]
        socket: String,
    },
}

/// Validate that the workspace path exists on the local filesystem.
/// Returns a CheckpointNotFound error if the path does not exist.
fn validate_workspace_exists(workspace: &str) -> Result<(), CoshError> {
    if !Path::new(workspace).exists() {
        return Err(CoshError::new(
            ErrorCode::CheckpointNotFound,
            format!(
                "workspace '{}' does not exist on local filesystem",
                workspace
            ),
            "checkpoint",
        )
        .with_hint(
            "check workspace path or run 'cosh-cli checkpoint init --workspace <path>' first",
        )
        .recoverable(false));
    }
    Ok(())
}

pub fn run(action: CheckpointCommands, distro: &Distro, start: Instant) -> i32 {
    let dry_run = false;

    match action {
        CheckpointCommands::Init { workspace, socket } => {
            if let Err(e) = validate_workspace_exists(&workspace) {
                return print_failure(e, build_meta("checkpoint", distro, start, dry_run));
            }
            let client = CkptClient::new(&socket);
            match client.init(&workspace) {
                Ok(result) => {
                    print_success(result, build_meta("checkpoint", distro, start, dry_run))
                }
                Err(e) => print_failure(e, build_meta("checkpoint", distro, start, dry_run)),
            }
        }
        CheckpointCommands::Recover { workspace, socket } => {
            let client = CkptClient::new(&socket);
            match client.recover(&workspace) {
                Ok(mut result) => {
                    let meta = match result.warning.take() {
                        Some(warning) => {
                            build_meta_with_warning("checkpoint", distro, start, dry_run, &warning)
                        }
                        None => build_meta("checkpoint", distro, start, dry_run),
                    };
                    print_success(result, meta)
                }
                Err(e) => print_failure(e, build_meta("checkpoint", distro, start, dry_run)),
            }
        }
        CheckpointCommands::Create {
            workspace,
            id,
            message,
            metadata,
            pin,
            socket,
        } => {
            if let Err(e) = validate_workspace_exists(&workspace) {
                return print_failure(e, build_meta("checkpoint", distro, start, dry_run));
            }
            let client = CkptClient::new(&socket);
            match client.create(
                &workspace,
                &id,
                message.as_deref(),
                metadata.as_deref(),
                pin,
            ) {
                Ok(result) => {
                    print_success(result, build_meta("checkpoint", distro, start, dry_run))
                }
                Err(e) => print_failure(e, build_meta("checkpoint", distro, start, dry_run)),
            }
        }
        CheckpointCommands::List { workspace, socket } => {
            let client = CkptClient::new(&socket);
            match client.list(workspace.as_deref()) {
                Ok(result) => {
                    print_success(result, build_meta("checkpoint", distro, start, dry_run))
                }
                Err(e) => print_failure(e, build_meta("checkpoint", distro, start, dry_run)),
            }
        }
        CheckpointCommands::Restore {
            id,
            workspace,
            socket,
        } => {
            if let Err(e) = validate_workspace_exists(&workspace) {
                return print_failure(e, build_meta("checkpoint", distro, start, dry_run));
            }
            let client = CkptClient::new(&socket);
            match client.restore(&workspace, &id) {
                Ok(result) => {
                    print_success(result, build_meta("checkpoint", distro, start, dry_run))
                }
                Err(e) => print_failure(e, build_meta("checkpoint", distro, start, dry_run)),
            }
        }
        CheckpointCommands::Status { workspace, socket } => {
            let client = CkptClient::new(&socket);
            match client.status(workspace.as_deref()) {
                Ok(result) => {
                    print_success(result, build_meta("checkpoint", distro, start, dry_run))
                }
                Err(e) => print_failure(e, build_meta("checkpoint", distro, start, dry_run)),
            }
        }
        CheckpointCommands::Delete {
            snapshot,
            workspace,
            force,
            socket,
        } => {
            let client = CkptClient::new(&socket);
            match client.delete(workspace.as_deref(), &snapshot, force) {
                Ok(result) => {
                    print_success(result, build_meta("checkpoint", distro, start, dry_run))
                }
                Err(e) => print_failure(e, build_meta("checkpoint", distro, start, dry_run)),
            }
        }
        CheckpointCommands::Diff {
            workspace,
            from,
            to,
            socket,
        } => {
            if let Err(e) = validate_workspace_exists(&workspace) {
                return print_failure(e, build_meta("checkpoint", distro, start, dry_run));
            }
            let client = CkptClient::new(&socket);
            match client.diff(&workspace, &from, &to) {
                Ok(result) => {
                    print_success(result, build_meta("checkpoint", distro, start, dry_run))
                }
                Err(e) => print_failure(e, build_meta("checkpoint", distro, start, dry_run)),
            }
        }
        CheckpointCommands::Cleanup {
            workspace,
            keep,
            socket,
        } => {
            if let Err(e) = validate_workspace_exists(&workspace) {
                return print_failure(e, build_meta("checkpoint", distro, start, dry_run));
            }
            let client = CkptClient::new(&socket);
            match client.cleanup(&workspace, keep) {
                Ok(result) => {
                    print_success(result, build_meta("checkpoint", distro, start, dry_run))
                }
                Err(e) => print_failure(e, build_meta("checkpoint", distro, start, dry_run)),
            }
        }
    }
}
