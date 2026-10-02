//! Install runner: copy a cached artifact into the ANOLISA-owned layout.
//!
//! This runner supports `tar_gz` artifacts. It decodes a gzipped tar archive
//! once, streaming every entry the component contract selects into a private
//! staging directory under `cache_dir`, then places each staged payload at its
//! destination. Payload bytes are never retained in memory for the whole
//! archive: peak memory tracks the fixed streaming buffer plus per-entry
//! metadata, not the uncompressed artifact size.
//!
//! All destinations must resolve under one of the ANOLISA-owned roots
//! (`bin_dir`, `etc_dir`, `state_dir`, `lib_dir`, `libexec_dir`, `datadir`,
//! `log_dir`, `cache_dir`). Anything else is rejected as
//! `InstallError::ExternalPath`. The runner refuses to modify or even
//! create files outside those roots, so a failed install can roll back by
//! deleting just the paths it returns.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use anolisa_platform::fs_layout::FsLayout;
use flate2::read::GzDecoder;
use fs2::FileExt;
use sha2::{Digest, Sha256};
use tar::Archive;
use tempfile::TempDir;

use crate::manifest::FileKind;

/// Fixed copy-buffer size for archive-to-staging and staging-to-destination
/// streaming. The only payload memory the runner holds at any instant.
const STREAM_BUF_SIZE: usize = 64 * 1024;

/// Prefix for private per-preparation directories under [`FsLayout::cache_dir`].
/// Staging stays out of the system temp dir, which is frequently a memory-backed
/// tmpfs in containers, without adding a fixed single-owner directory beneath a
/// cache that may be shared by several users.
const STAGING_DIR_PREFIX: &str = "install-prepare-";

/// Advisory lock held for the lifetime of one staging directory.
const STAGING_LOCK_FILE: &str = ".lock";

/// Undiscoverable lock name used until the new directory is safely locked.
const STAGING_LOCK_PENDING_FILE: &str = ".lock.pending";

/// Wire-form `artifact_type` strings accepted by the raw install runner.
///
/// Keep this in sync with [`InstallRunner::prepare_files`]; the CLI resolver
/// uses the same list to reject unsupported artifacts before downloading them.
pub const SUPPORTED_ARTIFACT_TYPES: &[&str] = &["tar_gz"];

/// One destination file written by the runner, with the sha256 of the
/// installed bytes, recorded in the owned installation state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledFile {
    /// Absolute destination path actually written.
    pub path: PathBuf,
    /// Role from the mapping that supplied this file, before source provenance is lost.
    pub kind: FileKind,
    /// Mode applied from that mapping or archive entry; `None` uses the runner default.
    pub mode: Option<String>,
    /// Lowercase-hex sha256 of the installed bytes. Empty for symlink
    /// entries (they record a [`referent`](Self::referent) instead).
    pub sha256: String,
    /// For managed symlinks: the absolute referent path the link points at.
    /// `None` for regular files.
    pub referent: Option<PathBuf>,
}

/// Source-to-destination mapping after manifest layout substitution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedInstallFile {
    /// Optional archive entry path. `None` means match by destination
    /// basename for backward-compatible manifests. For
    /// [`FileKind::Symlink`] entries this is the link's referent — an
    /// absolute layout-expanded path, not an archive member.
    pub source: Option<String>,
    /// Absolute destination after layout-template substitution.
    pub dest: PathBuf,
    /// Optional Unix file mode from the component manifest, e.g. `"0644"`.
    pub mode: Option<String>,
    /// File role; [`FileKind::Symlink`] entries are created after the
    /// regular files instead of being extracted from the artifact.
    pub kind: FileKind,
    /// Content-rendering request from the manifest `render` key. `None`
    /// places the archive bytes verbatim. Rendering runs before the
    /// installed sha256 is computed, so state records the bytes that
    /// actually land on disk.
    pub render: Option<RenderSpec>,
}

/// Content-rendering request carried on a [`ResolvedInstallFile`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderSpec {
    /// Versioned rendering mode resolved from the manifest `render` value.
    pub mode: RenderMode,
    /// Component name substituted for the `{component}` placeholder,
    /// mirroring destination-path expansion.
    pub component: String,
}

/// Supported file content-rendering modes.
///
/// Each variant corresponds to one versioned manifest `render` value; the
/// install resolver rejects values with no variant here so an unsupported
/// contract fails closed instead of installing unrendered placeholders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderMode {
    /// [`RENDER_ANOLISA_PATHS_V1`](crate::manifest::RENDER_ANOLISA_PATHS_V1):
    /// substitute layout placeholders (`{bindir}`, `{datadir}`, … plus
    /// `{component}`) inside UTF-8 file content against the final layout.
    AnolisaPathsV1,
}

impl RenderMode {
    /// Map a manifest `render` string to a supported mode. Returns `None`
    /// for values this CLI does not implement — callers must reject those.
    pub fn parse(value: &str) -> Option<Self> {
        (value == crate::manifest::RENDER_ANOLISA_PATHS_V1).then_some(Self::AnolisaPathsV1)
    }
}

/// Aggregate result of a single [`InstallRunner::install_prepared`] call.
#[derive(Debug, Clone)]
pub struct InstallOutcome {
    /// One entry per destination written, in prepared-file order.
    pub files: Vec<InstalledFile>,
}

/// Failure modes for artifact preparation and installation.
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    /// Artifact backend is not implemented by this milestone's runner.
    #[error("artifact_type '{0}' is not supported by this milestone (only 'tar_gz')")]
    UnsupportedArtifactType(String),

    /// Manifest resolved to no destination files.
    #[error("manifest must declare at least one destination file")]
    NoDestinations,

    /// Symlink layout entry lacks a `source` (the link referent).
    #[error("symlink destination '{path}' declares no source (link referent)")]
    SymlinkMissingSource {
        /// Symlink destination with no referent to point at.
        path: PathBuf,
    },

    /// Destination is outside the active ANOLISA-owned layout.
    #[error("destination '{path}' is not under an ANOLISA-owned root")]
    ExternalPath {
        /// Rejected destination path.
        path: PathBuf,
    },

    /// Destination contains traversal syntax after template rendering.
    #[error(
        "destination '{path}' contains a '.' or '..' segment — refuse to install via traversal"
    )]
    TraversalSegment {
        /// Rejected destination path.
        path: PathBuf,
    },

    /// Manifest requested a file mode that is not valid octal notation.
    #[error("destination '{path}' has invalid install mode '{mode}'")]
    InvalidMode {
        /// Destination whose mode could not be parsed.
        path: PathBuf,
        /// Raw manifest mode string.
        mode: String,
    },

    /// Fresh-install milestone refuses to overwrite existing files.
    #[error("destination '{path}' already exists — refuses to overwrite")]
    DestExists {
        /// Existing destination path.
        path: PathBuf,
    },

    /// Two manifest/archive entries resolved to the same destination.
    #[error("destination '{path}' is declared more than once")]
    DuplicateDestination {
        /// Duplicate destination path.
        path: PathBuf,
    },

    /// Layout substitution failed to consume a template placeholder.
    #[error(
        "destination '{path}' resolved to an unrendered template — manifest variable not substituted"
    )]
    UnresolvedTemplate {
        /// Destination still containing template syntax.
        path: PathBuf,
    },

    /// Archive did not contain the requested source entry.
    #[error("tar_gz archive entry for dest basename '{basename}' not found")]
    MissingArchiveEntry {
        /// Normalized archive key or legacy destination basename.
        basename: String,
    },

    /// Filesystem access failed while reading the cache or writing a
    /// destination.
    #[error("io error while accessing {path}: {source}")]
    Io {
        /// Path involved in the failed filesystem operation.
        path: PathBuf,
        /// Original I/O error from the OS.
        #[source]
        source: std::io::Error,
    },

    /// Archive stream could not be decoded or read.
    #[error("archive read error: {0}")]
    Archive(String),

    /// Embedded `.anolisa/component.toml` is not valid UTF-8 or could not be
    /// parsed as a component manifest.
    #[error("embedded component manifest could not be parsed: {0}")]
    EmbeddedManifestParse(String),

    /// A staged payload no longer matched the digest recorded during
    /// preparation when it was streamed to its destination.
    ///
    /// Preparation verifies content once; placement re-hashes what it writes
    /// so tampering with the staging area between the two phases cannot get
    /// unverified bytes installed.
    #[error(
        "staged payload for '{path}' does not match preparation: expected sha256 {expected_sha256} ({expected_size} bytes), got {actual_sha256} ({actual_size} bytes)"
    )]
    StagedContentMismatch {
        /// Destination whose staged payload failed re-verification.
        path: PathBuf,
        /// Digest recorded while the entry was spooled to staging.
        expected_sha256: String,
        /// Byte length recorded while the entry was spooled to staging.
        expected_size: u64,
        /// Digest of what was actually read back from staging.
        actual_sha256: String,
        /// Byte length actually read back from staging.
        actual_size: u64,
    },

    /// Content rendering (`render = "anolisa-paths-v1"`) failed for an entry.
    #[error("cannot render '{path}': {reason}")]
    Render {
        /// Destination of the entry that could not be rendered.
        path: PathBuf,
        /// What was wrong (non-UTF-8 content, unknown placeholder, or a
        /// render request on a non-regular entry).
        reason: String,
    },
}

/// Extract the embedded `.anolisa/component.toml` text from a tar.gz
/// artifact.
///
/// Returns `Ok(None)` when the archive has no such entry. This is used when
/// callers need to persist the published component contract byte-for-byte as
/// local install metadata.
///
/// # Errors
/// [`InstallError::Io`] when the archive cannot be opened or read;
/// [`InstallError::Archive`] when gzip/tar decoding fails;
/// [`InstallError::EmbeddedManifestParse`] when the entry is not valid UTF-8.
pub fn read_embedded_component_manifest_text(
    artifact: &Path,
) -> Result<Option<String>, InstallError> {
    let io_err = |source: std::io::Error| InstallError::Io {
        path: artifact.to_path_buf(),
        source,
    };
    let archive_err = |e: std::io::Error| InstallError::Archive(e.to_string());

    let file = File::open(artifact).map_err(io_err)?;
    let gz = GzDecoder::new(file);
    let mut archive = Archive::new(gz);
    for entry in archive.entries().map_err(archive_err)? {
        let mut entry = entry.map_err(archive_err)?;
        // Scope the path borrow so `read_to_end` can take `&mut entry`.
        let is_manifest = {
            let path = entry.path().map_err(archive_err)?;
            let normalized = path.strip_prefix("./").unwrap_or(&path);
            normalized == Path::new(".anolisa/component.toml")
        };
        if is_manifest {
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).map_err(io_err)?;
            let text = String::from_utf8(bytes)
                .map_err(|e| InstallError::EmbeddedManifestParse(e.to_string()))?;
            return Ok(Some(text));
        }
    }
    Ok(None)
}

/// Stateless installer bound to an [`FsLayout`] for ANOLISA-owned-root
/// validation. Construct one per install or update invocation.
pub struct InstallRunner<'a> {
    layout: &'a FsLayout,
}

impl<'a> InstallRunner<'a> {
    /// Build a runner over `layout` — used only to validate that every
    /// destination resolves under an ANOLISA-owned root.
    pub fn new(layout: &'a FsLayout) -> Self {
        Self { layout }
    }

    /// Install a file set previously returned by [`Self::prepare_files`].
    ///
    /// Each destination is streamed from the payload preparation spooled to
    /// staging and re-hashed on the way out; the cache is not reopened. A
    /// staged payload whose digest no longer matches the one recorded during
    /// preparation is rejected before its destination is renamed into place.
    /// Destination safety and vacancy are rechecked immediately before
    /// placement so a caller may safely carry this value across a lifecycle
    /// lock boundary.
    ///
    /// # Errors
    ///
    /// Fails if a destination is no longer safe or vacant, a staged payload no
    /// longer matches its recorded digest, or placement fails. A later
    /// placement failure best-effort removes paths created by this call.
    pub fn install_prepared(
        &self,
        prepared: PreparedFileSet,
    ) -> Result<InstallOutcome, InstallError> {
        let regular_files = prepared
            .regular
            .iter()
            .map(|staged| staged.file.clone())
            .collect::<Vec<_>>();
        let links = prepared
            .links
            .iter()
            .map(PreparedSymlink::as_resolved)
            .collect::<Vec<_>>();
        self.validate_install_targets(&regular_files, DestinationPolicy::Vacant)?;
        self.validate_symlink_entries(&links, DestinationPolicy::Vacant)?;
        let mut installed = Vec::with_capacity(prepared.regular.len() + prepared.links.len());
        for staged in &prepared.regular {
            match place_staged_file(staged) {
                Ok(file) => {
                    // Release the spooled copy as soon as it lands so staging
                    // does not hold a second full copy of the payload while
                    // the remaining entries are placed. Every staged path is
                    // claimed by exactly one destination, so nothing else can
                    // still need this file. A failure here cannot leak the
                    // payload: dropping `prepared` removes whatever is left.
                    let _ = fs::remove_file(&staged.staged_path);
                    installed.push(file);
                }
                Err(err) => {
                    rollback_installed_files(&installed);
                    return Err(err);
                }
            }
        }
        for link in &prepared.links {
            match create_symlink(link) {
                Ok(file) => installed.push(file),
                Err(err) => {
                    rollback_installed_files(&installed);
                    return Err(err);
                }
            }
        }
        // Staging is released by dropping `prepared` on the way out — the same
        // path every early return above takes. Deliberately not
        // `TempDir::close()`: that consumes the guard and disables `Drop`, so a
        // removal failure would leave the payload behind with nothing left to
        // retry it.
        Ok(InstallOutcome { files: installed })
    }

    /// Read and validate an artifact once, spooling the exact payloads that a
    /// later [`Self::install_prepared`] call will place into a private staging
    /// directory owned by the returned [`PreparedFileSet`].
    ///
    /// # Errors
    ///
    /// Fails when the artifact type is unsupported, a destination is unsafe or
    /// already exists, the cache cannot be read, or a requested entry is missing.
    pub fn prepare_files(
        &self,
        artifact_type: &str,
        cached_artifact: &Path,
        files: &[ResolvedInstallFile],
    ) -> Result<PreparedFileSet, InstallError> {
        self.prepare_files_with_policy(
            artifact_type,
            cached_artifact,
            files,
            DestinationPolicy::Vacant,
        )
    }

