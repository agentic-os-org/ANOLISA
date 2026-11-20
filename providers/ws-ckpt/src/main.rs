//! Adapts AW input submissions to the installed ws-ckpt CLI.

mod checkpoint;
mod policy;
mod skill;

use std::{
    io::{self, Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

use aw_provider::{Protocol, MAX_MESSAGE_BYTES, VERSION};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::time::Instant;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    match args.split_first() {
        Some((command, rest)) if command == "policy" => {
            return policy::control(rest).map_err(Into::into)
        }
        Some((command, rest)) if command == "bind" => {
            let (target, workspace) =
                match rest {
                    [agent_flag, target, workspace_flag, workspace]
                        if agent_flag == "--agent" && workspace_flag == "--workspace" =>
                    {
                        (target, workspace)
                    }
                    [workspace_flag, workspace, agent_flag, target]
                        if agent_flag == "--agent" && workspace_flag == "--workspace" =>
                    {
                        (target, workspace)
                    }
                    _ => return Err(
                        "usage: aw-provider-ws-ckpt bind --agent TARGET --workspace ABSOLUTE_PATH"
                            .into(),
                    ),
                };
            let target = target.to_str().ok_or("invalid AW target")?;
            let workspace = workspace.to_str().ok_or("invalid workspace path")?;
            policy::bind(&policy::account_policy_path()?, target, workspace)?;
            println!("{}", json!({"target":target,"workspace":workspace}));
            return Ok(());
        }
        _ => {}
    }
    let cli = match args.as_slice() {
        [flag, path] if flag == "--cli" && Path::new(path).is_absolute() => Path::new(path),
        _ => return Err("usage: aw-provider-ws-ckpt --cli ABSOLUTE_PATH".into()),
    };
    let mut bytes = Vec::new();
    io::stdin()
        .lock()
        .take((MAX_MESSAGE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    let protocol = Protocol::new()?;
    let request = protocol.parse_request(&bytes)?;
    let value = request.as_value();
    let mut response = json!({
        "api_version": VERSION, "request_id": value["request_id"], "status": "ok"
    });
    match dispatch(value, cli, Path::new("."), policy::account_policy_path).await {
        Ok(fields) => {
            for (key, value) in fields {
                response[key] = value;
            }
        }
        Err(code) => {
            response["status"] = json!("error");
            response["error_code"] = json!(code);
        }
    }
    let bytes = serde_json::to_vec(&response)?;
    match protocol.check_response(&request, &bytes) {
        Ok(_) | Err(aw_provider::Error::ProviderFailure { .. }) => {}
        Err(error) => return Err(error.into()),
    }
    let mut stdout = io::stdout().lock();
    stdout.write_all(&bytes)?;
    stdout.write_all(b"\n")?;
    Ok(())
}

async fn dispatch(
    request: &Value,
    cli: &Path,
    cwd: &Path,
    policy_path: impl FnOnce() -> io::Result<PathBuf>,
) -> std::result::Result<serde_json::Map<String, Value>, &'static str> {
    let method = request["method"].as_str().ok_or("invalid_method")?;
    if method == "describe" {
        return Ok(serde_json::Map::from_iter([(
            "operations".to_owned(),
            json!([{"name":"checkpoint","events":["input.submit"],"effects":["observe"]}]),
        )]));
    }
    if request["config"].get("workspace").is_some() {
        return Err("workspace_is_provider_managed");
    }
    if request["config"].get("policy_file").is_some() {
        return Err("policy_file_is_provider_managed");
    }
    let skills = skill::configured(&request["config"]).map_err(|error| {
        eprintln!("invalid ws-ckpt Provider Skill configuration: {error}");
        "invalid_skill_configuration"
    })?;
    if method == "validate_config" {
        return Ok(serde_json::Map::new());
    }
    let event = &request["event"];
    if method != "invoke"
        || request["operation"] != "checkpoint"
        || event["name"] != "input.submit"
        || request["allowed_effects"] != json!(["observe"])
    {
        return Err("unsupported_operation");
    }
    let budget = request["budget_ms"].as_u64().ok_or("invalid_budget")?;
    let deadline = Instant::now() + Duration::from_millis(budget.min(6000).saturating_sub(25));
    let target = event["agent"]["binding_id"]
        .as_str()
        .ok_or("missing_agent_target")?;
    let installation = skills
        .get(target)
        .and_then(|install| match install.apply(cwd) {
            Ok(true) => Some("skill_installed"),
            Ok(false) => None,
            Err(error) => {
                eprintln!("failed to prepare ws-ckpt Skill: {error}");
                Some("skill_install_failed")
            }
        });
    let file = policy_path().map_err(|error| {
        eprintln!("failed to resolve snapshot policy path: {error}");
        "snapshot_policy_unavailable"
    })?;
    let policy = policy::read(&file).map_err(|_| "snapshot_policy_unavailable")?;
    let reason = match policy.workspace(target) {
        Some(workspace) if policy.enabled(workspace, target) => {
            let checkpoint_id = checkpoint_id(workspace, event)?;
            let outcome = match checkpoint::ensure_outside_workspace(cwd, Path::new(workspace)) {
                Ok(()) => checkpoint::run(cli, workspace, &checkpoint_id, deadline).await,
                Err(reason) => Err(reason),
            };
            let result = outcome.as_deref().unwrap_or_else(|&error| error);
            if let Err(error) = policy::record(&file, workspace, target, result) {
                eprintln!("failed to record latest checkpoint result: {error}");
            }
            outcome?
        }
        Some(_) => "checkpoint_disabled".to_owned(),
        None => "checkpoint_unbound".to_owned(),
    };
    let mut effects = vec![json!({"type":"observe","reason_code":reason})];
    if let Some(installation) = installation {
        effects.push(json!({"type":"observe","reason_code":installation}));
    }
    Ok(serde_json::Map::from_iter([
        ("input_digest".to_owned(), request["input_digest"].clone()),
        ("effects".to_owned(), json!(effects)),
    ]))
}

fn checkpoint_id(workspace: &str, event: &Value) -> std::result::Result<String, &'static str> {
    let target = event["agent"]["binding_id"]
        .as_str()
        .ok_or("missing_submission_identity")?;
    let instance = event["agent"]["instance_id"]
        .as_str()
        .ok_or("missing_submission_identity")?;
    let session = event["session_id"]
        .as_str()
        .ok_or("missing_submission_identity")?;
    let turn = event["turn_id"]
        .as_str()
        .ok_or("missing_submission_identity")?;
    // Each Scope numbers submissions independently, even if it uses the same session name.
    let key = json!(["aw-ws-ckpt/v2", workspace, target, instance, session, turn]);
    let digest = Sha256::digest(key.to_string().as_bytes());
    Ok(format!("aw-{digest:x}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::{symlink, DirBuilderExt, PermissionsExt},
    };

    #[test]
    fn snapshot_ids_distinguish_submissions_and_scopes() {
        let mut event = json!({
            "agent": {"binding_id": "claude", "instance_id": "first-scope"},
            "session_id": "default", "turn_id": "1",
            "input": {"text": "prompt"}
        });
        let initial = checkpoint_id("/workspace", &event).unwrap();
        event["input"]["text"] = json!("a different prompt projection");
        assert_eq!(checkpoint_id("/workspace", &event).unwrap(), initial);
        event["agent"]["instance_id"] = json!("second-scope");
        assert_ne!(checkpoint_id("/workspace", &event).unwrap(), initial);
        event["agent"]["instance_id"] = json!("first-scope");
        event["turn_id"] = json!("2");
        assert_ne!(checkpoint_id("/workspace", &event).unwrap(), initial);
    }

    #[tokio::test]
    async fn configured_binding_is_rejected_during_validation() {
        let mut request = json!({
            "method": "validate_config",
            "config": {},
        });
        let unused_path = || unreachable!("validation must not read policy state");
        assert!(
            dispatch(&request, Path::new("/ws-ckpt"), Path::new("/"), unused_path)
                .await
                .is_ok()
        );
        request["config"]["policy_file"] = json!("/legacy.json");
        assert_eq!(
            dispatch(&request, Path::new("/ws-ckpt"), Path::new("/"), unused_path).await,
            Err("policy_file_is_provider_managed")
        );
        request["config"] = json!({"workspace":"/project"});
        assert_eq!(
            dispatch(&request, Path::new("/ws-ckpt"), Path::new("/"), unused_path).await,
            Err("workspace_is_provider_managed")
        );
    }

    #[tokio::test]
    async fn first_submission_installs_configured_skill_even_without_snapshots() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("SKILL.md"), "ws-ckpt Skill").unwrap();
        let destination = root.path().join(".qoder/skills/ws-ckpt/SKILL.md");
        let file = || Ok(root.path().join("state/policy.json"));
        let cli = root.path().join("missing-cli");
        let mut request = json!({
            "method": "validate_config",
            "config": {"skills": {"qoder": {
                "source": source, "destination": ".qoder/skills/ws-ckpt"
            }}}
        });
        assert!(dispatch(&request, &cli, root.path(), file).await.is_ok());
        assert!(!destination.exists());
        request["method"] = json!("invoke");
        request["operation"] = json!("checkpoint");
        request["allowed_effects"] = json!(["observe"]);
        request["budget_ms"] = json!(1000);
        request["event"] = json!({"name":"input.submit","agent":{"binding_id":"other"}});
        let result = dispatch(&request, &cli, root.path(), file).await.unwrap();
        assert_eq!(result["effects"][0]["reason_code"], "checkpoint_unbound");
        assert!(!destination.exists());

        request["event"]["agent"]["binding_id"] = json!("qoder");
        let result = dispatch(&request, &cli, root.path(), file).await.unwrap();
        assert_eq!(result["effects"][1]["reason_code"], "skill_installed");
        assert_eq!(fs::read_to_string(&destination).unwrap(), "ws-ckpt Skill");
        fs::write(&destination, "user Skill").unwrap();
        let result = dispatch(&request, &cli, root.path(), file).await.unwrap();
        assert_eq!(result["effects"].as_array().unwrap().len(), 1);
        assert_eq!(fs::read_to_string(&destination).unwrap(), "user Skill");
    }

    #[tokio::test]
    async fn each_submission_uses_the_latest_policy_for_its_workspace_and_target() {
        let directory = std::env::temp_dir().join(format!("aw-policy-{}", std::process::id()));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .unwrap();
        let file = directory.join(".local/state/ws-ckpt/aw-policy.json");
        let policy_path = || Ok(file.clone());
        let cli = directory.join("missing-cli");
        let project = directory.join("project");
        let workspace = project.to_str().unwrap();
        let child = project.join("child");
        fs::create_dir_all(&child).unwrap();
        let mut request = json!({
            "method":"invoke", "operation":"checkpoint", "allowed_effects":["observe"],
            "budget_ms": 1000, "input_digest":"digest",
            "config":{},
            "event":{"name":"input.submit","agent":{"binding_id":"qoder-dev","instance_id":"instance"},"session_id":"s","turn_id":"t"}
        });
        let unbound = dispatch(&request, &cli, &directory, policy_path)
            .await
            .unwrap();
        assert_eq!(unbound["effects"][0]["reason_code"], "checkpoint_unbound");
        assert!(!file.parent().unwrap().exists());
        policy::bind(&file, "qoder-dev", workspace).unwrap();
        let off = dispatch(&request, &cli, &directory, policy_path)
            .await
            .unwrap();
        assert_eq!(off["effects"][0]["reason_code"], "checkpoint_disabled");
        policy::set_enabled(&file, workspace, "qoder-dev", true).unwrap();
        assert_eq!(
            fs::metadata(file.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        policy::bind(&file, "qoder-review", "/docs").unwrap();
        policy::set_enabled(&file, "/docs", "qoder-review", true).unwrap();

        let cwd_alias = directory.join("cwd-alias");
        symlink(&child, &cwd_alias).unwrap();
        for cwd in [&project, &child, &cwd_alias] {
            assert_eq!(
                dispatch(&request, &cli, cwd, policy_path).await,
                Err("checkpoint_cwd_inside_workspace")
            );
        }
        let saved: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        assert_eq!(
            saved["workspaces"][workspace]["qoder-dev"]["last_result"],
            "checkpoint_cwd_inside_workspace"
        );
        let workspace_alias = directory.join("workspace-alias");
        symlink(&project, &workspace_alias).unwrap();
        let alias = workspace_alias.to_str().unwrap();
        policy::bind(&file, "qoder-dev", alias).unwrap();
        assert!(policy::set_enabled(&file, workspace, "qoder-dev", false).is_err());
        assert_eq!(
            dispatch(&request, &cli, &child, policy_path).await.unwrap(),
            off
        );
        policy::set_enabled(&file, alias, "qoder-dev", true).unwrap();
        assert_eq!(
            dispatch(&request, &cli, &child, policy_path).await,
            Err("checkpoint_cwd_inside_workspace")
        );
        policy::bind(&file, "qoder-dev", workspace).unwrap();

        assert_eq!(
            dispatch(&request, &cli, &directory, policy_path).await,
            Err("checkpoint_cli_unavailable")
        );
        request["event"]["agent"]["binding_id"] = json!("qoder-other");
        assert_eq!(
            dispatch(&request, &cli, &child, policy_path).await.unwrap(),
            unbound
        );
        policy::set_enabled(&file, workspace, "qoder-dev", false).unwrap();
        request["event"]["agent"]["binding_id"] = json!("qoder-dev");
        assert_eq!(
            dispatch(&request, &cli, &child, policy_path).await.unwrap(),
            off
        );
        assert!(policy::read(&file)
            .unwrap()
            .enabled("/docs", "qoder-review"));
        fs::write(&file, "{bad json").unwrap();
        assert_eq!(
            dispatch(&request, &cli, &directory, policy_path).await,
            Err("snapshot_policy_unavailable")
        );
        fs::remove_dir_all(directory).unwrap();
    }
}
