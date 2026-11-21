//! Thread-local storage faults exercise failure windows without filesystem tricks.

use std::{
    cell::RefCell,
    io,
    marker::PhantomData,
    path::{Path, PathBuf},
    rc::Rc,
};

thread_local! {
    static DATABASE_SYNC_FAILURE: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
    static TMPFILE_FAILURE: RefCell<bool> = const { RefCell::new(false) };
}

/// Restore the previous fault when the isolated test scope exits, including unwinding.
pub(crate) struct DatabaseSyncFailure {
    previous: Option<PathBuf>,
    _same_thread: PhantomData<Rc<()>>,
}

impl Drop for DatabaseSyncFailure {
    fn drop(&mut self) {
        DATABASE_SYNC_FAILURE.with(|failure| {
            let _ = failure.replace(self.previous.take());
        });
    }
}

/// Fail only the destination-directory barrier for this test's database path.
pub(crate) fn fail_database_sync(path: &Path) -> DatabaseSyncFailure {
    let previous = DATABASE_SYNC_FAILURE.with(|failure| failure.replace(Some(path.into())));
    DatabaseSyncFailure {
        previous,
        _same_thread: PhantomData,
    }
}

pub(super) fn check_database_sync(path: &Path) -> io::Result<()> {
    if DATABASE_SYNC_FAILURE.with(|failure| failure.borrow().as_deref() == Some(path)) {
        Err(io::Error::from_raw_os_error(libc::EIO))
    } else {
        Ok(())
    }
}

/// Restore the previous fault when the isolated test scope exits, including unwinding.
pub(crate) struct TmpfileFailure {
    previous: bool,
    _same_thread: PhantomData<Rc<()>>,
}

impl Drop for TmpfileFailure {
    fn drop(&mut self) {
        TMPFILE_FAILURE.with(|failure| {
            let _ = failure.replace(self.previous);
        });
    }
}

/// Fail O_TMPFILE staging until the guard drops, forcing the named fallback path.
pub(crate) fn fail_tmpfile() -> TmpfileFailure {
    let previous = TMPFILE_FAILURE.with(|failure| failure.replace(true));
    TmpfileFailure {
        previous,
        _same_thread: PhantomData,
    }
}

pub(super) fn check_tmpfile() -> Result<(), crate::store::Error> {
    if TMPFILE_FAILURE.with(|failure| *failure.borrow()) {
        Err(crate::store::Error::Io(io::Error::from_raw_os_error(
            libc::EOPNOTSUPP,
        )))
    } else {
        Ok(())
    }
}

