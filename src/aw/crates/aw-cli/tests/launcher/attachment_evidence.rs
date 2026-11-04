//! External attachment preserves optional observation and owned cleanup.

use super::{install_entrypoint, native, success, Fixture, Service};
use aw_service::Operation;
use serde_json::{json, Value};
use std::{fs, time::Instant};

#[test]
fn external_attachment_succeeds_with_evidence_off() {
    check_evidence("off");
}

#[test]
fn external_attachment_reports_enabled_evidence() {
    check_evidence("auto");
}

fn check_evidence(mode: &str) {
    for entry in [&["agent", "connect"][..], &["install"][..]] {
        let fixture = Fixture::new();
        let mut document = fixture.document();
        fixture.observation(&mut document, "qoder");
        document["spec"]["agents"]["qoder"]["evidence"]["mode"] = json!(mode);
        let service = Service::start(&fixture, &document);
        let (_process, pid) = native(&fixture);
        let installed = success(install_entrypoint(&fixture, pid, false, entry));
        assert_eq!(installed["attached"], true);
        let status = service.status();
        assert_eq!(status["bindings"], 1);
        let observations = status["observations"].as_array().unwrap();
        if mode == "off" {
            assert!(observations.is_empty());
        } else {
            assert_eq!(observations.len(), 1);
            let observation = &observations[0];
            assert_eq!(observation["instance_id"], installed["instance_id"]);
            assert_eq!(observation["run"], json!({"state":"started", "pid":pid}));
            let audit = service
                .client
                .call(
                    Operation::Audit {
                        key: observation["audit_key"].as_str().unwrap().into(),
                    },
                    Instant::now() + super::TIMEOUT,
                )
                .unwrap();
            assert!(audit["records"]
                .as_array()
                .unwrap()
                .iter()
                .any(|record| record["record"]["phase"] == "run_reported"
                    && record["record"]["outcome"] == observation["run"]));
        }
        let ownership = fixture.root.join("home/.qoder/.aw-attachment.json");
        let record: Value = serde_json::from_slice(&fs::read(&ownership).unwrap()).unwrap();
        let artifacts = std::path::PathBuf::from(record["artifacts"].as_str().unwrap());
        success(install_entrypoint(&fixture, pid, true, entry));
        service.released(&fixture);
        assert_eq!(service.status()["observations"], json!([]));
        assert!(!ownership.exists());
        assert!(!artifacts.exists());
        assert_eq!(
            serde_json::from_slice::<Value>(
                &fs::read(fixture.root.join("home/.qoder/settings.json")).unwrap()
            )
            .unwrap(),
            json!({"theme":"keep"})
        );
        aw_service::ProcessIdentity::read(pid)
            .unwrap()
            .check()
            .unwrap();
    }
}
