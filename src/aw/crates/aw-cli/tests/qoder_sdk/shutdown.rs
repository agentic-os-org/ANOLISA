//! Exercise the documented signal handler through AW readiness and the launcher.

use super::*;
use std::{
    os::unix::process::ExitStatusExt,
    time::{Duration, Instant},
};
use support::Process;

#[test]
fn documented_worker_drains_or_is_forcibly_reclaimed_in_both_modes() {
    let docs = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../docs/developer-guide");
    let example = |language| {
        fs::read_to_string(docs.join(format!("{language}/aw/agents/qoder-sdk.md")))
            .unwrap()
            .split("```javascript\n")
            .nth(1)
            .unwrap()
            .split("```")
            .next()
            .unwrap()
            .to_owned()
    };
    let worker = example("en");
    assert_eq!(worker, example("zh"));
    for reloadable in [false, true] {
        for signal in [libc::SIGTERM, libc::SIGINT] {
            for stubborn in [false, true] {
                let f = Fixture::new();
                let mut document = setup(&f);
                document["spec"]["agents"]["sdk"]["shutdown_grace_ms"] = json!(800);
                let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/qoder_sdk");
                let sdk = f.root.join("node_modules/@qoder-ai/qoder-agent-sdk/dist");
                fs::copy(source.join("sdk.mjs"), sdk.join("fixture-sdk.mjs")).unwrap();
                fs::copy(source.join("shutdown-sdk.mjs"), sdk.join("index.js")).unwrap();
                fs::write(f.root.join("app.mjs"), &worker).unwrap();
                let service = Service::start(&f, &document);
                let mut command = launch(&f);
                if reloadable {
                    command.arg("--reloadable");
                }
                if stubborn {
                    command.env("QODER_STUBBORN", "1");
                }
                command.args(["--", "fixture"]);
                let mut process = Process::spawn(&f, command);
                process
                    .wait_until(
                        "SDK consumption and native readiness",
                        Duration::from_secs(8),
                        |_| {
                            let state = service.status();
                            Ok((
                                f.root.join("consuming").exists()
                                    && state["instances"]
                                        .as_array()
                                        .unwrap()
                                        .iter()
                                        .any(|instance| instance["ready"] == true),
                                state.to_string(),
                            ))
                        },
                    )
                    .unwrap();
                let native: Value =
                    serde_json::from_slice(&fs::read(f.root.join("sdk-process.json")).unwrap())
                        .unwrap();
                let started = Instant::now();
                process.signal(signal);
                let output = process.finish();
                assert_eq!(
                    (output.status.code(), output.status.signal()),
                    if stubborn {
                        (None, Some(libc::SIGKILL))
                    } else {
                        (Some(0), None)
                    },
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                assert!(f.root.join("closing").exists());
                assert_eq!(f.root.join("query-drained").exists(), !stubborn);
                assert_eq!(f.root.join("daemon-stopped").exists(), !stubborn);
                if stubborn {
                    assert!(started.elapsed() >= Duration::from_millis(800));
                }
                assert!(started.elapsed() < Duration::from_secs(5));
                for key in ["worker", "pid"] {
                    assert!(
                        !Path::new(&format!("/proc/{}", native[key])).exists(),
                        "{key} remained after launcher exit"
                    );
                }
                service.released(&f);
            }
        }
    }
}
