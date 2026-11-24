//! Read-only Linux shell dependencies for a single prepared Hook invocation.

use std::collections::BTreeSet;
use std::fs;
use std::io::ErrorKind;

use asc_daemon_protocol::authorization::LaunchSpec;

use super::{PermissionInput, PermissionInputError};

const SYSTEM_DIRS: &[&str] = &[
    "/bin",
    "/usr/bin",
    "/lib",
    "/usr/lib",
    "/lib64",
    "/usr/lib64",
];

fn inspect(path: &str, error: impl std::fmt::Display) -> PermissionInputError {
    PermissionInputError::Unsupported(format!("cannot bind launch dependency {path}: {error}"))
}

pub(super) fn input(launch: &LaunchSpec) -> Result<PermissionInput, PermissionInputError> {
    let mut points = BTreeSet::from([launch.cwd.clone(), launch.resolved_program.clone()]);
    let mut trees = BTreeSet::new();
    for path in SYSTEM_DIRS.iter().copied().chain(["/etc/ld.so.cache"]) {
        match fs::symlink_metadata(path) {
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => return Err(inspect(path, error)),
            Ok(_) => {}
        }
        let target = fs::canonicalize(path).map_err(|e| inspect(path, e))?;
        let canonical = target
            .to_str()
            .ok_or_else(|| inspect(path, "non-UTF-8 target"))?;
        let metadata = fs::metadata(&target).map_err(|e| inspect(path, e))?;
        if path == "/etc/ld.so.cache" {
            if !metadata.is_file() {
                return Err(inspect(path, "expected a loader cache file"));
            }
        } else {
            if !metadata.is_dir() || !SYSTEM_DIRS.contains(&canonical) {
                return Err(inspect(
                    path,
                    format!("unexpected system directory {canonical}"),
                ));
            }
            trees.insert(canonical.to_owned());
        }
        // The loader and shebangs use the original link name inside the namespace.
        points.insert(path.to_owned());
        points.insert(canonical.to_owned());
    }
    let mut predicates = Vec::new();
    for path in points {
        asc_sandbox_types::validate_os_path(&path).map_err(|error| inspect(&path, error))?;
        predicates.push(format!(
            "resource.path == {}",
            serde_json::to_string(&path)?
        ));
    }
    for path in trees {
        predicates.push(format!(
            "resource.path like {}",
            serde_json::to_string(&format!("{path}/*"))?
        ));
    }
    let read = predicates.join(" || ");
    let program = serde_json::to_string(&launch.program)?;
    let resolved = serde_json::to_string(&launch.resolved_program)?;
    PermissionInput::parse(&format!(
        r#"permit(principal, action == SecCore::Action::"file.read", resource is SecCore::File)
           when {{ {read} }};
           permit(principal, action == SecCore::Action::"process.spawn", resource is SecCore::Process)
           when {{ resource.program == {program} && resource.resolved_program == {resolved} }};"#
    ))
}
