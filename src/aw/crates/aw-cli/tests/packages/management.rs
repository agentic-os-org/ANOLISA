//! Read-only plans, independent packages and live runtime removal protection.

use super::{output::invoke, *};
use std::{
    collections::BTreeMap,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

fn report(command: &Command, code: i32) -> Value {
    let output = invoke(command);
    assert_eq!(
        output.status.code(),
        Some(code),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    serde_json::from_slice(&output.stdout).unwrap()
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, (u32, u64, Option<String>)> {
    let mut result = BTreeMap::new();
    for entry in fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        let metadata = fs::symlink_metadata(&path).unwrap();
        let bytes = if metadata.is_file() {
            Some(aw_package::digest(&path).unwrap())
        } else {
            None
        };
        result.insert(path.clone(), (metadata.mode(), metadata.ino(), bytes));
        if metadata.is_dir() {
            result.extend(snapshot(&path));
        }
    }
    result
}

#[test]
fn dry_run_is_repeatable_and_creates_neither_database_nor_prefix() {
    let f = Fixture::new();
    let before = snapshot(&f.root);
    let first = report(f.install().args(["--dry-run", "--json"]), 0);
    let second = report(f.install().args(["--dry-run", "--json"]), 0);
    assert_eq!(first, second);
    assert_eq!(first["result"]["kind"], "package_planned");
    assert_eq!(
        first["result"]["plan"]["added"][0]["package_id"],
        "external.provider"
    );
    assert_eq!(first["result"]["plan"]["installation_id"], Value::Null);
    assert_eq!(before, snapshot(&f.root));
    assert!(!f.prefix.exists());
    assert!(!f.root.join("package-state").exists());
}

fn generic(f: &Fixture, id: &str, dependencies: Value) -> PathBuf {
    let bundle = f.root.join(format!("bundle-{id}"));
    let name = format!("bin/{id}");
    let path = bundle.join("payload").join(&name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, format!("ordinary {id}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(bundle.join("package.json"), serde_json::to_vec(&json!({
        "schema_version":1,"package_id":id,"version":"1.2.3","os":"linux",
        "arch":std::env::consts::ARCH,"files":{name:{"sha256":aw_package::digest(&path).unwrap(),"mode":493}},
        "dependencies":dependencies,"provider":{"protocol":"aw-provider/v1alpha1"}
    })).unwrap()).unwrap();
    bundle
}

