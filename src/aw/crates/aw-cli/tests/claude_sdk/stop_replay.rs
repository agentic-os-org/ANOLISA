//! A retired Stop must preserve the native result after the bridge cache evicts it.

use super::*;
use std::{process::Command, time::Duration};

#[test]
fn evicted_stop_replies_preserve_block_reason_and_allow_without_repeating_the_provider() {
    for (action, shadow) in [("report", false), ("block", false), ("report", true)] {
        let fixture = Fixture::new();
        let mut document = fixture.document();
        document["spec"]["agents"] =
            json!({"claude":{"adapter":"claude-sdk","argv":["python3","worker.py"]}});
        let provider = fixture.root.join("stop-provider.py");
        fs::write(
            &provider,
            r#"
import json, sys
from pathlib import Path
request = json.load(sys.stdin)
reply = {key: request[key] for key in ('api_version', 'request_id')}
reply['status'] = 'ok'
if request['method'] == 'describe':
    reply['operations'] = [
        {'name':'check','events':['turn.stop'],'effects':['block']},
        {'name':'observe_stop','events':['turn.stop'],'effects':['observe']},
    ]
elif request['method'] == 'invoke':
    native = request['event']['native']
    reply['input_digest'] = request['input_digest']
    reply['effects'] = []
    if request['operation'] == 'check':
        with Path(request['config']['calls']).open('a') as log:
            log.write(native['delivery_id'] + '\n')
        if native['veto']:
            reply['effects'] = [
                {'type':'block','reason_code':'policy_denied','text':'Wait for confirmation · 请先确认'}
            ]
    else:
        with Path(request['config']['shadow_calls']).open('a') as log:
            log.write(native['delivery_id'] + '\n')
print(json.dumps(reply))
"#,
        )
        .unwrap();
        document["spec"]["providers"]["policy"] = json!({
            "protocol":"aw-provider/v1alpha1","transport":{"type":"stdio","location":"agent",
            "argv":[fixture.python, provider]}, "timeout_ms":3000,"max_output_bytes":4096,
            "config":{"calls":fixture.root.join("calls"),"shadow_calls":fixture.root.join("shadow-calls")}
        });
        document["spec"]["events"] = json!({"turn.stop":{"enabled":true,"steps":[
            {"id":"check","provider":"policy","operation":"check",
                "effects":["block"],"on_error":action}
        ]}});
        if shadow {
            document["spec"]["events"]["turn.stop"]["steps"]
                .as_array_mut()
                .unwrap()
                .insert(
                    0,
                    json!({"id":"shadow","provider":"policy","operation":"observe_stop",
                    "effects":["observe"],"on_error":"report","mode":"shadow"}),
                );
        }
        let service = Service::start(&fixture, &document);
        let binding: Binding = serde_json::from_value(
            service
                .client
                .call(
                    Operation::Bind {
                        target: "claude".into(),
                        capabilities: Capabilities {
                            failure_actions: Default::default(),
                            adapter: "claude-sdk".into(),
                            version: "0.2.88".into(),
                            entrypoint: "sdk-worker".into(),
                            events: BTreeMap::from([(
                                "turn.stop".into(),
                                vec!["observe".into(), "block".into()],
                            )]),
                        },
                        cwd: fixture.root.to_str().unwrap().into(),
                        environment: Default::default(),
                    },
                    Instant::now() + TIMEOUT,
                )
                .unwrap(),
        )
        .unwrap();
        let binding_path = fixture.root.join("binding.json");
        fs::write(
            &binding_path,
            json!({"adapter":"claude-sdk","binding":binding,
            "socket":fixture.root.join("state/aw.sock"),"cwd":fixture.root,"events":{
                "turn.stop":{"budget_ms":5000,"steps":{"check":action}}
            }})
            .to_string(),
        )
        .unwrap();
        fs::set_permissions(&binding_path, fs::Permissions::from_mode(0o600)).unwrap();

        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut command = Command::new(&fixture.python);
        command
            .arg("-B")
            .arg(root.join("tests/fixtures/claude-sdk/stop_replay.py"))
            .arg(root.join("../aw-adapter/native/claude-sdk"))
            .arg(env!("CARGO_BIN_EXE_aw"))
            .arg(binding_path)
            .arg(&fixture.root)
            .arg(action);
        let output =
            support::Process::spawn(&fixture, command).finish_with_timeout(Duration::from_secs(40));
        assert!(
            output.status.success(),
            "{action}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        if shadow {
            let observed = fs::read_to_string(fixture.root.join("shadow-calls")).unwrap();
            assert_eq!(
                observed
                    .lines()
                    .filter(|delivery| *delivery == "original")
                    .count(),
                1
            );
        }
        service
            .client
            .call(
                Operation::ReleaseInstance {
                    instance_id: binding.instance_id,
                },
                Instant::now() + TIMEOUT,
            )
            .unwrap();
        service.released(&fixture);
    }
}
