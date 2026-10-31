//! Projects finite Cedar path regions without turning a point grant into a subtree grant.

use std::collections::{BTreeMap, BTreeSet};

use asc_policy_cedar::advice::Advice;
use asc_policy_types::{
    AuthorizationRequest, FileRight, FilesystemRule, NetworkMode, OsPermissionSet, PathFact,
    RuleEffect, Validate,
};

use crate::{
    PdpError, PdpRuntime,
    path_conditions::{contains, probe},
    request,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Access {
    read: bool,
    write: bool,
}

impl PdpRuntime {
    pub(crate) fn permissions(
        &self,
        input: &AuthorizationRequest,
        advice: &mut Vec<Advice>,
    ) -> Result<OsPermissionSet, PdpError> {
        let requested = &input.requested_permissions;
        let environment = &self.snapshot.environment_permissions;
        if !input.capabilities.strict_filesystem {
            return Err(PdpError::Unsupported(
                "PEP cannot establish a default-deny filesystem".into(),
            ));
        }
        if requested.network.mode == NetworkMode::Off && !input.capabilities.network_off {
            return Err(PdpError::Unsupported(
                "PEP cannot disable networking".into(),
            ));
        }
        if requested.network.mode == NetworkMode::Full {
            if environment.network.mode != NetworkMode::Full {
                return Err(PdpError::Denied(
                    "full networking exceeds the deployment bound".into(),
                ));
            }
            let query = request::network(input, &self.schema)?;
            for layer in &self.layers {
                let outcome = layer.evaluate(&query)?;
                if !outcome.allowed {
                    return Err(PdpError::Denied("network.use".into()));
                }
                advice.extend(outcome.advice);
            }
        }
        let facts: BTreeMap<_, _> = input
            .path_facts
            .iter()
            .map(|fact| (fact.path.as_str(), fact))
            .collect();
        let mut required: BTreeSet<String> = requested
            .filesystem
            .rules
            .iter()
            .chain(environment.filesystem.rules.iter())
            .map(|rule| rule.path.clone())
            .collect();
        required.extend(requested.execution.deny_paths.iter().cloned());
        required.extend(environment.execution.deny_paths.iter().cloned());
        required.insert(input.launch.resolved_program.clone());
        required.insert(input.launch.cwd.clone());
        require_facts(&required, &facts)?;
        let roots: Vec<_> = requested
            .filesystem
            .rules
            .iter()
            .filter(|rule| rule.effect == RuleEffect::Allow)
            .collect();
        let mut canonical_boundaries: BTreeSet<String> = requested
            .filesystem
            .rules
            .iter()
            .chain(environment.filesystem.rules.iter())
            .map(|rule| canonical(&rule.path, &facts))
            .collect();
        for layer in &self.layers {
            canonical_boundaries.extend(layer.boundaries.iter().cloned());
        }
        let mut boundaries: BTreeSet<String> = roots.iter().map(|rule| rule.path.clone()).collect();
        for root in &roots {
            for boundary in &canonical_boundaries {
                if let Some(path) = project(boundary, &root.path, &facts) {
                    boundaries.insert(path);
                }
            }
        }
        let noexec_sources: Vec<_> = requested
            .execution
            .deny_paths
            .iter()
            .chain(environment.execution.deny_paths.iter())
            .map(|path| canonical(path, &facts))
            .collect();
        let mut noexec: BTreeSet<_> = requested.execution.deny_paths.iter().cloned().collect();
        for root in &roots {
            let canonical_root = canonical(&root.path, &facts);
            for denied in &noexec_sources {
                if contains(denied, &canonical_root) {
                    noexec.insert(root.path.clone());
                } else if let Some(path) = project(denied, &root.path, &facts) {
                    noexec.insert(path);
                }
            }
        }
        required.extend(boundaries.iter().cloned());
        required.extend(noexec.iter().cloned());
        require_facts(&required, &facts)?;
        if boundaries.len() > 2048 {
            return Err(PdpError::Unsupported(
                "too many filesystem policy regions".into(),
            ));
        }
        self.compile_regions(input, &facts, &boundaries, noexec, advice)
    }

    fn compile_regions(
        &self,
        input: &AuthorizationRequest,
        facts: &BTreeMap<&str, &PathFact>,
        boundaries: &BTreeSet<String>,
        noexec: BTreeSet<String>,
        advice: &mut Vec<Advice>,
    ) -> Result<OsPermissionSet, PdpError> {
        let mut probe_boundaries = boundaries.clone();
        probe_boundaries.extend(facts.keys().map(|path| (*path).to_owned()));
        let mut regions = Vec::new();
        for path in boundaries {
            let access = self.access(input, path, facts, advice)?;
            let fact = facts
                .get(path.as_str())
                .ok_or_else(|| PdpError::MissingFacts(vec![path.clone()]))?;
            if fact.is_directory {
                let child = self.access(input, &probe(path, &probe_boundaries), facts, advice)?;
                if access != child {
                    return Err(PdpError::Unsupported(format!(
                        "directory-only rights cannot represent descendant rights at {path}"
                    )));
                }
            }
            regions.push((path.clone(), access));
        }
        regions.sort_by_key(|(path, _)| path.len());
        let mut effective = input.requested_permissions.clone();
        effective.filesystem.rules.clear();
        for (path, access) in &regions {
            for (right, allowed) in [
                (FileRight::Read, access.read),
                (FileRight::Write, access.write),
            ] {
                let inherited_allow = mounted_allow(&effective, path, right, facts);
                if *path == input.launch.resolved_program && right == FileRight::Read && !allowed {
                    return Err(PdpError::Denied(
                        "the executable is not readable in the authorized view".into(),
                    ));
                }
                if allowed && !inherited_allow {
                    if effective.filesystem.rules.iter().any(|rule| {
                        rule.effect == RuleEffect::Deny
                            && literal_covers(&rule.path, path, facts)
                            && rule_blocks(rule, right)
                    }) {
                        return Err(PdpError::Unsupported(format!(
                            "grant below a denied ancestor cannot be represented at {path}"
                        )));
                    }
                    add_rule(&mut effective, path, RuleEffect::Allow, right);
                } else if !allowed && inherited_allow {
                    add_rule(&mut effective, path, RuleEffect::Deny, right);
                }
            }
        }
        // The backend also mounts these pinned objects at their canonical launch paths.
        for required in [&input.launch.resolved_program, &input.launch.cwd] {
            if !rules_allow(&effective, required, FileRight::Read, facts) {
                return Err(PdpError::Denied(format!(
                    "required launch path is not readable: {required}"
                )));
            }
        }
        effective.execution.deny_paths = noexec.into_iter().collect();
        if !effective.execution.deny_paths.is_empty() && !input.capabilities.noexec {
            return Err(PdpError::Unsupported(
                "PEP cannot install the required noexec mounts".into(),
            ));
        }
        if effective
            .execution
            .deny_paths
            .iter()
            .any(|path| covers(path, &input.launch.resolved_program, facts))
        {
            return Err(PdpError::Denied(
                "the initial executable is under a noexec restriction".into(),
            ));
        }
        effective.validate()?;
        Ok(effective)
    }

    fn access(
        &self,
        input: &AuthorizationRequest,
        path: &str,
        facts: &BTreeMap<&str, &PathFact>,
        advice: &mut Vec<Advice>,
    ) -> Result<Access, PdpError> {
        let mut read = rules_allow(&input.requested_permissions, path, FileRight::Read, facts)
            && rules_allow(
                &self.snapshot.environment_permissions,
                path,
                FileRight::Read,
                facts,
            );
        let mut write = rules_allow(&input.requested_permissions, path, FileRight::Write, facts)
            && rules_allow(
                &self.snapshot.environment_permissions,
                path,
                FileRight::Write,
                facts,
            );
        let mut read_advice = Vec::new();
        let mut write_advice = Vec::new();
        for (right, allowed, applicable) in [
            (false, &mut read, &mut read_advice),
            (true, &mut write, &mut write_advice),
        ] {
            if !*allowed {
                continue;
            }
            let query = request::file(input, &self.schema, &canonical(path, facts), right)?;
            let mut matched = Vec::new();
            for layer in &self.layers {
                let outcome = layer.evaluate(&query)?;
                *allowed &= outcome.allowed;
                if outcome.allowed {
                    matched.extend(outcome.advice);
                }
            }
            if *allowed {
                applicable.extend(matched);
            }
        }
        write &= read;
        advice.extend(read_advice);
        if write {
            advice.extend(write_advice);
        }
        Ok(Access { read, write })
    }
}

fn require_facts(
    required: &BTreeSet<String>,
    facts: &BTreeMap<&str, &PathFact>,
) -> Result<(), PdpError> {
    let missing: Vec<_> = required
        .iter()
        .filter(|path| !facts.contains_key(path.as_str()))
        .cloned()
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(PdpError::MissingFacts(missing))
    }
}

