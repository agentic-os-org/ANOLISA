//! Read-only package-manager file inventory contract.
//!
//! Lifecycle package queries answer whether a package is installed; adapter
//! integrity additionally needs the package manager's authoritative per-file
//! metadata. Keeping that surface separate avoids forcing transaction/version
//! fakes to pretend they can query file ownership.

use crate::pkg_query::PackageQueryError;

/// Digest algorithm declared by the package inventory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageFileDigestAlgorithm {
    /// SHA-256 (RPM algorithm id 8).
    Sha256,
    /// An algorithm ANOLISA does not verify.
    Unsupported(u32),
}

/// Package-manager file type derived from the authoritative mode metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageFileKind {
    /// Regular file whose bytes can be hashed.
    Regular,
    /// Symbolic link whose literal target can be compared.
    Symlink,
    /// Directory; ownership/integrity is handled by the lifecycle layer.
    Directory,
    /// Device, socket, FIFO, or another non-adapter-input type.
    Other,
}

/// One file entry reported by the native package database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageFile {
    /// Absolute installed path.
    pub path: String,
    /// File type derived from the package-recorded Unix mode.
    pub kind: PackageFileKind,
    /// Package-recorded content digest. Directories and symlinks normally
    /// carry no digest.
    pub digest: Option<String>,
    /// Literal link target for [`PackageFileKind::Symlink`].
    pub link_target: Option<String>,
}

/// Authoritative file inventory for one installed native package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageFileInventory {
    /// Digest algorithm shared by the package's per-file digests.
    pub digest_algorithm: PackageFileDigestAlgorithm,
    /// Package-owned paths and their metadata.
    pub files: Vec<PackageFile>,
}

/// Read-only package file query, intentionally separate from version and
/// transaction interfaces.
pub trait PackageFileQuery: Send + Sync {
    /// Return the installed package's authoritative file inventory.
    ///
    /// # Errors
    ///
    /// Returns [`PackageQueryError`] when the package database cannot be
    /// queried or its output is malformed. Package absence is an error here:
    /// callers already hold a tracked installation and must surface the drift.
    fn query_file_inventory(
        &self,
        package: &str,
    ) -> Result<PackageFileInventory, PackageQueryError>;
}
