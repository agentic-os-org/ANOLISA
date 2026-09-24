//! Command-level validation and key lifecycle precede all per-Skill mutations.

use super::{ScanOptions, SkillRoot, SkillSecService};
use crate::scanner::requested_names;
use crate::{SkillSecError, check_deadline};
use serde_json::{Value, json};
use std::time::Instant;

/// Baseline selection and explicit administrator trust reset.
pub struct InitOptions {
    /// Scan discovered and registered roots after key initialization.
    pub baseline: bool,
    /// Replace existing keys; only the authenticated root caller may request this.
    pub force_keys: bool,
    /// None or empty selects the default built-in scanners.
    pub scanners: Option<Vec<String>>,
}

impl SkillSecService {
    /// Validates the whole baseline request before creating or rotating system trust.
    ///
    /// # Errors
    /// Rejects invalid scanner names, unauthorized rotation, unsafe state and deadlines.
    pub fn init(
        &self,
        roots: &[SkillRoot],
        options: &InitOptions,
        caller_uid: u32,
        deadline: Instant,
    ) -> Result<(Value, i64), SkillSecError> {
        let requested = requested_names(options.scanners.as_deref())?;
        if options.force_keys && caller_uid != 0 {
            return Err(SkillSecError::PermissionDenied);
        }
        let before = self.key_status(deadline)?;
        let key = if options.force_keys && before["initialized"] == true {
            let managed = self.rotation_skills(deadline)?;
            let managed_roots: Vec<_> = roots
                .iter()
                .filter(|root| managed.contains(&root.identity))
                .cloned()
                .collect();
            self.rotate_keys(&managed_roots, caller_uid, deadline)?
        } else {
            self.initialize_with_deadline(deadline)?
        };
        let (results, failed) = if options.baseline {
            batch(roots, deadline, |root| {
                self.scan_selected(root, &requested, false, deadline)
            })?
        } else {
            (Vec::new(), false)
        };
        Ok((
            json!({"command":"init","keyCreated":key["keyCreated"] == true || options.force_keys,"key":key,"baseline":options.baseline,"results":results}),
            i64::from(failed),
        ))
    }

    /// Validates one selection for the complete batch before initializing keys.
    ///
    /// # Errors
    /// Rejects empty batches, invalid names, unsafe key state and deadlines; Skill errors aggregate.
    pub fn scan_batch(
        &self,
        roots: &[SkillRoot],
        options: &ScanOptions,
        deadline: Instant,
    ) -> Result<(Value, i64), SkillSecError> {
        require_batch_roots(roots)?;
        let requested = requested_names(options.scanners.as_deref())?;
        let key = self.initialize_with_deadline(deadline)?;
        let (results, failed) = batch(roots, deadline, |root| {
            self.scan_selected(root, &requested, options.force, deadline)
        })?;
        Ok((
            json!({"command":"scan","keyCreated":key["keyCreated"],"key":key,"results":results}),
            i64::from(failed),
        ))
    }
}

pub(crate) fn with_key(mut output: Value, key: &Value) -> Value {
    output["keyCreated"] = key["keyCreated"].clone();
    if key["keyCreated"] == true {
        output["key"] = key.clone();
    }
    output
}

pub(crate) fn require_batch_roots(roots: &[SkillRoot]) -> Result<(), SkillSecError> {
    if roots.is_empty() {
        return Err(crate::io_error(
            std::path::Path::new("managed Skills"),
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "No skill directories found in system registry or caller discovery",
            ),
        ));
    }
    Ok(())
}

