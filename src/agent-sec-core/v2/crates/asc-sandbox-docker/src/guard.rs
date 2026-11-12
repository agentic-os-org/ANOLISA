//! A bounded pipe protocol connects the foreground supervisor to the independent resource owner.
use crate::{DockerBackend, DockerConfig, DockerError, client::Client};
use asc_sandbox_types::{ExecutionResult, ExecutionSpec};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Read as _, Write};
use std::os::unix::process::CommandExt as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const MAX_MESSAGE: u64 = 8 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    config: DockerConfig,
    spec: ExecutionSpec,
    name: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
enum Message {
    Owned { directory: PathBuf },
    Ready { directory: PathBuf, host: String },
    Finished { code: Option<i32> },
    Done { result: ExecutionResult },
    Failed { error: String, unsupported: bool },
}

fn receive<T: serde::de::DeserializeOwned>(reader: &mut impl BufRead) -> Result<T, DockerError> {
    let mut bytes = Vec::new();
    reader.take(MAX_MESSAGE + 1).read_until(b'\n', &mut bytes)?;
    if bytes.len() as u64 > MAX_MESSAGE || bytes.last() != Some(&b'\n') {
        return Err(DockerError::Protocol(
            "guard pipe closed or message exceeds limit".into(),
        ));
    }
    Ok(serde_json::from_slice(&bytes)?)
}

fn send(writer: &mut impl Write, value: &impl Serialize) -> Result<(), DockerError> {
    let mut bytes = serde_json::to_vec(value)?;
    if bytes.len() as u64 >= MAX_MESSAGE {
        return Err(DockerError::Protocol("guard request exceeds limit".into()));
    }
    bytes.push(b'\n');
    writer.write_all(&bytes)?;
    writer.flush()?;
    Ok(())
}

pub(crate) fn run(
    config: &DockerConfig,
    spec: &ExecutionSpec,
) -> Result<ExecutionResult, DockerError> {
    let mut config = config.clone();
    let helper = match &config.guard {
        Some(path) => path.clone(),
        None => std::env::current_exe()?.with_file_name("asc-sandbox-docker-guard"),
    };
    let helper = std::fs::canonicalize(helper).map_err(|error| {
        DockerError::Unsupported(format!(
            "install asc-sandbox-docker-guard alongside the caller: {error}"
        ))
    })?;
    crate::client::trusted_path(&helper, false)?;
    config.guard = Some(helper.clone());
    let name = format!("asc-sandbox-{}", uuid::Uuid::new_v4());
    let request = Request {
        config,
        spec: spec.clone(),
        name,
    };
    let mut child = Command::new(helper)
        .env_clear()
        .process_group(0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;
    let mut input = child
        .stdin
        .take()
        .ok_or_else(|| DockerError::Protocol("guard input missing".into()))?;
    let mut output = BufReader::new(
        child
            .stdout
            .take()
            .ok_or_else(|| DockerError::Protocol("guard output missing".into()))?,
    );
    send(&mut input, &request)?;
    let mut recovery = None;
    loop {
        let message = receive(&mut output).map_err(|error| {
            DockerError::Protocol(format!(
                "cleanup unconfirmed: {} at {}; recovery directory {:?}; {error}",
                request.name, request.config.host, recovery
            ))
        })?;
        match message {
            Message::Owned { directory } => recovery = Some(directory),
            Message::Ready { directory, host } => {
                let status = Command::new(&request.config.cli)
                    .env_clear()
                    .env("PATH", "/usr/bin:/bin")
                    .arg("--config")
                    .arg(directory)
                    .arg("--host")
                    .arg(host)
                    .args(["start", "--attach", "--interactive", &request.name])
                    .stdin(Stdio::inherit())
                    .stdout(Stdio::inherit())
                    .stderr(Stdio::inherit())
                    .status();
                send(
                    &mut input,
                    &Message::Finished {
                        code: status.ok().and_then(|status| status.code()),
                    },
                )?;
            }
            Message::Done { result } => {
                drop(input);
                if !child.wait()?.success() {
                    return Err(DockerError::Protocol(
                        "guard exit did not confirm cleanup".into(),
                    ));
                }
                return Ok(result);
            }
            Message::Failed { error, unsupported } => {
                drop(input);
                let _ = child.wait();
                return Err(if unsupported {
                    DockerError::Unsupported(error)
                } else {
                    DockerError::Protocol(error)
                });
            }
            Message::Finished { .. } => {
                return Err(DockerError::Protocol("unexpected guard message".into()));
            }
        }
    }
}

/// Internal entry point for the independent guard, not for direct application use.
///
/// # Errors
/// Returns configuration or protocol errors, or unconfirmed creation or cleanup.
pub fn guard_main() -> Result<(), DockerError> {
    let mut input = BufReader::new(std::io::stdin());
    let mut output = std::io::stdout();
    let request: Request = receive(&mut input)?;
    let outcome = serve(request, &mut input, &mut output);
    let message = match outcome {
        Ok(result) => Message::Done { result },
        Err(error) => {
            eprintln!("{error}");
            Message::Failed {
                unsupported: matches!(error, DockerError::Unsupported(_)),
                error: error.to_string(),
            }
        }
    };
    send(&mut output, &message)
}

fn serve(
    request: Request,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Result<ExecutionResult, DockerError> {
    let suffix = request
        .name
        .strip_prefix("asc-sandbox-")
        .unwrap_or_default();
    if uuid::Uuid::parse_str(suffix).is_err() {
        return Err(DockerError::Protocol(
            "invalid guard container identity".into(),
        ));
    }
    let backend = DockerBackend::new(request.config)?;
    let client = Client::new(&backend.config)?;
    send(
        output,
        &Message::Owned {
            directory: client.directory.clone(),
        },
    )?;
    backend.run_session(
        &request.spec,
        &client,
        &request.name,
        crate::mounts::PinnedMounts::prepare,
        |client, _| {
            send(
                output,
                &Message::Ready {
                    directory: client.directory.clone(),
                    host: client.host.clone(),
                },
            )?;
            match receive(input)? {
                Message::Finished { code } => Ok(code),
                _ => Err(DockerError::Protocol(
                    "parent lost or invalid finish message".into(),
                )),
            }
        },
    )
}
