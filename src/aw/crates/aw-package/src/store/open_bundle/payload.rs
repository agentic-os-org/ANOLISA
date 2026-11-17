//! Bounded payload hashing shares the inventory's descriptor traversal rules.

use crate::store::{database, require, Error, Payload, Result};
use sha2::{Digest, Sha256};
use std::{io::Read, os::unix::fs::MetadataExt, path::Path};

const MAX_FILE_BYTES: u64 = 1024 * 1024 * 1024;
pub(super) const MAX_TOTAL_BYTES: u64 = 4 * MAX_FILE_BYTES;

pub(crate) fn verify(path: &Path, expected: &Payload, budget: &mut u64) -> Result<()> {
    match check(path, expected, budget)? {
        Some(reason) => Err(Error::Invalid(reason)),
        None => Ok(()),
    }
}

pub(crate) fn drift(path: &Path, expected: &Payload, budget: &mut u64) -> Result<Option<String>> {
    match check(path, expected, budget) {
        Err(Error::Io(error))
            if matches!(
                error.raw_os_error(),
                Some(libc::ENOENT | libc::ENOTDIR | libc::ELOOP)
            ) =>
        {
            Ok(Some(error.to_string()))
        }
        result => result,
    }
}

fn check(path: &Path, expected: &Payload, budget: &mut u64) -> Result<Option<String>> {
    if !database::safe_metadata(path)?.is_file() {
        return Ok(Some(format!(
            "expected a regular package file: {}",
            path.display()
        )));
    }
    let mut file = database::safe_opened(path)?;
    let before = file.metadata()?;
    if !before.is_file() {
        return Ok(Some(format!(
            "expected a regular package file: {}",
            path.display()
        )));
    }
    require(
        before.len() <= MAX_FILE_BYTES && before.len() <= *budget,
        format!(
            "package file exceeds verification byte limit: {}",
            path.display()
        ),
    )?;
    if before.mode() & 0o7777 != expected.mode {
        return Ok(Some(format!(
            "package file mode mismatch: {}",
            path.display()
        )));
    }
    let mut hash = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        require(
            total <= MAX_FILE_BYTES && total <= *budget,
            format!(
                "package file grew beyond verification limit: {}",
                path.display()
            ),
        )?;
        hash.update(&buffer[..count]);
    }
    let after = file.metadata()?;
    require(
        before.len() == after.len()
            && before.mtime() == after.mtime()
            && before.mtime_nsec() == after.mtime_nsec()
            && before.ctime() == after.ctime()
            && before.ctime_nsec() == after.ctime_nsec(),
        format!(
            "package file changed during verification: {}",
            path.display()
        ),
    )?;
    database::same_file(path, &after)?;
    *budget -= total;
    if format!("{:x}", hash.finalize()) != expected.sha256 {
        return Ok(Some(format!(
            "package file checksum mismatch: {}",
            path.display()
        )));
    }
    Ok(None)
}
