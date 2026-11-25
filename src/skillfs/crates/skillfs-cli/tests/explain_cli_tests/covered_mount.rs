//! Verify the visible directory when another mount covers an active mount's parent.

use std::fs;
use std::path::Path;

use serde_json::json;

use super::{
    StopMount, VALID, config_at, live_fuse_available, runtime_report_at, skill, start_mount,
};

#[test]
fn runtime_report_rejects_a_mount_hidden_by_its_parent() {
    if !live_fuse_available() {
        return;
    }

    let temp = tempfile::tempdir_in("/tmp").expect("fixture");
    let root = temp.path();
    let parent = root.join("view");
    let nested = parent.join("skills/demo");
    fs::create_dir_all(&nested).expect("create nested mountpoint");

    // Keep the sources and configuration reachable while the parent covers the
    // nested directory. The two workers share the same managed runtime directory.
    let mount_config = |name: &str, mount: &Path| {
        let dir = root.join(name);
        let sources = [dir.join("first"), dir.join("second")];
        for source in &sources {
            skill(source, "demo", &format!("{VALID}{name}\n"));
        }
        config_at(&dir, mount, &sources)
    };
    let nested_config = mount_config("nested", &nested);
    let parent_config = mount_config("parent", &parent);

    let _stop_nested = StopMount(root, &nested);
    start_mount(root, &nested_config);
    let initial = runtime_report_at(root, &nested, "demo", 0);
    assert_eq!(initial["status"], "available");
    assert_eq!(
        initial["selected"]["source"],
        json!(root.join("nested/first"))
    );

    {
        let _stop_parent = StopMount(root, &parent);
        start_mount(root, &parent_config);
        // This pathname now names the parent's skill directory; the nested
        // worker is still alive and has a matching entry in mountinfo.
        assert_eq!(
            fs::read_to_string(nested.join("SKILL.md"))
                .expect("read visible skill")
                .lines()
                .next_back(),
            Some("parent")
        );
        let hidden = runtime_report_at(root, &nested, "demo", 1);
        assert_eq!(hidden["status"], "unavailable");
        assert!(hidden.get("selected").is_none());
    }

    assert_eq!(runtime_report_at(root, &nested, "demo", 0), initial);
    assert_eq!(
        fs::read_to_string(nested.join("skills/demo/SKILL.md"))
            .expect("read uncovered skill")
            .lines()
            .next_back(),
        Some("nested")
    );
}
