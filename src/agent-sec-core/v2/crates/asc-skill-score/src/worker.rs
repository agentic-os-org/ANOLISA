//! Fixed installed worker invocation, private file envelopes, and request-owned cancellation cleanup.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, Write};
use std::os::unix::{fs::OpenOptionsExt as _, process::CommandExt as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use asc_model_client::remote::RemoteModel;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use crate::{
    Error, Result,
    bounded_json::{JsonBudget, JsonOutput},
    contracts::{array, text},
    source::{private_json, private_write, read_json},
};

const PROTOCOL: &str = "agent-sec-skill-score/worker-v1";

mod lease;
#[cfg(test)]
mod report_tests;
use lease::Lease;

pub(crate) struct Runtime {
    lease: Lease,
    python: PathBuf,
    entry: PathBuf,
    image_ids: Value,
    pub cleanup_status: String,
    pub resources: Vec<Value>,
}

impl Runtime {
    pub fn installed(prefix: &Path) -> Result<Self> {
        let root = prefix.join("lib/anolisa/sec-core-skill-score");
        let manifest_path = root.join("runtime-manifest.json");
        if !manifest_path.is_file() {
            return Err(Error::Runtime("dynamic assessment unavailable; explicitly install: sudo anolisa --install-mode system install sec-core-skill-score".into()));
        }
        let lease = Lease::acquire(&root)?;
        let bytes = std::fs::read(&manifest_path)?;
        if bytes.len() > 1_048_576 {
            return Err(Error::Runtime("runtime manifest exceeds limit".into()));
        }
        let manifest: Value = serde_json::from_slice(&bytes)?;
        let digest = format!("{:x}", Sha256::digest(&bytes));
        let environment = root.join("environments").join(&digest);
        let ready=read_json(&environment.join("ready.json"),1_048_576).map_err(|_|Error::Runtime("scoring runtime is damaged; run sudo anolisa --install-mode system repair sec-core-skill-score".into()))?;
        let base = std::fs::read_to_string(
            prefix.join("share/anolisa/components/sec-core/component.toml"),
        )?;
        let base_version = base
            .lines()
            .find_map(|line| {
                line.trim()
                    .strip_prefix("version")
                    .and_then(|s| s.trim_start().strip_prefix('='))
                    .map(|s| s.trim().trim_matches('"'))
            })
            .ok_or_else(|| Error::Runtime("installed base component version is missing".into()))?;
        if text(&manifest, "schema") != "agent-sec-skill-score/runtime-v1"
            || text(&manifest, "component") != "sec-core-skill-score"
            || text(&manifest, "protocol_version") != PROTOCOL
            || text(&manifest, "version") != base_version
            || text(&ready, "schema") != "agent-sec-skill-score/ready-v1"
            || text(&ready, "manifest_sha256") != digest
        {
            return Err(Error::Runtime("base/optional scoring component mismatch; explicitly install/update matching ASC versions".into()));
        }
        for field in [
            "version",
            "source_commit",
            "protocol_version",
            "worker_version",
            "harbor_version",
            "images",
        ] {
            if ready[field] != manifest[field] {
                return Err(Error::Runtime(format!(
                    "scoring ready state mismatch: {field}; repair sec-core-skill-score"
                )));
            }
        }
        if ready["python_version"] != manifest["python"]["version"] {
            return Err(Error::Runtime(
                "scoring interpreter version mismatch".into(),
            ));
        }
        let python = environment.join("python/bin/python3.12");
        let entry = environment.join("worker-entry.py");
        if !python.is_file() || !entry.is_file() {
            return Err(Error::Runtime(
                "installed scoring worker is missing; repair sec-core-skill-score".into(),
            ));
        }
        require_system_docker()?;
        let deadline = Instant::now() + Duration::from_secs(45);
        let image_ids = resolve_image_ids(&manifest, |id| {
            docker_until(&["image".into(), "inspect".into(), id.into()], deadline)
        })?;
        Ok(Self {
            lease,
            python,
            entry,
            image_ids,
            cleanup_status: "complete".into(),
            resources: Vec::new(),
        })
    }

    pub fn invoke(&mut self, call: Invocation<'_>) -> Result<Value> {
        let request = self.prepare_request(&call)?;
        let mut command = self.command(call.model)?;
        command.arg(&request.input).arg(&request.output);
        self.lease.begin(&request.id, &request.project)?;
        let mut process = match command.spawn() {
            Ok(process) => process,
            Err(error) => {
                self.lease.complete(&request.id)?;
                return Err(error.into());
            }
        };
        let readers = [
            process.stdout.take().map(log_reader),
            process.stderr.take().map(log_reader),
        ];
        let (execution, interrupted) = await_worker(&mut process, call.deadline, call.cancelled);
        let cleanup = cleanup_owned(&request.project, &request.id);
        let journal = if cleanup {
            self.lease.complete(&request.id)
        } else {
            Ok(())
        };
        self.record_cleanup(&request.id, cleanup && journal.is_ok());
        if !cleanup {
            return reject_unreclaimed(call.root, &request);
        }
        // Collection runs even if logs or the worker failed; raw evidence must never survive rejection.
        let logs = collect_logs(call.root, call.model, readers);
        if let Err(error) = sanitize_or_discard(call.root, call.model) {
            return Err(match error {
                Error::UnsafeEvidence { .. } => Error::UnsafeEvidence {
                    request_id: request.id,
                    compose_project: request.project,
                },
                other => other,
            });
        }
        logs?;
        journal?;
        let status = execution?;
        if interrupted {
            return Err(if call.cancelled.load(Ordering::Relaxed) {
                Error::Cancelled
            } else {
                Error::Deadline
            });
        }
        read_response(&request, status, call.model)
    }

    fn record_cleanup(&mut self, id: &str, complete: bool) {
        if !complete {
            self.cleanup_status = "pending".into();
        }
        if let Some(resource) = self
            .resources
            .iter_mut()
            .find(|r| text(r, "request_id") == id)
        {
            resource["cleanup_status"] = if complete { "complete" } else { "pending" }.into();
        }
    }

    fn prepare_request(&mut self, call: &Invocation<'_>) -> Result<Request> {
        std::fs::create_dir_all(call.root)?;
        let id = uuid::Uuid::new_v4().simple().to_string();
        let project = format!("asc-score-{id}__env");
        let remaining = call
            .deadline
            .checked_duration_since(Instant::now())
            .ok_or(Error::Deadline)?;
        let deadline_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| Error::Runtime("system clock precedes Unix epoch".into()))?
            .as_millis()
            + remaining.as_millis();
        let image_ids = &self.image_ids;
        let ownership = json!({"request_id":id,"compose_project":project,"created":false,"cleanup_status":"pending"});
        private_json(&call.root.join("ownership.json"), &ownership)?;
        self.resources.push(ownership);
        let request = json!({"schema_version":PROTOCOL,"request_id":id,"operation":call.operation,"skill_path":call.snapshot,"root":call.root,
            "timeout":remaining.as_secs_f64(),"deadline_unix_ms":deadline_ms,"compose_project":project,"probe":call.probe,"image_ids":image_ids,
            "config":{"model":call.model.model(),"judge_model":call.model.model(),"judge_max_tokens":24000,"judge_concurrency":4,"docker_image":image_ids["work"],"claude_version":"2.1.143","timeout_seconds":600,"hard_run_limit":24,"deadline_minutes":45,"agentsight_traces":{}}});
        let input = call.root.join("worker-input.json");
        let output = call.root.join("worker-output.json");
        private_json(&input, &request)?;
        Ok(Request {
            id,
            project,
            input,
            output,
        })
    }

    fn command(&self, model: &RemoteModel) -> Result<Command> {
        require_system_docker()?;
        let mut command = Command::new(&self.python);
        command
            .arg("-I")
            .arg(&self.entry)
            .env_clear()
            .envs(model.worker_environment())
            .process_group(0)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for key in [
            "PATH",
            "HOME",
            "LANG",
            "LC_ALL",
            "SSL_CERT_FILE",
            "SSL_CERT_DIR",
        ] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        Ok(command)
    }
}