fn canonical(path: &str, facts: &BTreeMap<&str, &PathFact>) -> String {
    if let Some(fact) = facts.get(path) {
        return fact.canonical_path.clone();
    }
    // Only synthetic region probes lack exact facts; resolve them under a pinned directory.
    if let Some(fact) = facts
        .values()
        .filter(|fact| fact.is_directory && contains(&fact.path, path))
        .max_by_key(|fact| fact.path.len())
    {
        let suffix = path
            .strip_prefix(fact.path.trim_end_matches('/'))
            .unwrap_or_default();
        return format!("{}{suffix}", fact.canonical_path.trim_end_matches('/'));
    }
    path.to_owned()
}

fn project(boundary: &str, mount: &str, facts: &BTreeMap<&str, &PathFact>) -> Option<String> {
    let root = canonical(mount, facts);
    if root == boundary {
        return Some(mount.to_owned());
    }
    if !facts.get(mount).is_some_and(|fact| fact.is_directory) || !contains(&root, boundary) {
        return None;
    }
    let suffix = boundary.strip_prefix(root.trim_end_matches('/'))?;
    Some(format!("{}{suffix}", mount.trim_end_matches('/')))
}

fn literal_covers(root: &str, path: &str, facts: &BTreeMap<&str, &PathFact>) -> bool {
    root == path
        || facts
            .get(root)
            .is_some_and(|fact| fact.is_directory && contains(root, path))
}

