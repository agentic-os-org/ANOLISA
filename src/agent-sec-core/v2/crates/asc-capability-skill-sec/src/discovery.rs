//! Expand configured patterns with the same no-symlink traversal used by privileged Skill I/O.

use crate::config::Scope;
use crate::ledger::storage::{Directory, missing};
use crate::{ManagedSkillDir, SkillIdentity, SkillSecError, check_deadline, io_error};
use rustix::fs::{AtFlags, FileType, statat};
use std::collections::BTreeSet;
use std::time::Instant;

pub(crate) fn discover(
    pattern: &ManagedSkillDir,
    deadline: Instant,
    mounted: impl Fn(&SkillIdentity) -> bool,
) -> Result<BTreeSet<SkillIdentity>, SkillSecError> {
    check_deadline(deadline)?;
    let mut found = BTreeSet::new();
    let mut pending = vec![(pattern.root.path().to_owned(), 0)];
    while let Some((path, depth)) = pending.pop() {
        check_deadline(deadline)?;
        let directory = match Directory::open(&path) {
            Ok(directory) => directory,
            Err(error) if missing(&error) => continue,
            Err(error) => return Err(error),
        };
        check_deadline(deadline)?;
        if (pattern.scope != Scope::Children || depth == 1)
            && entry_type(&directory, "SKILL.md")? == Some(FileType::RegularFile)
        {
            found.insert(SkillIdentity::new(&directory.path)?);
        }
        if pattern.scope == Scope::Exact || (pattern.scope == Scope::Children && depth == 1) {
            continue;
        }
        if depth >= 128 {
            return Err(SkillSecError::Invalid(
                "managed Skill discovery exceeds 128 directory levels".into(),
            ));
        }
        for name in directory.names(deadline)? {
            check_deadline(deadline)?;
            if !name.starts_with('.') && entry_type(&directory, &name)? == Some(FileType::Directory)
            {
                if mounted(&SkillIdentity::new(directory.path.join(&name))?) {
                    continue;
                }
                // Reopen with no-follow traversal when visited instead of retaining unbounded FDs.
                pending.push((directory.path.join(name), depth + 1));
            }
        }
    }
    Ok(found)
}

fn entry_type(directory: &Directory, name: &str) -> Result<Option<FileType>, SkillSecError> {
    match statat(&directory.file, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => Ok(Some(FileType::from_raw_mode(stat.st_mode))),
        Err(rustix::io::Errno::NOENT) => Ok(None),
        Err(error) => Err(io_error(directory.path.join(name), error)),
    }
}
