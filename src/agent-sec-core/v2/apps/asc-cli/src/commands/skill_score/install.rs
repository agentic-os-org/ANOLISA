//! Route an explicit system install through the owner of the executing CLI.

use std::{ffi::OsStr, io, os::unix::process::CommandExt, path::Path, process::Command};

use serde_json::Value;

const RAW: &str = "/usr/local/bin/agent-sec-cli";
const RPM: &str = "/usr/bin/agent-sec-cli";
const OPTIONAL: &str = "sec-core-skill-score";
const RPM_ROOT: &str = "/usr/lib/agent-sec-core/skill-score";

fn error(message: &str) -> io::Error {
    io::Error::other(message)
}

fn command(program: &Path) -> Command {
    let mut cmd = Command::new(program);
    cmd.env_clear()
        .env(
            "PATH",
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
        )
        .env("HOME", "/root")
        .envs(std::env::vars_os().filter(|(key, _)| install_setting(key)));
    cmd
}

// Privileged helpers use the system CA store and root's local Docker configuration.
fn install_setting(key: &OsStr) -> bool {
    matches!(
        key.to_str(),
        Some(
            "HTTP_PROXY"
                | "HTTPS_PROXY"
                | "NO_PROXY"
                | "http_proxy"
                | "https_proxy"
                | "no_proxy"
                | "ANOLISA_SKILL_SCORE_SOURCE_DIR"
                | "ANOLISA_SKILL_SCORE_OFFLINE"
                | "ANOLISA_SKILL_SCORE_PYPI_INDEX"
                | "ANOLISA_SKILL_SCORE_DEPENDENCIES_OFFLINE"
                | "ANOLISA_SKILL_SCORE_BASE_IMAGE"
                | "ANOLISA_SKILL_SCORE_BASE_RECIPE"
                | "ANOLISA_SKILL_SCORE_BUILD_NETWORK"
                | "ANOLISA_SKILL_SCORE_STAGE_ONLY"
        )
    )
}

fn channel(executable: &Path, raw_present: bool, rpm_present: bool) -> io::Result<bool> {
    match (executable.to_str(), raw_present, rpm_present) {
        (_, true, true) => Err(error(
            "both /usr/local/bin/agent-sec-cli (raw) and /usr/bin/agent-sec-cli (RPM) are present; resolve the mixed installation before installing a runtime",
        )),
        (Some(RAW), true, false) => Ok(true),
        (Some(RPM), false, true) => Ok(false),
        _ => Err(error(
            "install-runtime requires a system-installed raw or RPM agent-sec-cli",
        )),
    }
}

fn record<'a>(status: &'a Value, name: &str) -> io::Result<Option<&'a Value>> {
    let rows = status
        .pointer("/data/components")
        .and_then(Value::as_array)
        .filter(|_| status["ok"] == true)
        .ok_or_else(|| error("invalid anolisa status response"))?;
    let mut matching = rows.iter().filter(|row| row["name"] == name);
    let first = matching.next();
    if matching.next().is_some()
        || status
            .pointer("/data/quarantined")
            .and_then(Value::as_array)
            .is_some_and(|rows| rows.iter().any(|row| row["name"] == name))
    {
        return Err(error(
            "ambiguous or quarantined component; reconcile anolisa state before installing",
        ));
    }
    Ok(first)
}

fn raw_version(row: &Value) -> io::Result<&str> {
    row["version"]
        .as_str()
        .filter(|version| {
            !version.is_empty()
                && row["scope"] == "system"
                && row["status"] == "installed"
                && row["rpm_package"].is_null()
        })
        .ok_or_else(|| error("expected an installed system raw component; check anolisa status"))
}

