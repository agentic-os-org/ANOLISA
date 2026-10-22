//! Verify installed files through bounded, no-follow descriptors and report all failures.

use super::{paths, read, Installation, Inventory, InventorySource, Manager, Registration};
use crate::{Error, Payload, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::Read,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

const MAX_FILE_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 4 * MAX_FILE_BYTES;

/// Stable file-verification finding category.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DoctorIssueCode {
    /// A recorded file or directory no longer exists.
    Missing,
    /// Installed permission bits differ from the record.
    ModeMismatch,
    /// Installed content differs from the recorded SHA-256.
    DigestMismatch,
    /// A symbolic link prevents safe traversal.
    Symlink,
    /// A recorded payload is a directory or special file.
    Nonregular,
    /// A read or metadata operation failed.
    Io,
    /// An installation directory or management lock has unsafe ownership or permissions.
    Directory,
    /// An exact dependency target is absent from the recorded inventory.
    Dependency,
    /// A file changed during verification, invalidating the observation.
    ChangedDuringRead,
    /// Verification exceeded the per-file or total byte budget.
    VerificationLimit,
    /// Native package verification requires the corresponding native manager.
    NativeManager,
}

/// One failure; subsequent files are still checked.
#[derive(Clone, Debug, Serialize)]
pub struct DoctorIssue {
    /// Stable reason suitable for JSON consumers.
    pub code: DoctorIssueCode,
    /// Record containing the file or dependency.
    pub installation_id: String,
    /// Package name when a package-specific check failed.
    pub package_id: Option<String>,
    /// File or installation root affected by the finding.
    pub path: PathBuf,
    /// Diagnostic without executing the package or reading its configuration.
    pub message: String,
}

/// File integrity observations, kept separate from installed-package records.
#[derive(Debug, Serialize)]
pub struct DoctorReport {
    /// Selected package-record database.
    pub database_path: PathBuf,
    /// Authority from which the records were read.
    pub source: InventorySource,
    /// Number of installation roots examined.
    pub installations_checked: usize,
    /// Number of package records examined.
    pub packages_checked: usize,
    /// Number of recorded file paths attempted, including missing files.
    pub files_checked: usize,
    /// Every independent integrity or safety failure observed.
    pub issues: Vec<DoctorIssue>,
}

impl DoctorReport {
    /// Whether every attempted check succeeded and no native-manager check is outstanding.
    pub fn healthy(&self) -> bool {
        self.issues.is_empty()
    }
}

pub(super) fn check(inventory: &Inventory) -> Result<DoctorReport> {
    read::recheck(inventory)?;
    let mut report = DoctorReport {
        database_path: inventory.database_path.clone(),
        source: inventory.source,
        installations_checked: 0,
        packages_checked: 0,
        files_checked: 0,
        issues: Vec::new(),
    };
    let mut budget = MAX_TOTAL_BYTES;
    for installation in &inventory.installations {
        report.installations_checked += 1;
        let root_issue = match paths::directory(&installation.prefix) {
            Ok(root) => {
                let metadata = root.metadata()?;
                (metadata.uid() != installation.owner_uid || metadata.mode() & 0o022 != 0)
                    .then_some((
                        DoctorIssueCode::Directory,
                        "installation root ownership or permissions changed".to_string(),
                    ))
            }
            Err(error) => Some((category(&error), error.to_string())),
        };
        if let Some((code, message)) = root_issue {
            issue(
                &mut report,
                installation,
                None,
                &installation.prefix,
                code,
                message,
            );
        }
        let _prefix_lock = if installation.registration == Registration::Registered
            && installation.manager == Manager::Raw
        {
            metadata(inventory, installation, &mut report)
        } else {
            None
        };
        for package in &installation.packages {
            report.packages_checked += 1;
            for dependency in &package.dependencies {
                if !inventory._dependency_targets.contains(&(
                    dependency.installation_id.clone(),
                    dependency.package_id.clone(),
                    dependency.version.clone(),
                )) {
                    issue(
                        &mut report,
                        installation,
                        Some(&package.package_id),
                        &installation.prefix,
                        DoctorIssueCode::Dependency,
                        format!(
                            "missing recorded dependency {}:{}@{}",
                            dependency.installation_id, dependency.package_id, dependency.version
                        ),
                    );
                }
            }
            if installation.manager != Manager::Raw {
                issue(&mut report, installation, Some(&package.package_id), &installation.prefix, DoctorIssueCode::NativeManager,
                    "native package verification is not available; consult the owning package manager".into());
                continue;
            }
            for (name, expected) in &package.files {
                report.files_checked += 1;
                let path = installation.prefix.join(name);
                match file(&path, expected, &mut budget) {
                    Ok(findings) => {
                        for (code, message) in findings {
                            issue(
                                &mut report,
                                installation,
                                Some(&package.package_id),
                                &path,
                                code,
                                message,
                            );
                        }
                    }
                    Err(error) => issue(
                        &mut report,
                        installation,
                        Some(&package.package_id),
                        &path,
                        category(&error),
                        error.to_string(),
                    ),
                }
            }
        }
    }
    read::recheck(inventory)?;
    Ok(report)
}

