//! Descriptor-pinned, bounded read access to existing private journal chains.

use super::{validate_envelope, validate_key};
use aw_contracts::canonical;
use serde_json::{json, Value};
use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Read},
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
};

/// Maximum directory entries examined by one complete inventory scan.
pub const MAX_ENTRIES: usize = 10_000;
/// Maximum bytes verified for one chain, including private envelopes.
pub const MAX_CHAIN_BYTES: u64 = 8 * 1024 * 1024;
/// Maximum source records verified for one chain.
pub const MAX_CHAIN_RECORDS: usize = 4096;
/// Maximum keys or source records returned in one page.
pub const MAX_PAGE: usize = 64;

/// Read failures preserve distinctions needed for downstream reconciliation.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Storage failed for a reason other than missing history.
    #[error("journal read I/O: {0}")]
    Io(#[from] std::io::Error),
    /// Private storage has the wrong owner, mode, type or link count.
    #[error("journal access denied")]
    Access,
    /// Requested retained history no longer exists.
    #[error("journal history missing")]
    Missing,
    /// Canonical records or linked digests are invalid.
    #[error("journal history corrupt")]
    Corrupt,
    /// A reservation is empty or its final record lacks a newline.
    #[error("journal history incomplete")]
    Incomplete,
    /// Caller limits, keys or retained positions are malformed.
    #[error("invalid journal read request")]
    Request,
    /// Inventory or chain exceeds documented read bounds.
    #[error("journal read limit exceeded")]
    Limit,
    /// Caller cancelled or exhausted its deadline.
    #[error("journal read cancelled")]
    Cancelled,
    /// A retained chain position or observed tip no longer matches the source.
    #[error("journal retained history changed")]
    Changed,
}

/// Verified source position; retain the highest seen tip to detect suffix loss.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Position {
    /// Zero-based private source sequence.
    pub sequence: u64,
    /// Digest of the complete private envelope at this sequence.
    pub digest: String,
}

/// Bounded sorted inventory with progress retained across concurrent key changes.
#[derive(Debug)]
pub struct Inventory {
    /// Canonical digest of the entire bounded key set.
    pub digest: String,
    /// Sorted keys in this page, excluding the continuation key.
    pub keys: Vec<String>,
    /// Last returned key if another page remains below the scan ceiling.
    pub next: Option<String>,
    /// Highest key captured when this scan began; newly higher keys wait for rescan.
    pub through: Option<String>,
    /// The key set differs from the previous page's observation.
    pub changed: bool,
}

/// Verified chain snapshot retaining only a bounded source page.
#[derive(Debug)]
pub struct Page {
    /// Initial reservation needed to interpret later records.
    pub initial: Value,
    /// Source envelopes after the consumed position, up to the page limit.
    pub records: Vec<Value>,
    /// Highest verified position captured by this read.
    pub tip: Position,
    /// Last returned position, or the supplied position for an empty page.
    pub next: Option<Position>,
    /// This page has consumed the captured tip.
    pub caught_up: bool,
    /// The captured tip explicitly closes the chain.
    pub terminal: bool,
}

/// Read-only handle pinned to an existing directory, without execution ownership.
///
/// `owner` comes from the authenticated local caller, never JSON. Directory and
/// files must belong to this UID. Hashes detect corruption, not malicious rewriting
/// by a trusted same-user writer.
pub struct JournalReader {
    directory: File,
    owner: u32,
}

impl JournalReader {
    /// Opens an existing private directory without following its final symlink.
    ///
    /// # Errors
    /// Rejects non-Linux use, missing directories, unsafe permissions and owners.
    pub fn open(path: &Path, owner: u32) -> Result<Self, Error> {
        if !cfg!(target_os = "linux") || !path.is_absolute() {
            return Err(Error::Access);
        }
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .map_err(open_error)?;
        let metadata = directory.metadata()?;
        if !metadata.is_dir() || metadata.uid() != owner || metadata.mode() & 0o7777 != 0o700 {
            return Err(Error::Access);
        }
        Ok(Self { directory, owner })
    }

    /// Path bound to the opened directory; valid only while this reader is alive.
    ///
    /// Service-owned metadata readers can use this path to share the same
    /// directory identity instead of reopening a replaceable user-supplied path.
    pub fn pinned_path(&self) -> PathBuf {
        PathBuf::from(format!("/proc/self/fd/{}", self.directory.as_raw_fd()))
    }

