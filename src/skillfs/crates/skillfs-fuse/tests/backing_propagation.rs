//! A backing alias must survive over-mounts in namespaces that already exist.

#![cfg(target_os = "linux")]

use skillfs_fuse::security::backing_root::LedgerBackingRoot;
use std::io::Read as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::{Child, Command, Stdio};

const CASE: &str = "backing_survives_overmount_in_existing_namespace";
const CHILD_ROOT: &str = "SKILLFS_BACKING_PROPAGATION_TEST_ROOT";

#[test]
#[ignore = "requires CAP_SYS_ADMIN, mount and unshare; run in a disposable Linux container"]
fn backing_survives_overmount_in_existing_namespace() {
    if let Some(root) = std::env::var_os(CHILD_ROOT) {
        exercise(Path::new(&root));
        return;
    }

    // All mounts live in the subprocess namespace. Even an assertion failure leaves
    // the parent free to remove only its own ordinary temporary files.
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    checked(
        Command::new("unshare")
            .args(["--mount", "--propagation", "private"])
            .arg(std::env::current_exe().unwrap())
            .args(["--exact", CASE, "--ignored", "--nocapture"])
            .env(CHILD_ROOT, root.path()),
    );
}

fn exercise(root: &Path) {
    let source = root.join("source");
    let replacement = root.join("replacement");
    let backing_path = root.join("backing");
    for path in [&source, &replacement] {
        std::fs::create_dir(path).unwrap();
    }
    std::fs::write(source.join("marker"), "original").unwrap();
    std::fs::write(replacement.join("marker"), "replacement").unwrap();
    checked(Command::new("mount").arg("--bind").arg(root).arg(root));
    checked(Command::new("mount").arg("--make-shared").arg(root));

    // Match systemd's receiving namespace and wait until its mount setup is complete.
    let mut receiver = Receiver(
        Command::new("unshare")
            .args(["--mount", "--propagation", "slave", "sh", "-c"])
            .arg("printf ready; read -r line")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut ready = [0; 5];
    receiver
        .0
        .stdout
        .as_mut()
        .unwrap()
        .read_exact(&mut ready)
        .unwrap();
    assert_eq!(&ready, b"ready");

    let backing = LedgerBackingRoot::setup(&source, &backing_path, &source, true).unwrap();
    let remote_backing = Path::new("/proc")
        .join(receiver.0.id().to_string())
        .join("root")
        .join(backing_path.strip_prefix("/").unwrap());
    assert_eq!(
        std::fs::read_to_string(remote_backing.join("marker")).unwrap(),
        "original"
    );

    // A bind over-mount exercises the same propagation event as an in-place FUSE mount.
    checked(
        Command::new("mount")
            .arg("--bind")
            .arg(&replacement)
            .arg(&source),
    );
    assert_eq!(
        std::fs::read_to_string(source.join("marker")).unwrap(),
        "replacement"
    );
    for path in [&backing_path, &remote_backing] {
        assert_eq!(
            std::fs::read_to_string(path.join("marker")).unwrap(),
            "original",
            "backing was over-mounted: {}",
            path.display()
        );
    }
    drop(receiver);
    drop(backing);
    assert!(!backing_path.exists(), "backing mount was not cleaned up");
    checked(Command::new("umount").arg(&source));
    checked(Command::new("umount").arg(root));
}

struct Receiver(Child);

impl Drop for Receiver {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn checked(command: &mut Command) {
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{command:?}\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