fn require_system_docker() -> Result<()> {
    if std::env::var_os("DOCKER_HOST").is_some() || std::env::var_os("DOCKER_CONTEXT").is_some() {
        return Err(Error::Runtime(
            "remote/custom Docker contexts are unsupported; use the validated local system daemon"
                .into(),
        ));
    }
    Ok(())
}

fn immutable_image_id(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn resolve_image_ids(
    manifest: &Value,
    mut inspect: impl FnMut(&str) -> Option<String>,
) -> Result<Value> {
    let images = array(manifest, "images");
    if images.len() != 2
        || ["work", "egress"].iter().any(|role| {
            images
                .iter()
                .filter(|image| {
                    text(image, "role") == *role
                        && immutable_image_id(text(image, "id"))
                        && immutable_image_id(text(image, "manifest_id"))
                        && text(image, "platform") == "linux/amd64"
                })
                .count()
                != 1
        })
    {
        return Err(Error::Runtime("manifest needs exactly two Linux amd64 images with fixed config and manifest identities".into()));
    }
    let mut actual = json!({});
    for image in images {
        let role = text(image, "role");
        let allowed = [text(image, "id"), text(image, "manifest_id")];
        for candidate in allowed {
            let Some(output) = inspect(candidate) else {
                continue;
            };
            let values: Value = serde_json::from_str(&output)
                .map_err(|_| Error::Runtime(format!("local {role} image inspection is invalid")))?;
            let values = values
                .as_array()
                .filter(|values| values.len() == 1)
                .ok_or_else(|| {
                    Error::Runtime(format!("local {role} image inspection is invalid"))
                })?;
            let inspected = &values[0];
            if !allowed.contains(&text(inspected, "Id"))
                || text(inspected, "Os") != "linux"
                || text(inspected, "Architecture") != "amd64"
            {
                return Err(Error::Runtime(format!(
                    "local {role} image identity or platform differs from its verified archive"
                )));
            }
            actual[role] = inspected["Id"].clone();
            break;
        }
        if actual.get(role).is_none() {
            return Err(Error::Runtime(format!(
                "required local {role} image is unavailable; repair sec-core-skill-score from its verified archive"
            )));
        }
    }
    Ok(actual)
}

#[derive(Clone, Copy)]
pub(crate) struct Invocation<'a> {
    pub operation: &'a str,
    pub snapshot: &'a Path,
    pub probe: Option<&'a Value>,
    pub root: &'a Path,
    pub model: &'a RemoteModel,
    pub deadline: Instant,
    pub cancelled: &'a AtomicBool,
}

struct Request {
    id: String,
    project: String,
    input: PathBuf,
    output: PathBuf,
}

type LogReceiver = std::sync::mpsc::Receiver<std::io::Result<Vec<u8>>>;

