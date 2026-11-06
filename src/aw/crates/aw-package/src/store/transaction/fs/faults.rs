//! Thread-local storage faults exercise the rename-visible, directory-sync-failed window.

use std::{
    cell::RefCell,
    io,
    marker::PhantomData,
    path::{Path, PathBuf},
    rc::Rc,
};

thread_local! {
    static DATABASE_SYNC_FAILURE: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
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
