//! Runs the installed CLI and checks the snapshot list after ambiguous writes.

use std::{path::Path, process::Output, time::Duration};

use serde_json::{json, Value};
use tokio::{
    process::Command,
    time::{timeout_at, Instant},
};

const MESSAGE: &str = "AW input.submit";

pub(super) fn ensure_outside_workspace(cwd: &Path, workspace: &Path) -> Result<(), &'static str> {
    // AW starts the Provider in the Agent binding's directory. Compare the
    // actual paths because ws-ckpt can replace the workspace with a managed link.
    let cwd = cwd
        .canonicalize()
        .map_err(|_| "checkpoint_cwd_unavailable")?;
    let workspace = workspace
        .canonicalize()
        .map_err(|_| "checkpoint_workspace_path_unavailable")?;
    if cwd.starts_with(workspace) {
        return Err("checkpoint_cwd_inside_workspace");
    }
    Ok(())
}

pub(super) async fn run(
    cli: &Path,
    workspace: &str,
    checkpoint_id: &str,
    deadline: Instant,
) -> Result<String, &'static str> {
    let metadata = json!({"auto":true,"source":"aw","event":"input.submit"}).to_string();
    let now = Instant::now();
    let remaining = deadline.saturating_duration_since(now);
    if remaining.is_zero() {
        return Err("checkpoint_budget_exhausted");
    }
    // Reserve part of the budget for a read-only lookup after a lost response.
    let create_deadline = deadline - (remaining / 3).min(Duration::from_secs(2));
    let args = [
        "checkpoint",
        "--workspace",
        workspace,
        "--snapshot",
        checkpoint_id,
        "--message",
        MESSAGE,
        "--metadata",
        &metadata,
    ];
    match invoke(cli, &args, create_deadline).await {
        Ok(output) if output.status.success() => {
            let created = format!("\x1b[32m✓ Checkpoint created: {checkpoint_id}\x1b[0m");
            if String::from_utf8_lossy(&output.stdout)
                .lines()
                .any(|line| line == created)
            {
                return Ok(format!("checkpoint_created.{checkpoint_id}"));
            }
            if String::from_utf8_lossy(&output.stderr)
                .contains("Empty workspace, no snapshot created.")
            {
                return Ok("checkpoint_skipped".to_owned());
            }
        }
        Err(CliError::Execution) => return Err("checkpoint_cli_unavailable"),
        _ => {}
    }
    if snapshot_exists(cli, workspace, checkpoint_id, deadline).await {
        return Ok(format!("checkpoint_present.{checkpoint_id}"));
    }
    Err("checkpoint_unconfirmed")
}

async fn snapshot_exists(
    cli: &Path,
    workspace: &str,
    checkpoint_id: &str,
    deadline: Instant,
) -> bool {
    let mut cursor = None::<String>;
    while Instant::now() < deadline {
        let mut args = vec![
            "list",
            "--workspace",
            workspace,
            "--format",
            "json",
            "--limit",
            "100",
        ];
        if let Some(token) = &cursor {
            args.extend(["--cursor", token]);
        }
        let Ok(output) = invoke(cli, &args, deadline).await else {
            return false;
        };
        if !output.status.success() {
            return false;
        }
        let Ok(page) = serde_json::from_slice::<Value>(&output.stdout) else {
            return false;
        };
        let Some(snapshots) = page["snapshots"].as_array() else {
            return false;
        };
        if snapshots.iter().any(|snapshot| {
            let meta = &snapshot["meta"];
            snapshot["id"] == checkpoint_id
                && meta["missing"] == false
                && meta["message"] == MESSAGE
                && meta["metadata"]["auto"] == true
                && meta["metadata"]["source"] == "aw"
                && meta["metadata"]["event"] == "input.submit"
        }) {
            return true;
        }
        let Some(next) = page["next_cursor"].as_str() else {
            return false;
        };
        if cursor.as_deref() == Some(next) {
            return false;
        }
        cursor = Some(next.to_owned());
    }
    false
}

#[derive(Debug)]
enum CliError {
    Timeout,
    Execution,
}

async fn invoke(cli: &Path, args: &[&str], deadline: Instant) -> Result<Output, CliError> {
    timeout_at(
        deadline,
        Command::new(cli).args(args).kill_on_drop(true).output(),
    )
    .await
    .map_err(|_| CliError::Timeout)?
    .map_err(|_| CliError::Execution)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt};

    #[tokio::test]
    async fn calls_existing_cli_and_checks_the_list_after_duplicate_or_failed_write() {
        let root = std::env::temp_dir().join(format!("aw-ws-ckpt-cli-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let cli = root.join("ws-ckpt");
        fs::write(
            &cli,
            r#"#!/bin/sh
set -eu
[ "$2" = --workspace ] && [ "$3" = '/workspace with spaces' ] || exit 64
case "$1" in
    checkpoint)
        [ "$4" = --snapshot ] && [ "$6" = --message ] && [ "$8" = --metadata ] || exit 64
        [ "$5" = aw-case ] || exit 1
        mkdir "${0%/*}/created" 2>/dev/null || exit 1
        printf '\033[32m✓ Checkpoint created: %s\033[0m\n' "$5"
        ;;
    list)
        [ "$4" = --format ] && [ "$5" = json ] && [ "$6" = --limit ] || exit 64
        if [ "$#" -eq 7 ]; then
            printf '%s\n' '{"snapshots":[],"next_cursor":"page-two"}'
        else
            [ "$8" = --cursor ] && [ "$9" = page-two ] || exit 64
            printf '%s\n' '{"snapshots":[{"id":"aw-case","meta":{"missing":false,"message":"AW input.submit","metadata":{"auto":true,"source":"aw","event":"input.submit"}}}],"next_cursor":null}'
        fi
        ;;
    *) exit 64 ;;
esac
"#,
        )
        .unwrap();
        fs::set_permissions(&cli, fs::Permissions::from_mode(0o700)).unwrap();
        let deadline = || Instant::now() + Duration::from_secs(2);
        let workspace = "/workspace with spaces";
        let created = run(&cli, workspace, "aw-case", deadline()).await;
        let duplicate = run(&cli, workspace, "aw-case", deadline()).await;
        let unknown = run(&cli, workspace, "aw-unknown", deadline()).await;
        fs::remove_dir_all(root).unwrap();
        assert_eq!(created.as_deref(), Ok("checkpoint_created.aw-case"));
        assert_eq!(duplicate.as_deref(), Ok("checkpoint_present.aw-case"));
        assert_eq!(unknown, Err("checkpoint_unconfirmed"));
    }
}
