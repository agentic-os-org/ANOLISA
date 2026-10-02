//! Agent matching logic and process context
//!
//! This module defines `CmdlineGlobMatcher` for identifying AI agent processes,
//! along with `ProcessContext` and helper matching functions.

use super::agent::AgentInfo;
use glob::Pattern;

/// Process context passed to agent matchers for identification
pub struct ProcessContext {
    /// Process name (from `<procfs root>/[pid]/comm` or BPF event)
    pub comm: String,
    /// Parsed command line arguments (argv vector)
    pub cmdline_args: Vec<String>,
    /// Executable file path
    pub exe_path: String,
}

/// Match cmdline args against glob patterns position-by-position.
///
/// Rules:
/// - `patterns[i]` is matched against `cmdline[i]` using glob (case-insensitive)
/// - If patterns is shorter than cmdline, extra cmdline args are ignored (prefix match)
/// - If cmdline is shorter than patterns, returns false (not enough args)
/// - `"*"` matches any value at that position
pub fn match_cmdline_glob(patterns: &[String], cmdline: &[String]) -> bool {
    if cmdline.len() < patterns.len() {
        return false;
    }
    for (pat, arg) in patterns.iter().zip(cmdline.iter()) {
        let pat_lower = pat.to_lowercase();
        let arg_lower = arg.to_lowercase();
        // Fast path for literal "*"
        if pat_lower == "*" {
            continue;
        }
        match Pattern::new(&pat_lower) {
            Ok(p) => {
                if !p.matches(&arg_lower) {
                    return false;
                }
            }
            Err(_) => return false,
        }
    }
    true
}

/// Check if a domain matches any of the given glob patterns.
pub fn match_domain_glob(domain: &str, patterns: &[String]) -> bool {
    let domain_lower = domain.to_lowercase();
    for pat in patterns {
        let pat_lower = pat.to_lowercase();
        match Pattern::new(&pat_lower) {
            Ok(p) => {
                if p.matches(&domain_lower) {
                    return true;
                }
            }
            Err(_) => continue,
        }
    }
    false
}

/// Matcher based on cmdline glob patterns (config-driven).
pub struct CmdlineGlobMatcher {
    info: AgentInfo,
    patterns: Vec<String>,
}

impl CmdlineGlobMatcher {
    pub fn new(agent_name: &str, patterns: Vec<String>) -> Self {
        Self {
            info: AgentInfo {
                name: agent_name.to_string(),
                process_names: patterns.clone(),
                description: "Config-driven agent".to_string(),
                category: "custom".to_string(),
            },
            patterns,
        }
    }

    /// Create from an allow rule (requires `allow=true` and non-empty patterns).
    pub fn from_config(rule: &crate::config::CmdlineRule) -> Option<Self> {
        if !rule.allow || rule.patterns.is_empty() {
            return None;
        }
        Some(Self::new(
            rule.agent_name.as_deref().unwrap_or("Custom Agent"),
            rule.patterns.clone(),
        ))
    }

    /// Create from a deny rule (requires `allow=false` and non-empty patterns).
    pub fn from_deny_rule(rule: &crate::config::CmdlineRule) -> Option<Self> {
        if rule.allow || rule.patterns.is_empty() {
            return None;
        }
        Some(Self::new(
            rule.agent_name.as_deref().unwrap_or("deny-rule"),
            rule.patterns.clone(),
        ))
    }

    /// Return the agent metadata
    pub fn info(&self) -> &AgentInfo {
        &self.info
    }

    /// Return the command-line glob rule used by this matcher.
    pub fn patterns(&self) -> &[String] {
        &self.patterns
    }

