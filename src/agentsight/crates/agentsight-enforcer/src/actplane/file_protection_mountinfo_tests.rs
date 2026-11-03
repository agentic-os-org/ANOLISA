use super::*;

fn mount(id: u32, parent_id: u32, root: &str, mountpoint: &str, fs_type: &str) -> MountInfo {
    MountInfo {
        id,
        parent_id,
        device: (8, 1),
        root: root.into(),
        mountpoint: mountpoint.into(),
        fs_type: fs_type.into(),
    }
}

#[test]
fn mountinfo_decodes_escapes_and_rejects_malformed_duplicate_and_bounds() {
    let parsed = parse_mountinfo(
        b"1 0 8:1 / / rw - ext4 /dev/sda rw\n2 1 8:1 /a\\040b /mnt\\134x rw - ext4 /dev/sda rw\n",
    )
    .expect("parse escaped mount paths");
    assert_eq!(parsed[1].root, "/a b");
    assert_eq!(parsed[1].mountpoint, "/mnt\\x");

    for malformed in [
        "1 0 8:1 / / rw ext4 /dev/sda rw\n",
        "1 0 8:1 / / rw - ext4 /dev/sda rw\n1 0 8:1 / /x rw - ext4 /dev/sda rw\n",
        "1 0 8:1 / / rw - ext4 /dev/sda rw\n2 99 8:1 / /x rw - ext4 /dev/sda rw\n",
        "1 0 8:1 /bad\\999 / rw - ext4 /dev/sda rw\n",
    ] {
        assert!(
            parse_mountinfo(malformed.as_bytes()).is_err(),
            "{malformed:?}"
        );
    }
    let oversized = format!(
        "1 0 8:1 / /{} rw - ext4 /dev/sda rw\n",
        "x".repeat(MAX_MOUNTINFO_LINE_BYTES)
    );
    assert!(parse_mountinfo(oversized.as_bytes()).is_err());
}

#[test]
fn mount_relations_derive_underlying_and_visible_paths() {
    let root_mount = mount(1, 0, "/", "/", "ext4");
    let bind_mount = mount(2, 1, "/outside/source", "/tree/mounted", "ext4");

    assert_eq!(
        underlying_mount_path(
            &[root_mount.clone(), mount(3, 1, "/base", "/mnt", "ext4")],
            "/mnt/tree",
        )
        .expect("derive filesystem-relative target"),
        "/base/tree"
    );
    assert_eq!(
        visible_paths_for_mount_root(&[root_mount, bind_mount.clone()], &bind_mount),
        vec!["/outside/source"]
    );
}

#[test]
fn visible_candidates_deduplicate_nonadjacent_equal_length_paths() {
    let mounted = mount(4, 1, "/sub", "/mounted", "ext4");
    let mounts = [
        mount(1, 0, "/", "/alias", "ext4"),
        mount(2, 1, "/", "/aliaz", "ext4"),
        mount(3, 1, "/", "/alias", "ext4"),
        mounted.clone(),
    ];
    assert_eq!(
        visible_paths_for_mount_root(&mounts, &mounted),
        vec!["/alias/sub", "/aliaz/sub"]
    );
}

#[test]
fn reading_mountinfo_reports_current_process_mounts() {
    let pid = i32::try_from(std::process::id()).expect("PID fits i32");
    let (mounts, digest) = read_mountinfo(pid).expect("read own mountinfo");

    assert!(mounts.iter().any(|mount| mount.mountpoint == "/"));
    assert_ne!(digest, 0);
    // An absent process must fail instead of reporting an empty topology.
    assert!(read_mountinfo(-1).is_err());
}
