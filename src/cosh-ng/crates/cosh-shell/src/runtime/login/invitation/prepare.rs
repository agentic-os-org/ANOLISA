use std::io;
use std::process::Command;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::process::{read_worker_request, write_worker_reply, BoundedWorker};

pub(crate) const WORKER_FLAG: &str = "--__cosh-login-invitation-worker-v1";
const BUDGET: Duration = Duration::from_millis(250);
const MAX_BYTES: usize = 4096;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Request {
    language: String,
    session: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Receipt {
    pub(super) generation: String,
}

pub(super) struct Preparation(BoundedWorker);

impl Preparation {
    pub(super) fn start(language: crate::Language, session: &str) -> io::Result<Self> {
        if session.is_empty() || session.len() > 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid invitation session",
            ));
        }
        let request = serde_json::to_vec(&Request {
            language: language.as_config_value().into(),
            session: session.into(),
        })?;
        if request.len() > MAX_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invitation request exceeds limit",
            ));
        }
        let mut command = Command::new(std::env::current_exe()?);
        command.arg(WORKER_FLAG);
        BoundedWorker::spawn(command, &request, Instant::now() + BUDGET, MAX_BYTES).map(Self)
    }

    pub(super) fn poll(&mut self) -> Option<Option<Receipt>> {
        self.0.poll_ready().map(|result| {
            result
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Receipt>(&bytes).ok())
                .filter(|receipt| {
                    receipt.generation.len() == 64
                        && receipt
                            .generation
                            .bytes()
                            .all(|byte| byte.is_ascii_hexdigit())
                })
        })
    }

    #[cfg(test)]
    pub(super) fn fixture(command: Command) -> Self {
        Self(BoundedWorker::spawn(command, b"", Instant::now() + BUDGET, MAX_BYTES).unwrap())
    }
}

pub(crate) fn run_worker() -> i32 {
    let Ok(bytes) = read_worker_request(MAX_BYTES, BUDGET) else {
        return 1;
    };
    let Ok(request) = serde_json::from_slice::<Request>(&bytes) else {
        return 1;
    };
    if request.session.is_empty() || request.session.len() > 1024 {
        return 1;
    }
    let language = match request.language.as_str() {
        "en-US" => crate::Language::EnUs,
        "zh-CN" => crate::Language::ZhCn,
        _ => return 1,
    };
    let Some(prepared) = super::prepare_offer(language, &request.session) else {
        return 1;
    };
    let Ok(output) = serde_json::to_vec(&Receipt {
        generation: prepared.generation,
    }) else {
        return 1;
    };
    i32::from(write_worker_reply(&output, MAX_BYTES).is_err())
}
