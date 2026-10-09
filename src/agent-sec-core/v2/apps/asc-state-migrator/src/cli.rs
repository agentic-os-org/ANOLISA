//! Command-line surface of `asc-state-migrator`.
//!
//! The migrator is deliberately explicit: the daemon never runs it implicitly,
//! and every write command (`apply`, `rollback`) names the destination and the
//! evidence it produced. `plan` and `verify` are read-only.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

/// Top-level arguments shared by every subcommand.
#[derive(Debug, Parser)]
#[command(
    name = "asc-state-migrator",
    about = "Migrate v1 per-user agent-sec-core state into the v2 system store",
    version
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,

    #[command(flatten)]
    pub common: CommonArgs,
}

/// Options shared by `plan` and `apply`.
///
/// A CLI flag group legitimately carries more than three booleans: they are
/// the command's contract, not a domain model.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Args)]
pub struct CommonArgs {
    /// Destination v2 system `security-events.db`.
    ///
    /// Defaults to the daemon's own resolution (`AGENT_SEC_DATA_DIR` override,
    /// else `/var/log/agent-sec`), so the migrator and the daemon agree on one
    /// store without configuration.
    #[arg(long, global = true)]
    pub destination: Option<PathBuf>,

    /// Explicit source directory; repeatable. Its verified owner is the
    /// directory's `uid` unless `--map-owner` overrides it.
    #[arg(long = "source", value_name = "DIR", global = true)]
    pub sources: Vec<PathBuf>,

    /// Admin owner mapping `DIR=UID`, repeatable. The given `uid` becomes the
    /// imported rows' owner instead of the directory's own `uid`.
    #[arg(long = "map-owner", value_name = "DIR=UID", global = true)]
    pub map_owner: Vec<String>,

    /// Root whose `*/.agent-sec-core` subdirectories are discovered as sources.
    #[arg(long, value_name = "ROOT", default_value = "/home", global = true)]
    pub discover_homes: Option<PathBuf>,

    /// Disable home-directory discovery.
    #[arg(long, global = true)]
    pub no_discover_homes: bool,

    /// Root whose `agent-sec-<uid>` subdirectories are discovered as sources.
    #[arg(long, value_name = "ROOT", default_value = "/tmp", global = true)]
    pub discover_tmp: Option<PathBuf>,

    /// Disable tmp-directory discovery.
    #[arg(long, global = true)]
    pub no_discover_tmp: bool,

    /// Drop source records older than this many days at import time, matching
    /// the stream's retention window. Defaults to the v1 default of 30.
    #[arg(long, value_name = "DAYS", default_value_t = 30, global = true)]
    pub retention_days: u32,

    /// Drop source observability records older than this many days at import
    /// time, matching the observability stream's own retention window.
    /// Defaults to the v1 default of 7 (#6605 phase 5).
    #[arg(long, value_name = "DAYS", default_value_t = 7, global = true)]
    pub observability_retention_days: u32,

    /// Import records of any age.
    #[arg(long, global = true)]
    pub no_retention_cutoff: bool,

    /// Only read each source's `SQLite` stream; skip `JSONL` gap recovery.
    #[arg(long, global = true)]
    pub sqlite_only: bool,

    /// Recover observability rows the source `SQLite` stream is missing from
    /// its `JSONL` stream. Off by default: the observability stream has no
    /// stable id, so recovery matches rows by content and stays an explicit
    /// operator decision (#6605 phase 5).
    #[arg(long, global = true)]
    pub recover_observability_jsonl: bool,

    /// Proceed even when a source was modified recently.
    #[arg(long, global = true)]
    pub force: bool,

    /// Sources modified within this many seconds are considered live.
    #[arg(long, value_name = "SECONDS", default_value_t = 300, global = true)]
    pub writer_grace: u32,

    /// Emit machine-readable JSON instead of the human summary.
    #[arg(long, global = true)]
    pub json: bool,
}

/// The four migrator verbs.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Discover, validate and count sources without writing anything.
    Plan,
    /// Import v1 security events into the v2 system store and journal it.
    Apply,
    /// Re-check journaled runs against the destination.
    Verify {
        /// Verify one run id; defaults to the most recent run.
        #[arg(long = "run-id", value_name = "ID")]
        run_id: Option<String>,
    },
    /// Remove the rows a journaled run imported. Sources are never touched.
    Rollback {
        /// Roll back one run id.
        #[arg(long = "run-id", value_name = "ID", conflicts_with = "all")]
        run_id: Option<String>,
        /// Roll back every run that has not been rolled back yet.
        #[arg(long)]
        all: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn defaults_match_the_v1_contract() {
        let cli = Cli::try_parse_from(["asc-state-migrator", "plan"]).expect("parses");
        assert!(cli.common.sources.is_empty());
        assert_eq!(
            cli.common.discover_homes.as_deref(),
            Some(std::path::Path::new("/home"))
        );
        assert_eq!(
            cli.common.discover_tmp.as_deref(),
            Some(std::path::Path::new("/tmp"))
        );
        assert_eq!(cli.common.retention_days, 30);
        assert_eq!(
            cli.common.observability_retention_days, 7,
            "the observability stream keeps a 7-day window in v1"
        );
        assert!(!cli.common.no_retention_cutoff);
        assert!(!cli.common.sqlite_only);
        assert!(
            !cli.common.recover_observability_jsonl,
            "observability JSONL recovery is an explicit decision"
        );
        assert_eq!(cli.common.writer_grace, 300);
    }

    #[test]
    fn explicit_sources_and_owner_maps_parse() {
        let cli = Cli::try_parse_from([
            "asc-state-migrator",
            "apply",
            "--source",
            "/srv/old",
            "--map-owner",
            "/srv/old=1001",
            "--retention-days",
            "7",
            "--observability-retention-days",
            "30",
            "--recover-observability-jsonl",
            "--json",
        ])
        .expect("parses");
        assert_eq!(cli.common.sources, [PathBuf::from("/srv/old")]);
        assert_eq!(cli.common.map_owner, ["/srv/old=1001".to_owned()]);
        assert_eq!(cli.common.retention_days, 7);
        assert_eq!(cli.common.observability_retention_days, 30);
        assert!(cli.common.recover_observability_jsonl);
    }

    #[test]
    fn rollback_requires_exactly_one_selector() {
        assert!(Cli::try_parse_from(["asc-state-migrator", "rollback"]).is_ok());
        let both =
            Cli::try_parse_from(["asc-state-migrator", "rollback", "--run-id", "abc", "--all"]);
        assert!(both.is_err(), "--run-id and --all conflict");
    }
}
