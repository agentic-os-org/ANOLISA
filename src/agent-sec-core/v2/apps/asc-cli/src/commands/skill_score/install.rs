//! Route an explicit system install through the owner of the executing CLI.

use std::{io, os::unix::process::CommandExt, path::Path, process::Command};

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
        .env("HOME", "/root");
    // Only inputs already accepted by the preparation transaction cross the root boundary.
    for key in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "NO_PROXY",
        "http_proxy",
        "https_proxy",
        "no_proxy",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
        "DOCKER_HOST",
        "DOCKER_CONTEXT",
        "DOCKER_CONFIG",
        "DOCKER_TLS_VERIFY",
        "DOCKER_CERT_PATH",
        "ANOLISA_SKILL_SCORE_SOURCE_DIR",
        "ANOLISA_SKILL_SCORE_OFFLINE",
        "ANOLISA_SKILL_SCORE_PYPI_INDEX",
        "ANOLISA_SKILL_SCORE_DEPENDENCIES_OFFLINE",
        "ANOLISA_SKILL_SCORE_BASE_IMAGE",
        "ANOLISA_SKILL_SCORE_BASE_RECIPE",
        "ANOLISA_SKILL_SCORE_BUILD_NETWORK",
        "ANOLISA_SKILL_SCORE_STAGE_ONLY",
    ] {
        if let Some(value) = std::env::var_os(key) {
            cmd.env(key, value);
        }
    }
    cmd
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

fn raw_identity(row: &Value) -> io::Result<(&str, &str)> {
    if row["scope"] == "system"
        && row["status"] == "installed"
        && row["rpm_package"].is_null()
        && let Some((repository, version)) = row["raw_repository_sha256"]
            .as_str()
            .zip(row["version"].as_str())
        && repository.len() == 64
        && repository.bytes().all(|b| b.is_ascii_hexdigit())
        && !version.is_empty()
    {
        return Ok((repository, version));
    }
    Err(error(
        "cannot confirm a healthy system raw component and its original repository; check anolisa status or use the matched release installer",
    ))
}

fn raw_install(manager: &Path, optional_root: &Path) -> io::Result<Command> {
    let output = command(manager)
        .args(["--install-mode", "system", "--json", "status"])
        .output()?;
    if !output.status.success() {
        return Err(error("anolisa could not verify the installed components"));
    }
    let status: Value = serde_json::from_slice(&output.stdout)?;
    let base =
        record(&status, "sec-core")?.ok_or_else(|| error("no raw sec-core installation record"))?;
    if base["scope"] == "system"
        && base["status"] == "installed"
        && base["rpm_package"].is_null()
        && base["raw_repository_sha256"].is_null()
    {
        return Err(error(
            "missing original raw repository: use anolisa >= 0.3.22 and verify the original repository and exact base version. A repair that replays sec-core records its source; if repair reports nothing to repair, install the optional component explicitly with the verified --repo and --version",
        ));
    }
    let identity = raw_identity(base)?;
    if let Some(optional) =
        record(&status, OPTIONAL)?.filter(|row| row["status"] != "not_installed")
    {
        if raw_identity(optional)? != identity {
            return Err(error(
                "installed runtime ownership, source or version differs from sec-core; reconcile with anolisa before retrying",
            ));
        }
        let mut cmd = command(&optional_root.join("prepare.sh"));
        // Only deployment-preloaded or explicitly staged files may progress beyond
        // verification here. Python validates their release binding before activation.
        if optional_root.join("preload").symlink_metadata().is_ok()
            || std::env::var("ANOLISA_SKILL_SCORE_STAGE_ONLY").as_deref() == Ok("1")
        {
            cmd.args([
                "--prefix",
                "/usr/local",
                "--cache-dir",
                "/var/cache/anolisa",
            ]);
        } else {
            cmd.args(["--check", "--prefix", "/usr/local"]);
        }
        return Ok(cmd);
    }
    if optional_root.join("runtime-manifest.json").try_exists()? {
        return Err(error(
            "runtime files exist without a matching installation record",
        ));
    }
    let mut cmd = command(manager);
    cmd.args([
        "--install-mode",
        "system",
        "install",
        OPTIONAL,
        "--match-installed",
        "sec-core",
    ]);
    Ok(cmd)
}

