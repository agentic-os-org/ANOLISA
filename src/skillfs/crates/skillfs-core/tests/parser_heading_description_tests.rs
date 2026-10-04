//! The description fallback must return the first *paragraph* of the body,
//! not a markdown heading line. A body whose first line is an H1 and whose
//! second is an H2 must not report the H2 markup as the description.

use skillfs_core::parser::parse_skill_md;

/// A body whose first line is an H1 and whose second is an H2 has its
/// first real paragraph as the description fallback — not the H2's
/// markup.
#[test]
fn test_parse_description_skips_all_leading_headings() {
    let content = "# Title\n\n## Subtitle\n\nActual first paragraph.\n\nMore.\n";

    let entry = parse_skill_md(content, "demo");

    assert_eq!(
        entry.metadata.description, "Actual first paragraph.",
        "leading headings are not paragraphs"
    );
}

/// A CRLF twin defines the same contract for the heading skip.
#[test]
fn test_parse_description_skips_all_leading_headings_crlf() {
    let content = "# Title\r\n\r\n## Subtitle\r\n\r\nActual first paragraph.\r\n\r\nMore.\r\n";

    let entry = parse_skill_md(content, "demo");

    assert_eq!(entry.metadata.description, "Actual first paragraph.");
}

/// `#hashtag is text` is a paragraph, not an ATX heading (no space after
/// the single `#`): the description must be that first paragraph, not
/// whatever follows it.
#[test]
fn test_parse_description_keeps_hashtag_paragraph() {
    let content = "# Title\n\n#hashtag is text\n\nLater.\n";

    let entry = parse_skill_md(content, "demo");

    assert_eq!(
        entry.metadata.description, "#hashtag is text",
        "a hashtag-led line is ordinary text, not a heading to skip"
    );
}

/// Seven or more `#`s are not a valid ATX heading level: the line is
/// ordinary content and must survive as the description.
#[test]
fn test_parse_description_keeps_seven_hashes_paragraph() {
    let content = "####### text\n\nLater.\n";

    let entry = parse_skill_md(content, "demo");

    assert_eq!(
        entry.metadata.description, "####### text",
        "seven hashes are not an ATX heading, the line must be kept"
    );
}

/// A line indented by four or more spaces is content even when it starts
/// with `#`: the heading decision must run before indentation is trimmed
/// away.
#[test]
fn test_parse_description_keeps_indented_hash_paragraph() {
    let content = "# Title\n\n    # indented comment\n\nLater.\n";

    let entry = parse_skill_md(content, "demo");

    assert_eq!(
        entry.metadata.description, "# indented comment",
        "a four-space indented # line is content, not a heading"
    );
}

/// The heading skip must still apply to an indented *valid* heading:
/// up to three spaces of indentation is allowed by CommonMark.
#[test]
fn test_parse_description_skips_indented_heading() {
    let content = "   ## Subtitle\n\nActual first paragraph.\n";

    let entry = parse_skill_md(content, "demo");

    assert_eq!(
        entry.metadata.description, "Actual first paragraph.",
        "a heading indented by three spaces is still skipped"
    );
}
