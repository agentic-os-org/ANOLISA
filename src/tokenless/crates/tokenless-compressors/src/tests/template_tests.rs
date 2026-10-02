#[test]
fn masks_only_runs_of_ascii_digits() {
    assert_eq!(
        mask("progress: item 123 finished in 45ms"),
        "progress: item 0 finished in 0ms"
    );
    assert_eq!(mask("source/path stays distinct"), "source/path stays distinct");
}

#[test]
fn generic_templates_require_numbered_progress_language() {
    assert_eq!(
        generic_progress_template("processing item 42 of batch 7"),
        Some("processing item 0 of batch 0".to_owned())
    );
    assert_eq!(generic_progress_template("customer record 42"), None);
    assert_eq!(generic_progress_template("processing next item"), None);
    assert_eq!(generic_progress_template(""), None);
}
