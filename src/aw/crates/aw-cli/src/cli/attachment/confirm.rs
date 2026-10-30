//! A native ConfigChange confirms Qoder adopted registration before the first prompt.

use super::super::{adapter::HookBinding, read_file, Result};
use aw_service::{Client, LifecyclePhase, Operation};
use serde_json::{json, Value};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

fn lock(file: File, deadline: Instant) -> Result<File> {
    loop {
        // SAFETY: the directory descriptor stays open throughout this lock.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            if file.metadata()?.nlink() == 0 {
                return Err("attachment artifacts were removed".into());
            }
            return Ok(file);
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::WouldBlock {
            return Err(error.into());
        }
        if Instant::now() >= deadline {
            return Err("attachment artifact lock timed out".into());
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn directory(path: &Path) -> Result<File> {
    Ok(OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(path)?)
}

pub(super) fn cleanup(path: &Path) -> Result<()> {
    // Acquire only after native confirmation, so callbacks can acknowledge detach.
    let _lock = lock(directory(path)?, Instant::now() + Duration::from_secs(2))?;
    fs::remove_dir_all(path)?;
    Ok(())
}

pub(crate) fn confirm(
    binding: &HookBinding,
    path: &str,
    native: &Value,
    deadline: Instant,
) -> Result<()> {
    // Serialize receipt publication with final cleanup. A callback that opened
    // the directory before deletion must not recreate files after taking the lock.
    let parent = Path::new(path)
        .parent()
        .ok_or("missing confirmation directory")?;
    let _lock = lock(directory(parent)?, deadline)?;
    let description: Value = serde_json::from_slice(&read_file(path, 4096)?)?;
    if binding.adapter != "qoder"
        || native["hook_event_name"] != "ConfigChange"
        || native["file_path"] != description["settings"]
    {
        return Err("native configuration confirmation mismatch".into());
    }
    let ready: PathBuf = description["ready"]
        .as_str()
        .ok_or("missing ready path")?
        .into();
    let client = Client::connect(&binding.socket, deadline)?;
    if client.identity() != &binding.binding.identity {
        return Err("stale attachment binding".into());
    }
    // SAFETY: exec-form native callbacks are children of Qoder, without a shell.
    let pid = unsafe { libc::getppid() } as u32;
    if ready.try_exists()? {
        return Ok(());
    }
    if description["detaching"] != true {
        client.call(
            Operation::BridgeLifecycle {
                instance_id: binding.binding.instance_id.clone(),
                pid,
                phase: LifecyclePhase::Ready,
                key: String::new(),
            },
            deadline,
        )?;
    }
    let temporary = ready.with_extension(format!("{}.tmp", std::process::id()));
    let result = (|| -> Result<()> {
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        output.write_all(&serde_json::to_vec(&json!({"version":1,"adapter":"qoder","pid":pid,"token":description["token"],"hooks":0}))?)?;
        output.sync_all()?;
        fs::rename(&temporary, &ready)?;
        Ok(())
    })();
    if temporary.try_exists()? {
        fs::remove_file(temporary)?;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn receipt_cleanup_waits_for_writer_and_rejects_a_late_callback() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/attachment-tests");
        fs::create_dir_all(&root).unwrap();
        let temporary = tempfile::tempdir_in(root).unwrap();
        let artifacts = temporary.path().join("artifacts");
        fs::create_dir(&artifacts).unwrap();
        let writer = lock(
            directory(&artifacts).unwrap(),
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap();
        let late_callback = directory(&artifacts).unwrap();
        let path = artifacts.clone();
        let (send, receive) = mpsc::channel();
        let cleanup_thread = thread::spawn(move || {
            send.send(()).unwrap();
            cleanup(&path).map_err(|error| error.to_string())
        });
        receive.recv_timeout(Duration::from_secs(2)).unwrap();
        thread::sleep(Duration::from_millis(20));
        assert!(artifacts.exists());
        // A confirmation already in flight finishes publication before removal.
        fs::write(artifacts.join("detach-ready.json"), b"confirmed").unwrap();
        drop(writer);
        cleanup_thread.join().unwrap().unwrap();
        assert!(!artifacts.exists());
        // An opened but delayed callback sees the unlinked directory under lock.
        let error = lock(late_callback, Instant::now() + Duration::from_secs(2)).unwrap_err();
        assert!(error.to_string().contains("artifacts were removed"));
        assert!(!artifacts.exists());
    }
}
