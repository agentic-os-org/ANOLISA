//! Discovery module for AI agent process detection
//!
//! This module provides functionality to discover and identify AI agent processes
//! running on the system by scanning the /proc filesystem.
//!
//! # Overview
//!
//! The discovery module consists of:
//! - `agent`: Core types (`AgentInfo`, `DiscoveredAgent`)
//! - `matcher`: Matching logic (`CmdlineGlobMatcher`, `ProcessContext`)
//! - `registry`: Config-driven agent list
//! - `scanner`: System scanner using /proc with allow/deny/domain rules
//!
//! # Example
//!
//! ```rust,ignore
//! use agentsight::discovery::AgentScanner;
//! use agentsight::config::default_cmdline_rules;
//!
//! let mut scanner = AgentScanner::from_rules(&default_cmdline_rules(), &[]);
//! let agents = scanner.scan();
//!
//! for agent in agents {
//!     println!("Found {} (PID: {})", agent.agent_info.name, agent.pid);
//! }
//! ```

pub mod agent;
pub mod connection_scanner;
pub mod matcher;
pub mod scanner;

pub use agent::{AgentInfo, DiscoveredAgent};
pub use connection_scanner::{ConnectionScanResult, ConnectionScanner, IpDomainCache};
pub use matcher::{CmdlineGlobMatcher, ProcessContext, match_cmdline_glob, match_domain_glob};
pub use scanner::AgentScanner;
