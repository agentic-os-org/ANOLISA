//! Startup mappings from Cedar agent categories to existing kernel process selectors.
use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use asc_policy_types::{Validate, scope::ProcessMatcher};

pub(super) fn load(path: &Path) -> io::Result<BTreeMap<String, ProcessMatcher>> {
    let mut matchers = BTreeMap::from([(
        "cosh".to_owned(),
        ProcessMatcher::Name {
            process_name: "cosh-core".into(),
        },
    )]);
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(matchers),
        Err(error) => return Err(error),
        Ok(_) => {}
    }
    let config =
        asc_capability_skill_sec::config::read_system_config(path).map_err(io::Error::other)?;
    let configured: BTreeMap<String, ProcessMatcher> =
        serde_json::from_value(config).map_err(io::Error::other)?;
    for (agent, matcher) in configured {
        if agent.trim().is_empty() || agent.chars().any(char::is_control) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "agentType must be a nonempty name",
            ));
        }
        matcher.validate().map_err(io::Error::other)?;
        matchers.insert(agent, matcher);
    }
    Ok(matchers)
}
