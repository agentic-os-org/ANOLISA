use super::super::{BuildLogCompressor, oracle, sample_lines};
use super::*;
use crate::Recoverability;
use crate::build_log::scope::{MAX_BYTES, MAX_EDITS, MAX_LINE_BYTES, MAX_ROWS};
use tokenless_ccr::{InMemoryStore, RecoveryMethod, StashStore};

fn compress_lossless(input: &str) -> crate::build_log::BuildLogOutcome {
    BuildLogCompressor.compress_with_recovery(input, None, &RecoveryMethod::None)
}

fn sample(text: &str) -> Vec<String> {
    sample_lines(text)
}

fn decode_both(input: &str) -> String {
    oracle::decode(&oracle::decode(input, COMPILE_HEADER), CMAKE_HEADER)
}

#[test]
fn single_lines_prose_fences_crlf_and_non_build_logs_do_not_trigger_detection() {
    let valid_progress = concat!(
        "[1/3] Building CXX object CMakeFiles/app.dir/a.cpp.o\n",
        "[2/3] Building CXX object CMakeFiles/app.dir/b.cpp.o\n",
        "[3/3] Linking CXX executable app\n",
    );
    for negative in [
        "[1/1] Building CXX object CMakeFiles/app.dir/main.cpp.o\n",
        "[1/1] /usr/bin/cc -O3 -c /work/src/a.c -o CMakeFiles/app.dir/a.c.o\n",
        "[100%] Built target app\n",
        "Building CXX object CMakeFiles/app.dir/main.cpp.o\n",
        "[1/3] Check configuration\n[2/3] Run migration\n[3/3] Done\n",
        "[5/3] Building CXX object CMakeFiles/bad.dir/x.cpp.o\n[6/3] Linking CXX executable bad\n",
        "[2026/11] Building CXX object foo.cpp.o\n[2026/12] Linking CXX executable foo\n",
        "[ 50%] Generating summary report\n[100%] Built target summary\n",
        "[150%] Built target overflow\n",
        "let text = \"[1/2] Building CXX object CMakeFiles/app.dir/a.cpp.o\";\n",
        "let cmd = \"/usr/bin/cc -O3 -MD -c /work/src/a.c -o a.o\";\n",
        "/usr/bin/cc -O3 -c /work/src/a.c -o a.o && rm -rf /tmp/x\n/usr/bin/cc -O3 -c /work/src/b.c -o b.o && rm -rf /tmp/y\n",
        &format!("```text\n{valid_progress}```\n"),
        &format!("~~~text\n{valid_progress}~~~\n"),
    ] {
        assert!(!detect(&sample(negative)), "false positive on: {negative}");
        assert!(
            !BuildLogCompressor::detect(negative),
            "false positive BuildLogCompressor::detect on: {negative}"
        );
    }

    let crlf = valid_progress.replace('\n', "\r\n");
    assert!(!detect(&sample(&crlf)));
    assert!(!BuildLogCompressor::detect(&crlf));

    let head_padding = format!("{}{valid_progress}", "ordinary output line\n".repeat(120));
    assert!(!detect(&sample(&head_padding)));
    assert!(!BuildLogCompressor::detect(&head_padding));

    let oversized_head = format!("{}\n{valid_progress}", "x".repeat(MAX_LINE_BYTES + 1));
    assert!(!detect(&sample(&oversized_head)));
    assert!(!BuildLogCompressor::detect(&oversized_head));

    let unclosed_fence_gap = format!("```text\n{}{valid_progress}", "example line\n".repeat(120));
    assert!(!detect(&sample(&unclosed_fence_gap)));
    assert!(!BuildLogCompressor::detect(&unclosed_fence_gap));

    // Handwritten short Makefiles keep routing to Stage 2 `Make`.
    let handwritten_make = concat!(
        "make: Entering directory '/work'\n",
        "cc -O2 -Wall -c src/a.c -o build/a.o\n",
        "cc -O2 -Wall -c src/b.c -o build/b.o\n",
    );
    assert!(!detect(&sample(handwritten_make)));
    assert!(BuildLogCompressor::detect(handwritten_make));
    let store = InMemoryStore::new();
    let make_outcome = BuildLogCompressor.compress_with_recovery(
        handwritten_make,
        Some(&store),
        &RecoveryMethod::Shell,
    );
    assert!(!make_outcome.output.contains(CMAKE_HEADER));
    assert!(!make_outcome.output.contains(COMPILE_HEADER));

    let configure_plus_build = concat!(
        "$ cmake -S /work/src -B /work/build && cmake --build /work/build -- -j2 VERBOSE=1\n",
        "-- The C compiler identification is GNU 13.3.0\n",
        "[ 50%] Building C object CMakeFiles/app.dir/a.c.o\n",
        "/usr/bin/cc -O3 -c /work/src/a.c -o CMakeFiles/app.dir/a.c.o\n",
        "[100%] Building C object CMakeFiles/app.dir/b.c.o\n",
        "/usr/bin/cc -O3 -c /work/src/b.c -o CMakeFiles/app.dir/b.c.o\n",
    );
    assert!(!detect(&sample(configure_plus_build)));
}

#[test]
fn positive_compile_detection_and_mutual_exclusion_hold() {
    let ninja_verbose = concat!(
        "[1/3] /usr/bin/cc -O3 -DNDEBUG -MD -MT CMakeFiles/app.dir/a.c.o -MF CMakeFiles/app.dir/a.c.o.d -o CMakeFiles/app.dir/a.c.o -c /work/src/a.c\n",
        "[2/3] /usr/bin/cc -O3 -DNDEBUG -MD -MT CMakeFiles/app.dir/b.c.o -MF CMakeFiles/app.dir/b.c.o.d -o CMakeFiles/app.dir/b.c.o -c /work/src/b.c\n",
    );
    assert!(detect(&sample(ninja_verbose)));
    assert!(BuildLogCompressor::detect(ninja_verbose));

    let make_verbose = concat!(
        "/usr/bin/cmake -S/work/src -B/work/build --check-build-system CMakeFiles/Makefile.cmake 0\n",
        "/usr/bin/gmake -f CMakeFiles/Makefile2 all\n",
        "gmake[1]: Entering directory '/work/build'\n",
        "[ 50%] Building C object CMakeFiles/app.dir/a.c.o\n",
        "cd /work/build/sub && /usr/bin/clang-18 -O2 -o CMakeFiles/app.dir/a.c.o -c /work/src/a.c\n",
        "[100%] Building C object CMakeFiles/app.dir/b.c.o\n",
        "cd /work/build/sub && /usr/bin/clang-18 -O2 -o CMakeFiles/app.dir/b.c.o -c /work/src/b.c\n",
    );
    assert!(detect(&sample(make_verbose)));

    let native_make = concat!(
        "make: Entering directory '/work/lib'\n",
        "CC obj/static/debug.o\n",
        "cc -O3 -Wall   -c -MMD -MP -o obj/static/debug.o /work/lib/debug.c\n",
        "AS obj/static/asm.o\n",
        "cc -O3 -Wall   -c -MMD -MP -o obj/static/asm.o /work/lib/asm.S\n",
    );
    assert!(detect(&sample(native_make)));

    // Line grammar variants are mutually exclusive.
    assert!(matches!(
        grammar::parse_line(""),
        Some(grammar::LineKind::Blank)
    ));
    assert!(matches!(
        grammar::parse_line("[1/3] /usr/bin/cc -O3 -c /work/a.c -o a.o"),
        Some(grammar::LineKind::Compile(c)) if c.has_ninja_step && c.is_verbose
    ));
    assert!(matches!(
        grammar::parse_line("[ 50%] Building C object CMakeFiles/app.dir/a.c.o"),
        Some(grammar::LineKind::Progress(_))
    ));
    assert!(matches!(
        grammar::parse_line("CC obj/static/debug.o"),
        Some(grammar::LineKind::MakeTag(_))
    ));
    assert!(matches!(
        grammar::parse_line("cc -O2 -c src/a.c -o build/a.o"),
        Some(grammar::LineKind::Compile(c)) if !c.has_ninja_step && !c.is_verbose
    ));
}

#[test]
fn valid_preambles_and_lifecycle_lines_preserve_detection_and_stay_verbatim() {
    let mut rows = String::new();
    for step in 1..=12 {
        rows.push_str(&format!(
            "[{step}/12] Building CXX object src/CMakeFiles/core.dir/module_{step:02}.cpp.o\n"
        ));
    }
    for preamble in [
        "$ cmake --build /work/build\n",
        "$ ninja -C /work/build\n",
        "+ ninja -C /work/build\n",
        "++ /usr/bin/gmake -j8\n",
        "ninja: Entering directory `/work/build'\n",
        "make[1]: Entering directory '/work/build'\n",
        "gmake[2]: Leaving directory '/work/build'\n",
        "Change Dir: '/work/build'\n",
        "Run Build Command(s): /usr/bin/ninja -v\n",
    ] {
        let input = format!("{preamble}{rows}exit code: 0\n");
        assert!(BuildLogCompressor::detect(&input), "preamble={preamble:?}");
        let outcome = compress_lossless(&input);
        assert_eq!(outcome.recoverability, Recoverability::Lossless);
        assert!(
            outcome.output.starts_with(preamble),
            "preamble={preamble:?}"
        );
        assert!(outcome.output.ends_with("exit code: 0\n"));
        assert_eq!(decode_both(&outcome.output), input);
    }

    let no_work = "$ ninja -C /work/build\nninja: no work to do.\nExit Status: 0\n";
    assert!(BuildLogCompressor::detect(no_work));
    assert_eq!(compress_lossless(no_work).output, no_work);
}

#[test]
fn ninja_and_cmake_make_variants_share_templates_and_round_trip() {
    let ninja = concat!(
        "[ 1/24] Creating library symlink libfoo.so.1 libfoo.so\n",
        "[ 2/24] Creating executable symlink bin/tool bin/tool-1\n",
        "[ 3/24] Generating gen/alpha.cc, gen/alpha.h\n",
        "[ 4/24] Generating gen/beta.cc, gen/beta.h\n",
        "[ 5/24] Generating gen/gamma.cc, gen/gamma.h\n",
        "[ 6/24] Generating gen/delta.cc, gen/delta.h\n",
        "[ 7/24] Automatic MOC and UIC for target ui_core\n",
        "[ 8/24] Automatic MOC and UIC for target ui_widgets\n",
        "[ 9/24] Automatic MOC and UIC for target ui_dialog\n",
        "[10/24] Automatic MOC and UIC for target ui_main\n",
        "[11/24] Building NVCC (Device) object src/CMakeFiles/gpu.dir/kernel_a.cu.o\n",
        "[12/24] Building NVCC (Device) object src/CMakeFiles/gpu.dir/kernel_b.cu.o\n",
        "[13/24] Building NVCC (Device) object src/CMakeFiles/gpu.dir/kernel_c.cu.o\n",
        "[14/24] Building NVCC (Device) object src/CMakeFiles/gpu.dir/kernel_d.cu.o\n",
        "[15/24] Compiling CXX object src/CMakeFiles/core.dir/alpha.cpp.o\n",
        "[16/24] Compiling CXX object src/CMakeFiles/core.dir/beta.cpp.o\n",
        "[17/24] Compiling CXX object src/CMakeFiles/core.dir/gamma.cpp.o\n",
        "[18/24] Compiling CXX object src/CMakeFiles/core.dir/delta.cpp.o\n",
        "[1/27] Running CMake to regenerate build system...\n",
        "[19/27] Linking static target sub/libcore_a.a\n",
        "[20/27] Linking static target sub/libcore_b.a\n",
        "[21/27] Linking static target sub/libcore_c.a\n",
        "[22/27] Linking CXX shared library lib/libcore.so.1.2\n",
        "[23/27] Linking CXX executable bin/demo\n",
        "[24/27] Built target demo\n",
    );
    assert!(BuildLogCompressor::detect(ninja));
    let outcome = compress_lossless(ninja);
    assert!(outcome.output.contains(CMAKE_HEADER));
    assert!(
        outcome
            .output
            .contains("[1/27] Running CMake to regenerate build system...\n")
    );
    assert_eq!(decode_both(&outcome.output), ninja);
    assert_eq!(compress_lossless(&outcome.output).output, outcome.output);

    let make_pct = concat!(
        "[  0%] Building CXX object src/CMakeFiles/app.dir/item_01.cpp.o\n",
        "[ 20%] Building CXX object src/CMakeFiles/app.dir/item_02.cpp.o\n",
        "[ 40%] Building CXX object src/CMakeFiles/app.dir/item_03.cpp.o\n",
        "[ 60%] Building CXX object src/CMakeFiles/app.dir/item_04.cpp.o\n",
        "[ 80%] Linking CXX executable ../bin/app\n",
        "[100%] Built target app\n",
    );
    assert!(BuildLogCompressor::detect(make_pct));
    let make_outcome = compress_lossless(make_pct);
    assert!(make_outcome.output.contains(CMAKE_HEADER));
    assert_eq!(decode_both(&make_outcome.output), make_pct);
    assert_eq!(
        compress_lossless(&make_outcome.output).output,
        make_outcome.output
    );
}

