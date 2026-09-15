//! Dependency provisioner: install missing system packages after resolver preflight.
//!
//! This module bridges the gap between the read-only [`DependencyResolver`](crate::resolver::DependencyResolver) and
//! the install executor. It consumes a [`ResolutionPlan`], classifies each
//! unsatisfied dependency by provisionability, and either auto-installs (system
//! mode) or reports remediation (user mode).
//!
//! Design invariants:
//! - The resolver remains pure (read-only, never mutates the host).
//! - This layer is the single place that decides whether to call the platform
//!   package manager.
//! - `PlatformCapability` dependencies are never provisioned — a miss is fatal.
//! - Behavior is deterministic: no interactive prompts, no TTY detection.

use crate::manifest::{DependencyKind, RuntimeDependency};
use crate::resolver::{DependencyStatus, ResolutionPlan, ResolverEnv};

// ---------------------------------------------------------------------------
// Strategy
// ---------------------------------------------------------------------------

/// How the provisioner should behave when dependencies are missing.
/// Selected solely by `install_mode` — no CLI flags in this iteration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProvisionStrategy {
    /// Auto-install missing system packages without prompting (system mode).
    Auto,
    /// Report missing dependencies and signal the caller to exit (user mode).
    ReportAndExit,
}

// ---------------------------------------------------------------------------
// Plan
// ---------------------------------------------------------------------------

/// A system package that can be auto-installed by the host package manager.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvisionablePackage {
    /// Logical dependency name from the manifest.
    pub name: String,
    /// The actual package name to pass to `dnf install` / `apt install`.
    /// Falls back to `name` when no `packages.rpm` / `packages.deb` mapping
    /// exists in the manifest.
    pub package_name: String,
    /// Human-readable remediation command (e.g. `sudo dnf install btrfs-progs`).
    pub remediation: String,
}

/// A dependency that requires manual intervention (e.g. a language runtime
/// whose version constraint cannot be satisfied by the system package manager).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManualDependency {
    /// Logical dependency name.
    pub name: String,
    /// Human-readable install hint surfaced to the user.
    pub hint: String,
}

/// A dependency that blocks provisioning: a host requirement cannot be met,
/// or a failed presence query makes automatic installation unsafe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnresolvableDependency {
    /// Logical dependency name.
    pub name: String,
    /// Host requirement or query failure that blocks installation.
    pub reason: String,
}

/// Classified view of a resolver preflight, ready for the provisioner to act on.
///
/// Built from [`ResolutionPlan`] + the original [`RuntimeDependency`] slice so
/// it can extract package-name mappings.
#[derive(Debug, Clone, Default)]
pub struct ProvisionPlan {
    /// System packages that can be auto-installed
    /// (`kind = SystemPackage`, `status = Unresolved`).
    pub installable: Vec<ProvisionablePackage>,
    /// Dependencies that require manual intervention
    /// (`kind = LanguageRuntime`, `status = Unresolved`).
    pub manual: Vec<ManualDependency>,
    /// Dependencies that block installation (`Unresolvable` or `ProbeFailed`).
    pub unresolvable: Vec<UnresolvableDependency>,
    /// Count of dependencies already satisfied.
    pub satisfied_count: usize,
    /// Non-fatal warnings forwarded from the resolver.
    pub warnings: Vec<String>,
}

