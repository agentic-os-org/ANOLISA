//! Preflight and register exact file identities for kernels without LSM path resolution.
//! Held O_PATH descriptors keep registered inode identities alive until map cleanup.

use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::sync::Arc;

use actplane_ifc_compiler::ast::{Clause, Effect, Expr, Kind, Op, Policy, Rule};
use actplane_ifc_compiler::{Compiled, RuleMeta, parse};
use ebpf_ifc_engine::{INODE_GUARD_RENAME, INODE_GUARD_UNLINK, PinnedEngine};

use crate::BackendError;

#[derive(Clone, Debug)]
struct InodeFile {
    ino: u64,
    dev: u32,
    path: String,
    rule_id: Option<u32>,
    _file: Arc<File>,
}

/// Validated identities; cloning retains their descriptors, not just their numbers.
#[derive(Clone, Debug, Default)]
pub(super) struct InodePolicy {
    files: Vec<InodeFile>,
}

impl InodePolicy {
    /// Validate everything without changing the active binding or any BPF map.
    pub(super) fn prepare(
        dsl: &str,
        compiled: &Compiled,
        inode_only: bool,
    ) -> Result<Self, BackendError> {
        // Full-path hooks evaluate the original policy, including globs and conditions.
        // Do not replace those semantics with an unconditional inode fast path.
        if !inode_only {
            return Ok(Self::default());
        }
        let policy = parse::parse(dsl).map_err(BackendError::CompileFailure)?;
        let guards = extract_guarded_paths(&policy, compiled)?;
        let mut prepared = Self::default();
        for source in policy
            .sources
            .iter()
            .filter(|source| source.kind == Kind::File)
        {
            prepared.add_file(&source.pattern, None)?;
        }
        for (path, rule_id) in guards {
            prepared.add_file(&path, Some(rule_id))?;
        }
        Ok(prepared)
    }

    fn add_file(&mut self, path: &str, rule_id: Option<u32>) -> Result<(), BackendError> {
        let file = pin_file(path, rule_id)?;
        if let Some(existing) = self
            .files
            .iter_mut()
            .find(|entry| entry.ino == file.ino && entry.dev == file.dev)
        {
            if existing.path != path {
                return Err(invalid(format!(
                    "ambiguous inode aliases '{}' and '{path}': use one canonical path per inode in a domain",
                    existing.path
                )));
            }
            // Sources carry no delete guard. Duplicate guards retain the first
            // lowered rule, matching the kernel's ordered rule table.
            existing.rule_id = match (existing.rule_id, rule_id) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
        } else {
            self.files.push(file);
        }
        Ok(())
    }

    /// Register paths for source matching/audit, and guards only for block clauses.
    /// The caller must batch-clean both maps if any insertion fails.
    pub(super) fn install(&self, engine: &PinnedEngine, domain: u32) -> io::Result<()> {
        self.populate(
            |file| engine.register_inode_path(file.ino, file.dev, domain, &file.path),
            |file, rule_id| {
                engine.guard_inode(
                    file.ino,
                    file.dev,
                    INODE_GUARD_UNLINK | INODE_GUARD_RENAME,
                    domain,
                    rule_id,
                )
            },
        )
    }

    fn populate(
        &self,
        mut register_path: impl FnMut(&InodeFile) -> io::Result<()>,
        mut guard: impl FnMut(&InodeFile, u32) -> io::Result<()>,
    ) -> io::Result<()> {
        for file in &self.files {
            register_path(file)
                .map_err(|error| registration_error("register inode path", file, error))?;
            if let Some(rule_id) = file.rule_id {
                guard(file, rule_id)
                    .map_err(|error| registration_error("guard inode", file, error))?;
            }
        }
        Ok(())
    }
}

/// Attempt both singleton map clears even if the first fails.
pub(super) fn clear(engine: &PinnedEngine) -> Vec<String> {
    let mut errors = Vec::new();
    if let Err(error) = engine.clear_inode_guards() {
        errors.push(format!("clear inode guards: {error}"));
    }
    if let Err(error) = engine.clear_inode_paths() {
        errors.push(format!("clear inode paths: {error}"));
    }
    errors
}