#[test]
fn flag_orders_spaces_quotes_and_versions_round_trip_byte_exact() {
    let input = concat!(
        "[1/4] /usr/bin/clang++-18 -DFOO=\"bar\"  -isystem /work/inc/. -I /work/other.c/dir -O3 -MD -MT CMakeFiles/app.dir/alpha.cpp.o -MF CMakeFiles/app.dir/alpha.cpp.o.d -o CMakeFiles/app.dir/alpha.cpp.o -c /work/src/alpha.cpp\n",
        "[2/4] /usr/bin/clang++-18 -DFOO=\"bar\"  -isystem /work/inc/. -I /work/other.c/dir -O3 -MD -MT CMakeFiles/app.dir/beta.cpp.o -MF CMakeFiles/app.dir/beta.cpp.o.d -o CMakeFiles/app.dir/beta.cpp.o -c /work/src/beta.cpp\n",
        "[3/4] /usr/bin/clang++-18 -DFOO=\"bar\"  -isystem /work/inc/. -I /work/other.c/dir -O3 -MD -MT CMakeFiles/app.dir/gamma.cpp.o -MF CMakeFiles/app.dir/gamma.cpp.o.d -o CMakeFiles/app.dir/gamma.cpp.o -c /work/src/gamma.cpp\n",
        "[4/4] /usr/bin/clang++-18 -DFOO=\"bar\"  -isystem /work/inc/. -I /work/other.c/dir -O3 -MD -MT CMakeFiles/app.dir/delta.cpp.o -MF CMakeFiles/app.dir/delta.cpp.o.d -o CMakeFiles/app.dir/delta.cpp.o -c /work/src/delta.cpp\n",
    );
    assert!(detect(&sample(input)));
    let outcome = compress_lossless(input);
    assert!(outcome.output.contains(COMPILE_HEADER));
    assert!(outcome.stash_writes.is_empty());
    assert_eq!(outcome.recoverability, Recoverability::Lossless);
    assert_eq!(decode_both(&outcome.output), input);
    assert_eq!(compress_lossless(&outcome.output).output, outcome.output);

    let mq_input = concat!(
        "[1/3] /usr/bin/cc -O3 -MD -MQ CMakeFiles/app.dir/alpha.c.o -MF CMakeFiles/app.dir/alpha.c.o.d -o CMakeFiles/app.dir/alpha.c.o -c /work/src/alpha.c\n",
        "[2/3] /usr/bin/cc -O3 -MD -MQ CMakeFiles/app.dir/beta.c.o -MF CMakeFiles/app.dir/beta.c.o.d -o CMakeFiles/app.dir/beta.c.o -c /work/src/beta.c\n",
        "[3/3] /usr/bin/cc -O3 -MD -MQ CMakeFiles/app.dir/gamma.c.o -MF CMakeFiles/app.dir/gamma.c.o.d -o CMakeFiles/app.dir/gamma.c.o -c /work/src/gamma.c\n",
    );
    let mq_out = compress_lossless(mq_input);
    assert!(mq_out.output.contains(COMPILE_HEADER));
    assert_eq!(
        mq_out
            .output
            .lines()
            .find(|line| line.starts_with(COMPILE_HEADER))
            .map(|line| {
                let json: serde_json::Value =
                    serde_json::from_str(line.trim_start_matches(COMPILE_HEADER)).unwrap();
                json["literals"].as_array().unwrap().len()
            }),
        Some(1)
    );
    assert_eq!(decode_both(&mq_out.output), mq_input);
}

#[test]
fn diagnostics_failures_and_control_sequences_close_scope_and_reopen_cleanly() {
    let mut base_rows = Vec::new();
    for step in 1..=12 {
        base_rows.push(format!(
            "[{step:2}/12] Building CXX object src/CMakeFiles/lib.dir/source_{step:02}.cpp.o\n"
        ));
    }

    for injection in [
        "FAILED: src/CMakeFiles/lib.dir/source_05.cpp.o \n",
        "src/source_05.cpp:42:5: error: use of undeclared identifier 'x'\n",
        "/work/include/fmt-c.h:59:1: note: the ABI has changed\n",
        "warning: deprecated declaration [-Wdeprecated-declarations]\n",
        "ninja: build stopped: subcommand failed.\n",
        "make[2]: *** [CMakeFiles/lib.dir/build.make:76: source_05.cpp.o] Error 1\n",
        "opaque redraw 50%\r",
        "\x1b[0Kunknown progress marker\n",
        "unexpected plain stdout text\n",
        "```text\n[1/2] Building CXX object CMakeFiles/ex.dir/a.cpp.o\n[2/2] Built target ex\n```\n",
    ] {
        for pos in 0..=base_rows.len() {
            let mut mutated = String::new();
            for row in &base_rows[..pos] {
                mutated.push_str(row);
            }
            mutated.push_str(injection);
            for row in &base_rows[pos..] {
                mutated.push_str(row);
            }
            let outcome = compress_lossless(&mutated);
            assert_eq!(
                decode_both(&outcome.output),
                oracle::strip_sgr(&mutated),
                "pos={pos}, injection={injection:?}"
            );
            assert!(
                outcome.output.contains(&oracle::strip_sgr(injection)),
                "pos={pos}, injection={injection:?}"
            );
            assert_eq!(outcome.recoverability, Recoverability::Lossless);
        }
    }

    let isolated_after_diag = concat!(
        "[1/6] Building CXX object src/CMakeFiles/lib.dir/a.cpp.o\n",
        "[2/6] Building CXX object src/CMakeFiles/lib.dir/b.cpp.o\n",
        "[3/6] Building CXX object src/CMakeFiles/lib.dir/c.cpp.o\n",
        "[4/6] Building CXX object src/CMakeFiles/lib.dir/d.cpp.o\n",
        "warning: single diagnostic closes scope\n",
        "[5/6] Building CXX object src/CMakeFiles/lib.dir/e.cpp.o\n",
        "another unknown line before reopen\n",
        "[6/6] Linking CXX executable bin/app\n",
    );
    let out = compress_lossless(isolated_after_diag);
    assert!(
        out.output
            .contains("[5/6] Building CXX object src/CMakeFiles/lib.dir/e.cpp.o\n")
    );
    assert!(
        out.output
            .contains("[6/6] Linking CXX executable bin/app\n")
    );
    assert_eq!(decode_both(&out.output), isolated_after_diag);
}

#[test]
fn compile_diagnostics_fences_controls_and_oversized_lines_preserve_safety_boundaries() {
    let mut rows = Vec::new();
    for step in 1..=8 {
        rows.push(format!(
            "[{step}/8] /usr/bin/c++ -O2 -I/work/inc -MD -MT CMakeFiles/lib.dir/mod_{step:02}.cpp.o -MF CMakeFiles/lib.dir/mod_{step:02}.cpp.o.d -o CMakeFiles/lib.dir/mod_{step:02}.cpp.o -c /work/src/mod_{step:02}.cpp\n"
        ));
    }

    for injection in [
        "FAILED: CMakeFiles/lib.dir/mod_04.cpp.o \n",
        "/work/src/mod_04.cpp:12:5: error: unknown type name 'bad'\n",
        "/work/inc/fmt-c.h:59:1: note: the ABI has changed\n",
        "ninja: build stopped: subcommand failed.\n",
        "gmake[2]: *** [CMakeFiles/lib.dir/build.make:76: mod_04.cpp.o] Error 1\n",
        "opaque redraw 50%\r",
        "\x1b[0Kunknown progress marker\n",
        "```text\n[1/2] /usr/bin/cc -O2 -c /work/a.c -o a.o\n[2/2] /usr/bin/cc -O2 -c /work/b.c -o b.o\n```\n",
    ] {
        for pos in 0..=rows.len() {
            let mut mutated = String::new();
            for row in &rows[..pos] {
                mutated.push_str(row);
            }
            mutated.push_str(injection);
            for row in &rows[pos..] {
                mutated.push_str(row);
            }
            let outcome = compress_lossless(&mutated);
            assert_eq!(
                decode_both(&outcome.output),
                oracle::strip_sgr(&mutated),
                "pos={pos}, inj={injection:?}"
            );
            assert!(
                outcome.output.contains(&oracle::strip_sgr(injection)),
                "pos={pos}, inj={injection:?}"
            );
        }
    }

    let huge_link = format!("/usr/bin/c++ {} -o bin/big\n", "obj/mod.o ".repeat(500));
    let with_huge_link = format!("{}{}{}", rows[..4].concat(), huge_link, rows[4..].concat());
    let out = compress_lossless(&with_huge_link);
    assert!(out.output.contains(&huge_link));
    assert_eq!(out.output.matches(COMPILE_HEADER).count(), 2);
    assert_eq!(decode_both(&out.output), with_huge_link);

    let too_huge = format!("diagnostic {}\n", "x".repeat(MAX_BYTES + 1));
    let too_huge_input = format!("{}{too_huge}", rows.concat());
    let lines: Vec<&str> = too_huge_input.split_inclusive('\n').collect();
    let edits = plan(&lines);
    assert!(!edits.is_empty());
    assert!(!edits.iter().any(|edit| {
        lines[edit.start..edit.end]
            .iter()
            .any(|line| line.len() > MAX_BYTES)
    }));
}

#[test]
fn limits_idempotence_fences_and_anchored_generic_fallback_hold() {
    let before = (1..=6)
        .map(|i| format!("[{i}/14] Building CXX object src/CMakeFiles/a.dir/pre_{i:02}.cpp.o\n"))
        .collect::<String>();
    let long_target = format!(
        "[7/14] Building CXX object src/CMakeFiles/a.dir/{}.cpp.o\n",
        "x".repeat(MAX_LINE_BYTES)
    );
    let after = (8..=14)
        .map(|i| format!("[{i}/14] Building CXX object src/CMakeFiles/a.dir/post_{i:02}.cpp.o\n"))
        .collect::<String>();
    let long_line_log = format!("{before}{long_target}{after}");
    let long_outcome = compress_lossless(&long_line_log);
    assert!(long_outcome.output.contains(&long_target));
    assert!(long_outcome.output.matches(CMAKE_HEADER).count() >= 2);
    assert_eq!(decode_both(&long_outcome.output), long_line_log);

    let huge_line = format!("diagnostic payload: {}\n", "x".repeat(MAX_BYTES + 1));
    let huge_input = format!("{before}{huge_line}{after}");
    let huge_lines: Vec<&str> = huge_input.split_inclusive('\n').collect();
    let huge_edits = plan(&huge_lines);
    assert!(!huge_edits.is_empty());
    assert!(!huge_edits.iter().any(|edit| {
        huge_lines[edit.start..edit.end]
            .iter()
            .any(|line| line.len() > MAX_BYTES)
    }));

    let large_input = (1..=300)
        .map(|i| format!("[{i}/300] Building CXX object src/CMakeFiles/a.dir/file_{i:03}.cpp.o\n"))
        .collect::<String>();
    let large_lines: Vec<&str> = large_input.split_inclusive('\n').collect();
    let large_edits = plan(&large_lines);
    assert!(large_edits.len() >= 2);
    for edit in &large_edits {
        assert!(edit.end - edit.start <= MAX_ROWS);
    }
    assert_eq!(
        decode_both(&compress_lossless(&large_input).output),
        large_input
    );

    let wide_input = (1..=120)
        .map(|i| {
            format!(
                "[{i}/120] Building CXX object src/CMakeFiles/a.dir/sub_{i:03}/{}.cpp.o\n",
                "w".repeat(700)
            )
        })
        .collect::<String>();
    let wide_lines: Vec<&str> = wide_input.split_inclusive('\n').collect();
    let wide_edits = plan(&wide_lines);
    assert!(wide_edits.len() >= 2);
    for edit in &wide_edits {
        let orig: String = wide_lines[edit.start..edit.end].concat();
        assert!(orig.len() <= MAX_BYTES);
    }

    let mut many_blocks = String::new();
    for block in 0..270 {
        for sub in 0..5 {
            let step = block * 5 + sub + 1;
            many_blocks.push_str(&format!(
                "[{step}/1350] Building CXX object src/CMakeFiles/m_{block:03}.dir/unit_component_{sub:02}.cpp.o\n"
            ));
        }
        many_blocks.push_str(&format!("step marker {block}\n"));
    }
    let many_lines: Vec<&str> = many_blocks.split_inclusive('\n').collect();
    let many_edits = plan(&many_lines);
    assert_eq!(many_edits.len(), MAX_EDITS);

    // >4 KiB template headers stay idempotent across sample gaps, while fenced headers do not veto real runs.
    let big_header_input = (0..240)
        .map(|i| {
            let group = i / 3;
            let step = i + 1;
            let sub = i % 3;
            format!(
                "[{step}/240] Building CXX object pkg_component_{group:03}/CMakeFiles/lib_{group:03}.dir/source_unit_{sub}.cpp.o\n"
            )
        })
        .collect::<String>();
    let compacted = compress_lossless(&big_header_input);
    let header_line = compacted
        .output
        .lines()
        .find(|line| line.starts_with(CMAKE_HEADER))
        .unwrap();
    assert!(header_line.len() > MAX_LINE_BYTES);
    let gapped = format!("{}{}", "$ ninja\n".repeat(120), compacted.output);
    let gapped_again = compress_lossless(&gapped);
    assert_eq!(gapped_again.output, gapped);

    let quoted_header =
        format!("```text\n{CMAKE_HEADER}{{\"rows\":1,\"literals\":[[\"example\"]]}}\n0\n```\n");
    let mixed = format!("{before}{quoted_header}{after}");
    let mixed_outcome = compress_lossless(&mixed);
    assert!(mixed_outcome.output.contains(&quoted_header));
    assert!(mixed_outcome.output.matches(CMAKE_HEADER).count() >= 3);
    assert_eq!(decode_both(&mixed_outcome.output), mixed);

    let anchored = format!(
        "$ cmake --build /work/build\n\
         [1/2] Building CXX object src/CMakeFiles/app.dir/main.cpp.o\n\
         {}\
         [2/2] Linking CXX executable app\n\
         exit code: 0\n",
        (1..=14)
            .map(|i| format!("processing generated build step {i} of 14 in custom target\n"))
            .collect::<String>()
    );
    assert!(BuildLogCompressor::detect(&anchored));
    let store = InMemoryStore::new();
    let fallback =
        BuildLogCompressor.compress_with_recovery(&anchored, Some(&store), &RecoveryMethod::Shell);
    assert_eq!(fallback.recoverability, Recoverability::Retrievable);
    assert!(fallback.output.contains("[1/2] Building CXX object"));
    assert!(fallback.output.contains("[2/2] Linking CXX executable app"));
}

