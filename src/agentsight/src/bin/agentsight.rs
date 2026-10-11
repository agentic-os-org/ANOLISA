//! AgentSight CLI - AI Agent observability tool
//!
//! Linux: full eBPF observability (trace, discover, token, audit, etc.)
//! macOS: trajectory collection (trace) + local viewer (serve)
use agentsight::token_breakdown::AnalyzeChatmlCommand;
use structopt::StructOpt;

mod cli;
#[cfg(all(feature = "server", target_os = "linux"))]
use cli::dashboard::DashboardCommand;
#[cfg(feature = "server")]
use cli::serve::ServeCommand;
#[cfg(any(target_os = "linux", feature = "server"))]
use cli::trace::TraceCommand;
#[cfg(target_os = "linux")]
use cli::{
    audit::AuditCommand, discover::DiscoverCommand, interruption::InterruptionCommand,
    metrics::MetricsCommand, skill_metrics::SkillMetricsCommand, summary::SummaryCommand,
    token::TokenCommand,
};

#[derive(Debug, StructOpt)]
#[cfg_attr(
    target_os = "linux",
    structopt(
        name = "agentsight",
        about = "AI Agent observability tool - trace processes, SSL traffic, and LLM API calls via eBPF"
    )
)]
#[cfg_attr(
    not(target_os = "linux"),
    structopt(
        name = "agentsight",
        about = "AI agent trajectory collector and viewer - trace local sessions and serve dashboard"
    )
)]
pub enum Command {
    /// Query token consumption data
    #[cfg(target_os = "linux")]
    Token(TokenCommand),
    /// Trace agent activity (default)
    #[cfg(any(target_os = "linux", feature = "server"))]
    Trace(TraceCommand),
    /// Query audit events
    #[cfg(target_os = "linux")]
    Audit(AuditCommand),
    /// Discover running AI agents on the system
    #[cfg(target_os = "linux")]
    Discover(DiscoverCommand),
    /// Print per-agent token usage metrics in Prometheus text format
    #[cfg(target_os = "linux")]
    Metrics(MetricsCommand),
    /// Query and manage session interruption events detected during agent conversations
    #[cfg(target_os = "linux")]
    Interruption(InterruptionCommand),
    /// Compute and display skill usage metrics
    #[cfg(target_os = "linux")]
    #[structopt(name = "skill-metrics")]
    SkillMetrics(SkillMetricsCommand),
    /// Print a unified summary of sessions, interruptions, and tokenless savings
    #[cfg(target_os = "linux")]
    Summary(SummaryCommand),
    /// Analyze ChatML token consumption from a Chrome Trace file
    #[structopt(name = "analyze-chatml")]
    AnalyzeChatml(AnalyzeChatmlCommand),
    /// Start the API server
    #[cfg(feature = "server")]
    Serve(ServeCommand),
    /// Display dashboard URL and ECS console access guide
    #[cfg(all(feature = "server", target_os = "linux"))]
    Dashboard(DashboardCommand),
}

fn main() {
    let cmd = Command::from_args();

    match cmd {
        #[cfg(target_os = "linux")]
        Command::Token(token_cmd) => token_cmd.execute(),
        #[cfg(any(target_os = "linux", feature = "server"))]
        Command::Trace(trace_cmd) => trace_cmd.execute(),
        #[cfg(target_os = "linux")]
        Command::Audit(audit_cmd) => audit_cmd.execute(),
        #[cfg(target_os = "linux")]
        Command::Discover(discover_cmd) => discover_cmd.execute(),
        #[cfg(target_os = "linux")]
        Command::Metrics(metrics_cmd) => metrics_cmd.execute(),
        #[cfg(target_os = "linux")]
        Command::Interruption(interruption_cmd) => interruption_cmd.execute(),
        #[cfg(target_os = "linux")]
        Command::SkillMetrics(skill_metrics_cmd) => skill_metrics_cmd.execute(),
        #[cfg(target_os = "linux")]
        Command::Summary(summary_cmd) => summary_cmd.execute(),
        Command::AnalyzeChatml(analyze_chatml_cmd) => analyze_chatml_cmd.execute(),
        #[cfg(feature = "server")]
        Command::Serve(serve_cmd) => serve_cmd.execute(),
        #[cfg(all(feature = "server", target_os = "linux"))]
        Command::Dashboard(dashboard_cmd) => dashboard_cmd.execute(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The analyze-chatml subcommand must stay registered in the Command
    /// tree — the token_breakdown module once sat unwired behind no `mod`
    /// declaration, and its CLI was unreachable despite being documented.
    #[test]
    fn analyze_chatml_subcommand_is_reachable() {
        let cmd = Command::from_iter_safe([
            "agentsight",
            "analyze-chatml",
            "--chrome-trace",
            "/nonexistent/trace.json",
        ])
        .expect("analyze-chatml must parse as a registered subcommand");
        assert!(matches!(cmd, Command::AnalyzeChatml(_)));
    }
}
