//! Private, bounded snapshots bind the complete delivered tree rather than model text alone.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Component, Path, PathBuf};

use rustix::fs::{Mode, OFlags, openat};
use sha2::{Digest as _, Sha256};
use tempfile::TempDir;

use crate::{Error, Result};

const MAX_TREE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_TREE_ENTRIES: usize = 8192;
const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;
pub(crate) const MAX_TEXT_BYTES: usize = 600_000;

pub(crate) struct Snapshot {
    directory: TempDir,
    pub hash: String,
    pub files: BTreeMap<String, String>,
    pub omitted: Vec<String>,
}

impl Snapshot {
    pub fn create(input: &Path) -> Result<Self> {
        let root = File::from(openat(
            rustix::fs::CWD,
            input,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?);
        let directory = tempfile::Builder::new()
            .prefix("asc-score-source-")
            .tempdir()?;
        let mut snapshot = Self {
            directory,
            hash: String::new(),
            files: BTreeMap::new(),
            omitted: Vec::new(),
        };
        let mut digest = Sha256::new();
        digest.update(b"agent-sec-skill-score/source-tree-v2\0");
        let mut budget = Budget::default();
        let destination = snapshot.directory.path().to_path_buf();
        snapshot.copy(&root, Path::new(""), &destination, &mut digest, &mut budget)?;
        if !snapshot.files.contains_key("SKILL.md") {
            return Err(Error::Contract(
                "Skill must contain a readable UTF-8 SKILL.md".into(),
            ));
        }
        snapshot.hash = format!("{:x}", digest.finalize());
        Ok(snapshot)
    }

    pub fn validate_paths(
        &self,
        skill: &Path,
        model: &asc_model_client::remote::RemoteModel,
    ) -> Result<()> {
        use std::os::unix::ffi::OsStrExt as _;
        // Omitted files still appear in limitations and in the controller's internal report.
        if model.contains_secret(skill.as_os_str().as_bytes())
            || self
                .omitted
                .iter()
                .any(|name| model.contains_secret(name.as_bytes()))
        {
            return Err(Error::Contract(
                "Skill path contains a resolved credential".into(),
            ));
        }
        Ok(())
    }

    pub fn path(&self) -> &Path {
        self.directory.path()
    }

    fn copy(
        &mut self,
        source: &File,
        relative: &Path,
        target: &Path,
        digest: &mut Sha256,
        budget: &mut Budget,
    ) -> Result<()> {
        let before_directory = source.metadata()?;
        // Read directory names through the opened descriptor, never through a replaceable input path.
        let directory = PathBuf::from(format!(
            "/proc/self/fd/{}",
            std::os::fd::AsRawFd::as_raw_fd(source)
        ));
        let remaining = MAX_TREE_ENTRIES - budget.entries;
        let mut names = std::fs::read_dir(directory)?
            .take(remaining + 1)
            .map(|entry| entry.map(|e| e.file_name()))
            .collect::<std::io::Result<Vec<_>>>()?;
        if names.len() > remaining {
            return Err(Error::Contract("input exceeds entry limit".into()));
        }
        // Reserve the whole listing: sibling names remain allocated while descending.
        budget.entries += names.len();
        names.sort();
        for name in names {
            let path = relative.join(&name);
            let label = path
                .to_str()
                .ok_or_else(|| Error::Contract("input contains a non-UTF-8 filename".into()))?;
            if path.components().count() > 64 {
                return Err(Error::Contract(format!(
                    "input exceeds entry/depth limit at {label}"
                )));
            }
            let mut file = File::from(
                openat(
                    source,
                    &name,
                    OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .map_err(|_| {
                    Error::Contract(format!(
                        "cannot safely read input {label}; links and special files are unsupported"
                    ))
                })?,
            );
            let metadata = file.metadata()?;
            if !metadata.is_file() && !metadata.is_dir() {
                return Err(Error::Contract(format!(
                    "special input file is unsupported: {label}"
                )));
            }
            let mode = metadata.mode() & 0o777;
            digest.update((label.len() as u64).to_be_bytes());
            digest.update(label.as_bytes());
            digest.update(if metadata.is_dir() { b"d" } else { b"f" });
            digest
                .update(((mode & 0o111) | if metadata.is_dir() { 0o100 } else { 0 }).to_be_bytes());
            let output = target.join(&name);
            if metadata.is_dir() {
                std::fs::create_dir(&output)?;
                std::fs::set_permissions(&output, std::fs::Permissions::from_mode(0o700))?;
                self.copy(&file, &path, &output, digest, budget)?;
                std::fs::set_permissions(
                    &output,
                    std::fs::Permissions::from_mode(0o700 | (mode & 0o111)),
                )?;
            } else {
                self.copy_file(&mut file, &metadata, label, &output, digest, budget)?;
            }
        }
        let after_directory = source.metadata()?;
        if before_directory.mtime() != after_directory.mtime()
            || before_directory.mtime_nsec() != after_directory.mtime_nsec()
            || before_directory.ctime() != after_directory.ctime()
            || before_directory.ctime_nsec() != after_directory.ctime_nsec()
        {
            return Err(Error::Contract(format!(
                "input directory changed while snapshotting: {}",
                relative.display()
            )));
        }
        Ok(())
    }
    fn copy_file(
        &mut self,
        file: &mut File,
        metadata: &std::fs::Metadata,
        label: &str,
        output: &Path,
        digest: &mut Sha256,
        budget: &mut Budget,
    ) -> Result<()> {
        let mode = metadata.mode() & 0o777;
        if metadata.len() > MAX_FILE_BYTES {
            return Err(Error::Contract(format!(
                "input file exceeds 32 MiB: {label}"
            )));
        }
        let mut bytes = Vec::new();
        (&mut *file)
            .take(MAX_FILE_BYTES + 1)
            .read_to_end(&mut bytes)?;
        let after = file.metadata()?;
        if after.len() != metadata.len()
            || bytes.len() as u64 != metadata.len()
            || after.mtime() != metadata.mtime()
            || after.mtime_nsec() != metadata.mtime_nsec()
            || after.ctime() != metadata.ctime()
            || after.ctime_nsec() != metadata.ctime_nsec()
        {
            return Err(Error::Contract(format!(
                "input changed while snapshotting: {label}"
            )));
        }
        budget.bytes += bytes.len() as u64;
        if bytes.len() as u64 > MAX_FILE_BYTES || budget.bytes > MAX_TREE_BYTES {
            return Err(Error::Contract(format!(
                "input exceeds size limit at {label}"
            )));
        }
        digest.update((bytes.len() as u64).to_be_bytes());
        digest.update(&bytes);
        let mut writer = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(output)?;
        writer.write_all(&bytes)?;
        // Preserve executable bits; owner read/write is required for the private snapshot lifecycle.
        writer.set_permissions(std::fs::Permissions::from_mode(0o600 | (mode & 0o111)))?;
        if let Ok(text) = std::str::from_utf8(&bytes)
            && !text.contains('\0')
            && budget.text + text.len() <= MAX_TEXT_BYTES
        {
            // Scanners also expand per-line source references, so bound the raw view first.
            budget.catalog.visit(label, text, |_, _| {})?;
            budget.text += text.len();
            self.files.insert(label.into(), text.into());
            return Ok(());
        }
        self.omitted.push(format!(
            "{label} (binary or model-review limit; included in execution digest)"
        ));
        Ok(())
    }
}

#[derive(Default)]
struct Budget {
    entries: usize,
    bytes: u64,
    text: usize,
    catalog: CatalogBudget,
}

struct CatalogBudget {
    lines: usize,
    bytes: usize,
}

impl Default for CatalogBudget {
    fn default() -> Self {
        Self {
            lines: 32_768,
            bytes: 4 * 1024 * 1024,
        }
    }
}

impl CatalogBudget {
    fn visit(&mut self, path: &str, text: &str, mut visit: impl FnMut(usize, &str)) -> Result<()> {
        for (index, line) in text.lines().enumerate() {
            let number = index + 1;
            // Charge the full reference and text before the caller can copy either string.
            let bytes = "source::".len() + path.len() + number.ilog10() as usize + 1 + line.len();
            if self.lines == 0 || bytes > self.bytes {
                return Err(Error::Contract(
                    "source catalog exceeds line or byte limit".into(),
                ));
            }
            self.lines -= 1;
            self.bytes -= bytes;
            visit(number, line);
        }
        Ok(())
    }
}

pub(crate) fn catalog(files: &BTreeMap<String, String>) -> Result<BTreeMap<String, String>> {
    let mut entries = BTreeMap::new();
    let mut budget = CatalogBudget::default();
    for (path, text) in files {
        budget.visit(path, text, |number, line| {
            entries.insert(format!("source:{path}:{number}"), line.to_owned());
        })?;
    }
    Ok(entries)
}

pub(crate) fn contained_relative(value: &str) -> bool {
    !value.is_empty()
        && Path::new(value)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
        && !value.contains("//")
        && !value.ends_with('/')
}

pub(crate) fn private_json(path: &Path, value: &serde_json::Value) -> Result<()> {
    private_write(path, |file| {
        serde_json::to_writer_pretty(&mut *file, value)?;
        file.write_all(b"\n")?;
        Ok(())
    })
}

pub(crate) fn private_write(
    path: &Path,
    write: impl FnOnce(&mut File) -> Result<()>,
) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::Contract("JSON path lacks parent".into()))?;
    std::fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    write(file.as_file_mut())?;
    file.persist(path).map_err(|e| Error::Io(e.error))?;
    Ok(())
}