fn ensure_raw_recipe(manager: &Path, root: &Path) -> io::Result<()> {
    let output = command(manager)
        .args(["--install-mode", "system", "--json", "status"])
        .output()?;
    if !output.status.success() {
        return Err(error("anolisa could not verify the installed components"));
    }
    let status: Value = serde_json::from_slice(&output.stdout)?;
    let base =
        record(&status, "sec-core")?.ok_or_else(|| error("no raw sec-core installation record"))?;
    let version = raw_version(base)?;
    if let Some(optional) =
        record(&status, OPTIONAL)?.filter(|row| row["status"] != "not_installed")
    {
        if raw_version(optional)? != version {
            return Err(io::Error::other(format!(
                "Skill Score recipe does not match sec-core {version}; align both packages using anolisa update before retrying"
            )));
        }
        return Ok(());
    }
    if root.join("runtime-manifest.json").try_exists()? || root.join("prepare.sh").try_exists()? {
        return Err(error(
            "runtime files exist without a matching installation record",
        ));
    }

    // The system manager verifies the recipe from its configured raw repository.
    let installed = command(manager)
        .args([
            "--install-mode",
            "system",
            "install",
            OPTIONAL,
            "--backend",
            "raw",
            "--version",
            version,
        ])
        .status()?;
    if !installed.success() {
        return Err(io::Error::other(format!(
            "anolisa could not install {OPTIONAL} {version} ({installed})"
        )));
    }
    Ok(())
}

fn verify_rpm_recipe(root: &Path, rpm: &Path) -> io::Result<()> {
    for path in [
        Path::new(RPM),
        &root.join("prepare.sh"),
        &root.join("runtime-manifest.json"),
    ] {
        let output = command(rpm)
            .args(["-qf", "--qf", "%{NAME}"])
            .arg(path)
            .output()?;
        if !output.status.success() || output.stdout != b"agent-sec-core" {
            return Err(error(
                "rpmdb did not confirm that agent-sec-core owns the executing CLI and its matching runtime recipe",
            ));
        }
    }
    Ok(())
}