fn await_worker(
    process: &mut std::process::Child,
    deadline: Instant,
    cancelled: &AtomicBool,
) -> (Result<std::process::ExitStatus>, bool) {
    let pid = i32::try_from(process.id())
        .ok()
        .and_then(rustix::process::Pid::from_raw);
    let mut interrupted = false;
    let execution = (|| {
        loop {
            if let Some(status) = process.try_wait()? {
                return Ok(status);
            }
            if cancelled.load(Ordering::Relaxed) || Instant::now() >= deadline {
                interrupted = true;
                if let Some(pid) = pid {
                    let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::TERM);
                }
                let grace = Instant::now() + Duration::from_secs(45);
                while Instant::now() < grace && process.try_wait()?.is_none() {
                    std::thread::sleep(Duration::from_millis(100));
                }
                if let Some(pid) = pid {
                    let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
                }
                return Ok(process.wait()?);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    })();
    // Descendants must not keep pipes or mutate evidence after the worker exits.
    if let Some(pid) = pid {
        let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
    }
    if execution.is_err() {
        let _ = process.kill();
        let _ = process.wait();
    }
    (execution, interrupted)
}

fn collect_logs(root: &Path, model: &RemoteModel, readers: [Option<LogReceiver>; 2]) -> Result<()> {
    let mut log = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(root.join("worker.log"))?;
    for reader in readers {
        let bytes = reader
            .ok_or_else(|| Error::Runtime("worker log pipe unavailable".into()))?
            .recv_timeout(Duration::from_secs(2))
            .map_err(|_| {
                Error::Runtime("worker log pipe did not close after process cleanup".into())
            })??;
        log.write_all(
            crate::redaction::text(&model.redact(&String::from_utf8_lossy(&bytes))).as_bytes(),
        )?;
    }
    Ok(())
}

fn read_response(
    request: &Request,
    status: std::process::ExitStatus,
    model: &RemoteModel,
) -> Result<Value> {
    let response = read_json(&request.output, 32 * 1024 * 1024)
        .map_err(|_| Error::Runtime("worker returned no valid bounded response".into()))?;
    if text(&response, "schema_version") != PROTOCOL || text(&response, "request_id") != request.id
    {
        return Err(Error::Runtime(
            "worker protocol/version/request identity mismatch".into(),
        ));
    }
    if text(&response["resources"], "compose_project") != request.project {
        return Err(Error::Runtime(
            "worker resource ownership response mismatch".into(),
        ));
    }
    if !status.success() || response.get("error").is_some_and(|e| !e.is_null()) {
        return Err(Error::Runtime(format!(
            "worker {}: {}",
            text(&response["error"], "stage"),
            model.redact(text(&response["error"], "message"))
        )));
    }
    response
        .get("result")
        .filter(|r| r.is_object())
        .cloned()
        .ok_or_else(|| Error::Runtime("worker result is not an object".into()))
}

fn log_reader(mut input: impl Read + Send + 'static) -> LogReceiver {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = (|| {
            let mut bytes = Vec::new();
            let mut buffer = [0; 8192];
            let mut exceeded = false;
            loop {
                let length = input.read(&mut buffer)?;
                if length == 0 {
                    break;
                }
                if bytes.len() + length <= 8 * 1024 * 1024 {
                    bytes.extend_from_slice(&buffer[..length]);
                } else {
                    exceeded = true;
                }
            }
            if exceeded {
                Err(std::io::Error::other("worker log exceeds 8 MiB"))
            } else {
                Ok(bytes)
            }
        })();
        let _ = sender.send(result);
    });
    receiver
}

