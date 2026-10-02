fn lines(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn python_trace_bounds() {
    let text = lines(&[
        "before",
        "Traceback (most recent call last):",
        "  File \"/app/main.py\", line 10, in <module>",
        "    run()",
        "  File \"/app/lib.py\", line 4, in run",
        "    raise ValueError(\"boom\")",
        "ValueError: boom",
        "after",
    ]);
    assert_eq!(trace_regions(&text), vec![1..7]);
}

#[test]
fn python_chained_trace_survives_blank_lines() {
    // A chained traceback separated by blank lines is ONE region — cutting
    // it at the blanks is a known failure mode in prior art.
    let text = lines(&[
        "Traceback (most recent call last):",
        "  File \"/app/a.py\", line 1, in f",
        "    inner()",
        "KeyError: 'x'",
        "",
        "During handling of the above exception, another exception occurred:",
        "",
        "Traceback (most recent call last):",
        "  File \"/app/b.py\", line 2, in g",
        "    f()",
        "RuntimeError: broken",
        "tail line",
    ]);
    assert_eq!(trace_regions(&text), vec![0..11]);
}

#[test]
fn rust_panic_with_backtrace() {
    let text = lines(&[
        "thread 'main' panicked at src/main.rs:5:5:",
        "index out of bounds: the len is 3 but the index is 7",
        "note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace",
        "stack backtrace:",
        "   0: std::panicking::begin_panic",
        "             at /rustc/abc/library/std/src/panicking.rs:645:5",
        "   1: main::main",
        "unrelated",
    ]);
    assert_eq!(trace_regions(&text), vec![0..7]);
}

#[test]
fn go_panic_with_goroutine_dumps() {
    let text = lines(&[
        "panic: runtime error: index out of range [7] with length 3",
        "",
        "goroutine 1 [running]:",
        "main.main()",
        "\t/work/main.go:10 +0x24",
        "created by main.init",
        "\t/work/main.go:5 +0x1d",
        "done",
    ]);
    assert_eq!(trace_regions(&text), vec![0..7]);
}

#[test]
fn node_error_with_at_frames() {
    let text = lines(&[
        "TypeError: Cannot read properties of undefined (reading 'x')",
        "    at foo (/app/index.js:10:5)",
        "    at bar (/app/index.js:20:1)",
        "next",
    ]);
    assert_eq!(trace_regions(&text), vec![0..3]);
}

#[test]
fn java_exception_with_cause_chain() {
    let text = lines(&[
        "Exception in thread \"main\" java.lang.RuntimeException: boom",
        "\tat com.example.Main.run(Main.java:10)",
        "Caused by: java.lang.NullPointerException",
        "\tat com.example.Util.get(Util.java:5)",
        "\t... 3 more",
        "after",
    ]);
    assert_eq!(trace_regions(&text), vec![0..5]);
}

#[test]
fn error_line_without_frames_is_not_a_region() {
    let text = lines(&["Error: something went wrong", "plain line", "another"]);
    assert!(trace_regions(&text).is_empty());
}

#[test]
fn dotnet_inner_exception_chain_stays_one_region() {
    let text = vec![
        "System.Exception: outer failure".to_owned(),
        " ---> System.InvalidOperationException: inner failure".to_owned(),
        "   at App.Inner.Run() in /work/Inner.cs:line 10".to_owned(),
        "   --- End of inner exception stack trace ---".to_owned(),
        "   at App.Outer.Run() in /work/Outer.cs:line 20".to_owned(),
        "summary".to_owned(),
    ];
    assert_eq!(trace_regions(&text), vec![0..5]);
}
