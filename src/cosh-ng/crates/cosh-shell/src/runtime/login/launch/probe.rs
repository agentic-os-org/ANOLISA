use std::io;
use std::process::Command;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::process::{read_worker_request, write_worker_reply, BoundedWorker};

pub(crate) const WORKER_FLAG: &str = "--__cosh-login-shell-plan-v1";
const BUDGET: Duration = Duration::from_millis(250);
const MAX_BYTES: usize = 4096;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    #[serde(deserialize_with = "Option::deserialize")]
    bash_path: Option<String>,
}

pub(crate) fn query() -> Result<Option<String>, String> {
    if !cfg!(target_os = "linux") {
        return Ok(None);
    }
    let probe = || -> io::Result<Option<String>> {
        let mut command = Command::new(std::env::current_exe()?);
        command.arg(WORKER_FLAG);
        let mut worker = BoundedWorker::spawn(command, b"{}", Instant::now() + BUDGET, MAX_BYTES)?;
        loop {
            if let Some(result) = worker.poll_ready() {
                return decode_reply(&result?);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    };
    probe().map_err(|_| "unable to verify the default login Bash within the startup limit".into())
}

fn decode_reply(bytes: &[u8]) -> io::Result<Option<String>> {
    let reply: Reply = serde_json::from_slice(bytes)?;
    if reply
        .bash_path
        .as_deref()
        .is_some_and(|path| !super::account::supported_bash(path))
    {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(reply.bash_path)
}

pub(crate) fn run_worker() -> i32 {
    let Ok(bytes) = read_worker_request(MAX_BYTES, BUDGET) else {
        return 1;
    };
    if serde_json::from_slice::<Request>(&bytes).is_err() {
        return 1;
    }
    let Ok(plan) = super::login_shell_plan(true) else {
        return 1;
    };
    let Ok(reply) = serde_json::to_vec(&Reply {
        bash_path: plan.map(|plan| plan.bash_path),
    }) else {
        return 1;
    };
    i32::from(write_worker_reply(&reply, MAX_BYTES).is_err())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_exec_probe_accepts_only_explicit_fixed_bash_or_non_cosh() {
        for path in ["/bin/bash", "/usr/bin/bash"] {
            let bytes = serde_json::to_vec(&Reply {
                bash_path: Some(path.into()),
            })
            .unwrap();
            assert_eq!(decode_reply(&bytes).unwrap().as_deref(), Some(path));
        }
        assert_eq!(decode_reply(br#"{"bash_path":null}"#).unwrap(), None);
        for bytes in [
            br#"{"bash_path":"bash"}"#.as_slice(),
            br#"{"bash_path":"/home/alice/bash"}"#,
            br#"{"bash_path":null,"extra":true}"#,
            b"{}",
            b"",
        ] {
            assert!(decode_reply(bytes).is_err());
        }
    }
}
