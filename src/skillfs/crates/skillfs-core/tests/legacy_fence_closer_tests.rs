//! Frontmatter closed by a Markdown thematic break (`----`, `-----`).
//!
//! 1071e10c9 required the closing fence to be exactly `---`. A legacy
//! author's `----` closer — a valid Markdown thematic break — then no
//! longer closed frontmatter, so the whole file became body: the skill
//! parsed degraded with empty metadata, the load produced NO error, and
//! the no-frontmatter description fallback smeared the raw yaml block
//! into the served description (e.g. `--- name: legacy-closer
//! description: ok ---- Body.`).
//!
//! The closer rule now accepts any all-dash run of three or more dashes
//! when the opener was an exact `---`, restoring the legacy shape while
//! keeping 1071e10c9's truncation fixes: a `----`/`---text` opener still
//! never opens frontmatter, and a `---text` line still never closes it.

use skillfs_core::{ParseConfig, parser, store::SkillStore};

fn write_skill(source: &std::path::Path, name: &str, content: &str) {
    let dir = source.join(name);
    std::fs::create_dir_all(&dir).expect("create skill dir");
    std::fs::write(dir.join("SKILL.md"), content).expect("write SKILL.md");
}

const FINE: &str = "---\nname: fine\ndescription: ok\n---\nBody.\n";
/// Valid frontmatter closed by a 4-dash thematic break: parsed before
/// 1071e10c9, silently degraded by it, restored by the dash-run closer.
const DASH4_CLOSER: &str = "---\nname: legacy-closer\ndescription: ok\n----\nBody.\n";
/// Same shape with five dashes, CRLF line endings, and a trailing space
/// on the closer — the closer rule tolerates all of them.
const DASH5_CLOSER_CRLF: &str =
    "---\r\nname: legacy-five\r\ndescription: ok\r\n----- \r\nBody.\r\n";
/// 4-dash opener: still never opens frontmatter (the yaml inside the
/// body is not honored) — 1071e10c9's truncation fix stays.
const DASH4_OPENER: &str = "----\n# rule\n\n---\nname: never\n---\nBody.\n";
/// A line that merely starts with dashes is content and never closes.
const DASH_TEXT_CLOSER: &str = "---\nname: x\ndescription: ok\n---text\nBody.\n";
/// CRLF file: must keep working on both sides of the change.
const CRLF: &str = "---\r\nname: crlf-skill\r\ndescription: ok\r\n---\r\nBody.\r\n";
/// Trailing-space fences: tolerated by the trim_end rule.
const TRAILING_SPACE: &str = "--- \nname: spaced\ndescription: ok\n--- \nBody.\n";

#[test]
fn dash_run_closer_restores_legacy_frontmatter() {
    let parsed = parser::parse_skill_md(DASH4_CLOSER, "legacy-closer");
    assert_eq!(parsed.metadata.name, "legacy-closer");
    assert_eq!(
        parsed.metadata.description, "ok",
        "the yaml description must be served, not smeared with the raw block"
    );
    assert_eq!(parsed.body, "Body.\n");
    assert!(
        !parsed.parse_status.is_degraded(),
        "the legacy closer shape must parse cleanly: {:?}",
        parsed.parse_status
    );

    // Five dashes, CRLF, trailing space on the closer: same outcome.
    let five = parser::parse_skill_md(DASH5_CLOSER_CRLF, "legacy-five");
    assert_eq!(five.metadata.description, "ok");
    assert_eq!(five.body, "Body.\r\n");
    assert!(!five.parse_status.is_degraded(), "{:?}", five.parse_status);
}

#[test]
fn dash_run_opener_and_dash_text_closer_stay_whole_content() {
    // A longer dash run as the OPENER is a thematic break, not a fence:
    // the whole file stays body and the yaml inside is not honored.
    let opener = parser::parse_skill_md(DASH4_OPENER, "legacy-opener");
    assert_ne!(opener.metadata.description, "never");
    assert!(opener.parse_status.is_degraded());

    // `---text` never closes: the shape stays a bare opener.
    let text = parser::parse_skill_md(DASH_TEXT_CLOSER, "bare");
    assert!(text.parse_status.is_degraded());
    assert_ne!(text.metadata.description, "ok");
    assert_eq!(text.body, DASH_TEXT_CLOSER);
}

#[test]
fn store_loads_legacy_fence_skills_without_errors() {
    let source = tempfile::tempdir().expect("source tempdir");
    write_skill(source.path(), "fine", FINE);
    write_skill(source.path(), "legacy-closer", DASH4_CLOSER);
    write_skill(source.path(), "legacy-opener", DASH4_OPENER);
    write_skill(source.path(), "crlf-skill", CRLF);
    write_skill(source.path(), "spaced", TRAILING_SPACE);

    let mut store = SkillStore::new();
    let errors = store.load_from_directory(source.path(), &ParseConfig::default());
    assert!(
        errors.is_empty(),
        "legacy-fence shapes must not be LoadErrors: {errors:?}"
    );

    let names = store.list();
    for want in [
        "fine",
        "legacy-closer",
        "legacy-opener",
        "crlf-skill",
        "spaced",
    ] {
        assert!(
            names.iter().any(|n| *n == want),
            "discovery must keep listing {want}: {names:?}"
        );
    }

    // The restored skill serves its real metadata through the store; the
    // dash-run opener stays degraded (whole content) without smearing a
    // *parsed* description.
    assert_eq!(
        store
            .get("legacy-closer")
            .map(|e| e.metadata.description.clone())
            .unwrap_or_default(),
        "ok"
    );
    assert_eq!(
        store
            .get("crlf-skill")
            .map(|e| e.metadata.description.clone())
            .unwrap_or_default(),
        "ok",
        "CRLF frontmatter must keep parsing"
    );
    assert_eq!(
        store
            .get("spaced")
            .map(|e| e.metadata.description.clone())
            .unwrap_or_default(),
        "ok",
        "trailing-space fences must keep parsing"
    );

    // Views do not drop the legacy shapes, and duplicate view entries
    // still collapse to one listing (0c71c3d87 set semantics).
    let (primary, _secondary) = store.split_primary(Some(&[
        "legacy-closer".to_string(),
        "legacy-closer".to_string(),
        "fine".to_string(),
    ]));
    assert_eq!(primary.len(), 2, "dedup + legacy both list: {primary:?}");
}

#[test]
fn bom_frontmatter_keeps_parsing() {
    // A UTF-8 BOM before the opening fence is stripped before fence
    // extraction, so BOM skills parse cleanly — before AND after the
    // closer-rule change.
    let content = "\u{feff}---\nname: bom-skill\ndescription: ok\n---\nBody.\n";
    let parsed = parser::parse_skill_md(content, "bom-skill");
    assert_eq!(
        parsed.metadata.description, "ok",
        "BOM frontmatter keeps parsing (pre-existing strip)"
    );
    assert!(
        !parsed.parse_status.is_degraded(),
        "BOM skill must stay non-degraded: {:?}",
        parsed.parse_status
    );

    // CRLF with BOM, closed by a dash run, behaves the same.
    let crlf_bom = "\u{feff}---\r\nname: bom-crlf\r\ndescription: ok\r\n----\r\nBody.\r\n";
    let parsed2 = parser::parse_skill_md(crlf_bom, "bom-crlf");
    assert_eq!(parsed2.metadata.description, "ok");
    assert!(!parsed2.parse_status.is_degraded());
}