#[test]
fn custom_counter_heads_reconfigure_and_scored_or_generic_fallbacks_preserve_compression() {
    let custom_head_lines = (1..=16)
        .map(|i| format!("[{i}/24] Generating custom build manifest {i:02}\n"))
        .collect::<String>();
    let progress_tail_lines = (17..=24)
        .map(|i| format!("[{i}/24] Building C object CMakeFiles/app.dir/src/unit_{i:02}.c.o\n"))
        .collect::<String>();

    let mixed_unanchored = format!("{custom_head_lines}{progress_tail_lines}");
    assert!(BuildLogCompressor::detect(&mixed_unanchored));
    let mixed_out = compress_lossless(&mixed_unanchored);
    assert!(mixed_out.output.contains(CMAKE_HEADER));
    assert!(
        mixed_out
            .output
            .contains("[1/24] Generating custom build manifest 01\n")
    );
    assert_eq!(decode_both(&mixed_out.output), mixed_unanchored);

    let configure_only = concat!(
        "$ cmake --build /work/build\n",
        "[1/4] Generating docs\n",
        "[2/4] Updating index\n",
        "exit code: 0\n",
    );
    assert!(!BuildLogCompressor::detect(configure_only));

    let reconfig_rows = (1..=24)
        .map(|step| {
            format!("[{step}/24] Building CXX object CMakeFiles/app.dir/unit_{step:02}.cpp.o\n")
        })
        .collect::<String>();
    let reconfig = format!(
        "[0/1] Re-running CMake...\n\
         -- Configuring done (0.1s)\n\
         -- Generating done (0.0s)\n\
         -- Build files have been written to: /work/build\n\
         {reconfig_rows}"
    );
    assert!(BuildLogCompressor::detect(&reconfig));
    let reconfig_out = compress_lossless(&reconfig);
    assert!(reconfig_out.output.contains("[0/1] Re-running CMake...\n"));
    assert!(
        reconfig_out
            .output
            .contains("-- Build files have been written to: /work/build\n")
    );
    assert!(reconfig_out.output.contains(CMAKE_HEADER));
    assert_eq!(decode_both(&reconfig_out.output), reconfig);

    let configure_then_build = format!(
        "$ cmake -S src -B build -G Ninja && cmake --build build --parallel 1\n\
         -- The CXX compiler identification is GNU 13.3.0\n\
         -- Detecting CXX compiler ABI info - done\n\
         -- Configuring done (0.1s)\n\
         {reconfig_rows}\
         exit code: 0\n"
    );
    assert!(BuildLogCompressor::detect(&configure_then_build));
    let store = InMemoryStore::new();
    let generic_fallback = BuildLogCompressor.compress_with_recovery(
        &configure_then_build,
        Some(&store),
        &RecoveryMethod::Shell,
    );
    assert_eq!(generic_fallback.recoverability, Recoverability::Retrievable);
    assert_eq!(generic_fallback.stash_writes.len(), 1);
    assert!(!generic_fallback.output.contains(CMAKE_HEADER));

    let cargo_rows = (1..=30)
        .map(|i| format!("   Compiling dep_{i:02} v0.1.{i}\n"))
        .collect::<String>();
    let ninja_then_cargo = format!("ninja: no work to do.\n{cargo_rows}");
    let cargo_store = InMemoryStore::new();
    let cargo_out = BuildLogCompressor.compress_with_recovery(
        &ninja_then_cargo,
        Some(&cargo_store),
        &RecoveryMethod::Shell,
    );
    assert_eq!(cargo_out.recoverability, Recoverability::Retrievable);
    assert_eq!(cargo_out.stash_writes.len(), 1);
    assert!(cargo_out.output.starts_with("ninja: no work to do.\n"));
    assert!(cargo_out.output.contains("lines omitted."));

    // Unprefixed `c++` rows paired with `[PPP%] Building` labels compact losslessly under `COMPILE_HEADER`.
    let make_rows = (1..=14)
        .map(|i| {
            format!(
                "c++ -DAPP_EXPORTS -I/work/include -O2 -fPIC -o CMakeFiles/app.dir/mod_{i:02}.cpp.o -c /work/src/mod_{i:02}.cpp\n"
            )
        })
        .collect::<String>();
    let paired_cmake_then_make = format!(
        "make[1]: Entering directory '/work/build'\n\
         [  8%] Building CXX object CMakeFiles/app.dir/mod_01.cpp.o\n\
         [ 16%] Building CXX object CMakeFiles/app.dir/mod_02.cpp.o\n\
         [ 24%] Building CXX object CMakeFiles/app.dir/mod_03.cpp.o\n\
         {make_rows}\
         make[1]: Leaving directory '/work/build'\n"
    );
    let make_store = InMemoryStore::new();
    let paired_out = BuildLogCompressor.compress_with_recovery(
        &paired_cmake_then_make,
        Some(&make_store),
        &RecoveryMethod::Shell,
    );
    assert_eq!(paired_out.recoverability, Recoverability::Lossless);
    assert!(paired_out.stash_writes.is_empty());
    assert!(paired_out.output.contains(COMPILE_HEADER));
    assert_eq!(decode_both(&paired_out.output), paired_cmake_then_make);

    // Compressing Ninja progress must preserve scored Make omission for bare handwritten Make later in the same log.
    let ninja_head = (1..=9)
        .map(|i| {
            format!("[{i}/9] Building CXX object CMakeFiles/ninja_app.dir/src/unit_{i:02}.cpp.o\n")
        })
        .collect::<String>();
    let cc_tail = (1..=24)
        .map(|i| {
            format!(
                "cc -O2 -Wall -Iinclude -c src/legacy_module_{i:02}.c -o build/legacy_module_{i:02}.o\n"
            )
        })
        .collect::<String>();
    let ninja_then_make = format!(
        "{ninja_head}\
         make: Entering directory '/work/make'\n\
         {cc_tail}\
         make: Leaving directory '/work/make'\n"
    );
    let lossless_only = compress_lossless(&ninja_then_make);
    assert_eq!(lossless_only.recoverability, Recoverability::Lossless);
    assert!(lossless_only.stash_writes.is_empty());

    let mixed_store = InMemoryStore::new();
    let composed = BuildLogCompressor.compress_with_recovery(
        &ninja_then_make,
        Some(&mixed_store),
        &RecoveryMethod::Shell,
    );
    assert_eq!(composed.recoverability, Recoverability::Retrievable);
    assert_eq!(composed.stash_writes.len(), 1);
    assert!(composed.output.contains(CMAKE_HEADER));
    assert!(composed.output.contains("lines omitted."));
    let marker = composed
        .output
        .split_inclusive('\n')
        .find(|line| line.contains("lines omitted."))
        .unwrap();
    let stashed = mixed_store
        .retrieve(&composed.stash_writes[0].key)
        .unwrap()
        .unwrap();
    let stash_restored = composed.output.replacen(marker, &stashed, 1);
    assert_eq!(decode_both(&stash_restored), ninja_then_make);
    let idempotent = BuildLogCompressor.compress_with_recovery(
        &composed.output,
        Some(&mixed_store),
        &RecoveryMethod::Shell,
    );
    assert_eq!(idempotent.output, composed.output);

    // Leading Ninja recovery warnings stay preserved without replacing build evidence.
    assert!(!BuildLogCompressor::detect(
        "ninja: warning: premature end of file; recovering\n"
    ));
    let warn_prefix = "ninja: warning: premature end of file; recovering\n";
    let warn_rows = (1..=16)
        .map(|i| format!("[{i}/16] Building C object CMakeFiles/app.dir/src/unit_{i:02}.c.o\n"))
        .collect::<String>();
    let leading_warn = format!("{warn_prefix}{warn_rows}");
    let warn_store = InMemoryStore::new();
    let warn_out = BuildLogCompressor.compress_with_recovery(
        &leading_warn,
        Some(&warn_store),
        &RecoveryMethod::Shell,
    );
    assert_eq!(warn_out.recoverability, Recoverability::Lossless);
    assert!(warn_out.stash_writes.is_empty());
    assert!(warn_out.output.starts_with(warn_prefix));
    assert!(warn_out.output.contains(CMAKE_HEADER));
    assert_eq!(decode_both(&warn_out.output), leading_warn);

    // Leading whitespace/TAB in Unix Makefiles custom step bodies stays supported without matching blanks or failure markers.
    assert!(!grammar::is_counter_step("[ 43%]   \t  "));
    assert!(!grammar::is_counter_step(
        "[5/9] \tFAILED: CMakeFiles/app.dir/src/main.c.o"
    ));
    assert!(!grammar::is_counter_step(
        "[5/9]   ninja: build stopped: subcommand failed."
    ));
    assert!(is_protected_step(
        "[ 43%] \tProcessing generated build resource 01"
    ));
    assert!(is_protected_step(
        "[ 43%]   Processing generated build resource 01"
    ));
    for indent in ["\t", "  "] {
        let custom_prefix = (1..=24)
            .map(|i| {
                let pct = (i * 100) / 40;
                format!(
                    "[{pct:>3}%] {indent}Processing generated build resource {i:02} for the project metadata index\n"
                )
            })
            .collect::<String>();
        let compile_tail = (25..=40)
            .map(|i| {
                let pct = (i * 100) / 40;
                let idx = i - 24;
                format!("[{pct:>3}%] Building C object CMakeFiles/app.dir/src/unit_{idx:02}.c.o\n")
            })
            .collect::<String>();
        let anchored = format!("$ cmake --build /work/build\n{custom_prefix}{compile_tail}");
        let unanchored = format!("{custom_prefix}{compile_tail}");
        for case in [anchored, unanchored] {
            assert!(BuildLogCompressor::detect(&case));
            let store = InMemoryStore::new();
            let out = BuildLogCompressor.compress_with_recovery(
                &case,
                Some(&store),
                &RecoveryMethod::Shell,
            );
            assert_eq!(out.recoverability, Recoverability::Lossless);
            assert!(out.stash_writes.is_empty());
            assert!(out.output.contains(CMAKE_HEADER));
            assert!(
                out.output
                    .contains(&format!("] {indent}Processing generated build resource 18"))
            );
            assert_eq!(decode_both(&out.output), case);
            assert_eq!(
                BuildLogCompressor
                    .compress_with_recovery(&out.output, Some(&store), &RecoveryMethod::Shell)
                    .output,
                out.output
            );
        }
    }
}

#[test]
fn mixed_stream_keeps_custom_steps_fences_and_controls_while_allowing_anchored_fallback() {
    let compile_head = (1..=8)
        .map(|i| {
            format!(
                "[{i}/20] /usr/bin/cc -O3 -I/work/include -MD -MT CMakeFiles/app.dir/src/f_{i:02}.c.o -MF CMakeFiles/app.dir/src/f_{i:02}.c.o.d -o CMakeFiles/app.dir/src/f_{i:02}.c.o -c /work/src/f_{i:02}.c\n"
            )
        })
        .collect::<String>();
    let custom_tab = (9..=20)
        .map(|i| format!("[{i}/20] \tProcessing asset {i:02} for bundle packaging step\n"))
        .collect::<String>();
    let generic_tail = (1..=14)
        .map(|i| format!("downloading artifact chunk {i:02} of 14 to local cache\n"))
        .collect::<String>();
    let input = format!("$ ninja -v\n{compile_head}{custom_tab}{generic_tail}exit code: 0\n");

    assert!(BuildLogCompressor::detect(&input));
    let store = InMemoryStore::new();
    let outcome =
        BuildLogCompressor.compress_with_recovery(&input, Some(&store), &RecoveryMethod::Shell);
    assert!(outcome.output.contains(COMPILE_HEADER));
    assert!(outcome.output.contains("[15/20] \tProcessing asset 15"));
    assert!(outcome.output.contains("lines omitted."));
    assert_eq!(outcome.recoverability, Recoverability::Retrievable);
    assert_eq!(outcome.stash_writes.len(), 1);
    let marker = outcome
        .output
        .split_inclusive('\n')
        .find(|line| line.contains("lines omitted."))
        .unwrap();
    let recovered = store
        .retrieve(&outcome.stash_writes[0].key)
        .unwrap()
        .unwrap();
    assert_eq!(
        decode_both(&outcome.output.replacen(marker, &recovered, 1)),
        input
    );
}

