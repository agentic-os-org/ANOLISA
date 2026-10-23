//! SDK launcher contract fixtures; published-SDK tool adoption is tested by public-smoke.py.
#![cfg(target_os = "linux")]
#[allow(dead_code)]
#[path = "launcher/support.rs"]
mod support;
use serde_json::{json, Value};
use std::{fs, path::Path, process::Command};
use support::{Fixture, Service};

fn setup(fixture: &Fixture) -> Value {
    let node = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|p| p.join("node"))
        .find(|p| p.is_file())
        .expect("Pi fixtures require Node.js >=22.19");
    let application = fixture.root.join("app.mjs");
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pi/synthetic.mjs"),
        &application,
    )
    .unwrap();
    for (name,source) in [("pi-agent-core","export class Agent { subscribe(){ return ()=>{}; } }".to_string()),
        ("pi-coding-agent","import {Agent} from '@earendil-works/pi-agent-core'; export class AgentSession { constructor(){this.agent=new Agent();this.sessionManager={getSessionId:()=> 'session-1'};} dispose(){} }".to_string())] {
        let path=fixture.root.join("node_modules/@earendil-works").join(name);fs::create_dir_all(path.join("dist")).unwrap();
        fs::write(path.join("package.json"),json!({"name":format!("@earendil-works/{name}"),"version":"0.84.2","type":"module","exports":{".":{"import":"./dist/index.js"}}}).to_string()).unwrap();
        fs::write(path.join("dist/index.js"),source).unwrap();
    }
    let mut document = fixture.document();
    document["spec"]["agents"] = json!({"pi":{"adapter":"pi","argv":[node,application]}});
    document
}
fn launch(fixture: &Fixture) -> Command {
    let mut command = fixture.command();
    command
        .args(["run", "--config"])
        .arg(fixture.root.join("aw.json"))
        .args(["--agent", "pi"]);
    command
}
fn policy(fixture: &Fixture, document: &mut Value, behavior: &str) {
    for (event, label) in [("tool.before", "before"), ("tool.after", "after")] {
        document["spec"]["providers"][label] = json!({"transport":{"type":"stdio","location":"agent","argv":[fixture.python,Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pi/policy.py"),fixture.root,behavior]},"timeout_ms":2000,"max_output_bytes":32});
        document["spec"]["events"][event] = json!({"enabled":true,"required":true,"steps":[{"id":label,"provider":label,"effects":if label=="before" {vec!["observe","block"]}else{vec!["observe"]},"on_error":if label=="before" {"block"}else{"report"}}]});
    }
}
fn events(fixture: &Fixture) -> Vec<Value> {
    fs::read_to_string(fixture.root.join("policy-events.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
#[test]
fn foreground_factory_installs_both_hooks_and_releases_binding() {
    let fixture = Fixture::new();
    let mut document = setup(&fixture);
    policy(&fixture, &mut document, "normal");
    let service = Service::start(&fixture, &document);
    assert_eq!(fixture.successful(launch(&fixture))["denied"], false);
    let events = events(&fixture);
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["name"], "tool.before");
    assert_eq!(events[1]["name"], "tool.after");
    assert_eq!(events[0]["tool"]["call_id"], events[1]["tool"]["call_id"]);
    assert_eq!(events[0]["session_id"], "session-1");
    assert!(fixture.root.join("side-effect").exists());
    service.released(&fixture);
}
#[test]
fn policy_denial_and_native_mutation_skip_execution_and_after() {
    for variable in ["PI_DENY", "PI_MUTATE"] {
        let fixture = Fixture::new();
        let mut document = setup(&fixture);
        policy(&fixture, &mut document, "normal");
        let service = Service::start(&fixture, &document);
        let mut command = launch(&fixture);
        command.env(variable, "1");
        assert_eq!(fixture.successful(command)["denied"], true);
        assert!(!fixture.root.join("side-effect").exists());
        let events = events(&fixture);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["tool"]["input"]["deny"], true);
        service.released(&fixture);
    }
}
#[test]
fn native_denial_keeps_precedence_and_never_invokes_aw() {
    let fixture = Fixture::new();
    let mut document = setup(&fixture);
    policy(&fixture, &mut document, "normal");
    let service = Service::start(&fixture, &document);
    let mut command = launch(&fixture);
    command.env("PI_NATIVE_DENY", "1");
    assert_eq!(fixture.successful(command)["denied"], true);
    assert!(!fixture.root.join("side-effect").exists());
    assert!(!fixture.root.join("policy-events.jsonl").exists());
    service.released(&fixture);
}
#[test]
fn after_observes_native_replacement_and_error_flag() {
    let fixture = Fixture::new();
    let mut document = setup(&fixture);
    policy(&fixture, &mut document, "normal");
    let service = Service::start(&fixture, &document);
    let mut command = launch(&fixture);
    command.env("PI_REPLACE", "1");
    fixture.successful(command);
    let events = events(&fixture);
    assert_eq!(
        events[1]["tool"]["result"]["content"][0]["text"],
        "native replacement"
    );
    assert_eq!(events[1]["tool"]["result"]["isError"], true);
    service.released(&fixture);
}
#[test]
fn unavailable_before_policy_adopts_failure_block() {
    let fixture = Fixture::new();
    let mut document = setup(&fixture);
    policy(&fixture, &mut document, "error");
    let service = Service::start(&fixture, &document);
    assert_eq!(fixture.successful(launch(&fixture))["denied"], true);
    assert!(!fixture.root.join("side-effect").exists());
    service.released(&fixture);
}
#[test]
fn actual_package_version_is_checked_without_importing_business_factory() {
    let fixture = Fixture::new();
    let document = setup(&fixture);
    fixture.save(&document);
    let package = fixture
        .root
        .join("node_modules/@earendil-works/pi-coding-agent/package.json");
    let mut metadata: Value = serde_json::from_slice(&fs::read(&package).unwrap()).unwrap();
    metadata["version"] = json!("0.84.3");
    fs::write(package, metadata.to_string()).unwrap();
    assert!(!fixture.run(launch(&fixture)).status.success());
    assert!(!fixture.root.join("state").exists());
    assert!(!fixture.root.join("side-effect").exists());
}
#[test]
fn missing_or_non_session_factory_result_cannot_publish_readiness() {
    for source in [
        "export async function createSession(){return {session:{}};} export async function run(){}",
        "export async function createSession(){}",
    ] {
        let fixture = Fixture::new();
        let document = setup(&fixture);
        fs::write(fixture.root.join("app.mjs"), source).unwrap();
        let service = Service::start(&fixture, &document);
        assert!(!fixture.run(launch(&fixture)).status.success());
        service.released(&fixture);
    }
}

#[test]
fn repeated_early_denials_close_events_before_the_next_tool_preparation() {
    for behavior in [
        "policy",
        "native-block",
        "invalid-json",
        "nonzero",
        "signal",
    ] {
        let fixture = Fixture::new();
        let mut document = setup(&fixture);
        policy(&fixture, &mut document, "normal");
        let before = &mut document["spec"]["events"]["tool.before"]["steps"];
        before.as_array_mut().unwrap().push(json!({
            "id":"unvisited", "provider":"before", "effects":["observe"], "on_error":"report"
        }));
        if behavior != "policy" {
            let source = format!(
                r#"import json, os, signal, sys
r = json.load(sys.stdin)
if r['tool_call']['input']['deny']:
    behavior = {behavior:?}
    if behavior == 'invalid-json': print('invalid JSON'); sys.exit(0)
    if behavior == 'nonzero': sys.exit(3)
    if behavior == 'signal': os.kill(os.getpid(), signal.SIGTERM)
    print(json.dumps({{'block':True,'reason':'native denial'}}))
else: print('{{}}')
"#
            );
            document["spec"]["providers"]["native"] = json!({"protocol":"native-hook/v1alpha1",
                "transport":{"type":"stdio","location":"agent","argv":[fixture.python,"-c",source]},
                "timeout_ms":2000,"max_output_bytes":4096,"config":{}});
            document["spec"]["events"]["tool.before"]["steps"][0] = json!({
                "id":"first", "provider":"native", "native":{}, "on_error":if behavior == "native-block" {"report"} else {"block"}
            });
        }
        let application = fs::read_to_string(fixture.root.join("app.mjs")).unwrap();
        let application = application
            .split("export async function run(session)")
            .next()
            .unwrap();
        fs::write(fixture.root.join("app.mjs"), format!(r#"{application}
import {{ readFileSync }} from 'node:fs';
import {{ spawnSync }} from 'node:child_process';
export async function run(session) {{
    const config = JSON.parse(readFileSync(process.env.AW_NATIVE_CONFIG, 'utf8'));
    const binding = JSON.parse(readFileSync(config.binding, 'utf8'));
    for (let i = 0; i < 41; i++) {{
        const context = {{ toolCall:{{id:'reused-model-id',name:'test'}},args:{{deny:i<40}},
            result:{{content:[{{type:'text',text:'okay'}}],details:{{}}}},isError:false }};
        const directive = await session.agent.beforeToolCall(context);
        if ((directive?.block === true) !== (i < 40)) throw Error('wrong denial');
        const status = spawnSync(config.executable, ['status','--socket',binding.socket], {{encoding:'utf8',timeout:2000}});
        if (status.status !== 0 || (i < 40 && JSON.parse(status.stdout).result.events !== 0)) throw Error('live event leaked at '+i+': '+status.status+' '+status.stdout+' '+status.stderr);
        if (i === 40) await session.agent.afterToolCall(context);
    }}
    console.log(JSON.stringify({{denied:40}}));
}}
"#)).unwrap();
        let service = Service::start(&fixture, &document);
        assert_eq!(
            fixture.successful(launch(&fixture))["denied"],
            40,
            "{behavior}"
        );
        let recorded = events(&fixture);
        assert_eq!(
            recorded
                .iter()
                .filter(|event| event["name"] == "tool.after")
                .count(),
            1
        );
        assert_eq!(
            recorded
                .iter()
                .filter(|event| event["name"] == "tool.before"
                    && event["tool"]["input"]["deny"] == false)
                .count(),
            if behavior == "policy" { 2 } else { 1 }
        );
        service.released(&fixture);
    }
}

#[test]
fn bridge_accepts_only_authorized_text_and_preserves_native_overrides() {
    let fixture = Fixture::new();
    let mut command = Command::new("node");
    command
        .current_dir(&fixture.root)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .args(["--test"])
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pi/bridge-contract.mjs"));
    let output = fixture.run(command);
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