    /// Prepare replacement bytes while the recorded destinations still
    /// exist; [`Self::install_prepared`] still requires them to be vacant.
    ///
    /// # Errors
    ///
    /// Returns artifact, mapping, and path-safety errors. Existing
    /// destinations are not an error during preparation.
    pub fn prepare_replacement_files(
        &self,
        artifact_type: &str,
        cached_artifact: &Path,
        files: &[ResolvedInstallFile],
    ) -> Result<PreparedFileSet, InstallError> {
        self.prepare_files_with_policy(
            artifact_type,
            cached_artifact,
            files,
            DestinationPolicy::MayExist,
        )
    }

    fn prepare_files_with_policy(
        &self,
        artifact_type: &str,
        cached_artifact: &Path,
        files: &[ResolvedInstallFile],
        destination_policy: DestinationPolicy,
    ) -> Result<PreparedFileSet, InstallError> {
        if files.is_empty() {
            return Err(InstallError::NoDestinations);
        }
        // Symlink entries never touch the artifact: split them out, install
        // the regular files, then create the links — referents that point at
        // freshly installed files exist by the time the link is made.
        let (links, regular): (Vec<_>, Vec<_>) = files
            .iter()
            .cloned()
            .partition(|f| f.kind == FileKind::Symlink);
        self.validate_symlink_entries(&links, destination_policy)?;
        let links = links
            .into_iter()
            .map(|link| {
                let referent = link
                    .source
                    .ok_or_else(|| InstallError::SymlinkMissingSource {
                        path: link.dest.clone(),
                    })?;
                Ok(PreparedSymlink {
                    dest: link.dest,
                    referent: PathBuf::from(referent),
                })
            })
            .collect::<Result<Vec<_>, InstallError>>()?;
        if regular.is_empty() {
            // A links-only manifest has no use for the downloaded artifact —
            // treat it as the same defect as declaring no files at all.
            return Err(InstallError::NoDestinations);
        }
        let (staging, regular) = match artifact_type {
            "tar_gz" => self.prepare_tar_gz(cached_artifact, &regular, destination_policy),
            other => Err(InstallError::UnsupportedArtifactType(other.to_string())),
        }?;
        Ok(PreparedFileSet {
            staging,
            regular,
            links,
        })
    }

    /// Up-front checks for symlink entries, run before any byte lands so a
    /// rejected link cannot leave a half-finished install: referent and
    /// destination must be ANOLISA-owned; vacancy follows the caller policy.
    fn validate_symlink_entries(
        &self,
        links: &[ResolvedInstallFile],
        destination_policy: DestinationPolicy,
    ) -> Result<(), InstallError> {
        let mut seen = BTreeSet::new();
        for link in links {
            // A symlink has no content to render; a render request on one
            // is a contract defect, not something to silently drop. The CLI
            // rejects this earlier with contract context (raw install's
            // render resolution); this re-check is defense-in-depth for
            // callers constructing `ResolvedInstallFile` directly.
            if link.render.is_some() {
                return Err(InstallError::Render {
                    path: link.dest.clone(),
                    reason: "render applies to regular files, not symlinks".to_string(),
                });
            }
            let referent =
                link.source
                    .as_deref()
                    .ok_or_else(|| InstallError::SymlinkMissingSource {
                        path: link.dest.clone(),
                    })?;
            // A link must not point outside the owned roots any more than a
            // regular file may be written there.
            self.validate_dest(Path::new(referent))?;
            self.validate_dest(&link.dest)?;
            if !seen.insert(link.dest.clone()) {
                return Err(InstallError::DuplicateDestination {
                    path: link.dest.clone(),
                });
            }
            if destination_policy == DestinationPolicy::Vacant {
                ensure_destination_vacant(&link.dest)?;
            }
        }
        Ok(())
    }
    /// Spool the contract-selected archive entries to staging, then expand
    /// them into one prepared destination each.
    ///
    /// The archive is decoded exactly once (gzip is sequential, so an index
    /// would have to buffer payloads), and only entries an [`EntrySelector`]
    /// accepts are written to disk.
    fn prepare_tar_gz(
        &self,
        cached_artifact: &Path,
        files: &[ResolvedInstallFile],
        destination_policy: DestinationPolicy,
    ) -> Result<(StagingDir, Vec<PreparedRegularFile>), InstallError> {
        let selector = EntrySelector::build(files)?;
        let mut staged = StagedArchive::spool(cached_artifact, self.staging_parent()?, &selector)?;

        let mut expanded: Vec<PreparedRegularFile> = Vec::new();
        for file in files {
            if let Some(source) = file.source.as_deref()
                && archive_source_is_dir(source)
            {
                let prefix = normalize_archive_key(source);
                let prefix = prefix.trim_end_matches('/');
                // Collect before claiming: `claim` needs `&mut staged`, and
                // the sorted map iteration is what makes the expanded order
                // of a directory source stable across runs.
                let matches = staged.entries_under(prefix);
                if matches.is_empty() {
                    return Err(InstallError::MissingArchiveEntry {
                        basename: format!("{prefix}/"),
                    });
                }
                for (key, relative, index) in matches {
                    let claim = staged.claim(index)?;
                    let mode = file
                        .mode
                        .clone()
                        .or_else(|| claim.mode.map(|mode| format!("{mode:04o}")));
                    expanded.push(PreparedRegularFile {
                        file: ResolvedInstallFile {
                            source: Some(key),
                            dest: file.dest.join(relative),
                            mode,
                            kind: file.kind,
                            render: None,
                        },
                        staged_path: claim.path,
                        sha256: claim.sha256,
                        size: claim.size,
                    });
                }
                continue;
            }

            let key = archive_source_key(file)?;
            let index =
                staged
                    .index_for(&key)
                    .ok_or_else(|| InstallError::MissingArchiveEntry {
                        basename: key.clone(),
                    })?;
            // Rendering is the one case that needs the payload in memory: it
            // rewrites the whole content as text. Only single-file sources may
            // render (see `EntrySelector::build`), so a large directory
            // payload never takes this branch.
            let claim = match file.render.as_ref() {
                None => staged.claim(index)?,
                Some(spec) => {
                    let bytes = staged.read_staged(index)?;
                    let rendered = self.render_bytes(file, spec, &bytes)?;
                    staged.stage_bytes(&rendered)?
                }
            };
            expanded.push(PreparedRegularFile {
                file: file.clone(),
                staged_path: claim.path,
                sha256: claim.sha256,
                size: claim.size,
            });
        }

        let expanded_files: Vec<ResolvedInstallFile> =
            expanded.iter().map(|staged| staged.file.clone()).collect();
        self.validate_install_targets(&expanded_files, destination_policy)?;

        // Duplicate archive keys are last-write-wins, so a superseded payload
        // can never be claimed — drop it now instead of carrying it until the
        // staging directory is dropped.
        staged.prune_unclaimed();
        Ok((staged.into_staging(), expanded))
    }

    /// Ensure the cache directory used as the staging parent exists.
    ///
    /// `cache_dir` is the trust anchor supplied by [`FsLayout`] and shared with
    /// the artifact cache (`cache_dir/downloads`). Each preparation creates its
    /// own random `0700` child directly beneath it, so a group-shared cache does
    /// not acquire a fixed directory owned by whichever user installs first.
    fn staging_parent(&self) -> Result<&Path, InstallError> {
        let cache_dir = &self.layout.cache_dir;
        fs::create_dir_all(cache_dir).map_err(|source| InstallError::Io {
            path: cache_dir.clone(),
            source,
        })?;
        Ok(cache_dir)
    }

    /// Apply the entry's content-rendering request to the staged bytes.
    ///
    /// For [`RenderMode::AnolisaPathsV1`] the bytes must be UTF-8 text; layout
    /// placeholders are substituted via the same expansion vocabulary used
    /// for destination paths, so path and content semantics cannot drift on
    /// what a placeholder means. Content additionally keeps `${VAR}`
    /// environment-variable references verbatim for the runtime consumer to
    /// resolve (see [`crate::adapter::expand_layout_placeholders_content`]).
    fn render_bytes(
        &self,
        file: &ResolvedInstallFile,
        spec: &RenderSpec,
        bytes: &[u8],
    ) -> Result<Vec<u8>, InstallError> {
        match spec.mode {
            RenderMode::AnolisaPathsV1 => {
                let text = std::str::from_utf8(bytes).map_err(|_| InstallError::Render {
                    path: file.dest.clone(),
                    reason: "content is not valid UTF-8 — anolisa-paths-v1 renders text files only"
                        .to_string(),
                })?;
                let rendered = crate::adapter::expand_layout_placeholders_content(
                    text,
                    self.layout,
                    &[("component", spec.component.as_str())],
                )
                .map_err(|err| {
                    let reason = match err {
                        crate::adapter::AdapterError::UnknownPlaceholder {
                            placeholder, ..
                        } => format!("content references unknown placeholder '{{{placeholder}}}'"),
                        other => other.to_string(),
                    };
                    InstallError::Render {
                        path: file.dest.clone(),
                        reason,
                    }
                })?;
                Ok(rendered.into_bytes())
            }
        }
    }

    fn validate_dest(&self, dest: &Path) -> Result<(), InstallError> {
        if dest.to_string_lossy().contains('{') {
            return Err(InstallError::UnresolvedTemplate {
                path: dest.to_path_buf(),
            });
        }
        // Shared lexical + canonical boundary check (see path_safety).
        // Uninstall uses the same helper before backup/remove so the two
        // verbs cannot drift out of lockstep on what counts as
        // "ANOLISA-owned".
        crate::path_safety::validate_owned_path(self.layout, dest).map_err(|err| match err {
            crate::path_safety::PathBoundaryError::Traversal { path } => {
                InstallError::TraversalSegment { path }
            }
            crate::path_safety::PathBoundaryError::External { path } => {
                InstallError::ExternalPath { path }
            }
        })
    }