fn extract_guarded_paths(
    policy: &Policy,
    compiled: &Compiled,
) -> Result<Vec<(String, u32)>, BackendError> {
    let seed = if compiled.labels.contains_key("COMMAND") {
        "COMMAND"
    } else {
        "AGENT"
    };
    let mut paths = Vec::new();
    for rule in &policy.rules {
        for clause in &rule.clauses {
            if !matches!(clause.op, Op::Unlink | Op::Write) || clause.effect == Effect::Notify {
                continue;
            }
            let seeded = matches!(&clause.when, Expr::True)
                || matches!(&clause.when, Expr::Label(label) if label == seed);
            if clause.target.kind != Kind::File
                || clause.effect != Effect::Block
                || clause.unless.is_some()
                || !seeded
            {
                return Err(invalid(format!(
                    "rule '{}', clause {}: inode-only delete protection requires block unlink/write file with no unless and no condition other than 'if {seed}'; use full-path mode for this policy",
                    rule.name, clause.source_index
                )));
            }
            let matches: Vec<_> = compiled
                .meta
                .iter()
                .enumerate()
                .filter(|(_, meta)| matches_clause(meta, rule, clause))
                .collect();
            if matches.len() != 1 {
                return Err(invalid(format!(
                    "rule '{}', clause {}: expected one unambiguous lowered write rule, found {}",
                    rule.name,
                    clause.source_index,
                    matches.len()
                )));
            }
            let (rule_id, meta) = matches[0];
            let source_matches = policy
                .rules
                .iter()
                .flat_map(|rule| {
                    rule.clauses
                        .iter()
                        .filter(move |clause| matches_clause(meta, rule, clause))
                })
                .count();
            if source_matches != 1
                || meta.kernel_op != "write"
                || meta.effect != Effect::Block
                || meta.reason != rule.reason
                || compiled.reasons.get(rule_id) != Some(&rule.reason)
            {
                return Err(invalid(format!(
                    "rule '{}', clause {}: ambiguous or inconsistent lowered rule metadata",
                    rule.name, clause.source_index
                )));
            }
            let rule_id = u32::try_from(rule_id)
                .map_err(|_| invalid("lowered rule index exceeds the inode guard ABI"))?;
            paths.push((clause.target.pattern.clone(), rule_id));
        }
    }
    Ok(paths)
}

fn matches_clause(meta: &RuleMeta, rule: &Rule, clause: &Clause) -> bool {
    let op = match clause.op {
        Op::Unlink => "unlink",
        Op::Write => "write",
        _ => return false,
    };
    meta.name == rule.name
        && meta.clause_source_index == clause.source_index
        && meta.clause_op == op
        && meta.target_kind == clause.target.kind
        && meta.target_pattern == clause.target.pattern
        && meta.target_arg == clause.target.arg
}

fn pin_file(path: &str, rule_id: Option<u32>) -> Result<InodeFile, BackendError> {
    if !Path::new(path).is_absolute() || path.contains(['*', '?', '[', ']', '{', '}', '\\']) {
        return Err(invalid(format!(
            "'{path}': inode-only paths must be exact absolute literals, not globs; use full-path mode for patterns"
        )));
    }
    if path.len() > 126 {
        return Err(invalid(format!(
            "'{path}': inode path exceeds the 126-byte event limit; use a shorter canonical path (the compiler also has a smaller pattern limit)"
        )));
    }
    let canonical = fs::canonicalize(path).map_err(|error| invalid(format!(
        "'{path}': cannot resolve inode path: {error}; create an existing regular file before applying the policy"
    )))?;
    // Compare raw spelling, since Path equality normalizes redundant separators.
    if canonical.as_os_str() != OsStr::new(path) {
        return Err(invalid(format!(
            "'{path}': inode path is not canonical; use '{}'",
            canonical.display()
        )));
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| invalid(format!("'{path}': cannot hold inode with O_PATH: {error}")))?;
    let metadata = file
        .metadata()
        .map_err(|error| invalid(format!("'{path}': cannot stat held inode: {error}")))?;
    if !metadata.is_file() {
        return Err(invalid(format!(
            "'{path}': inode paths must name existing regular files"
        )));
    }
    Ok(InodeFile {
        ino: metadata.ino(),
        dev: userspace_dev_to_kernel(metadata.dev()),
        path: path.into(),
        rule_id,
        _file: Arc::new(file),
    })
}

