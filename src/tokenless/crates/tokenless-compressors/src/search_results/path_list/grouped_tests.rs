//! Directory grouping selection and byte-preservation regressions.

use super::*;

fn legacy_view(input: &str) -> String {
    let list = parse(input).unwrap();
    let base = "/workspace/project/";
    let paths: Vec<_> = list
        .paths
        .iter()
        .map(|path| path.strip_prefix(base).unwrap())
        .collect();
    format!(
        "{HEADER}{}",
        serde_json::json!({
            "base": base, "paths": paths,
            "line_ending": list.line_ending, "trailing_newline": list.trailing_newline,
        })
    )
}

#[test]
fn directory_runs_improve_on_the_legacy_view() {
    let input: String = (0..8)
        .flat_map(|directory| {
            (0..12).map(move |file| {
                format!(
                    "/workspace/project/long_component_directory_{directory}/nested/file_{file}.rs\n"
                )
            })
        })
        .collect();
    let view = compress(&input).unwrap();
    let legacy = legacy_view(&input);
    assert!(view.starts_with(grouped::HEADER));
    assert_eq!(grouped::restore(&view).as_deref(), Some(input.as_str()));
    assert!(view.len() < legacy.len() / 2);
    assert!(estimate_tokens(&view) < estimate_tokens(&legacy) / 2);
}

#[test]
fn alternating_directories_keep_the_identical_legacy_view() {
    let input: String = (0..90)
        .map(|file| format!("/workspace/project/component_{}/file_{file}.rs\n", file % 3))
        .collect();
    let legacy = legacy_view(&input);
    assert_eq!(compress(&input).as_deref(), Some(legacy.as_str()));
    assert_eq!(restore_flat(&legacy).as_deref(), Some(input.as_str()));
}

#[test]
fn grouped_paths_preserve_roots_names_duplicates_and_endings() {
    for root in [
        "/workspace/project/",
        "./项目 中文/",
        "../project/",
        "src/",
        "/project/../project//",
    ] {
        for ending in ["\n", "\r\n"] {
            for trailing in [false, true] {
                let names = [
                    "first file.rs",
                    "中文.rs",
                    "line\u{2028}separator.rs",
                    "line\u{2029}separator.rs",
                    "quote\"back\\slash.rs",
                    "Dir=\"other\"",
                    "Base=other",
                    "\"quoted.rs",
                    "[Paths grouped by directory; all received paths retained; full path = base + Dir + entry]",
                    "",
                    "first file.rs",
                ];
                let paths: Vec<_> = [
                    "",
                    "long_directory_one/nested/",
                    "目录 quote\"back\\slash/",
                    "",
                ]
                .into_iter()
                .flat_map(|directory| {
                    names
                        .iter()
                        .cycle()
                        .take(24)
                        .map(move |name| format!("{root}{directory}{name}"))
                })
                .collect();
                let input = format!(
                    "{}{}",
                    paths.join(ending),
                    if trailing { ending } else { "" }
                );
                let view = compress(&input).unwrap();
                assert!(view.starts_with(grouped::HEADER), "{root:?} {ending:?}");
                assert_eq!(grouped::restore(&view).as_deref(), Some(input.as_str()));
                assert!(view.contains("Dir=\"\"\n"));
                assert!(view.contains("\"Dir=\\\"other\\\"\"\n"));
                assert!(view.contains("\"\"\n"));
            }
        }
    }
}

#[test]
fn every_selected_view_is_no_larger_than_the_legacy_view() {
    // Vary directory locality and file length without relying on a random crate.
    for run in [1, 2, 3, 8, 30] {
        for name_length in [1, 4, 32] {
            let input: String = (0..90)
                .map(|index| {
                    format!(
                        "/workspace/project/directory_{}/{index}_{}.rs\n",
                        (index / run) % 3,
                        "x".repeat(name_length)
                    )
                })
                .collect();
            let view = compress(&input).unwrap();
            let legacy = legacy_view(&input);
            assert!(view.len() <= legacy.len());
            assert!(estimate_tokens(&view) <= estimate_tokens(&legacy));
            let restored = if view.starts_with(grouped::HEADER) {
                grouped::restore(&view)
            } else {
                restore_flat(&view)
            };
            assert_eq!(restored.as_deref(), Some(input.as_str()));
        }
    }
}

#[test]
fn grouping_is_profitable_even_when_the_flat_view_expands_input() {
    let paths: Vec<_> = (0..30)
        .map(|index| format!("module_{}/long_directory/file_{index}.rs", index / 10))
        .collect();
    let input: String = paths.iter().map(|path| format!("src/{path}\n")).collect();
    let flat = format!(
        "{HEADER}{}",
        serde_json::json!({
            "base": "src/", "paths": paths,
            "line_ending": "\n", "trailing_newline": true,
        })
    );
    assert!(flat.len() >= input.len());
    assert!(estimate_tokens(&flat) >= estimate_tokens(&input));
    let view = compress(&input).unwrap();
    assert!(view.starts_with(grouped::HEADER));
    assert!(view.len() < input.len());
    assert!(estimate_tokens(&view) < estimate_tokens(&input));
    assert_eq!(grouped::restore(&view).as_deref(), Some(input.as_str()));
}

#[test]
fn grouping_can_save_filename_escaping_without_directory_locality() {
    let input: String = (0..60)
        .map(|index| {
            format!(
                "/workspace/project/component_{}/{}_{index}.rs\n",
                index % 3,
                "quote\"back\\slash".repeat(20)
            )
        })
        .collect();
    let view = compress(&input).unwrap();
    let legacy = legacy_view(&input);
    assert!(view.starts_with(grouped::HEADER));
    assert!(view.len() < legacy.len());
    assert!(estimate_tokens(&view) < estimate_tokens(&legacy));
    assert_eq!(grouped::restore(&view).as_deref(), Some(input.as_str()));
}

#[test]
fn flat_verification_rejects_an_unsupported_line_ending() {
    let view = format!(
        "{HEADER}{}",
        serde_json::json!({
            "base": "src/", "paths": ["a", "b", "c"],
            "line_ending": "\r", "trailing_newline": true,
        })
    );
    assert!(restore_flat(&view).is_none());
}

#[test]
fn invalid_group_metadata_and_missing_directory_are_rejected() {
    let metadata = r#"{"base":"src/","line_ending":"\n","trailing_newline":true}"#;
    for records in [
        "file.rs\n",
        "Dir=not-json\nfile.rs\n",
        "Dir=\"not-a-directory\"\nfile.rs\n",
        "Dir=\"\"\n\n",
        "Dir=\"\"\n\"unterminated\n",
        "Dir=\"\"\n",
    ] {
        assert!(grouped::restore(&format!("{}{metadata}\n{records}", grouped::HEADER)).is_none());
    }
    assert!(grouped::restore(&format!("{}{{}}\nDir=\"\"\nfile.rs\n", grouped::HEADER)).is_none());
}
