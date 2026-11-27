//! Explicit policy changes address an existing instance, never the candidate's daemon path.

use super::{Arguments, Exit, Result};
use aw_service::{Operation, ProcessIdentity};
use serde_json::Value;
use std::{
    fs,
    time::{Duration, Instant},
};

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
    let pid = args.flags.get("--pid");
    if pid.is_some() && args.flags.contains_key("--instance") {
        return Err("choose --pid or --instance".into());
    }
    if pid.is_none()
        && !args.flags.contains_key("--agent")
        && !args.flags.contains_key("--instance")
    {
        return Err("select --agent, --instance, or --pid".into());
    }
    let process = pid.map(|pid| read_process(pid)).transpose()?;
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
    let instance = select(args, &status, process.as_ref())?;
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

fn read_process(pid: &str) -> Result<Value> {
    let pid: u32 = pid.parse().map_err(|_| "--pid requires a positive u32")?;
    if pid == 0 {
        return Err("--pid requires a positive u32".into());
    }
    let process = ProcessIdentity::read(pid)
        .map_err(|error| format!("cannot read live process identity for PID {pid}: {error}"))?
        .metadata();
    if process["alive"] != true {
        return Err(format!("PID {pid} exited or its process identity changed").into());
    }
    let namespace = fs::read_link("/proc/self/ns/pid")?;
    if process["pid_namespace"].as_str() != namespace.to_str() {
        return Err(format!("PID {pid} is outside the caller's PID namespace").into());
    }
    Ok(process)
}

fn select<'a>(args: &Arguments, status: &'a Value, process: Option<&Value>) -> Result<&'a Value> {
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
                && process.is_none_or(|process| {
                    let associated = &instance["process"];
                    associated["alive"] == true
                        && ["pid", "start_ticks", "uid", "pid_namespace"]
                            .iter()
                            .all(|field| associated[*field] == process[*field])
                })
        });
    let Some(instance) = matches.next() else {
        return Err(match process {
            Some(process) => format!(
                "no matching live root instance for PID {}; check the process identity and target in aw service status",
                process["pid"]
            )
            .into(),
            None => "no matching instance; use aw service status --config FILE".into(),
        });
    };
    if matches.next().is_some() {
        return Err(match process {
            Some(process) => format!(
                "multiple matching root instances for PID {}; select one with --instance",
                process["pid"]
            )
            .into(),
            None => "multiple matching instances; specify --instance".into(),
        });
    }
    Ok(instance)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn runtime_rejects_missing_and_conflicting_selectors() {
        let mut args = Arguments::default();
        assert!(
            matches!(update(&args), Err(error) if error.to_string().contains("select --agent, --instance, or --pid"))
        );
        args.flags.insert("--pid".into(), "123".into());
        args.flags.insert("--instance".into(), "root".into());
        assert!(
            matches!(update(&args), Err(error) if error.to_string().contains("choose --pid or --instance"))
        );
        args.flags.remove("--instance");
        args.flags.insert("--pid".into(), "0".into());
        assert!(
            matches!(update(&args), Err(error) if error.to_string().contains("--pid requires a positive u32"))
        );
    }

    #[test]
    fn target_selection_ignores_children_and_requires_one_root() {
        let mut args = Arguments::default();
        args.flags.insert("--agent".into(), "shared".into());
        let root = json!({"instance_id":"root","target":"shared"});
        let child = json!({"instance_id":"child","target":"shared","parent_instance_id":"root"});
        let mut status = json!({"instances":[root,child]});
        assert_eq!(select(&args, &status, None).unwrap()["instance_id"], "root");
        status["instances"]
            .as_array_mut()
            .unwrap()
            .push(json!({"instance_id":"other","target":"shared"}));
        assert!(select(&args, &status, None)
            .unwrap_err()
            .to_string()
            .contains("multiple matching"));
        args.flags.insert("--instance".into(), "root".into());
        assert_eq!(select(&args, &status, None).unwrap()["instance_id"], "root");
        args.flags.insert("--agent".into(), "wrong".into());
        assert!(select(&args, &status, None).is_err());
        args.flags.remove("--agent");
        args.flags.insert("--instance".into(), "missing".into());
        assert!(select(&args, &status, None).is_err());
    }

    #[test]
    fn pid_selection_requires_one_live_root_with_the_same_process_identity() {
        let mut args = Arguments::default();
        args.flags.insert("--agent".into(), "shared".into());
        let process = json!({
            "pid": 123, "start_ticks": "456", "uid": 1000,
            "pid_namespace": "pid:[1234]", "alive": true
        });
        let mut selected =
            json!({"target": "shared", "instance_id": "selected", "process": process});
        let mut rejected = vec![json!({"target": "shared", "instance_id": "unassociated"})];
        for (field, value) in [
            ("pid", json!(124)),
            ("start_ticks", json!("789")),
            ("uid", json!(1001)),
            ("pid_namespace", json!("pid:[9876]")),
            ("alive", json!(false)),
        ] {
            let mut instance = selected.clone();
            instance["process"][field] = value;
            rejected.push(instance);
        }
        let mut child = selected.clone();
        child["parent_instance_id"] = json!("other");
        rejected.push(child);
        let mut other_target = selected.clone();
        other_target["target"] = json!("other");
        rejected.push(other_target);
        for instance in &rejected {
            let status = json!({"instances": [instance]});
            assert!(
                select(&args, &status, Some(&process))
                    .unwrap_err()
                    .to_string()
                    .contains("no matching live root instance for PID 123"),
                "{instance}"
            );
        }

        rejected.push(selected.clone());
        let mut status = json!({"instances": rejected});
        assert_eq!(select(&args, &status, Some(&process)).unwrap(), &selected);
        selected["instance_id"] = json!("another-root");
        status["instances"].as_array_mut().unwrap().push(selected);
        assert!(select(&args, &status, Some(&process))
            .unwrap_err()
            .to_string()
            .contains("multiple matching root instances for PID 123"));
    }
}