pub(super) fn install() -> io::Result<()> {
    if !rustix::process::geteuid().is_root() {
        return Err(error(
            "use sudo agent-sec-cli skillsec score install-runtime",
        ));
    }
    if std::env::consts::OS != "linux" || std::env::consts::ARCH != "x86_64" {
        return Err(error("the optional scoring runtime requires Linux x86_64"));
    }
    let executable = std::env::current_exe()?;
    let (root, prefix, cache) = if channel(
        &executable,
        Path::new(RAW).try_exists()?,
        Path::new(RPM).try_exists()?,
    )? {
        let manager = [
            Path::new("/usr/local/bin/anolisa"),
            Path::new("/usr/bin/anolisa"),
        ]
        .into_iter()
        .find(|p| p.is_file())
        .ok_or_else(|| error("install the anolisa system package manager"))?;
        let root = Path::new("/usr/local/lib/anolisa/sec-core-skill-score");
        ensure_raw_recipe(manager, root)?;
        (root, "/usr/local", "/var/cache/anolisa")
    } else {
        let root = Path::new(RPM_ROOT);
        verify_rpm_recipe(root, Path::new("/usr/bin/rpm"))?;
        (root, "/usr", "/var/cache/agent-sec-core")
    };
    let helper = root.join("prepare.sh");
    let failure = command(&helper)
        .args(["--prefix", prefix, "--cache-dir", cache])
        .exec();
    Err(io::Error::new(
        failure.kind(),
        format!("execute {}: {failure}", helper.display()),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn executable(path: &Path, contents: &str) {
        // A parallel test can fork while this thread writes. Let only the install child
        // hold a writable descriptor for the file that the test will execute.
        let source = path.with_extension("source");
        std::fs::write(&source, contents).unwrap();
        assert!(
            Command::new("install")
                .args(["-m", "0700"])
                .arg(source)
                .arg(path)
                .status()
                .unwrap()
                .success()
        );
    }

    #[test]
    fn privileged_helpers_ignore_caller_docker_and_ca_settings() {
        for key in [
            "SSL_CERT_FILE",
            "SSL_CERT_DIR",
            "DOCKER_HOST",
            "DOCKER_CONTEXT",
            "DOCKER_CONFIG",
            "DOCKER_TLS_VERIFY",
            "DOCKER_CERT_PATH",
        ] {
            assert!(!install_setting(OsStr::new(key)), "{key}");
        }
        for key in [
            "HTTPS_PROXY",
            "ANOLISA_SKILL_SCORE_OFFLINE",
            "ANOLISA_SKILL_SCORE_STAGE_ONLY",
        ] {
            assert!(install_setting(OsStr::new(key)), "{key}");
        }
    }

    #[test]
    fn mixed_or_unknown_system_installation_cannot_choose_a_writer() {
        for path in [RAW, RPM] {
            assert!(channel(Path::new(path), true, true).is_err());
        }
        assert!(channel(Path::new("/tmp/agent-sec-cli"), true, false).is_err());
        assert!(channel(Path::new(RAW), true, false).unwrap());
        assert!(!channel(Path::new(RPM), false, true).unwrap());
    }

    fn manager_status(manager: &Path, rows: &[&Value], extra: &str) {
        let response = json!({"ok":true,"data":{"components":rows}});
        executable(
            manager,
            &format!(
                "#!/bin/sh\nif [ \"$*\" = '--install-mode system --json status' ]; then\n  printf '%s\\n' '{response}'\nelse\n  {extra}\nfi\n"
            ),
        );
    }

    #[test]
    fn raw_installs_the_exact_preview_recipe_then_reuses_its_files_on_retry() {
        let temp = tempfile::tempdir().unwrap();
        let manager = temp.path().join("anolisa");
        let root = temp.path().join("optional");
        let log = temp.path().join("args");
        let version = "1.1.0-skillscore.0123456789ab";
        let base =
            json!({"name":"sec-core","scope":"system","status":"installed","version":version});
        let mut optional = base.clone();
        optional["name"] = json!(OPTIONAL);
        manager_status(
            &manager,
            &[&base],
            &format!("printf '%s\\n' \"$@\" > '{}'", log.display()),
        );
        ensure_raw_recipe(&manager, &root).unwrap();
        assert_eq!(
            std::fs::read_to_string(log)
                .unwrap()
                .lines()
                .collect::<Vec<_>>(),
            [
                "--install-mode",
                "system",
                "install",
                OPTIONAL,
                "--backend",
                "raw",
                "--version",
                version
            ]
        );
        manager_status(&manager, &[&base], "exit 9");
        assert!(
            ensure_raw_recipe(&manager, &root)
                .unwrap_err()
                .to_string()
                .contains(version)
        );
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("runtime-manifest.json"), "{}").unwrap();
        assert!(
            ensure_raw_recipe(&manager, &root)
                .unwrap_err()
                .to_string()
                .contains("without a matching installation record")
        );
        manager_status(&manager, &[&base, &optional], "exit 9");
        // A failed preparation leaves an owned recipe but no ready environment.
        ensure_raw_recipe(&manager, &root).unwrap();
        optional["version"] = json!("1.0.0");
        manager_status(&manager, &[&base, &optional], "exit 9");
        assert!(
            ensure_raw_recipe(&manager, &root)
                .unwrap_err()
                .to_string()
                .contains(version)
        );
        manager_status(&manager, &[&base, &base], "exit 9");
        assert!(
            ensure_raw_recipe(&manager, &root)
                .unwrap_err()
                .to_string()
                .contains("ambiguous")
        );
    }

    #[test]
    fn rpm_verifies_ownership_of_the_cli_and_recipe_before_preparation() {
        let temp = tempfile::tempdir().unwrap();
        let query = temp.path().join("rpm");
        executable(
            &query,
            "#!/bin/sh\ncase \"$4\" in */runtime-manifest.json) printf foreign;; *) printf agent-sec-core;; esac\n",
        );
        assert!(verify_rpm_recipe(temp.path(), &query).is_err());
        executable(&query, "#!/bin/sh\nprintf agent-sec-core\n");
        verify_rpm_recipe(temp.path(), &query).unwrap();
    }
}
