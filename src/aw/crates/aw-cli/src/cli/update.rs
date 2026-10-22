//! Explicit policy changes address an existing instance, never the candidate's daemon path.

use super::{Arguments, Exit, Result};
use aw_service::Operation;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub(super) fn update(args: &Arguments) -> Result<Exit> {
    args.check(
        &[
            "--config",
            "--socket",
            "--agent",
            "--instance",
            "--pid",
            "--expected-revision",
            "--policy",
        ],
        false,
    )?;
    if !["--agent", "--instance", "--pid"]
        .iter()
        .any(|flag| args.flags.contains_key(*flag))
    {
        return Err("select --agent, --instance or --pid".into());
    }
    if args.flags.contains_key("--pid") && args.flags.contains_key("--instance") {
        return Err("--pid conflicts with --instance".into());
    }
    let process = args
        .flags
        .get("--pid")
        .map(|pid| -> Result<_> { Ok(aw_service::ProcessIdentity::read(pid.parse()?)?) })
        .transpose()?;
    let config = super::read_file(args.required("--policy")?, aw_config::MAX_DOCUMENT_BYTES)?;
    aw_config::Validator::new()?.parse(&config)?;
    super::run::install_signals()?;
    let deadline = Instant::now() + Duration::from_secs(30);
    let client = super::connect_service(args, deadline)?;
    let status = client.call_cancellable(Operation::Status, deadline, &super::run::CANCELLED)?;
    let instance = select(
        args,
        &status,
        process.as_ref().map(|process| process.metadata()).as_ref(),
    )?;
    if let Some(process) = process {
        let adapter = instance["adapter"]
            .as_str()
            .ok_or("service status missing adapter")?;
        let pid = instance["process"]["pid"]
            .as_u64()
            .ok_or("service status missing PID")?;
        let service_pid = status["pid"].as_u64().ok_or("service status missing PID")?;
        for pid in [pid, service_pid] {
            super::attachment::process::namespaces(
                &PathBuf::from(format!("/proc/{pid}/ns")),
                Path::new("/proc/self/ns"),
                adapter,
            )?;
        }
        process.check()?;
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

fn select<'a>(args: &Arguments, status: &'a Value, process: Option<&Value>) -> Result<&'a Value> {
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
                && process.is_none_or(|identity| &instance["process"] == identity)
        });
    let instance = matches
        .next()
        .ok_or("no matching instance; use aw status")?;
    if matches.next().is_some() {
        return Err(
            "multiple matching instances; specify --instance or a uniquely associated --pid".into(),
        );
    }
    Ok(instance)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn pid_selection_requires_one_complete_live_identity_and_intersects_target() {
        let identity = aw_service::ProcessIdentity::read(std::process::id())
            .unwrap()
            .metadata();
        let args = Arguments::default();
        let original = json!({"instance_id":"selected","target":"shared","process":identity});
        let mut other = original.clone();
        other["instance_id"] = json!("other");
        other["process"]["pid"] = json!(1);
        let mut status = json!({"instances":[original, other]});
        assert_eq!(
            select(&args, &status, Some(&identity)).unwrap()["instance_id"],
            "selected"
        );
        for (field, value) in [
            ("start_ticks", json!("reused")),
            ("uid", json!(u32::MAX)),
            ("pid_namespace", json!("pid:[different]")),
            ("alive", json!(false)),
        ] {
            let mut stale = status.clone();
            stale["instances"][0]["process"][field] = value;
            assert!(select(&args, &stale, Some(&identity)).is_err(), "{field}");
        }
        let mut constrained = Arguments::default();
        constrained.flags.insert("--agent".into(), "wrong".into());
        assert!(select(&constrained, &status, Some(&identity)).is_err());
        status["instances"][1]["process"] = identity.clone();
        assert!(select(&args, &status, Some(&identity))
            .unwrap_err()
            .to_string()
            .contains("multiple matching"));
        status["instances"][0]["process"] = Value::Null;
        status["instances"][1]["process"] = Value::Null;
        assert!(select(&args, &status, Some(&identity)).is_err());
    }
}