impl ProvisionPlan {
    /// Build a provision plan from a completed resolver preflight.
    ///
    /// `deps` is the original manifest `runtime_deps` slice — needed to look up
    /// the `packages` mapping for each unresolved system-package dependency.
    /// `env` provides the host's package-base family for selecting the correct
    /// package name.
    pub fn from_resolution(
        plan: &ResolutionPlan,
        deps: &[RuntimeDependency],
        env: &ResolverEnv,
    ) -> Self {
        let mut result = Self {
            warnings: plan.warnings.clone(),
            ..Default::default()
        };

        for resolution in &plan.resolutions {
            match &resolution.status {
                DependencyStatus::Resolved => {
                    result.satisfied_count += 1;
                }
                DependencyStatus::Unresolved { remediation } => {
                    // Look up the original dep for package-name mapping.
                    let dep = deps.iter().find(|d| d.name == resolution.name);

                    match resolution.kind {
                        DependencyKind::SystemPackage => {
                            let package_name =
                                resolve_package_name(dep, env).unwrap_or(resolution.name.clone());
                            result.installable.push(ProvisionablePackage {
                                name: resolution.name.clone(),
                                package_name,
                                remediation: remediation.clone(),
                            });
                        }
                        DependencyKind::LanguageRuntime => {
                            // If the manifest provides a package mapping, treat
                            // it as installable (the system repo might have it).
                            if let Some(pkg) = resolve_package_name(dep, env) {
                                result.installable.push(ProvisionablePackage {
                                    name: resolution.name.clone(),
                                    package_name: pkg,
                                    remediation: remediation.clone(),
                                });
                            } else {
                                result.manual.push(ManualDependency {
                                    name: resolution.name.clone(),
                                    hint: remediation.clone(),
                                });
                            }
                        }
                        DependencyKind::PlatformCapability => {
                            // Should not happen (platform caps go Unresolvable),
                            // but handle defensively.
                            result.unresolvable.push(UnresolvableDependency {
                                name: resolution.name.clone(),
                                reason: remediation.clone(),
                            });
                        }
                    }
                }
                DependencyStatus::Unresolvable { reason } => {
                    result.unresolvable.push(UnresolvableDependency {
                        name: resolution.name.clone(),
                        reason: reason.clone(),
                    });
                }
                DependencyStatus::ProbeFailed { error } => {
                    result.unresolvable.push(UnresolvableDependency {
                        name: resolution.name.clone(),
                        reason: format!("dependency probe failed: {error}"),
                    });
                }
            }
        }

        result
    }

    /// Whether the plan has any unresolvable dependencies that block install
    /// regardless of mode.
    pub fn has_blockers(&self) -> bool {
        !self.unresolvable.is_empty()
    }

    /// Whether there are installable packages to provision.
    pub fn has_installable(&self) -> bool {
        !self.installable.is_empty()
    }

    /// Whether there are manual-only dependencies (warnings, non-blocking).
    pub fn has_manual(&self) -> bool {
        !self.manual.is_empty()
    }

    /// Whether every dependency is satisfied (nothing to do).
    pub fn is_satisfied(&self) -> bool {
        self.installable.is_empty() && self.manual.is_empty() && self.unresolvable.is_empty()
    }

