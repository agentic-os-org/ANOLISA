use super::*;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

#[test]
fn snapshots_include_children_of_non_leader_threads() {
    let mut parent = Command::new("python3")
        .args([
            "-c",
            r"
import subprocess, sys, threading
def worker():
    with subprocess.Popen(['cat'], stdin=subprocess.PIPE) as child:
        print(threading.get_native_id(), child.pid, flush=True)
        sys.stdin.read()
        child.stdin.close()
thread = threading.Thread(target=worker)
thread.start()
thread.join()
",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let root = identity(parent.id(), &boot_id().unwrap()).unwrap().0;
    let mut line = String::new();
    BufReader::new(parent.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let (tid, pid) = line.trim().split_once(' ').unwrap();
    let (tid, pid) = (tid.parse::<u32>().unwrap(), pid.parse::<u32>().unwrap());
    assert_ne!(tid, root.pid);
    let family = family_by_parent(&root).unwrap();
    assert!(family.iter().any(|process| process.pid == pid));
    if Path::new("/proc/thread-self/children").exists() {
        let family = subtree(&root, false).unwrap();
        assert_eq!(
            family.iter().map(|process| process.pid).collect::<Vec<_>>(),
            [root.pid, pid]
        );
        let tasks = subtree(&root, true).unwrap();
        assert!(tasks.iter().any(|process| process.pid == tid));
    }
    assert!(parent.wait().unwrap().success());
}