#[test]
fn leading_custom_step_and_chained_make_fallback_preserve_boundaries() {
    let leading_custom = "[1/13] cd /work/build && /usr/bin/cmake -E copy_if_different /work/src/in.h /work/build/generated.h\n";
    let ninja_compiles = (2..=13)
        .map(|i| {
            format!(
                "[{i}/13] /usr/bin/cc -O2 -I/work/build -MD -MT CMakeFiles/app.dir/src/m_{i:02}.c.o -MF CMakeFiles/app.dir/src/m_{i:02}.c.o.d -o CMakeFiles/app.dir/src/m_{i:02}.c.o -c /work/src/m_{i:02}.c\n"
            )
        })
        .collect::<String>();
    let with_leading_gen = format!("{leading_custom}{ninja_compiles}");
    assert!(detect(&sample(&with_leading_gen)));
    assert!(BuildLogCompressor::detect(&with_leading_gen));
    let gen_out = compress_lossless(&with_leading_gen);
    assert!(gen_out.output.starts_with(leading_custom));
    assert!(gen_out.output.contains(COMPILE_HEADER));
    assert!(gen_out.stash_writes.is_empty());
    assert_eq!(gen_out.recoverability, Recoverability::Lossless);
    assert_eq!(decode_both(&gen_out.output), with_leading_gen);

    let custom_only = concat!(
        "[1/3] cd /work/build && /usr/bin/cmake -E copy_if_different /work/src/in.h /work/build/generated.h\n",
        "[2/3] Generating custom manifest\n",
        "[3/3] Custom finish step\n",
    );
    assert!(!detect(&sample(custom_only)));

    let ninja_head = (1..=6)
        .map(|i| {
            format!(
                "[{i}/6] /usr/bin/cc -O2 -MD -MT CMakeFiles/n.dir/s_{i:02}.c.o -MF CMakeFiles/n.dir/s_{i:02}.c.o.d -o CMakeFiles/n.dir/s_{i:02}.c.o -c /work/s_{i:02}.c\n"
            )
        })
        .collect::<String>();
    let chained_recipes = (1..=12)
        .map(|i| {
            format!(
                "cd /work/project && /usr/bin/gcc -O2 -Wall -c src/a_{i:02}.c -o obj/a_{i:02}.o && cp obj/a_{i:02}.o stage/a_{i:02}.o\n"
            )
        })
        .collect::<String>();
    let ninja_then_chained = format!(
        "{ninja_head}\
         make: Entering directory '/work/project'\n\
         {chained_recipes}\
         make: Leaving directory '/work/project'\n"
    );
    let store = InMemoryStore::new();
    let chained_out = BuildLogCompressor.compress_with_recovery(
        &ninja_then_chained,
        Some(&store),
        &RecoveryMethod::Shell,
    );
    assert!(chained_out.output.contains(COMPILE_HEADER));
    assert!(chained_out.output.contains(&chained_recipes));
    assert_eq!(chained_out.recoverability, Recoverability::Lossless);
    assert!(chained_out.stash_writes.is_empty());
    assert_eq!(store.len(), 0);
    assert_eq!(decode_both(&chained_out.output), ninja_then_chained);

    let standalone_chained = format!(
        "make: Entering directory '/work/proj'\n{}\
         make: Leaving directory '/work/proj'\n",
        (1..=12)
            .map(|i| {
                format!(
                    "/usr/bin/gcc -O2 -Wall -c src/m_{i:02}.c -o build/m_{i:02}.o && echo built m_{i:02}\n"
                )
            })
            .collect::<String>()
    );
    let standalone_store = InMemoryStore::new();
    let standalone_out = BuildLogCompressor.compress_with_recovery(
        &standalone_chained,
        Some(&standalone_store),
        &RecoveryMethod::Shell,
    );
    assert_eq!(standalone_out.recoverability, Recoverability::Lossless);
    assert!(standalone_out.stash_writes.is_empty());
    assert_eq!(standalone_out.output, standalone_chained);

    let make_link_routine = (1..=16)
        .map(|i| format!("gcc -O2 -Wall -o build/tool_{i:02} obj/tool_{i:02}.o -lm\n"))
        .collect::<String>();
    let mixed_chained_and_unrelated = format!(
        "{ninja_head}\
         make: Entering directory '/work/project'\n\
         {chained_recipes}\
         {make_link_routine}\
         make: Leaving directory '/work/project'\n"
    );
    let mixed_store = InMemoryStore::new();
    let mixed_out = BuildLogCompressor.compress_with_recovery(
        &mixed_chained_and_unrelated,
        Some(&mixed_store),
        &RecoveryMethod::Shell,
    );
    assert!(mixed_out.output.contains(COMPILE_HEADER));
    assert!(mixed_out.output.contains(&chained_recipes));
    assert!(mixed_out.output.contains("lines omitted."));
    assert_eq!(mixed_out.recoverability, Recoverability::Retrievable);
    assert_eq!(mixed_out.stash_writes.len(), 1);
    let marker = mixed_out
        .output
        .split_inclusive('\n')
        .find(|line| line.contains("lines omitted."))
        .unwrap();
    let recovered = mixed_store
        .retrieve(&mixed_out.stash_writes[0].key)
        .unwrap()
        .unwrap();
    assert_eq!(
        decode_both(&mixed_out.output.replacen(marker, &recovered, 1)),
        mixed_chained_and_unrelated
    );
}

#[test]
fn mixed_ninja_progress_and_unprofitable_compiles_keep_cmake_build_compression() {
    let progress_first = format!(
        "{}{}",
        (1..=24)
            .map(|i| format!("[{i}/32] Building C object CMakeFiles/app.dir/mod_{i:02}.c.o\n"))
            .collect::<String>(),
        (25..=32)
            .map(|i| format!(
                "[{i}/32] /usr/bin/cc -O3 -I/work/include -MD -MT CMakeFiles/app.dir/src/f_{i:02}.c.o -MF CMakeFiles/app.dir/src/f_{i:02}.c.o.d -o CMakeFiles/app.dir/src/f_{i:02}.c.o -c /work/src/f_{i:02}.c\n"
            ))
            .collect::<String>()
    );
    let compile_first = format!(
        "{}{}",
        (1..=8)
            .map(|i| format!(
                "[{i}/32] /usr/bin/cc -O3 -I/work/include -MD -MT CMakeFiles/app.dir/src/f_{i:02}.c.o -MF CMakeFiles/app.dir/src/f_{i:02}.c.o.d -o CMakeFiles/app.dir/src/f_{i:02}.c.o -c /work/src/f_{i:02}.c\n"
            ))
            .collect::<String>(),
        (9..=32)
            .map(|i| format!("[{i}/32] Building C object CMakeFiles/app.dir/mod_{i:02}.c.o\n"))
            .collect::<String>()
    );
    let store = InMemoryStore::new();
    for mixed_both in [progress_first, compile_first] {
        assert!(BuildLogCompressor::detect(&mixed_both));
        let composed = BuildLogCompressor.compress_with_recovery(
            &mixed_both,
            Some(&store),
            &RecoveryMethod::Shell,
        );
        assert!(composed.output.contains(COMPILE_HEADER));
        assert!(composed.output.contains(CMAKE_HEADER));
        assert!(composed.stash_writes.is_empty());
        assert_eq!(composed.recoverability, Recoverability::Lossless);
        assert_eq!(decode_both(&composed.output), mixed_both);
        let again = BuildLogCompressor.compress_with_recovery(
            &composed.output,
            Some(&store),
            &RecoveryMethod::Shell,
        );
        assert_eq!(again.output, composed.output);
    }

    let tail_two_unprofitable = concat!(
        "[201/202] /usr/bin/cc -O1 -DVARIANT=1 -MD -MT CMakeFiles/app.dir/v1.c.o -MF CMakeFiles/app.dir/v1.c.o.d -o CMakeFiles/app.dir/v1.c.o -c /work/src/v1.c\n",
        "[202/202] /usr/bin/cc -O2 -DVARIANT=2 -MD -MT CMakeFiles/app.dir/v2.c.o -MF CMakeFiles/app.dir/v2.c.o.d -o CMakeFiles/app.dir/v2.c.o -c /work/src/v2.c\n",
    );
    let tail_window_mixed = format!(
        "{}{tail_two_unprofitable}",
        (1..=200)
            .map(|i| format!("[{i}/202] Building C object CMakeFiles/app.dir/mod_{i:03}.c.o\n"))
            .collect::<String>()
    );
    assert!(BuildLogCompressor::detect(&tail_window_mixed));
    let tail_out = BuildLogCompressor.compress_with_recovery(
        &tail_window_mixed,
        Some(&store),
        &RecoveryMethod::Shell,
    );
    assert!(tail_out.output.contains(CMAKE_HEADER));
    assert!(!tail_out.output.contains(COMPILE_HEADER));
    assert!(tail_out.output.contains(tail_two_unprofitable));
    assert_eq!(decode_both(&tail_out.output), tail_window_mixed);

    let interleaved_verbose = (1..=200)
        .map(|i| {
            if i % 20 == 0 {
                format!(
                    "[{i}/200] /usr/bin/cc -O3 -I/work/include -MD -MT CMakeFiles/app.dir/src/f_{i:03}.c.o -MF CMakeFiles/app.dir/src/f_{i:03}.c.o.d -o CMakeFiles/app.dir/src/f_{i:03}.c.o -c /work/src/f_{i:03}.c\n"
                )
            } else {
                format!("[{i}/200] Building C object CMakeFiles/app.dir/mod_{i:03}.c.o\n")
            }
        })
        .collect::<String>();
    let interleaved_out = BuildLogCompressor.compress_with_recovery(
        &interleaved_verbose,
        Some(&store),
        &RecoveryMethod::Shell,
    );
    assert!(interleaved_out.output.contains(CMAKE_HEADER));
    assert!(!interleaved_out.output.contains(COMPILE_HEADER));
    assert_eq!(decode_both(&interleaved_out.output), interleaved_verbose);
}

#[test]
fn ninja_v_never_sticks_to_percent_progress_and_peels_abutting_make_progress() {
    let mut progress = String::new();
    for i in 1..=24 {
        let pct = (i * 100) / 60;
        progress.push_str(&format!(
            "[{pct:>3}%] Building C object CMakeFiles/libalpha.dir/src/alpha/unit_{i:02}.c.o\n"
        ));
    }
    for i in 25..=48 {
        let pct = (i * 100) / 60;
        progress.push_str(&format!(
            "[{pct:>3}%] Building C object CMakeFiles/libbeta.dir/src/beta/unit_{i:02}.c.o\n"
        ));
    }
    let ninja_v = (49..=60)
        .map(|i| {
            format!(
                "[{i}/60] /usr/bin/cc -O3 -I/work/include -MD -MT CMakeFiles/app.dir/src/f_{i:02}.c.o -MF CMakeFiles/app.dir/src/f_{i:02}.c.o.d -o CMakeFiles/app.dir/src/f_{i:02}.c.o -c /work/src/f_{i:02}.c\n"
            )
        })
        .collect::<String>();

    let store = InMemoryStore::new();
    for input in [
        format!("{progress}{ninja_v}"),
        format!("{progress}gmake[1]: Leaving directory '/work/build'\n{ninja_v}"),
    ] {
        let out =
            BuildLogCompressor.compress_with_recovery(&input, Some(&store), &RecoveryMethod::Shell);
        assert!(out.output.contains(CMAKE_HEADER));
        assert!(out.output.contains(COMPILE_HEADER));
        assert!(out.output.contains("libalpha.dir/src/alpha/"));
        assert!(out.output.contains("libbeta.dir/src/beta/"));
        let cc_header_line = out
            .output
            .lines()
            .find(|line| line.starts_with(COMPILE_HEADER))
            .unwrap();
        let cc_json: serde_json::Value =
            serde_json::from_str(cc_header_line.trim_start_matches(COMPILE_HEADER)).unwrap();
        assert_eq!(cc_json["rows"], 12);
        assert_eq!(decode_both(&out.output), input);
    }

    // Pure [PPP%] abutting Make VERBOSE=1 at head and tail (even without Linking/Built target) peels cleanly.
    let make_verbose = (25..=32)
        .map(|i| {
            let pct = (i * 100) / 40;
            format!(
                "[{pct:>3}%] Building C object CMakeFiles/app.dir/src/f_{i:02}.c.o\n\
                 /usr/bin/cc -O3 -I/work/include -MD -MT CMakeFiles/app.dir/src/f_{i:02}.c.o -MF CMakeFiles/app.dir/src/f_{i:02}.c.o.d -o CMakeFiles/app.dir/src/f_{i:02}.c.o -c /work/src/f_{i:02}.c\n"
            )
        })
        .collect::<String>();
    let tail_progress = (33..=40)
        .map(|i| {
            let pct = (i * 100) / 40;
            format!(
                "[{pct:>3}%] Building C object CMakeFiles/libgamma.dir/src/gamma/unit_{i:02}.c.o\n"
            )
        })
        .collect::<String>();
    let abutted = format!("{progress}{make_verbose}{tail_progress}");
    let abutted_out =
        BuildLogCompressor.compress_with_recovery(&abutted, Some(&store), &RecoveryMethod::Shell);
    assert_eq!(abutted_out.output.matches(CMAKE_HEADER).count(), 2);
    assert_eq!(abutted_out.output.matches(COMPILE_HEADER).count(), 1);
    assert_eq!(decode_both(&abutted_out.output), abutted);
}

