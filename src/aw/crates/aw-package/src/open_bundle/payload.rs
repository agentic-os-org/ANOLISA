//! Bounded payload hashing shares the inventory's descriptor traversal rules.

use crate::{database, require, Payload, Result};
use sha2::{Digest, Sha256};
use std::{io::Read, os::unix::fs::MetadataExt, path::Path};

const MAX_FILE_BYTES: u64 = 1024 * 1024 * 1024;
pub(super) const MAX_TOTAL_BYTES: u64 = 4 * MAX_FILE_BYTES;

pub(crate) fn verify(path: &Path, expected: &Payload, budget: &mut u64) -> Result<()> {
    let mut file = database::safe_file(path)?;
    let before = file.metadata()?;
    require(
        before.len() <= MAX_FILE_BYTES && before.len() <= *budget,
        format!(
            "package file exceeds verification byte limit: {}",
            path.display()
        ),
    )?;
    require(
        before.mode() & 0o7777 == expected.mode,
        format!("package file mode mismatch: {}", path.display()),
    )?;
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
    require(
        format!("{:x}", hash.finalize()) == expected.sha256,
        format!("package file checksum mismatch: {}", path.display()),
    )?;
    *budget -= total;
    Ok(())
}