pub(crate) fn read_json(path: &Path, limit: u64) -> Result<serde_json::Value> {
    let file = File::from(openat(
        rustix::fs::CWD,
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )?);
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 || metadata.len() > limit {
        return Err(Error::Contract(
            "JSON input is not a bounded private regular file".into(),
        ));
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(Error::Contract("JSON input exceeds limit".into()));
    }
    Ok(serde_json::from_slice(&bytes)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn catalog_charges_lines_and_expanded_references_across_files() {
        let path = "a".repeat(2000);
        let text = "\n".repeat(10);
        let bytes = 10 * ("source::".len() + path.len() + 1) + 1;
        for (lines, bytes_left, accepted) in [(9, bytes, 9), (10, bytes - 1, 9), (11, bytes, 10)] {
            let mut budget = CatalogBudget {
                lines,
                bytes: bytes_left,
            };
            let mut visited = 0;
            let result = budget.visit(&path, &text, |number, line| {
                visited = number;
                assert_eq!(line, "");
            });
            assert_eq!(visited, accepted);
            assert_eq!(result.is_ok(), accepted == 10);
            if accepted == 10 || lines == 9 {
                assert!(
                    budget
                        .visit("z", "x", |_, _| panic!("over-budget line reached caller"))
                        .is_err()
                );
            }
        }
    }

    #[test]
    fn directory_listing_reserves_siblings_before_descending() -> Result<()> {
        let input = tempfile::tempdir()?;
        std::fs::create_dir(input.path().join("a"))?;
        std::fs::write(input.path().join("a/child"), "nested")?;
        std::fs::write(input.path().join("z"), "sibling")?;
        let output = tempfile::tempdir()?;
        let mut snapshot = Snapshot {
            directory: tempfile::tempdir()?,
            hash: String::new(),
            files: BTreeMap::new(),
            omitted: Vec::new(),
        };
        let mut budget = Budget {
            entries: MAX_TREE_ENTRIES - 2,
            ..Budget::default()
        };
        let result = snapshot.copy(
            &File::open(input.path())?,
            Path::new(""),
            output.path(),
            &mut Sha256::new(),
            &mut budget,
        );
        assert!(matches!(result, Err(Error::Contract(_))));
        assert!(output.path().join("a").is_dir());
        assert!(!output.path().join("a/child").exists());
        Ok(())
    }

    #[test]
    fn digest_covers_dependencies_binaries_and_execute_permissions() {
        let root = tempfile::tempdir().expect("directory");
        std::fs::write(root.path().join("SKILL.md"), "hello").expect("skill");
        std::fs::create_dir(root.path().join("node_modules")).expect("deps");
        let binary = root.path().join("node_modules/data");
        std::fs::write(&binary, [0, 1]).expect("binary");
        let first = Snapshot::create(root.path()).expect("snapshot");
        std::fs::write(&binary, [0, 2]).expect("change");
        let second = Snapshot::create(root.path()).expect("snapshot");
        assert_ne!(first.hash, second.hash);
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700))
            .expect("permissions");
        assert_ne!(
            second.hash,
            Snapshot::create(root.path()).expect("snapshot").hash
        );
        std::os::unix::fs::symlink("/etc/passwd", root.path().join("link")).expect("link");
        assert!(Snapshot::create(root.path()).is_err());
    }
}
