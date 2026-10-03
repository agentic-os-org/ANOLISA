//! Bounded metadata chains using the existing durable journal storage contract.

use aw_core::{journal::FileJournal, ports::Journal};
use aw_host::{CallRecord, Failure, FailureAction, Invocation, Method, StepOutput};
use serde_json::{json, Value};
use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
};

use crate::{rejected, Result};

pub(crate) struct Audit {
    journal: Mutex<FileJournal>,
    directory: PathBuf,
    failed: AtomicBool,
}

impl Audit {
    pub(crate) fn new(directory: &Path) -> Result<Self> {
        if let Ok(metadata) = fs::symlink_metadata(directory) {
            // SAFETY: geteuid has no pointer arguments or preconditions.
            if !metadata.is_dir()
                || metadata.uid() != unsafe { libc::geteuid() }
                || metadata.mode() & 0o7777 != 0o700
            {
                return Err(rejected("unsafe_audit_directory"));
            }
        }
        let journal = FileJournal::new(directory).map_err(|_| rejected("audit_open"))?;
        Ok(Self {
            journal: Mutex::new(journal),
            directory: directory.into(),
            failed: AtomicBool::new(false),
        })
    }

    pub(crate) fn healthy(&self) -> bool {
        !self.failed.load(Ordering::Acquire)
    }

    fn mutate(
        &self,
        action: impl FnOnce(
            &mut FileJournal,
        ) -> std::result::Result<Value, aw_core::ports::JournalError>,
    ) -> Result<Value> {
        let mut journal = self
            .journal
            .lock()
            .map_err(|_| rejected("audit_unavailable"))?;
        if !self.healthy() {
            return Err(rejected("audit_unavailable"));
        }
        action(&mut journal).map_err(|_| {
            self.failed.store(true, Ordering::Release);
            rejected("audit_unavailable")
        })
    }

    pub(crate) fn claim(&self, key: &str, record: Value) -> Result<Value> {
        self.mutate(|journal| journal.claim(key, &record))
    }

    pub(crate) fn append(&self, key: &str, record: Value) -> Result<Value> {
        self.mutate(|journal| journal.append(key, &record))
    }

    pub(crate) fn release(&self, key: &str) {
        if let Ok(mut journal) = self.journal.lock() {
            journal.release(key);
        }
    }

    pub(crate) fn read(&self, key: &str) -> Result<Value> {
        if key.len() != 64
            || !key
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(rejected("invalid_audit_key"));
        }
        let metadata = fs::symlink_metadata(self.directory.join(format!("{key}.jsonl")))?;
        if !metadata.is_file() || metadata.len() > 1024 * 1024 {
            return Err(rejected("audit_query_limit"));
        }
        let records = self
            .journal
            .lock()
            .map_err(|_| rejected("audit_unavailable"))?
            .read(key)
            .map_err(|_| rejected("audit_invalid"))?;
        let terminal = records
            .last()
            .is_some_and(|record| record["record"]["phase"] == "closed");
        Ok(json!({"records": records, "terminal": terminal}))
    }
}

pub(crate) fn call(record: &CallRecord) -> Value {
    use std::os::unix::process::ExitStatusExt;
    json!({
        "request_id": record.request_id,
        "provider": record.provider,
        "method": match record.method { Method::Describe => "describe", Method::ValidateConfig => "validate_config", Method::Invoke => "invoke", Method::NativeHook => "native_hook" },
        "host_event_id": record.event_id,
        "step_id": record.step_id,
        "elapsed_ms": record.elapsed.as_millis().min(u128::from(u32::MAX)) as u32,
        "process": record.process.as_ref().map(|process| json!({
            "exit_code": process.status.code(), "signal": process.status.signal(),
            "stdin_bytes": process.input_bytes_written, "stdout_bytes": process.stdout_bytes,
            "stderr_bytes": process.stderr.len(),
        })),
    })
}

pub(crate) fn invocation(value: Invocation) -> (Value, Value) {
    let metadata = call(&value.record);
    let native = value.record.method == Method::NativeHook;
    let mut native_output = Value::Null;
    let (status, effects, failure) = match value.result {
        Ok(StepOutput::Provider(outcome)) => ("ok", outcome.as_value()["effects"].clone(), None),
        Ok(StepOutput::Native(output)) => {
            use std::os::unix::process::ExitStatusExt;
            native_output = json!({"stdout": output.stdout, "stderr": output.stderr,
                "exit_code": output.status.code(), "signal": output.status.signal()});
            ("ok", Value::Null, None)
        }
        Err(error) => (
            "error",
            json!([]),
            Some(match error {
                Failure::Transport(_) => "transport",
                Failure::Exit => "exit",
                Failure::IncompleteInput => "incomplete_input",
                Failure::Provider { .. } => "provider",
                Failure::Protocol(_) => "protocol",
            }),
        ),
    };
    let action = value.failure_action.map(|action| match action {
        FailureAction::Report => "report",
        FailureAction::Block => "block",
    });
    let blocked = effects
        .as_array()
        .is_some_and(|effects| effects.iter().any(|effect| effect["type"] == "block"));
    let kind = if native { "native" } else { "provider" };
    let record = json!({"kind": kind, "phase": "completed", "call": metadata, "status": status, "failure": failure,
        "failure_action": action, "policy_block": if native { None } else { Some(blocked) }});
    let mut result = json!({"kind": kind, "status": status, "failure": failure,
        "failure_action": action, "call": metadata});
    if native {
        result["native"] = native_output;
    } else {
        result["effects"] = effects;
    }
    (record, result)
}

pub(crate) fn preparation(error: &aw_host::Error) -> Value {
    match error {
        aw_host::Error::Preparation(value) => json!({
            "completed": value.completed.iter().map(call).collect::<Vec<_>>(),
            "failed": preparation(&value.cause),
        }),
        aw_host::Error::Call(value) => call(&value.record),
        _ => Value::Null,
    }
}
