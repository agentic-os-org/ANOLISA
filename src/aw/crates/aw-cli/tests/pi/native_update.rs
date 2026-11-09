//! Native application startup retains task snapshots across explicit updates.
use super::*;

#[test]
fn native_entrypoints_preserve_configured_application_arguments() {
    for entrypoint in ["run", "runWorker"] {
        let fixture = Fixture::new();
        let mut document = setup(&fixture);
        document["spec"]["agents"]["pi"]["argv"]
            .as_array_mut()
            .unwrap()
            .extend([json!("task with spaces"), json!("--native-option=kept")]);
        document["spec"]["events"] = json!({});
        document["spec"]["providers"] = json!({});
        let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pi");
        fs::copy(
            assets.join("native-start.mjs"),
            fixture.root.join("native-start.mjs"),
        )
        .unwrap();
        fs::copy(
            assets.join("submission-sdk.mjs"),
            fixture
                .root
                .join("node_modules/@earendil-works/pi-coding-agent/dist/index.js"),
        )
        .unwrap();
        let factory = r#"
import assert from 'node:assert/strict';
import {writeFileSync} from 'node:fs';
const expected = ['task with spaces', '--native-option=kept'];
export async function createSession({sdk,args}) {
  assert.deepEqual(args, expected);
  return {session:new sdk.AgentSession(process.cwd())};
}
"#;
        let execute = if entrypoint == "run" {
            "export async function run(session,{args}) { assert.deepEqual(args,expected); assert.ok(session); writeFileSync('args-ok','ok'); }"
        } else {
            "export async function runWorker({createSession,args}) { assert.deepEqual(args,expected); const task=await createSession(); await task.close(); writeFileSync('args-ok','ok'); }"
        };
        fs::write(
            fixture.root.join("app.mjs"),
            format!("{factory}\n{execute}\n"),
        )
        .unwrap();
        fs::write(fixture.root.join("aw.yaml"), document.to_string()).unwrap();
        let service = Service::start(&fixture, &document);
        let mut command = Command::new(
            document["spec"]["agents"]["pi"]["argv"][0]
                .as_str()
                .unwrap(),
        );
        command
            .arg(fixture.root.join("native-start.mjs"))
            .current_dir(&fixture.root)
            .env("HOME", fixture.root.join("home"))
            .env("AW_TEST_EXECUTABLE", env!("CARGO_BIN_EXE_aw"));
        let output = fixture.run(command);
        assert!(
            output.status.success(),
            "{}: {}",
            entrypoint,
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            fs::read_to_string(fixture.root.join("args-ok")).unwrap(),
            "ok"
        );
        service.released(&fixture);
    }
}

#[test]
fn native_worker_updates_only_between_closed_sessions() {
    native_updates("0.84.2", "0.84.2");
}

#[test]
fn native_worker_rejects_strict_update_for_unverified_package_pairs() {
    for (sdk, core) in [("0.84.3", "0.84.2"), ("0.84.2", "0.84.3")] {
        native_updates(sdk, core);
    }
}

