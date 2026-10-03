//! Receipt-derived journals for OpenCode link replacement and cleanup.
//!
//! The Manager holds the install lock and persists every allowed target before
//! calling this module. Fixed journal slots survive process exit without adding
//! untrusted paths to the receipt. Only matching symlinks may be discarded.

use std::fs;
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::AdapterError;
use super::util::symlink_matches_at;

/// Replay pending work, then atomically install or remove a receipt-owned link.
pub(super) fn reconcile(
    link: &Path,
    targets: &[&Path],
    replacement: Option<&Path>,
    before_change: impl FnOnce(),
    after_change: impl FnOnce(),
) -> Result<bool, AdapterError> {
    let parent = link.parent().ok_or_else(|| AdapterError::Io {
        path: link.to_path_buf(),
        source: io::Error::new(io::ErrorKind::InvalidInput, "plugin link has no parent"),
    })?;
    let journal = Journal {
        link,
        parent,
        targets,
        directory: journal_path(link),
    };
    let result: io::Result<bool> = (|| {
        if present(&journal.directory)? {
            journal.validate()?;
            if !journal.recover()? {
                journal.finish()?;
                return Ok(false);
            }
            journal.finish()?;
        }
        if let Some(target) = replacement
            && symlink_matches_at(link, link, target).map_err(io::Error::other)?
        {
            return Ok(true);
        }
        if !present(link)? {
            if let Some(target) = replacement {
                fs::create_dir_all(parent)?;
                std::os::unix::fs::symlink(target, link)?;
                sync_directory(parent)?;
            }
            return Ok(true);
        }
        if !journal.matches(link)? {
            return Ok(false);
        }
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&journal.directory)?;
        sync_directory(parent)?;
        let matched = if let Some(target) = replacement {
            let staged = journal.directory.join("replacement");
            std::os::unix::fs::symlink(target, &staged)?;
            sync_directory(&journal.directory)?;
            before_change();
            // Exchange retains the displaced inode and never leaves the public
            // pathname empty. An unsupported filesystem fails before mutation.
            rename(&staged, link, true)?;
            journal.sync()?;
            after_change();
            journal.recover()?
        } else {
            before_change();
            journal.detach()?;
            after_change();
            journal.recover()?
        };
        journal.finish()?;
        Ok(matched)
    })();
    result.map_err(|source| AdapterError::Io {
        path: journal.directory.clone(),
        source: io::Error::other(format!(
            "link transaction for {}: {source}; preserved entries in {}; clear any public-path conflict and retry enable or disable",
            link.display(), journal.directory.display()
        )),
    })
}

// The persisted receipt's link pathname identifies this journal even when the
// public link or its source no longer exists. The digest avoids filename limits.
fn journal_path(link: &Path) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    link.with_file_name(format!(
        ".anolisa-link-{:x}",
        Sha256::digest(link.as_os_str().as_bytes())
    ))
}

struct Journal<'a> {
    link: &'a Path,
    parent: &'a Path,
    targets: &'a [&'a Path],
    directory: PathBuf,
}