    fn validate_install_targets(
        &self,
        files: &[ResolvedInstallFile],
        destination_policy: DestinationPolicy,
    ) -> Result<(), InstallError> {
        let mut seen = BTreeSet::new();
        for file in files {
            if !seen.insert(file.dest.clone()) {
                return Err(InstallError::DuplicateDestination {
                    path: file.dest.clone(),
                });
            }
            self.validate_dest(&file.dest)?;
        }
        if destination_policy == DestinationPolicy::Vacant {
            for file in files {
                ensure_destination_vacant(&file.dest)?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DestinationPolicy {
    Vacant,
    MayExist,
}

/// One validated destination whose payload waits in the staging directory.
#[derive(Debug)]
struct PreparedRegularFile {
    /// Destination mapping, with directory sources already expanded.
    file: ResolvedInstallFile,
    /// Staged payload, inside the owning [`PreparedFileSet::staging`] dir.
    /// Claimed by exactly this destination, so placement may delete it.
    staged_path: PathBuf,
    /// Digest computed while the payload was streamed to staging; re-checked
    /// during placement and recorded in `InstalledState`.
    sha256: String,
    /// Byte length streamed to staging, re-checked during placement.
    size: u64,
}

/// Validated install destinations whose regular-file payloads are spooled to a
/// private staging directory.
///
/// Fields are private so every instance preserves the path, archive, and
/// symlink invariants established by [`InstallRunner::prepare_files`]. The
/// value may be held across a lifecycle lock, dependency provisioning, and
/// pre-install hooks: dropping it removes every remaining staged payload, on
/// both the success and the error path.
#[derive(Debug)]
pub struct PreparedFileSet {
    /// Owns the staged payloads. Never read: cleanup is the `TempDir` `Drop`
    /// impl, which is exactly why it must not be replaced by an explicit
    /// `close()` — `close()` consumes the guard and disables `Drop`, so a
    /// removal that fails would leak the whole payload instead of being
    /// retried on scope exit.
    #[expect(
        dead_code,
        reason = "RAII guard: held so Drop removes staged payloads on every exit path"
    )]
    staging: StagingDir,
    regular: Vec<PreparedRegularFile>,
    links: Vec<PreparedSymlink>,
}

impl PreparedFileSet {
    /// Describe the files this set will install without touching disk.
    ///
    /// Reports the digests recorded while the payloads were streamed to
    /// staging; the staged files are not re-read, so this stays cheap for
    /// multi-gigabyte payloads. [`InstallRunner::install_prepared`] verifies
    /// the same digests as it writes, so its `InstalledFile` entries match.
    pub fn preview(&self) -> InstallOutcome {
        let mut files = self
            .regular
            .iter()
            .map(|staged| InstalledFile {
                path: staged.file.dest.clone(),
                kind: staged.file.kind,
                mode: staged.file.mode.clone(),
                sha256: staged.sha256.clone(),
                referent: None,
            })
            .collect::<Vec<_>>();
        files.extend(self.links.iter().map(|link| InstalledFile {
            path: link.dest.clone(),
            kind: FileKind::Symlink,
            mode: None,
            sha256: String::new(),
            referent: Some(link.referent.clone()),
        }));
        InstallOutcome { files }
    }
}

#[derive(Debug)]
struct PreparedSymlink {
    dest: PathBuf,
    referent: PathBuf,
}

impl PreparedSymlink {
    fn as_resolved(&self) -> ResolvedInstallFile {
        ResolvedInstallFile {
            source: Some(self.referent.to_string_lossy().into_owned()),
            dest: self.dest.clone(),
            mode: None,
            kind: FileKind::Symlink,
            render: None,
        }
    }
}

fn ensure_destination_vacant(dest: &Path) -> Result<(), InstallError> {
    match fs::symlink_metadata(dest) {
        Ok(_) => Err(InstallError::DestExists {
            path: dest.to_path_buf(),
        }),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(InstallError::Io {
            path: dest.to_path_buf(),
            source,
        }),
    }
}

/// What the component contract asks the archive for.
///
/// Built before the archive is decoded so an entry no destination maps to is
/// skipped without ever being written to staging.
struct EntrySelector {
    /// Exact keys: normalized full archive paths and legacy destination
    /// basenames. Matched against an entry's full path *and* its basename,
    /// mirroring the dual-keyed lookup this replaces.
    keys: BTreeSet<String>,
    /// Directory sources, trailing slash trimmed. An empty prefix (source
    /// `"/"`) selects the whole archive, as before.
    prefixes: Vec<String>,
}

impl EntrySelector {
    fn build(files: &[ResolvedInstallFile]) -> Result<Self, InstallError> {
        let mut keys = BTreeSet::new();
        let mut prefixes = Vec::new();
        for file in files {
            match file.source.as_deref() {
                Some(source) if archive_source_is_dir(source) => {
                    // Rendering targets one regular file; a directory source
                    // fans out to arbitrarily many entries and would silently
                    // rewrite payloads the contract author never meant to
                    // template. Fail closed. Mirrors the CLI-side early check
                    // in raw install's render resolution; keep both in sync.
                    if file.render.is_some() {
                        return Err(InstallError::Render {
                            path: file.dest.clone(),
                            reason: "render applies to single regular files, not directory sources"
                                .to_string(),
                        });
                    }
                    let prefix = normalize_archive_key(source);
                    prefixes.push(prefix.trim_end_matches('/').to_string());
                }
                _ => {
                    keys.insert(archive_source_key(file)?);
                }
            }
        }
        Ok(Self { keys, prefixes })
    }

    fn selects(&self, path_key: &str, basename: &str) -> bool {
        self.keys.contains(path_key)
            || self.keys.contains(basename)
            || self.prefixes.iter().any(|prefix| {
                archive_relative_under(path_key, prefix).is_some_and(|rel| !rel.is_empty())
            })
    }
}

/// A staged payload reserved for one destination.
struct StagedClaim {
    path: PathBuf,
    sha256: String,
    size: u64,
    mode: Option<u32>,
}

/// One archive entry spooled to disk.
struct StagedEntry {
    path: PathBuf,
    sha256: String,
    size: u64,
    mode: Option<u32>,
}

/// Contract-selected archive entries spooled into a private staging directory,
/// addressable by full archive path and by basename.
///
/// Replaces the previous in-memory entry index: the maps hold indices into
/// `entries`, and the payloads live on disk, so memory scales with the entry
/// count rather than with the uncompressed payload size.
struct StagedArchive {
    staging: StagingDir,
    entries: Vec<StagedEntry>,
    /// Full archive path -> entry index. Drives directory-source expansion;
    /// sorted iteration gives that expansion a stable order.
    full_paths: BTreeMap<String, usize>,
    /// Basename *and* full archive path -> entry index. Duplicate keys are
    /// last-write-wins in archive order, matching the previous behavior.
    lookup: BTreeMap<String, usize>,
    /// Whether an entry has been reserved for a destination. A second claim
    /// gets its own hard link so each destination owns its staged file.
    claimed: Vec<bool>,
    /// Monotonic counter for unique staged file names.
    next_id: u64,
}

impl StagedArchive {
    /// Decode `artifact` once, streaming each selected entry into a fresh
    /// private directory under `staging_parent`.
    fn spool(
        artifact: &Path,
        staging_parent: &Path,
        selector: &EntrySelector,
    ) -> Result<Self, InstallError> {
        let file = File::open(artifact).map_err(|source| InstallError::Io {
            path: artifact.to_path_buf(),
            source,
        })?;
        let staging = StagingDir::create(staging_parent)?;
        let mut staged = Self {
            staging,
            entries: Vec::new(),
            full_paths: BTreeMap::new(),
            lookup: BTreeMap::new(),
            claimed: Vec::new(),
            next_id: 0,
        };
        let mut archive = Archive::new(GzDecoder::new(file));
        let entries = archive
            .entries()
            .map_err(|e| InstallError::Archive(format!("entries: {e}")))?;
        for entry_res in entries {
            let mut entry = entry_res.map_err(|e| InstallError::Archive(format!("entry: {e}")))?;
            if !entry.header().entry_type().is_file() {
                continue;
            }
            let mode = entry.header().mode().ok().map(|mode| mode & 0o7777);
            let entry_path = entry
                .path()
                .map_err(|e| InstallError::Archive(format!("path: {e}")))?
                .into_owned();
            // Unsafe paths are rejected whether or not the contract selects
            // them: an archive that tries to escape is not one to install from.
            let Some(path_key) = archive_key_from_path(&entry_path)? else {
                continue;
            };
            let basename = path_key.rsplit('/').next().unwrap_or(&path_key).to_string();
            if !selector.selects(&path_key, &basename) {
                continue;
            }
            let index = staged.spool_entry(&mut entry, &path_key, mode)?;
            // Basename first, then the full path — the same insertion order as
            // the map this replaces, so which entry wins a basename/full-path
            // collision does not change.
            staged.lookup.insert(basename, index);
            staged.lookup.insert(path_key.clone(), index);
            staged.full_paths.insert(path_key, index);
        }
        Ok(staged)
    }

    /// Stream one entry to a staging file, hashing as it goes.
    fn spool_entry(
        &mut self,
        source: &mut impl Read,
        path_key: &str,
        mode: Option<u32>,
    ) -> Result<usize, InstallError> {
        let path = self.next_staged_path();
        let mut out = create_exclusive_no_follow(&path)?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; STREAM_BUF_SIZE];
        let mut size = 0u64;
        loop {
            let read = source
                .read(&mut buf)
                .map_err(|e| InstallError::Archive(format!("read entry '{path_key}': {e}")))?;
            if read == 0 {
                break;
            }
            hasher.update(&buf[..read]);
            out.write_all(&buf[..read])
                .map_err(|source| InstallError::Io {
                    path: path.clone(),
                    source,
                })?;
            size += read as u64;
        }
        out.flush().map_err(|source| InstallError::Io {
            path: path.clone(),
            source,
        })?;
        self.entries.push(StagedEntry {
            path,
            sha256: to_lower_hex(&hasher.finalize()),
            size,
            mode,
        });
        self.claimed.push(false);
        Ok(self.entries.len() - 1)
    }

    /// Spool already-in-memory bytes (a rendered file) as a new staged entry.
    ///
    /// The result is claimed immediately: rendered content belongs to exactly
    /// the destination that requested it and is not addressable by archive key.
    fn stage_bytes(&mut self, bytes: &[u8]) -> Result<StagedClaim, InstallError> {
        let index = self.spool_entry(&mut &bytes[..], "<rendered>", None)?;
        self.claim(index)
    }

    /// Entry index for an explicit archive `source` or legacy dest basename.
    fn index_for(&self, key: &str) -> Option<usize> {
        self.lookup.get(key).copied()
    }

    /// Entries under a directory source, as `(archive key, relative path,
    /// entry index)` in sorted-key order.
    fn entries_under(&self, prefix: &str) -> Vec<(String, String, usize)> {
        self.full_paths
            .iter()
            .filter_map(|(key, &index)| {
                let relative = archive_relative_under(key, prefix)?;
                (!relative.is_empty()).then(|| (key.clone(), relative.to_string(), index))
            })
            .collect()
    }

    /// Reserve entry `index` for one destination.
    ///
    /// A repeat claim — two contract entries mapping the same archive entry to
    /// different destinations — gets its own hard link to the same payload, so
    /// placement can delete each staged file as soon as it is consumed without
    /// pulling the payload out from under the other destination.
    fn claim(&mut self, index: usize) -> Result<StagedClaim, InstallError> {
        let entry = self.entries.get(index).ok_or_else(|| {
            InstallError::Archive(format!("internal: staged entry {index} is missing"))
        })?;
        let (source, sha256, size, mode) = (
            entry.path.clone(),
            entry.sha256.clone(),
            entry.size,
            entry.mode,
        );
        let already_claimed = self.claimed.get(index).copied().unwrap_or(false);
        if !already_claimed {
            self.claimed[index] = true;
            return Ok(StagedClaim {
                path: source,
                sha256,
                size,
                mode,
            });
        }
        let path = self.next_staged_path();
        // Same directory, hence same filesystem: the link is O(1) and adds no
        // disk. `link(2)` neither follows nor replaces an existing new path.
        if fs::hard_link(&source, &path).is_err() {
            // Filesystems without hard-link support: copy through the same
            // exclusive no-follow open the rest of staging uses, rather than
            // `fs::copy`, which follows and truncates its destination.
            let mut reader = File::open(&source).map_err(|err| InstallError::Io {
                path: source.clone(),
                source: err,
            })?;
            let mut writer = create_exclusive_no_follow(&path)?;
            std::io::copy(&mut reader, &mut writer).map_err(|err| InstallError::Io {
                path: path.clone(),
                source: err,
            })?;
        }
        Ok(StagedClaim {
            path,
            sha256,
            size,
            mode,
        })
    }

    /// Read a staged payload back into memory. Only used for rendering, which
    /// is restricted to single-file sources.
    fn read_staged(&self, index: usize) -> Result<Vec<u8>, InstallError> {
        let entry = self.entries.get(index).ok_or_else(|| {
            InstallError::Archive(format!("internal: staged entry {index} is missing"))
        })?;
        fs::read(&entry.path).map_err(|source| InstallError::Io {
            path: entry.path.clone(),
            source,
        })
    }

    /// Delete staged payloads no destination claimed — entries superseded by a
    /// later duplicate archive key.
    fn prune_unclaimed(&self) {
        for (index, entry) in self.entries.iter().enumerate() {
            if !self.claimed.get(index).copied().unwrap_or(false) {
                let _ = fs::remove_file(&entry.path);
            }
        }
    }

    /// Hand the staging directory to the [`PreparedFileSet`] that owns the
    /// claims, which keeps the payloads alive until placement finishes.
    fn into_staging(self) -> StagingDir {
        self.staging
    }

    fn next_staged_path(&mut self) -> PathBuf {
        let id = self.next_id;
        self.next_id += 1;
        self.staging.path().join(format!("{id:08}"))
    }
}

/// Private staging directory whose advisory lock distinguishes active work
/// from debris left by an interrupted process.
#[derive(Debug)]
struct StagingDir {
    // Keep the directory before the lock so field drop order removes payloads
    // while the lock is still held.
    dir: TempDir,
    #[expect(
        dead_code,
        reason = "RAII guard: the open file keeps this staging directory active"
    )]
    lock: File,
}

impl StagingDir {
    fn create(parent: &Path) -> Result<Self, InstallError> {
        reclaim_stale_staging_dirs(parent);

        let mut builder = tempfile::Builder::new();
        builder.prefix(STAGING_DIR_PREFIX);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            builder.permissions(fs::Permissions::from_mode(0o700));
        }
        let dir = builder
            .tempdir_in(parent)
            .map_err(|source| InstallError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        let pending_lock_path = dir.path().join(STAGING_LOCK_PENDING_FILE);
        let lock =
            open_staging_lock(&pending_lock_path, true).map_err(|source| InstallError::Io {
                path: pending_lock_path.clone(),
                source,
            })?;
        lock.lock_exclusive().map_err(|source| InstallError::Io {
            path: pending_lock_path.clone(),
            source,
        })?;
        let lock_path = dir.path().join(STAGING_LOCK_FILE);
        fs::rename(&pending_lock_path, &lock_path).map_err(|source| InstallError::Io {
            path: pending_lock_path,
            source,
        })?;
        Ok(Self { dir, lock })
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }
}

/// Best-effort removal of staging left behind after a process dies.
///
/// A live [`StagingDir`] holds its lock until `TempDir` cleanup finishes. A
/// killed process releases the kernel lock automatically, so the next prepare
/// can acquire it and remove the orphan without an age heuristic that could
/// delete a long-running concurrent installation.
fn reclaim_stale_staging_dirs(parent: &Path) {
    let Ok(entries) = fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry
            .file_name()
            .to_string_lossy()
            .starts_with(STAGING_DIR_PREFIX)
            || !entry.file_type().is_ok_and(|kind| kind.is_dir())
        {
            continue;
        }
        let lock_path = entry.path().join(STAGING_LOCK_FILE);
        let Ok(lock) = open_staging_lock(&lock_path, false) else {
            continue;
        };
        if lock.try_lock_exclusive().is_ok() {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

fn open_staging_lock(path: &Path, create_new: bool) -> std::io::Result<File> {
    let mut opts = OpenOptions::new();
    opts.read(true).write(true).create_new(create_new);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(nix::libc::O_NOFOLLOW);
    }
    opts.open(path)
}

fn archive_source_key(file: &ResolvedInstallFile) -> Result<String, InstallError> {
    let key = match file.source.as_deref() {
        Some(source) => normalize_archive_key(source),
        None => file
            .dest
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string(),
    };
    if key.is_empty() {
        return Err(InstallError::ExternalPath {
            path: file.dest.clone(),
        });
    }
    Ok(key)
}

fn archive_source_is_dir(source: &str) -> bool {
    source.ends_with('/')
}

fn archive_relative_under<'a>(key: &'a str, prefix: &str) -> Option<&'a str> {
    if prefix.is_empty() {
        return Some(key);
    }
    let rest = key.strip_prefix(prefix)?;
    rest.strip_prefix('/')
}

fn normalize_archive_key(path: &str) -> String {
    path.trim_start_matches("./").to_string()
}

fn archive_key_from_path(path: &Path) -> Result<Option<String>, InstallError> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => {
                let Some(part) = part.to_str() else {
                    return Ok(None);
                };
                parts.push(part.to_string());
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(InstallError::Archive(format!(
                    "unsafe archive entry path '{}'",
                    path.display()
                )));
            }
        }
    }
    if parts.is_empty() {
        Ok(None)
    } else {
        Ok(Some(parts.join("/")))
    }
}

/// Create one validated symlink entry and record the referent path.
///
/// Returns `sha256 = ""` with `referent = Some(target_path)` — the
/// integrity probe verifies symlinks by checking `readlink` against
/// the recorded referent rather than hashing content through the link.
/// A referent that does not exist fails here: installing a dangling
/// convenience link would be a manifest defect, not a usable install.
fn create_symlink(link: &PreparedSymlink) -> Result<InstalledFile, InstallError> {
    if !link.referent.exists() {
        return Err(InstallError::Io {
            path: link.referent.clone(),
            source: std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "symlink referent does not exist",
            ),
        });
    }
    if let Some(parent) = link.dest.parent() {
        fs::create_dir_all(parent).map_err(|source| InstallError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    std::os::unix::fs::symlink(&link.referent, &link.dest).map_err(|source| InstallError::Io {
        path: link.dest.clone(),
        source,
    })?;
    Ok(InstalledFile {
        path: link.dest.clone(),
        kind: FileKind::Symlink,
        mode: None,
        sha256: String::new(),
        referent: Some(link.referent.clone()),
    })
}

fn rollback_installed_files(files: &[InstalledFile]) {
    for file in files.iter().rev() {
        let _ = fs::remove_file(&file.path);
    }
}

/// Stream one staged payload to its destination.
///
/// Opens the staged file and hands it to [`write_dest_atomic`], which
/// re-verifies the recorded digest before the rename.
fn place_staged_file(staged: &PreparedRegularFile) -> Result<InstalledFile, InstallError> {
    let mut source = File::open(&staged.staged_path).map_err(|source| InstallError::Io {
        path: staged.staged_path.clone(),
        source,
    })?;
    write_dest_atomic(
        &staged.file,
        &mut source,
        StagedContent {
            sha256: &staged.sha256,
            size: staged.size,
        },
    )
}

/// Digest and length a staged payload must still have at placement time.
struct StagedContent<'a> {
    sha256: &'a str,
    size: u64,
}

