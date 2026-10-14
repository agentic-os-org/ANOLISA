//! Minimum-version Qoder launch contracts using the native-host fixture.

use super::{hooks, Fixture, Service};
use std::fs;

#[test]
fn qoder_versions_share_the_hook_contract_and_keep_actual_version_evidence() {
    for version in [
        "1.1.64".to_owned(),
        "1.1.66".to_owned(),
        "1.1.66-preview".to_owned(),
        "1.1.64+build.7".to_owned(),
        "1.1.100".to_owned(),
        format!("1.1.66+{}", "a".repeat(121)),
    ] {
        let fixture = Fixture::new();
        let mut document = fixture.document();
        fixture.provider(&mut document);
        let service = Service::start(&fixture, &document);
        for scenario in ["allow", "block"] {
            let mut command = fixture.launch(&["--run-id", scenario, "--scenario", scenario], None);
            command.env("FAKE_VERSION", &version);
            let report = fixture.successful(command);
            let blocked = scenario == "block";
            assert_eq!(report["tool_executed"], !blocked);
            assert_eq!(
                fixture.root.join(format!("tool-{scenario}")).exists(),
                !blocked
            );
            assert_eq!(
                hooks(&report, "PreToolUse")[0]["status"],
                if blocked { 2 } else { 0 }
            );
            if !blocked {
                assert_eq!(hooks(&report, "PostToolUse")[0]["status"], 0);
            }
            assert_eq!(
                fs::read_to_string(fixture.root.join("qoder-version-describe.txt")).unwrap(),
                version
            );
            assert_eq!(
                fs::read_to_string(fixture.root.join("qoder-version-invoke.txt")).unwrap(),
                version
            );
            service.released(&fixture);
        }
    }
}

#[test]
fn older_and_invalid_qoder_versions_are_rejected_before_binding() {
    let fixture = Fixture::new();
    let mut document = fixture.document();
    fixture.provider(&mut document);
    let service = Service::start(&fixture, &document);
    for (version, diagnostic) in [
        ("1.1.6", "1.1.64 or newer"),
        ("1.1.63", "1.1.64 or newer"),
        ("1.1.64-preview", "1.1.64 or newer"),
        ("1.1.64-preview+build.7", "1.1.64 or newer"),
        ("unknown", "expected a bare SemVer"),
        ("1.1", "expected a bare SemVer"),
        ("1.1.066", "expected a bare SemVer"),
        ("1.1.66+", "expected a bare SemVer"),
        ("v1.1.66", "expected a bare SemVer"),
        ("Qoder CLI 1.1.66", "expected a bare SemVer"),
        ("qodercli 1.1.66", "expected a bare SemVer"),
        ("1.1.66 (global)", "expected a bare SemVer"),
        ("1.1.66\ncommit: abc", "expected a bare SemVer"),
        (
            "Node.js 22.0.0 / Qoder CLI 1.1.63",
            "expected a bare SemVer",
        ),
    ] {
        let mut command = fixture.launch(&[], None);
        command.env("FAKE_VERSION", version);
        let output = fixture.run(command);
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(diagnostic), "{version:?}: {stderr}");
        if diagnostic == "expected a bare SemVer" {
            assert!(stderr.contains(&format!("got {version:?}")), "{stderr}");
        }
        assert_eq!(service.status()["bindings"], 0);
        assert!(!fixture.root.join("started-one").exists());
        assert!(!fixture.root.join("qoder-version-describe.txt").exists());
    }
    service.released(&fixture);
}

#[test]
fn oversized_qoder_versions_are_rejected_before_binding_or_provider_discovery() {
    let fixture = Fixture::new();
    let mut document = fixture.document();
    fixture.provider(&mut document);
    let service = Service::start(&fixture, &document);
    for version in ["v".repeat(129), "\u{00e9}".repeat(65)] {
        let mut command = fixture.launch(&[], None);
        command.env("FAKE_VERSION", version);
        let output = fixture.run(command);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("1..128 bytes"));
        assert_eq!(service.status()["bindings"], 0);
        assert!(!fixture.root.join("started-one").exists());
        assert!(!fixture.root.join("qoder-version-describe.txt").exists());
    }
    service.released(&fixture);
}
