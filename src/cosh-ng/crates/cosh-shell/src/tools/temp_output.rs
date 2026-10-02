//! Anonymous temp-file capture for child stdout/stderr buffering.
//!
//! The guarded executors buffer child output in temp files instead of
//! pipes so the wait loop never deadlocks on a full pipe buffer. The
//! files are anonymous (unlinked at creation): no path ever appears in
//! the shared temp dir, so a local attacker can neither pre-plant nor
//! swap a symlink to redirect the child's writes or poison the read-back,
//! and a crash can never leave world-readable output behind.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};

pub(crate) struct TempOutput {
    file: File,
}

impl TempOutput {
    pub(crate) fn new() -> io::Result<Self> {
        Ok(Self {
            file: tempfile::tempfile()?,
        })
    }

    /// Child-facing half of the capture file. The cloned handle shares the
    /// file offset with `self`, which is safe because the parent only
    /// rewinds and reads after the child has exited.
    pub(crate) fn try_clone(&self) -> io::Result<File> {
        self.file.try_clone()
    }

    /// Rewinds so the next reader (parent read-back or the next pipeline
    /// stage's stdin) starts at the beginning.
    pub(crate) fn rewind(&mut self) -> io::Result<()> {
        self.file.seek(SeekFrom::Start(0)).map(|_| ())
    }

    /// Reads the captured bytes from the start.
    pub(crate) fn read_all(&mut self) -> io::Result<Vec<u8>> {
        self.rewind()?;
        let mut bytes = Vec::new();
        self.file.read_to_end(&mut bytes)?;
        Ok(bytes)
    }
}

/// Test-only attacker shared by the swap-race probes: polls a directory
/// for files whose names start with the executor's historical temp-file
/// prefix, unlinks them, and symlinks the same name to a victim file.
#[cfg(test)]
pub(crate) mod test_support {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::thread::JoinHandle;
    use std::time::Duration;

    /// Builds the probe prefix for this process. The pid is bracketed by
    /// dashes so a probe running as pid 123 never matches files belonging
    /// to pid 1234.
    pub(crate) fn prefix_for(stem: &str, pid: u32) -> String {
        format!("{stem}-{pid}-")
    }

    pub(crate) struct SwapAttacker {
        pub(crate) sightings: Arc<AtomicUsize>,
        pub(crate) swaps: Arc<AtomicUsize>,
        pub(crate) enum_errors: Arc<AtomicUsize>,
        stop_flag: Arc<AtomicBool>,
        handle: Option<JoinHandle<()>>,
    }

    impl SwapAttacker {
        /// Starts the attacker against `dir`, matching this process's
        /// historical temp files for `stem` (e.g. "cosh-guarded-diagnostic").
        pub(crate) fn for_process(stem: &str, dir: &Path, victim: PathBuf) -> Self {
            let prefix = prefix_for(stem, std::process::id());
            let sightings = Arc::new(AtomicUsize::new(0));
            let swaps = Arc::new(AtomicUsize::new(0));
            let enum_errors = Arc::new(AtomicUsize::new(0));
            let stop_flag = Arc::new(AtomicBool::new(false));
            let handle = {
                let dir = dir.to_path_buf();
                let sightings = Arc::clone(&sightings);
                let swaps = Arc::clone(&swaps);
                let enum_errors = Arc::clone(&enum_errors);
                let stop_flag = Arc::clone(&stop_flag);
                std::thread::spawn(move || {
                    while !stop_flag.load(Ordering::Relaxed) {
                        match std::fs::read_dir(&dir) {
                            Ok(entries) => {
                                for entry in entries.flatten() {
                                    let name = entry.file_name();
                                    let Some(name) = name.to_str() else { continue };
                                    if !name.starts_with(&prefix) {
                                        continue;
                                    }
                                    sightings.fetch_add(1, Ordering::Relaxed);
                                    let path = entry.path();
                                    if std::fs::remove_file(&path).is_ok()
                                        && std::os::unix::fs::symlink(&victim, &path).is_ok()
                                    {
                                        swaps.fetch_add(1, Ordering::Relaxed);
                                    }
                                }
                            }
                            // A probe that cannot enumerate the directory
                            // must not be able to pass with zero sightings.
                            Err(_) => {
                                enum_errors.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                        std::thread::sleep(Duration::from_micros(50));
                    }
                })
            };
            Self {
                sightings,
                swaps,
                enum_errors,
                stop_flag,
                handle: Some(handle),
            }
        }

        /// Stops the attacker and waits for its thread to exit.
        pub(crate) fn finish(mut self) {
            self.shutdown();
        }

        fn shutdown(&mut self) {
            self.stop_flag.store(true, Ordering::Relaxed);
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    // A panicking probe must not leave the attacker scanning and swapping
    // files for the rest of the test process lifetime.
    impl Drop for SwapAttacker {
        fn drop(&mut self) {
            self.shutdown();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{prefix_for, SwapAttacker};
    use super::*;
    use std::io::Write;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    #[test]
    fn temp_output_round_trips_writes_through_clone() {
        let mut output = TempOutput::new().expect("temp output");
        output
            .try_clone()
            .expect("clone")
            .write_all(b"hello")
            .expect("write via clone");
        assert_eq!(output.read_all().expect("read back"), b"hello");
        // A second read starts from the beginning again.
        assert_eq!(output.read_all().expect("re-read"), b"hello");
    }

    #[test]
    fn probe_prefix_brackets_pid_with_dashes() {
        let prefix = prefix_for("cosh-guarded-diagnostic", 123);
        assert!("cosh-guarded-diagnostic-123-9-stdout".starts_with(&prefix));
        assert!(!"cosh-guarded-diagnostic-1234-9-stdout".starts_with(&prefix));
    }

    #[test]
    fn swap_attacker_surfaces_enumeration_errors() {
        let missing =
            std::env::temp_dir().join(format!("cosh-probe-no-such-dir-{}", std::process::id()));
        let victim = tempfile::NamedTempFile::new().expect("victim");
        let attacker =
            SwapAttacker::for_process("cosh-never-matches", &missing, victim.path().to_path_buf());
        std::thread::sleep(Duration::from_millis(20));
        let errors = attacker.enum_errors.load(Ordering::Relaxed);
        attacker.finish();
        assert!(errors > 0, "enumeration failures must be counted");
    }
}