    fn file(&self, name: &str) -> Result<File, Error> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(self.pinned_path().join(name))
            .map_err(open_error)?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.uid() != self.owner
            || metadata.mode() & 0o7777 != 0o600
            || metadata.nlink() != 1
        {
            return Err(Error::Access);
        }
        Ok(file)
    }

    /// Enumerates a stable sorted inventory with bounded work and cancellation.
    ///
    /// Pass the returned digest and ceiling on subsequent pages. Key changes
    /// remain visible without resetting progress. Rescan for newly earlier keys
    /// and revisit known open chains separately.
    ///
    /// # Errors
    /// Rejects invalid cursors, excess entries or cancellation.
    pub fn enumerate(
        &self,
        after: Option<&str>,
        expected: Option<&str>,
        through: Option<&str>,
        limit: usize,
        cancelled: &impl Fn() -> bool,
    ) -> Result<Inventory, Error> {
        check_limit(limit)?;
        for key in [after, through].into_iter().flatten() {
            validate_key(key).map_err(|_| Error::Request)?;
        }
        if after
            .zip(through)
            .is_some_and(|(after, through)| after > through)
        {
            return Err(Error::Request);
        }
        let mut keys = Vec::new();
        for (index, entry) in fs::read_dir(self.pinned_path())?.enumerate() {
            check_cancel(cancelled)?;
            if index >= MAX_ENTRIES {
                return Err(Error::Limit);
            }
            let name = entry?.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let Some(key) = name.strip_suffix(".jsonl") else {
                continue;
            };
            validate_key(key).map_err(|_| Error::Corrupt)?;
            keys.push(key.to_owned());
        }
        keys.sort_unstable();
        keys.dedup();
        let digest = canonical::document_digest(&json!(keys)).map_err(|_| Error::Limit)?;
        let changed = expected.is_some_and(|expected| expected != digest);
        let through = through.map(str::to_owned).or_else(|| keys.last().cloned());
        if let Some(through) = &through {
            keys.truncate(keys.partition_point(|key| key <= through));
        }
        let start = after.map_or(0, |key| keys.partition_point(|v| v.as_str() <= key));
        let end = (start + limit).min(keys.len());
        let next = (end < keys.len()).then(|| keys[end - 1].clone());
        check_cancel(cancelled)?;
        Ok(Inventory {
            digest,
            keys: keys[start..end].to_vec(),
            next,
            through,
            changed,
        })
    }

    /// Verifies a bounded chain and returns a page after a retained position.
    ///
    /// Every call verifies from sequence zero to the captured file size.
    /// `observed` detects loss of a previously seen complete suffix. Appends beyond
    /// the captured size are read on the next visit.
    ///
    /// # Errors
    /// Reports missing/corrupt/incomplete history, changed anchors, bounds or cancellation.
    pub fn read(
        &self,
        key: &str,
        after: Option<&Position>,
        observed: Option<&Position>,
        limit: usize,
        cancelled: &impl Fn() -> bool,
    ) -> Result<Page, Error> {
        check_limit(limit)?;
        validate_key(key).map_err(|_| Error::Request)?;
        for position in [after, observed].into_iter().flatten() {
            validate_key(&position.digest).map_err(|_| Error::Request)?;
            if position.sequence >= MAX_CHAIN_RECORDS as u64 {
                return Err(Error::Request);
            }
        }
        check_cancel(cancelled)?;
        let file = self.file(&format!("{key}.jsonl"))?;
        let length = file.metadata()?.len();
        if length > MAX_CHAIN_BYTES {
            return Err(Error::Limit);
        }
        if length == 0 {
            return Err(Error::Incomplete);
        }
        let mut reader = BufReader::new(file.take(length));
        let mut previous = Value::Null;
        let mut initial = Value::Null;
        let mut records = Vec::new();
        let mut sequence = 0;
        let mut consumed = 0u64;
        let mut terminal = false;
        let mut next = after.cloned();
        loop {
            check_cancel(cancelled)?;
            let mut bytes = Vec::new();
            let count = reader
                .by_ref()
                .take(canonical::MAX_DOCUMENT_BYTES as u64 + 2)
                .read_until(b'\n', &mut bytes)?;
            if count == 0 {
                break;
            }
            consumed += count as u64;
            if sequence >= MAX_CHAIN_RECORDS as u64 {
                return Err(Error::Limit);
            }
            if bytes.len() > canonical::MAX_DOCUMENT_BYTES + 1 {
                return Err(Error::Limit);
            }
            if bytes.pop() != Some(b'\n') {
                return Err(Error::Incomplete);
            }
            let envelope = canonical::parse(&bytes).map_err(|_| Error::Corrupt)?;
            validate_envelope(&envelope, key, sequence, &previous).map_err(|_| Error::Corrupt)?;
            let digest = envelope["digest"].as_str().ok_or(Error::Corrupt)?;
            for position in [after, observed].into_iter().flatten() {
                if position.sequence == sequence && position.digest != digest {
                    return Err(Error::Changed);
                }
            }
            if sequence == 0 {
                initial = envelope.clone();
            }
            if terminal {
                return Err(Error::Corrupt);
            }
            terminal = envelope["record"]["phase"] == "closed";
            previous = envelope["digest"].clone();
            if after.is_none_or(|after| sequence > after.sequence) && records.len() < limit {
                next = Some(Position {
                    sequence,
                    digest: digest.to_owned(),
                });
                records.push(envelope);
            }
            sequence += 1;
        }
        if consumed != length {
            return Err(Error::Changed);
        }
        if sequence == 0 {
            return Err(Error::Incomplete);
        }
        if [after, observed]
            .into_iter()
            .flatten()
            .any(|p| p.sequence >= sequence)
        {
            return Err(Error::Changed);
        }
        let tip = Position {
            sequence: sequence - 1,
            digest: previous.as_str().ok_or(Error::Corrupt)?.into(),
        };
        let caught_up = next.as_ref() == Some(&tip);
        check_cancel(cancelled)?;
        Ok(Page {
            initial,
            records,
            tip,
            next,
            caught_up,
            terminal,
        })
    }
}

fn check_limit(limit: usize) -> Result<(), Error> {
    if !(1..=MAX_PAGE).contains(&limit) {
        return Err(Error::Request);
    }
    Ok(())
}
fn check_cancel(cancelled: &impl Fn() -> bool) -> Result<(), Error> {
    if cancelled() {
        return Err(Error::Cancelled);
    }
    Ok(())
}

fn open_error(error: std::io::Error) -> Error {
    match error.kind() {
        std::io::ErrorKind::NotFound => Error::Missing,
        std::io::ErrorKind::PermissionDenied => Error::Access,
        _ if error.raw_os_error() == Some(libc::ELOOP) => Error::Access,
        _ => Error::Io(error),
    }
}
