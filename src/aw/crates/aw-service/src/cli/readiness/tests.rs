//! Startup failures must stop the real owned process without accepting stale receipts.

use super::*;
use serde_json::json;
use std::{
    collections::BTreeMap, fs, os::unix::fs::PermissionsExt, path::PathBuf,
    sync::atomic::AtomicUsize,
};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/readiness-tests")
            .join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }
    fn ready(&self) -> Readiness {
        Readiness {
            path: self.0.join("ready.json"),
            token: "owned-launch-token".into(),
            adapter: "fixture".into(),
            hooks: 2,
            timeout: Duration::from_secs(2),
        }
    }
    fn command(&self, mode: &str) -> CommandSpec {
        let script = r#"import json,os,sys,time
from pathlib import Path
root=Path(sys.argv[1]);mode=sys.argv[2]
(root/'process.json').write_text(json.dumps({'pid':os.getpid(),'pgid':os.getpgrp(),'cwd':str(root),'command':sys.argv,'ports':[],'deadline_seconds':3,'stop':f'kill -TERM -- -{os.getpgrp()}'}))
if mode in ('ready','unrelated'):
 p=root/'ready.tmp';p.write_text(json.dumps({'version':1,'adapter':'fixture','token':'owned-launch-token','pid':os.getppid() if mode=='unrelated' else os.getpid(),'hooks':2}));p.chmod(0o600);p.rename(root/'ready.json')
elif mode=='missing':time.sleep(60)
if mode=='unrelated':time.sleep(1);(root/'continued').write_text('unexpected')
"#;
        CommandSpec {
            program: "/usr/bin/python3".into(),
            args: vec![
                "-c".into(),
                script.into(),
                self.0.clone().into_os_string(),
                mode.into(),
            ],
            cwd: self.0.clone(),
            environment: BTreeMap::new(),
        }
    }
    fn reaped(&self) {
        let process: Value =
            serde_json::from_slice(&fs::read(self.0.join("process.json")).unwrap()).unwrap();
        let pid = process["pid"].as_u64().unwrap();
        assert!(!PathBuf::from(format!("/proc/{pid}")).exists());
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn startup_receipt_is_bound_to_launch_and_private_file() {
    let fixture = Fixture::new();
    let ready = fixture.ready();
    assert!(!receipt(&ready, std::process::id()).unwrap());
    let mut value =
        json!({"version":1,"adapter":"fixture","token":"wrong","pid":std::process::id(),"hooks":2});
    fs::write(&ready.path, value.to_string()).unwrap();
    fs::set_permissions(&ready.path, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(receipt(&ready, std::process::id()).is_err());
    value["token"] = json!(ready.token);
    fs::write(&ready.path, value.to_string()).unwrap();
    assert!(receipt(&ready, std::process::id()).unwrap());
    fs::set_permissions(&ready.path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(receipt(&ready, std::process::id()).is_err());
}

#[test]
fn readiness_timeout_cancels_and_reaps_owned_agent() {
    let fixture = Fixture::new();
    let started = Instant::now();
    let result = run(
        &fixture.command("missing"),
        &AtomicI32::new(0),
        Some(fixture.ready()),
    );
    assert!(result.unwrap_err().to_string().contains("startup deadline"));
    assert!(started.elapsed() < Duration::from_secs(4));
    fixture.reaped();
}

#[test]
fn successful_native_startup_preserves_exit_status() {
    let fixture = Fixture::new();
    let status = run(
        &fixture.command("ready"),
        &AtomicI32::new(0),
        Some(fixture.ready()),
    )
    .unwrap();
    assert!(status.success());
    fixture.reaped();
}

#[test]
fn process_exit_is_not_hook_readiness() {
    let fixture = Fixture::new();
    let result = run(
        &fixture.command("early"),
        &AtomicI32::new(0),
        Some(fixture.ready()),
    );
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("before native hooks"));
    fixture.reaped();
}

#[test]
fn unrelated_live_pid_cannot_attest_readiness_and_owned_agent_is_reaped() {
    let fixture = Fixture::new();
    let result = run(
        &fixture.command("unrelated"),
        &AtomicI32::new(0),
        Some(fixture.ready()),
    );
    assert!(result.unwrap_err().to_string().contains("does not match"));
    assert!(!fixture.0.join("continued").exists());
    fixture.reaped();
}

#[test]
fn spawn_failure_returns_without_starting_a_readiness_watcher() {
    let fixture = Fixture::new();
    let mut command = fixture.command("ready");
    command.program = fixture.0.join("missing-agent");
    assert!(run(&command, &AtomicI32::new(0), Some(fixture.ready())).is_err());
    assert!(!fixture.ready().path.exists());
}
