//! Owned external daemon and bounded CLI processes for launcher contract tests.

use aw_service::{Client, Operation};
use serde_json::{json, Value};
use std::{
    fs,
    os::unix::{
        fs::{DirBuilderExt, PermissionsExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::atomic::{AtomicUsize, Ordering},
    thread,
    time::{Duration, Instant},
};

pub const TIMEOUT: Duration = Duration::from_secs(12);
pub const LITERAL: &str = "literal ; $(touch NEVER) $HOME ' spaces";
static NEXT: AtomicUsize = AtomicUsize::new(0);

pub struct Fixture {
    pub root: PathBuf,
    pub python: PathBuf,
    next: AtomicUsize,
    native_profile: Option<PathBuf>,
}
impl Fixture {
    pub fn new() -> Self {
        let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/launcher-tests");
        fs::create_dir_all(&parent).unwrap();
        let root = parent.join(format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let root = fs::canonicalize(root).unwrap();
        fs::create_dir(root.join("home")).unwrap();
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/launcher/qoder.py"),
            root.join("fake qoder"),
        )
        .unwrap();
        fs::set_permissions(root.join("fake qoder"), fs::Permissions::from_mode(0o700)).unwrap();
        let python = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .map(|directory| directory.join("python3"))
            .find(|candidate| candidate.is_file())
            .expect("launcher tests require python3 on PATH");
        Self {
            root,
            python: fs::canonicalize(python).unwrap(),
            next: AtomicUsize::new(0),
            native_profile: None,
        }
    }
    pub fn native_profile(&mut self) -> PathBuf {
        // Workspace ancestors may be group-writable; /tmp is root-owned sticky.
        let mut template = b"/tmp/aw-hermes-launcher-XXXXXX\0".to_vec();
        // SAFETY: the terminated writable template creates an exclusively owned directory.
        let path = unsafe { libc::mkdtemp(template.as_mut_ptr().cast()) };
        assert!(!path.is_null());
        let path = PathBuf::from(unsafe { std::ffi::CStr::from_ptr(path) }.to_str().unwrap());
        assert!(self.native_profile.is_none());
        self.native_profile = Some(path.clone());
        path
    }
    pub fn action(&self) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/launcher/action.py")
    }
    pub fn document(&self) -> Value {
        json!({"apiVersion":"aw/v1alpha1","kind":"AWConfiguration","metadata":{"name":"launcher"},"spec":{
            "daemon":{"startup":"external","endpoint":"auto","state_dir":self.root.join("state")},
            "execution":{"guarantee":"native_hook","default_event_budget_ms":5000},
            "audit":{"enabled":true,"payload":"metadata_only"},
            "agents":{"qoder":{"adapter":"qoder","argv":[self.root.join("fake qoder")]}},
            "providers":{},"events":{}}})
    }
    pub fn native(&self, document: &mut Value, label: &str, behavior: &str) {
        document["spec"]["providers"][label] = json!({"protocol":"native-hook/v1alpha1",
            "transport":{"type":"stdio","location":"agent","argv":[self.python,self.action(),"--root",self.root,"--protocol","native","--label",label,"--behavior",behavior,"--literal",LITERAL]},
            "timeout_ms":4500,"max_output_bytes":4096,"config":{}});
        if document["spec"]["events"]["tool.before"].is_null() {
            document["spec"]["events"]["tool.before"] = json!({"enabled":true,"steps":[]});
        }
        document["spec"]["events"]["tool.before"]["steps"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id":label,"provider":label,"native":{},"on_error":"block"}));
    }
    pub fn provider(&self, document: &mut Value) {
        document["spec"]["providers"]["policy"] = json!({"protocol":"aw-provider/v1alpha1",
            "transport":{"type":"stdio","location":"agent","argv":[self.python,self.action(),"--root",self.root,"--protocol","provider"]},
            "timeout_ms":4500,"max_output_bytes":4096,"config":{}});
        document["spec"]["events"] = json!({
            "tool.before":{"enabled":true,"steps":[{"id":"check","provider":"policy","operation":"check","effects":["observe","block"],"on_error":"block"}]},
            "tool.after":{"enabled":true,"steps":[{"id":"record","provider":"policy","operation":"record","effects":["observe"],"on_error":"report"}]}});
    }
    pub fn save(&self, document: &Value) {
        fs::write(
            self.root.join("aw.json"),
            serde_json::to_vec(document).unwrap(),
        )
        .unwrap();
    }
    pub fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_aw"));
        command
            .current_dir(&self.root)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.root.join("home"))
            .env("FAKE_ROOT", &self.root);
        command
    }
    pub fn launch(&self, native: &[&str], settings: Option<&Path>) -> Command {
        let mut command = self.command();
        command
            .args(["run", "--config"])
            .arg(self.root.join("aw.json"))
            .args(["--agent", "qoder"]);
        if let Some(settings) = settings {
            command.arg("--native-settings").arg(settings);
        }
        command.arg("--").args(native);
        command
    }
    pub fn run(&self, command: Command) -> Output {
        Process::spawn(self, command).finish()
    }
    pub fn successful(&self, command: Command) -> Value {
        let output = self.run(command);
        assert!(
            output.status.success(),
            "launcher: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
    pub fn clean_launches(&self) {
        assert!(!fs::read_dir(self.root.join("state"))
            .unwrap()
            .any(|entry| entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("launch-")));
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(profile) = &self.native_profile {
            fs::remove_dir_all(profile).unwrap();
        }
        fs::remove_dir_all(&self.root).unwrap();
    }
}

pub struct Process {
    child: Child,
    stdout: PathBuf,
    stderr: PathBuf,
    record: PathBuf,
    value: Value,
}
impl Process {
    pub fn spawn(fixture: &Fixture, mut command: Command) -> Self {
        let id = fixture.next.fetch_add(1, Ordering::Relaxed);
        let stdout = fixture.root.join(format!("process-{id}.stdout"));
        let stderr = fixture.root.join(format!("process-{id}.stderr"));
        let record = fixture.root.join(format!("process-{id}.json"));
        let mut value = json!({"command":std::iter::once(command.get_program().to_string_lossy().into_owned()).chain(command.get_args().map(|a|a.to_string_lossy().into_owned())).collect::<Vec<_>>(),
            "cwd":fixture.root,"pid":null,"ports":[],"stdout":stdout,"stderr":stderr,"timeout_seconds":14,"stop":null});
        fs::write(&record, value.to_string()).unwrap();
        let child = command
            .stdin(Stdio::null())
            .stdout(fs::File::create(&stdout).unwrap())
            .stderr(fs::File::create(&stderr).unwrap())
            .process_group(0)
            .spawn()
            .unwrap();
        value["pid"] = json!(child.id());
        value["stop"] = json!(format!("kill -TERM -- -{}", child.id()));
        fs::write(&record, value.to_string()).unwrap();
        Self {
            child,
            stdout,
            stderr,
            record,
            value,
        }
    }
    pub fn signal(&self, signal: i32) {
        // SAFETY: this unreaped Child reserves its PID until finish or Drop.
        assert_eq!(unsafe { libc::kill(self.child.id() as i32, signal) }, 0);
    }
    pub fn finish(mut self) -> Output {
        let deadline = Instant::now() + TIMEOUT;
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "owned CLI timed out; stderr: {}",
                fs::read_to_string(&self.stderr).unwrap_or_default()
            );
            thread::sleep(Duration::from_millis(5));
        };
        self.value["exited"] = json!(true);
        fs::write(&self.record, self.value.to_string()).unwrap();
        Output {
            status,
            stdout: fs::read(&self.stdout).unwrap(),
            stderr: fs::read(&self.stderr).unwrap(),
        }
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        if self.child.try_wait().unwrap().is_none() {
            // This unreaped Child reserves its PID; signal only the owned launcher.
            unsafe {
                libc::kill(self.child.id() as libc::pid_t, libc::SIGTERM);
            }
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline && self.child.try_wait().unwrap().is_none() {
                thread::sleep(Duration::from_millis(5));
            }
            if self.child.try_wait().unwrap().is_none() {
                self.child.kill().unwrap();
            }
        }
        self.child.wait().unwrap();
    }
}