// Translate glibc's encoded st_dev into the kernel's MKDEV(major, minor).
fn userspace_dev_to_kernel(dev: u64) -> u32 {
    (libc::major(dev) << 20) | libc::minor(dev)
}

fn invalid(message: impl Into<String>) -> BackendError {
    BackendError::CompileFailure(message.into())
}

fn registration_error(action: &str, file: &InodeFile, error: io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("{action} '{}': {error}", file.path))
}

#[cfg(test)]
mod tests {
    use std::os::fd::AsRawFd;
    use std::path::PathBuf;

    use actplane_ifc_compiler::compile_str;
    use agentsight_enforcement_protocol::{
        CredentialExfiltrationPolicy, DestinationScope, PolicyMode,
    };
    use uuid::Uuid;

    use super::*;

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            // Keep paths below the compiler's 63-byte pattern ABI even when
            // the test runner exports a long TMPDIR.
            let dir = fs::canonicalize("/tmp")
                .unwrap()
                .join(format!("ai-{}", Uuid::new_v4().simple()));
            fs::create_dir(&dir).unwrap();
            Self(dir)
        }

        fn path(&self, name: &str) -> String {
            self.0.join(name).to_str().unwrap().to_owned()
        }

        fn file(&self, name: &str) -> String {
            let path = self.path(name);
            fs::write(&path, "fixture").unwrap();
            path
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    fn prepare(dsl: &str) -> Result<InodePolicy, BackendError> {
        InodePolicy::prepare(
            dsl,
            &compile_str(dsl).map_err(BackendError::CompileFailure)?,
            true,
        )
    }

    fn guard_dsl(path: &str, condition: &str) -> String {
        format!(
            "source AGENT = exec \"**\"\nrule protect:\n block unlink file \"{path}\" {condition}\n because \"keep file\"\n"
        )
    }

    #[test]
    fn lowered_index_accounts_for_dnf_and_clause_offset_and_first_guard_wins() {
        let fixture = Fixture::new();
        let path = fixture.file("p");
        let dsl = format!(
            "source AGENT = exec \"**\"\nsource OTHER = exec \"**\"\n\
             rule prior:\n notify connect endpoint \"*\" if AGENT or OTHER\n because \"prior\"\n\
             rule protect:\n notify connect endpoint \"*\"\n block write file \"{path}\" if AGENT\n because \"keep file\"\n\
             rule later:\n block unlink file \"{path}\"\n because \"later\"\n"
        );
        let compiled = compile_str(&dsl).unwrap();
        assert_eq!(compiled.meta[3].name, "protect");
        assert_eq!(compiled.meta[3].clause_source_index, 1);
        let prepared = InodePolicy::prepare(&dsl, &compiled, true).unwrap();
        assert_eq!(prepared.files.len(), 1);
        assert_eq!(prepared.files[0].rule_id, Some(3));
        assert_eq!(prepared.files[0].path, path);
    }

    #[test]
    fn multiline_exceptions_and_complex_conditions_fail_closed() {
        let fixture = Fixture::new();
        let path = fixture.file("p");
        for condition in [
            "if AGENT\n unless target \"/safe\"",
            "if AGENT\n and OTHER",
            "if AGENT or OTHER",
            "if AGENT and not OTHER",
            "if CREDENTIAL",
        ] {
            let error = prepare(&guard_dsl(&path, condition)).unwrap_err();
            assert!(
                error.to_string().contains("inode-only delete protection"),
                "{error}"
            );
        }
        for condition in ["", "if true", "if AGENT"] {
            assert!(prepare(&guard_dsl(&path, condition)).is_ok());
        }
        let dsl = guard_dsl(&path, "");
        assert!(prepare(&dsl.replace("block", "kill")).is_err());
        assert!(prepare(&dsl.replace("unlink file", "unlink endpoint")).is_err());
        assert!(
            prepare(&dsl.replace("block", "notify"))
                .unwrap()
                .files
                .is_empty()
        );
        let command = format!(
            "source COMMAND = exec \"**\"\n{}",
            guard_dsl(&path, "if AGENT")
        );
        assert!(
            prepare(&command)
                .unwrap_err()
                .to_string()
                .contains("if COMMAND")
        );
        assert!(prepare(&command.replace("if AGENT", "if COMMAND")).is_ok());
    }

    #[test]
    fn guarded_paths_reject_wrapped_non_agent_labels() {
        let fixture = Fixture::new();
        let source = fixture.file("secret");
        let first = fixture.file("a");
        let second = fixture.file("b");
        let sources = format!("source AGENT = exec \"**\"\nsource SECRET = file \"{source}\"\n");
        for clauses in [
            format!("block unlink file \"{first}\"\n if SECRET"),
            format!("block unlink file \"{first}\"\n if\n SECRET"),
            format!("block unlink file \"{first}\" unless target \"/safe\""),
            format!(
                "block unlink file \"{first}\" if AGENT\n block write file \"{second}\" if SECRET"
            ),
        ] {
            let dsl = format!("{sources}rule r:\n {clauses}\n because \"x\"\n");
            let error = prepare(&dsl).unwrap_err();
            assert!(
                error.to_string().contains("inode-only delete protection"),
                "{error}"
            );
        }
        let malformed = guard_dsl(&first, "unless AGENT");
        assert!(
            prepare(&malformed)
                .unwrap_err()
                .to_string()
                .contains("unknown unless cond 'AGENT'")
        );
        let prepared = prepare(&guard_dsl(&first, "if AGENT")).unwrap();
        assert_eq!(prepared.files.len(), 1);
        assert_eq!(prepared.files[0].path, first);
        assert_eq!(prepared.files[0].rule_id, Some(0));
    }

    #[test]
    fn credential_source_registers_only_a_path_and_keeps_inode_alive() {
        let fixture = Fixture::new();
        let path = fixture.file("cred");
        let policy = CredentialExfiltrationPolicy {
            policy_id: "credentials".into(),
            revision: 1,
            source_patterns: vec![path.clone()],
            trusted_endpoints: vec![],
            taint_label: "CREDENTIAL".into(),
            taint_ttl_secs: 900,
            destination_scope: DestinationScope::PublicIpv4,
            mode: PolicyMode::Audit,
        };
        let dsl = super::super::compile_credential_exfiltration_policy(&policy).unwrap();
        assert!(dsl.contains("notify connect endpoint"));
        let prepared = prepare(&dsl).unwrap();
        assert_eq!(prepared.files.len(), 1);
        assert_eq!(prepared.files[0].rule_id, None);
        let mut registered = Vec::new();
        prepared
            .populate(
                |file| {
                    registered.push(file.path.clone());
                    Ok(())
                },
                |_, _| panic!("source must not get a delete guard"),
            )
            .unwrap();
        assert_eq!(registered, vec![path.clone()]);
        let active = prepared.clone();
        drop(prepared);
        fs::remove_file(&path).unwrap();
        assert_eq!(
            active.files[0]._file.metadata().unwrap().ino(),
            active.files[0].ino
        );
        // SAFETY: fcntl only queries flags on the live descriptor held by active.
        let flags = unsafe { libc::fcntl(active.files[0]._file.as_raw_fd(), libc::F_GETFL) };
        assert_ne!(flags & libc::O_PATH, 0);
    }

    #[test]
    fn required_paths_reject_missing_glob_noncanonical_and_nonregular_files() {
        let fixture = Fixture::new();
        let path = fixture.file("p");
        std::os::unix::fs::symlink(&path, fixture.path("link")).unwrap();
        for bad in [
            fixture.path("missing"),
            fixture.path("*"),
            fixture.path("?"),
            fixture.path("[p]"),
            fixture.path("./p"),
            fixture.path("link"),
            fixture.path(""),
            "relative".into(),
        ] {
            assert!(prepare(&guard_dsl(&bad, "if AGENT")).is_err(), "{bad}");
            let source =
                format!("source AGENT = exec \"**\"\nsource CREDENTIAL = file \"{bad}\"\n");
            assert!(prepare(&source).is_err(), "source: {bad}");
        }
        let error = pin_file(&format!("/{}", "a".repeat(126)), None).unwrap_err();
        assert!(error.to_string().contains("126-byte"));
    }

    #[test]
    fn hardlink_aliases_fail_even_between_source_and_guard() {
        let fixture = Fixture::new();
        let path = fixture.file("p");
        let alias = fixture.path("alias");
        fs::hard_link(&path, &alias).unwrap();
        let dsl = format!(
            "source CREDENTIAL = file \"{alias}\"\n{}",
            guard_dsl(&path, "")
        );
        assert!(
            prepare(&dsl)
                .unwrap_err()
                .to_string()
                .contains("ambiguous inode aliases")
        );
        let dsl = dsl.replace(&alias, &path);
        let prepared = prepare(&dsl).unwrap();
        assert_eq!(prepared.files.len(), 1);
        assert_eq!(prepared.files[0].rule_id, Some(0));
    }

    #[test]
    fn ambiguous_or_inconsistent_lowered_metadata_is_rejected() {
        let fixture = Fixture::new();
        let path = fixture.file("p");
        let dsl = guard_dsl(&path, "");
        let duplicate =
            format!("{dsl}rule protect:\n block unlink file \"{path}\"\n because \"other\"\n");
        assert!(prepare(&duplicate).is_err());
        let mut compiled = compile_str(&dsl).unwrap();
        compiled.meta.push(compiled.meta[0].clone());
        assert!(
            InodePolicy::prepare(&dsl, &compiled, true)
                .unwrap_err()
                .to_string()
                .contains("unambiguous")
        );
        let mut compiled = compile_str(&dsl).unwrap();
        compiled.meta[0].target_pattern = "/wrong".into();
        assert!(InodePolicy::prepare(&dsl, &compiled, true).is_err());
        let mut compiled = compile_str(&dsl).unwrap();
        compiled.meta[0].kernel_op = "read".into();
        assert!(InodePolicy::prepare(&dsl, &compiled, true).is_err());
        let mut compiled = compile_str(&dsl).unwrap();
        compiled.reasons[0] = "wrong reason".into();
        assert!(InodePolicy::prepare(&dsl, &compiled, true).is_err());
    }

    #[test]
    fn full_path_mode_leaves_glob_and_conditional_semantics_to_compiler() {
        let dsl = format!(
            "source CREDENTIAL = file \"/missing/**\"\n{}",
            guard_dsl("/missing/*", "if AGENT or CREDENTIAL")
        );
        let compiled = compile_str(&dsl).unwrap();
        assert!(
            InodePolicy::prepare(&dsl, &compiled, false)
                .unwrap()
                .files
                .is_empty()
        );
    }

    #[test]
    fn map_registration_failures_propagate_with_path_context() {
        let fixture = Fixture::new();
        let path = fixture.file("p");
        let prepared = prepare(&guard_dsl(&path, "")).unwrap();
        let error = prepared
            .populate(
                |_| Err(io::Error::other("map full")),
                |_, _| panic!("guard must not run after failed path registration"),
            )
            .unwrap_err();
        assert!(error.to_string().contains(&path));
        assert!(error.to_string().contains("register inode path"));
        let error = prepared
            .populate(|_| Ok(()), |_, _| Err(io::Error::other("map full")))
            .unwrap_err();
        assert!(error.to_string().contains("guard inode"));
        assert!(error.to_string().contains(&path));
    }

    #[test]
    fn device_encoding_preserves_large_minor_numbers() {
        for (major, minor) in [(0, 0), (8, 1), (259, 0xabcde), (0xfff, 0xfffff)] {
            assert_eq!(
                userspace_dev_to_kernel(libc::makedev(major, minor)),
                (major << 20) | minor
            );
        }
    }
}
