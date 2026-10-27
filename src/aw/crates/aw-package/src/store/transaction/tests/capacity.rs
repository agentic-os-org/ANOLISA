//! Compare admission counts with real preparation, including historical metadata changes.

use super::*;

fn add_legacy_provider(fixture: &Fixture, changes: &mut ChangeSet) {
    let mut files = BTreeMap::new();
    for name in ["aw-provider-tokenless", "tokenless", "rtk"] {
        let relative = format!("libexec/aw/providers/tokenless/{name}");
        let path = fixture.prefix.join(&relative);
        fixture.record(&path, b"old tool\n", 0o755);
        files.insert(relative, fs::bytes_payload(b"old tool\n", 0o755));
    }
    let component = Component {
        format: 1,
        component: "aw-provider-tokenless".into(),
        version: "old-build".into(),
        source_commit: "historical-source".into(),
        os: "linux".into(),
        arch: std::env::consts::ARCH.into(),
        provider_protocol: "aw-provider/v1alpha1".into(),
        requires: BTreeMap::from([("aw-core".into(), "old-build".into())]),
        files,
    };
    crate::database::validate_legacy_component(&component).unwrap();
    let bytes = serde_json::to_vec_pretty(&component).unwrap();
    fixture.record(
        &fixture
            .prefix
            .join(".aw-packages/aw-provider-tokenless.json"),
        &bytes,
        0o644,
    );
    changes
        .legacy
        .insert("aw-provider-tokenless.json".into(), bytes);
    changes.verify.extend(component.files.clone());
    changes.database_after.installations[0]
        .packages
        .push(InstalledPackage {
            package_id: component.component,
            version: component.version,
            os: component.os,
            arch: component.arch,
            files: component.files,
            dependencies: vec![crate::database::InstalledDependency {
                installation_id: changes.installation_id.clone(),
                package_id: "aw-core".into(),
                version: "old-build".into(),
            }],
            source: SourceEvidence {
                bundle_sha256: None,
                source_commit: Some(component.source_commit),
            },
            provider: Some(crate::description::ProviderDeclaration {
                protocol: component.provider_protocol,
            }),
        });
}

#[test]
fn action_capacity_matches_preparation_for_locator_and_legacy_record_changes() {
    for legacy in [false, true] {
        for remove_locator in [false, true] {
            if remove_locator && !legacy {
                continue;
            }
            let fixture = Fixture::new();
            let mut changes = fixture.changes(legacy);
            if legacy {
                add_legacy_provider(&fixture, &mut changes);
            }
            if remove_locator {
                changes.writes.clear();
                changes.removes = changes.verify.clone();
                changes.database_after.installations[0].packages.clear();
                changes.marker_after = None;
            }
            let count = super::super::capacity::actions(&fixture.paths, &changes).unwrap();
            let expected = if remove_locator {
                7
            } else if legacy {
                3
            } else {
                2
            };
            assert_eq!(count, expected);
            let prepared = prepare::begin(&fixture.paths, &changes).unwrap();
            assert_eq!(count, prepared.journal.actions.len());
            prepared
                .journal
                .validate(&fixture.paths, &prepared.journal.directory())
                .unwrap();
        }
    }
}

#[test]
fn journal_record_guards_reject_action_and_directory_overflow_before_publication() {
    let fixture = Fixture::new();
    let changes = fixture.changes(false);
    let prepared = prepare::begin(&fixture.paths, &changes).unwrap();
    let mut entry = prepared.journal.clone();
    let action = entry.actions[0].clone();
    entry.actions = vec![action; super::super::capacity::MAX_ACTIONS + 1];
    assert!(entry
        .validate(&fixture.paths, &entry.directory())
        .unwrap_err()
        .to_string()
        .contains("record limits"));
    let mut entry = prepared.journal.clone();
    let identity = fs::directory_identity(&fixture.root).unwrap();
    for index in 0..=super::super::capacity::MAX_DIRECTORIES {
        entry
            .directories
            .insert(format!("share/directory-{index}"), identity.clone());
    }
    assert!(entry
        .validate(&fixture.paths, &entry.directory())
        .unwrap_err()
        .to_string()
        .contains("record limits"));
    entry.directories.remove(&format!(
        "share/directory-{}",
        super::super::capacity::MAX_DIRECTORIES
    ));
    let before = disk::read(entry.directory().join("journal.json")).unwrap();
    fixture.directory(&fixture.prefix);
    let error = prepare::parents(&mut entry, Path::new("unexpected/leaf")).unwrap_err();
    assert!(error.to_string().contains("directory capacity"));
    assert!(!fixture.prefix.join("unexpected").exists());
    assert_eq!(
        entry.directories.len(),
        super::super::capacity::MAX_DIRECTORIES
    );
    assert_eq!(
        disk::read(entry.directory().join("journal.json")).unwrap(),
        before
    );
    journal::read(&fixture.paths, &entry.directory()).unwrap();
}