#[test]
fn parallel_j4_verbose_and_catch2_cd_mt_merge_into_single_block() {
    // Interleaved 40-file `-j4 -- VERBOSE=1` stream: 4 progress lines followed by 4 compile lines in shuffled order.
    let mut parallel_j4 = String::new();
    for batch in 0..10 {
        for offset in 0..4 {
            let i = batch * 4 + offset + 1;
            let pct = (i * 100) / 40;
            parallel_j4.push_str(&format!(
                "[{pct:>3}%] Building C object CMakeFiles/brotlienc.dir/c/enc/unit_{i:02}.c.o\n"
            ));
        }
        for offset in [2usize, 0, 3, 1] {
            let i = batch * 4 + offset + 1;
            parallel_j4.push_str(&format!(
                "/usr/bin/cc -DBROTLIENC_SHARED_COMPILATION -I/work/source/c/include -O3 -DNDEBUG -fPIC -MD -MT CMakeFiles/brotlienc.dir/c/enc/unit_{i:02}.c.o -MF CMakeFiles/brotlienc.dir/c/enc/unit_{i:02}.c.o.d -o CMakeFiles/brotlienc.dir/c/enc/unit_{i:02}.c.o -c /work/source/c/enc/unit_{i:02}.c\n"
            ));
        }
    }
    parallel_j4.push_str("[100%] Linking C shared library libbrotlienc.so\n");
    parallel_j4.push_str("[100%] Built target brotlienc\n");

    let store = InMemoryStore::new();
    let j4_out = BuildLogCompressor.compress_with_recovery(
        &parallel_j4,
        Some(&store),
        &RecoveryMethod::Shell,
    );
    assert_eq!(j4_out.output.matches(COMPILE_HEADER).count(), 1);
    assert!(!j4_out.output.contains(CMAKE_HEADER));
    assert!(
        j4_out
            .output
            .contains("[100%] Linking C shared library libbrotlienc.so\n")
    );
    assert!(j4_out.output.contains("[100%] Built target brotlienc\n"));
    let j4_json: serde_json::Value = serde_json::from_str(
        j4_out
            .output
            .lines()
            .find(|line| line.starts_with(COMPILE_HEADER))
            .unwrap()
            .trim_start_matches(COMPILE_HEADER),
    )
    .unwrap();
    assert_eq!(j4_json["rows"], 80);
    assert_eq!(decode_both(&j4_out.output), parallel_j4);
    assert!(
        tokenless_protocol::estimate_tokens(&j4_out.output) * 3
            < tokenless_protocol::estimate_tokens(&parallel_j4)
    );

    // Catch2-style `cd /work/build-make/src && ... -MT src/CMakeFiles/... -o CMakeFiles/...`
    // pairs via `-MT` while rejecting wrong subdirs when `-MT` is absent.
    let mut catch2_with_mt = String::new();
    for i in 1..=6 {
        let pct = (i * 100) / 6;
        catch2_with_mt.push_str(&format!(
            "[{pct:>3}%] Building CXX object src/CMakeFiles/Catch2.dir/catch2/unit_{i:02}.cpp.o\n"
        ));
    }
    for i in (1..=6).rev() {
        catch2_with_mt.push_str(&format!(
            "cd /work/build-make/src && /usr/bin/c++ -I/work/source/src -O3 -MD -MT src/CMakeFiles/Catch2.dir/catch2/unit_{i:02}.cpp.o -MF CMakeFiles/Catch2.dir/catch2/unit_{i:02}.cpp.o.d -o CMakeFiles/Catch2.dir/catch2/unit_{i:02}.cpp.o -c /work/source/src/catch2/unit_{i:02}.cpp\n"
        ));
    }
    let catch2_out = compress_lossless(&catch2_with_mt);
    assert_eq!(catch2_out.output.matches(COMPILE_HEADER).count(), 1);
    assert!(!catch2_out.output.contains(CMAKE_HEADER));
    assert_eq!(decode_both(&catch2_out.output), catch2_with_mt);

    let wrong_subdir = concat!(
        "[ 25%] Building CXX object other/CMakeFiles/Catch2.dir/catch2/a.cpp.o\n",
        "[ 50%] Building CXX object other/CMakeFiles/Catch2.dir/catch2/b.cpp.o\n",
        "[ 75%] Building CXX object other/CMakeFiles/Catch2.dir/catch2/c.cpp.o\n",
        "[100%] Building CXX object other/CMakeFiles/Catch2.dir/catch2/d.cpp.o\n",
        "cd /work/build-make/src && /usr/bin/c++ -I/work/source/src -O3 -o CMakeFiles/Catch2.dir/catch2/a.cpp.o -c /work/source/src/catch2/a.cpp\n",
        "cd /work/build-make/src && /usr/bin/c++ -I/work/source/src -O3 -o CMakeFiles/Catch2.dir/catch2/b.cpp.o -c /work/source/src/catch2/b.cpp\n",
        "cd /work/build-make/src && /usr/bin/c++ -I/work/source/src -O3 -o CMakeFiles/Catch2.dir/catch2/c.cpp.o -c /work/source/src/catch2/c.cpp\n",
        "cd /work/build-make/src && /usr/bin/c++ -I/work/source/src -O3 -o CMakeFiles/Catch2.dir/catch2/d.cpp.o -c /work/source/src/catch2/d.cpp\n",
    );
    let wrong_out = compress_lossless(wrong_subdir);
    assert!(wrong_out.output.contains(CMAKE_HEADER));
    assert!(wrong_out.output.contains(COMPILE_HEADER));
    assert_eq!(decode_both(&wrong_out.output), wrong_subdir);
}

#[test]
fn chunk_cut_boundary_rewinds_pending_paired_labels_and_cost_bucketing_avoids_bloat() {
    // Exceed MAX_BYTES inside one parallel CompileGroup so a trailing `[PPP%]` whose `Compile`
    // fell past 64 KiB gets peeled into the second chunk alongside its matching `Compile`.
    let long_flags = format!("-I/work/{}", "include/".repeat(70));
    let mut big_parallel = String::new();
    for batch in 0..30 {
        for offset in 0..4 {
            let i = batch * 4 + offset + 1;
            let pct = (i * 100) / 120;
            big_parallel.push_str(&format!(
                "[{pct:>3}%] Building C object CMakeFiles/app.dir/src/mod_{i:03}.c.o\n"
            ));
        }
        for offset in 0..4 {
            let i = batch * 4 + offset + 1;
            big_parallel.push_str(&format!(
                "/usr/bin/cc {long_flags} -MD -MT CMakeFiles/app.dir/src/mod_{i:03}.c.o -MF CMakeFiles/app.dir/src/mod_{i:03}.c.o.d -o CMakeFiles/app.dir/src/mod_{i:03}.c.o -c /work/src/mod_{i:03}.c\n"
            ));
        }
    }
    let cut_out = compress_lossless(&big_parallel);
    assert_eq!(cut_out.output.matches(COMPILE_HEADER).count(), 2);
    assert!(!cut_out.output.contains(CMAKE_HEADER));
    assert_eq!(decode_both(&cut_out.output), big_parallel);

    // Cost-aware bucketing: 20 multi-dir short-template rows bucket cleanly; long-flag zstd-like rows stay 1 template.
    let mut two_dir_make = String::new();
    for i in 1..=10 {
        two_dir_make.push_str(&format!(
            "[ 10%] Building C object CMakeFiles/alpha.dir/c/alpha/unit_{i:02}.c.o\n\
             /usr/bin/cc -DCOMMON -O3 -MD -MT CMakeFiles/alpha.dir/c/alpha/unit_{i:02}.c.o -MF CMakeFiles/alpha.dir/c/alpha/unit_{i:02}.c.o.d -o CMakeFiles/alpha.dir/c/alpha/unit_{i:02}.c.o -c /work/c/alpha/unit_{i:02}.c\n"
        ));
    }
    for i in 11..=20 {
        two_dir_make.push_str(&format!(
            "[ 50%] Building C object CMakeFiles/beta.dir/c/beta/unit_{i:02}.c.o\n\
             /usr/bin/cc -DCOMMON -O3 -MD -MT CMakeFiles/beta.dir/c/beta/unit_{i:02}.c.o -MF CMakeFiles/beta.dir/c/beta/unit_{i:02}.c.o.d -o CMakeFiles/beta.dir/c/beta/unit_{i:02}.c.o -c /work/c/beta/unit_{i:02}.c\n"
        ));
    }
    let bucket_out = compress_lossless(&two_dir_make);
    assert!(bucket_out.output.contains("CMakeFiles/alpha.dir/c/alpha/"));
    assert!(bucket_out.output.contains("CMakeFiles/beta.dir/c/beta/"));
    assert_eq!(decode_both(&bucket_out.output), two_dir_make);

    let zstd_long_flags = format!("-O3 -Wall {}", "-Wextra-flag-padding ".repeat(22));
    let mut zstd_two_dirs = String::from("make: Entering directory '/work/lib'\n");
    for (idx, subdir) in [
        "common", "common", "common", "compress", "compress", "compress",
    ]
    .into_iter()
    .enumerate()
    {
        zstd_two_dirs.push_str(&format!(
            "CC obj/static/u_{idx:02}.o\n\
             cc {zstd_long_flags} -c -MMD -MP -o obj/static/u_{idx:02}.o /work/lib/{subdir}/u_{idx:02}.c\n"
        ));
    }
    let zstd_out = compress_lossless(&zstd_two_dirs);
    let cc_json: serde_json::Value = serde_json::from_str(
        zstd_out
            .output
            .lines()
            .find(|line| line.starts_with(COMPILE_HEADER))
            .unwrap()
            .trim_start_matches(COMPILE_HEADER),
    )
    .unwrap();
    // 2 templates total (1 for CC tags, 1 for cc commands; long-flag dirs are NOT duplicated).
    assert_eq!(cc_json["literals"].as_array().unwrap().len(), 2);
    assert_eq!(decode_both(&zstd_out.output), zstd_two_dirs);
}

fn assert_make_and_ninja_compose_symmetrically(make_part: &str, ninja_part: &str) {
    let store = InMemoryStore::new();
    let make_first = format!("{make_part}{ninja_part}");
    let ninja_first = format!("{ninja_part}{make_part}");

    let mut token_counts = Vec::new();
    for mixed in [make_first, ninja_first] {
        assert!(BuildLogCompressor::detect(&mixed));
        let out =
            BuildLogCompressor.compress_with_recovery(&mixed, Some(&store), &RecoveryMethod::Shell);
        assert!(out.output.contains(COMPILE_HEADER));
        assert!(out.output.contains(CMAKE_HEADER));
        assert!(out.stash_writes.is_empty());
        assert_eq!(out.recoverability, Recoverability::Lossless);
        assert!(
            tokenless_protocol::estimate_tokens(&out.output)
                < tokenless_protocol::estimate_tokens(&mixed)
        );
        assert_eq!(decode_both(&out.output), mixed);
        let again = BuildLogCompressor.compress_with_recovery(
            &out.output,
            Some(&store),
            &RecoveryMethod::Shell,
        );
        assert_eq!(again.output, out.output);
        token_counts.push(tokenless_protocol::estimate_tokens(&out.output));
    }
    assert_eq!(token_counts[0], token_counts[1]);
}

#[test]
fn zstd_make_and_ninja_mixed_logs_compose_symmetrically_in_either_order() {
    let fixtures_root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/build_logs");
    let zstd =
        std::fs::read_to_string(fixtures_root.join("cc_compile/zstd_make_lib_v1_gcc.txt")).unwrap();
    let brotli =
        std::fs::read_to_string(fixtures_root.join("cmake_build/brotli_ninja_cold.txt")).unwrap();
    assert_make_and_ninja_compose_symmetrically(&zstd, &brotli);

    let ninja_blank = (1..=24)
        .map(|i| format!("[{i}/24] Building C object CMakeFiles/app.dir/src/mod_{i:02}.c.o\n\n"))
        .collect::<String>();
    assert_make_and_ninja_compose_symmetrically(&zstd, &ninja_blank);
}

#[test]
fn progress_compaction_survives_barrier_splits_and_rejected_compile_spans() {
    // Case A: `[PPP%] Generating gen.h` splits matching `[PPP%] Building ...` labels from
    // their compile commands; those unclaimed progress rows must still merge into `CMAKE_HEADER`
    // alongside the `COMPILE_HEADER` block.
    let mut split_log = String::new();
    for i in 1..=4 {
        let pct = i * 10;
        split_log.push_str(&format!(
            "[{pct:>3}%] Building C object CMakeFiles/app.dir/src/u_{i:02}.c.o\n"
        ));
    }
    split_log.push_str("[ 50%] Generating gen.h\n");
    for i in 1..=4 {
        split_log.push_str(&format!(
            "/usr/bin/cc -O3 -DCOMMON -MD -MT CMakeFiles/app.dir/src/u_{i:02}.c.o -MF CMakeFiles/app.dir/src/u_{i:02}.c.o.d -o CMakeFiles/app.dir/src/u_{i:02}.c.o -c /work/src/u_{i:02}.c\n"
        ));
    }
    let split_out = compress_lossless(&split_log);
    assert_eq!(split_out.output.matches(CMAKE_HEADER).count(), 1);
    assert_eq!(split_out.output.matches(COMPILE_HEADER).count(), 1);
    assert!(split_out.stash_writes.is_empty());
    assert_eq!(split_out.recoverability, Recoverability::Lossless);
    assert_eq!(decode_both(&split_out.output), split_log);
    assert_eq!(
        compress_lossless(&split_out.output).output,
        split_out.output
    );

    // Case B: Combined compile span (6 paired `[PPP%] Building ...` rows + 16 unshared compile
    // templates) fails `saves_both` in `render_compile`; the 6 progress rows must still fall back
    // to `CMAKE_HEADER`.
    let mut rejected_compile_log = String::new();
    for i in 1..=6 {
        let pct = (i * 100) / 16;
        rejected_compile_log.push_str(&format!(
            "[{pct:>3}%] Building C object CMakeFiles/app.dir/src/u_{i:02}.c.o\n"
        ));
    }
    for i in 1..=16 {
        rejected_compile_log.push_str(&format!(
            "cc -DUNIQUE_{i}={i} -O{opt} -MD -MT CMakeFiles/app.dir/src/u_{i:02}.c.o -MF CMakeFiles/app.dir/src/u_{i:02}.c.o.d -o CMakeFiles/app.dir/src/u_{i:02}.c.o -c /work/src/u_{i:02}.c\n",
            opt = i % 3
        ));
    }
    let rejected_out = compress_lossless(&rejected_compile_log);
    assert!(rejected_out.output.contains(CMAKE_HEADER));
    assert!(!rejected_out.output.contains(COMPILE_HEADER));
    assert!(rejected_out.stash_writes.is_empty());
    assert_eq!(rejected_out.recoverability, Recoverability::Lossless);
    assert_eq!(decode_both(&rejected_out.output), rejected_compile_log);
    assert_eq!(
        compress_lossless(&rejected_out.output).output,
        rejected_out.output
    );
}

