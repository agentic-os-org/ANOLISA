//! Explicit policy changes address an existing instance, never the candidate's daemon path.

use super::{Arguments, Exit, Result};
use aw_service::Operation;
use serde_json::Value;
use std::time::{Duration, Instant};

pub(super) fn update(args: &Arguments) -> Result<Exit> {
    args.check(
        &[
            "--config",
            "--socket",
            "--agent",
            "--instance",
            "--expected-revision",
            "--policy",
        ],
        false,
    )?;
    if !["--agent", "--instance"]
        .iter()
        .any(|flag| args.flags.contains_key(*flag))
    {
        return Err("select --agent or --instance".into());
    }
    let config = super::read_file(
        args.flags
            .get("--policy")
            .or_else(|| args.flags.get("--config"))
            .ok_or("select --config or provide --policy with --socket")?,
        aw_config::MAX_DOCUMENT_BYTES,
    )?;
    aw_config::Validator::new()?.parse(&config)?;
    super::run::install_signals()?;
    let deadline = Instant::now() + Duration::from_secs(30);
    let client = super::connect_service(args, deadline)?;
    let status = client.call_cancellable(Operation::Status, deadline, &super::run::CANCELLED)?;
    let instance = select(args, &status)?;
    let expected = args
        .flags
        .get("--expected-revision")
        .map(String::as_str)
        .or_else(|| instance["policy_revision"].as_str())
        .ok_or("service status missing policy revision")?;
    let result = client.call_cancellable(
        Operation::UpdatePolicy {
            instance_id: instance["instance_id"]
                .as_str()
                .ok_or("service status missing instance ID")?
                .into(),
            expected_revision: expected.into(),
            config: String::from_utf8(config)?,
        },
        deadline,
        &super::run::CANCELLED,
    )?;
    println!("{}", serde_json::to_string(&result)?);
    Ok(Exit::Code(0))
}

fn select<'a>(args: &Arguments, status: &'a Value) -> Result<&'a Value> {
    let mut matches = status["instances"]
        .as_array()
        .ok_or("service status missing instances")?
        .iter()
        .filter(|instance| {
            // Child task snapshots are not independently updateable targets.
            (args.flags.contains_key("--instance") || instance["parent_instance_id"].is_null())
                && args
                    .flags
                    .get("--agent")
                    .is_none_or(|target| instance["target"].as_str() == Some(target))
                && args
                    .flags
                    .get("--instance")
                    .is_none_or(|id| instance["instance_id"].as_str() == Some(id))
        });
    let instance = matches
        .next()
        .ok_or("no matching instance; use aw status")?;
    if matches.next().is_some() {
        return Err("multiple matching instances; specify --instance".into());
    }
    Ok(instance)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn target_selection_ignores_children_and_requires_one_root() {
        let mut args = Arguments::default();
        args.flags.insert("--agent".into(), "shared".into());
        let root = json!({"instance_id":"root","target":"shared"});
        let child = json!({"instance_id":"child","target":"shared","parent_instance_id":"root"});
        let mut status = json!({"instances":[root,child]});
        assert_eq!(select(&args, &status).unwrap()["instance_id"], "root");
        status["instances"]
            .as_array_mut()
            .unwrap()
            .push(json!({"instance_id":"other","target":"shared"}));
        assert!(select(&args, &status)
            .unwrap_err()
            .to_string()
            .contains("multiple matching"));
        args.flags.insert("--instance".into(), "root".into());
        assert_eq!(select(&args, &status).unwrap()["instance_id"], "root");
        args.flags.insert("--agent".into(), "wrong".into());
        assert!(select(&args, &status).is_err());
        args.flags.remove("--agent");
        args.flags.insert("--instance".into(), "missing".into());
        assert!(select(&args, &status).is_err());
    }
}