fn metadata(
    inventory: &Inventory,
    installation: &Installation,
    report: &mut DoctorReport,
) -> Option<File> {
    let paths = super::DatabasePaths {
        root: inventory
            .database_path
            .parent()
            .unwrap_or(Path::new("/"))
            .into(),
        scope: installation.scope,
    };
    let marker = installation.prefix.join(".aw-packages/aw-core.json");
    if let Err(error) = super::registration::require_registered(
        &paths,
        &installation.prefix,
        &installation.installation_id,
    ) {
        let code = if matches!(&error, Error::Invalid(_)) {
            DoctorIssueCode::Directory
        } else {
            category(&error)
        };
        issue(report, installation, None, &marker, code, error.to_string());
    }
    // Listing may retain central facts after metadata loss. A raw installation cannot be
    // called healthy without the locator and lock that make future mutations safe.
    let lock_path = installation.prefix.join(".aw-install.lock");
    match paths::lock(&lock_path) {
        Ok(lock) => match lock.metadata() {
            Ok(metadata)
                if metadata.uid() == installation.owner_uid && metadata.mode() & 0o022 == 0 =>
            {
                Some(lock)
            }
            Ok(_) => {
                issue(
                    report,
                    installation,
                    None,
                    &lock_path,
                    DoctorIssueCode::Directory,
                    "installation lock ownership or permissions changed".into(),
                );
                None
            }
            Err(error) => {
                issue(
                    report,
                    installation,
                    None,
                    &lock_path,
                    DoctorIssueCode::Io,
                    error.to_string(),
                );
                None
            }
        },
        Err(error) => {
            issue(
                report,
                installation,
                None,
                &lock_path,
                category(&error),
                error.to_string(),
            );
            None
        }
    }
}

fn issue(
    report: &mut DoctorReport,
    installation: &Installation,
    package: Option<&str>,
    path: &Path,
    code: DoctorIssueCode,
    message: String,
) {
    report.issues.push(DoctorIssue {
        code,
        installation_id: installation.installation_id.clone(),
        package_id: package.map(str::to_owned),
        path: path.to_path_buf(),
        message,
    });
}

fn category(error: &Error) -> DoctorIssueCode {
    match error {
        Error::Io(error) if error.kind() == std::io::ErrorKind::NotFound => {
            DoctorIssueCode::Missing
        }
        Error::Io(error) if matches!(error.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR)) => {
            DoctorIssueCode::Symlink
        }
        _ => DoctorIssueCode::Io,
    }
}

fn file(
    path: &Path,
    expected: &Payload,
    budget: &mut u64,
) -> Result<Vec<(DoctorIssueCode, String)>> {
    let mut file = paths::opened(path)?;
    let before = file.metadata()?;
    if !before.is_file() {
        return Ok(vec![(
            DoctorIssueCode::Nonregular,
            "recorded payload is not a regular file".into(),
        )]);
    }
    if before.len() > MAX_FILE_BYTES || before.len() > *budget {
        return Ok(vec![(
            DoctorIssueCode::VerificationLimit,
            "file exceeds the verification byte budget".into(),
        )]);
    }
    let mut findings = Vec::new();
    if before.mode() & 0o7777 != expected.mode {
        findings.push((
            DoctorIssueCode::ModeMismatch,
            format!(
                "expected mode {:04o}, found {:04o}",
                expected.mode,
                before.mode() & 0o7777
            ),
        ));
    }
    let limit = MAX_FILE_BYTES.min(*budget);
    let mut hash = Sha256::new();
    let mut count = 0_u64;
    let mut buffer = [0_u8; 65536];
    loop {
        let length = file.read(&mut buffer)?;
        if length == 0 {
            break;
        }
        count += length as u64;
        if count > limit {
            *budget = budget.saturating_sub(count);
            findings.push((
                DoctorIssueCode::VerificationLimit,
                "file grew beyond the verification byte budget".into(),
            ));
            return Ok(findings);
        }
        hash.update(&buffer[..length]);
    }
    *budget -= count;
    let after = file.metadata()?;
    if before.len() != after.len()
        || before.mtime() != after.mtime()
        || before.mtime_nsec() != after.mtime_nsec()
        || before.ctime() != after.ctime()
        || before.ctime_nsec() != after.ctime_nsec()
        || paths::same(path, &after).is_err()
    {
        findings.push((
            DoctorIssueCode::ChangedDuringRead,
            "file changed while checking; retry verification".into(),
        ));
    } else if format!("{:x}", hash.finalize()) != expected.sha256 {
        findings.push((
            DoctorIssueCode::DigestMismatch,
            "installed content does not match recorded SHA-256".into(),
        ));
    }
    Ok(findings)
}