#[test]
fn unmatched_make_tags_do_not_hijack_bare_handwritten_make_commands() {
    let ninja_head = (1..=8)
        .map(|i| format!("[{i}/8] Building C object CMakeFiles/n.dir/src/n_{i:02}.c.o\n"))
        .collect::<String>();
    let bare_make = (1..=16)
        .map(|i| format!("cc -O2 -Wall -Iinclude -c src/m_{i:02}.c -o build/m_{i:02}.o\n"))
        .collect::<String>();
    let ninja_plus_orphan_make = format!(
        "{ninja_head}\
         make: Entering directory '/work/handwritten'\n\
         CC orphan.o\n\
         {bare_make}\
         make: Leaving directory '/work/handwritten'\n"
    );
    let store = InMemoryStore::new();
    let mixed = BuildLogCompressor.compress_with_recovery(
        &ninja_plus_orphan_make,
        Some(&store),
        &RecoveryMethod::Shell,
    );
    assert!(!mixed.output.contains(COMPILE_HEADER));
    assert!(mixed.output.contains(CMAKE_HEADER));
    assert!(mixed.output.contains("CC orphan.o\n"));
    assert_eq!(mixed.recoverability, Recoverability::Retrievable);
    assert_eq!(mixed.stash_writes.len(), 1);
    let marker = mixed
        .output
        .split_inclusive('\n')
        .find(|line| line.contains("lines omitted."))
        .unwrap();
    let recovered = store.retrieve(&mixed.stash_writes[0].key).unwrap().unwrap();
    assert_eq!(
        decode_both(&mixed.output.replacen(marker, &recovered, 1)),
        ninja_plus_orphan_make
    );
    assert_eq!(
        BuildLogCompressor
            .compress_with_recovery(&mixed.output, Some(&store), &RecoveryMethod::Shell)
            .output,
        mixed.output
    );

    let verbose_head = (1..=6)
        .map(|i| {
            format!(
                "/usr/bin/cc -O3 -I/work/inc -MD -MT CMakeFiles/app.dir/src/v_{i:02}.c.o -MF CMakeFiles/app.dir/src/v_{i:02}.c.o.d -o CMakeFiles/app.dir/src/v_{i:02}.c.o -c /work/src/v_{i:02}.c\n"
            )
        })
        .collect::<String>();
    let verbose_plus_orphan = format!(
        "{verbose_head}\
         CC orphan.o\n\
         cc -O2 -Wall -c src/b1.c -o build/b1.o\n\
         cc -O2 -Wall -c src/b2.c -o build/b2.o\n"
    );
    let verbose_out = compress_lossless(&verbose_plus_orphan);
    assert_eq!(verbose_out.output.matches(COMPILE_HEADER).count(), 1);
    assert!(verbose_out.output.contains("CC orphan.o\n"));
    assert!(
        verbose_out
            .output
            .contains("cc -O2 -Wall -c src/b1.c -o build/b1.o\n")
    );
    assert_eq!(decode_both(&verbose_out.output), verbose_plus_orphan);
}