/// Stream `source` into `dest.tmp`, verify it against `expected`, then rename.
///
/// The content is hashed on the way out and compared with the digest recorded
/// during preparation; a mismatch removes the temporary sibling and fails
/// before anything is renamed over the destination.
fn write_dest_atomic(
    file: &ResolvedInstallFile,
    source: &mut impl Read,
    expected: StagedContent<'_>,
) -> Result<InstalledFile, InstallError> {
    let dest = &file.dest;
    #[cfg(unix)]
    let parsed_mode = parse_unix_mode(file.mode.as_deref(), dest)?;

    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|source| InstallError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let tmp = tmp_sibling(dest);
    let sha = match stream_write_and_hash(&tmp, source) {
        Ok((sha, size)) if sha == expected.sha256 && size == expected.size => sha,
        Ok((sha, size)) => {
            let _ = fs::remove_file(&tmp);
            return Err(InstallError::StagedContentMismatch {
                path: dest.to_path_buf(),
                expected_sha256: expected.sha256.to_string(),
                expected_size: expected.size,
                actual_sha256: sha,
                actual_size: size,
            });
        }
        Err(err) => {
            let _ = fs::remove_file(&tmp);
            return Err(err);
        }
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(parsed_mode);
        if let Err(source) = fs::set_permissions(&tmp, perms) {
            let _ = fs::remove_file(&tmp);
            return Err(InstallError::Io {
                path: tmp.clone(),
                source,
            });
        }
    }
    fs::rename(&tmp, dest).map_err(|source| {
        let _ = fs::remove_file(&tmp);
        InstallError::Io {
            path: dest.to_path_buf(),
            source,
        }
    })?;
    Ok(InstalledFile {
        path: dest.to_path_buf(),
        kind: file.kind,
        mode: file.mode.clone(),
        sha256: sha,
        referent: None,
    })
}

#[cfg(unix)]
fn parse_unix_mode(mode: Option<&str>, dest: &Path) -> Result<u32, InstallError> {
    const DEFAULT_MODE: u32 = 0o755;
    let Some(raw) = mode else {
        return Ok(DEFAULT_MODE);
    };
    let trimmed = raw.trim();
    let octal = trimmed.strip_prefix("0o").unwrap_or(trimmed);
    let parsed = u32::from_str_radix(octal, 8).map_err(|_| InstallError::InvalidMode {
        path: dest.to_path_buf(),
        mode: raw.to_string(),
    })?;
    if parsed > 0o7777 {
        return Err(InstallError::InvalidMode {
            path: dest.to_path_buf(),
            mode: raw.to_string(),
        });
    }
    Ok(parsed)
}

