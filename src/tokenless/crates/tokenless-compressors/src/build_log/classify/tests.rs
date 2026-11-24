use super::*;

#[test]
fn error_warning_and_failure_lines_are_diagnostic() {
    let templates = HashSet::new();
    for (format, line) in [
        (BuildLogFormat::Cargo, "error[E0308]: mismatched types"),
        (BuildLogFormat::Cargo, "warning: unused variable"),
        (BuildLogFormat::Npm, "npm ERR! code E404"),
        (BuildLogFormat::Go, "--- FAIL: TestThing (0.01s)"),
        (BuildLogFormat::Make, "Segmentation fault (core dumped)"),
    ] {
        assert_eq!(
            classify(line, format, &templates),
            LineRole::Diagnostic,
            "line: {line}"
        );
    }
}

#[test]
fn dialects_assign_only_known_progress_to_routine_roles() {
    let templates = HashSet::new();
    for (format, line, family) in [
        (
            BuildLogFormat::Cargo,
            "Compiling serde v1.0.0",
            RoutineFamily::CargoCompile,
        ),
        (
            BuildLogFormat::Cargo,
            "test parser::accepts_valid_input ... ok",
            RoutineFamily::CargoTestPass,
        ),
        (
            BuildLogFormat::Pytest,
            "tests/test_a.py::case PASSED [10%]",
            RoutineFamily::PytestPass,
        ),
        (
            BuildLogFormat::Pytest,
            "tests/test_a.py ........ [ 40%]",
            RoutineFamily::PytestProgress,
        ),
        (
            BuildLogFormat::Npm,
            "npm http fetch GET 200 https://registry/npm 20ms",
            RoutineFamily::NpmFetch,
        ),
        (
            BuildLogFormat::Jest,
            "PASS src/a.test.js",
            RoutineFamily::JestPass,
        ),
        (
            BuildLogFormat::Go,
            "go: downloading example.com/mod v1.0.0",
            RoutineFamily::GoDownload,
        ),
        (
            BuildLogFormat::Make,
            "cc -O2 -c src/a.c -o build/a.o",
            RoutineFamily::MakeCompile,
        ),
    ] {
        assert_eq!(
            classify(line, format, &templates),
            LineRole::Routine(family)
        );
    }
    assert_eq!(
        classify(
            "unrecognized but potentially important output",
            BuildLogFormat::Cargo,
            &templates,
        ),
        LineRole::Unknown
    );
}

#[test]
fn pytest_nonstandard_outcomes_remain_diagnostic() {
    let templates = HashSet::new();
    for line in [
        "tests/test_a.py::case XPASS [25%]",
        "tests/test_a.py::case XFAIL [50%]",
        "XPASS tests/test_a.py::case - behavior changed",
        "XFAIL tests/test_a.py::case - expected failure",
    ] {
        assert_eq!(
            classify(line, BuildLogFormat::Pytest, &templates),
            LineRole::Diagnostic,
            "line: {line}"
        );
    }
}

#[test]
fn pytest_quiet_summary_is_strong_dialect_evidence() {
    assert_eq!(
        format_evidence(BuildLogFormat::Pytest, "38 passed in 1.23s"),
        Evidence { strong: 1, weak: 0 }
    );
    assert_eq!(
        format_evidence(BuildLogFormat::Pytest, "1 failed, 33 passed in 2.14s"),
        Evidence { strong: 1, weak: 0 }
    );
}

#[test]
fn npm_warnings_and_make_directories_are_preserved() {
    let templates = HashSet::new();
    assert_eq!(
        classify(
            "npm WARN deprecated old-package@1.0.0",
            BuildLogFormat::Npm,
            &templates,
        ),
        LineRole::Diagnostic
    );
    for line in [
        "make[1]: Entering directory '/work/lib'",
        "make[1]: Leaving directory '/work/lib'",
    ] {
        assert_eq!(
            classify(line, BuildLogFormat::Make, &templates),
            LineRole::Phase
        );
    }
}

#[test]
fn summaries_and_phase_boundaries_are_not_routine() {
    let templates = HashSet::new();
    assert_eq!(
        classify(
            "Finished `release` profile [optimized] target(s) in 1s",
            BuildLogFormat::Cargo,
            &templates,
        ),
        LineRole::Summary
    );
    assert_eq!(
        classify(
            "===== test session starts =====",
            BuildLogFormat::Pytest,
            &templates,
        ),
        LineRole::Phase
    );
}

#[test]
fn package_names_containing_error_are_not_diagnostics() {
    let templates = HashSet::new();
    assert_eq!(
        classify(
            "npm http fetch GET 200 https://registry/npm/http-errors 20ms",
            BuildLogFormat::Npm,
            &templates,
        ),
        LineRole::Routine(RoutineFamily::NpmFetch)
    );
    assert_eq!(
        classify(
            "Compiling error-chain v0.12.4",
            BuildLogFormat::Cargo,
            &templates,
        ),
        LineRole::Routine(RoutineFamily::CargoCompile)
    );
}

#[test]
fn generic_requires_a_preselected_dominant_template() {
    let line = "progress: item 42 complete";
    let template = generic_progress_template(line).unwrap();
    assert_eq!(
        classify(line, BuildLogFormat::Generic, &HashSet::new()),
        LineRole::Unknown
    );
    assert_eq!(
        classify(line, BuildLogFormat::Generic, &HashSet::from([template]),),
        LineRole::Routine(RoutineFamily::Generic)
    );
}

#[test]
fn make_notices_failures_and_compiler_command_boundaries() {
    let templates = HashSet::new();
    for strong in [
        "make: *** [Makefile:12: all] Error 2",
        "make[1]: *** [CMakeFiles/app.dir/build.make:76: a.o] Error 1",
        "gmake[2]: *** [Makefile:9: b.o] Error 1",
        "mingw32-make: *** [all] Error 2",
        "gmake[2]: Entering directory '/work/build'",
        "gmake[2]: Leaving directory '/work/build'",
    ] {
        assert_eq!(
            format_evidence(BuildLogFormat::Make, strong),
            Evidence { strong: 1, weak: 0 },
            "expected strong Make evidence for: {strong}"
        );
    }
    assert_eq!(
        format_evidence(BuildLogFormat::Make, "make[abc]: *** [bad] Error 1"),
        Evidence { strong: 0, weak: 0 }
    );

    assert_eq!(
        classify(
            "cc -O2 -c src/a.c -o build/a.o",
            BuildLogFormat::Make,
            &templates,
        ),
        LineRole::Routine(RoutineFamily::MakeCompile)
    );
    for kept_inline in [
        "cd /work/project && /usr/bin/gcc -O2 -Wall -c src/a.c -o obj/a.o && cp obj/a.o stage/a.o",
        "/usr/bin/gcc -O2 -Wall -c src/m0.c -o build/m0.o && echo built m0",
        "ccache gcc -O2 -c src/a.c -o build/a.o",
        "gcc-13 -O2 -c src/a.c -o build/a.o",
    ] {
        assert_eq!(
            classify(kept_inline, BuildLogFormat::Make, &templates),
            LineRole::Unknown,
            "expected Unknown for: {kept_inline}"
        );
    }
}