pub(crate) fn batch(
    roots: &[SkillRoot],
    deadline: Instant,
    operation: impl Fn(&SkillRoot) -> Result<Value, SkillSecError>,
) -> Result<(Vec<Value>, bool), SkillSecError> {
    let mut results = Vec::new();
    let mut failed = false;
    for root in roots {
        check_deadline(deadline)?;
        let value = match operation(root) {
            Ok(value) => value,
            Err(SkillSecError::Timeout) => return Err(SkillSecError::Timeout),
            Err(error) => {
                failed = true;
                json!({"skillName":root.identity.name(),"canonicalSkillDir":root.identity,"status":"error","error":error.to_string()})
            }
        };
        results.push(value);
    }
    Ok((results, failed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::SkillSecExecutor;
    use crate::service::tests::{deadline, fixture, uninitialized_fixture};
    use asc_action_runtime::{CapabilityExecutor as _, ExecutionControl};
    use asc_action_types::SkillSecCommand;
    use asc_action_types::SkillSecRequest;
    use std::fs;

    #[test]
    fn invalid_names_and_reports_never_initialize_keys_at_either_entrypoint() {
        let (_temporary, service, root) = uninitialized_fixture();
        let invalid = ScanOptions {
            scanners: Some(vec!["cisco-static-scanner".into()]),
            force: false,
        };
        assert!(service.scan(&root, &invalid, deadline()).is_err());
        assert!(
            service
                .scan_batch(std::slice::from_ref(&root), &invalid, deadline())
                .is_err()
        );
        assert!(
            service
                .certify(&root, "skill-code-scanner", None, &json!([]), deadline())
                .is_err()
        );
        assert!(
            service
                .certify(&root, "custom", None, &json!({"findings":7}), deadline())
                .is_err()
        );
        assert!(!service.config.state_dir.join("signing-key.pk8").exists());
        let executor = SkillSecExecutor::new(service.clone());
        for command in [
            SkillSecCommand::Scan {
                skill_dir: Some(root.identity.clone()),
                all: false,
                skill_dirs: vec![],
                force: false,
                scanners: invalid.scanners.clone(),
            },
            SkillSecCommand::Scan {
                skill_dir: None,
                all: true,
                skill_dirs: vec![root.identity.clone()],
                force: false,
                scanners: invalid.scanners.clone(),
            },
            SkillSecCommand::Certify {
                skill_dir: root.identity.clone(),
                scanner: "skill-code-scanner".into(),
                scanner_version: None,
                findings: json!([]),
            },
            SkillSecCommand::Certify {
                skill_dir: root.identity.clone(),
                scanner: "custom".into(),
                scanner_version: None,
                findings: json!({"findings":7}),
            },
            SkillSecCommand::Init {
                baseline: false,
                force_keys: false,
                skill_dirs: vec![],
                scanners: invalid.scanners.clone(),
            },
        ] {
            let outcome = executor.execute(
                &ExecutionControl {
                    deadline: deadline(),
                    cancelled: false,
                },
                &SkillSecRequest {
                    command,
                    caller_uid: 0,
                },
            );
            assert!(!outcome.success);
            assert!(!service.config.state_dir.join("signing-key.pk8").exists());
            assert!(!root.io_dir.join(".skill-meta").exists());
        }
        let result = service
            .certify(&root, "custom", None, &json!([]), deadline())
            .unwrap();
        assert_eq!(result["keyCreated"], true);
    }

    #[test]
    fn invalid_baseline_selection_cannot_reset_existing_trust() {
        let (_temporary, service, root) = fixture();
        service
            .certify(&root, "custom", None, &json!([]), deadline())
            .unwrap();
        let key_path = service.config.state_dir.join("signing-key.pk8");
        let key = fs::read(&key_path).unwrap();
        let before = service.show(&root, deadline()).unwrap();
        let executor = SkillSecExecutor::new(service.clone());
        for baseline in [false, true] {
            let options = InitOptions {
                baseline,
                force_keys: true,
                scanners: Some(vec!["cisco-static-scanner".into()]),
            };
            assert!(matches!(
                service.init(std::slice::from_ref(&root), &options, 0, deadline()),
                Err(SkillSecError::Invalid(_))
            ));
            let outcome = executor.execute(
                &ExecutionControl {
                    deadline: deadline(),
                    cancelled: false,
                },
                &SkillSecRequest {
                    command: SkillSecCommand::Init {
                        baseline,
                        force_keys: true,
                        skill_dirs: vec![],
                        scanners: options.scanners,
                    },

                    caller_uid: 0,
                },
            );
            assert!(!outcome.success);
            assert_eq!(fs::read(&key_path).unwrap(), key);
            assert!(!service.config.state_dir.join("key-rotation.json").exists());
            assert_eq!(service.show(&root, deadline()).unwrap(), before);
            let audit = service.audit(&root, true, deadline()).unwrap();
            assert_eq!(audit["valid"], true);
            assert_eq!(audit["versions_checked"], 1);
        }
    }
}
