//! Explicit policy changes address an existing instance, never the candidate's daemon path.

use super::{Arguments, Exit, Result};
use aw_service::Operation;
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
    if !args.flags.contains_key("--agent") && !args.flags.contains_key("--instance") {
        return Err("select --agent or --instance".into());
    }
    let config = super::read_file(args.required("--policy")?, aw_config::MAX_DOCUMENT_BYTES)?;
    aw_config::Validator::new()?.parse(&config)?;
    super::run::install_signals()?;
    let deadline = Instant::now() + Duration::from_secs(30);
    let client = super::connect_service(args, deadline)?;
    let status = client.call_cancellable(Operation::Status, deadline, &super::run::CANCELLED)?;
    let mut matches = status["instances"]
        .as_array()
        .ok_or("service status missing instances")?
        .iter()
        .filter(|instance| {
            args.flags
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
