//! Bounded metadata chains using the existing durable journal storage contract.

use aw_core::{journal::FileJournal, ports::Journal};
use aw_host::{CallRecord, Failure, FailureAction, Invocation, Method, StepOutput};
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
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
    recent: Mutex<VecDeque<Value>>,
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
            recent: Mutex::new(VecDeque::new()),
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
        self.mutate(|journal| {
            let written = journal.append(key, &record)?;
            if record["phase"] == "completed" {
                // Presentation is best effort and cannot turn a durable successful
                // write into an event failure. Keep the journal lock until publication
                // so concurrent completions retain durable order; readers clone 20 items.
                if let Ok(mut recent) = self.recent.lock() {
                    let call = &record["call"];
                    recent.push_front(json!({"provider":call["provider"],"step_id":call["step_id"],
                        "method":call["method"],"elapsed_ms":call["elapsed_ms"],
                        "status":record["status"],"failure":record["failure"],
                        "failure_action":record["failure_action"],"policy_block":record["policy_block"]}));
                    recent.truncate(20);
                }
            }
            Ok(written)
        })
    }

    pub(crate) fn recent(&self) -> Result<Value> {
        let recent = self
            .recent
            .lock()
            .map_err(|_| rejected("audit_unavailable"))?;
        Ok(json!({"scope":"service_generation","limit":20,"items":*recent}))
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
        // A well-formed key without a journal is an unknown handle, not a
        // service failure; every other stat error keeps propagating as one.
        let metadata = match fs::symlink_metadata(self.directory.join(format!("{key}.jsonl"))) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(rejected("unknown_audit_key"));
            }
            Err(error) => return Err(error.into()),
        };
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
                Failure::NativeEnvironment(_) => "native_environment",
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        thread,
        time::{Duration, Instant},
    };

    #[test]
    fn recent_publication_preserves_durable_order_and_latest_twenty() {
        let directory = tempfile::tempdir().unwrap();
        let audit = Audit::new(&directory.path().join("journal")).unwrap();
        let key = "a".repeat(64);
        audit.claim(&key, json!({"kind":"event"})).unwrap();
        let completion = |id| {
            json!({"phase":"completed","call":{"provider":"fixture",
            "step_id":format!("step-{id}"),"method":"invoke","elapsed_ms":1},"status":"ok"})
        };
        thread::scope(|scope| {
            let reader = audit.recent.lock().unwrap();
            let writer = scope.spawn(|| audit.append(&key, completion(0)).unwrap());
            let deadline = Instant::now() + Duration::from_secs(2);
            while !fs::read_to_string(audit.directory.join(format!("{key}.jsonl")))
                .unwrap()
                .contains("completed")
                && Instant::now() < deadline
            {
                thread::sleep(Duration::from_millis(1));
            }
            let written = fs::read_to_string(audit.directory.join(format!("{key}.jsonl")))
                .unwrap()
                .contains("completed");
            let deadline = Instant::now() + Duration::from_millis(100);
            let mut unlocked_before_publication = false;
            while Instant::now() < deadline {
                if audit.journal.try_lock().is_ok() {
                    unlocked_before_publication = true;
                    break;
                }
                thread::sleep(Duration::from_millis(1));
            }
            drop(reader);
            writer.join().unwrap();
            assert!(written);
            assert!(!unlocked_before_publication);
        });
        thread::scope(|scope| {
            for id in 1..33 {
                let audit = &audit;
                let key = &key;
                let completion = &completion;
                scope.spawn(move || audit.append(key, completion(id)).unwrap());
            }
        });
        let durable = audit.read(&key).unwrap();
        let expected: Vec<_> = durable["records"]
            .as_array()
            .unwrap()
            .iter()
            .rev()
            .take(20)
            .map(|record| record["record"]["call"]["step_id"].clone())
            .collect();
        let recent = audit.recent().unwrap();
        let actual: Vec<_> = recent["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["step_id"].clone())
            .collect();
        assert_eq!(actual, expected);
        assert!(audit.append(&"b".repeat(64), completion(99)).is_err());
        assert!(!audit.healthy());
        assert_eq!(audit.recent().unwrap(), recent);
    }
}
