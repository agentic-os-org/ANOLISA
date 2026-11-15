//! Launch baseline derived from the launch itself, not from a per-machine file.
//!
//! A command candidate only states what the command line names literally: it
//! never contains the interpreter, the loader or the working directory. The
//! daemon requires both the resolved program and the cwd to be readable before it
//! authorizes a launch, so those facts have to be supplied somewhere. They are
//! exactly the facts `prepare_launch` already computed, which makes them derivable
//! on any machine instead of being maintained as a checked-in preset.
//!
//! The baseline grants read access to the fixed system directories a shell needs
//! and to the launch program's own directory, plus process start. It grants no
//! write right, so it can never widen a candidate's write scope.
//!
//! Every root is emitted as its canonical path. On a merged-`/usr` system `/bin`,
//! `/lib` and friends are symlinks, and the subtree grants name the link targets.
//! The link names are still needed inside the sandbox: an ELF interpreter such as
//! `/lib64/ld-linux-x86-64.so.2` and `#!/bin/sh` shebangs are resolved through
//! them, and without the link every dynamic program fails `execve` with `ENOENT`.
//! So a symlinked system root is also granted as a point, which the sandbox
//! recreates as a link to its readable target.

use std::path::Path;

use asc_sandbox_types::LaunchSpec;

/// System directories every `/bin/bash -c` launch needs to read.
const SYSTEM_READ_ROOTS: &[&str] = &[
    "/bin",
    "/usr/bin",
    "/sbin",
    "/usr/sbin",
    "/lib",
    "/lib64",
    "/usr/lib",
    "/usr/lib64",
    "/etc",
];

/// Renders the baseline as Cedar in the deployment shape the sandbox projects.
///
/// The text is a plain permission candidate: it carries bare `principal` headers
/// and no context, so it unions cleanly with an intent candidate. Callers combine
/// it with [`super::PermissionInput::with_runtime`].
#[must_use]
pub fn cedar(launch: &LaunchSpec) -> String {
    let program_dir = parent(&launch.resolved_program);
    // A missing system root needs no grant; a missing cwd or program directory is
    // kept literally so the daemon reports the launch error instead of the baseline.
    let mut roots: Vec<String> = Vec::new();
    let links: Vec<&str> = SYSTEM_READ_ROOTS
        .iter()
        .copied()
        .filter(|root| {
            std::fs::symlink_metadata(root).is_ok_and(|meta| meta.file_type().is_symlink())
                && canonical(root).is_some()
        })
        .collect();
    let system = SYSTEM_READ_ROOTS.iter().filter_map(|root| canonical(root));
    let launch_roots = [launch.cwd.as_str(), program_dir.as_str()]
        .into_iter()
        .map(|root| canonical(root).unwrap_or_else(|| root.to_owned()));
    for root in system.chain(launch_roots) {
        if !roots.contains(&root) {
            roots.push(root);
        }
    }
    let mut clauses = Vec::with_capacity(roots.len() * 2);
    for root in &roots {
        let escaped = escape(root);
        clauses.push(format!("resource.path == \"{escaped}\""));
        if root != "/" {
            clauses.push(format!("resource.path like \"{escaped}/*\""));
        }
    }
    for link in links {
        clauses.push(format!("resource.path == \"{}\"", escape(link)));
    }
    let body = clauses.join(" ||\n  ");
    format!(
        r#"// Launch baseline: execution facts only, no write right.
permit(
  principal,
  action == SecCore::Action::"file.read",
  resource is SecCore::File
)
when {{
  {body}
}};

permit(
  principal,
  action == SecCore::Action::"process.spawn",
  resource is SecCore::Process
);

permit(
  principal,
  action == SecCore::Action::"file.exec",
  resource is SecCore::File
);
"#
    )
}

/// Parses the baseline into a candidate that can be unioned with a command candidate.
///
/// # Errors
/// Returns the parser error when the generated text is not accepted.
pub fn input(launch: &LaunchSpec) -> Result<super::PermissionInput, super::PermissionInputError> {
    super::PermissionInput::parse(&cedar(launch))
}

/// Resolves a root to the path the sandbox mounts, or `None` when it does not exist.
fn canonical(path: &str) -> Option<String> {
    std::fs::canonicalize(Path::new(path))
        .ok()?
        .to_str()
        .map(str::to_owned)
}

/// Parent directory of a resolved program, or the program itself at the root.
fn parent(program: &str) -> String {
    match program.rfind('/') {
        Some(0) | None => "/".into(),
        Some(index) => program[..index].to_owned(),
    }
}

/// Escapes a path for a Cedar string literal.
fn escape(path: &str) -> String {
    path.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn launch(program: &str, cwd: &str) -> LaunchSpec {
        LaunchSpec {
            program: program.into(),
            resolved_program: program.into(),
            args: vec!["-c".into(), "true".into()],
            cwd: cwd.into(),
            env: BTreeMap::new(),
        }
    }

    #[test]
    fn baseline_names_the_launch_program_cwd_and_system_roots() {
        let text = cedar(&launch("/usr/bin/bash", "/srv/work"));
        assert!(text.contains("resource.path == \"/usr/bin\""));
        assert!(text.contains("resource.path like \"/usr/bin/*\""));
        assert!(text.contains("resource.path == \"/srv/work\""));
        assert!(text.contains("resource.path like \"/srv/work/*\""));
        assert!(text.contains("process.spawn"));
        assert!(text.contains("file.exec"));
    }

    #[test]
    fn baseline_never_grants_write_or_context() {
        let text = cedar(&launch("/usr/bin/python3", "/srv/work"));
        assert!(!text.contains("file.write"));
        assert!(!text.contains("context."));
    }

    #[test]
    fn baseline_parses_as_a_candidate() {
        let text = cedar(&launch("/opt/py/bin/python3", "/srv/work"));
        assert!(text.contains("resource.path == \"/opt/py/bin\""));
        assert!(super::super::PermissionInput::parse(&text).is_ok());
    }

    #[test]
    fn root_program_does_not_emit_a_wildcard_root() {
        let text = cedar(&launch("/bash", "/srv/work"));
        assert!(!text.contains("resource.path like \"//*\""));
    }

    #[test]
    fn symlinked_roots_are_emitted_as_their_targets() {
        let base = std::env::temp_dir().join(format!("asc-baseline-{}", std::process::id()));
        let target = base.join("real");
        let link = base.join("link");
        std::fs::create_dir_all(&target).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let text = cedar(&launch(
            &format!("{}/tool", link.display()),
            link.to_str().unwrap(),
        ));
        let target = std::fs::canonicalize(&target).unwrap();
        std::fs::remove_dir_all(&base).unwrap();
        assert!(text.contains(&format!("resource.path == \"{}\"", target.display())));
        assert!(!text.contains(link.to_str().unwrap()));
        // A symlinked system root is granted as a point only; its subtree is the target's.
        for root in SYSTEM_READ_ROOTS {
            if std::fs::symlink_metadata(root).is_ok_and(|meta| meta.file_type().is_symlink()) {
                assert!(
                    text.contains(&format!("resource.path == \"{root}\"")),
                    "{root}"
                );
                assert!(
                    !text.contains(&format!("resource.path like \"{root}/*\"")),
                    "{root}"
                );
            }
        }
    }
}