#[test]
fn compile_target_index_matches_mt_and_output_obj_with_dot_slash_normalization() {
    let lines = [
        "cd /work/build/pkg && /usr/bin/cc -O3 -DCOMMON -MD -MT pkg/obj/with_mt.o -MF obj/with_mt.o.d -o obj/with_mt.o -c /work/src/a.c",
        "/usr/bin/cc -O3 -DCOMMON -o ./obj/dot_rel.o -c /work/src/b.c",
        "cd /work/build/pkg && /usr/bin/cc -O3 -DCOMMON -o obj/no_mt.o -c /work/src/c.c",
    ];
    let parsed = lines
        .into_iter()
        .filter_map(|line| match grammar::parse_line(line) {
            Some(grammar::LineKind::Compile(row)) => Some(row),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(parsed.len(), 3);

    let mut index = grammar::CompileTargetIndex::default();
    for row in &parsed {
        index.insert(row);
    }
    assert!(index.matches("pkg/obj/with_mt.o"));
    assert!(index.matches("./pkg/obj/with_mt.o"));
    assert!(index.matches("obj/dot_rel.o"));
    assert!(index.matches("./obj/dot_rel.o"));
    assert!(index.matches("obj/no_mt.o"));
    assert!(!index.matches("pkg/obj/no_mt.o"));
    assert!(!index.matches("obj/missing.o"));
}

#[test]
fn unprofitable_ninja_compiles_stay_out_of_stash_and_window_bounds_large_runs() {
    // Finding 1: unprofitable `[k/N]` compile commands must stay protected from Stage 2 Generic
    // Stash omission even when numbered tokens resemble a dominant Generic template.
    let mut unprofitable_ninja = String::from("$ cmake --build /work/build\n");
    for i in 1..=14 {
        unprofitable_ninja.push_str(&format!(
            "[{i}/14] /usr/bin/cc -DUNIT_{i}={i} -O{opt} -MD -MT CMakeFiles/app.dir/building_{i:02}.c.o -MF CMakeFiles/app.dir/building_{i:02}.c.o.d -o CMakeFiles/app.dir/building_{i:02}.c.o -c /work/src/building_{i:02}.c\n",
            opt = i % 3
        ));
    }
    unprofitable_ninja.push_str("exit code: 0\n");

    let store = InMemoryStore::new();
    let unprofitable_out = BuildLogCompressor.compress_with_recovery(
        &unprofitable_ninja,
        Some(&store),
        &RecoveryMethod::Shell,
    );
    assert_eq!(unprofitable_out.output, unprofitable_ninja);
    assert_eq!(unprofitable_out.recoverability, Recoverability::Lossless);
    assert!(unprofitable_out.stash_writes.is_empty());
    assert_eq!(store.len(), 0);

    // Finding 2 (Case A): huge Make-style progress run stops at MAX_EDITS with disjoint sorted edits.
    let huge_total = MAX_EDITS * MAX_ROWS + 2000;
    let huge_progress = (1..=huge_total)
        .map(|i| {
            let pct = (i * 100) / huge_total;
            format!("[{pct:>3}%] Building C object CMakeFiles/huge.dir/u_{i:05}.c.o\n")
        })
        .collect::<String>();
    let huge_lines: Vec<&str> = huge_progress.split_inclusive('\n').collect();
    let huge_edits = plan(&huge_lines);
    assert_eq!(huge_edits.len(), MAX_EDITS);
    for pair in huge_edits.windows(2) {
        assert!(pair[0].end <= pair[1].start);
    }

    // Finding 2 (Case B): multi-window (>2*MAX_ROWS) interleaved Make `VERBOSE=1` run round-trips
    // cleanly across the 2x lookahead window commit boundary.
    let total_units = 620usize;
    let mut multi_window_verbose = String::new();
    for batch in 0..(total_units / 4) {
        for offset in 0..4 {
            let i = batch * 4 + offset + 1;
            let pct = (i * 100) / total_units;
            multi_window_verbose.push_str(&format!(
                "[{pct:>3}%] Building C object CMakeFiles/mw.dir/src/mod_{i:04}.c.o\n"
            ));
        }
        for offset in [3usize, 1, 2, 0] {
            let i = batch * 4 + offset + 1;
            multi_window_verbose.push_str(&format!(
                "/usr/bin/cc -O3 -DCOMMON -I/work/inc -MD -MT CMakeFiles/mw.dir/src/mod_{i:04}.c.o -MF CMakeFiles/mw.dir/src/mod_{i:04}.c.o.d -o CMakeFiles/mw.dir/src/mod_{i:04}.c.o -c /work/src/mod_{i:04}.c\n"
            ));
        }
    }
    let mw_out = compress_lossless(&multi_window_verbose);
    assert!(mw_out.output.matches(COMPILE_HEADER).count() >= 5);
    assert!(!mw_out.output.contains(CMAKE_HEADER));
    assert_eq!(decode_both(&mw_out.output), multi_window_verbose);
    assert_eq!(compress_lossless(&mw_out.output).output, mw_out.output);
}

#[test]
fn tilde_fences_and_shell_comments_preserve_safety_boundaries() {
    // Finding 3: tilde fence opener with `/usr/bin/cc` info string immediately after a compile run
    // must stop the run before opening the fence and keep everything inside verbatim.
    let fenced_opener = "~~~/usr/bin/cc -O2 -c /work/src/example.c -o example.o";
    assert!(grammar::parse_line(fenced_opener).is_none());

    let head = (1..=6)
        .map(|i| {
            format!(
                "/usr/bin/cc -O3 -I/work/inc -MD -MT CMakeFiles/a.dir/h_{i:02}.c.o -MF CMakeFiles/a.dir/h_{i:02}.c.o.d -o CMakeFiles/a.dir/h_{i:02}.c.o -c /work/src/h_{i:02}.c\n"
            )
        })
        .collect::<String>();
    let fenced_body = concat!(
        "~~~/usr/bin/cc -O2 -c /work/src/example.c -o example.o\n",
        "/usr/bin/cc -O2 -c /work/src/inside1.c -o inside1.o\n",
        "/usr/bin/cc -O2 -c /work/src/inside2.c -o inside2.o\n",
        "~~~\n",
    );
    let tail = (1..=6)
        .map(|i| {
            format!(
                "/usr/bin/cc -O3 -I/work/inc -MD -MT CMakeFiles/b.dir/t_{i:02}.c.o -MF CMakeFiles/b.dir/t_{i:02}.c.o.d -o CMakeFiles/b.dir/t_{i:02}.c.o -c /work/src/t_{i:02}.c\n"
            )
        })
        .collect::<String>();
    let with_tilde_fence = format!("{head}{fenced_body}{tail}");
    let fence_out = compress_lossless(&with_tilde_fence);
    assert_eq!(fence_out.output.matches(COMPILE_HEADER).count(), 2);
    assert!(fence_out.output.contains(fenced_body));
    assert_eq!(decode_both(&fence_out.output), with_tilde_fence);

    // Finding 4: unquoted `#` shell comments must reject compile detection instead of reading `-c`
    // from commented text, while literal `#` inside flags like `-DFOO=#bar` stays valid.
    assert!(
        grammar::parse_line("[1/12] gcc -o bin/app_01 obj/main_01.o # -c sample_01.c").is_none()
    );
    let comment_link_log = (1..=12)
        .map(|i| format!("[{i}/12] gcc -o bin/app_{i:02} obj/main_{i:02}.o # -c sample_{i:02}.c\n"))
        .collect::<String>();
    assert!(!detect(&sample(&comment_link_log)));
    assert!(!BuildLogCompressor::detect(&comment_link_log));
    let comment_out = compress_lossless(&comment_link_log);
    assert_eq!(comment_out.output, comment_link_log);

    let hash_macro_log = (1..=4)
        .map(|i| {
            format!(
                "[{i}/4] /usr/bin/cc -O2 -DFOO=#bar -MD -MT CMakeFiles/a.dir/m_{i:02}.c.o -MF CMakeFiles/a.dir/m_{i:02}.c.o.d -o CMakeFiles/a.dir/m_{i:02}.c.o -c /work/src/m_{i:02}.c\n"
            )
        })
        .collect::<String>();
    assert!(BuildLogCompressor::detect(&hash_macro_log));
    let hash_out = compress_lossless(&hash_macro_log);
    assert!(hash_out.output.contains(COMPILE_HEADER));
    assert_eq!(decode_both(&hash_out.output), hash_macro_log);
}

#[test]
fn preamble_fences_quoted_flags_interleaved_progress_and_boundary_labels_hold() {
    // Round 3 Finding 1: leading tilde fence with CMake/Make info string must not be skipped
    // as a static preamble, keeping the fenced block verbatim while compacting trailing build output.
    for info in [
        "~~~/usr/bin/cmake -P build.cmake",
        "~~~/usr/bin/make -C build",
        "```/usr/bin/cmake -E rm -f a.o",
    ] {
        let close = if info.starts_with("```") {
            "```"
        } else {
            "~~~"
        };
        let fenced_example = format!(
            "{info}\n\
             [1/6] Building C object a_1.c.o\n\
             [2/6] Building C object a_2.c.o\n\
             [3/6] Building C object a_3.c.o\n\
             [4/6] Building C object a_4.c.o\n\
             [5/6] Building C object a_5.c.o\n\
             [6/6] Linking C executable demo\n\
             {close}\n"
        );
        let real_tail = (1..=6)
            .map(|i| format!("[{i}/6] Building C object real/CMakeFiles/app.dir/unit_{i:02}.c.o\n"))
            .collect::<String>();
        let doc = format!("{fenced_example}{real_tail}");
        assert!(!BuildLogCompressor::detect(&doc), "{info}");
        assert_eq!(compress_lossless(&doc).output, doc, "{info}");

        // Even if the planner runs directly without Stage 0 detection, `leading_preamble_len`
        // must stop at the fence opener so only the trailing real build block is edited.
        let doc_lines: Vec<&str> = doc.split_inclusive('\n').collect();
        let fence_line_count = fenced_example.lines().count();
        let direct_edits = plan(&doc_lines);
        assert_eq!(direct_edits.len(), 1, "{info}");
        assert!(direct_edits[0].start >= fence_line_count, "{info}");

        // When valid build output precedes the fence, full compression compacts both outside
        // blocks while keeping the entire fenced CMake/Make snippet verbatim.
        let doc_mid = format!("{real_tail}{fenced_example}{real_tail}");
        assert!(BuildLogCompressor::detect(&doc_mid), "{info}");
        let mid_out = compress_lossless(&doc_mid);
        assert!(mid_out.output.contains(&fenced_example), "{info}");
        assert_eq!(mid_out.output.matches(CMAKE_HEADER).count(), 2, "{info}");
        assert_eq!(decode_both(&mid_out.output), doc_mid, "{info}");
    }

    // Round 3 Finding 2: `-c` and source extensions inside quoted shell strings or escaped
    // spaces must not trigger compile classification, while real `-c` commands with quoted
    // macro values still compact losslessly.
    for bogus in [
        "[1/12] gcc -DNAME=\"text -c /work/src/example.c trailing\" -o bin/app obj/main.o",
        "[1/12] gcc -DNAME='text -c /work/src/example.c trailing' -o bin/app obj/main.o",
        "[1/12] gcc -DNAME=escaped\\ -c\\ /work/src/example.c -o bin/app obj/main.o",
        "[1/12] gcc -DNAME=\"unclosed -c /work/src/example.c -o bin/app obj/main.o",
        "[1/12] /usr/bin/cc -DNAME=\"hello\" -c /work/src/a.c\\",
    ] {
        assert!(
            grammar::parse_line(bogus).is_none(),
            "expected None: {bogus}"
        );
    }
    let quoted_link_log = (1..=12)
        .map(|i| {
            format!(
                "[{i}/12] gcc -DNAME=\"text -c /work/src/example_{i:02}.c trailing\" -o bin/app_{i:02} obj/main_{i:02}.o\n"
            )
        })
        .collect::<String>();
    assert!(!detect(&sample(&quoted_link_log)));
    assert!(!BuildLogCompressor::detect(&quoted_link_log));
    assert_eq!(compress_lossless(&quoted_link_log).output, quoted_link_log);

    let real_quoted_compile = (1..=4)
        .map(|i| {
            format!(
                "[{i}/4] /usr/bin/cc -DMSG=\"hello \\\"world\\\" -c fake.c\" -O2 -MD -MT CMakeFiles/q.dir/m_{i:02}.c.o -MF CMakeFiles/q.dir/m_{i:02}.c.o.d -o CMakeFiles/q.dir/m_{i:02}.c.o -c /work/src/m_{i:02}.c\n"
            )
        })
        .collect::<String>();
    assert!(BuildLogCompressor::detect(&real_quoted_compile));
    let real_quoted_out = compress_lossless(&real_quoted_compile);
    assert!(real_quoted_out.output.contains(COMPILE_HEADER));
    assert_eq!(decode_both(&real_quoted_out.output), real_quoted_compile);

    // Round 3 Finding 3: interleaved non-verbose `[k/N]` and `[PPP%]` progress shares one
    // `CMake/Ninja templates` block, while Ninja `-v` stays strictly isolated from `[PPP%]`.
    let interleaved_progress = (1..=6)
        .map(|i| {
            let pct = (i * 100) / 6;
            format!(
                "[{i}/6] Building C object ninja/CMakeFiles/n.dir/unit_{i:02}.c.o\n\
                 [{pct:>3}%] Building C object make/CMakeFiles/m.dir/unit_{i:02}.c.o\n"
            )
        })
        .collect::<String>();
    assert!(BuildLogCompressor::detect(&interleaved_progress));
    let interleaved_prog_out = compress_lossless(&interleaved_progress);
    assert_eq!(interleaved_prog_out.output.matches(CMAKE_HEADER).count(), 1);
    assert!(!interleaved_prog_out.output.contains(COMPILE_HEADER));
    assert_eq!(
        decode_both(&interleaved_prog_out.output),
        interleaved_progress
    );
    assert_eq!(
        compress_lossless(&interleaved_prog_out.output).output,
        interleaved_prog_out.output
    );

    // Round 3 Finding 4: at the MAX_ROWS=256 cut boundary, an interior `[PPP%]` label appearing
    // before Chunk 1's last compile commands while its own command lands in Chunk 2 must move
    // cleanly into Chunk 2 alongside its matching command instead of rendering inside Chunk 1.
    let mut chunk_boundary_log = String::new();
    for i in 1..=125 {
        let pct = (i * 100) / 140;
        chunk_boundary_log.push_str(&format!(
            "[{pct:>3}%] Building C object CMakeFiles/app.dir/src/u_{i:03}.c.o\n\
             /usr/bin/cc -O3 -DCOMMON -MD -MT CMakeFiles/app.dir/src/u_{i:03}.c.o -MF CMakeFiles/app.dir/src/u_{i:03}.c.o.d -o CMakeFiles/app.dir/src/u_{i:03}.c.o -c /work/src/u_{i:03}.c\n"
        ));
    }
    // Rows 250..256 sit right across MAX_ROWS=256: labels for 126..127 precede cross_boundary
    // (row 252), followed by compiles for 126..127 (rows 253..254), and compile for
    // cross_boundary falls past row 256.
    chunk_boundary_log.push_str("[ 90%] Building C object CMakeFiles/app.dir/src/u_126.c.o\n");
    chunk_boundary_log.push_str("[ 91%] Building C object CMakeFiles/app.dir/src/u_127.c.o\n");
    chunk_boundary_log
        .push_str("[ 92%] Building C object CMakeFiles/app.dir/src/cross_boundary.c.o\n");
    chunk_boundary_log.push_str(
        "/usr/bin/cc -O3 -DCOMMON -MD -MT CMakeFiles/app.dir/src/u_126.c.o -MF CMakeFiles/app.dir/src/u_126.c.o.d -o CMakeFiles/app.dir/src/u_126.c.o -c /work/src/u_126.c\n",
    );
    chunk_boundary_log.push_str(
        "/usr/bin/cc -O3 -DCOMMON -MD -MT CMakeFiles/app.dir/src/u_127.c.o -MF CMakeFiles/app.dir/src/u_127.c.o.d -o CMakeFiles/app.dir/src/u_127.c.o -c /work/src/u_127.c\n",
    );
    chunk_boundary_log.push_str("[ 93%] Building C object CMakeFiles/app.dir/src/u_128.c.o\n");
    chunk_boundary_log.push_str(
        "/usr/bin/cc -O3 -DCOMMON -MD -MT CMakeFiles/app.dir/src/u_128.c.o -MF CMakeFiles/app.dir/src/u_128.c.o.d -o CMakeFiles/app.dir/src/u_128.c.o -c /work/src/u_128.c\n",
    );
    chunk_boundary_log.push_str(
        "/usr/bin/cc -O3 -DCOMMON -MD -MT CMakeFiles/app.dir/src/cross_boundary.c.o -MF CMakeFiles/app.dir/src/cross_boundary.c.o.d -o CMakeFiles/app.dir/src/cross_boundary.c.o -c /work/src/cross_boundary.c\n",
    );
    let boundary_out = compress_lossless(&chunk_boundary_log);
    assert_eq!(boundary_out.output.matches(COMPILE_HEADER).count(), 2);
    assert!(!boundary_out.output.contains(CMAKE_HEADER));
    assert_eq!(decode_both(&boundary_out.output), chunk_boundary_log);
    let second_hdr_pos = boundary_out.output.rfind(COMPILE_HEADER).unwrap();
    let (chunk1, chunk2) = boundary_out.output.split_at(second_hdr_pos);
    assert!(!chunk1.contains("cross_boundary"));
    let decoded_chunk2 = decode_both(chunk2);
    assert!(
        decoded_chunk2
            .contains("[ 92%] Building C object CMakeFiles/app.dir/src/cross_boundary.c.o\n")
    );
    assert!(decoded_chunk2.contains("-c /work/src/cross_boundary.c\n"));
}

#[test]
fn consolidated_grammar_gates_and_tokenized_lifecycle_predicates_hold() {
    // `-o` / `-MT` / `-MQ` / `-MF` consume their immediately following token without letting `-c`
    // masquerade as the compile flag or output paths masquerade as source inputs.
    for rejected in [
        "/usr/bin/cc -o -c /work/a.c",
        "/usr/bin/cc -c -o /work/out.c",
        "/usr/bin/cc -c -MT /work/out.c",
        "/usr/bin/cc -c -MQ /work/out.c",
        "/usr/bin/cc -c -MF /work/deps.c",
    ] {
        assert!(
            grammar::parse_line(rejected).is_none(),
            "expected None: {rejected}"
        );
    }
    assert!(matches!(
        grammar::parse_line("/usr/bin/cc -c -o \"quoted.o\" /work/src/a.c"),
        Some(grammar::LineKind::Compile(row)) if !row.literals.is_empty()
    ));

    // Real CMake / Make / Ninja link, archive, submake, and preamble steps stay accepted.
    for valid_preamble in [
        "cd /work/build && /usr/bin/cmake -E cmake_depends \"Unix Makefiles\" /work/src /work/build",
        "/usr/bin/cmake -S/work/src -B/work/build --check-build-system CMakeFiles/Makefile.cmake 0",
        "/usr/bin/cmake -S /work/src -B /work/build --check-build-system CMakeFiles/Makefile.cmake 0",
        "make --no-print-directory libzstd.a BUILD_DIR=obj/conf_123 CPPFLAGS=\"-DXXH=1 \"",
        "mkdir -p obj/static",
    ] {
        assert!(
            grammar::is_static_preamble(valid_preamble),
            "expected preamble: {valid_preamble}"
        );
        assert!(
            grammar::is_in_scope_lifecycle(valid_preamble),
            "expected lifecycle: {valid_preamble}"
        );
    }
    for valid_lifecycle in [
        "[8/77] : && /usr/bin/cmake -E rm -f libfmt.a && /usr/bin/ar qc libfmt.a obj/a.o && /usr/bin/ranlib libfmt.a && :",
        ": && /usr/bin/cc -O3 -o app obj/a.o && :",
        ": && cd /work/build && /usr/bin/cc -O3 -o app obj/a.o && :",
        "cd /work/build/src && /usr/bin/cmake -P CMakeFiles/Catch2.dir/cmake_clean_target.cmake",
        "/usr/bin/ar qc libCatch2.a CMakeFiles/Catch2.dir/a.cpp.o",
        "/usr/bin/ranlib libCatch2.a",
    ] {
        assert!(
            grammar::is_in_scope_lifecycle(valid_lifecycle),
            "expected lifecycle: {valid_lifecycle}"
        );
    }

    // Shell chaining outside allowed wrappers, comments, unclosed quotes, and fences fail.
    for rejected_shell in [
        "make all && rm -rf /",
        "mkdir -p foo && echo hi",
        "cd /work/build && /usr/bin/cmake -E cmake_depends \"unclosed",
        "~~~/usr/bin/cmake -P build.cmake",
    ] {
        assert!(
            !grammar::is_static_preamble(rejected_shell),
            "unexpected preamble: {rejected_shell}"
        );
        assert!(
            !grammar::is_in_scope_lifecycle(rejected_shell),
            "unexpected lifecycle: {rejected_shell}"
        );
    }
    assert!(!grammar::is_static_preamble("mkdir -p"));
    for rejected_link in [
        ": && echo \"fake -o target\" && :",
        ": && /usr/bin/cc -o app a.o # comment",
        ": && /usr/bin/cc -O3 -c src/a.c -o a.o && :",
        ": && rm -rf /tmp && :",
        ": && : &&",
        ": && :",
    ] {
        assert!(
            !grammar::is_in_scope_lifecycle(rejected_link),
            "unexpected lifecycle: {rejected_link}"
        );
    }

    // `has_strong_evidence` detects 2+ distinct paired targets (including cd subdirs), while
    // duplicate bare commands matching only 1 `CC` tag stay routed to Stage 2 `Make`.
    let paired_cd = concat!(
        "[ 50%] Building C object pkg_a/obj/a.o\n",
        "cd /work/build/pkg_a && cc -O2 -o obj/a.o -c /work/src/a.c\n",
        "[100%] Building C object pkg_b/obj/b.o\n",
        "cd /work/build/pkg_b && cc -O2 -o obj/b.o -c /work/src/b.c\n",
    );
    assert!(detect(&sample(paired_cd)));

    let two_distinct_cc = concat!(
        "make: Entering directory '/work'\n",
        "CC obj/a.o\n",
        "cc -O2 -Wall -c src/a.c -o obj/a.o\n",
        "CC obj/b.o\n",
        "cc -O2 -Wall -c src/b.c -o obj/b.o\n",
        "make: Leaving directory '/work'\n",
    );
    assert!(detect(&sample(two_distinct_cc)));

    let single_tag_dup_bare = concat!(
        "make: Entering directory '/work'\n",
        "CC obj/a.o\n",
        "cc -O2 -Wall -c src/a.c -o obj/a.o\n",
        "cc -O2 -Wall -c src/a.c -o obj/a.o\n",
        "cc -O2 -Wall -c src/other.c -o obj/other.o\n",
        "make: Leaving directory '/work'\n",
    );
    assert!(!detect(&sample(single_tag_dup_bare)));
    assert!(BuildLogCompressor::detect(single_tag_dup_bare));
}

#[test]
fn joined_options_quoted_paths_and_leading_short_make_compact_losslessly() {
    let store = InMemoryStore::new();

    // 1. Joined `-MF` / `-MT` / `-o` options share paths across templates without mis-splitting
    // unrelated `-o<flag>` options like `-objcmt-migrate`.
    let joined_flags = (1..=6)
        .map(|i| {
            let pct = (i * 100) / 6;
            format!(
                "[{pct:>3}%] Building C object obj/u_{i:02}.o\n\
                 /usr/bin/cc -O3 -DCOMMON -MD -MTobj/u_{i:02}.o -MFobj/u_{i:02}.d -oobj/u_{i:02}.o -c src/u_{i:02}.c\n"
            )
        })
        .collect::<String>();
    assert!(BuildLogCompressor::detect(&joined_flags));
    let joined_out = BuildLogCompressor.compress_with_recovery(
        &joined_flags,
        Some(&store),
        &RecoveryMethod::Shell,
    );
    assert!(joined_out.output.contains(COMPILE_HEADER));
    assert!(!joined_out.output.contains(CMAKE_HEADER));
    assert!(joined_out.stash_writes.is_empty());
    assert_eq!(joined_out.recoverability, Recoverability::Lossless);
    assert!(
        tokenless_protocol::estimate_tokens(&joined_out.output)
            < tokenless_protocol::estimate_tokens(&joined_flags)
    );
    assert_eq!(decode_both(&joined_out.output), joined_flags);
    assert_eq!(
        compress_lossless(&joined_out.output).output,
        joined_out.output
    );

    let joined_standalone = (1..=6)
        .map(|i| {
            format!(
                "cc -O2 -MMD -MTobj/m_{i:02}.o -MFobj/m_{i:02}.d -oobj/m_{i:02}.o -c src/m_{i:02}.c\n"
            )
        })
        .collect::<String>();
    assert!(BuildLogCompressor::detect(&joined_standalone));
    let standalone_out = compress_lossless(&joined_standalone);
    assert!(standalone_out.output.contains(COMPILE_HEADER));
    assert_eq!(decode_both(&standalone_out.output), joined_standalone);

    assert!(grammar::parse_line("cc -c -objcmt-migrate /work/src/a.c").is_some());
    assert!(grammar::parse_line("/usr/bin/cc -oapp obj/main.o").is_none());
    assert!(grammar::is_in_scope_lifecycle(
        "/usr/bin/cc -oapp obj/main.o"
    ));

    // 2. Quoted and backslash-escaped paths with spaces round-trip cleanly and compact losslessly.
    let single_quoted = (1..=6)
        .map(|i| {
            format!(
                "[{i}/6] /usr/bin/cc -O3 -MD -MT '/work/my project/build/u_{i:02}.c.o' -MF '/work/my project/build/u_{i:02}.c.o.d' -o '/work/my project/build/u_{i:02}.c.o' -c '/work/my project/src/u_{i:02}.c'\n"
            )
        })
        .collect::<String>();
    let escaped_space = (1..=6)
        .map(|i| {
            format!(
                "[{i}/6] /usr/bin/cc -O3 -MD -MT /work/my\\ project/build/u_{i:02}.c.o -MF /work/my\\ project/build/u_{i:02}.c.o.d -o /work/my\\ project/build/u_{i:02}.c.o -c /work/my\\ project/src/u_{i:02}.c\n"
            )
        })
        .collect::<String>();
    let quoted_cd = (1..=6)
        .map(|i| {
            format!(
                "cd \"/work/my project/build\" && /usr/bin/cc -O3 -o obj/u_{i:02}.c.o -c \"/work/my project/src/u_{i:02}.c\"\n"
            )
        })
        .collect::<String>();

    for space_log in [single_quoted, escaped_space, quoted_cd] {
        assert!(BuildLogCompressor::detect(&space_log), "{space_log}");
        let out = compress_lossless(&space_log);
        assert!(out.output.contains(COMPILE_HEADER), "{}", out.output);
        assert!(
            tokenless_protocol::estimate_tokens(&out.output)
                < tokenless_protocol::estimate_tokens(&space_log)
        );
        assert_eq!(decode_both(&out.output), space_log);
        assert_eq!(compress_lossless(&out.output).output, out.output);
    }

    // 3. A short leading Make compile recipe followed by verbose compile invocations detects
    // and compacts without hijacking all-short handwritten Make logs.
    let mut mixed_leading_short = String::from("cc -O2 -c tools/gen.c -o gen.o\n");
    for i in 1..=6 {
        mixed_leading_short.push_str(&format!(
            "cc -O3 -MMD -MT obj/u_{i:02}.o -MF obj/u_{i:02}.d -o obj/u_{i:02}.o -c src/u_{i:02}.c\n"
        ));
    }
    assert!(detect(&sample(&mixed_leading_short)));
    assert!(BuildLogCompressor::detect(&mixed_leading_short));
    let mixed_out = BuildLogCompressor.compress_with_recovery(
        &mixed_leading_short,
        Some(&store),
        &RecoveryMethod::Shell,
    );
    assert!(mixed_out.output.contains(COMPILE_HEADER));
    assert!(mixed_out.stash_writes.is_empty());
    assert_eq!(mixed_out.recoverability, Recoverability::Lossless);
    assert_eq!(decode_both(&mixed_out.output), mixed_leading_short);

    let all_short_make = concat!(
        "cc -O2 -c tools/gen.c -o gen.o\n",
        "cc -O2 -c src/a.c -o a.o\n",
        "cc -O2 -c src/b.c -o b.o\n",
    );
    assert!(!detect(&sample(all_short_make)));

    let mut separated_short_make =
        String::from("cc -O2 -c tools/gen.c -o gen.o\nmake: Entering directory '/work/sub'\n");
    for i in 1..=6 {
        separated_short_make.push_str(&format!(
            "cc -O3 -MMD -MT obj/u_{i:02}.o -MF obj/u_{i:02}.d -o obj/u_{i:02}.o -c src/u_{i:02}.c\n"
        ));
    }
    assert!(BuildLogCompressor::detect(&separated_short_make));
    let sep_out = BuildLogCompressor.compress_with_recovery(
        &separated_short_make,
        Some(&store),
        &RecoveryMethod::Shell,
    );
    assert!(sep_out.output.contains(COMPILE_HEADER));
    assert!(sep_out.stash_writes.is_empty());
    assert_eq!(sep_out.recoverability, Recoverability::Lossless);
    assert_eq!(decode_both(&sep_out.output), separated_short_make);
    assert_eq!(
        BuildLogCompressor
            .compress_with_recovery(&sep_out.output, Some(&store), &RecoveryMethod::Shell)
            .output,
        sep_out.output
    );
}

#[test]
fn leading_link_and_archive_make_steps_and_deep_dirs_preserve_routes() {
    // 1. Deeply nested working directories in compile-only logs compact cleanly and losslessly.
    let deep_dir = format!(
        "/{}",
        (1..=120)
            .map(|i| format!("d{i:03}"))
            .collect::<Vec<_>>()
            .join("/")
    );
    let deep_compile_only = (1..=12)
        .map(|i| {
            format!(
                "cd {deep_dir} && /usr/bin/cc -O3 -DCOMMON -o obj/u_{i:02}.o -c /work/src/u_{i:02}.c\n"
            )
        })
        .collect::<String>();
    let deep_out = compress_lossless(&deep_compile_only);
    assert!(deep_out.output.contains(COMPILE_HEADER));
    assert_eq!(decode_both(&deep_out.output), deep_compile_only);

    // 2. Incremental `make V=1` starting with link or archive steps before verbose compiles
    // detects, compacts cleanly, preserves leading steps, and stays idempotent.
    let verbose_block = (1..=10)
        .map(|i| {
            format!(
                "/usr/bin/cc -O3 -DCOMMON -MMD -MT obj/u_{i:02}.o -MF obj/u_{i:02}.d -o obj/u_{i:02}.o -c src/u_{i:02}.c\n"
            )
        })
        .collect::<String>();
    let store = InMemoryStore::new();
    for lead in [
        "make[1]: Entering directory '/work/build'\ncc -o bin/codegen obj/codegen.o\n",
        "make[1]: Entering directory '/work/build'\nar rcs libhelper.a obj/h.o\nranlib libhelper.a\n",
    ] {
        let log = format!("{lead}{verbose_block}make[1]: Leaving directory '/work/build'\n");
        assert!(detect(&sample(&log)), "{lead}");
        let out =
            BuildLogCompressor.compress_with_recovery(&log, Some(&store), &RecoveryMethod::Shell);
        assert!(out.output.starts_with(lead), "{}", out.output);
        assert!(out.output.contains(COMPILE_HEADER), "{}", out.output);
        assert!(out.stash_writes.is_empty());
        assert_eq!(out.recoverability, Recoverability::Lossless);
        assert_eq!(decode_both(&out.output), log);
        assert_eq!(
            BuildLogCompressor
                .compress_with_recovery(&out.output, Some(&store), &RecoveryMethod::Shell)
                .output,
            out.output
        );
    }

    // 3. Negative checks: leading link + short bare Make (or clipped header) stays out of Stage 1.
    let link_then_short_make = concat!(
        "make: Entering directory '/work/build'\n",
        "cc -o bin/codegen obj/codegen.o\n",
        "cc -O2 -Wall -c src/a.c -o obj/a.o\n",
        "cc -O2 -Wall -c src/b.c -o obj/b.o\n",
        "cc -O2 -Wall -c src/c.c -o obj/c.o\n",
        "cc -O2 -Wall -c src/d.c -o obj/d.o\n",
    );
    assert!(!detect(&sample(link_then_short_make)));
    assert!(BuildLogCompressor::detect(link_then_short_make));

    let link_then_clipped = format!(
        "make: Entering directory '/work/build'\n\
         cc -o bin/codegen obj/codegen.o\n\
         {CMAKE_HEADER}{{\"rows\":2,\"literals\":[[\"[\",\"/2] Building C object \",\".c.o\"]]}}\n\
         0\t1\ta\n\
         0\t2\tb\n"
    );
    assert!(!detect(&sample(&link_then_clipped)));
}

#[test]
fn chunk_starting_with_paired_then_unpaired_label_resumes_after_unpaired_label() {
    let long_flags = format!("-I/work/{}", "include/".repeat(70));
    let mut input = String::from(
        "[  1%] Building C object CMakeFiles/app.dir/src/a.c.o\n\
         [  2%] Building C object CMakeFiles/app.dir/src/deferred.c.o\n",
    );
    input.push_str(&format!(
        "/usr/bin/cc {long_flags} -MD -MT CMakeFiles/app.dir/src/a.c.o -MF CMakeFiles/app.dir/src/a.c.o.d -o CMakeFiles/app.dir/src/a.c.o -c /work/src/a.c\n"
    ));
    for i in 1..=50 {
        let pct = 2 + (i * 96) / 50;
        input.push_str(&format!(
            "[{pct:>3}%] Building C object CMakeFiles/app.dir/src/mod_{i:02}.c.o\n\
             /usr/bin/cc {long_flags} -MD -MT CMakeFiles/app.dir/src/mod_{i:02}.c.o -MF CMakeFiles/app.dir/src/mod_{i:02}.c.o.d -o CMakeFiles/app.dir/src/mod_{i:02}.c.o -c /work/src/mod_{i:02}.c\n"
        ));
    }
    input.push_str(&format!(
        "/usr/bin/cc {long_flags} -MD -MT CMakeFiles/app.dir/src/deferred.c.o -MF CMakeFiles/app.dir/src/deferred.c.o.d -o CMakeFiles/app.dir/src/deferred.c.o -c /work/src/deferred.c\n"
    ));

    let out = compress_lossless(&input);
    assert!(out.output.contains(COMPILE_HEADER));
    assert!(!out.output.contains(CMAKE_HEADER));
    assert!(
        tokenless_protocol::estimate_tokens(&out.output) * 2
            < tokenless_protocol::estimate_tokens(&input)
    );
    assert_eq!(decode_both(&out.output), input);
}

#[test]
fn make_dir_notice_boundaries_reject_oversized_control_warning_and_failure_lines() {
    for valid in [
        "make: Entering directory '/work/build'",
        "make[1]: Leaving directory '/work/build'",
        "gmake[2]: Entering directory '/work/build'",
        "mingw32-make: Leaving directory '/work/build'",
    ] {
        assert!(
            grammar::is_make_dir_notice(valid),
            "expected valid dir notice: {valid}"
        );
    }

    let oversized = format!(
        "make: Entering directory '/work/{}'",
        "a".repeat(MAX_LINE_BYTES)
    );
    for invalid in [
        oversized.as_str(),
        "make: Entering directory '/work/\x07bad'",
        "make: warning: Entering directory failed",
        "make[abc]: Entering directory '/work'",
        "make: *** [Makefile:1: all] Entering directory failed",
        "```make: Entering directory '/work'",
    ] {
        assert!(
            !grammar::is_make_dir_notice(invalid),
            "expected rejection for: {invalid:?}"
        );
    }
}