fn covers(root: &str, path: &str, facts: &BTreeMap<&str, &PathFact>) -> bool {
    let canonical_root = canonical(root, facts);
    let canonical_path = canonical(path, facts);
    canonical_root == canonical_path
        || facts
            .get(root)
            .is_some_and(|fact| fact.is_directory && contains(&canonical_root, &canonical_path))
}

fn rule_blocks(rule: &FilesystemRule, right: FileRight) -> bool {
    rule.rights.contains(&right)
        || right == FileRight::Write && rule.rights.contains(&FileRight::Read)
}

fn rules_allow(
    permissions: &OsPermissionSet,
    path: &str,
    right: FileRight,
    facts: &BTreeMap<&str, &PathFact>,
) -> bool {
    check_rules(permissions, path, right, facts, true)
}

fn mounted_allow(
    permissions: &OsPermissionSet,
    path: &str,
    right: FileRight,
    facts: &BTreeMap<&str, &PathFact>,
) -> bool {
    check_rules(permissions, path, right, facts, false)
}

fn check_rules(
    permissions: &OsPermissionSet,
    path: &str,
    right: FileRight,
    facts: &BTreeMap<&str, &PathFact>,
    identities: bool,
) -> bool {
    let mut allowed = false;
    for rule in permissions.filesystem.rules.iter().filter(|rule| {
        if identities {
            covers(&rule.path, path, facts)
        } else {
            literal_covers(&rule.path, path, facts)
        }
    }) {
        if rule.effect == RuleEffect::Deny && rule_blocks(rule, right) {
            return false;
        }
        if rule.effect == RuleEffect::Allow
            && (rule.rights.contains(&right)
                || right == FileRight::Read && rule.rights.contains(&FileRight::Write))
        {
            allowed = true;
        }
    }
    allowed
}

fn add_rule(permissions: &mut OsPermissionSet, path: &str, effect: RuleEffect, right: FileRight) {
    if let Some(rule) = permissions
        .filesystem
        .rules
        .iter_mut()
        .find(|rule| rule.path == path && rule.effect == effect)
    {
        if !rule.rights.contains(&right) {
            rule.rights.push(right);
        }
    } else {
        permissions.filesystem.rules.push(FilesystemRule {
            effect,
            path: path.to_owned(),
            rights: vec![right],
        });
    }
}
