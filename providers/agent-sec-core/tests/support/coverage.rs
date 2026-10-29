//! A deterministic consumer with real Provider transport and recorded native side effects.

use aw_adapter::{select, NativeContext};
use aw_host::{Host, Invocation, ProcessContext, StepOutput};
use aw_provider::admission::AdapterCapabilities;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::{Duration, Instant},
};

static NEXT: AtomicUsize = AtomicUsize::new(0);
const BUDGET: Duration = Duration::from_secs(10);

pub struct Consumer {
    pub root: PathBuf,
}

impl Consumer {
    pub fn new() -> Self {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/coverage")
            .join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&root).unwrap();
        let provider = root.join("provider");
        fs::copy(env!("CARGO_BIN_EXE_aw-provider-sec-core"), &provider).unwrap();
        let cli = root.join("scanner");
        fs::write(
            &cli,
            r#"#!/usr/bin/python3
import json, os, pathlib, sys, time
root = pathlib.Path(__file__).parent
stat = pathlib.Path(f'/proc/{os.getpid()}/stat').read_text().rsplit(')', 1)[1].split()
(root / 'scanner.pid').write_text(f'{os.getpid()} {stat[19]}')
with (root / 'scans.jsonl').open('a') as output:
    output.write(json.dumps(sys.argv[1:]) + '\n')
mode = (root / 'mode').read_text()
if mode == 'timeout': time.sleep(30)
if mode == 'backend':
    print(json.dumps({'ok': False, 'verdict': 'error'}))
    sys.exit(1)
print(json.dumps({'ok': True, 'verdict': 'deny' if mode == 'deny' else 'pass'}))
"#,
        )
        .unwrap();
        fs::set_permissions(cli, fs::Permissions::from_mode(0o700)).unwrap();
        let consumer = Self { root };
        consumer.scanner_mode("pass");
        consumer
    }

    pub fn scanner_mode(&self, mode: &str) {
        fs::write(self.root.join("mode"), mode).unwrap();
    }

    pub fn document(&self, adapter: &str) -> Value {
        let mut document = if matches!(adapter, "qoder" | "openclaw") {
            aw_package::document(&aw_package::Settings {
                prefix: &self.root,
                config: &self.root.join("aw.yaml"),
                state: &self.root.join("state"),
                policy: aw_package::Policy::Template {
                    definition: serde_json::from_str(include_str!(
                        "../../../../distribution/packaging/aw/templates/sec-core.json"
                    ))
                    .unwrap(),
                    variables: BTreeMap::from([
                        ("PROVIDERS_DIR".into(), self.root.display().to_string()),
                        (
                            "SEC_CORE_CLI".into(),
                            self.root.join("scanner").display().to_string(),
                        ),
                        ("SOCKET".into(), "/unused.sock".into()),
                    ]),
                },
                qoder: Some(Path::new("/bin/true")),
                node: Some(Path::new("/bin/true")),
                openclaw: Some(Path::new("/bin/true")),
            })
            .unwrap()
        } else if adapter == "provider-example" {
            serde_yaml_ng::from_str(include_str!("../../examples/aw.yaml")).unwrap()
        } else {
            let mut document: Value = serde_yaml_ng::from_str(include_str!(
                "../../../../src/aw/crates/aw-service/examples/aw.sdk-security.yaml"
            ))
            .unwrap();
            // Isolate the example's security step from optional application scripts.
            document["spec"]["providers"]
                .as_object_mut()
                .unwrap()
                .retain(|name, _| name == "security");
            document["spec"]["events"]["tool.before"]["steps"]
                .as_array_mut()
                .unwrap()
                .truncate(1);
            document["spec"]["events"]
                .as_object_mut()
                .unwrap()
                .remove("tool.after");
            document["spec"]["events"]["tool.before"]["steps"][0]["id"] = json!("scan-code");
            document
        };
        let provider = &mut document["spec"]["providers"]["security"];
        provider["transport"]["argv"] = json!([
            self.root.join("provider"),
            "--cli",
            self.root.join("scanner"),
            "--socket",
            "/unused.sock"
        ]);
        provider["timeout_ms"] = json!(1000);
        document
    }

    pub fn prepare(&self, document: &Value, adapter: &str) -> Host {
        Host::prepare(
            &serde_json::to_vec(document).unwrap(),
            adapter,
            AdapterCapabilities {
                adapter: adapter.into(),
                version: "fixture".into(),
                entrypoint: "fixture".into(),
                events: BTreeMap::from([
                    ("tool.before".into(), vec!["observe".into(), "block".into()]),
                    ("tool.after".into(), vec!["observe".into()]),
                ]),
                failure_actions: Default::default(),
            },
            ProcessContext {
                cwd: self.root.clone(),
                environment: BTreeMap::new(),
                stderr_bytes: 4096,
            },
            Instant::now() + BUDGET,
            &AtomicBool::new(false),
        )
        .unwrap()
    }

    pub fn covered_name(&self, adapter: &str) -> &'static str {
        match adapter {
            "qoder" | "qoder-sdk" => "Bash",
            "openclaw" => "exec",
            "qwenpaw" => "execute_shell_command",
            _ => panic!("missing dialect"),
        }
    }

    pub fn event(&self, adapter: &str, name: &str, input: Value) -> Value {
        let native = match adapter {
            "qoder" | "qoder-sdk" => json!({"hook_event_name":"PreToolUse", "cwd":self.root,
                "session_id":"session", "tool_use_id":"call", "tool_name":name,"tool_input":input}),
            "openclaw" => {
                json!({"hook":"before_tool_call", "event":{"toolName":name,"params":input,
                "toolCallId":"call","runId":"run"}, "context":{"sessionId":"session","runId":"run","toolCallId":"call"}})
            }
            "qwenpaw" => {
                json!({"event":"tool.before", "invocation_id":"00000000-0000-4000-8000-000000000001",
                "session_id":"session", "tool_call":{"name":name,"id":"call","input":input.to_string()},"effective_input":input})
            }
            _ => panic!("missing dialect"),
        };
        select(adapter)
            .unwrap()
            .normalize(
                &NativeContext {
                    target: adapter.into(),
                    instance_id: "instance".into(),
                    cwd: self.root.clone(),
                },
                "tool.before",
                &native,
            )
            .unwrap()
    }

    pub fn invoke(&self, host: &Host, value: Value) -> Invocation {
        let before = value["name"] == "tool.before";
        let cancelled = AtomicBool::new(false);
        let event = host
            .event(value, Instant::now() + BUDGET, &cancelled)
            .unwrap();
        let step = if before {
            "scan-code"
        } else {
            "observe-completion"
        };
        let result = event.invoke(step).unwrap();
        assert!(event.invoke(step).is_err());
        result
    }

    pub fn dispatch(&self, result: &Invocation, native_denied: bool) -> bool {
        let permitted = !native_denied
            && match &result.result {
                Ok(StepOutput::Skipped) => true,
                Ok(StepOutput::Provider(outcome)) => !outcome.requests_block(),
                _ => false,
            };
        if permitted {
            let count = self.handler_calls() + 1;
            fs::write(self.root.join("handler-count"), count.to_string()).unwrap();
            fs::write(self.root.join("native-effect"), "executed").unwrap();
        }
        permitted
    }

    pub fn scan_calls(&self) -> usize {
        fs::read_to_string(self.root.join("scans.jsonl"))
            .map(|s| s.lines().count())
            .unwrap_or(0)
    }

    pub fn handler_calls(&self) -> usize {
        fs::read_to_string(self.root.join("handler-count"))
            .map(|s| s.parse().unwrap())
            .unwrap_or(0)
    }

    pub fn effect_exists(&self) -> bool {
        self.root.join("native-effect").exists()
    }

    pub fn crash_provider(&self) {
        fs::write(self.root.join("provider"), "#!/bin/sh\nkill -KILL $$\n").unwrap();
    }
}

impl Drop for Consumer {
    fn drop(&mut self) {
        if let Ok(identity) = fs::read_to_string(self.root.join("scanner.pid")) {
            let (pid, started) = identity.split_once(' ').unwrap();
            if let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) {
                let fields: Vec<_> = stat
                    .rsplit_once(')')
                    .unwrap()
                    .1
                    .split_whitespace()
                    .collect();
                assert!(
                    fields[19] != started || matches!(fields[0], "Z" | "X"),
                    "scanner outlived its Provider"
                );
            }
        }
        fs::remove_dir_all(&self.root).unwrap();
    }
}