thread_local! {
    static STAGING_WRITE_FAILURE: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
    static STAGING_SYNC_FAILURE: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

/// Restore the previous fault when the isolated test scope exits, including unwinding.
pub(crate) struct StagingWriteFailure {
    previous: Option<PathBuf>,
    _same_thread: PhantomData<Rc<()>>,
}

impl Drop for StagingWriteFailure {
    fn drop(&mut self) {
        STAGING_WRITE_FAILURE.with(|failure| {
            let _ = failure.replace(self.previous.take());
        });
    }
}

/// Fail the next staging write under this test directory, standing in for a
/// device that reports ENOSPC after the callback already wrote some bytes.
pub(crate) fn fail_staging_write(root: &Path) -> StagingWriteFailure {
    let previous = STAGING_WRITE_FAILURE.with(|failure| failure.replace(Some(root.into())));
    StagingWriteFailure {
        previous,
        _same_thread: PhantomData,
    }
}

pub(super) fn check_staging_write(parent: &Path) -> Result<(), crate::store::Error> {
    let armed = STAGING_WRITE_FAILURE.with(|failure| {
        let mut slot = failure.borrow_mut();
        let matches = slot.as_deref().is_some_and(|root| parent.starts_with(root));
        if matches {
            *slot = None;
        }
        matches
    });
    if armed {
        return Err(crate::store::Error::Io(io::Error::from_raw_os_error(
            libc::ENOSPC,
        )));
    }
    Ok(())
}

/// Restore the previous fault when the isolated test scope exits, including unwinding.
pub(crate) struct StagingSyncFailure {
    previous: Option<PathBuf>,
    _same_thread: PhantomData<Rc<()>>,
}

impl Drop for StagingSyncFailure {
    fn drop(&mut self) {
        STAGING_SYNC_FAILURE.with(|failure| {
            let _ = failure.replace(self.previous.take());
        });
    }
}

/// Fail the next staging sync under this test directory with EIO.
pub(crate) fn fail_staging_sync(root: &Path) -> StagingSyncFailure {
    let previous = STAGING_SYNC_FAILURE.with(|failure| failure.replace(Some(root.into())));
    StagingSyncFailure {
        previous,
        _same_thread: PhantomData,
    }
}

pub(super) fn check_staging_sync(parent: &Path) -> Result<(), crate::store::Error> {
    let armed = STAGING_SYNC_FAILURE.with(|failure| {
        let mut slot = failure.borrow_mut();
        let matches = slot.as_deref().is_some_and(|root| parent.starts_with(root));
        if matches {
            *slot = None;
        }
        matches
    });
    if armed {
        return Err(crate::store::Error::Io(io::Error::from_raw_os_error(
            libc::EIO,
        )));
    }
    Ok(())
}

thread_local! {
    static WORKSPACE_CHECK: RefCell<Option<Box<dyn FnOnce()>>> = const { RefCell::new(None) };
}

/// Restore the previous hook when the isolated test scope exits, including unwinding.
pub(crate) struct WorkspaceCheckHook {
    previous: Option<Box<dyn FnOnce()>>,
    _same_thread: PhantomData<Rc<()>>,
}

impl Drop for WorkspaceCheckHook {
    fn drop(&mut self) {
        WORKSPACE_CHECK.with(|hook| {
            let _ = hook.replace(self.previous.take());
        });
    }
}

/// Run `action` once, after recovery verified the workspace identity and before
/// it removes staging names; lets tests replace the workspace in that window.
pub(crate) fn replace_workspace_after_check(action: impl FnOnce() + 'static) -> WorkspaceCheckHook {
    let previous = WORKSPACE_CHECK.with(|hook| hook.replace(Some(Box::new(action))));
    WorkspaceCheckHook {
        previous,
        _same_thread: PhantomData,
    }
}

pub(crate) fn check_workspace() {
    let action = WORKSPACE_CHECK.with(|hook| hook.borrow_mut().take());
    if let Some(action) = action {
        action();
    }
}

thread_local! {
    static ENUMERATION_FAILURE: RefCell<Option<(PathBuf, usize, usize)>> = const { RefCell::new(None) };
}

/// Restore the previous fault when the isolated test scope exits, including unwinding.
pub(crate) struct EnumerationFailure {
    previous: Option<(PathBuf, usize, usize)>,
    _same_thread: PhantomData<Rc<()>>,
}

impl Drop for EnumerationFailure {
    fn drop(&mut self) {
        ENUMERATION_FAILURE.with(|failure| {
            let _ = failure.replace(self.previous.take());
        });
    }
}

fn arm_enumeration(path: &Path, after: usize, skip: usize) -> EnumerationFailure {
    let previous =
        ENUMERATION_FAILURE.with(|failure| failure.replace(Some((path.into(), after, skip))));
    EnumerationFailure {
        previous,
        _same_thread: PhantomData,
    }
}

/// Fail the next enumeration of `path` with EIO once `after` entries were read,
/// so the injected fault lands on the `readdir` boundary with a partial list
/// pending instead of at the entrance to the enumeration.
pub(crate) fn fail_enumeration(path: &Path, after: usize) -> EnumerationFailure {
    arm_enumeration(path, after, 0)
}

/// Like [`fail_enumeration`], but lets the first `skip` matching enumerations
/// succeed, targeting a later pass over the same directory such as the `extras`
/// listing in `replay::finish` after `replay::classify` already listed it.
pub(crate) fn fail_enumeration_nth(path: &Path, after: usize, skip: usize) -> EnumerationFailure {
    arm_enumeration(path, after, skip)
}

/// True once the armed fault applies to this directory and enough entries were
/// read, so the caller raises EIO on the null return of `readdir`.
pub(super) fn enumeration_failure(path: &Path, collected: usize) -> bool {
    ENUMERATION_FAILURE.with(|failure| {
        let mut slot = failure.borrow_mut();
        let Some((armed, after, skip)) = slot.as_mut() else {
            return false;
        };
        if path != armed.as_path() || collected < *after {
            return false;
        }
        if *skip > 0 {
            *skip -= 1;
            return false;
        }
        *slot = None;
        true
    })
}