fn rpm_install(root: &Path, rpm: &Path) -> io::Result<Command> {
    let prepare = root.join("prepare.sh");
    let manifest = root.join("runtime-manifest.json");
    for path in [Path::new(RPM), &prepare, &manifest] {
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
    let mut cmd = command(&prepare);
    cmd.args([
        "--prefix",
        "/usr",
        "--cache-dir",
        "/var/cache/agent-sec-core",
    ]);
    Ok(cmd)
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
    let mut cmd = if channel(
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
        .ok_or_else(|| error("install the release-matched anolisa system installer"))?;
        raw_install(
            manager,
            Path::new("/usr/local/lib/anolisa/sec-core-skill-score"),
        )?
    } else {
        rpm_install(Path::new(RPM_ROOT), Path::new("/usr/bin/rpm"))?
    };
    Err(cmd.exec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::os::unix::fs::PermissionsExt;

    fn executable(path: &Path, contents: &str) {
        // Replacing a recently executed shell stub in place can race Linux's executable lease.
        let replacement = path.with_extension("new");
        std::fs::write(&replacement, contents).unwrap();
        std::fs::set_permissions(&replacement, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::rename(replacement, path).unwrap();
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

    #[test]
    fn raw_delegates_exact_repository_and_version_then_only_checks_owned_runtime() {
        let temp = tempfile::tempdir().unwrap();
        let manager = temp.path().join("anolisa");
        let root = temp.path().join("optional");
        let base = json!({"name":"sec-core","scope":"system","status":"installed","version":"1.1.0-skillscore.0123456789ab","raw_repository_sha256":"a".repeat(64)});
        let mut optional = base.clone();
        optional["name"] = json!(OPTIONAL);
        let status = |rows: Vec<Value>| {
            let result = json!({"ok":true,"data":{"components":rows}});
            executable(
                &manager,
                &format!(
                    "#!/bin/sh\n[ \"$*\" = '--install-mode system --json status' ] || exit 2\nprintf '%s\\n' '{result}'\n"
                ),
            );
        };
        let mut legacy = base.clone();
        legacy
            .as_object_mut()
            .unwrap()
            .remove("raw_repository_sha256");
        status(vec![legacy]);
        assert!(
            raw_install(&manager, &root)
                .unwrap_err()
                .to_string()
                .contains("verified --repo and --version")
        );
        status(vec![base.clone()]);
        let cmd = raw_install(&manager, &root).unwrap();
        assert_eq!(cmd.get_program(), manager.as_os_str());
        assert_eq!(
            cmd.get_args().collect::<Vec<_>>(),
            [
                "--install-mode",
                "system",
                "install",
                OPTIONAL,
                "--match-installed",
                "sec-core"
            ]
        );
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("runtime-manifest.json"), "{}").unwrap();
        assert!(raw_install(&manager, &root).is_err());
        status(vec![base.clone(), optional.clone()]);
        let cmd = raw_install(&manager, &root).unwrap();
        assert_eq!(cmd.get_program(), root.join("prepare.sh"));
        assert_eq!(
            cmd.get_args().collect::<Vec<_>>(),
            ["--check", "--prefix", "/usr/local"]
        );
        std::fs::create_dir(root.join("preload")).unwrap();
        let cmd = raw_install(&manager, &root).unwrap();
        assert_eq!(cmd.get_program(), root.join("prepare.sh"));
        assert_eq!(
            cmd.get_args().collect::<Vec<_>>(),
            [
                "--prefix",
                "/usr/local",
                "--cache-dir",
                "/var/cache/anolisa"
            ]
        );
        optional["version"] = json!("1.0.0");
        status(vec![base.clone(), optional]);
        assert!(raw_install(&manager, &root).is_err());
        status(vec![
            json!({"name":"sec-core","scope":"system","status":"adopted","rpm_package":"agent-sec-cli"}),
        ]);
        assert!(raw_install(&manager, &root).is_err());
    }

    #[test]
    fn rpm_requires_native_ownership_and_delegates_to_the_idempotent_preparer() {
        let temp = tempfile::tempdir().unwrap();
        let query = temp.path().join("rpm");
        executable(
            &query,
            "#!/bin/sh\ncase \"$4\" in */runtime-manifest.json) printf foreign;; *) printf agent-sec-core;; esac\n",
        );
        assert!(rpm_install(temp.path(), &query).is_err());
        executable(&query, "#!/bin/sh\nprintf agent-sec-core\n");
        for _ in 0..2 {
            let cmd = rpm_install(temp.path(), &query).unwrap();
            assert_eq!(cmd.get_program(), temp.path().join("prepare.sh"));
            assert_eq!(
                cmd.get_args().collect::<Vec<_>>(),
                [
                    "--prefix",
                    "/usr",
                    "--cache-dir",
                    "/var/cache/agent-sec-core"
                ]
            );
        }
    }
}