/// Create a file for writing, refusing to reuse or follow anything already at
/// `path`.
///
/// Security-critical, and used for both staged payloads and destination tmp
/// siblings: `O_CREAT|O_EXCL` makes a pre-placed symlink (or any other existing
/// entry) fail the open with `EEXIST`/`ELOOP` instead of letting the write
/// through to a path outside the ANOLISA-owned roots. On Unix `O_NOFOLLOW` is
/// belt-and-suspenders — even on a kernel that resolves `O_CREAT|O_EXCL`
/// race-y against a concurrently planted symlink, the final component cannot be
/// followed. `File::create` does neither: it opens with `O_TRUNC` and follows
/// symlinks, which is exactly the hole this closes.
fn create_exclusive_no_follow(path: &Path) -> Result<File, InstallError> {
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(nix::libc::O_NOFOLLOW);
    }
    opts.open(path).map_err(|source| InstallError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Copy `source` into a freshly created `tmp`, returning its sha256 and length.
fn stream_write_and_hash(
    tmp: &Path,
    source: &mut impl Read,
) -> Result<(String, u64), InstallError> {
    let mut out = create_exclusive_no_follow(tmp)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; STREAM_BUF_SIZE];
    let mut size = 0u64;
    loop {
        let read = source.read(&mut buf).map_err(|source| InstallError::Io {
            path: tmp.to_path_buf(),
            source,
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
        out.write_all(&buf[..read])
            .map_err(|source| InstallError::Io {
                path: tmp.to_path_buf(),
                source,
            })?;
        size += read as u64;
    }
    out.flush().map_err(|source| InstallError::Io {
        path: tmp.to_path_buf(),
        source,
    })?;
    Ok((to_lower_hex(&hasher.finalize()), size))
}

fn tmp_sibling(dest: &Path) -> PathBuf {
    let mut s = dest.as_os_str().to_os_string();
    s.push(".tmp");
    PathBuf::from(s)
}

fn to_lower_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use tar::{Builder, Header};
    use tempfile::tempdir;

    impl ResolvedInstallFile {
        fn dest_only(dest: PathBuf) -> Self {
            Self {
                source: None,
                dest,
                mode: None,
                kind: FileKind::Data,
                render: None,
            }
        }
    }

    impl InstallRunner<'_> {
        fn install_fixture(
            &self,
            artifact_type: &str,
            cached_artifact: &Path,
            destinations: &[PathBuf],
        ) -> Result<InstallOutcome, InstallError> {
            let files: Vec<_> = destinations
                .iter()
                .cloned()
                .map(ResolvedInstallFile::dest_only)
                .collect();
            self.install_mapped_fixture(artifact_type, cached_artifact, &files)
        }

        fn install_mapped_fixture(
            &self,
            artifact_type: &str,
            cached_artifact: &Path,
            files: &[ResolvedInstallFile],
        ) -> Result<InstallOutcome, InstallError> {
            let prepared = self.prepare_files(artifact_type, cached_artifact, files)?;
            self.install_prepared(prepared)
        }
    }
    fn layout_for(home: &Path) -> FsLayout {
        FsLayout::user_with_overrides(home.to_path_buf(), None, None, None, None, None)
    }

    fn write_cached(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, bytes).unwrap();
        p
    }

    fn sha256_of(bytes: &[u8]) -> String {
        let mut h = Sha256::new();
        h.update(bytes);
        to_lower_hex(&h.finalize())
    }

    fn build_tar_gz(entries: &[(&str, &[u8])]) -> Vec<u8> {
        build_tar_gz_with_modes(
            &entries
                .iter()
                .map(|(path, data)| (*path, *data, 0o644))
                .collect::<Vec<_>>(),
        )
    }

    fn build_tar_gz_with_modes(entries: &[(&str, &[u8], u32)]) -> Vec<u8> {
        let buf: Vec<u8> = Vec::new();
        let enc = GzEncoder::new(buf, Compression::default());
        let mut tar = Builder::new(enc);
        for (path, data, mode) in entries {
            let mut hdr = Header::new_gnu();
            hdr.set_size(data.len() as u64);
            hdr.set_mode(*mode);
            hdr.set_cksum();
            tar.append_data(&mut hdr, path, *data).unwrap();
        }
        let enc = tar.into_inner().unwrap();
        enc.finish().unwrap()
    }

    /// Append an entry whose recorded name escapes the archive root.
    ///
    /// `Builder::append_data` refuses to write `..`, so the name goes straight
    /// into the header — the only way to produce the hostile archive the
    /// runner must reject.
    fn build_tar_gz_with_unsafe_entry(safe: &[(&str, &[u8])], unsafe_name: &[u8]) -> Vec<u8> {
        let buf: Vec<u8> = Vec::new();
        let enc = GzEncoder::new(buf, Compression::default());
        let mut tar = Builder::new(enc);
        for (path, data) in safe {
            let mut hdr = Header::new_gnu();
            hdr.set_size(data.len() as u64);
            hdr.set_mode(0o644);
            hdr.set_cksum();
            tar.append_data(&mut hdr, path, *data).unwrap();
        }
        let mut hdr = Header::new_gnu();
        hdr.set_size(1);
        hdr.set_mode(0o644);
        hdr.as_old_mut().name[..unsafe_name.len()].copy_from_slice(unsafe_name);
        hdr.set_cksum();
        tar.append(&hdr, &b"x"[..]).unwrap();
        tar.into_inner().unwrap().finish().unwrap()
    }

    /// Per-preparation staging directories currently on disk. Empty means every
    /// `PreparedFileSet` has been dropped and cleaned up.
    fn staging_dirs(layout: &FsLayout) -> Vec<PathBuf> {
        let Ok(entries) = fs::read_dir(&layout.cache_dir) else {
            return Vec::new();
        };
        let mut dirs: Vec<PathBuf> = entries
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(STAGING_DIR_PREFIX)
                    && entry.file_type().is_ok_and(|kind| kind.is_dir())
            })
            .map(|entry| entry.path())
            .collect();
        dirs.sort();
        dirs
    }

    /// Payload files spooled under every staging directory, sorted by path.
    fn staged_files(layout: &FsLayout) -> Vec<PathBuf> {
        let mut files = Vec::new();
        for dir in staging_dirs(layout) {
            if let Ok(entries) = fs::read_dir(&dir) {
                files.extend(
                    entries
                        .flatten()
                        .filter(|entry| entry.file_name() != STAGING_LOCK_FILE)
                        .map(|entry| entry.path()),
                );
            }
        }
        files.sort();
        files
    }

    fn staged_bytes(layout: &FsLayout) -> u64 {
        staged_files(layout)
            .iter()
            .map(|p| fs::metadata(p).map(|m| m.len()).unwrap_or(0))
            .sum()
    }

    /// Bytes the prepared set keeps in memory: record metadata plus the heap
    /// behind its paths and digests. Must not scale with payload size.
    fn retained_metadata_bytes(prepared: &PreparedFileSet) -> usize {
        prepared
            .regular
            .iter()
            .map(|staged| {
                std::mem::size_of::<PreparedRegularFile>()
                    + staged.staged_path.as_os_str().len()
                    + staged.sha256.len()
                    + staged.file.dest.as_os_str().len()
                    + staged.file.source.as_deref().map_or(0, str::len)
            })
            .sum()
    }

    #[test]
    fn tar_gz_install_unresolved_template_rejected() {
        let home = tempdir().unwrap();
        let cache = tempdir().unwrap();
        let layout = layout_for(home.path());
        let runner = InstallRunner::new(&layout);
        let gz = build_tar_gz(&[("foo", b"x")]);
        let cached = write_cached(cache.path(), "payload.tar.gz", &gz);

        let dest = PathBuf::from("{bindir}/foo");
        let err = runner
            .install_fixture("tar_gz", &cached, std::slice::from_ref(&dest))
            .expect_err("must error");
        match err {
            InstallError::UnresolvedTemplate { path } => assert_eq!(path, dest),
            other => panic!("expected UnresolvedTemplate, got {other:?}"),
        }
    }

    #[test]
    fn tar_gz_install_external_path_rejected() {
        let home = tempdir().unwrap();
        let cache = tempdir().unwrap();
        let layout = layout_for(home.path());
        let runner = InstallRunner::new(&layout);
        let gz = build_tar_gz(&[("foo", b"x")]);
        let cached = write_cached(cache.path(), "payload.tar.gz", &gz);

        let dest = PathBuf::from("/tmp/escape/foo");
        let err = runner
            .install_fixture("tar_gz", &cached, std::slice::from_ref(&dest))
            .expect_err("must error");
        match err {
            InstallError::ExternalPath { path } => assert_eq!(path, dest),
            other => panic!("expected ExternalPath, got {other:?}"),
        }
    }

    #[test]
    fn tar_gz_install_creates_missing_parent_dirs() {
        let home = tempdir().unwrap();
        let cache = tempdir().unwrap();
        let layout = layout_for(home.path());
        let runner = InstallRunner::new(&layout);
        let gz = build_tar_gz(&[("file.bin", b"deep")]);
        let cached = write_cached(cache.path(), "payload.tar.gz", &gz);

        let dest = layout.state_dir.join("sub").join("deep").join("file.bin");
        let outcome = runner
            .install_fixture("tar_gz", &cached, std::slice::from_ref(&dest))
            .expect("install ok");
        assert!(dest.exists());
        assert_eq!(outcome.files[0].sha256, sha256_of(b"deep"));
    }

    #[test]
    fn tar_gz_install_extracts_matching_basenames() {
        let home = tempdir().unwrap();
        let cache = tempdir().unwrap();
        let layout = layout_for(home.path());
        let runner = InstallRunner::new(&layout);

        let bin_bytes: &[u8] = b"agentsight-binary";
        let data_bytes: &[u8] = b"data-file-contents";
        let gz = build_tar_gz(&[
            ("bin/agentsight", bin_bytes),
            ("share/data.toml", data_bytes),
        ]);
        let cached = cache.path().join("payload.tar.gz");
        fs::write(&cached, &gz).unwrap();

        let dest_bin = layout.bin_dir.join("agentsight");
        let dest_data = layout.datadir.join("data.toml");
        let outcome = runner
            .install_fixture("tar_gz", &cached, &[dest_bin.clone(), dest_data.clone()])
            .expect("install ok");

        assert_eq!(outcome.files.len(), 2);
        assert_eq!(fs::read(&dest_bin).unwrap(), bin_bytes);
        assert_eq!(fs::read(&dest_data).unwrap(), data_bytes);
        assert_eq!(outcome.files[0].sha256, sha256_of(bin_bytes));
        assert_eq!(outcome.files[1].sha256, sha256_of(data_bytes));
    }

    #[test]
    fn prepared_preview_matches_the_install_outcome() {
        let home = tempdir().unwrap();
        let cache = tempdir().unwrap();
        let layout = layout_for(home.path());
        let runner = InstallRunner::new(&layout);
        let payload: &[u8] = b"skillfs-binary";
        let gz = build_tar_gz(&[("bin/skillfs", payload)]);
        let cached = write_cached(cache.path(), "payload.tar.gz", &gz);
        let dest = layout.bin_dir.join("skillfs");
        let files = [ResolvedInstallFile::dest_only(dest)];

        let prepared = runner
            .prepare_files("tar_gz", &cached, &files)
            .expect("prepare files");
        let inspected = prepared.preview();
        let installed = runner.install_prepared(prepared).expect("install files");

        assert_eq!(inspected.files, installed.files);
    }

    #[test]
    fn prepared_files_keep_verified_bytes_when_the_cache_changes() {
        let home = tempdir().unwrap();
        let cache = tempdir().unwrap();
        let layout = layout_for(home.path());
        let runner = InstallRunner::new(&layout);
        let original: &[u8] = b"verified-skillfs";
        let changed: &[u8] = b"changed-after-preview";
        let cached = write_cached(
            cache.path(),
            "payload.tar.gz",
            &build_tar_gz(&[("bin/skillfs", original)]),
        );
        let dest = layout.bin_dir.join("skillfs");
        let files = [ResolvedInstallFile::dest_only(dest.clone())];
        let prepared = runner
            .prepare_files("tar_gz", &cached, &files)
            .expect("prepare verified files");
        let expected = prepared.preview();
        std::fs::write(&cached, build_tar_gz(&[("bin/skillfs", changed)]))
            .expect("replace cache entry");

        let installed = runner
            .install_prepared(prepared)
            .expect("install prepared files");

        assert_eq!(installed.files, expected.files);
        assert_eq!(std::fs::read(dest).expect("read installed file"), original);
    }

    #[test]
    fn replacement_preparation_allows_recorded_destinations_until_install() {
        let home = tempdir().unwrap();
        let cache = tempdir().unwrap();
        let layout = layout_for(home.path());
        let runner = InstallRunner::new(&layout);
        let cached = write_cached(
            cache.path(),
            "payload.tar.gz",
            &build_tar_gz(&[("bin/skillfs", b"replacement")]),
        );
        let dest = layout.bin_dir.join("skillfs");
        std::fs::create_dir_all(&layout.bin_dir).expect("create bin dir");
        std::fs::write(&dest, b"recorded").expect("write recorded file");
        let files = [ResolvedInstallFile::dest_only(dest.clone())];

        let prepared = runner
            .prepare_replacement_files("tar_gz", &cached, &files)
            .expect("prepare replacement");
        std::fs::remove_file(&dest).expect("remove recorded file");
        runner
            .install_prepared(prepared)
            .expect("install prepared replacement");

        assert_eq!(
            std::fs::read(dest).expect("read replacement"),
            b"replacement"
        );
    }

    #[test]
    fn tar_gz_install_uses_source_but_writes_dest() {
        let home = tempdir().unwrap();
        let cache = tempdir().unwrap();
        let layout = layout_for(home.path());
        let runner = InstallRunner::new(&layout);

        let payload: &[u8] = b"tool-bytes";
        let gz = build_tar_gz(&[("target/release/source-name", payload)]);
        let cached = cache.path().join("payload.tar.gz");
        fs::write(&cached, &gz).unwrap();

        let dest = layout.bin_dir.join("dest-name");
        let outcome = runner
            .install_mapped_fixture(
                "tar_gz",
                &cached,
                &[ResolvedInstallFile {
                    source: Some("target/release/source-name".to_string()),
                    dest: dest.clone(),
                    mode: None,
                    kind: FileKind::Data,
                    render: None,
                }],
            )
            .expect("install ok");

        assert_eq!(outcome.files.len(), 1);
        assert_eq!(outcome.files[0].path, dest);
        assert_eq!(fs::read(&outcome.files[0].path).unwrap(), payload);
    }

    #[test]
    fn tar_gz_install_expands_directory_source_prefix() {
        let home = tempdir().unwrap();
        let cache = tempdir().unwrap();
        let layout = layout_for(home.path());
        let runner = InstallRunner::new(&layout);

        let manifest: &[u8] = br#"{"name":"tokenless"}"#;
        let script: &[u8] = b"console.log('ok');";
        let gz = build_tar_gz(&[
            ("target/release/openclaw-plugin/plugin.json", manifest),
            ("target/release/openclaw-plugin/dist/index.js", script),
            ("target/release/other-plugin/ignored.txt", b"ignored"),
        ]);
        let cached = cache.path().join("payload.tar.gz");
        fs::write(&cached, &gz).unwrap();

        let dest_root = layout.datadir.join("adapters/tokenless/openclaw");
        let outcome = runner
            .install_mapped_fixture(
                "tar_gz",
                &cached,
                &[ResolvedInstallFile {
                    source: Some("target/release/openclaw-plugin/".to_string()),
                    dest: dest_root.clone(),
                    mode: Some("0644".to_string()),
                    kind: FileKind::Data,
                    render: None,
                }],
            )
            .expect("install ok");

        assert_eq!(outcome.files.len(), 2);
        assert_eq!(fs::read(dest_root.join("plugin.json")).unwrap(), manifest);
        assert_eq!(fs::read(dest_root.join("dist/index.js")).unwrap(), script);
        assert!(!dest_root.join("ignored.txt").exists());
    }

    #[test]
    fn tar_gz_install_rejects_unsafe_archive_paths() {
        let err = archive_key_from_path(Path::new("../escape.txt"))
            .expect_err("must reject unsafe archive path");
        match err {
            InstallError::Archive(msg) => assert!(msg.contains("unsafe archive entry path")),
            other => panic!("expected Archive, got {other:?}"),
        }
    }

    #[test]
    #[cfg(unix)]
    fn install_files_honors_manifest_mode() {
        use std::os::unix::fs::PermissionsExt;

        let home = tempdir().unwrap();
        let cache = tempdir().unwrap();
        let layout = layout_for(home.path());
        let runner = InstallRunner::new(&layout);

        let payload: &[u8] = b"config-bytes";
        let gz = build_tar_gz(&[("share/config.toml", payload)]);
        let cached = cache.path().join("payload.tar.gz");
        fs::write(&cached, &gz).unwrap();

        let dest = layout.datadir.join("config.toml");
        runner
            .install_mapped_fixture(
                "tar_gz",
                &cached,
                &[ResolvedInstallFile {
                    source: Some("share/config.toml".to_string()),
                    dest: dest.clone(),
                    mode: Some("0644".to_string()),
                    kind: FileKind::Data,
                    render: None,
                }],
            )
            .expect("install ok");

        let mode = fs::metadata(dest).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644);
    }

    #[test]
    #[cfg(unix)]
    fn directory_source_without_manifest_mode_preserves_archive_modes() {
        use std::os::unix::fs::PermissionsExt;

        let home = tempdir().unwrap();
        let cache = tempdir().unwrap();
        let layout = layout_for(home.path());
        let runner = InstallRunner::new(&layout);

        let gz = build_tar_gz_with_modes(&[
            (
                "adapters/tokenless/codex/scripts/tool-ready",
                b"#!/usr/bin/env python3\n",
                0o755,
            ),
            (
                "adapters/tokenless/codex/hooks/hooks.json",
                br#"{"hooks":{}}"#,
                0o644,
            ),
        ]);
        let cached = cache.path().join("payload.tar.gz");
        fs::write(&cached, &gz).unwrap();

        let dest_root = layout.datadir.join("adapters/tokenless/codex");
        runner
            .install_mapped_fixture(
                "tar_gz",
                &cached,
                &[ResolvedInstallFile {
                    source: Some("adapters/tokenless/codex/".to_string()),
                    dest: dest_root.clone(),
                    mode: None,
                    kind: FileKind::Data,
                    render: None,
                }],
            )
            .expect("install ok");

        let script_mode = fs::metadata(dest_root.join("scripts/tool-ready"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        let json_mode = fs::metadata(dest_root.join("hooks/hooks.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(script_mode, 0o755);
        assert_eq!(json_mode, 0o644);
    }

    #[test]
    fn invalid_mode_rejected_without_tmp_sibling() {
        let home = tempdir().unwrap();
        let cache = tempdir().unwrap();
        let layout = layout_for(home.path());
        let runner = InstallRunner::new(&layout);
        let gz = build_tar_gz(&[("tool", b"tool-bytes")]);
        let cached = write_cached(cache.path(), "payload.tar.gz", &gz);

        let dest = layout.bin_dir.join("tool");
        let err = runner
            .install_mapped_fixture(
                "tar_gz",
                &cached,
                &[ResolvedInstallFile {
                    source: Some("tool".to_string()),
                    dest: dest.clone(),
                    mode: Some("not-octal".to_string()),
                    kind: FileKind::Data,
                    render: None,
                }],
            )
            .expect_err("must reject invalid mode");
        match err {
            InstallError::InvalidMode { path, .. } => assert_eq!(path, dest),
            other => panic!("expected InvalidMode, got {other:?}"),
        }
        assert!(!dest.exists(), "destination must not be created");
        assert!(!tmp_sibling(&dest).exists(), "tmp sibling must be cleaned");
    }

    #[test]
    fn tar_gz_install_missing_entry_reports_basename() {
        let home = tempdir().unwrap();
        let cache = tempdir().unwrap();
        let layout = layout_for(home.path());
        let runner = InstallRunner::new(&layout);

        let gz = build_tar_gz(&[("bin/something-else", b"x")]);
        let cached = cache.path().join("payload.tar.gz");
        fs::write(&cached, &gz).unwrap();

        let dest = layout.bin_dir.join("missing");
        let err = runner
            .install_fixture("tar_gz", &cached, &[dest])
            .expect_err("must error");
        match err {
            InstallError::MissingArchiveEntry { basename } => assert_eq!(basename, "missing"),
            other => panic!("expected MissingArchiveEntry, got {other:?}"),
        }
    }

    #[test]
    fn binary_artifact_type_rejected() {
        let home = tempdir().unwrap();
        let cache = tempdir().unwrap();
        let layout = layout_for(home.path());
        let runner = InstallRunner::new(&layout);
        let cached = write_cached(cache.path(), "x", b"x");

        let dest = layout.bin_dir.join("a");
        let err = runner
            .install_fixture("binary", &cached, &[dest])
            .expect_err("must error");
        match err {
            InstallError::UnsupportedArtifactType(s) => assert_eq!(s, "binary"),
            other => panic!("expected UnsupportedArtifactType, got {other:?}"),
        }
    }

    #[test]
    fn no_dests_rejected() {
        let home = tempdir().unwrap();
        let cache = tempdir().unwrap();
        let layout = layout_for(home.path());
        let runner = InstallRunner::new(&layout);
        let cached = write_cached(cache.path(), "x", b"x");

        let err = runner
            .install_fixture("tar_gz", &cached, &[])
            .expect_err("must error");
        assert!(matches!(err, InstallError::NoDestinations));
    }

    #[test]
    fn tar_gz_install_refuses_when_any_dest_preexists() {
        // Pre-existence check runs before extraction, so neither dest is
        // written even if only one of them collides.
        let home = tempdir().unwrap();
        let cache = tempdir().unwrap();
        let layout = layout_for(home.path());
        let runner = InstallRunner::new(&layout);

        let bin_bytes: &[u8] = b"agentsight-binary";
        let data_bytes: &[u8] = b"data-file-contents";
        let gz = build_tar_gz(&[
            ("bin/agentsight", bin_bytes),
            ("share/data.toml", data_bytes),
        ]);
        let cached = cache.path().join("payload.tar.gz");
        fs::write(&cached, &gz).unwrap();

        let dest_bin = layout.bin_dir.join("agentsight");
        let dest_data = layout.datadir.join("data.toml");
        std::fs::create_dir_all(dest_data.parent().unwrap()).unwrap();
        std::fs::write(&dest_data, b"existing-data").unwrap();

        let err = runner
            .install_fixture("tar_gz", &cached, &[dest_bin.clone(), dest_data.clone()])
            .expect_err("must refuse");
        match err {
            InstallError::DestExists { path } => assert_eq!(path, dest_data),
            other => panic!("expected DestExists, got {other:?}"),
        }
        assert!(!dest_bin.exists(), "bin dest must not be created");
        assert_eq!(std::fs::read(&dest_data).unwrap(), b"existing-data");
    }

    #[test]
    fn tar_gz_install_dotdot_segment_rejected() {
        let home = tempdir().unwrap();
        let cache = tempdir().unwrap();
        let layout = layout_for(home.path());
        let runner = InstallRunner::new(&layout);
        let gz = build_tar_gz(&[("file", b"x")]);
        let cached = write_cached(cache.path(), "payload.tar.gz", &gz);

        // dest = <bin_dir>/../escape/file — passes the old lexical
        // starts_with check but would write outside bin_dir.
        let dest = layout.bin_dir.join("..").join("escape").join("file");
        let err = runner
            .install_mapped_fixture(
                "tar_gz",
                &cached,
                &[ResolvedInstallFile {
                    source: Some("file".to_string()),
                    dest: dest.clone(),
                    mode: None,
                    kind: FileKind::Data,
                    render: None,
                }],
            )
            .expect_err("must reject");
        match err {
            InstallError::TraversalSegment { path } => assert_eq!(path, dest),
            other => panic!("expected TraversalSegment, got {other:?}"),
        }
    }

    #[test]
    fn tar_gz_install_dotdot_at_tail_rejected() {
        // `..` as the final segment would resolve to a directory and let
        // rename overwrite something the user did not name. Same defense
        // as the mid-path case but covers the tail position explicitly.
        let home = tempdir().unwrap();
        let cache = tempdir().unwrap();
        let layout = layout_for(home.path());
        let runner = InstallRunner::new(&layout);
        let gz = build_tar_gz(&[("file", b"x")]);
        let cached = write_cached(cache.path(), "payload.tar.gz", &gz);

        let dest = layout.bin_dir.join("sub").join("..");
        let err = runner
            .install_mapped_fixture(
                "tar_gz",
                &cached,
                &[ResolvedInstallFile {
                    source: Some("file".to_string()),
                    dest: dest.clone(),
                    mode: None,
                    kind: FileKind::Data,
                    render: None,
                }],
            )
            .expect_err("must reject");
        match err {
            InstallError::TraversalSegment { path } => assert_eq!(path, dest),
            other => panic!("expected TraversalSegment, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn tar_gz_install_refuses_broken_symlink_dest() {
        // exists() returns false for a broken symlink (target missing) but
        // symlink_metadata() returns Ok. We must treat the broken symlink
        // as "occupied" and refuse, otherwise rename() would silently
        // replace it.
        let home = tempdir().unwrap();
        let cache = tempdir().unwrap();
        let layout = layout_for(home.path());
        let runner = InstallRunner::new(&layout);
        let gz = build_tar_gz(&[("agentsight", b"new-bytes")]);
        let cached = write_cached(cache.path(), "payload.tar.gz", &gz);

        let dest = layout.bin_dir.join("agentsight");
        fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink("/nonexistent/target", &dest).unwrap();
        assert!(!dest.exists(), "test precondition: broken symlink");
        assert!(
            fs::symlink_metadata(&dest).is_ok(),
            "symlink itself present"
        );

        let err = runner
            .install_fixture("tar_gz", &cached, std::slice::from_ref(&dest))
            .expect_err("must refuse");
        match err {
            InstallError::DestExists { path } => assert_eq!(path, dest),
            other => panic!("expected DestExists, got {other:?}"),
        }
        // Symlink untouched.
        assert!(fs::symlink_metadata(&dest).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn tar_gz_install_symlink_ancestor_escapes_root_rejected() {
        // bin_dir/escape -> <outside>, dest = bin_dir/escape/file. The
        // lexical starts_with check passes (it's literally under bin_dir),
        // but canonicalize_nearest_existing resolves the symlink and the
        // canonical dest no longer lives under the canonical root.
        let home = tempdir().unwrap();
        let cache = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let layout = layout_for(home.path());
        let runner = InstallRunner::new(&layout);
        let gz = build_tar_gz(&[("file", b"x")]);
        let cached = write_cached(cache.path(), "payload.tar.gz", &gz);

        fs::create_dir_all(&layout.bin_dir).unwrap();
        let escape_link = layout.bin_dir.join("escape");
        std::os::unix::fs::symlink(outside.path(), &escape_link).unwrap();

        let dest = escape_link.join("file");
        let err = runner
            .install_fixture("tar_gz", &cached, std::slice::from_ref(&dest))
            .expect_err("must reject");
        assert!(
            matches!(err, InstallError::ExternalPath { ref path } if path == &dest),
            "expected ExternalPath for symlink-escape, got {err:?}",
        );
        assert!(
            !outside.path().join("file").exists(),
            "must not write through the symlink",
        );
    }

    #[cfg(unix)]
    #[test]
    fn tar_gz_install_refuses_when_tmp_sibling_is_a_symlink() {
        // Same defense applies to the tar_gz backend — it routes through
        // the same `write_dest_atomic` helper so a single fix covers both,
        // but we lock that down with an explicit regression test so a
        // future refactor that splits the helpers cannot regress one
        // backend without tripping a test.
        let home = tempdir().unwrap();
        let cache = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let layout = layout_for(home.path());
        let runner = InstallRunner::new(&layout);

        let gz = build_tar_gz(&[
            ("bin/first", b"first-bytes"),
            ("bin/agentsight", b"new-bytes"),
        ]);
        let cached = cache.path().join("payload.tar.gz");
        fs::write(&cached, &gz).unwrap();

        let first_dest = layout.bin_dir.join("first");
        let dest = layout.bin_dir.join("agentsight");
        fs::create_dir_all(dest.parent().unwrap()).unwrap();
        let outside_target = outside.path().join("victim");
        fs::write(&outside_target, b"untouched-bytes").unwrap();
        let tmp_plant = {
            let mut s = dest.as_os_str().to_os_string();
            s.push(".tmp");
            PathBuf::from(s)
        };
        std::os::unix::fs::symlink(&outside_target, &tmp_plant).unwrap();

        let err = runner
            .install_fixture("tar_gz", &cached, &[first_dest.clone(), dest.clone()])
            .expect_err("must refuse to write through symlinked tmp");
        match err {
            InstallError::Io { path, .. } => assert_eq!(path, tmp_plant),
            other => panic!("expected Io on tmp, got {other:?}"),
        }

        let victim_bytes = fs::read(&outside_target).expect("external file readable");
        assert_eq!(victim_bytes, b"untouched-bytes");
        assert!(!dest.exists());
        assert!(
            !first_dest.exists(),
            "earlier tar_gz writes must roll back when a later write fails"
        );
    }

    #[test]
    fn tar_gz_external_dest_rejected_before_extraction() {
        let home = tempdir().unwrap();
        let cache = tempdir().unwrap();
        let layout = layout_for(home.path());
        let runner = InstallRunner::new(&layout);

        let gz = build_tar_gz(&[("bin/foo", b"foo-bytes")]);
        let cached = cache.path().join("payload.tar.gz");
        fs::write(&cached, &gz).unwrap();

        let dest = PathBuf::from("/tmp/escape/foo");
        let err = runner
            .install_fixture("tar_gz", &cached, &[dest])
            .expect_err("must error");
        assert!(matches!(err, InstallError::ExternalPath { .. }));
        let leaked = layout.bin_dir.join("foo");
        assert!(!leaked.exists(), "must not extract before validating dest");
    }

    fn symlink_entry(referent: &Path, dest: PathBuf) -> ResolvedInstallFile {
        ResolvedInstallFile {
            source: Some(referent.to_string_lossy().into_owned()),
            dest,
            mode: None,
            kind: FileKind::Symlink,
            render: None,
        }
    }

    #[test]
    fn symlink_created_after_regular_files_with_referent_hash() {
        let home = tempdir().unwrap();
        let cache = tempdir().unwrap();
        let layout = layout_for(home.path());
        let runner = InstallRunner::new(&layout);

        let payload: &[u8] = b"rtk-bytes";
        let gz = build_tar_gz(&[("libexec/anolisa/tokenless/rtk", payload)]);
        let cached = cache.path().join("payload.tar.gz");
        fs::write(&cached, &gz).unwrap();

        let referent = layout.libexec_dir.join("tokenless").join("rtk");
        let link_dest = layout.bin_dir.join("rtk");
        let files = vec![
            ResolvedInstallFile {
                source: Some("libexec/anolisa/tokenless/rtk".into()),
                dest: referent.clone(),
                mode: Some("0755".into()),
                kind: FileKind::Data,
                render: None,
            },
            symlink_entry(&referent, link_dest.clone()),
        ];

        let outcome = runner
            .install_mapped_fixture("tar_gz", &cached, &files)
            .expect("install ok");

        assert!(fs::symlink_metadata(&link_dest).unwrap().is_symlink());
        assert_eq!(fs::read_link(&link_dest).unwrap(), referent);
        // Symlinks carry the referent path instead of a content hash.
        let link_file = outcome
            .files
            .iter()
            .find(|f| f.path == link_dest)
            .expect("link recorded in outcome");
        assert!(link_file.sha256.is_empty());
        assert_eq!(link_file.referent.as_deref(), Some(referent.as_path()));
    }

    #[test]
    fn symlink_without_source_rejected_before_any_write() {
        let home = tempdir().unwrap();
        let cache = tempdir().unwrap();
        let layout = layout_for(home.path());
        let runner = InstallRunner::new(&layout);

        let gz = build_tar_gz(&[("bin/foo", b"foo-bytes")]);
        let cached = cache.path().join("payload.tar.gz");
        fs::write(&cached, &gz).unwrap();

        let regular_dest = layout.bin_dir.join("foo");
        let link_dest = layout.bin_dir.join("foo-link");
        let files = vec![
            ResolvedInstallFile::dest_only(regular_dest.clone()),
            ResolvedInstallFile {
                source: None,
                dest: link_dest.clone(),
                mode: None,
                kind: FileKind::Symlink,
                render: None,
            },
        ];

        let err = runner
            .install_mapped_fixture("tar_gz", &cached, &files)
            .expect_err("must error");
        match err {
            InstallError::SymlinkMissingSource { path } => assert_eq!(path, link_dest),
            other => panic!("expected SymlinkMissingSource, got {other:?}"),
        }
        assert!(!regular_dest.exists(), "must validate links before writing");
    }

    #[test]
    fn symlink_dest_exists_rejected_even_for_broken_link() {
        let home = tempdir().unwrap();
        let cache = tempdir().unwrap();
        let layout = layout_for(home.path());
        let runner = InstallRunner::new(&layout);

        let gz = build_tar_gz(&[("bin/foo", b"foo-bytes")]);
        let cached = cache.path().join("payload.tar.gz");
        fs::write(&cached, &gz).unwrap();

        let referent = layout.bin_dir.join("foo");
        let link_dest = layout.bin_dir.join("foo-link");
        fs::create_dir_all(link_dest.parent().unwrap()).unwrap();
        // Pre-existing *broken* link: plain exists() would miss it.
        std::os::unix::fs::symlink(layout.bin_dir.join("missing"), &link_dest).unwrap();

        let files = vec![
            ResolvedInstallFile::dest_only(referent),
            symlink_entry(&layout.bin_dir.join("foo"), link_dest.clone()),
        ];
        let err = runner
            .install_mapped_fixture("tar_gz", &cached, &files)
            .expect_err("must error");
        match err {
            InstallError::DestExists { path } => assert_eq!(path, link_dest),
            other => panic!("expected DestExists, got {other:?}"),
        }
    }

    #[test]
    fn symlink_referent_outside_owned_roots_rejected() {
        let home = tempdir().unwrap();
        let cache = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let layout = layout_for(home.path());
        let runner = InstallRunner::new(&layout);

        let gz = build_tar_gz(&[("bin/foo", b"foo-bytes")]);
        let cached = cache.path().join("payload.tar.gz");
        fs::write(&cached, &gz).unwrap();

        let external = outside.path().join("victim");
        let files = vec![
            ResolvedInstallFile::dest_only(layout.bin_dir.join("foo")),
            symlink_entry(&external, layout.bin_dir.join("foo-link")),
        ];
        let err = runner
            .install_mapped_fixture("tar_gz", &cached, &files)
            .expect_err("must error");
        match err {
            InstallError::ExternalPath { path } => assert_eq!(path, external),
            other => panic!("expected ExternalPath, got {other:?}"),
        }
    }

    #[test]
    fn symlink_dangling_referent_rejected_and_link_removed() {
        let home = tempdir().unwrap();
        let cache = tempdir().unwrap();
        let layout = layout_for(home.path());
        let runner = InstallRunner::new(&layout);

        let gz = build_tar_gz(&[("bin/foo", b"foo-bytes")]);
        let cached = cache.path().join("payload.tar.gz");
        fs::write(&cached, &gz).unwrap();

        // Referent is owned but nothing installs it: the link would dangle.
        let referent = layout.libexec_dir.join("tokenless").join("missing");
        let link_dest = layout.bin_dir.join("missing-link");
        let regular_dest = layout.bin_dir.join("foo");
        let files = vec![
            ResolvedInstallFile::dest_only(regular_dest.clone()),
            symlink_entry(&referent, link_dest.clone()),
        ];
        let err = runner
            .install_mapped_fixture("tar_gz", &cached, &files)
            .expect_err("must error");
        match err {
            InstallError::Io { path, .. } => assert_eq!(path, referent),
            other => panic!("expected Io on referent, got {other:?}"),
        }
        assert!(
            fs::symlink_metadata(&link_dest).is_err(),
            "dangling link must not be left behind"
        );
        assert!(
            !regular_dest.exists(),
            "regular files written before the failed link must be rolled back"
        );
    }

    #[test]
    fn links_only_manifest_rejected() {
        let home = tempdir().unwrap();
        let cache = tempdir().unwrap();
        let layout = layout_for(home.path());
        let runner = InstallRunner::new(&layout);
        let cached = write_cached(cache.path(), "x", b"x");

        let files = vec![symlink_entry(
            &layout.bin_dir.join("foo"),
            layout.bin_dir.join("foo-link"),
        )];
        let err = runner
            .install_mapped_fixture("tar_gz", &cached, &files)
            .expect_err("must error");
        assert!(matches!(err, InstallError::NoDestinations));
    }

    // -- content rendering (`render = "anolisa-paths-v1"`) ------------------

    fn render_entry(source: &str, dest: PathBuf) -> ResolvedInstallFile {
        ResolvedInstallFile {
            source: Some(source.to_string()),
            dest,
            mode: Some("0644".to_string()),
            kind: FileKind::Data,
            render: Some(RenderSpec {
                mode: RenderMode::AnolisaPathsV1,
                component: "sec-core".to_string(),
            }),
        }
    }

    #[test]
    fn render_substitutes_placeholders_and_hashes_rendered_bytes() {
        let home = tempdir().unwrap();
        let cache = tempdir().unwrap();
        let layout = layout_for(home.path());
        let runner = InstallRunner::new(&layout);

        let template = "[Service]\nExecStart=\"{bindir}/agent-sec-daemon\" serve\nReadWritePaths=\"{datadir}\"\n";
        let gz = build_tar_gz(&[("share/agent-sec-core.service.in", template.as_bytes())]);
        let cached = write_cached(cache.path(), "payload.tar.gz", &gz);

        let dest = layout.datadir.join("agent-sec-core.service");
        let outcome = runner
            .install_mapped_fixture(
                "tar_gz",
                &cached,
                &[render_entry(
                    "share/agent-sec-core.service.in",
                    dest.clone(),
                )],
            )
            .expect("install ok");

        let expected = template
            .replace("{bindir}", &layout.bin_dir.to_string_lossy())
            .replace("{datadir}", &layout.datadir.to_string_lossy());
        let installed = fs::read_to_string(&dest).unwrap();
        assert_eq!(installed, expected, "placeholders must be substituted");
        assert!(
            !installed.contains('{'),
            "no literal placeholder may survive rendering"
        );
        // State records the sha256 of the *rendered* bytes, so integrity
        // verification over the installed file passes.
        assert_eq!(outcome.files[0].sha256, sha256_of(expected.as_bytes()));
    }

    #[test]
    fn render_expands_component_variable() {
        let home = tempdir().unwrap();
        let cache = tempdir().unwrap();
        let layout = layout_for(home.path());
        let runner = InstallRunner::new(&layout);

        let gz = build_tar_gz(&[("conf.in", b"root={datadir}/adapters/{component}\n")]);
        let cached = write_cached(cache.path(), "payload.tar.gz", &gz);

        let dest = layout.etc_dir.join("conf");
        runner
            .install_mapped_fixture("tar_gz", &cached, &[render_entry("conf.in", dest.clone())])
            .expect("install ok");
        assert_eq!(
            fs::read_to_string(&dest).unwrap(),
            format!("root={}/adapters/sec-core\n", layout.datadir.display())
        );
    }

    #[test]
    fn render_preserves_env_refs_and_expands_nested_placeholders() {
        let home = tempdir().unwrap();
        let cache = tempdir().unwrap();
        let layout = layout_for(home.path());
        let runner = InstallRunner::new(&layout);

        // Shape taken from cosh-gateway@.service.in: a layout placeholder and
        // an EnvironmentFile-backed reference on one line, plus a parameter
        // expansion whose default is itself a layout placeholder.
        let template = concat!(
            "ExecStart=\"{libexecdir}/cosh-gateway\" --workspace=${COSH_GATEWAY_WORKSPACE}\n",
            "Environment=SKILLS=${SKILL_ROOT:-{datadir}/skills}\n"
        );
        let gz = build_tar_gz(&[("unit.in", template.as_bytes())]);
        let cached = write_cached(cache.path(), "payload.tar.gz", &gz);

        let dest = layout.datadir.join("cosh-gateway@.service");
        runner
            .install_mapped_fixture("tar_gz", &cached, &[render_entry("unit.in", dest.clone())])
            .expect("install ok");

        assert_eq!(
            fs::read_to_string(&dest).unwrap(),
            format!(
                "ExecStart=\"{}/cosh-gateway\" --workspace=${{COSH_GATEWAY_WORKSPACE}}\n\
                 Environment=SKILLS=${{SKILL_ROOT:-{}/skills}}\n",
                layout.libexec_dir.display(),
                layout.datadir.display()
            )
        );
    }

    #[test]
    fn render_unknown_placeholder_nested_in_env_default_rejected() {
        let home = tempdir().unwrap();
        let cache = tempdir().unwrap();
        let layout = layout_for(home.path());
        let runner = InstallRunner::new(&layout);

        let gz = build_tar_gz(&[("unit.in", b"Environment=X=${ROOT:-{no_such_dir}}\n")]);
        let cached = write_cached(cache.path(), "payload.tar.gz", &gz);

        let dest = layout.datadir.join("unit");
        let err = runner
            .install_mapped_fixture("tar_gz", &cached, &[render_entry("unit.in", dest.clone())])
            .expect_err("must reject unknown placeholder inside an env default");
        match err {
            InstallError::Render { reason, .. } => assert!(
                reason.contains("no_such_dir"),
                "reason must name the placeholder: {reason}"
            ),
            other => panic!("expected Render, got {other:?}"),
        }
        assert!(!dest.exists(), "nothing may land on a failed render");
    }

    #[test]
    fn render_unknown_placeholder_rejected() {
        let home = tempdir().unwrap();
        let cache = tempdir().unwrap();
        let layout = layout_for(home.path());
        let runner = InstallRunner::new(&layout);

        let gz = build_tar_gz(&[("unit.in", b"ExecStart={no_such_dir}/daemon\n")]);
        let cached = write_cached(cache.path(), "payload.tar.gz", &gz);

        let dest = layout.datadir.join("unit");
        let err = runner
            .install_mapped_fixture("tar_gz", &cached, &[render_entry("unit.in", dest.clone())])
            .expect_err("must reject unknown placeholder");
        match err {
            InstallError::Render { path, reason } => {
                assert_eq!(path, dest);
                assert!(
                    reason.contains("no_such_dir"),
                    "reason must name the placeholder: {reason}"
                );
            }
            other => panic!("expected Render, got {other:?}"),
        }
        assert!(!dest.exists(), "nothing may land on a failed render");
    }

    #[test]
    fn render_non_utf8_content_rejected() {
        let home = tempdir().unwrap();
        let cache = tempdir().unwrap();
        let layout = layout_for(home.path());
        let runner = InstallRunner::new(&layout);

        let gz = build_tar_gz(&[("blob.in", &[0xff, 0xfe, 0x00, 0x7b][..])]);
        let cached = write_cached(cache.path(), "payload.tar.gz", &gz);

        let dest = layout.datadir.join("blob");
        let err = runner
            .install_mapped_fixture("tar_gz", &cached, &[render_entry("blob.in", dest.clone())])
            .expect_err("must reject non-UTF-8 content");
        match err {
            InstallError::Render { path, reason } => {
                assert_eq!(path, dest);
                assert!(reason.contains("UTF-8"), "reason must say UTF-8: {reason}");
            }
            other => panic!("expected Render, got {other:?}"),
        }
    }

    #[test]
    fn render_on_directory_source_rejected() {
        let home = tempdir().unwrap();
        let cache = tempdir().unwrap();
        let layout = layout_for(home.path());
        let runner = InstallRunner::new(&layout);

        let gz = build_tar_gz(&[("tree/file", b"x")]);
        let cached = write_cached(cache.path(), "payload.tar.gz", &gz);

        let dest = layout.datadir.join("tree");
        let err = runner
            .install_mapped_fixture("tar_gz", &cached, &[render_entry("tree/", dest.clone())])
            .expect_err("must reject render on a directory source");
        match err {
            InstallError::Render { path, .. } => assert_eq!(path, dest),
            other => panic!("expected Render, got {other:?}"),
        }
    }

    #[test]
    fn render_on_symlink_rejected() {
        let home = tempdir().unwrap();
        let cache = tempdir().unwrap();
        let layout = layout_for(home.path());
        let runner = InstallRunner::new(&layout);

        let gz = build_tar_gz(&[("bin/foo", b"x")]);
        let cached = write_cached(cache.path(), "payload.tar.gz", &gz);

        let referent = layout.bin_dir.join("foo");
        let link_dest = layout.bin_dir.join("foo-link");
        let mut link = symlink_entry(&referent, link_dest.clone());
        link.render = Some(RenderSpec {
            mode: RenderMode::AnolisaPathsV1,
            component: "sec-core".to_string(),
        });
        let files = vec![
            ResolvedInstallFile {
                source: Some("bin/foo".to_string()),
                dest: referent,
                mode: None,
                kind: FileKind::Data,
                render: None,
            },
            link,
        ];
        let err = runner
            .install_mapped_fixture("tar_gz", &cached, &files)
            .expect_err("must reject render on a symlink");
        match err {
            InstallError::Render { path, .. } => assert_eq!(path, link_dest),
            other => panic!("expected Render, got {other:?}"),
        }
    }

    #[test]
    fn render_mode_parse_accepts_only_v1() {
        assert_eq!(
            RenderMode::parse("anolisa-paths-v1"),
            Some(RenderMode::AnolisaPathsV1)
        );
        assert_eq!(RenderMode::parse("anolisa-paths-v2"), None);
        assert_eq!(RenderMode::parse(""), None);
    }

    // -- disk staging (no whole-archive payload in memory) -------------------

    #[test]
    fn prepared_set_spools_payload_to_staging_under_cache_dir() {
        let home = tempdir().unwrap();
        let cache = tempdir().unwrap();
        let layout = layout_for(home.path());
        let runner = InstallRunner::new(&layout);

        let payload = vec![0xa5u8; 3 * 1024 * 1024];
        let cached = write_cached(
            cache.path(),
            "payload.tar.gz",
            &build_tar_gz(&[("bin/tool", &payload)]),
        );
        let dest = layout.bin_dir.join("tool");
        let files = [ResolvedInstallFile::dest_only(dest.clone())];

        let prepared = runner
            .prepare_files("tar_gz", &cached, &files)
            .expect("prepare files");

        // The payload lives in a private per-preparation directory directly
        // under cache_dir, not in the prepared record.
        let staged = &prepared.regular[0];
        assert!(
            staged.staged_path.starts_with(&layout.cache_dir),
            "staging must live under cache_dir, got {}",
            staged.staged_path.display()
        );
        assert_eq!(staging_dirs(&layout).len(), 1);
        assert_eq!(staged.size, payload.len() as u64);
        assert_eq!(staged.sha256, sha256_of(&payload));
        assert_eq!(
            fs::metadata(&staged.staged_path).unwrap().len(),
            payload.len() as u64,
            "whole entry must be spooled to disk"
        );
        assert_eq!(staged_bytes(&layout), payload.len() as u64);
        assert!(
            retained_metadata_bytes(&prepared) < 1024,
            "prepared record must hold metadata only, not {} bytes",
            retained_metadata_bytes(&prepared)
        );

        runner.install_prepared(prepared).expect("install prepared");
        assert_eq!(fs::read(&dest).unwrap(), payload);
        assert!(
            staging_dirs(&layout).is_empty(),
            "staging must be cleaned up after a successful install"
        );
    }

    #[test]
    fn only_contract_selected_entries_are_staged() {
        let home = tempdir().unwrap();
        let cache = tempdir().unwrap();
        let layout = layout_for(home.path());
        let runner = InstallRunner::new(&layout);

        let wanted: &[u8] = b"wanted-payload";
        let gz = build_tar_gz(&[
            ("bin/wanted", wanted),
            ("bin/unwanted", &[7u8; 4096][..]),
            ("share/also-unwanted", &[9u8; 4096][..]),
        ]);
        let cached = write_cached(cache.path(), "payload.tar.gz", &gz);
        let files = [ResolvedInstallFile::dest_only(
            layout.bin_dir.join("wanted"),
        )];

        let prepared = runner
            .prepare_files("tar_gz", &cached, &files)
            .expect("prepare files");

        assert_eq!(
            staged_files(&layout).len(),
            1,
            "entries no destination maps to must never be spooled"
        );
        assert_eq!(staged_bytes(&layout), wanted.len() as u64);
        drop(prepared);
    }

    #[test]
    fn prepared_directory_source_installs_original_after_cache_is_replaced() {
        let home = tempdir().unwrap();
        let cache = tempdir().unwrap();
        let layout = layout_for(home.path());
        let runner = InstallRunner::new(&layout);

        let plugin: &[u8] = br#"{"name":"original"}"#;
        let script: &[u8] = b"console.log('original');";
        let cached = write_cached(
            cache.path(),
            "payload.tar.gz",
            &build_tar_gz(&[("tree/plugin.json", plugin), ("tree/dist/index.js", script)]),
        );
        let dest_root = layout.datadir.join("adapters/openclaw");
        let files = [ResolvedInstallFile {
            source: Some("tree/".to_string()),
            dest: dest_root.clone(),
            mode: Some("0644".to_string()),
            kind: FileKind::Data,
            render: None,
        }];

        let prepared = runner
            .prepare_files("tar_gz", &cached, &files)
            .expect("prepare files");
        let preview = prepared.preview();

        // Swap the cache for a different archive, then delete it outright: the
        // staged payloads are the only source placement may use.
        fs::write(
            &cached,
            build_tar_gz(&[
                ("tree/plugin.json", br#"{"name":"tampered"}"#),
                ("tree/dist/index.js", b"console.log('tampered');"),
            ]),
        )
        .unwrap();
        fs::remove_file(&cached).unwrap();

        let installed = runner.install_prepared(prepared).expect("install prepared");

        assert_eq!(installed.files, preview.files, "preview must match install");
        assert_eq!(fs::read(dest_root.join("plugin.json")).unwrap(), plugin);
        assert_eq!(fs::read(dest_root.join("dist/index.js")).unwrap(), script);
        assert!(staging_dirs(&layout).is_empty(), "staging must be cleaned");
    }

    #[test]
    fn preview_digests_match_installed_digests_across_entry_kinds() {
        let home = tempdir().unwrap();
        let cache = tempdir().unwrap();
        let layout = layout_for(home.path());
        let runner = InstallRunner::new(&layout);

        let template = "root={datadir}/adapters/{component}\n";
        let cached = write_cached(
            cache.path(),
            "payload.tar.gz",
            &build_tar_gz(&[
                ("tree/a.bin", b"tree-a"),
                ("tree/nested/b.bin", b"tree-b"),
                ("conf.in", template.as_bytes()),
                ("libexec/anolisa/tokenless/rtk", b"rtk-bytes"),
            ]),
        );
        let referent = layout.libexec_dir.join("tokenless").join("rtk");
        let files = vec![
            ResolvedInstallFile {
                source: Some("tree/".to_string()),
                dest: layout.datadir.join("tree"),
                mode: Some("0644".to_string()),
                kind: FileKind::Data,
                render: None,
            },
            render_entry("conf.in", layout.etc_dir.join("conf")),
            ResolvedInstallFile {
                source: Some("libexec/anolisa/tokenless/rtk".to_string()),
                dest: referent.clone(),
                mode: Some("0755".to_string()),
                kind: FileKind::Data,
                render: None,
            },
            symlink_entry(&referent, layout.bin_dir.join("rtk")),
        ];

        let prepared = runner
            .prepare_files("tar_gz", &cached, &files)
            .expect("prepare files");
        let preview = prepared.preview();
        let installed = runner.install_prepared(prepared).expect("install prepared");

        assert_eq!(installed.files, preview.files);
        // Rendered content is hashed as it will land, not as it shipped.
        let rendered = format!("root={}/adapters/sec-core\n", layout.datadir.display());
        let conf = installed
            .files
            .iter()
            .find(|f| f.path == layout.etc_dir.join("conf"))
            .expect("rendered file recorded");
        assert_eq!(conf.sha256, sha256_of(rendered.as_bytes()));
        assert_eq!(fs::read_to_string(&conf.path).unwrap(), rendered);
    }

    #[test]
    fn missing_entry_cleans_staging_and_leaves_destinations_untouched() {
        let home = tempdir().unwrap();
        let cache = tempdir().unwrap();
        let layout = layout_for(home.path());
        let runner = InstallRunner::new(&layout);

        let cached = write_cached(
            cache.path(),
            "payload.tar.gz",
            &build_tar_gz(&[("bin/present", b"present-bytes")]),
        );
        let present = layout.bin_dir.join("present");
        let absent = layout.bin_dir.join("absent");

        let err = runner
            .prepare_files(
                "tar_gz",
                &cached,
                &[
                    ResolvedInstallFile::dest_only(present.clone()),
                    ResolvedInstallFile::dest_only(absent.clone()),
                ],
            )
            .expect_err("must reject a missing archive entry");
        match err {
            InstallError::MissingArchiveEntry { basename } => assert_eq!(basename, "absent"),
            other => panic!("expected MissingArchiveEntry, got {other:?}"),
        }
        assert!(
            staging_dirs(&layout).is_empty(),
            "a failed preparation must leave no staged payloads"
        );
        assert!(!present.exists(), "no destination may be created");
        assert!(!absent.exists());
    }

    #[test]
    fn unsafe_archive_path_cleans_staging_and_leaves_destinations_untouched() {
        let home = tempdir().unwrap();
        let cache = tempdir().unwrap();
        let layout = layout_for(home.path());
        let runner = InstallRunner::new(&layout);

        // The selected entry comes first, so a payload is already spooled when
        // the hostile entry aborts the pass.
        let gz = build_tar_gz_with_unsafe_entry(&[("bin/tool", b"tool-bytes")], b"../escape.txt");
        let cached = write_cached(cache.path(), "payload.tar.gz", &gz);
        let dest = layout.bin_dir.join("tool");

        let err = runner
            .prepare_files(
                "tar_gz",
                &cached,
                &[ResolvedInstallFile::dest_only(dest.clone())],
            )
            .expect_err("must reject an escaping archive entry");
        match err {
            InstallError::Archive(msg) => assert!(
                msg.contains("unsafe archive entry path"),
                "unexpected message: {msg}"
            ),
            other => panic!("expected Archive, got {other:?}"),
        }
        assert!(
            staging_dirs(&layout).is_empty(),
            "staged payloads must be dropped when the archive is rejected"
        );
        assert!(!dest.exists(), "no destination may be created");
        assert!(!home.path().join("escape.txt").exists());
    }

    #[test]
    fn duplicate_destination_cleans_staging_and_leaves_destination_untouched() {
        let home = tempdir().unwrap();
        let cache = tempdir().unwrap();
        let layout = layout_for(home.path());
        let runner = InstallRunner::new(&layout);

        let cached = write_cached(
            cache.path(),
            "payload.tar.gz",
            &build_tar_gz(&[("bin/tool", b"tool-bytes"), ("share/tool", b"other-bytes")]),
        );
        let dest = layout.bin_dir.join("tool");
        let duplicate = [
            ResolvedInstallFile {
                source: Some("bin/tool".to_string()),
                dest: dest.clone(),
                mode: None,
                kind: FileKind::Data,
                render: None,
            },
            ResolvedInstallFile {
                source: Some("share/tool".to_string()),
                dest: dest.clone(),
                mode: None,
                kind: FileKind::Data,
                render: None,
            },
        ];

        let err = runner
            .prepare_files("tar_gz", &cached, &duplicate)
            .expect_err("must reject a duplicate destination");
        match err {
            InstallError::DuplicateDestination { path } => assert_eq!(path, dest),
            other => panic!("expected DuplicateDestination, got {other:?}"),
        }
        assert!(!dest.exists(), "destination must not be touched");
        assert!(staging_dirs(&layout).is_empty(), "staging must be cleaned");
    }

    #[cfg(unix)]
    #[test]
    fn placement_failure_rolls_back_and_cleans_staging_and_tmp() {
        let home = tempdir().unwrap();
        let cache = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let layout = layout_for(home.path());
        let runner = InstallRunner::new(&layout);

        let cached = write_cached(
            cache.path(),
            "payload.tar.gz",
            &build_tar_gz(&[
                ("bin/first", b"first-bytes"),
                ("bin/second", b"second-bytes"),
            ]),
        );
        let first = layout.bin_dir.join("first");
        let second = layout.bin_dir.join("second");
        let files = [
            ResolvedInstallFile::dest_only(first.clone()),
            ResolvedInstallFile::dest_only(second.clone()),
        ];

        let prepared = runner
            .prepare_files("tar_gz", &cached, &files)
            .expect("prepare files");
        assert_eq!(staged_files(&layout).len(), 2);

        // Plant a symlinked tmp sibling so the *second* placement fails after
        // the first already landed.
        fs::create_dir_all(&layout.bin_dir).unwrap();
        let victim = outside.path().join("victim");
        fs::write(&victim, b"untouched").unwrap();
        std::os::unix::fs::symlink(&victim, tmp_sibling(&second)).unwrap();

        let err = runner
            .install_prepared(prepared)
            .expect_err("must refuse to write through a symlinked tmp sibling");
        assert!(matches!(err, InstallError::Io { .. }), "got {err:?}");

        assert_eq!(fs::read(&victim).unwrap(), b"untouched");
        assert!(!first.exists(), "earlier placement must roll back");
        assert!(!second.exists());
        assert!(
            !tmp_sibling(&first).exists(),
            "tmp siblings must be removed"
        );
        assert!(!tmp_sibling(&second).exists());
        assert!(
            staging_dirs(&layout).is_empty(),
            "staging must be cleaned on the failure path"
        );
    }

    #[test]
    fn duplicate_archive_basename_keeps_last_write_wins() {
        let home = tempdir().unwrap();
        let cache = tempdir().unwrap();
        let layout = layout_for(home.path());
        let runner = InstallRunner::new(&layout);

        let last: &[u8] = b"second-wins";
        let cached = write_cached(
            cache.path(),
            "payload.tar.gz",
            &build_tar_gz(&[("a/dup", b"first-loses"), ("b/dup", last)]),
        );
        let dest = layout.bin_dir.join("dup");
        let files = [ResolvedInstallFile::dest_only(dest.clone())];

        let prepared = runner
            .prepare_files("tar_gz", &cached, &files)
            .expect("prepare files");
        assert_eq!(prepared.regular[0].sha256, sha256_of(last));
        assert_eq!(
            staged_files(&layout).len(),
            1,
            "the superseded payload must not stay spooled"
        );

        runner.install_prepared(prepared).expect("install prepared");
        assert_eq!(fs::read(&dest).unwrap(), last);
    }

    #[test]
    fn one_archive_entry_may_feed_several_destinations() {
        let home = tempdir().unwrap();
        let cache = tempdir().unwrap();
        let layout = layout_for(home.path());
        let runner = InstallRunner::new(&layout);

        let payload: &[u8] = b"shared-bytes";
        let cached = write_cached(
            cache.path(),
            "payload.tar.gz",
            &build_tar_gz(&[("bin/tool", payload)]),
        );
        let bin_dest = layout.bin_dir.join("tool");
        let libexec_dest = layout.libexec_dir.join("tool");
        let entry = |dest: PathBuf| ResolvedInstallFile {
            source: Some("bin/tool".to_string()),
            dest,
            mode: None,
            kind: FileKind::Data,
            render: None,
        };

        let outcome = runner
            .install_mapped_fixture(
                "tar_gz",
                &cached,
                &[entry(bin_dest.clone()), entry(libexec_dest.clone())],
            )
            .expect("install both destinations");

        assert_eq!(outcome.files.len(), 2);
        assert_eq!(fs::read(&bin_dest).unwrap(), payload);
        assert_eq!(fs::read(&libexec_dest).unwrap(), payload);
        assert!(staging_dirs(&layout).is_empty(), "staging must be cleaned");
    }

    #[test]
    fn large_payload_preparation_retains_metadata_only() {
        let home = tempdir().unwrap();
        let cache = tempdir().unwrap();
        let layout = layout_for(home.path());
        let runner = InstallRunner::new(&layout);

        // 8 x 2 MiB: large enough that a retained copy of any single entry —
        // let alone the whole payload — would dwarf the metadata budget below.
        const ENTRIES: usize = 8;
        const ENTRY_LEN: usize = 2 * 1024 * 1024;
        let payloads: Vec<Vec<u8>> = (0..ENTRIES).map(|i| vec![i as u8 + 1; ENTRY_LEN]).collect();
        let names: Vec<String> = (0..ENTRIES).map(|i| format!("tree/part-{i}.bin")).collect();
        let entries: Vec<(&str, &[u8])> = names
            .iter()
            .zip(&payloads)
            .map(|(name, data)| (name.as_str(), data.as_slice()))
            .collect();
        let cached = write_cached(cache.path(), "payload.tar.gz", &build_tar_gz(&entries));

        let dest_root = layout.datadir.join("tree");
        let prepared = runner
            .prepare_files(
                "tar_gz",
                &cached,
                &[ResolvedInstallFile {
                    source: Some("tree/".to_string()),
                    dest: dest_root.clone(),
                    mode: Some("0644".to_string()),
                    kind: FileKind::Data,
                    render: None,
                }],
            )
            .expect("prepare files");

        let total = (ENTRIES * ENTRY_LEN) as u64;
        assert_eq!(prepared.regular.len(), ENTRIES);
        assert_eq!(
            staged_bytes(&layout),
            total,
            "the whole selected payload must be spooled to disk"
        );
        // The retained footprint is a function of entry count and path
        // lengths, never of payload size.
        let retained = retained_metadata_bytes(&prepared);
        assert!(
            retained < 4096,
            "prepared set retained {retained} bytes for a {total}-byte payload"
        );
        // Directory expansion stays in sorted archive-key order.
        let sources: Vec<&str> = prepared
            .regular
            .iter()
            .filter_map(|staged| staged.file.source.as_deref())
            .collect();
        let mut sorted = sources.clone();
        sorted.sort_unstable();
        assert_eq!(sources, sorted);

        // Placement reads staging, not the cache.
        fs::remove_file(&cached).unwrap();
        let installed = runner.install_prepared(prepared).expect("install prepared");
        assert_eq!(installed.files.len(), ENTRIES);
        for (name, payload) in names.iter().zip(&payloads) {
            let relative = name.strip_prefix("tree/").unwrap();
            assert_eq!(
                fs::metadata(dest_root.join(relative)).unwrap().len(),
                payload.len() as u64
            );
        }
        assert!(
            staging_dirs(&layout).is_empty(),
            "staging must be cleaned after placement"
        );
    }

    #[cfg(unix)]
    #[test]
    fn preparations_use_independent_private_dirs_in_shared_cache() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let home = tempdir().unwrap();
        let cache = tempdir().unwrap();
        let layout = layout_for(home.path());
        let runner = InstallRunner::new(&layout);

        fs::create_dir_all(&layout.cache_dir).unwrap();
        fs::set_permissions(&layout.cache_dir, fs::Permissions::from_mode(0o777)).unwrap();
        let cached = write_cached(
            cache.path(),
            "payload.tar.gz",
            &build_tar_gz(&[("bin/tool", b"tool-bytes")]),
        );
        let entry = |dest: PathBuf| ResolvedInstallFile {
            source: Some("bin/tool".to_string()),
            dest,
            mode: None,
            kind: FileKind::Data,
            render: None,
        };

        let first = runner
            .prepare_files("tar_gz", &cached, &[entry(layout.bin_dir.join("first"))])
            .expect("first preparation");
        let second = runner
            .prepare_files("tar_gz", &cached, &[entry(layout.bin_dir.join("second"))])
            .expect("second preparation");

        let dirs = staging_dirs(&layout);
        assert_eq!(dirs.len(), 2, "each preparation needs its own directory");
        assert_ne!(dirs[0], dirs[1]);
        for dir in &dirs {
            assert_eq!(
                fs::metadata(dir).unwrap().mode() & 0o777,
                0o700,
                "per-preparation staging must stay private"
            );
            assert_eq!(dir.parent(), Some(layout.cache_dir.as_path()));
        }

        drop(first);
        assert_eq!(staging_dirs(&layout).len(), 1);
        drop(second);
        assert!(staging_dirs(&layout).is_empty());
    }

    #[test]
    fn next_preparation_reclaims_interrupted_staging() {
        let home = tempdir().unwrap();
        let cache = tempdir().unwrap();
        let layout = layout_for(home.path());
        let runner = InstallRunner::new(&layout);

        fs::create_dir_all(&layout.cache_dir).unwrap();
        let orphan = layout
            .cache_dir
            .join(format!("{STAGING_DIR_PREFIX}interrupted"));
        fs::create_dir(&orphan).unwrap();
        fs::write(orphan.join("00000000"), b"orphaned payload").unwrap();
        let orphan_lock = open_staging_lock(&orphan.join(STAGING_LOCK_FILE), true).unwrap();
        orphan_lock.lock_exclusive().unwrap();
        drop(orphan_lock); // Simulate the kernel releasing locks on process exit.

        let cached = write_cached(
            cache.path(),
            "payload.tar.gz",
            &build_tar_gz(&[("bin/tool", b"tool-bytes")]),
        );
        let prepared = runner
            .prepare_files(
                "tar_gz",
                &cached,
                &[ResolvedInstallFile {
                    source: Some("bin/tool".to_string()),
                    dest: layout.bin_dir.join("tool"),
                    mode: None,
                    kind: FileKind::Data,
                    render: None,
                }],
            )
            .expect("prepare after interrupted install");

        assert!(!orphan.exists(), "unlocked staging must be reclaimed");
        assert_eq!(staging_dirs(&layout).len(), 1);
        drop(prepared);
        assert!(staging_dirs(&layout).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn staging_writes_refuse_to_follow_a_planted_symlink() {
        // Staged payloads and destination tmp siblings share one hardened open
        // path; a planted entry must fail it rather than be written through.
        let dir = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let victim = outside.path().join("victim");
        fs::write(&victim, b"untouched").unwrap();

        let planted = dir.path().join("00000000");
        std::os::unix::fs::symlink(&victim, &planted).unwrap();

        let err = create_exclusive_no_follow(&planted).expect_err("must refuse a planted symlink");
        match err {
            InstallError::Io { path, .. } => assert_eq!(path, planted),
            other => panic!("expected Io, got {other:?}"),
        }
        assert_eq!(fs::read(&victim).unwrap(), b"untouched");

        // The same helper still creates a fresh file normally.
        let fresh = dir.path().join("00000001");
        create_exclusive_no_follow(&fresh).expect("fresh staged file");
        assert!(fresh.exists());
    }

    #[test]
    fn staged_payload_tampering_is_rejected_before_the_rename() {
        let home = tempdir().unwrap();
        let cache = tempdir().unwrap();
        let layout = layout_for(home.path());
        let runner = InstallRunner::new(&layout);

        let original: &[u8] = b"verified-bytes";
        let cached = write_cached(
            cache.path(),
            "payload.tar.gz",
            &build_tar_gz(&[("bin/tool", original)]),
        );
        let dest = layout.bin_dir.join("tool");
        let prepared = runner
            .prepare_files(
                "tar_gz",
                &cached,
                &[ResolvedInstallFile::dest_only(dest.clone())],
            )
            .expect("prepare files");

        // Rewrite the staged payload behind the runner's back: placement
        // re-hashes what it writes, so the swap cannot reach the destination.
        fs::write(&prepared.regular[0].staged_path, b"tampered").unwrap();
        let err = runner
            .install_prepared(prepared)
            .expect_err("must reject a staged payload that no longer matches");
        match err {
            InstallError::StagedContentMismatch {
                path,
                expected_sha256,
                actual_sha256,
                ..
            } => {
                assert_eq!(path, dest);
                assert_eq!(expected_sha256, sha256_of(original));
                assert_eq!(actual_sha256, sha256_of(b"tampered"));
            }
            other => panic!("expected StagedContentMismatch, got {other:?}"),
        }
        assert!(!dest.exists(), "destination must not be created");
        assert!(!tmp_sibling(&dest).exists(), "tmp sibling must be removed");
    }
}