    /// Check if a process matches this matcher's patterns
    pub fn matches(&self, ctx: &ProcessContext) -> bool {
        match_cmdline_glob(&self.patterns, &ctx.cmdline_args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_match_cmdline_glob_exact() {
        let patterns = vec!["node".to_string(), "*claude*".to_string()];
        let cmdline = vec!["node".to_string(), "/path/claude-code".to_string()];
        assert!(match_cmdline_glob(&patterns, &cmdline));
    }

    #[test]
    fn test_match_cmdline_glob_prefix() {
        // rule shorter than cmdline -> prefix match succeeds
        let patterns = vec!["node".to_string()];
        let cmdline = vec!["node".to_string(), "extra".to_string()];
        assert!(match_cmdline_glob(&patterns, &cmdline));
    }

    #[test]
    fn test_match_cmdline_glob_too_short() {
        // cmdline shorter than rule -> fails
        let patterns = vec!["node".to_string(), "*claude*".to_string()];
        let cmdline = vec!["node".to_string()];
        assert!(!match_cmdline_glob(&patterns, &cmdline));
    }

    #[test]
    fn test_match_cmdline_glob_wildcard() {
        let patterns = vec!["*".to_string(), "*aider*".to_string()];
        let cmdline = vec!["python3".to_string(), "/path/aider".to_string()];
        assert!(match_cmdline_glob(&patterns, &cmdline));
    }

    #[test]
    fn test_match_cmdline_glob_case_insensitive() {
        let patterns = vec!["NODE".to_string(), "*CLAUDE*".to_string()];
        let cmdline = vec!["node".to_string(), "claude".to_string()];
        assert!(match_cmdline_glob(&patterns, &cmdline));
    }

    #[test]
    fn test_match_domain_glob() {
        let patterns = vec!["*.openai.com".to_string()];
        assert!(match_domain_glob("api.openai.com", &patterns));
        assert!(!match_domain_glob("example.com", &patterns));
    }

    #[test]
    fn test_cmdline_glob_matcher() {
        let matcher = CmdlineGlobMatcher::new(
            "Claude Code",
            vec!["node".to_string(), "*claude*".to_string()],
        );
        let ctx = ProcessContext {
            comm: "node".to_string(),
            cmdline_args: vec!["node".to_string(), "/path/claude-code".to_string()],
            exe_path: "".to_string(),
        };
        assert!(matcher.matches(&ctx));
        assert_eq!(matcher.info().name, "Claude Code");
        assert_eq!(matcher.info().process_names, matcher.patterns());
    }

    #[test]
    fn test_from_config_keeps_cmdline_rule_metadata() {
        let rule = crate::config::CmdlineRule {
            patterns: vec!["*node*".to_string(), "*codex*".to_string()],
            agent_name: Some("Codex".to_string()),
            allow: true,
        };
        let matcher = CmdlineGlobMatcher::from_config(&rule).expect("allow rule creates matcher");

        assert_eq!(matcher.info().name, "Codex");
        assert_eq!(matcher.patterns(), rule.patterns);
        assert_eq!(matcher.info().process_names, rule.patterns);
    }

    #[test]
    fn test_match_cmdline_glob_empty_patterns() {
        // Empty patterns matches any cmdline (no constraints)
        let patterns: Vec<String> = vec![];
        let cmdline = vec!["node".to_string()];
        assert!(match_cmdline_glob(&patterns, &cmdline));
    }

    #[test]
    fn test_match_cmdline_glob_empty_cmdline() {
        let patterns = vec!["node".to_string()];
        let cmdline: Vec<String> = vec![];
        assert!(!match_cmdline_glob(&patterns, &cmdline));
    }

    #[test]
    fn test_match_cmdline_glob_question_mark() {
        let patterns = vec!["node".to_string(), "?.js".to_string()];
        let cmdline = vec!["node".to_string(), "a.js".to_string()];
        assert!(match_cmdline_glob(&patterns, &cmdline));
        let cmdline2 = vec!["node".to_string(), "ab.js".to_string()];
        assert!(!match_cmdline_glob(&patterns, &cmdline2));
    }

    #[test]
    fn test_match_domain_glob_multiple_or() {
        let patterns = vec!["*.openai.com".to_string(), "*.anthropic.com".to_string()];
        assert!(match_domain_glob("api.openai.com", &patterns));
        assert!(match_domain_glob("api.anthropic.com", &patterns));
        assert!(!match_domain_glob("example.com", &patterns));
    }

    #[test]
    fn test_cmdline_glob_matcher_from_config_allow() {
        let rule = crate::config::CmdlineRule {
            patterns: vec!["node".to_string(), "*claude*".to_string()],
            agent_name: Some("Claude Code".to_string()),
            allow: true,
        };
        let matcher = CmdlineGlobMatcher::from_config(&rule).unwrap();
        let ctx = ProcessContext {
            comm: "node".to_string(),
            cmdline_args: vec!["node".to_string(), "/path/claude-code".to_string()],
            exe_path: "".to_string(),
        };
        assert!(matcher.matches(&ctx));
        assert_eq!(matcher.info().name, "Claude Code");
    }

    #[test]
    fn test_cmdline_glob_matcher_from_config_deny_returns_none() {
        let rule = crate::config::CmdlineRule {
            patterns: vec!["node".to_string()],
            agent_name: None,
            allow: false,
        };
        assert!(CmdlineGlobMatcher::from_config(&rule).is_none());
    }

    #[test]
    fn test_cmdline_glob_matcher_from_config_empty_patterns_returns_none() {
        let rule = crate::config::CmdlineRule {
            patterns: vec![],
            agent_name: Some("Test".to_string()),
            allow: true,
        };
        assert!(CmdlineGlobMatcher::from_config(&rule).is_none());
    }

    #[test]
    fn test_cmdline_glob_matcher_from_deny_rule() {
        let rule = crate::config::CmdlineRule {
            patterns: vec!["*spam*".to_string()],
            agent_name: None,
            allow: false,
        };
        let matcher = CmdlineGlobMatcher::from_deny_rule(&rule).unwrap();
        let ctx = ProcessContext {
            comm: "".to_string(),
            cmdline_args: vec!["spam-process".to_string()],
            exe_path: "".to_string(),
        };
        assert!(matcher.matches(&ctx));
    }

    #[test]
    fn test_cmdline_glob_matcher_from_deny_rule_allow_returns_none() {
        let rule = crate::config::CmdlineRule {
            patterns: vec!["node".to_string()],
            agent_name: Some("Test".to_string()),
            allow: true,
        };
        assert!(CmdlineGlobMatcher::from_deny_rule(&rule).is_none());
    }

    /// Match a cmdline against the embedded default rules and return the
    /// first matching agent name (mirrors `AgentScanner::find_match`).
    fn match_default_rules(args: &[&str], exe_path: &str) -> Option<String> {
        let ctx = ProcessContext {
            comm: String::new(),
            cmdline_args: args.iter().map(|s| s.to_string()).collect(),
            exe_path: exe_path.to_string(),
        };
        crate::config::default_cmdline_rules()
            .iter()
            .filter_map(CmdlineGlobMatcher::from_config)
            .find(|m| m.matches(&ctx))
            .map(|m| m.info().name.clone())
    }

    #[test]
    fn test_default_rules_capture_claude_code_bare_name() {
        // Launched from a shell as `claude`
        assert_eq!(
            match_default_rules(&["claude"], "").as_deref(),
            Some("Claude")
        );
    }

    #[test]
    fn test_default_rules_capture_claude_code_absolute_path() {
        // Launched via absolute path (native binary install)
        assert_eq!(
            match_default_rules(&["/usr/local/bin/claude"], "").as_deref(),
            Some("Claude")
        );
        assert_eq!(
            match_default_rules(
                &["/home/user/.local/share/claude/versions/2.0.14/claude"],
                ""
            )
            .as_deref(),
            Some("Claude")
        );
    }

    #[test]
    fn test_default_rules_capture_claude_code_node_wrapper() {
        // npm global install: `claude` is a shebang script, the kernel rewrites
        // argv to `node <script>` so argv[0] is node, not claude
        assert_eq!(
            match_default_rules(
                &[
                    "node",
                    "/usr/local/lib/node_modules/@anthropic-ai/claude-code/cli.js"
                ],
                ""
            )
            .as_deref(),
            Some("Claude")
        );
        assert_eq!(
            match_default_rules(
                &[
                    "/usr/bin/node",
                    "/home/user/.claude/local/node_modules/.bin/claude"
                ],
                ""
            )
            .as_deref(),
            Some("Claude")
        );
    }

    #[test]
    fn test_default_rules_no_false_positive_for_unrelated_process() {
        assert_eq!(match_default_rules(&["vim", "main.rs"], ""), None);
    }

    #[test]
    fn test_default_rules_match_with_exe_path_set() {
        // Verify matching works when exe_path carries a real path,
        // as it does in production eBPF-captured ProcessContext.
        assert_eq!(
            match_default_rules(
                &[
                    "node",
                    "/usr/local/lib/node_modules/@anthropic-ai/claude-code/cli.js"
                ],
                "/usr/bin/node"
            )
            .as_deref(),
            Some("Claude")
        );
        assert_eq!(
            match_default_rules(&["/usr/local/bin/claude"], "/usr/local/bin/claude").as_deref(),
            Some("Claude")
        );
    }

    #[test]
    fn test_default_rules_capture_qwencode_bare_name() {
        assert_eq!(
            match_default_rules(&["qwen"], "").as_deref(),
            Some("QwenCode")
        );
    }

    #[test]
    fn test_default_rules_capture_qwencode_absolute_path() {
        assert_eq!(
            match_default_rules(&["/usr/local/bin/qwen"], "").as_deref(),
            Some("QwenCode")
        );
        assert_eq!(
            match_default_rules(&["/home/user/.local/bin/qwen-code"], "").as_deref(),
            Some("QwenCode")
        );
    }

    #[test]
    fn test_default_rules_capture_qwencode_node_wrapper() {
        assert_eq!(
            match_default_rules(
                &[
                    "node",
                    "/usr/local/lib/node_modules/@qwen-code/cli/index.js"
                ],
                ""
            )
            .as_deref(),
            Some("QwenCode")
        );
    }

    #[test]
    fn test_default_rules_capture_qwencode_node_with_expose_gc() {
        assert_eq!(
            match_default_rules(
                &[
                    "/root/.local/lib/qwen-code/node/bin/node",
                    "--expose-gc",
                    "/root/.local/lib/qwen-code/lib/cli.js",
                ],
                ""
            )
            .as_deref(),
            Some("QwenCode")
        );
    }

    #[test]
    fn test_default_rules_capture_openclaw_gateway_with_node_heap_flag() {
        // OpenClaw 2026.9.2 launches the gateway via systemd --user with a
        // V8 heap flag, so argv[0] is the full node path and "openclaw" sits
        // at argv[2] (not argv[1]) — the flag position must be wildcarded.
        assert_eq!(
            match_default_rules(
                &[
                    "/usr/bin/node",
                    "--max-old-space-size=7722",
                    "/usr/local/lib/node_modules/openclaw/dist/index.js",
                    "gateway",
                    "--port",
                    "18789",
                ],
                ""
            )
            .as_deref(),
            Some("OpenClaw")
        );
    }
}
