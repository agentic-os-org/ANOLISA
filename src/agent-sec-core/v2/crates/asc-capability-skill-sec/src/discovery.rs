//! Expand configured patterns with the same no-symlink traversal used by privileged Skill I/O.

use crate::config::Scope;
use crate::ledger::storage::{Directory, missing};
use crate::{ManagedSkillDir, SkillIdentity, SkillSecError, check_deadline, io_error};
use rustix::fs::{AtFlags, FileType, statat};
use std::collections::BTreeSet;
use std::time::Instant;

pub(crate) struct DefaultBudget {
    entries: usize,
    skills: usize,
    pub exhausted: bool,
}

impl Default for DefaultBudget {
    fn default() -> Self {
        Self {
            entries: 10_000,
            skills: 1024,
            exhausted: false,
        }
    }
}

fn limit_error() -> SkillSecError {
    SkillSecError::Invalid(
        "default Skill discovery exceeds directory limits; coverage is incomplete".into(),
    )
}

pub(crate) fn discover(
    pattern: &ManagedSkillDir,
    deadline: Instant,
    mounted: impl Fn(&SkillIdentity) -> bool,
    found: &mut BTreeSet<SkillIdentity>,
    mut budget: Option<&mut DefaultBudget>,
) -> Result<(), SkillSecError> {
    check_deadline(deadline)?;
    let mut truncated = false;
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
            let identity = SkillIdentity::new(&directory.path)?;
            if !found.contains(&identity) {
                if let Some(budget) = budget.as_deref_mut() {
                    if budget.skills == 0 {
                        budget.exhausted = true;
                        return Err(limit_error());
                    }
                    budget.skills -= 1;
                }
                found.insert(identity);
            }
        }
        if pattern.scope == Scope::Exact || (pattern.scope == Scope::Children && depth == 1) {
            continue;
        }
        if depth >= 128 {
            return Err(SkillSecError::Invalid(
                "managed Skill discovery exceeds 128 directory levels".into(),
            ));
        }
        let names = if let Some(budget) = budget.as_deref_mut() {
            let (names, limited) = directory.bounded_names(deadline, &mut budget.entries)?;
            truncated |= limited;
            names
        } else {
            directory.names(deadline)?
        };
        for name in names {
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
    if truncated {
        if let Some(budget) = budget {
            budget.exhausted = true;
        }
        return Err(limit_error());
    }
    Ok(())
}

fn entry_type(directory: &Directory, name: &str) -> Result<Option<FileType>, SkillSecError> {
    match statat(&directory.file, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => Ok(Some(FileType::from_raw_mode(stat.st_mode))),
        Err(rustix::io::Errno::NOENT) => Ok(None),
        Err(error) => Err(io_error(directory.path.join(name), error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::Duration;

    #[test]
    fn directory_budget_is_shared_and_applied_before_collecting_names() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        for n in 0..5 {
            fs::write(base.join(format!("entry-{n}")), "").unwrap();
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        let directory = Directory::open(&base).unwrap();
        let mut remaining = 3;
        let (names, truncated) = directory.bounded_names(deadline, &mut remaining).unwrap();
        assert_eq!(names.len(), 3);
        assert!(truncated);
        assert_eq!(remaining, 0);
        let (names, truncated) = directory.bounded_names(deadline, &mut remaining).unwrap();
        assert!(names.is_empty());
        assert!(truncated);
        let mut budget = DefaultBudget {
            entries: 3,
            skills: 1024,
            exhausted: false,
        };
        let mut found = BTreeSet::new();
        assert!(
            discover(
                &ManagedSkillDir::new(base.join("*")).unwrap(),
                deadline,
                |_| false,
                &mut found,
                Some(&mut budget)
            )
            .is_err()
        );
        assert!(budget.exhausted);
    }
}
