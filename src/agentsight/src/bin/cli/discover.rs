//! Discover subcommand - scan for running AI agents
//!
//! This module provides the `discover` subcommand which scans the system
//! for running AI agent processes.

use agentsight::{AgentScanner, CmdlineGlobMatcher, ProcessContext};
use structopt::StructOpt;

/// Discover subcommand for finding AI agents running on the system
#[derive(Debug, StructOpt, Clone)]
pub struct DiscoverCommand {
    /// Show detailed output including executable path
    #[structopt(short, long)]
    pub verbose: bool,

    /// List all known agents and show currently matched PIDs
    #[structopt(long)]
    pub list_known: bool,

    /// Path to JSON configuration file
    #[structopt(short, long, default_value = "/etc/agentsight/config.json")]
    pub config: String,

    /// Output as JSON
    #[structopt(long)]
    pub json: bool,
}

impl DiscoverCommand {
    pub fn execute(&self) {
        if self.list_known {
            self.list_known_agents();
            return;
        }

        self.scan_agents();
    }

    /// Cmdline rules the tracer would use: the configured file when it can be
    /// parsed, otherwise the rules embedded in the binary.
    ///
    /// Reading the same file as `trace` is what makes this command able to
    /// confirm a custom rule. A successfully parsed file is used as-is — even
    /// an empty rule set — so discovery mirrors what `trace` would actually
    /// match; the built-in fallback only covers a missing or invalid file
    /// (fresh install, non-root user, malformed JSON).
    fn cmdline_rules(&self) -> Vec<agentsight::config::CmdlineRule> {
        let content = match std::fs::read_to_string(&self.config) {
            Ok(content) => content,
            Err(e) => {
                eprintln!(
                    "Hint: cannot read config {} ({e}); falling back to built-in cmdline rules.",
                    self.config
                );
                return agentsight::default_cmdline_rules();
            }
        };

        match agentsight::config::parse_json_rules(&content) {
            Ok((rules, _, _)) => rules,
            Err(e) => {
                eprintln!(
                    "Hint: cannot parse config {} ({e}); falling back to built-in cmdline rules.",
                    self.config
                );
                agentsight::default_cmdline_rules()
            }
        }
    }

    /// List all known agents that can be detected
    fn list_known_agents(&self) {
        let rules = self.cmdline_rules();
        let matchers: Vec<CmdlineGlobMatcher> = rules
            .iter()
            .filter_map(CmdlineGlobMatcher::from_config)
            .collect();
        let mut scanner = AgentScanner::from_rules(&rules, &[]);
        let running_agents = scanner.scan();

        if self.json {
            let infos: Vec<_> = matchers.iter().map(|m| m.info().clone()).collect();
            super::print_json(&infos);
            return;
        }

        println!("已知 AI Agent（共 {} 条规则）:", matchers.len());
        println!("{}", "=".repeat(60));
        println!();

        for matcher in &matchers {
            let agent = matcher.info();
            let matched_pids: Vec<String> = running_agents
                .iter()
                .filter(|running_agent| {
                    let ctx = ProcessContext {
                        comm: String::new(),
                        cmdline_args: running_agent.cmdline_args.clone(),
                        exe_path: running_agent.exe_path.clone(),
                    };
                    matcher.matches(&ctx)
                })
                .map(|running_agent| running_agent.pid.to_string())
                .collect();
            let running_pids = if matched_pids.is_empty() {
                "无".to_string()
            } else {
                matched_pids.join(", ")
            };

            println!("  {} ({})", agent.name, agent.category);
            println!("    命令行规则: {}", matcher.patterns().join(" "));
            println!("    运行中 PID: {running_pids}");
            println!("    {}", agent.description);
            println!();
        }
    }

    /// Scan the system for running AI agents
    fn scan_agents(&self) {
        let rules = self.cmdline_rules();
        let mut scanner = AgentScanner::from_rules(&rules, &[]);
        let agents = scanner.scan();

        if self.json {
            super::print_json(&agents);
            return;
        }

        if agents.is_empty() {
            println!("未发现正在运行的 AI Agent。");
            println!();
            println!("提示：使用 --list-known 查看所有可检测的 Agent。");
            return;
        }

        println!("已发现 AI Agent（共 {} 个）:", agents.len());
        println!("{}", "=".repeat(60));
        println!();

        for agent in &agents {
            println!("  {} [PID: {}]", agent.agent_info.name, agent.pid);
            println!("    类别: {}", agent.agent_info.category);

            let cmdline_str = agent.cmdline_args.join(" ");
            let cmdline = if cmdline_str.len() > 80 && !self.verbose {
                format!("{}...", &cmdline_str[..77])
            } else {
                cmdline_str
            };
            println!("    命令:  {cmdline}");

            if self.verbose && !agent.exe_path.is_empty() {
                println!("    可执行文件: {}", agent.exe_path);
            }

            println!();
        }

        println!("总计: {} 个 Agent", agents.len());
    }
}
