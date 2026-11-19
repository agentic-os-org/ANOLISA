#[test]
fn masks_only_runs_of_ascii_digits() {
    assert_eq!(
        mask("progress: item 123 finished in 45ms"),
        "progress: item 0 finished in 0ms"
    );
    assert_eq!(
        mask("source/path stays distinct"),
        "source/path stays distinct"
    );
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
    assert_eq!(
        generic_progress_template("Downloading from central: https://repo.example/a-1.jar"),
        None
    );
    assert_eq!(generic_progress_template("Progress (0): 1 kB\r"), None);
    assert_eq!(
        generic_progress_template("progress: step 1\ropaque redraw 2"),
        None
    );
    assert_eq!(
        generic_progress_template("> Task :svc-1:runningIntegrationTests FAILED"),
        None
    );
    assert_eq!(
        generic_progress_template("> Task :mod-2:buildingJar UP-TO-DATE"),
        None
    );
    assert_eq!(
        generic_progress_template("      Start  1: runningIntegrationTests"),
        None
    );
    assert_eq!(
        generic_progress_template(
            " 1/12 Test  #1: buildingSuite ....................   Passed    0.05 sec"
        ),
        None
    );
    assert_eq!(
        generic_progress_template(
            " 2/12 Test  #2: compilingCase ....................***Failed    0.10 sec"
        ),
        None
    );
}
