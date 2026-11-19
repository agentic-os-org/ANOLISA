//! Descriptor validation, staging and cleanup without host mount privileges.

use super::*;
use crate::{MountPlan, backend_tests::fixture};
use asc_sandbox_types::PathFact;
use std::os::unix::{fs::symlink, net::UnixListener};

#[test]
fn open_source_rejects_symlinks_at_every_level_and_parent_components() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("dir")).unwrap();
    File::create(root.path().join("dir/file")).unwrap();
    let link = root.path().join("link");
    symlink(root.path().join("dir"), &link).unwrap();
    for (path, reason) in [
        (link.clone(), "symlink bind source"),
        (link.join("file"), "without symlinks"),
        (root.path().join("dir/../dir"), "invalid bind path"),
    ] {
        let error = open_source(&path).unwrap_err();
        assert!(matches!(error, DockerError::Unsupported(s) if s.contains(reason)));
    }
}

#[test]
fn descriptor_must_match_all_bound_path_facts() {
    let fixture = fixture();
    let mut spec = fixture.spec();
    let source = spec.launch.cwd.clone();
    let metadata = open_source(Path::new(&source)).unwrap().metadata().unwrap();
    let fact = PathFact {
        path: source.clone(),
        canonical_path: source.clone(),
        device: metadata.dev(),
        inode: metadata.ino(),
        is_directory: true,
    };
    spec.path_facts.push(fact.clone());
    verify_source(&spec, &source, &metadata).unwrap();
    for changed in [
        PathFact {
            device: fact.device ^ 1,
            ..fact.clone()
        },
        PathFact {
            inode: fact.inode ^ 1,
            ..fact.clone()
        },
        PathFact {
            is_directory: false,
            ..fact.clone()
        },
        PathFact {
            canonical_path: "/other".into(),
            ..fact.clone()
        },
    ] {
        spec.path_facts[0] = changed;
        let error = verify_source(&spec, &source, &metadata).unwrap_err();
        assert!(
            matches!(error, DockerError::Unsupported(s) if s.contains("path identity changed"))
        );
    }
    let error = verify_source(&spec, "/unbound", &metadata).unwrap_err();
    assert!(matches!(error, DockerError::Unsupported(s) if s.contains("missing bound path")));
    let socket = Path::new(&source).join("socket");
    let _listener = UnixListener::bind(&socket).unwrap();
    let metadata = open_source(&socket).unwrap().metadata().unwrap();
    let error = verify_source(&spec, socket.to_str().unwrap(), &metadata).unwrap_err();
    assert!(matches!(error, DockerError::Unsupported(s) if s.contains("special bind source")));
}

#[test]
fn staging_rewrites_only_bind_sources_and_releases_its_directory() {
    let root = tempfile::tempdir().unwrap();
    let fixture = fixture();
    let spec = fixture.spec();
    let mut plan = ContainerPlan {
        create_args: vec!["--read-only".into()],
        mounts: vec![],
        network_mode: "none".into(),
    };
    for name in ["input", "output"] {
        let source = root.path().join(name);
        File::create(&source).unwrap();
        let mount = MountPlan {
            source: source.to_str().unwrap().into(),
            destination: format!("/work/{name}"),
            read_only: name == "input",
        };
        plan.create_args
            .extend(["--mount".into(), mount.argument()]);
        plan.mounts.push(mount);
    }
    let original = plan.clone();
    // A hard link gives the staged file the same identity without a mount namespace.
    let pinned = PinnedMounts::stage(&spec, &mut plan, root.path(), |_, _, name, target| {
        fs::remove_file(target)?;
        fs::hard_link(name, target)?;
        Ok(())
    })
    .unwrap();
    for (index, (mount, before)) in plan.mounts.iter().zip(&original.mounts).enumerate() {
        assert_ne!(mount.source, before.source);
        assert_eq!(mount.destination, before.destination);
        assert_eq!(mount.read_only, before.read_only);
        assert_eq!(plan.create_args[2 + index * 2], mount.argument());
    }
    drop(pinned);
    for (after, before) in plan.mounts.iter().zip(&original.mounts) {
        assert!(!Path::new(&after.source).exists());
        assert!(Path::new(&before.source).is_file());
    }
}

#[test]
fn wrong_staged_directory_identity_aborts_without_publishing() {
    let root = tempfile::tempdir().unwrap();
    let fixture = fixture();
    let spec = fixture.spec();
    let mut plan = crate::translate::translate(&spec, &fixture.config(), "test").unwrap();
    let original = plan.clone();
    let result = PinnedMounts::stage(&spec, &mut plan, root.path(), |_, _, _, target| {
        assert!(target.is_dir());
        Ok(())
    });
    assert!(matches!(result, Err(DockerError::Protocol(s)) if s.contains("identity mismatch")));
    assert_eq!(plan, original);
    assert_eq!(root.path().read_dir().unwrap().count(), 0);
}

#[test]
fn unconfirmed_unmount_retains_staging_for_recovery() {
    let root = tempfile::tempdir().unwrap();
    let directory = tempfile::tempdir_in(root.path()).unwrap();
    let marker = directory.path().join("0");
    File::create(&marker).unwrap();
    // An ordinary file cannot be unmounted, even by a privileged test runner.
    drop(PinnedMounts {
        directory: Some(directory),
        paths: vec![marker.clone()],
    });
    assert!(marker.is_file());
}