pub struct Service {
    pub client: Client,
    process: Option<Process>,
}
impl Service {
    pub fn start(fixture: &Fixture, document: &Value) -> Self {
        fixture.save(document);
        let mut command = fixture.command();
        command
            .args(["serve", "--config"])
            .arg(fixture.root.join("aw.json"))
            .arg("--state-dir")
            .arg(fixture.root.join("state"));
        let process = Process::spawn(fixture, command);
        let deadline = Instant::now() + TIMEOUT;
        let client = loop {
            if let Ok(client) = Client::connect(
                fixture.root.join("state/aw.sock"),
                Instant::now() + Duration::from_millis(100),
            ) {
                break client;
            }
            assert!(Instant::now() < deadline, "daemon startup failed");
            thread::sleep(Duration::from_millis(10));
        };
        Self {
            client,
            process: Some(process),
        }
    }
    pub fn status(&self) -> Value {
        self.client
            .call(Operation::Status, Instant::now() + TIMEOUT)
            .unwrap()
    }
    pub fn released(&self, fixture: &Fixture) {
        assert_eq!(self.status()["bindings"], 0);
        assert_eq!(self.status()["events"], 0);
        fixture.clean_launches();
    }
}
impl Drop for Service {
    fn drop(&mut self) {
        let stopped = self.client.call(Operation::Stop, Instant::now() + TIMEOUT);
        let result = self.process.take().unwrap().finish();
        if !thread::panicking() {
            stopped.unwrap();
            assert!(
                result.status.success(),
                "daemon teardown: {}",
                String::from_utf8_lossy(&result.stderr)
            );
        }
    }
}
