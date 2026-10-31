//! An explicit upper-PEP consumer; the product sandbox CLI remains daemon-independent.

use std::collections::BTreeMap;
use std::io::{self, Write as _};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use asc_pep_runtime::{
    ApprovalHandler, AuthorizationClient, DaemonAuthorizationClient, FixedPermissions, PepError,
    PepRuntime, SandboxBackend, prepare_launch,
};
use asc_policy_types::{
    AgentPrincipal, AuthorizationRequest, Decision, DecisionVerdict, ExecutionReceipt,
    OsPermissionSet, ReceiptAck,
};
use clap::Parser;

#[derive(Parser)]
#[command(about = "Run one explicitly authorized sandbox invocation through the daemon PDP")]
struct Options {
    /// Installed strict linux-sandbox executable.
    #[arg(long)]
    helper: PathBuf,
    /// Root-owned daemon socket.
    #[arg(long)]
    socket: PathBuf,
    /// JSON OS permission request, including runtime dependencies.
    #[arg(long)]
    permissions: PathBuf,
    /// Logical Agent identity supplied by the embedding application.
    #[arg(long)]
    agent_id: String,
    /// Agent family, such as codex or qoder.
    #[arg(long)]
    agent_type: String,
    /// Enable a local approval prompt when the PDP returns `STEP_UP`.
    #[arg(long)]
    ask: bool,
    /// Program and arguments after --.
    #[arg(last = true, required = true, num_args = 1..)]
    command: Vec<String>,
}

struct LocalClient(DaemonAuthorizationClient);

impl AuthorizationClient for LocalClient {
    fn evaluate(&self, request: &AuthorizationRequest) -> Result<Decision, PepError> {
        let decision = self.0.evaluate(request)?;
        // Continuations preserve warnings; show them once the final invocation is known.
        if decision.verdict != DecisionVerdict::Defer {
            for warning in &decision.warnings {
                eprintln!("POLICY_WARNING {warning}");
            }
        }
        Ok(decision)
    }

    fn report(&self, receipt: &ExecutionReceipt) -> Result<ReceiptAck, PepError> {
        self.0.report(receipt)
    }
}

struct LocalApproval;
impl ApprovalHandler for LocalApproval {
    fn ask(&mut self, decision: &Decision) -> Result<bool, PepError> {
        eprintln!(
            "APPROVAL_REQUIRED {} {:?}; permissions={}",
            decision.launch.program,
            decision.launch.args,
            serde_json::to_string(&decision.effective_permissions)?
        );
        eprint!("Allow this invocation? [y/N] ");
        io::stderr().flush()?;
        let mut answer = String::new();
        io::stdin().read_line(&mut answer)?;
        Ok(matches!(
            answer.trim().to_ascii_lowercase().as_str(),
            "y" | "yes"
        ))
    }
}

fn run(options: Options) -> Result<u8, Box<dyn std::error::Error>> {
    let permissions: OsPermissionSet =
        serde_json::from_reader(std::fs::File::open(options.permissions)?)?;
    let (program, args) = options
        .command
        .split_first()
        .ok_or("missing target command")?;
    let environment = std::env::vars_os()
        .map(|(key, value)| {
            Ok((
                key.into_string()
                    .map_err(|_| "non-UTF-8 environment name")?,
                value
                    .into_string()
                    .map_err(|_| "non-UTF-8 environment value")?,
            ))
        })
        .collect::<Result<BTreeMap<_, _>, &str>>()?;
    let launch = prepare_launch(
        program,
        args.to_vec(),
        &std::env::current_dir()?,
        environment,
    )?;
    let mut runtime = PepRuntime::new(SandboxBackend::new(options.helper));
    let client = LocalClient(DaemonAuthorizationClient {
        socket: options.socket,
        timeout: Duration::from_secs(10),
    });
    let mut approval = LocalApproval;
    let channel = options
        .ask
        .then_some(&mut approval as &mut dyn ApprovalHandler);
    let outcome = runtime.run(
        &AgentPrincipal {
            agent_id: options.agent_id,
            agent_type: options.agent_type,
        },
        launch,
        &FixedPermissions(permissions),
        &client,
        channel,
    )?;
    // Do not print the target environment or duplicate sensitive command arguments in the receipt summary.
    println!(
        "{}",
        serde_json::json!({
            "decision_id": outcome.decision.decision_id,
            "policy_version": outcome.decision.policy_version,
            "verdict": outcome.decision.verdict,
            "warnings": outcome.decision.warnings,
            "result": outcome.result,
            "receipt_error": outcome.receipt_error,
        })
    );
    if outcome.receipt_error.is_some() || outcome.result.error.is_some() {
        return Ok(1);
    }
    if let Some(signal) = outcome.result.cancel_signal.or(outcome.result.signal) {
        return Ok(u8::try_from(128_i32.saturating_add(signal)).unwrap_or(255));
    }
    Ok(outcome
        .result
        .exit_code
        .and_then(|value| u8::try_from(value).ok())
        .unwrap_or(1))
}

fn main() -> ExitCode {
    match run(Options::parse()) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("authorized_sandbox: {error}");
            ExitCode::FAILURE
        }
    }
}