fn docker_until(arguments: &[String], deadline: Instant) -> Option<String> {
    let budget = deadline
        .checked_duration_since(Instant::now())?
        .min(Duration::from_secs(20));
    let mut process = Command::new("docker")
        .args(["--host", "unix:///var/run/docker.sock"])
        .args(arguments)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let output = log_reader(process.stdout.take()?);
    let started = Instant::now();
    loop {
        match process.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                return String::from_utf8(output.recv_timeout(Duration::from_secs(1)).ok()?.ok()?)
                    .ok();
            }
            Err(_) => {
                let _ = process.kill();
                let _ = process.wait();
                return None;
            }
            Ok(None) => {}
        }
        if started.elapsed() > budget {
            let _ = process.kill();
            let _ = process.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn cleanup_owned(project: &str, request: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(45);
    let docker = |arguments: &[String]| docker_until(arguments, deadline);
    let mut complete = true;
    for resource in ["container", "network", "volume"] {
        let args = vec![
            resource.into(),
            "ls".into(),
            if resource == "container" {
                "-aq".into()
            } else {
                "-q".into()
            },
            "--filter".into(),
            format!("label=com.docker.compose.project={project}"),
        ];
        let Some(output) = docker(&args) else {
            complete = false;
            continue;
        };
        for id in output.split_whitespace() {
            if id.len() > 255
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
                || !id.starts_with(|c: char| c.is_ascii_alphanumeric())
            {
                complete = false;
                continue;
            }
            let labels = if resource == "container" {
                "{{json .Config.Labels}}"
            } else {
                "{{json .Labels}}"
            };
            let inspected = docker(&[
                resource.into(),
                "inspect".into(),
                "--format".into(),
                labels.into(),
                id.into(),
            ])
            .and_then(|s| serde_json::from_str::<Value>(&s).ok());
            if inspected.as_ref().is_none_or(|v| {
                text(v, "com.docker.compose.project") != project
                    || text(v, "io.anolisa.skill-score.request") != request
            }) {
                complete = false;
                continue;
            }
            let mut remove = vec![resource.into(), "rm".into()];
            if resource == "container" {
                remove.push("-f".into());
            }
            remove.push(id.into());
            if docker(&remove).is_none() {
                complete = false;
            }
        }
        if docker(&args).is_none_or(|s| !s.trim().is_empty()) {
            complete = false;
        }
    }
    complete
}

fn reject_unreclaimed(root: &Path, request: &Request) -> Result<Value> {
    // Keep recovery coordinates outside the exportable evidence tree, even when disposal fails.
    let recovery = tempfile::Builder::new()
        .prefix("asc-score-recovery-")
        .tempdir_in("/tmp")
        .map(tempfile::TempDir::keep);
    let recorded = recovery.and_then(|directory| {
        let path = directory.join("resources.json");
        private_json(
            &path,
            &json!({"request_id":request.id,"compose_project":request.project,
            "cleanup_status":"pending","evidence_available":false}),
        )
        .map_err(|_| std::io::Error::other("cannot write recovery coordinates"))?;
        Ok(path)
    });
    remove_request_tree(root).map_err(|_| Error::UnsafeEvidence {
        request_id: request.id.clone(),
        compose_project: request.project.clone(),
    })?;
    let record = recorded.map_or_else(
        |_| "record unavailable".to_owned(),
        |path| path.display().to_string(),
    );
    Err(Error::Runtime(format!(
        "cleanup unconfirmed; raw evidence discarded; request={}, compose_project={}, private recovery={record}",
        request.id, request.project
    )))
}

fn sanitize_or_discard(root: &Path, model: &RemoteModel) -> Result<()> {
    sanitize_with_budget(root, model, EvidenceBudget::default())
}

fn sanitize_with_budget(root: &Path, model: &RemoteModel, budget: EvidenceBudget) -> Result<()> {
    if validate_evidence_tree(root, model, budget).is_err() {
        remove_request_tree(root).map_err(|_| Error::UnsafeEvidence {
            request_id: String::new(),
            compose_project: String::new(),
        })?;
        return Err(Error::Runtime(
            "unsafe or oversized evidence rejected; raw request directory removed".into(),
        ));
    }
    Ok(())
}

fn remove_request_tree(root: &Path) -> Result<()> {
    use rustix::fs::{Mode, OFlags, openat};
    let parent = root
        .parent()
        .ok_or_else(|| Error::Runtime("request directory lacks parent".into()))?;
    let name = root
        .file_name()
        .ok_or_else(|| Error::Runtime("request directory lacks name".into()))?;
    let parent = File::from(openat(
        rustix::fs::CWD,
        parent,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?);
    remove_owned_entry(&parent, name, 0)
}

fn remove_owned_entry(parent: &File, name: &std::ffi::OsStr, depth: usize) -> Result<()> {
    use rustix::fs::{AtFlags, Mode, OFlags, fchmod, openat, unlinkat};
    if depth > 128 {
        return Err(Error::Runtime(
            "rejected evidence exceeds safe removal depth".into(),
        ));
    }
    // Delete non-directories first: this unlinks symlinks and hard links without touching their targets.
    match unlinkat(parent, name, AtFlags::empty()) {
        Ok(()) | Err(rustix::io::Errno::NOENT) => return Ok(()),
        Err(rustix::io::Errno::ISDIR | rustix::io::Errno::PERM) => {}
        Err(error) => return Err(error.into()),
    }
    let directory = File::from(openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?);
    fchmod(&directory, Mode::from_raw_mode(0o700))?;
    let descriptor = std::os::fd::AsRawFd::as_raw_fd(&directory);
    for entry in std::fs::read_dir(format!("/proc/self/fd/{descriptor}"))? {
        remove_owned_entry(&directory, &entry?.file_name(), depth + 1)?;
    }
    unlinkat(parent, name, AtFlags::REMOVEDIR)?;
    Ok(())
}

fn validate_evidence_tree(
    root: &Path,
    model: &RemoteModel,
    mut budget: EvidenceBudget,
) -> Result<()> {
    use rustix::fs::{Mode, OFlags, openat};
    let directory = File::from(openat(
        rustix::fs::CWD,
        root,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?);
    let mut manifest = JsonOutput::new(budget.output_limit());
    manifest.write_all(MANIFEST_PREFIX)?;
    collect_evidence(&directory, Path::new(""), model, &mut budget, &mut manifest)?;
    manifest.write_all(b"]}")?;
    budget.account_output(&manifest.bytes, model)?;
    let descriptor = std::os::fd::AsRawFd::as_raw_fd(&directory);
    private_write(
        &PathBuf::from(format!("/proc/self/fd/{descriptor}")).join("collection-manifest.json"),
        |file| Ok(file.write_all(&manifest.bytes)?),
    )
}

const MANIFEST_PREFIX: &[u8] = br#"{"files":["#;
const MAX_EVIDENCE_FILE: usize = 64 * 1024 * 1024;
const MAX_EVIDENCE_REQUEST: usize = 256 * 1024 * 1024;

struct EvidenceBudget {
    bytes: u64,
    entries: usize,
    json: JsonBudget,
    output_file: usize,
    output_remaining: usize,
}

impl Default for EvidenceBudget {
    fn default() -> Self {
        Self {
            bytes: 0,
            entries: 0,
            json: JsonBudget::default(),
            output_file: MAX_EVIDENCE_FILE,
            output_remaining: MAX_EVIDENCE_REQUEST,
        }
    }
}

impl EvidenceBudget {
    fn output_limit(&self) -> usize {
        self.output_file.min(self.output_remaining)
    }

    fn account_output(&mut self, bytes: &[u8], model: &RemoteModel) -> Result<()> {
        if bytes.len() > self.output_limit() {
            return Err(Error::Runtime("evidence exceeds output size limit".into()));
        }
        if model.contains_secret(bytes) {
            return Err(Error::Runtime(
                "serialized evidence contains a resolved credential".into(),
            ));
        }
        self.output_remaining -= bytes.len();
        Ok(())
    }
}

fn redact_evidence(
    path: &Path,
    mut bytes: Vec<u8>,
    model: &RemoteModel,
    budget: &mut EvidenceBudget,
) -> Result<Vec<u8>> {
    let lines = path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("jsonl") || ext.eq_ignore_ascii_case("ndjson"));
    // Harbor invokes Claude with --output-format=stream-json and redirects stderr into this file.
    let claude_stream = path
        .file_name()
        .is_some_and(|name| name == "claude-code.txt");
    let Ok(content) = std::str::from_utf8(&bytes) else {
        if lines || claude_stream {
            return Err(Error::Runtime(
                "structured evidence stream is not UTF-8".into(),
            ));
        }
        model.redact_bytes(&mut bytes);
        return Ok(bytes);
    };
    let mut output = JsonOutput::new(budget.output_limit());
    if !lines
        && !claude_stream
        && let Some(mut value) = budget.json.parse(content)?
    {
        crate::redaction::redact_with_budget(&mut value, model, &mut budget.json)?;
        output.append(&value)?;
    } else {
        let write_text = |out: &mut JsonOutput, text: &str| -> Result<()> {
            if !text.is_empty() {
                out.write_all(
                    crate::redaction::text(&model.redact_without_growth(text)).as_bytes(),
                )?;
            }
            Ok(())
        };
        let mut offset = 0;
        let mut text_start = 0;
        for line in content.split_inclusive('\n') {
            let start = offset;
            offset += line.len();
            let trimmed = line.trim();
            if trimmed.is_empty() || (!lines && !trimmed.starts_with(['{', '[', '"'])) {
                continue;
            }
            let Some(mut record) = budget.json.parse(trimmed)? else {
                if lines {
                    return Err(Error::Runtime("invalid JSON Lines record".into()));
                }
                continue;
            };
            // Keep contiguous diagnostics together so multiline private keys are masked as a unit.
            write_text(&mut output, &content[text_start..start])?;
            crate::redaction::redact_with_budget(&mut record, model, &mut budget.json)?;
            output.append(&record)?;
            if line.ends_with('\n') {
                output.write_all(b"\n")?;
            }
            text_start = offset;
        }
        write_text(&mut output, &content[text_start..])?;
    }
    Ok(output.bytes)
}

fn collect_evidence(
    directory: &File,
    relative: &Path,
    model: &RemoteModel,
    budget: &mut EvidenceBudget,
    manifest: &mut JsonOutput,
) -> Result<()> {
    use rustix::fs::{Mode, OFlags, openat};
    use std::os::unix::{ffi::OsStrExt as _, fs::MetadataExt as _};
    let descriptor = std::os::fd::AsRawFd::as_raw_fd(directory);
    let entries = std::fs::read_dir(format!("/proc/self/fd/{descriptor}"))?;
    for entry in entries {
        let name = entry?.file_name();
        let path = relative.join(&name);
        if model.contains_secret(path.as_os_str().as_bytes()) {
            return Err(Error::Runtime(
                "evidence filename contains a resolved credential".into(),
            ));
        }
        budget.entries += 1;
        if budget.entries > 20_000 || path.components().count() > 64 {
            return Err(Error::Runtime("evidence exceeds entry/depth limit".into()));
        }
        let opened = openat(
            directory,
            &name,
            OFlags::RDWR | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        );
        let mut file = match opened {
            Ok(file) => File::from(file),
            Err(rustix::io::Errno::ISDIR) => {
                let child = File::from(openat(
                    directory,
                    &name,
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )?);
                collect_evidence(&child, &path, model, budget, manifest)?;
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        let bytes = read_evidence_file(&mut file, budget)?;
        let raw_digest = Sha256::digest(&bytes);
        let redacted = redact_evidence(&path, bytes, model, budget)?;
        budget.account_output(&redacted, model)?;
        if Sha256::digest(&redacted) != raw_digest {
            // Reuse the verified descriptor: parent swaps and links cannot redirect writes.
            file.rewind()?;
            file.write_all(&redacted)?;
            file.set_len(redacted.len() as u64)?;
            if file.metadata()?.nlink() != 1 {
                return Err(Error::Runtime(
                    "evidence acquired an external hard link".into(),
                ));
            }
        }
        if manifest.bytes.len() > MANIFEST_PREFIX.len() {
            manifest.write_all(b",")?;
        }
        manifest.append(&json!({"path":path,"collected_sha256":format!("{raw_digest:x}"),"producer":"worker/observed-container","modifiable_by":["evaluated-container-before-collection"],"authenticity":"untrusted observations require independent attribution"}))?;
    }
    Ok(())
}

fn read_evidence_file(file: &mut File, budget: &mut EvidenceBudget) -> Result<Vec<u8>> {
    use std::os::unix::fs::MetadataExt as _;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err(Error::Runtime(
            "evidence contains special files or hard links".into(),
        ));
    }
    budget.bytes += metadata.len();
    if metadata.len() > MAX_EVIDENCE_FILE as u64 || budget.bytes > MAX_EVIDENCE_REQUEST as u64 {
        return Err(Error::Runtime("evidence exceeds size limits".into()));
    }
    let mut bytes = Vec::new();
    std::io::Read::by_ref(file)
        .take(MAX_EVIDENCE_FILE as u64 + 1)
        .read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    if bytes.len() as u64 != metadata.len()
        || after.len() != metadata.len()
        || after.nlink() != 1
        || after.mtime() != metadata.mtime()
        || after.mtime_nsec() != metadata.mtime_nsec()
        || after.ctime() != metadata.ctime()
        || after.ctime_nsec() != metadata.ctime_nsec()
    {
        return Err(Error::Runtime("evidence changed during collection".into()));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn model() -> Result<RemoteModel> {
        model_with_key("worker-test-key-value")
    }

    fn model_with_key(key: &str) -> Result<RemoteModel> {
        Ok(RemoteModel::resolve(
            Path::new("/nonexistent/score-worker-test"),
            &BTreeMap::from([
                ("ANTHROPIC_API_KEY".into(), key.into()),
                (
                    "ANTHROPIC_BASE_URL".into(),
                    "https://dashscope.aliyuncs.com/apps/anthropic".into(),
                ),
            ]),
        )?)
    }

    #[test]
    fn evidence_symlinks_and_hardlinks_never_redirect_redaction() -> Result<()> {
        let root = tempfile::tempdir()?;
        let external = tempfile::tempdir()?;
        let target = external.path().join("private");
        std::fs::write(&target, "worker-test-key-value")?;
        std::os::unix::fs::symlink(external.path(), root.path().join("subdir"))?;
        assert!(validate_evidence_tree(root.path(), &model()?, EvidenceBudget::default()).is_err());
        assert_eq!(std::fs::read_to_string(&target)?, "worker-test-key-value");
        std::fs::remove_file(root.path().join("subdir"))?;
        std::fs::hard_link(&target, root.path().join("linked"))?;
        assert!(validate_evidence_tree(root.path(), &model()?, EvidenceBudget::default()).is_err());
        assert_eq!(std::fs::read_to_string(&target)?, "worker-test-key-value");
        Ok(())
    }

    #[test]
    fn collection_preserves_raw_digest_and_redacts_structured_secrets() -> Result<()> {
        let root = tempfile::tempdir()?;
        std::fs::create_dir(root.path().join("nested"))?;
        let raw =
            br#"{"authorization":"Bearer arbitrary-secret","message":"worker-test-key-value"}"#;
        std::fs::write(root.path().join("nested/event.json"), raw)?;
        validate_evidence_tree(root.path(), &model()?, EvidenceBudget::default())?;
        let stored = std::fs::read_to_string(root.path().join("nested/event.json"))?;
        assert!(!stored.contains("arbitrary-secret"));
        assert!(!stored.contains("worker-test-key-value"));
        let manifest = read_json(&root.path().join("collection-manifest.json"), 65_536)?;
        assert_eq!(
            manifest["files"][0]["collected_sha256"],
            format!("{:x}", Sha256::digest(raw))
        );
        assert!(text(&manifest["files"][0], "authenticity").contains("untrusted"));
        Ok(())
    }
    #[test]
    fn exported_json_cannot_reconstitute_credentials_from_escaped_keys_or_values() -> Result<()> {
        let root = tempfile::tempdir()?;
        let path = root.path().join("escaped.json");
        let inner = r#"{"worker-test-\u006bey-value":"drop","message":"worker-test-\u006bey-value","safe":1}"#;
        let raw = format!(
            r#"{{"direct":{inner},"encoded":{}}}"#,
            serde_json::to_string(inner)?
        );
        let model = model()?;
        assert!(!model.contains_secret(raw.as_bytes()));
        std::fs::write(&path, raw)?;
        sanitize_or_discard(root.path(), &model)?;
        let exported = std::fs::read(&path)?;
        assert!(!model.contains_secret(&exported));
        let value: Value = serde_json::from_slice(&exported)?;
        let nested: Value = serde_json::from_str(value["encoded"].as_str().expect("nested JSON"))?;
        assert_eq!(value["direct"], json!({"message":"[REDACTED]","safe":1}));
        assert_eq!(nested, value["direct"]);
        Ok(())
    }

    #[test]
    fn json_lines_exports_preserve_records_and_redact_each_decoded_value() -> Result<()> {
        let root = tempfile::tempdir()?;
        let raw = r#"{"worker-test-\u006bey-value":"drop","keep":1}

{"nested":"{\"message\":\"worker-test-\\u006bey-value\"}"}"#;
        let model = model()?;
        for extension in ["jsonl", "ndjson"] {
            std::fs::write(root.path().join(format!("events.{extension}")), raw)?;
        }
        sanitize_or_discard(root.path(), &model)?;
        for extension in ["jsonl", "ndjson"] {
            let exported =
                std::fs::read_to_string(root.path().join(format!("events.{extension}")))?;
            let lines = exported.split('\n').collect::<Vec<_>>();
            assert_eq!(lines.len(), 3);
            assert_eq!(serde_json::from_str::<Value>(lines[0])?, json!({"keep":1}));
            assert_eq!(lines[1], "");
            let last: Value = serde_json::from_str(lines[2])?;
            let nested: Value = serde_json::from_str(last["nested"].as_str().expect("JSON text"))?;
            assert_eq!(nested, json!({"message":"[REDACTED]"}));
            assert!(!model.contains_secret(exported.as_bytes()));
        }
        Ok(())
    }

    #[test]
    fn json_lines_validate_original_syntax_before_masking_short_keys() -> Result<()> {
        let model = model_with_key("x\"y")?;
        let mut budget = EvidenceBudget::default();
        let path = Path::new("trace.jsonl");
        let good = br#"{"detail":"x\"yx\"y"}"#;
        let safe = redact_evidence(path, good.to_vec(), &model, &mut budget)?;
        assert_eq!(
            serde_json::from_slice::<Value>(&safe)?,
            json!({"detail":"**"})
        );
        let malformed = br#"{"detail":"x"y"}"#;
        assert!(redact_evidence(path, malformed.to_vec(), &model, &mut budget).is_err());
        Ok(())
    }

    #[test]
    fn mixed_claude_stream_keeps_diagnostics_and_redacts_each_json_record() -> Result<()> {
        let model = model()?;
        let raw = concat!(
            r#"{"type":"user","message":"worker-test-\u006bey-value"}"#,
            "\n",
            "diagnostic: retrying\n-----BEGIN PRIVATE KEY-----\nprivate-body\n-----END PRIVATE KEY-----\n",
            r#"{"type":"result","message":"worker-test-\u006bey-value"}"#,
            "\n",
        );
        for name in ["agent/claude-code.txt", "worker-output.log"] {
            let output = redact_evidence(
                Path::new(name),
                raw.as_bytes().to_vec(),
                &model,
                &mut EvidenceBudget::default(),
            )?;
            let text = std::str::from_utf8(&output).expect("UTF-8 log");
            assert!(text.contains("diagnostic: retrying") && !text.contains("private-body"));
            let records = text
                .lines()
                .filter(|line| line.starts_with('{'))
                .map(serde_json::from_str::<Value>)
                .collect::<std::result::Result<Vec<_>, _>>()?;
            assert_eq!(records.len(), 2);
            assert_eq!(records[0]["type"], "user");
            assert_eq!(records[1]["type"], "result");
            assert!(
                records
                    .iter()
                    .all(|record| record["message"] == "[REDACTED]")
            );
        }
        Ok(())
    }

    #[test]
    fn invalid_lines_and_oversized_top_or_nested_json_discard_the_request() -> Result<()> {
        let parent = tempfile::tempdir()?;
        let model = model()?;
        for (name, file, raw, units) in [
            ("wide", "event.json", "[0,0,0,0,0,0]", 6),
            ("nested", "event.json", r#"{"body":"[0,0,0,0]"}"#, 6),
            ("cumulative", "events.jsonl", "[0,0]\n[0,0]\n", 5),
            ("broken", "events.ndjson", "{}\nnot JSON\n", 100),
        ] {
            let root = parent.path().join(name);
            std::fs::create_dir(&root)?;
            std::fs::write(root.join(file), raw)?;
            std::fs::write(root.join("sibling.bin"), [0xff])?;
            let budget = EvidenceBudget {
                json: JsonBudget(units),
                ..EvidenceBudget::default()
            };
            assert!(
                sanitize_with_budget(&root, &model, budget).is_err(),
                "{name}"
            );
            assert!(!root.exists(), "{name}");
        }
        Ok(())
    }

    #[test]
    fn compact_export_caps_each_file_and_the_request_including_its_manifest() -> Result<()> {
        let model = model()?;
        let raw = format!(
            "{}{}{}",
            "[".repeat(32),
            "0,".repeat(64) + "1",
            "]".repeat(32)
        );
        let mut budget = EvidenceBudget {
            output_file: raw.len(),
            ..EvidenceBudget::default()
        };
        let path = Path::new("nested.json");
        assert_eq!(
            redact_evidence(path, raw.as_bytes().to_vec(), &model, &mut budget)?,
            raw.as_bytes()
        );
        budget.output_file -= 1;
        assert!(redact_evidence(path, raw.as_bytes().to_vec(), &model, &mut budget).is_err());

        let parent = tempfile::tempdir()?;
        let root = parent.path().join("request");
        std::fs::create_dir(&root)?;
        std::fs::write(root.join(path), &raw)?;
        sanitize_or_discard(&root, &model)?;
        let exact = raw.len() + std::fs::read(root.join("collection-manifest.json"))?.len();
        std::fs::remove_dir_all(&root)?;
        for limit in [exact - 1, exact] {
            std::fs::create_dir(&root)?;
            std::fs::write(root.join(path), &raw)?;
            let budget = EvidenceBudget {
                output_remaining: limit,
                ..EvidenceBudget::default()
            };
            assert_eq!(
                sanitize_with_budget(&root, &model, budget).is_ok(),
                limit == exact
            );
            assert_eq!(root.exists(), limit == exact);
        }
        Ok(())
    }

    #[test]
    fn unredactable_final_bytes_discard_the_whole_request() -> Result<()> {
        let parent = tempfile::tempdir()?;
        let root = parent.path().join("request");
        std::fs::create_dir(&root)?;
        std::fs::write(root.join("raw.json"), br#"{"authorization":"opaque"}"#)?;
        std::fs::write(root.join("other"), "sibling evidence")?;
        // This synthetic credential coincides with the mask introduced during serialization.
        let model = model_with_key("[REDACTED]")?;
        assert!(matches!(
            sanitize_or_discard(&root, &model),
            Err(Error::Runtime(_))
        ));
        assert!(!root.exists());
        assert!(parent.path().is_dir());
        Ok(())
    }

    #[test]
    fn binary_evidence_larger_than_python_limit_has_exact_credentials_removed() -> Result<()> {
        let root = tempfile::tempdir()?;
        let model = model()?;
        let mut bytes = vec![0xff; 17 * 1024 * 1024];
        let secret = b"worker-test-key-value";
        bytes[16 * 1024 * 1024..16 * 1024 * 1024 + secret.len()].copy_from_slice(secret);
        std::fs::write(root.path().join("binary.dat"), &bytes)?;
        sanitize_or_discard(root.path(), &model)?;
        let stored = std::fs::read(root.path().join("binary.dat"))?;
        assert_eq!(stored.len(), bytes.len());
        assert!(!model.contains_secret(&stored));
        let manifest = read_json(&root.path().join("collection-manifest.json"), 65_536)?;
        assert_eq!(
            manifest["files"][0]["collected_sha256"],
            format!("{:x}", Sha256::digest(&bytes))
        );
        Ok(())
    }

    #[test]
    fn credential_names_and_oversized_or_linked_evidence_remove_only_request_root() -> Result<()> {
        let parent = tempfile::tempdir()?;
        let external = parent.path().join("external");
        std::fs::write(&external, "worker-test-key-value")?;
        for name in ["credential", "oversize", "linked", "hardlinked"] {
            let root = parent.path().join(name);
            std::fs::create_dir(&root)?;
            match name {
                "credential" => std::fs::write(root.join("worker-test-key-value"), "data")?,
                "oversize" => {
                    let file = File::create(root.join("too-large"))?;
                    file.set_len(64 * 1024 * 1024 + 1)?;
                }
                "linked" => std::os::unix::fs::symlink(&external, root.join("link"))?,
                _ => std::fs::hard_link(&external, root.join("link"))?,
            }
            let error = sanitize_or_discard(&root, &model()?)
                .err()
                .ok_or_else(|| Error::Runtime("unsafe evidence was accepted".into()))?;
            assert!(!error.to_string().contains("worker-test-key-value"));
            assert!(!root.exists());
            assert_eq!(std::fs::read_to_string(&external)?, "worker-test-key-value");
        }
        Ok(())
    }

    #[test]
    fn later_cleanup_success_does_not_hide_an_earlier_pending_request() -> Result<()> {
        let root = tempfile::tempdir()?;
        let mut runtime = Runtime {
            lease: Lease::fixture(root.path())?,
            python: PathBuf::new(),
            entry: PathBuf::new(),
            image_ids: json!({}),
            cleanup_status: "complete".into(),
            resources: vec![
                json!({"request_id":"first"}),
                json!({"request_id":"second"}),
            ],
        };
        runtime.record_cleanup("first", false);
        runtime.record_cleanup("second", true);
        assert_eq!(runtime.cleanup_status, "pending");
        assert_eq!(runtime.resources[0]["cleanup_status"], "pending");
        assert_eq!(runtime.resources[1]["cleanup_status"], "complete");
        Ok(())
    }
    #[test]
    fn unconfirmed_cleanup_discards_raw_evidence_and_retains_private_recovery() -> Result<()> {
        let parent = tempfile::tempdir()?;
        let root = parent.path().join("request");
        std::fs::create_dir(&root)?;
        std::fs::write(root.join("raw"), "worker-test-key-value")?;
        let request = Request {
            id: "pending-test".into(),
            project: "asc-score-pending-test__env".into(),
            input: root.join("input"),
            output: root.join("output"),
        };
        let error = reject_unreclaimed(&root, &request)
            .err()
            .ok_or_else(|| Error::Runtime("unconfirmed evidence accepted".into()))?;
        assert!(!root.exists());
        let message = error.to_string();
        assert!(!message.contains("worker-test-key-value"));
        let record = message
            .split("private recovery=")
            .nth(1)
            .ok_or_else(|| Error::Runtime("recovery path missing".into()))?;
        let path = Path::new(record);
        assert!(!path.starts_with(parent.path()));
        let recovery = read_json(path, 65_536)?;
        assert_eq!(recovery["cleanup_status"], "pending");
        assert_eq!(recovery["evidence_available"], false);
        assert_eq!(recovery["request_id"], "pending-test");
        std::fs::remove_dir_all(
            path.parent()
                .ok_or_else(|| Error::Runtime("recovery parent missing".into()))?,
        )?;
        Ok(())
    }
    fn image_manifest() -> Value {
        json!({"images":[
            {"role":"work","name":"unused-work-tag","id":format!("sha256:{}", "a".repeat(64)),"manifest_id":format!("sha256:{}", "b".repeat(64)),"platform":"linux/amd64"},
            {"role":"egress","name":"unused-egress-tag","id":format!("sha256:{}", "c".repeat(64)),"manifest_id":format!("sha256:{}", "d".repeat(64)),"platform":"linux/amd64"}
        ]})
    }

    fn inspected_image(id: &str) -> String {
        json!([{"Id":id,"Os":"linux","Architecture":"amd64"}]).to_string()
    }

    #[test]
    fn classic_and_containerd_stores_select_only_prebound_immutable_ids() -> Result<()> {
        let manifest = image_manifest();
        for field in ["id", "manifest_id"] {
            let mut queried = Vec::new();
            let selected = resolve_image_ids(&manifest, |candidate| {
                queried.push(candidate.to_owned());
                array(&manifest, "images")
                    .iter()
                    .find(|image| text(image, field) == candidate)
                    .map(|_| inspected_image(candidate))
            })?;
            for image in array(&manifest, "images") {
                assert_eq!(selected[text(image, "role")], image[field]);
            }
            assert!(queried.iter().all(|id| immutable_image_id(id)));
            assert_eq!(queried.len(), if field == "id" { 2 } else { 4 });
        }
        Ok(())
    }

    #[test]
    fn config_lookup_may_resolve_to_its_verified_manifest_id() -> Result<()> {
        let manifest = image_manifest();
        let selected = resolve_image_ids(&manifest, |candidate| {
            array(&manifest, "images")
                .iter()
                .find(|image| text(image, "id") == candidate)
                .map(|image| inspected_image(text(image, "manifest_id")))
        })?;
        assert_eq!(selected["work"], manifest["images"][0]["manifest_id"]);
        assert_eq!(selected["egress"], manifest["images"][1]["manifest_id"]);
        Ok(())
    }

    #[test]
    fn malformed_identity_pairs_fail_before_any_docker_inspection() {
        let original = image_manifest();
        let mut malformed = Vec::new();
        for (field, value) in [
            ("id", "agent-sec:latest"),
            ("manifest_id", "sha256:ABC"),
            ("manifest_id", ""),
            ("platform", "linux/arm64"),
            ("role", "egress"),
        ] {
            let mut manifest = original.clone();
            manifest["images"][0][field] = value.into();
            malformed.push(manifest);
        }
        let mut extra = original.clone();
        extra["images"] = json!([
            original["images"][0],
            original["images"][1],
            original["images"][0]
        ]);
        malformed.push(extra);
        for manifest in malformed {
            let mut calls = 0;
            assert!(
                resolve_image_ids(&manifest, |_| {
                    calls += 1;
                    None
                })
                .is_err()
            );
            assert_eq!(calls, 0);
        }
    }

    #[test]
    fn inspection_rejects_unknown_cross_role_wrong_platform_and_malformed_results() {
        let manifest = image_manifest();
        for output in [
            inspected_image(&format!("sha256:{}", "e".repeat(64))),
            inspected_image(text(&manifest["images"][1], "id")),
            json!([{"Id":manifest["images"][0]["id"],"Os":"linux","Architecture":"arm64"}])
                .to_string(),
            "[]".into(),
            "not JSON; secret-fixture-value".into(),
        ] {
            let mut calls = 0;
            let error = resolve_image_ids(&manifest, |_| {
                calls += 1;
                Some(output.clone())
            });
            assert!(error.is_err());
            assert_eq!(calls, 1);
            assert!(
                !error
                    .err()
                    .map_or_else(String::new, |error| error.to_string())
                    .contains("secret-fixture-value")
            );
        }
        let mut calls = 0;
        assert!(
            resolve_image_ids(&manifest, |_| {
                calls += 1;
                None
            })
            .is_err()
        );
        assert_eq!(calls, 2);
    }

    #[test]
    fn worker_request_uses_resolved_ids_without_changing_manifest_bindings() -> Result<()> {
        let manifest = image_manifest();
        let original = manifest.clone();
        let image_ids = resolve_image_ids(&manifest, |candidate| {
            array(&manifest, "images")
                .iter()
                .find(|image| text(image, "manifest_id") == candidate)
                .map(|_| inspected_image(candidate))
        })?;
        let root = tempfile::tempdir()?;
        let mut runtime = Runtime {
            lease: Lease::fixture(root.path())?,
            python: PathBuf::new(),
            entry: PathBuf::new(),
            image_ids: image_ids.clone(),
            cleanup_status: "complete".into(),
            resources: Vec::new(),
        };
        let model = model()?;
        let cancelled = AtomicBool::new(false);
        let request = runtime.prepare_request(&Invocation {
            operation: "environment",
            snapshot: root.path(),
            probe: None,
            root: root.path(),
            model: &model,
            deadline: Instant::now() + Duration::from_secs(30),
            cancelled: &cancelled,
        })?;
        let body = read_json(&request.input, 65_536)?;
        assert_eq!(body["image_ids"], image_ids);
        assert_eq!(body["config"]["docker_image"], image_ids["work"]);
        assert_eq!(manifest, original);
        Ok(())
    }

    #[test]
    fn expired_image_inspection_budget_never_starts_docker() -> Result<()> {
        let deadline = Instant::now()
            .checked_sub(Duration::from_secs(1))
            .ok_or(Error::Deadline)?;
        assert!(
            docker_until(
                &["image".into(), "inspect".into(), "not-an-image".into()],
                deadline
            )
            .is_none()
        );
        Ok(())
    }
}