fn native_updates(sdk: &str, core: &str) {
    let fixture = Fixture::new();
    let mut document = setup(&fixture);
    document["spec"]["agents"]["pi"]["argv"]
        .as_array_mut()
        .unwrap()
        .extend([json!("task with spaces"), json!("--native-option=kept")]);
    document["spec"]["events"] = json!({});
    document["spec"]["providers"] = json!({});
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pi");
    for (source, target) in [
        ("native-update.mjs", "app.mjs"),
        ("native-start.mjs", "native-start.mjs"),
        (
            "submission-sdk.mjs",
            "node_modules/@earendil-works/pi-coding-agent/dist/index.js",
        ),
    ] {
        fs::copy(assets.join(source), fixture.root.join(target)).unwrap();
    }
    for (name, version) in [("pi-coding-agent", sdk), ("pi-agent-core", core)] {
        let path = fixture
            .root
            .join(format!("node_modules/@earendil-works/{name}/package.json"));
        let mut metadata: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        metadata["version"] = json!(version);
        fs::write(path, metadata.to_string()).unwrap();
    }
    let mut strict = document.clone();
    policy(&fixture, &mut strict, "normal");
    strict["spec"]["events"]["tool.after"]["steps"][0]["on_error"] = json!("withhold_result");
    fs::write(fixture.root.join("strict.json"), strict.to_string()).unwrap();
    let mut deny = document.clone();
    deny["spec"]["providers"]["submit"] = json!({
        "protocol":"aw-provider/v1alpha1",
        "transport":{"type":"stdio","location":"agent","argv":[fixture.python,
          assets.join("submission-policy.py"),fixture.root]},
        "timeout_ms":500,"max_output_bytes":4096,"config":{}});
    deny["spec"]["events"]["input.submit"] = json!({"enabled":true,"required":true,"budget_ms":1500,
        "steps":[{"id":"first","provider":"submit","operation":"submit",
          "effects":["observe","block"],"on_error":"block"}]});
    fs::write(fixture.root.join("deny.json"), deny.to_string()).unwrap();
    fs::write(fixture.root.join("aw.yaml"), document.to_string()).unwrap();
    let service = Service::start(&fixture, &document);
    let mut command = Command::new(
        document["spec"]["agents"]["pi"]["argv"][0]
            .as_str()
            .unwrap(),
    );
    command
        .arg(fixture.root.join("native-start.mjs"))
        .current_dir(&fixture.root)
        .env("HOME", fixture.root.join("home"))
        .env("FAKE_ROOT", &fixture.root)
        .env(
            "PI_REJECT_STRICT",
            if sdk == "0.84.2" && core == "0.84.2" {
                "0"
            } else {
                "1"
            },
        )
        .env("AW_TEST_EXECUTABLE", env!("CARGO_BIN_EXE_aw"));
    let output = fixture.run(command);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value =
        serde_json::from_slice(&fs::read(fixture.root.join("native-result.json")).unwrap())
            .unwrap();
    assert_eq!(result["passed"], true);
    service.released(&fixture);
}

#[test]
fn native_restart_uses_edited_default_policy_in_the_same_service() {
    let fixture = Fixture::new();
    let mut document = setup(&fixture);
    document["spec"]["events"] = json!({});
    document["spec"]["providers"] = json!({});
    fs::write(fixture.root.join("app.mjs"),
        "export async function createSession() { throw new Error('unused'); }\nexport async function runWorker() {}\n").unwrap();
    fs::write(fixture.root.join("aw.yaml"), document.to_string()).unwrap();
    let service = Service::start(&fixture, &document);
    let mut candidate = document.clone();
    policy(&fixture, &mut candidate, "normal");
    fs::write(fixture.root.join("candidate.json"), candidate.to_string()).unwrap();
    let script = r#"
import assert from 'node:assert/strict';
import {spawnSync} from 'node:child_process';
import {readFileSync, writeFileSync} from 'node:fs';
import {createHash} from 'node:crypto';
const aw = process.env.AW_TEST_EXECUTABLE;
const call = args => {
  const result = spawnSync(aw, args, {encoding:'utf8', timeout:35000});
  assert.equal(result.status, 0, result.stderr);
  return result.stdout.trim() ? JSON.parse(result.stdout) : null;
};
const read = path => JSON.parse(readFileSync(path, 'utf8'));
const first = call(['instance','prepare','--agent','pi']);
const original = read(first.binding);
call(['instance','release','--binding',first.binding]);
const bytes = readFileSync('candidate.json');
writeFileSync('aw.yaml', bytes);
const next = call(['instance','prepare','--agent','pi']);
try {
  const binding = read(next.binding);
  assert.equal(binding.socket, original.socket);
  assert.deepEqual(binding.binding.identity, original.binding.identity);
  assert.notEqual(binding.registration.policy_revision, original.registration.policy_revision);
  assert.equal(binding.registration.policy_revision, createHash('sha256').update(bytes).digest('hex'));
  const config = read(next.environment.AW_NATIVE_CONFIG);
  assert.ok(config.hooks.some(hook => hook.event === 'tool.before'));
} finally { call(['instance','release','--binding',next.binding]); }
"#;
    let mut command = Command::new(
        document["spec"]["agents"]["pi"]["argv"][0]
            .as_str()
            .unwrap(),
    );
    command
        .args(["--input-type=module", "-e", script])
        .current_dir(&fixture.root)
        .env("HOME", fixture.root.join("home"))
        .env("AW_TEST_EXECUTABLE", env!("CARGO_BIN_EXE_aw"));
    let output = fixture.run(command);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    service.released(&fixture);
}
