//! Projects finite Cedar path regions without turning a point grant into a subtree grant.

use std::collections::BTreeSet;

use asc_daemon_protocol::authorization::{
    ApprovalTarget, AuthorizationRequest, FileRight, FilesystemRule, NetworkMode, OsPermissionSet,
    RuleEffect, Validate,
};

use crate::{
    PdpError, PdpRuntime,
    decision::ScopedAdvice,
    path_conditions::{contains, probe},
    request,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Access {
    read: bool,
    write: bool,
}

pub(crate) struct Projection {
    pub permissions: OsPermissionSet,
    pub narrowed: bool,
    pub require_files: Vec<String>,
}

impl PdpRuntime {
    pub(crate) fn permissions(
        &self,
        input: &AuthorizationRequest,
        advice: &mut Vec<ScopedAdvice>,
    ) -> Result<Projection, PdpError> {
        let requested = &input.requested_permissions;
        let environment = &self.snapshot.environment_permissions;
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
                advice.extend(
                    outcome
                        .advice
                        .into_iter()
                        .map(|value| (ApprovalTarget::Network, value)),
                );
            }
        }
        let roots: Vec<_> = requested
            .filesystem
            .rules
            .iter()
            .filter(|rule| rule.effect == RuleEffect::Allow)
            .map(|rule| &rule.path)
            .collect();
        let boundaries: BTreeSet<_> = requested
            .filesystem
            .rules
            .iter()
            .chain(&environment.filesystem.rules)
            .map(|rule| &rule.path)
            .chain(self.layers.iter().flat_map(|layer| layer.boundaries.iter()))
            .filter(|path| roots.iter().any(|root| contains(root, path)))
            .cloned()
            .collect();
        let mut noexec: BTreeSet<_> = requested.execution.deny_paths.iter().cloned().collect();
        for root in roots {
            for denied in &environment.execution.deny_paths {
                if contains(denied, root) {
                    noexec.insert(root.clone());
                } else if contains(root, denied) {
                    noexec.insert(denied.clone());
                }
            }
        }
        if boundaries.len() > 2048 {
            return Err(PdpError::Unsupported(
                "too many filesystem policy regions".into(),
            ));
        }
        self.compile_regions(input, &boundaries, noexec, advice)
    }

    fn compile_regions(
        &self,
        input: &AuthorizationRequest,
        boundaries: &BTreeSet<String>,
        noexec: BTreeSet<String>,
        advice: &mut Vec<ScopedAdvice>,
    ) -> Result<Projection, PdpError> {
        let mut require_files = Vec::new();
        let mut narrowed = false;
        let mut regions = Vec::new();
        for path in boundaries {
            let mut region_advice = Vec::new();
            let access = self.access(input, path, &mut region_advice)?;
            if path != &input.launch.resolved_program {
                let mut child_advice = Vec::new();
                let child = self.access(input, &probe(path, boundaries), &mut child_advice)?;
                if access == child {
                    region_advice.extend(child_advice);
                } else if path == &input.launch.cwd {
                    return Err(PdpError::Unsupported(format!(
                        "directory-only rights at {path}"
                    )));
                } else {
                    // Only a file can implement point rights when its subtree would differ.
                    require_files.push(path.clone());
                }
            }
            bind_region_advice(path, boundaries, &mut region_advice);
            advice.extend(region_advice);
            narrowed |= [
                (FileRight::Read, access.read),
                (FileRight::Write, access.write),
            ]
            .iter()
            .any(|(right, allowed)| {
                !allowed && rules_allow(&input.requested_permissions, path, *right)
            });
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
                let inherited_allow = rules_allow(&effective, path, right);
                if allowed && !inherited_allow {
                    if effective.filesystem.rules.iter().any(|rule| {
                        rule.effect == RuleEffect::Deny
                            && contains(&rule.path, path)
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
            if !rules_allow(&effective, required, FileRight::Read) {
                return Err(PdpError::Denied(format!(
                    "required launch path is not readable: {required}"
                )));
            }
        }
        effective.execution.deny_paths = noexec.into_iter().collect();
        if effective
            .execution
            .deny_paths
            .iter()
            .any(|path| contains(path, &input.launch.resolved_program))
        {
            return Err(PdpError::Denied(
                "the initial executable is under a noexec restriction".into(),
            ));
        }
        effective.validate()?;
        narrowed |= effective.execution.deny_paths.iter().any(|p| {
            !input
                .requested_permissions
                .execution
                .deny_paths
                .iter()
                .any(|base| contains(base, p))
                && (rules_allow(&effective, p, FileRight::Read)
                    || effective.filesystem.rules.iter().any(|r| {
                        contains(p, &r.path) && rules_allow(&effective, &r.path, FileRight::Read)
                    }))
        });
        Ok(Projection {
            permissions: effective,
            narrowed,
            require_files,
        })
    }

    fn access(
        &self,
        input: &AuthorizationRequest,
        path: &str,
        advice: &mut Vec<ScopedAdvice>,
    ) -> Result<Access, PdpError> {
        let mut read = rules_allow(&input.requested_permissions, path, FileRight::Read)
            && rules_allow(
                &self.snapshot.environment_permissions,
                path,
                FileRight::Read,
            );
        let mut write = rules_allow(&input.requested_permissions, path, FileRight::Write)
            && rules_allow(
                &self.snapshot.environment_permissions,
                path,
                FileRight::Write,
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
            let query = request::file(input, &self.schema, path, right)?;
            let mut matched = Vec::new();
            for layer in &self.layers {
                let outcome = layer.evaluate(&query)?;
                *allowed &= outcome.allowed;
                if outcome.allowed {
                    matched.extend(outcome.advice.into_iter().map(|value| {
                        (
                            ApprovalTarget::Filesystem {
                                path: path.into(),
                                right: if right {
                                    FileRight::Write
                                } else {
                                    FileRight::Read
                                },
                                excluded_paths: Vec::new(),
                            },
                            value,
                        )
                    }));
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

fn bind_region_advice(path: &str, boundaries: &BTreeSet<String>, advice: &mut [ScopedAdvice]) {
    let mut excluded_paths: Vec<String> = Vec::new();
    for child in boundaries
        .iter()
        .filter(|child| child.as_str() != path && contains(path, child))
    {
        if !excluded_paths.iter().any(|parent| contains(parent, child)) {
            excluded_paths.push(child.clone());
        }
    }
    // A representative child is an evaluation probe, never a user-visible resource.
    for (target, _) in advice {
        if let ApprovalTarget::Filesystem {
            path: root,
            excluded_paths: excluded,
            ..
        } = target
        {
            path.clone_into(root);
            excluded.clone_from(&excluded_paths);
        }
    }
}

fn rule_blocks(rule: &FilesystemRule, right: FileRight) -> bool {
    rule.rights.contains(&right)
        || right == FileRight::Write && rule.rights.contains(&FileRight::Read)
}

fn rules_allow(permissions: &OsPermissionSet, path: &str, right: FileRight) -> bool {
    let mut allowed = false;
    for rule in permissions
        .filesystem
        .rules
        .iter()
        .filter(|rule| contains(&rule.path, path))
    {
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
