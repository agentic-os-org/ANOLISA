//! Native exec-form startup uses the real service over a deterministic Qoder host.

use super::{
    attachment::{action, native_command, success, wait},
    support::{Fixture, Process, Service},
};
use serde_json::{json, Value};
use std::{fs, os::unix::fs::PermissionsExt, path::Path};

fn setup(fixture: &Fixture, remove: bool) -> Value {
    let mut command = fixture.command();
    command
        .args(["instance", "setup", "--config"])
        .arg(fixture.root.join("aw.json"))
        .args(["--agent", "qoder", "--directory"])
        .arg(fixture.root.join("startup"));
    if remove {
        command.arg("--remove");
    }
    success(fixture.run(command))
}

fn prepare(fixture: &Fixture) {
    let profile = fixture.root.join("home/.qoder");
    fs::create_dir(&profile).unwrap();
    fs::set_permissions(&profile, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(profile.join("settings.json"), br#"{"theme":"keep","hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"/bin/true"}]}]}}"#).unwrap();
    fs::set_permissions(
        profile.join("settings.json"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/launcher/attach_host.py"),
        fixture.root.join("fake qoder"),
    )
    .unwrap();
    fs::set_permissions(
        fixture.root.join("fake qoder"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
}

fn deny_policy(fixture: &Fixture, document: &mut Value) {
    fixture.native(document, "deny", "block");
    document["spec"]["providers"]["deny"]["transport"]["argv"] = json!([
        "/bin/sh",
        "-c",
        "printf 'invoked\\n' >> \"$1\"; exit 2",
        "policy-fixture",
        fixture.root.join("deny-invocations")
    ]);
}

#[test]
fn native_print_startup_updates_and_releases_without_aw_run() {
    let fixture = Fixture::new();
    prepare(&fixture);
    let original = fs::read(fixture.root.join("home/.qoder/settings.json")).unwrap();
    let mut document = fixture.document();
    deny_policy(&fixture, &mut document);
    let service = Service::start(&fixture, &document);
    setup(&fixture, false);
    let mut command = native_command(&fixture);
    command
        .args(["-p", "fixture"])
        .current_dir(&fixture.root)
        .env_remove("XDG_RUNTIME_DIR")
        .env("HOME", fixture.root.join("home"))
        .env("FAKE_ROOT", &fixture.root)
        .env("FAKE_NATIVE_START", "1")
        .env("FAKE_NATIVE_VERSION", "1.1.79");
    let process = Process::spawn(&fixture, command);
    let started = wait(&fixture.root.join("native-start.json"));
    assert!(
        started
            .as_array()
            .unwrap()
            .iter()
            .all(|hook| hook["code"] == 0),
        "{started}"
    );
    assert_eq!(service.status()["instances"][0]["ready"], true);
    assert_eq!(service.status()["instances"][0]["entrypoint"], "print");
    assert!(action(&fixture, 0, "turn")
        .as_array()
        .unwrap()
        .iter()
        .any(|hook| hook["code"] == 2));
    document["spec"]["events"]["tool.before"]["steps"] = json!([]);
    fixture.save(&document);
    let mut update = fixture.command();
    update
        .args(["instance", "update", "--config"])
        .arg(fixture.root.join("aw.json"))
        .args(["--agent", "qoder"]);
    success(fixture.run(update));
    assert!(action(&fixture, 1, "turn")
        .as_array()
        .unwrap()
        .iter()
        .all(|hook| hook["code"] == 0));
    fs::write(
        fixture.root.join("attach-command-2.json"),
        br#"{"action":"stop"}"#,
    )
    .unwrap();
    let output = process.finish();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let ended = wait(&fixture.root.join("native-end.json"));
    assert!(
        ended
            .as_array()
            .unwrap()
            .iter()
            .all(|hook| hook["code"] == 0),
        "{ended}"
    );
    service.released(&fixture);
    assert_eq!(
        fs::read_dir(fixture.root.join("startup")).unwrap().count(),
        1
    );
    setup(&fixture, true);
    let restored: Value =
        serde_json::from_slice(&fs::read(fixture.root.join("home/.qoder/settings.json")).unwrap())
            .unwrap();
    assert_eq!(
        restored,
        serde_json::from_slice::<Value>(&original).unwrap()
    );
}

fn start(fixture: &Fixture, work: &Path) -> Process {
    fs::create_dir_all(work).unwrap();
    let mut command = native_command(fixture);
    command
        .args(["-p", "fixture"])
        .current_dir(work)
        .env_remove("XDG_RUNTIME_DIR")
        .env("HOME", fixture.root.join("home"))
        .env("FAKE_ROOT", work)
        .env("FAKE_NATIVE_START", "1");
    let process = Process::spawn(fixture, command);
    let result = wait(&work.join("native-start.json"));
    assert!(
        result
            .as_array()
            .unwrap()
            .iter()
            .all(|hook| hook["code"] == 0),
        "{result}"
    );
    process
}

fn turn(work: &Path, index: usize) -> Value {
    fs::write(
        work.join(format!("attach-command-{index}.json")),
        br#"{"action":"turn"}"#,
    )
    .unwrap();
    wait(&work.join(format!("attach-result-{index}.json")))
}

#[test]
fn rejected_configuration_keeps_active_policy_and_exit_independent_of_disk() {
    let fixture = Fixture::new();
    prepare(&fixture);
    let mut document = fixture.document();
    deny_policy(&fixture, &mut document);
    let service = Service::start(&fixture, &document);
    setup(&fixture, false);
    let process = start(&fixture, &fixture.root);
    let policy = service.status()["instances"][0]["policy_revision"].clone();
    for (index, mutation) in ["invalid", "arguments", "executable", "adapter"]
        .iter()
        .enumerate()
    {
        let mut candidate = document.clone();
        match *mutation {
            "invalid" => fs::write(fixture.root.join("aw.json"), b"invalid: [").unwrap(),
            "arguments" => {
                candidate["spec"]["agents"]["qoder"]["argv"] =
                    json!([fixture.root.join("fake qoder"), "--changed"]);
                fixture.save(&candidate);
            }
            "executable" => {
                candidate["spec"]["agents"]["qoder"]["argv"][0] = json!("/bin/false");
                fixture.save(&candidate);
            }
            _ => {
                candidate["spec"]["agents"]["qoder"]["adapter"] = json!("qoder-sdk");
                fixture.save(&candidate);
            }
        }
        let mut update = fixture.command();
        update
            .args(["instance", "update", "--config"])
            .arg(fixture.root.join("aw.json"))
            .args(["--agent", "qoder"]);
        let rejected = fixture.run(update);
        assert!(!rejected.status.success(), "accepted {mutation}");
        assert_eq!(service.status()["instances"][0]["policy_revision"], policy);
        let callbacks = turn(&fixture.root, index);
        assert_eq!(callbacks[0]["code"], 0, "{mutation}: {callbacks}");
        assert_eq!(callbacks[1]["code"], 2, "{mutation}: {callbacks}");
        assert!(
            callbacks
                .as_array()
                .unwrap()
                .iter()
                .all(|hook| !hook["stderr"]
                    .as_str()
                    .unwrap()
                    .contains("startup callback failed")),
            "{mutation}: {callbacks}"
        );
        assert_eq!(
            fs::read_to_string(fixture.root.join("deny-invocations"))
                .unwrap()
                .lines()
                .count(),
            index + 1
        );
    }
    document["spec"]["events"]["tool.before"]["steps"] = json!([]);
    fixture.save(&document);
    let mut update = fixture.command();
    update
        .args(["instance", "update", "--config"])
        .arg(fixture.root.join("aw.json"))
        .args(["--agent", "qoder"]);
    success(fixture.run(update));
    fs::write(fixture.root.join("aw.json"), b"invalid: [").unwrap();
    assert!(turn(&fixture.root, 4)
        .as_array()
        .unwrap()
        .iter()
        .all(|hook| hook["code"] == 0));
    fs::write(
        fixture.root.join("attach-command-5.json"),
        br#"{"action":"stop"}"#,
    )
    .unwrap();
    assert!(process.finish().status.success());
    let ended = wait(&fixture.root.join("native-end.json"));
    assert!(
        ended
            .as_array()
            .unwrap()
            .iter()
            .all(|hook| hook["code"] == 0),
        "{ended}"
    );
    service.released(&fixture);
    assert_eq!(
        fs::read_dir(fixture.root.join("startup")).unwrap().count(),
        1
    );
    fixture.save(&document);
    setup(&fixture, true);
}

#[test]
fn shared_profile_owners_reject_missing_stale_and_cross_owner_locators() {
    let fixture = Fixture::new();
    prepare(&fixture);
    let service = Service::start(&fixture, &fixture.document());
    setup(&fixture, false);
    let first_work = fixture.root.join("first");
    let second_work = fixture.root.join("second");
    let first = start(&fixture, &first_work);
    let second = start(&fixture, &second_work);
    assert_eq!(service.status()["instances"].as_array().unwrap().len(), 2);
    let pid = wait(&first_work.join("native-pid.json")).as_u64().unwrap();
    let locators: Vec<_> = fs::read_dir(fixture.root.join("startup"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("owner-")
        })
        .collect();
    assert_eq!(locators.len(), 2);
    let path = locators
        .iter()
        .find(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(&format!("owner-{pid}-"))
        })
        .unwrap();
    let other = locators
        .iter()
        .find(|candidate| *candidate != path)
        .unwrap();
    let original: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert!(original["launch"].get("args").is_none());
    assert_eq!(
        original["launch"]["args_digest"].as_str().unwrap().len(),
        64
    );
    let foreign: Value = serde_json::from_slice(&fs::read(other).unwrap()).unwrap();
    let backup = path.with_extension("held");
    fs::rename(path, &backup).unwrap();
    assert_eq!(turn(&first_work, 0)[0]["code"], 2);
    fs::rename(&backup, path).unwrap();
    for (index, mutation) in ["start", "generation", "session", "launch", "other_owner"]
        .iter()
        .enumerate()
    {
        let mut record = original.clone();
        match *mutation {
            "start" => record["process"]["start_ticks"] = json!("0"),
            "generation" => record["identity"]["generation"] = json!("stale"),
            "session" => record["session_id"] = json!("other-session"),
            "launch" => record["launch"]["args_digest"] = json!("stale"),
            _ => record["binding"] = foreign["binding"].clone(),
        }
        fs::write(path, record.to_string()).unwrap();
        assert_eq!(turn(&first_work, index + 1)[0]["code"], 2);
        assert!(!first_work
            .join(format!("model-dispatch-{}.json", index + 1))
            .exists());
    }
    fs::write(path, original.to_string()).unwrap();
    assert_eq!(turn(&first_work, 6)[0]["code"], 0);
    assert_eq!(turn(&second_work, 0)[0]["code"], 0);
    let mut remove = fixture.command();
    remove
        .args(["instance", "setup", "--remove", "--config"])
        .arg(fixture.root.join("aw.json"))
        .args(["--agent", "qoder", "--directory"])
        .arg(fixture.root.join("startup"));
    let result = fixture.run(remove);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("live owner"));
    fs::write(
        first_work.join("attach-command-7.json"),
        br#"{"action":"stop"}"#,
    )
    .unwrap();
    assert!(first.finish().status.success());
    second.signal(libc::SIGKILL);
    assert!(!second.finish().status.success());
    service.released(&fixture);
    setup(&fixture, true);
    assert_eq!(
        fs::read_dir(fixture.root.join("startup")).unwrap().count(),
        0
    );
    assert!(!fs::read_dir(fixture.root.join("state"))
        .unwrap()
        .any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("native-")));
}