    /// Aggregate package names for a single `PackageManager::install` call.
    pub fn installable_package_names(&self) -> Vec<&str> {
        self.installable
            .iter()
            .map(|p| p.package_name.as_str())
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Outcome
// ---------------------------------------------------------------------------

/// Result of running the provisioner for a single install operation.
#[derive(Debug, Clone)]
pub enum ProvisionOutcome {
    /// All installable deps were provisioned successfully (system mode).
    Provisioned {
        /// Package names that were installed.
        installed_packages: Vec<String>,
    },
    /// Provisioning failed for some packages (system mode).
    Failed {
        /// Why the package-manager call failed.
        reason: String,
    },
    /// No provisioning needed — all deps already satisfied.
    NothingToDo,
    /// Reported missing deps and signaled exit (user mode).
    Reported {
        /// Logical dependency names that are missing.
        missing: Vec<String>,
        /// Remediation commands for the user.
        remediation: Vec<String>,
    },
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Resolve the platform-appropriate package name for a dependency.
///
/// Priority: `packages.rpm` / `packages.deb` (matching host pkg_base) > `None`.
/// Returns `None` when no mapping exists and the caller should fall back to
/// the dependency's logical name or treat it as manual.
fn resolve_package_name(dep: Option<&RuntimeDependency>, env: &ResolverEnv) -> Option<String> {
    let dep = dep?;
    match env.pkg_base.as_deref() {
        Some("rpm") => {
            if dep.kind == DependencyKind::SystemPackage {
                // System packages always have a resolvable name.
                Some(dep.packages.rpm.clone().unwrap_or_else(|| dep.name.clone()))
            } else {
                // Language runtimes need an explicit mapping.
                dep.packages.rpm.clone()
            }
        }
        Some("deb") => {
            if dep.kind == DependencyKind::SystemPackage {
                Some(dep.packages.deb.clone().unwrap_or_else(|| dep.name.clone()))
            } else {
                dep.packages.deb.clone()
            }
        }
        _ => {
            // Unknown package base: system packages fall back to dep name,
            // language runtimes require an explicit mapping.
            if dep.kind == DependencyKind::SystemPackage {
                Some(
                    dep.packages
                        .rpm
                        .clone()
                        .or_else(|| dep.packages.deb.clone())
                        .unwrap_or_else(|| dep.name.clone()),
                )
            } else {
                dep.packages
                    .rpm
                    .clone()
                    .or_else(|| dep.packages.deb.clone())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolver::DependencyResolution;

    fn make_resolution(
        name: &str,
        kind: DependencyKind,
        status: DependencyStatus,
    ) -> DependencyResolution {
        DependencyResolution {
            name: name.to_string(),
            kind,
            status,
            detail: None,
        }
    }

    fn make_dep(name: &str, kind: DependencyKind) -> RuntimeDependency {
        RuntimeDependency {
            name: name.to_string(),
            kind,
            version: None,
            probe: None,
            source: None,
            packages: Default::default(),
            check: None,
            min_kernel: None,
        }
    }

    #[test]
    fn satisfied_deps_counted() {
        let plan = ResolutionPlan {
            resolutions: vec![
                make_resolution(
                    "jq",
                    DependencyKind::SystemPackage,
                    DependencyStatus::Resolved,
                ),
                make_resolution(
                    "curl",
                    DependencyKind::SystemPackage,
                    DependencyStatus::Resolved,
                ),
            ],
            warnings: vec![],
        };
        let deps = vec![
            make_dep("jq", DependencyKind::SystemPackage),
            make_dep("curl", DependencyKind::SystemPackage),
        ];
        let env = ResolverEnv {
            pkg_base: Some("rpm".into()),
            ..Default::default()
        };

        let pp = ProvisionPlan::from_resolution(&plan, &deps, &env);
        assert_eq!(pp.satisfied_count, 2);
        assert!(pp.is_satisfied());
    }

    #[test]
    fn unresolved_system_package_is_installable() {
        let plan = ResolutionPlan {
            resolutions: vec![make_resolution(
                "kernel-headers",
                DependencyKind::SystemPackage,
                DependencyStatus::Unresolved {
                    remediation: "sudo dnf install kernel-headers".into(),
                },
            )],
            warnings: vec![],
        };
        let deps = vec![make_dep("kernel-headers", DependencyKind::SystemPackage)];
        let env = ResolverEnv {
            pkg_base: Some("rpm".into()),
            ..Default::default()
        };

        let pp = ProvisionPlan::from_resolution(&plan, &deps, &env);
        assert!(pp.has_installable());
        assert_eq!(pp.installable[0].package_name, "kernel-headers");
    }

    #[test]
    fn unresolved_language_runtime_without_package_is_manual() {
        let plan = ResolutionPlan {
            resolutions: vec![make_resolution(
                "node",
                DependencyKind::LanguageRuntime,
                DependencyStatus::Unresolved {
                    remediation: "install node>=20 manually".into(),
                },
            )],
            warnings: vec![],
        };
        let deps = vec![make_dep("node", DependencyKind::LanguageRuntime)];
        let env = ResolverEnv {
            pkg_base: Some("rpm".into()),
            ..Default::default()
        };

        let pp = ProvisionPlan::from_resolution(&plan, &deps, &env);
        // No package mapping → manual
        assert!(!pp.has_installable());
        assert!(pp.has_manual());
    }

    #[test]
    fn unresolvable_is_blocker() {
        let plan = ResolutionPlan {
            resolutions: vec![make_resolution(
                "btf",
                DependencyKind::PlatformCapability,
                DependencyStatus::Unresolvable {
                    reason: "kernel BTF not available".into(),
                },
            )],
            warnings: vec![],
        };
        let deps = vec![make_dep("btf", DependencyKind::PlatformCapability)];
        let env = ResolverEnv::default();

        let pp = ProvisionPlan::from_resolution(&plan, &deps, &env);
        assert!(pp.has_blockers());
    }

    #[test]
    fn language_runtime_with_package_mapping_is_installable() {
        let plan = ResolutionPlan {
            resolutions: vec![make_resolution(
                "node",
                DependencyKind::LanguageRuntime,
                DependencyStatus::Unresolved {
                    remediation: "sudo dnf install nodejs".into(),
                },
            )],
            warnings: vec![],
        };
        let mut dep = make_dep("node", DependencyKind::LanguageRuntime);
        dep.packages.rpm = Some("nodejs".into());
        let deps = vec![dep];
        let env = ResolverEnv {
            pkg_base: Some("rpm".into()),
            ..Default::default()
        };

        let pp = ProvisionPlan::from_resolution(&plan, &deps, &env);
        assert!(pp.has_installable());
        // package_name is what gets passed to `dnf install`.
        assert_eq!(pp.installable[0].package_name, "nodejs");
        // name is the logical manifest dep name — used for recheck filtering.
        assert_eq!(pp.installable[0].name, "node");
    }
}
