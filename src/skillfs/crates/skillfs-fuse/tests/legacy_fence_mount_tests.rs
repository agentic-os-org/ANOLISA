//! Mount-side behavior for a legacy-fence skill (frontmatter closed by
//! a `----` thematic break).
//!
//! After the closer-rule fix the skill stays discoverable through a
//! live mount AND serves its real metadata: /skills lists it, its
//! SKILL.md is readable, and skill-discover renders the true
//! description instead of the smeared raw yaml block the degraded
//! fallback used to serve. Hermes (categorized) layout behaves the
//! same.

mod common;

use common::{MountFixture, create_skill_dir};

const LEGACY_CLOSER: &str = "---\nname: legacy-closer\ndescription: used to parse\n----\nBody.\n";

fn seed_flat(src: &std::path::Path) {
    create_skill_dir(src, "fine");
    std::fs::create_dir_all(src.join("legacy-closer")).expect("mkdir legacy-closer");
    std::fs::write(src.join("legacy-closer/SKILL.md"), LEGACY_CLOSER)
        .expect("write legacy SKILL.md");
}

fn seed_hermes(src: &std::path::Path) {
    std::fs::create_dir_all(src.join("catalog/legacy-closer")).expect("mkdir nested skill");
    std::fs::write(src.join("catalog/legacy-closer/SKILL.md"), LEGACY_CLOSER)
        .expect("write nested legacy SKILL.md");
}

#[test]
fn legacy_fence_skill_serves_real_metadata_flat_mount() {
    if !common::fuse_available() {
        eprintln!("SKIP legacy_fence_skill_serves_real_metadata_flat_mount: FUSE not available");
        return;
    }
    let fx = MountFixture::normal(seed_flat);

    let entries: Vec<String> = std::fs::read_dir(fx.skills_root())
        .expect("readdir /skills")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        entries.iter().any(|n| n == "legacy-closer"),
        "legacy-fence skill must stay listed in /skills: {entries:?}"
    );

    let body = std::fs::read_to_string(fx.skill_path("legacy-closer").join("SKILL.md"))
        .expect("read legacy SKILL.md through mount");
    assert!(
        body.contains("Body."),
        "served content keeps the body: {body:?}"
    );

    let discover = std::fs::read_to_string(fx.skill_path("skill-discover").join("SKILL.md"))
        .expect("read discover manifest");
    assert!(
        discover.contains("legacy-closer"),
        "discover must still surface the legacy-fence skill: {discover}"
    );
    assert!(
        discover.contains("| legacy-closer | used to parse |"),
        "discover must render the parsed description in its table row (the \
         degraded fallback rendered the fence line instead): {discover}"
    );
}

#[test]
fn legacy_fence_skill_survives_hermes_mount() {
    if !common::fuse_available() {
        eprintln!("SKIP legacy_fence_skill_survives_hermes_mount: FUSE not available");
        return;
    }
    let fx = MountFixture::normal_hermes(seed_hermes);
    let nested = fx.skill_path("catalog").join("legacy-closer");
    let body = std::fs::read_to_string(nested.join("SKILL.md"))
        .expect("read nested legacy SKILL.md through hermes mount");
    assert!(
        body.contains("Body."),
        "hermes nested read works and serves the content: {body:?}"
    );
}
