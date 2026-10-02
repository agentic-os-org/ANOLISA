//! Opt-in lifecycle verification against disposable containers created by rpm-lifecycle.sh.

use anolisa_platform::command::{CommandOutput, CommandRunner};
use anolisa_platform::pkg_query::PackageQuery;
use anolisa_platform::pkg_transaction::PackageTransaction;
use anolisa_platform::rpm_query::RpmPackageQuery;
use anolisa_platform::rpm_repo::RpmRepoSource;
use anolisa_platform::rpm_tool::{RpmDialect, RpmTool};
use anolisa_platform::rpm_transaction::RpmTransaction;
use std::process::Command;

struct Container(String);
impl CommandRunner for Container {
    fn run(&self, program: &str, args: &[&str]) -> std::io::Result<CommandOutput> {
        // Only the generated temporary config crosses the container boundary.
        if matches!(program, "yum" | "dnf")
            && let Some(position) = args.iter().position(|arg| *arg == "-c")
        {
            let path = args[position + 1];
            let output = Command::new("docker")
                .args(["cp", path, &format!("{}:{path}", self.0)])
                .output()?;
            if !output.status.success() {
                return Err(std::io::Error::other(
                    String::from_utf8_lossy(&output.stderr).into_owned(),
                ));
            }
        }
        let output = Command::new("docker")
            .args(["exec", "-e", "LC_ALL=C", &self.0, program])
            .args(args)
            .output()?;
        eprintln!(
            "{program} {args:?}: exit {:?}\n{}{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(CommandOutput {
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

#[test]
#[ignore = "requires a disposable fixture container; run tests/rpm-lifecycle.sh"]
fn native_component_lifecycle() {
    let name = std::env::var("ANOLISA_RPM_TEST_CONTAINER").expect("fixture container name");
    let label = Command::new("docker")
        .args([
            "inspect",
            "--format",
            "{{ index .Config.Labels \"anolisa.rpm-test\" }}",
            &name,
        ])
        .output()
        .unwrap();
    assert!(label.status.success());
    assert_eq!(String::from_utf8_lossy(&label.stdout).trim(), "3311");
    let runner = Container(name.clone());
    let tool = RpmTool::detect(&runner).unwrap();
    let query = RpmPackageQuery::with_runner(Container(name.clone()));
    let transaction = RpmTransaction::with_tool(
        Container(name.clone()),
        tool,
        Some(RpmRepoSource::new(
            "anolisa-fixture",
            std::env::var("ANOLISA_RPM_TEST_REPO").expect("shared fixture repository"),
            Some(false),
        )),
    );
    transaction
        .check_install(&["anolisa-probe-app-1-1.noarch"])
        .unwrap();
    assert!(
        query
            .query_installed("anolisa-probe-app")
            .unwrap()
            .is_none()
    );
    let missing = transaction.check_install(&["anolisa-probe-app", "anolisa-absent-3311"]);
    assert!(
        missing.is_err(),
        "partial targets were accepted: {missing:?}"
    );
    let config = match tool.dialect {
        RpmDialect::Yum => "/etc/yum.conf",
        RpmDialect::Dnf => "/etc/dnf/dnf.conf",
    };
    let change_config = |script: &str| {
        let result = runner
            .run("sh", &["-c", script, "fixture", config])
            .unwrap();
        assert_eq!(result.code, Some(0), "{result:?}");
    };
    change_config(
        "cp \"$1\" /tmp/site-backup.conf; printf '\\nexclude=anolisa-probe-app\\n' >> \"$1\"",
    );
    assert!(transaction.check_install(&["anolisa-probe-app"]).is_err());
    change_config(
        "cp /tmp/site-backup.conf \"$1\"; mkdir -p /tmp/no-repos; sed -i 's@/probe/repos@/tmp/no-repos@' \"$1\"",
    );
    assert!(transaction.check_install(&["anolisa-probe-app"]).is_err());
    change_config("cp /tmp/site-backup.conf \"$1\"");
    transaction
        .install(&["anolisa-probe-app-1-1.noarch"])
        .unwrap();
    assert_eq!(
        query
            .query_installed("anolisa-probe-app")
            .unwrap()
            .unwrap()
            .version
            .version,
        "1"
    );
    assert!(
        query
            .query_installed("anolisa-probe-dep")
            .unwrap()
            .is_some()
    );
    if tool.dialect == RpmDialect::Yum {
        change_config("printf '\\nexclude=anolisa-probe-app\\n' >> \"$1\"");
        assert!(transaction.update(&["anolisa-probe-app"]).is_err());
        assert_eq!(
            query
                .query_installed("anolisa-probe-app")
                .unwrap()
                .unwrap()
                .version
                .version,
            "1"
        );
        change_config("cp /tmp/site-backup.conf \"$1\"");
    }
    transaction.update(&["anolisa-probe-app"]).unwrap();
    assert_eq!(
        query
            .query_installed("anolisa-probe-app")
            .unwrap()
            .unwrap()
            .version
            .version,
        "2"
    );
    // Install another target from the host repository, then repair both together.
    let result = runner
        .run(tool.program, &["-y", "install", "anolisa-probe-other"])
        .unwrap();
    assert_eq!(result.code, Some(0), "{result:?}");
    let result = runner
        .run(
            "rm",
            &[
                "/usr/share/anolisa-probe-app/payload",
                "/usr/share/anolisa-probe-other/payload",
            ],
        )
        .unwrap();
    assert_eq!(result.code, Some(0), "{result:?}");
    transaction
        .reinstall(&["anolisa-probe-app", "anolisa-probe-other"])
        .unwrap();
    for package in ["anolisa-probe-app", "anolisa-probe-other"] {
        assert_eq!(
            runner
                .run("test", &["-f", &format!("/usr/share/{package}/payload")])
                .unwrap()
                .code,
            Some(0)
        );
    }
    assert_eq!(
        query
            .query_installed("anolisa-probe-app")
            .unwrap()
            .unwrap()
            .version
            .version,
        "2"
    );
    // An external upgrade can put an adopted target ahead of the component repo.
    transaction
        .update(&["anolisa-probe-app", "anolisa-probe-other"])
        .unwrap();
    let result = runner
        .run(tool.program, &["-y", "update", "anolisa-probe-app"])
        .unwrap();
    assert_eq!(result.code, Some(0), "{result:?}");
    assert_eq!(
        query
            .query_installed("anolisa-probe-app")
            .unwrap()
            .unwrap()
            .version
            .version,
        "9"
    );
    transaction.update(&["anolisa-probe-app"]).unwrap();
    transaction
        .update(&["anolisa-probe-app", "anolisa-probe-other"])
        .unwrap();
    assert_eq!(
        query
            .query_installed("anolisa-probe-app")
            .unwrap()
            .unwrap()
            .version
            .version,
        "9"
    );
    let offline = RpmTransaction::with_tool(
        Container(name.clone()),
        tool,
        Some(RpmRepoSource::new(
            "missing",
            "file:///absent-component-repo",
            None,
        )),
    );
    offline
        .remove(&["anolisa-probe-app", "anolisa-probe-other"])
        .unwrap();
    assert!(
        query
            .query_installed("anolisa-probe-app")
            .unwrap()
            .is_none()
    );
    assert!(
        query
            .query_installed("anolisa-probe-other")
            .unwrap()
            .is_none()
    );
    println!(
        "verified {} {:?}: preflight, pinned install, cross-repo dependency, update, mixed-origin repair, remove",
        tool.program, tool.dialect
    );
}
