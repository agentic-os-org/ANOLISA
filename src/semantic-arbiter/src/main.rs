//! CLI judgments remain advisory even when an explicitly configured model is invoked.

use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read, Write};
use std::process::ExitCode;
use std::sync::mpsc;

use rustix::event::{poll, PollFd, PollFlags, Timespec};
use rustix::fs::{fcntl_getfl, fcntl_setfl, OFlags};
use rustix::io::Errno;
use semantic_arbiter::check::check;
use semantic_arbiter::config::ModelConfig;
use semantic_arbiter::deadline::Ingress;
use semantic_arbiter::evaluation::{EvaluationRequest, Evaluator, EVALUATION_API_VERSION};
use semantic_arbiter::model::http::HttpBackend;
use semantic_arbiter::outcome::CandidateControl;
use semantic_arbiter::protocol::{
    capabilities, decode_request, CHECK_API_VERSION, MAX_MESSAGE_BYTES,
};
use serde::Serialize;
use tokio::time::Instant;

const HELP: &str = "\
semantic-arbiter - Evaluate task intent and protect existing results

Returns advisory judgments. Consumers authorize and execute all actions,
including snapshots and audit writes.

Usage:
  semantic-arbiter <COMMAND>
  semantic-arbiter help [COMMAND]

Commands:
  describe  Print supported capabilities as JSON
  check     Check frozen evidence offline with its recorded clock
  evaluate  Evaluate task intent using a configured Bailian or TypeSafe model
  help      Show this help or help for a command

Options:
  -h, --help  Show help (also available after each command)

Scenarios:
  preservation  Check retention requirements and local rebuild evidence;
                recommend a snapshot for unique results when modification is allowed
  task_scope    Check whether the operation fits the task and allowed resources
  Deterministic blockers block; model ambiguity or failure contributes nop.

Examples (from the component directory):
  semantic-arbiter describe
  semantic-arbiter check examples/preservation-allow.json
  semantic-arbiter check - < examples/snapshot.json
  semantic-arbiter evaluate --config config.json request.json

Input and output:
  Requests are UTF-8 JSON, at most 262144 bytes (256 KiB); '-' reads stdin.
  Business commands write JSON to stdout; failures write sanitized JSON to stderr.
  Model evaluation sends selected input to the configured backend; authorize it first.

Exit status:
  0  Help, capabilities, or a complete judgment (including block and unknown)
  2  Invalid arguments, input/configuration errors, or I/O failure
  Exit 0 is not permission to execute; inspect outcome.control.candidate.
";

const DESCRIBE_HELP: &str = "\
semantic-arbiter describe - Print supported capabilities as JSON

Usage:
  semantic-arbiter describe

Reports protocol versions, operations, modes, model backends, scenarios,
candidate controls, obligation kinds, and the message size limit.
Reads no request or credentials and makes no model calls.

Example:
  semantic-arbiter describe | jq
";

const CHECK_HELP: &str = "\
semantic-arbiter check - Check frozen evidence offline

Usage:
  semantic-arbiter check <request.json|->

Arguments:
  <request.json|->  Request file, or '-' to read stdin (at most 256 KiB)

Requires semantic-arbiter/v1alpha1 with mode deterministic_check.
Uses check_at_ms and structured constraints; it does not interpret task text
with a model or collect new evidence. Enable preservation, task_scope, or both
in the request's scenarios array.

Examples (from the component directory):
  semantic-arbiter check examples/preservation-allow.json
  semantic-arbiter check - < examples/snapshot.json

Output:
  JSON judgment containing answers and outcome. Results are advisory;
  snapshot/audit obligations are not executed. Block and unknown judgments exit 0.
  Invalid input or I/O failure exits 2 with sanitized JSON on stderr.
";

const EVALUATE_HELP: &str = "\
semantic-arbiter evaluate - Evaluate task intent with a configured model

Usage:
  semantic-arbiter evaluate --config <config.json> <request.json|->

Options and arguments:
  --config <config.json>  Required configuration file; stdin is not accepted
  <request.json|->        Request file, or '-' to read stdin (at most 256 KiB)

Requires semantic-arbiter/v1alpha2 with mode model_evaluation and Unix-millisecond
deadlines. Deterministic constraints run before eligible model questions.
Backends: bailian, typesafe. Credentials come from the configured backend.key_env;
never put key values in command arguments or JSON. Authorize the endpoint and
outbound task/evidence data before running this command.

Thresholds:
  thresholds.preservation_ppm and thresholds.task_scope_ppm default to 900000.
  The Bailian example sets both to 800000. Values are model support, not accuracy.

Examples (from the component directory, after configuring endpoint and credentials):
  semantic-arbiter evaluate --config config.json request.json
  python3 examples/evaluation/request.py preservation-allow | semantic-arbiter evaluate --config examples/evaluation/bailian.json -

Output:
  JSON judgment with answers, outcome, model observations and configuration digest.
  Results are advisory; consumers execute required protections and revalidate first.
  Complete judgments exit 0, including unknown/nop from model authentication or
  response errors. Inspect judgments[].error and outcome.control.candidate.
  Invalid input/configuration, missing credentials or I/O failure exits 2.
";

#[derive(Serialize)]
struct ErrorEnvelope<'a> {
    api_version: &'a str,
    error: &'a str,
}

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let api_version = if args.first().is_some_and(|arg| arg == "evaluate") {
        EVALUATION_API_VERSION
    } else {
        CHECK_API_VERSION
    };
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(code) => {
            let envelope = ErrorEnvelope {
                api_version,
                error: &code,
            };
            let mut stderr = io::stderr().lock();
            let _ = serde_json::to_writer(&mut stderr, &envelope);
            let _ = writeln!(stderr);
            ExitCode::from(2)
        }
    }
}