impl Journal<'_> {
    fn validate(&self) -> io::Result<()> {
        let metadata = fs::symlink_metadata(&self.directory)?;
        if !metadata.is_dir()
            || metadata.uid() != nix::unistd::geteuid().as_raw()
            || metadata.mode() & 0o777 != 0o700
        {
            return Err(io::Error::other(
                "journal must be an owned private directory",
            ));
        }
        Ok(())
    }

    fn matches(&self, path: &Path) -> io::Result<bool> {
        for target in self.targets {
            if symlink_matches_at(path, self.link, target).map_err(io::Error::other)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn sync(&self) -> io::Result<()> {
        sync_directory(&self.directory)?;
        sync_directory(self.parent)
    }

    fn detach(&self) -> io::Result<()> {
        match rename(self.link, &self.directory.join("removed"), false) {
            Ok(()) => self.sync(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn recover_removed(&self) -> io::Result<bool> {
        let captured = self.directory.join("removed");
        if !present(&captured)? {
            return Ok(true);
        }
        let matched = self.matches(&captured)?;
        if matched {
            fs::remove_file(&captured)?;
        } else {
            // Unlike hard-link restoration, exclusive rename supports directories.
            rename(&captured, self.link, false)?;
        }
        self.sync()?;
        Ok(matched)
    }

    fn recover(&self) -> io::Result<bool> {
        if !self.recover_removed()? {
            return Ok(false);
        }
        let captured = self.directory.join("replacement");
        if !present(&captured)? {
            return Ok(true);
        }
        if self.matches(&captured)? {
            fs::remove_file(&captured)?;
            self.sync()?;
            return Ok(true);
        }
        // A competing installer replaced the checked entry before exchange.
        // Roll back our replacement, verifying its detached inode again before
        // deletion. Either slot remains replayable if restoration is blocked.
        if self.matches(self.link)? {
            self.detach()?;
            self.recover_removed()?;
        }
        rename(&captured, self.link, false)?;
        self.sync()?;
        Ok(false)
    }

    fn finish(&self) -> io::Result<()> {
        fs::remove_dir(&self.directory)?;
        sync_directory(self.parent)
    }
}

fn present(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn sync_directory(path: &Path) -> io::Result<()> {
    fs::File::open(path)?.sync_all()
}

fn rename(from: &Path, to: &Path, exchange: bool) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let from = CString::new(from.as_os_str().as_bytes())?;
    let to = CString::new(to.as_os_str().as_bytes())?;
    #[cfg(target_os = "linux")]
    // SAFETY: both C strings are valid through the syscall. Using the syscall
    // directly preserves compatibility with our pre-glibc-2.28 release baseline.
    let result = unsafe {
        nix::libc::syscall(
            nix::libc::SYS_renameat2,
            nix::libc::AT_FDCWD,
            from.as_ptr(),
            nix::libc::AT_FDCWD,
            to.as_ptr(),
            if exchange {
                nix::libc::RENAME_EXCHANGE
            } else {
                nix::libc::RENAME_NOREPLACE
            },
        )
    };
    #[cfg(target_os = "macos")]
    // SAFETY: both C strings are valid through the call; no pointers are retained.
    let result = unsafe {
        nix::libc::renameatx_np(
            nix::libc::AT_FDCWD,
            from.as_ptr(),
            nix::libc::AT_FDCWD,
            to.as_ptr(),
            if exchange {
                nix::libc::RENAME_SWAP
            } else {
                nix::libc::RENAME_EXCL
            },
        )
    };
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    return Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "atomic link transactions require Linux or macOS",
    ));
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if result == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn exchange_has_no_vacant_path_for_a_competing_installer() {
        use std::sync::Barrier;
        let tmp = tempfile::tempdir().unwrap();
        let link = tmp.path().join("plugin.js");
        let old = tmp.path().join("old.js");
        let new = tmp.path().join("new.js");
        symlink(&old, &link).unwrap();
        let barrier = Barrier::new(2);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                for expected in [&old, &new] {
                    barrier.wait();
                    assert_eq!(fs::read_link(&link).unwrap(), *expected);
                    assert_eq!(
                        symlink("competitor.js", &link).unwrap_err().kind(),
                        io::ErrorKind::AlreadyExists
                    );
                    barrier.wait();
                }
            });
            assert!(
                reconcile(
                    &link,
                    &[&old, &new],
                    Some(&new),
                    || {
                        barrier.wait();
                        barrier.wait();
                    },
                    || {
                        barrier.wait();
                        barrier.wait();
                    }
                )
                .unwrap()
            );
        });
        assert_eq!(fs::read_link(&link).unwrap(), new);
        assert!(!journal_path(&link).exists());
    }

    #[test]
    fn exchange_failure_keeps_the_previous_entry_available() {
        let tmp = tempfile::tempdir().unwrap();
        let link = tmp.path().join("plugin.js");
        let old = tmp.path().join("old.js");
        let new = tmp.path().join("new.js");
        symlink(&old, &link).unwrap();
        // Remove the staged source to deterministically fail the exchange itself.
        assert!(
            reconcile(
                &link,
                &[&old, &new],
                Some(&new),
                || {
                    fs::remove_file(journal_path(&link).join("replacement")).unwrap();
                },
                || {}
            )
            .is_err()
        );
        assert_eq!(fs::read_link(&link).unwrap(), old);
        assert!(reconcile(&link, &[&old, &new], Some(&new), || {}, || {}).unwrap());
        assert_eq!(fs::read_link(&link).unwrap(), new);
    }

    #[test]
    fn raced_entries_are_restored_without_overwriting() {
        for exchange in [false, true] {
            for directory in [false, true] {
                for occupied in [false, true] {
                    let tmp = tempfile::tempdir().unwrap();
                    let link = tmp.path().join("plugin.js");
                    let old = tmp.path().join("old.js");
                    let new = tmp.path().join("new.js");
                    symlink(&old, &link).unwrap();
                    let result = reconcile(
                        &link,
                        &[&old, &new],
                        exchange.then_some(new.as_path()),
                        || {
                            fs::remove_file(&link).unwrap();
                            if directory {
                                fs::create_dir(&link).unwrap();
                                fs::write(link.join("user.js"), "user data").unwrap();
                            } else {
                                fs::write(&link, "user data").unwrap();
                            }
                        },
                        || {
                            if occupied {
                                if exchange {
                                    fs::remove_file(&link).unwrap();
                                }
                                fs::write(&link, "second installer").unwrap();
                            }
                        },
                    );
                    if occupied {
                        assert!(result.is_err());
                        assert_eq!(fs::read_to_string(&link).unwrap(), "second installer");
                        fs::remove_file(&link).unwrap();
                        assert!(!reconcile(&link, &[&old, &new], None, || {}, || {}).unwrap());
                    } else {
                        assert!(!result.unwrap());
                    }
                    let data = if directory {
                        link.join("user.js")
                    } else {
                        link.clone()
                    };
                    assert_eq!(fs::read_to_string(data).unwrap(), "user data");
                    assert!(!journal_path(&link).exists());
                    // A restored user entry is still a conflict, never successful cleanup.
                    assert!(!reconcile(&link, &[&old, &new], None, || {}, || {}).unwrap());
                }
            }
        }
    }

    #[test]
    fn recovery_rejects_a_symlink_in_place_of_its_private_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let link = tmp.path().join("plugin.js");
        let target = tmp.path().join("source.js");
        symlink(&target, &link).unwrap();
        let outside = tmp.path().join("user-directory");
        fs::create_dir(&outside).unwrap();
        symlink(&target, outside.join("removed")).unwrap();
        symlink(&outside, journal_path(&link)).unwrap();
        assert!(reconcile(&link, &[&target], None, || {}, || {}).is_err());
        assert_eq!(fs::read_link(&link).unwrap(), target);
        assert_eq!(fs::read_link(outside.join("removed")).unwrap(), target);
    }

    #[test]
    fn process_exit_replays_receipt_derived_journal() {
        const CHILD: &str = "ANOLISA_LINK_TRANSACTION_CHILD";
        if let Some(root) = std::env::var_os(CHILD) {
            let root = PathBuf::from(root);
            let scenario = std::env::var("ANOLISA_LINK_TRANSACTION_SCENARIO").unwrap();
            let link = root.join("plugin.js");
            let old = root.join("old.js");
            let new = root.join("new.js");
            let exchange = scenario.starts_with("exchange");
            reconcile(
                &link,
                &[&old, &new],
                exchange.then_some(new.as_path()),
                || {
                    if scenario.ends_with("directory") {
                        fs::remove_file(&link).unwrap();
                        fs::create_dir(&link).unwrap();
                        fs::write(link.join("user.js"), "survives process exit").unwrap();
                    }
                    if scenario.ends_with("before") {
                        std::process::exit(73);
                    }
                },
                || {
                    std::process::exit(73);
                },
            )
            .unwrap();
            panic!("child must exit at the mutation boundary");
        }
        for scenario in [
            "remove",
            "remove_directory",
            "exchange",
            "exchange_directory",
            "exchange_before",
        ] {
            for retry_enable in [false, true] {
                let tmp = tempfile::tempdir().unwrap();
                let link = tmp.path().join("plugin.js");
                let old = tmp.path().join("old.js");
                let new = tmp.path().join("new.js");
                symlink(&old, &link).unwrap();
                let status = std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", "adapter::link_transaction::tests::process_exit_replays_receipt_derived_journal", "--nocapture"])
                    .env(CHILD, tmp.path())
                    .env("ANOLISA_LINK_TRANSACTION_SCENARIO", scenario)
                    .status().unwrap();
                assert_eq!(status.code(), Some(73));
                assert!(journal_path(&link).is_dir());
                let result = reconcile(
                    &link,
                    &[&old, &new],
                    retry_enable.then_some(new.as_path()),
                    || {},
                    || {},
                )
                .unwrap();
                if scenario.ends_with("directory") {
                    assert!(!result);
                    assert_eq!(
                        fs::read_to_string(link.join("user.js")).unwrap(),
                        "survives process exit"
                    );
                } else {
                    assert!(result);
                    if retry_enable {
                        assert_eq!(fs::read_link(&link).unwrap(), new);
                    } else {
                        assert!(!present(&link).unwrap());
                    }
                }
                assert!(!journal_path(&link).exists());
            }
        }
    }
}
