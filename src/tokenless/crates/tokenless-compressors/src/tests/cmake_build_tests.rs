fn compress_lossless(input: &str) -> crate::build_log::BuildLogOutcome {
    BuildLogCompressor.compress_with_recovery(input, None, &RecoveryMethod::None)
}

fn sample(text: &str) -> Vec<String> {
    sample_lines(text)
}

#[test]
fn single_lines_prose_fences_crlf_and_non_cmake_logs_do_not_trigger_detection() {
    let valid = concat!(
        "[1/3] Building CXX object CMakeFiles/app.dir/a.cpp.o\n",
        "[2/3] Building CXX object CMakeFiles/app.dir/b.cpp.o\n",
        "[3/3] Linking CXX executable app\n",
    );
    for negative in [
        "[1/1] Building CXX object CMakeFiles/app.dir/main.cpp.o\n",
        "[100%] Built target app\n",
        "Building CXX object CMakeFiles/app.dir/main.cpp.o\n",
        "[1/3] Check configuration\n[2/3] Run migration\n[3/3] Done\n",
        "[5/3] Building CXX object CMakeFiles/bad.dir/x.cpp.o\n[6/3] Linking CXX executable bad\n",
        "[2026/11] Building CXX object foo.cpp.o\n[2026/12] Linking CXX executable foo\n",
        "[ 50%] Generating summary report\n[100%] Built target summary\n",
        "[150%] Built target overflow\n",
        "let text = \"[1/2] Building CXX object CMakeFiles/app.dir/a.cpp.o\";\n",
        &format!("```text\n{valid}```\n"),
        &format!("~~~text\n{valid}~~~\n"),
    ] {
        assert!(!detect(&sample(negative)), "false positive on: {negative}");
        assert!(
            !BuildLogCompressor::detect(negative),
            "false positive BuildLogCompressor::detect on: {negative}"
        );
    }

    let crlf = valid.replace('\n', "\r\n");
    assert!(!detect(&sample(&crlf)));
    assert!(!BuildLogCompressor::detect(&crlf));

    let head_padding = format!("{}{valid}", "ordinary output line\n".repeat(120));
    assert!(!detect(&sample(&head_padding)));
    assert!(!BuildLogCompressor::detect(&head_padding));

    let oversized_head = format!("{}\n{valid}", "x".repeat(MAX_LINE_BYTES + 1));
    assert!(!detect(&sample(&oversized_head)));
    assert!(!BuildLogCompressor::detect(&oversized_head));

    let unclosed_fence_gap = format!("```text\n{}{valid}", "example line\n".repeat(120));
    assert!(!detect(&sample(&unclosed_fence_gap)));
    assert!(!BuildLogCompressor::detect(&unclosed_fence_gap));

    // Handwritten Makefiles and `/usr/bin/cmake -S` configure heads must not be claimed by CmakeBuild.
    let handwritten_make = concat!(
        "make: Entering directory '/work'\n",
        "cc -O2 -c src/a.c -o build/a.o\n",
        "cc -O2 -c src/b.c -o build/b.o\n",
    );
    assert!(!detect(&sample(handwritten_make)));
    assert!(BuildLogCompressor::detect(handwritten_make));
    let store = InMemoryStore::new();
    let make_outcome = BuildLogCompressor.compress_with_recovery(
        handwritten_make,
        Some(&store),
        &RecoveryMethod::Shell,
    );
    assert!(!make_outcome.output.contains(HEADER));

    let verbose_make = concat!(
        "/usr/bin/cmake -S/work/source -B/work/build-make --check-build-system CMakeFiles/Makefile.cmake 0\n",
        "/usr/bin/gmake -f CMakeFiles/Makefile2 all\n",
        "[  2%] Building C object CMakeFiles/app.dir/src/a.c.o\n",
        "/usr/bin/cc -O3 -c /work/source/src/a.c -o CMakeFiles/app.dir/src/a.c.o\n",
        "[  4%] Building C object CMakeFiles/app.dir/src/b.c.o\n",
        "/usr/bin/cc -O3 -c /work/source/src/b.c -o CMakeFiles/app.dir/src/b.c.o\n",
    );
    assert!(!detect(&sample(verbose_make)));
    assert!(!BuildLogCompressor::detect(verbose_make));
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
        assert_eq!(oracle::decode(&outcome.output, HEADER), input);
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
    assert!(outcome.output.contains(HEADER));
    assert!(
        outcome
            .output
            .contains("[1/27] Running CMake to regenerate build system...\n")
    );
    assert_eq!(oracle::decode(&outcome.output, HEADER), ninja);
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
    assert!(make_outcome.output.contains(HEADER));
    assert_eq!(oracle::decode(&make_outcome.output, HEADER), make_pct);
    assert_eq!(
        compress_lossless(&make_outcome.output).output,
        make_outcome.output
    );
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
                oracle::decode(&outcome.output, HEADER),
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
    assert_eq!(oracle::decode(&out.output, HEADER), isolated_after_diag);
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
    assert!(long_outcome.output.matches(HEADER).count() >= 2);
    assert_eq!(oracle::decode(&long_outcome.output, HEADER), long_line_log);

    let huge_line = format!("diagnostic payload: {}\n", "x".repeat(MAX_BYTES + 1));
    let huge_input = format!("{before}{huge_line}{after}");
    let huge_lines: Vec<&str> = huge_input.split_inclusive('\n').collect();
    assert!(plan(&huge_lines).is_empty());

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
        oracle::decode(&compress_lossless(&large_input).output, HEADER),
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
        .find(|line| line.starts_with(HEADER))
        .unwrap();
    assert!(header_line.len() > MAX_LINE_BYTES);
    let gapped = format!("{}{}", "$ ninja\n".repeat(120), compacted.output);
    let gapped_again = compress_lossless(&gapped);
    assert_eq!(gapped_again.output, gapped);

    let quoted_header =
        format!("```text\n{HEADER}{{\"rows\":1,\"literals\":[[\"example\"]]}}\n0\n```\n");
    let mixed = format!("{before}{quoted_header}{after}");
    let mixed_outcome = compress_lossless(&mixed);
    assert!(mixed_outcome.output.contains(&quoted_header));
    assert!(mixed_outcome.output.matches(HEADER).count() >= 3);
    assert_eq!(oracle::decode(&mixed_outcome.output, HEADER), mixed);

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
    let custom_comment_rows = (1..=16)
        .map(|step| {
            let pct = (step * 95) / 16 + 5;
            format!("[{pct:3}%] Building CXX object CMakeFiles/lib.dir/item_{step:02}.cpp.o\n")
        })
        .collect::<String>();
    let custom_comment = format!("[  5%] Preparing build metadata\n{custom_comment_rows}");
    assert!(BuildLogCompressor::detect(&custom_comment));
    let custom_out = compress_lossless(&custom_comment);
    assert!(
        custom_out
            .output
            .starts_with("[  5%] Preparing build metadata\n")
    );
    assert!(custom_out.output.contains(HEADER));
    assert_eq!(oracle::decode(&custom_out.output, HEADER), custom_comment);

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
    assert!(reconfig_out.output.contains(HEADER));
    assert_eq!(oracle::decode(&reconfig_out.output, HEADER), reconfig);

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
    assert!(!generic_fallback.output.contains(HEADER));

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

    let make_rows = (1..=14)
        .map(|i| {
            format!(
                "c++ -DAPP_EXPORTS -I/work/include -O2 -fPIC -o CMakeFiles/app.dir/mod_{i:02}.cpp.o -c /work/src/mod_{i:02}.cpp\n"
            )
        })
        .collect::<String>();
    let short_cmake_then_make = format!(
        "make[1]: Entering directory '/work/build'\n\
         [  8%] Building CXX object CMakeFiles/app.dir/mod_01.cpp.o\n\
         [ 16%] Building CXX object CMakeFiles/app.dir/mod_02.cpp.o\n\
         [ 24%] Building CXX object CMakeFiles/app.dir/mod_03.cpp.o\n\
         {make_rows}\
         make[1]: Leaving directory '/work/build'\n"
    );
    let make_store = InMemoryStore::new();
    let make_out = BuildLogCompressor.compress_with_recovery(
        &short_cmake_then_make,
        Some(&make_store),
        &RecoveryMethod::Shell,
    );
    assert_eq!(make_out.recoverability, Recoverability::Retrievable);
    assert_eq!(make_out.stash_writes.len(), 1);
    assert!(
        make_out
            .output
            .contains("[  8%] Building CXX object CMakeFiles/app.dir/mod_01.cpp.o\n")
    );
    assert!(make_out.output.contains("lines omitted."));

    // Compressing Ninja progress must also preserve scored Make omission later in the same log.
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
    assert!(composed.output.contains(HEADER));
    assert!(composed.output.contains("lines omitted."));
    assert!(
        tokenless_protocol::estimate_tokens(&composed.output)
            < tokenless_protocol::estimate_tokens(&lossless_only.output)
    );
    let marker = composed
        .output
        .split_inclusive('\n')
        .find(|line| line.contains("lines omitted."))
        .unwrap();
    let stashed = tokenless_ccr::StashStore::retrieve(&mixed_store, &composed.stash_writes[0].key)
        .unwrap()
        .unwrap();
    let stash_restored = composed.output.replacen(marker, &stashed, 1);
    assert_eq!(oracle::decode(&stash_restored, HEADER), ninja_then_make);
    let idempotent = BuildLogCompressor.compress_with_recovery(
        &composed.output,
        Some(&mixed_store),
        &RecoveryMethod::Shell,
    );
    assert_eq!(idempotent.output, composed.output);
    assert_eq!(tokenless_ccr::StashStore::len(&mixed_store), 1);

    let fenced_cc = format!(
        "{ninja_head}\
         ```sh\n\
         {}\
         ```\n",
        (1..=16)
            .map(|i| format!("cc -O2 -c src/example_{i:02}.c -o build/example_{i:02}.o\n"))
            .collect::<String>()
    );
    let fence_store = InMemoryStore::new();
    let fence_out = BuildLogCompressor.compress_with_recovery(
        &fenced_cc,
        Some(&fence_store),
        &RecoveryMethod::Shell,
    );
    assert_eq!(fence_out.recoverability, Recoverability::Lossless);
    assert!(fence_out.stash_writes.is_empty());
    assert!(fence_out.output.contains("cc -O2 -c src/example_08.c"));
    assert_eq!(oracle::decode(&fence_out.output, HEADER), fenced_cc);

    let counter_failed = format!(
        "{}\
         [5/9] FAILED: CMakeFiles/app.dir/src/unit_05.cpp.o\n\
         /usr/bin/c++ -DAPP -c /work/src/unit_05.cpp -o CMakeFiles/app.dir/src/unit_05.cpp.o\n\
         /work/src/unit_05.cpp:12:5: error: use of undeclared identifier 'x'\n\
         ninja: build stopped: subcommand failed.\n\
         {}",
        (1..=4)
            .map(|i| format!(
                "[{i}/9] Building CXX object CMakeFiles/app.dir/src/unit_{i:02}.cpp.o\n"
            ))
            .collect::<String>(),
        (6..=9)
            .map(|i| format!(
                "[{i}/9] Building CXX object CMakeFiles/app.dir/src/unit_{i:02}.cpp.o\n"
            ))
            .collect::<String>(),
    );
    assert!(!is_counter_step(
        "[5/9] FAILED: CMakeFiles/app.dir/src/unit_05.cpp.o"
    ));
    let counter_fail_store = InMemoryStore::new();
    let counter_fail_out = BuildLogCompressor.compress_with_recovery(
        &counter_failed,
        Some(&counter_fail_store),
        &RecoveryMethod::Shell,
    );
    assert_eq!(counter_fail_out.recoverability, Recoverability::Lossless);
    assert!(counter_fail_out.stash_writes.is_empty());
    assert_eq!(counter_fail_out.output.matches(HEADER).count(), 2);
    for kept in [
        "[5/9] FAILED: CMakeFiles/app.dir/src/unit_05.cpp.o\n",
        "/usr/bin/c++ -DAPP -c /work/src/unit_05.cpp -o CMakeFiles/app.dir/src/unit_05.cpp.o\n",
        "/work/src/unit_05.cpp:12:5: error: use of undeclared identifier 'x'\n",
        "ninja: build stopped: subcommand failed.\n",
    ] {
        assert!(counter_fail_out.output.contains(kept));
    }
    assert_eq!(
        oracle::decode(&counter_fail_out.output, HEADER),
        counter_failed
    );
    assert_eq!(
        compress_lossless(&counter_fail_out.output).output,
        counter_fail_out.output
    );

    // Anchored mixed runs with CMake/Ninja layout edits must retain Generic omission on non-CMake tails.
    let wheel_tail = (1..=60)
        .map(|i| format!("Processing wheel {i} of 60\n"))
        .collect::<String>();
    let anchored_mixed = format!(
        "$ cmake --build /work/build && python package_wheels.py\n\
         {ninja_head}\
         {wheel_tail}\
         exit code: 0\n"
    );
    let anchored_lossless = compress_lossless(&anchored_mixed);
    let anchored_store = InMemoryStore::new();
    let anchored_composed = BuildLogCompressor.compress_with_recovery(
        &anchored_mixed,
        Some(&anchored_store),
        &RecoveryMethod::Shell,
    );
    assert_eq!(
        anchored_composed.recoverability,
        Recoverability::Retrievable
    );
    assert_eq!(anchored_composed.stash_writes.len(), 1);
    assert!(anchored_composed.output.contains(HEADER));
    assert!(anchored_composed.output.contains("lines omitted."));
    assert!(
        tokenless_protocol::estimate_tokens(&anchored_composed.output)
            < tokenless_protocol::estimate_tokens(&anchored_lossless.output)
    );
    let wheel_marker = anchored_composed
        .output
        .split_inclusive('\n')
        .find(|line| line.contains("lines omitted."))
        .unwrap();
    let wheel_payload = tokenless_ccr::StashStore::retrieve(
        &anchored_store,
        &anchored_composed.stash_writes[0].key,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        oracle::decode(
            &anchored_composed
                .output
                .replacen(wheel_marker, &wheel_payload, 1),
            HEADER
        ),
        anchored_mixed
    );
    assert_eq!(
        BuildLogCompressor
            .compress_with_recovery(
                &anchored_composed.output,
                Some(&anchored_store),
                &RecoveryMethod::Shell,
            )
            .output,
        anchored_composed.output
    );

    // Custom [k/N] counter steps must stay verbatim on CmakeBuild rather than spilling into Generic Stash.
    let custom_counter_steps = format!(
        "$ cmake --build /work/build --parallel 1\n\
         {}\
         [21/22] Building C object CMakeFiles/review_lib.dir/lib.c.o\n\
         [22/22] Linking C static library libreview_lib.a\n\
         exit code: 0\n",
        (1..=20)
            .map(|i| format!(
                "[{i}/22] Building generated review artifact {i:02} for the project metadata index\n"
            ))
            .collect::<String>()
    );
    let custom_steps_store = InMemoryStore::new();
    let custom_steps_out = BuildLogCompressor.compress_with_recovery(
        &custom_counter_steps,
        Some(&custom_steps_store),
        &RecoveryMethod::Shell,
    );
    assert_eq!(custom_steps_out.recoverability, Recoverability::Lossless);
    assert!(custom_steps_out.stash_writes.is_empty());
    assert_eq!(custom_steps_out.output, custom_counter_steps);

    // Non-SGR control lines inside CmakeBuild fallback classification stay verbatim outside Stash.
    let ctrl_line =
        "cc -O2 -Wall -Iinclude -c src/legacy_module_12.c\x1b[0K -o build/legacy_module_12.o\n";
    let make_with_ctrl = (1..=24)
        .map(|i| {
            if i == 12 {
                ctrl_line.to_owned()
            } else {
                format!(
                    "cc -O2 -Wall -Iinclude -c src/legacy_module_{i:02}.c -o build/legacy_module_{i:02}.o\n"
                )
            }
        })
        .collect::<String>();
    let ninja_then_ctrl_make = format!(
        "{ninja_head}\
         make: Entering directory '/work/make'\n\
         {make_with_ctrl}\
         make: Leaving directory '/work/make'\n"
    );
    let ctrl_store = InMemoryStore::new();
    let ctrl_out = BuildLogCompressor.compress_with_recovery(
        &ninja_then_ctrl_make,
        Some(&ctrl_store),
        &RecoveryMethod::Shell,
    );
    assert_eq!(ctrl_out.recoverability, Recoverability::Retrievable);
    // Keeping row 12 verbatim splits the 24-row Make run into 11- and 12-row Stash blocks.
    assert_eq!(ctrl_out.stash_writes.len(), 2);
    assert!(ctrl_out.output.contains(ctrl_line));
    let ctrl_markers = ctrl_out
        .output
        .split_inclusive('\n')
        .filter(|line| line.contains("lines omitted."))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    assert_eq!(ctrl_markers.len(), 2);
    let mut ctrl_restored = ctrl_out.output.clone();
    for (marker, write) in ctrl_markers.iter().zip(&ctrl_out.stash_writes) {
        let payload = tokenless_ccr::StashStore::retrieve(&ctrl_store, &write.key)
            .unwrap()
            .unwrap();
        assert!(!payload.contains("\x1b[0K"));
        ctrl_restored = ctrl_restored.replacen(marker, &payload, 1);
    }
    assert_eq!(oracle::decode(&ctrl_restored, HEADER), ninja_then_ctrl_make);

    // Standard progress rows outside layouts (e.g. interleaved 1:1 with script progress)
    // stay eligible for Generic omission rather than splitting adjacent runs into no_savings.
    let interleaved_30 = format!(
        "$ cmake --build /work/build && python postprocess.py\n\
         {}\
         exit code: 0\n",
        (1..=30)
            .map(|i| format!(
                "[{i}/30] Building CXX object CMakeFiles/app.dir/src/unit_{i:02}.cpp.o\n\
                 Processing item {i} of 30\n"
            ))
            .collect::<String>()
    );
    let interleaved_store = InMemoryStore::new();
    let interleaved_out = BuildLogCompressor.compress_with_recovery(
        &interleaved_30,
        Some(&interleaved_store),
        &RecoveryMethod::Shell,
    );
    assert_eq!(interleaved_out.recoverability, Recoverability::Retrievable);
    assert_eq!(interleaved_out.stash_writes.len(), 1);
    assert!(interleaved_out.output.contains("lines omitted."));
    assert!(tokenless_protocol::estimate_tokens(&interleaved_out.output) < 120);
    let interleaved_marker = interleaved_out
        .output
        .split_inclusive('\n')
        .find(|line| line.contains("lines omitted."))
        .unwrap();
    let interleaved_payload = tokenless_ccr::StashStore::retrieve(
        &interleaved_store,
        &interleaved_out.stash_writes[0].key,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        interleaved_out
            .output
            .replacen(interleaved_marker, &interleaved_payload, 1),
        interleaved_30
    );
    assert_eq!(
        BuildLogCompressor
            .compress_with_recovery(
                &interleaved_out.output,
                Some(&interleaved_store),
                &RecoveryMethod::Shell,
            )
            .output,
        interleaved_out.output
    );

    let intermittent_counter_rows = format!(
        "$ cmake --build /work/build && python generate.py\n\
         [1/14] Building CXX object CMakeFiles/app.dir/src/a.cpp.o\n\
         [2/14] Building CXX object CMakeFiles/app.dir/src/b.cpp.o\n\
         {}\
         exit code: 0\n",
        (1..=60)
            .map(|i| {
                let mut chunk = format!("Processing item {i} of 60\n");
                if i % 5 == 0 {
                    let step = 2 + (i / 5);
                    chunk.push_str(&format!(
                        "[{step}/14] Building CXX object CMakeFiles/app.dir/src/step_{step:02}.cpp.o\n"
                    ));
                }
                chunk
            })
            .collect::<String>()
    );
    let intermittent_store = InMemoryStore::new();
    let intermittent_out = BuildLogCompressor.compress_with_recovery(
        &intermittent_counter_rows,
        Some(&intermittent_store),
        &RecoveryMethod::Shell,
    );
    assert_eq!(intermittent_out.recoverability, Recoverability::Retrievable);
    assert_eq!(intermittent_out.stash_writes.len(), 1);
    assert!(tokenless_protocol::estimate_tokens(&intermittent_out.output) < 120);
    let intermittent_marker = intermittent_out
        .output
        .split_inclusive('\n')
        .find(|line| line.contains("lines omitted."))
        .unwrap();
    let intermittent_payload = tokenless_ccr::StashStore::retrieve(
        &intermittent_store,
        &intermittent_out.stash_writes[0].key,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        intermittent_out
            .output
            .replacen(intermittent_marker, &intermittent_payload, 1),
        intermittent_counter_rows
    );

    let verbose_anchored = format!(
        "$ cmake --build /work/build -- VERBOSE=1\n\
         make[1]: Entering directory '/work/build'\n\
         {}\
         make[1]: Leaving directory '/work/build'\n\
         exit code: 0\n",
        (1..=12)
            .map(|i| {
                let pct = (i * 100) / 12;
                format!(
                    "[{pct:3}%] Building CXX object CMakeFiles/app.dir/unit_{i:02}.cpp.o\n\
                     /usr/bin/c++ -O2 -o CMakeFiles/app.dir/unit_{i:02}.cpp.o -c /work/src/unit_{i:02}.cpp\n"
                )
            })
            .collect::<String>()
    );
    let verbose_store = InMemoryStore::new();
    let verbose_out = BuildLogCompressor.compress_with_recovery(
        &verbose_anchored,
        Some(&verbose_store),
        &RecoveryMethod::Shell,
    );
    assert_eq!(verbose_out.recoverability, Recoverability::Lossless);
    assert!(verbose_out.stash_writes.is_empty());
    assert_eq!(verbose_out.output, verbose_anchored);

    // Unicode-leading custom counter steps work both as log heads and under Generic-fallback shielding.
    let c_compile_tail = (21..=32)
        .map(|i| format!("[{i}/32] Building C object CMakeFiles/lib.dir/src/unit_{i:02}.c.o\n"))
        .collect::<String>();
    let unicode_mid_custom = format!(
        "$ cmake --build /work/build\n\
         [1/32] Building generated review artifact 01 for the project metadata index\n\
         {}\
         {c_compile_tail}\
         exit code: 0\n",
        (2..=20)
            .map(|i| format!(
                "[{i}/32] \u{1f527} Building generated review artifact {i:02} for the project metadata index\n"
            ))
            .collect::<String>()
    );
    let unicode_mid_store = InMemoryStore::new();
    let unicode_mid_out = BuildLogCompressor.compress_with_recovery(
        &unicode_mid_custom,
        Some(&unicode_mid_store),
        &RecoveryMethod::Shell,
    );
    assert_eq!(unicode_mid_out.recoverability, Recoverability::Lossless);
    assert!(unicode_mid_out.stash_writes.is_empty());
    assert!(unicode_mid_out.output.contains(HEADER));
    assert!(
        unicode_mid_out
            .output
            .contains("[20/32] \u{1f527} Building generated review artifact 20")
    );
    assert_eq!(
        oracle::decode(&unicode_mid_out.output, HEADER),
        unicode_mid_custom
    );
    assert_eq!(
        BuildLogCompressor
            .compress_with_recovery(
                &unicode_mid_out.output,
                Some(&unicode_mid_store),
                &RecoveryMethod::Shell,
            )
            .output,
        unicode_mid_out.output
    );

    let unicode_head_custom = format!(
        "{}{c_compile_tail}",
        (1..=20)
            .map(|i| format!(
                "[{i}/32] \u{1f527} Building generated review artifact {i:02} for the project metadata index\n"
            ))
            .collect::<String>()
    );
    assert!(BuildLogCompressor::detect(&unicode_head_custom));
    let unicode_head_store = InMemoryStore::new();
    let unicode_head_out = BuildLogCompressor.compress_with_recovery(
        &unicode_head_custom,
        Some(&unicode_head_store),
        &RecoveryMethod::Shell,
    );
    assert_eq!(unicode_head_out.recoverability, Recoverability::Lossless);
    assert!(unicode_head_out.stash_writes.is_empty());
    assert!(unicode_head_out.output.contains(HEADER));
    assert!(
        tokenless_protocol::estimate_tokens(&unicode_head_out.output)
            < tokenless_protocol::estimate_tokens(&unicode_head_custom)
    );
    assert_eq!(
        oracle::decode(&unicode_head_out.output, HEADER),
        unicode_head_custom
    );
    assert_eq!(
        BuildLogCompressor
            .compress_with_recovery(
                &unicode_head_out.output,
                Some(&unicode_head_store),
                &RecoveryMethod::Shell,
            )
            .output,
        unicode_head_out.output
    );
}