fn run(args: &[std::ffi::OsString]) -> Result<(), String> {
    match args {
        [] => print_help(OsStr::new("help")),
        [command] if command == "--help" || command == "-h" || command == "help" => {
            print_help(OsStr::new("help"))
        }
        [command, flag] if flag == "--help" || flag == "-h" => print_help(command),
        [help, command] if help == "help" => print_help(command),
        [command] if command == "describe" => write_json(&capabilities()),
        [command, path] if command == "check" => {
            let bytes = read_bounded(path)?;
            let request = decode_request(&bytes).map_err(|error| error.to_string())?;
            let response = check(&request).map_err(|error| error.to_string())?;
            write_json(&response)
        }
        [command, flag, config, path]
            if command == "evaluate" && flag == "--config" && config != "-" =>
        {
            evaluate(config, path)
        }
        _ => Err("invalid_arguments".into()),
    }
}

fn print_help(command: &OsStr) -> Result<(), String> {
    let text = match command.to_str() {
        Some("help") => HELP,
        Some("describe") => DESCRIBE_HELP,
        Some("check") => CHECK_HELP,
        Some("evaluate") => EVALUATE_HELP,
        _ => return Err("invalid_arguments".into()),
    };
    io::stdout()
        .lock()
        .write_all(text.as_bytes())
        .map_err(|_| "output_error".into())
}

fn evaluate(config_path: &OsStr, path: &OsStr) -> Result<(), String> {
    let ingress = Ingress::capture().map_err(|error| error.to_string())?;
    let bytes = read_before(config_path, ingress.input_deadline())?;
    let config = ModelConfig::decode(&bytes).map_err(|error| error.to_string())?;
    let bytes = read_before(path, ingress.preparation_deadline(config.timeout_ms))?;
    let request = EvaluationRequest::decode(&bytes).map_err(|error| error.to_string())?;
    let backend = HttpBackend::from_env(&config).map_err(|error| error.to_string())?;
    let evaluator = Evaluator::new(config.clone(), backend).map_err(|error| error.to_string())?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| "runtime_error")?;
    let run = runtime
        .block_on(evaluator.evaluate_from_ingress(&request, ingress))
        .map_err(|error| error.to_string())?;
    let mut bytes = serde_json::to_vec(&run.response).map_err(|_| "output_error")?;
    bytes.push(b'\n');
    if matches!(
        run.response.outcome.control.candidate,
        CandidateControl::Nop | CandidateControl::Allow
    ) {
        let expires_at_ms = request
            .evidence
            .iter()
            .filter(|evidence| run.response.outcome.evidence_refs.contains(&evidence.id))
            .map(|evidence| evidence.valid_until_ms)
            .fold(request.budget.deadline_ms, u64::min);
        let deadline = if run.response.outcome.control.candidate == CandidateControl::Nop {
            ingress.event_deadline_at(expires_at_ms)
        } else {
            ingress.deadline_at(expires_at_ms, config.timeout_ms)
        };
        return write_before(&bytes, deadline);
    }
    io::stdout()
        .lock()
        .write_all(&bytes)
        .map_err(|_| "output_error".into())
}

fn write_before(mut bytes: &[u8], deadline: Instant) -> Result<(), String> {
    let stdout = io::stdout();
    let flags = fcntl_getfl(&stdout).map_err(|_| "output_error")?;
    fcntl_setfl(&stdout, flags | OFlags::NONBLOCK).map_err(|_| "output_error")?;
    let result = (|| {
        while !bytes.is_empty() {
            if Instant::now() >= deadline {
                return Err("deadline_exceeded".into());
            }
            match rustix::io::write(&stdout, bytes) {
                Ok(0) => return Err("output_error".into()),
                Ok(written) => bytes = &bytes[written..],
                Err(Errno::INTR) => continue,
                Err(Errno::AGAIN) => {
                    let timeout =
                        Timespec::try_from(deadline.saturating_duration_since(Instant::now()))
                            .map_err(|_| "output_error")?;
                    match poll(&mut [PollFd::new(&stdout, PollFlags::OUT)], Some(&timeout)) {
                        Ok(_) | Err(Errno::INTR) => {}
                        Err(_) => return Err("output_error".into()),
                    }
                }
                Err(_) => return Err("output_error".into()),
            }
        }
        if Instant::now() >= deadline {
            return Err("deadline_exceeded".into());
        }
        Ok(())
    })();
    fcntl_setfl(&stdout, flags).map_err(|_| "output_error")?;
    result
}

fn read_before(path: &OsStr, deadline: Instant) -> Result<Vec<u8>, String> {
    let path = path.to_os_string();
    let (sender, receiver) = mpsc::sync_channel(1);
    // A stalled pipe must not hold Tokio's blocking-pool shutdown past the deadline.
    std::thread::spawn(move || {
        let _ = sender.send(read_bounded(&path));
    });
    receiver
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .map_err(|_| "deadline_exceeded".to_string())?
}

fn read_bounded(path: &OsStr) -> Result<Vec<u8>, String> {
    let input: Box<dyn Read> = if path == "-" {
        Box::new(io::stdin())
    } else {
        Box::new(File::open(path).map_err(|_| "input_error")?)
    };
    let mut bytes = Vec::new();
    input
        .take((MAX_MESSAGE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "input_error")?;
    Ok(bytes)
}

fn write_json(value: &impl Serialize) -> Result<(), String> {
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(&mut stdout, value).map_err(|_| "output_error")?;
    writeln!(stdout).map_err(|_| "output_error".into())
}