#[test]
fn generic_packages_support_selective_removal_exact_dependencies_and_reinstall() {
    let f = Fixture::new();
    let base = generic(&f, "example-base", json!([]));
    let tool = generic(
        &f,
        "example-tool",
        json!([{"package_id":"example-base","version":"1.2.3"}]),
    );
    let install = |bundle: &Path| {
        report(
            f.command()
                .args(["install", "--bundle"])
                .arg(bundle)
                .arg("--prefix")
                .arg(&f.prefix)
                .arg("--json"),
            0,
        )
    };
    install(&base);
    install(&tool);
    assert!(!f.prefix.join("bin/provider").exists());
    let before = snapshot(&f.root);
    let blocked = report(
        f.command()
            .args([
                "uninstall",
                "example-base",
                "--dry-run",
                "--json",
                "--prefix",
            ])
            .arg(&f.prefix),
        1,
    );
    assert_eq!(blocked["error"]["code"], "plan_blocked");
    assert!(blocked["result"]["plan"]["problems"]
        .as_array()
        .unwrap()
        .iter()
        .any(|problem| problem["code"] == "required_by"));
    assert_eq!(before, snapshot(&f.root));
    report(
        f.command()
            .args(["uninstall", "example-tool", "--yes", "--json", "--prefix"])
            .arg(&f.prefix),
        0,
    );
    assert!(f.prefix.join("bin/example-base").exists());
    assert!(!f.prefix.join("bin/example-tool").exists());
    fs::write(f.prefix.join("user.txt"), b"keep").unwrap();
    report(
        f.command()
            .args(["uninstall", "--yes", "--json", "--prefix"])
            .arg(&f.prefix),
        0,
    );
    let listed = report(
        f.command()
            .args(["list", "--installed", "--json", "--prefix"])
            .arg(&f.prefix),
        0,
    );
    assert!(
        listed["result"]["inventory"]["installations"][0]["packages"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(f.prefix.join(".local-packages/installation.json").is_file());
    install(&base);
    assert_eq!(fs::read(f.prefix.join("user.txt")).unwrap(), b"keep");
}

#[test]
fn ordinary_software_is_rejected_without_creating_an_aw_database() {
    let f = Fixture::new();
    let bundle = generic(&f, "ordinary-software", json!([]));
    let descriptor = bundle.join("package.json");
    let mut value: Value = serde_json::from_slice(&fs::read(&descriptor).unwrap()).unwrap();
    value.as_object_mut().unwrap().remove("provider");
    fs::write(&descriptor, serde_json::to_vec(&value).unwrap()).unwrap();
    let before = snapshot(&f.root);
    let output = report(
        f.command()
            .args(["install", "--bundle"])
            .arg(&bundle)
            .arg("--prefix")
            .arg(&f.prefix)
            .arg("--json"),
        1,
    );
    assert_eq!(output["error"]["code"], "package_contract");
    assert_eq!(before, snapshot(&f.root));
    assert!(!f.prefix.exists());
    assert!(!f.root.join("package-state").exists());
}

#[test]
fn prefix_queries_follow_registration_from_another_user_database_location() {
    let f = Fixture::new();
    success(f.install().output().unwrap());
    let foreign = f.root.join("uncreated-state");
    for command in [vec!["list", "--installed"], vec!["doctor"]] {
        let value = report(
            f.command()
                .env("XDG_STATE_HOME", &foreign)
                .args(command)
                .args(["--json", "--prefix"])
                .arg(&f.prefix),
            0,
        );
        assert_eq!(value["status"], "succeeded");
    }
    assert!(!foreign.exists());
    let before = snapshot(&f.root);
    let value = report(
        f.command()
            .args([
                "list",
                "--installed",
                "--scope",
                "system",
                "--json",
                "--prefix",
            ])
            .arg(&f.prefix),
        1,
    );
    assert_eq!(value["error"]["code"], "package_contract");
    assert_eq!(before, snapshot(&f.root));
}

#[test]
fn recover_on_an_absent_database_is_read_only_and_has_a_stable_report() {
    let f = Fixture::new();
    let before = snapshot(&f.root);
    let value = report(f.command().args(["recover", "--json"]), 0);
    assert_eq!(
        value["result"]["report"],
        json!({"committed":0,"rolled_back":0,"conflicts":[]})
    );
    assert_eq!(before, snapshot(&f.root));
}

#[test]
fn blocked_human_plans_only_write_stderr_and_json_retains_the_plan() {
    let f = Fixture::new();
    for installed in [false, true] {
        if installed {
            success(f.install().output().unwrap());
            fs::write(f.prefix.join("bin/provider"), b"changed").unwrap();
        }
        for execution in ["--dry-run", "--yes"] {
            for quiet in [false, true] {
                let mut command = f.command();
                command
                    .args(["uninstall", execution, "--prefix"])
                    .arg(&f.prefix);
                if quiet {
                    command.arg("--quiet");
                }
                let output = invoke(&command);
                assert_eq!(output.status.code(), Some(1));
                assert!(
                    output.stdout.is_empty(),
                    "{}",
                    String::from_utf8_lossy(&output.stdout)
                );
                assert!(
                    String::from_utf8_lossy(&output.stderr).contains(if installed {
                        "file_changed"
                    } else {
                        "not_installed"
                    })
                );
            }
            let output = report(
                f.command()
                    .args(["uninstall", execution, "--json", "--prefix"])
                    .arg(&f.prefix),
                1,
            );
            assert_eq!(output["error"]["code"], "plan_blocked");
            assert_eq!(output["result"]["kind"], "package_planned");
        }
    }
}

#[test]
fn keep_changed_previews_and_reports_paths_without_deleting_them() {
    let f = Fixture::new();
    success(f.install().output().unwrap());
    let changed = f.prefix.join("bin/provider");
    fs::write(&changed, b"user changes").unwrap();
    let before = snapshot(&f.root);
    let preview = report(
        f.command()
            .args([
                "uninstall",
                "--keep-changed",
                "--dry-run",
                "--json",
                "--prefix",
            ])
            .arg(&f.prefix),
        0,
    );
    assert_eq!(
        preview["result"]["plan"]["preserved"][0]["path"],
        "bin/provider"
    );
    assert_eq!(before, snapshot(&f.root));
    let unconfirmed = report(
        f.command()
            .args(["uninstall", "--keep-changed", "--json", "--prefix"])
            .arg(&f.prefix),
        1,
    );
    assert_eq!(unconfirmed["error"]["code"], "confirmation_required");
    assert_eq!(before, snapshot(&f.root));
    let result = report(
        f.command()
            .args(["uninstall", "--keep-changed", "--yes", "--json", "--prefix"])
            .arg(&f.prefix),
        0,
    );
    assert_eq!(result["result"]["kind"], "uninstalled");
    assert_eq!(
        result["result"]["preserved"],
        preview["result"]["plan"]["preserved"]
    );
    assert_eq!(fs::read(changed).unwrap(), b"user changes");
}

#[test]
fn keep_changed_recovers_a_deleted_prefix_and_allows_reinstallation() {
    let f = Fixture::new();
    success(f.install().output().unwrap());
    fs::remove_dir_all(&f.prefix).unwrap();
    let result = report(
        f.command()
            .args(["uninstall", "--keep-changed", "--yes", "--json", "--prefix"])
            .arg(&f.prefix),
        0,
    );
    assert_eq!(result["result"]["kind"], "uninstalled");
    assert!(!result["result"]["preserved"].as_array().unwrap().is_empty());
    success(f.install().output().unwrap());
    assert!(f.prefix.join("bin/provider").is_file());
}
