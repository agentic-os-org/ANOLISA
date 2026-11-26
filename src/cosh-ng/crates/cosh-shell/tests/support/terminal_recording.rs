use std::fs::File;
use std::io::Read;
use std::time::Instant;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub(super) struct TerminalRecording {
    name: String,
    started: Instant,
    binary_sha256: String,
    shell: String,
    language: String,
    rows: u16,
    cols: u16,
    events: Vec<Value>,
    bytes: usize,
    process: Option<Value>,
    adapter: String,
    provider_fixture_sha256: Option<String>,
}

impl TerminalRecording {
    pub(super) fn new(name: &str, shell: &str, language: &str, rows: u16, cols: u16) -> Self {
        let mut binary = File::open(env!("CARGO_BIN_EXE_cosh-shell")).unwrap();
        let mut digest = Sha256::new();
        let mut buffer = [0; 8192];
        loop {
            let count = binary.read(&mut buffer).unwrap();
            if count == 0 {
                break;
            }
            digest.update(&buffer[..count]);
        }
        Self {
            name: name.to_string(),
            started: Instant::now(),
            binary_sha256: format!("{:x}", digest.finalize()),
            shell: shell.to_string(),
            language: language.to_string(),
            rows,
            cols,
            events: Vec::new(),
            bytes: 0,
            process: None,
            adapter: "fake".to_string(),
            provider_fixture_sha256: None,
        }
    }

    pub(super) fn provider_fixture(&mut self, adapter: &str, program: Option<&str>) {
        self.adapter = adapter.to_string();
        self.provider_fixture_sha256 =
            program.map(|program| format!("{:x}", Sha256::digest(std::fs::read(program).unwrap())));
    }

    pub(super) fn bind_process(&mut self, pid: u32) {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
        let start_ticks = stat
            .rsplit_once(") ")
            .and_then(|(_, fields)| fields.split_whitespace().nth(19))
            .unwrap()
            .to_string();
        let boot_id = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap();
        self.process = Some(json!({
            "pid": pid,
            "start_ticks": start_ticks,
            "boot_id": boot_id.trim(),
        }));
    }

    pub(super) fn bytes(&mut self, kind: &str, bytes: &[u8]) {
        self.bytes += bytes.len();
        assert!(self.bytes <= 64 * 1024, "approval recording byte limit");
        assert!(self.events.len() < 4096, "approval recording event limit");
        let data_hex = bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        self.events.push(json!({
            "at_ns": self.started.elapsed().as_nanos().to_string(),
            "kind": kind,
            "data_hex": data_hex,
        }));
    }

    pub(super) fn resize(&mut self, rows: u16, cols: u16) {
        self.events.push(json!({
            "at_ns": self.started.elapsed().as_nanos().to_string(),
            "kind": "resize",
            "rows": rows,
            "cols": cols,
        }));
    }

    pub(super) fn point(&mut self, label: &str) {
        self.events.push(json!({
            "at_ns": self.started.elapsed().as_nanos().to_string(),
            "kind": "point",
            "label": label,
        }));
    }

    pub(super) fn emit(self, closed: bool, exit_code: Option<i32>) {
        let closed_at_ns = self.started.elapsed().as_nanos().to_string();
        println!(
            "COSH_APPROVAL_PTY_RECORDING {}",
            json!({
                "schema_version": 1,
                "name": self.name,
                "binary_sha256": self.binary_sha256,
                "adapter": self.adapter,
                "provider_fixture_sha256": self.provider_fixture_sha256,
                "shell": self.shell,
                "language": self.language,
                "rows": self.rows,
                "cols": self.cols,
                "clock": "std::time::Instant",
                "closed": closed,
                "exit_code": exit_code,
                "closed_at_ns": closed_at_ns,
                "process": self.process,
                "closure_scope": "direct-shell-exit-and-terminal-restoration",
                "cleanup_verdict": "NOT_EVALUATED",
                "events": self.events,
            })
        );
    }
}
