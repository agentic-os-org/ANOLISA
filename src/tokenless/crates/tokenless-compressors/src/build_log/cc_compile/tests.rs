use super::super::{BuildLogCompressor, oracle, sample_lines};
use super::*;
use crate::Recoverability;
use tokenless_ccr::{InMemoryStore, RecoveryMethod, StashStore};

fn compress_lossless(input: &str) -> crate::build_log::BuildLogOutcome {
    BuildLogCompressor.compress_with_recovery(input, None, &RecoveryMethod::None)
}

fn sample(text: &str) -> Vec<String> {
    sample_lines(text)
}

#[test]
fn positive_and_negative_detection_respect_boundaries() {
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

    // Handwritten short Make logs, non-verbose CMake/Ninja, prose, fences, and CRLF stay excluded.
    for negative in [
        "[1/1] /usr/bin/cc -O3 -c /work/src/a.c -o CMakeFiles/app.dir/a.c.o\n",
        "let cmd = \"/usr/bin/cc -O3 -MD -c /work/src/a.c -o a.o\";\n",
        "/usr/bin/cc -O3 -c /work/src/a.c -o a.o && rm -rf /tmp/x\n/usr/bin/cc -O3 -c /work/src/b.c -o b.o && rm -rf /tmp/y\n",
        "[1/3] Building C object CMakeFiles/app.dir/a.c.o\n[2/3] Building C object CMakeFiles/app.dir/b.c.o\n",
        "[ 50%] Building C object CMakeFiles/app.dir/a.c.o\n[100%] Built target app\n",
        "make: Entering directory '/work'\ncc -O2 -Wall -c src/a.c -o build/a.o\ncc -O2 -Wall -c src/b.c -o build/b.o\n",
        &format!("```text\n{ninja_verbose}```\n"),
        &format!("~~~text\n{ninja_verbose}~~~\n"),
        &ninja_verbose.replace('\n', "\r\n"),
    ] {
        assert!(!detect(&sample(negative)), "false positive on: {negative}");
    }

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
fn flag_orders_spaces_quotes_and_versions_round_trip_byte_exact() {
    let input = concat!(
        "[1/4] /usr/bin/clang++-18 -DFOO=\"bar\"  -isystem /work/inc/. -I /work/other.c/dir -O3 -MD -MT CMakeFiles/app.dir/alpha.cpp.o -MF CMakeFiles/app.dir/alpha.cpp.o.d -o CMakeFiles/app.dir/alpha.cpp.o -c /work/src/alpha.cpp\n",
        "[2/4] /usr/bin/clang++-18 -DFOO=\"bar\"  -isystem /work/inc/. -I /work/other.c/dir -O3 -MD -MT CMakeFiles/app.dir/beta.cpp.o -MF CMakeFiles/app.dir/beta.cpp.o.d -o CMakeFiles/app.dir/beta.cpp.o -c /work/src/beta.cpp\n",
        "[3/4] /usr/bin/clang++-18 -DFOO=\"bar\"  -isystem /work/inc/. -I /work/other.c/dir -O3 -MD -MT CMakeFiles/app.dir/gamma.cpp.o -MF CMakeFiles/app.dir/gamma.cpp.o.d -o CMakeFiles/app.dir/gamma.cpp.o -c /work/src/gamma.cpp\n",
        "[4/4] /usr/bin/clang++-18 -DFOO=\"bar\"  -isystem /work/inc/. -I /work/other.c/dir -O3 -MD -MT CMakeFiles/app.dir/delta.cpp.o -MF CMakeFiles/app.dir/delta.cpp.o.d -o CMakeFiles/app.dir/delta.cpp.o -c /work/src/delta.cpp\n",
    );
    assert!(detect(&sample(input)));
    let outcome = compress_lossless(input);
    assert!(outcome.output.contains(HEADER));
    assert!(outcome.stash_writes.is_empty());
    assert_eq!(outcome.recoverability, Recoverability::Lossless);
    assert_eq!(oracle::decode(&outcome.output, HEADER), input);
    assert_eq!(compress_lossless(&outcome.output).output, outcome.output);
    // `-I /work/other.c/dir` must stay literal, not treated as a source path column.
    assert!(outcome.output.contains("-I /work/other.c/dir"));

    let mq_input = concat!(
        "[1/3] /usr/bin/cc -O3 -MD -MQ CMakeFiles/app.dir/alpha.c.o -MF CMakeFiles/app.dir/alpha.c.o.d -o CMakeFiles/app.dir/alpha.c.o -c /work/src/alpha.c\n",
        "[2/3] /usr/bin/cc -O3 -MD -MQ CMakeFiles/app.dir/beta.c.o -MF CMakeFiles/app.dir/beta.c.o.d -o CMakeFiles/app.dir/beta.c.o -c /work/src/beta.c\n",
        "[3/3] /usr/bin/cc -O3 -MD -MQ CMakeFiles/app.dir/gamma.c.o -MF CMakeFiles/app.dir/gamma.c.o.d -o CMakeFiles/app.dir/gamma.c.o -c /work/src/gamma.c\n",
    );
    let mq_out = compress_lossless(mq_input);
    assert!(mq_out.output.contains(HEADER));
    assert_eq!(
        mq_out
            .output
            .lines()
            .find(|line| line.starts_with(HEADER))
            .map(|line| {
                let json: serde_json::Value =
                    serde_json::from_str(line.trim_start_matches(HEADER)).unwrap();
                json["literals"].as_array().unwrap().len()
            }),
        Some(1)
    );
    assert_eq!(oracle::decode(&mq_out.output, HEADER), mq_input);
}

#[test]
fn diagnostics_fences_controls_and_oversized_lines_preserve_safety_boundaries() {
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
                oracle::decode(&outcome.output, HEADER),
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
    assert_eq!(out.output.matches(HEADER).count(), 2);
    assert_eq!(oracle::decode(&out.output, HEADER), with_huge_link);

    let too_huge = format!("diagnostic {}\n", "x".repeat(MAX_BYTES + 1));
    let too_huge_input = format!("{}{too_huge}", rows.concat());
    let lines: Vec<&str> = too_huge_input.split_inclusive('\n').collect();
    assert!(plan(&lines, &[]).is_empty());
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
    assert!(outcome.output.contains(HEADER));
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
        oracle::decode(&outcome.output.replacen(marker, &recovered, 1), HEADER),
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
    assert!(gen_out.output.contains(HEADER));
    assert!(gen_out.stash_writes.is_empty());
    assert_eq!(gen_out.recoverability, Recoverability::Lossless);
    assert_eq!(oracle::decode(&gen_out.output, HEADER), with_leading_gen);

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
    assert!(chained_out.output.contains(HEADER));
    assert!(chained_out.output.contains(&chained_recipes));
    assert_eq!(chained_out.recoverability, Recoverability::Lossless);
    assert!(chained_out.stash_writes.is_empty());
    assert_eq!(store.len(), 0);
    assert_eq!(
        oracle::decode(&chained_out.output, HEADER),
        ninja_then_chained
    );

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
    assert!(mixed_out.output.contains(HEADER));
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
        oracle::decode(&mixed_out.output.replacen(marker, &recovered, 1), HEADER),
        mixed_chained_and_unrelated
    );
}

#[test]
fn mixed_ninja_progress_and_unprofitable_compiles_keep_cmake_build_compression() {
    const CMAKE_HEADER: &str = "CMake/Ninja templates (id<TAB>fields; interleave with literals): ";
    let progress = (1..=24)
        .map(|i| format!("[{i}/26] Building C object CMakeFiles/app.dir/mod_{i:02}.c.o\n"))
        .collect::<String>();
    let two_unprofitable = concat!(
        "[25/26] /usr/bin/cc -O1 -DVARIANT=1 -MD -MT CMakeFiles/app.dir/v1.c.o -MF CMakeFiles/app.dir/v1.c.o.d -o CMakeFiles/app.dir/v1.c.o -c /work/src/v1.c\n",
        "[26/26] /usr/bin/cc -O2 -DVARIANT=2 -MD -MT CMakeFiles/app.dir/v2.c.o -MF CMakeFiles/app.dir/v2.c.o.d -o CMakeFiles/app.dir/v2.c.o -c /work/src/v2.c\n",
    );
    let mixed = format!("{progress}{two_unprofitable}");
    assert!(BuildLogCompressor::detect(&mixed));
    let out = compress_lossless(&mixed);
    assert!(!out.output.contains(HEADER));
    assert!(out.output.contains(CMAKE_HEADER));
    assert!(out.output.contains(two_unprofitable));
    assert!(out.stash_writes.is_empty());
    assert_eq!(out.recoverability, Recoverability::Lossless);
    assert_eq!(oracle::decode(&out.output, CMAKE_HEADER), mixed);

    let custom_head = "[1/27] cd /work/build && /usr/bin/cmake -E copy_if_different /work/src/in.h /work/build/generated.h\n";
    let custom_mixed = format!(
        "{custom_head}{}{}",
        (2..=25)
            .map(|i| format!("[{i}/27] Building C object CMakeFiles/app.dir/mod_{i:02}.c.o\n"))
            .collect::<String>(),
        concat!(
            "[26/27] /usr/bin/cc -O1 -DVARIANT=1 -MD -MT CMakeFiles/app.dir/v1.c.o -MF CMakeFiles/app.dir/v1.c.o.d -o CMakeFiles/app.dir/v1.c.o -c /work/src/v1.c\n",
            "[27/27] /usr/bin/cc -O2 -DVARIANT=2 -MD -MT CMakeFiles/app.dir/v2.c.o -MF CMakeFiles/app.dir/v2.c.o.d -o CMakeFiles/app.dir/v2.c.o -c /work/src/v2.c\n",
        )
    );
    assert!(BuildLogCompressor::detect(&custom_mixed));
    let custom_out = compress_lossless(&custom_mixed);
    assert!(custom_out.output.starts_with(custom_head));
    assert!(!custom_out.output.contains(HEADER));
    assert!(custom_out.output.contains(CMAKE_HEADER));
    assert_eq!(
        oracle::decode(&custom_out.output, CMAKE_HEADER),
        custom_mixed
    );

    // Mixed logs with profitable compile commands and profitable Ninja progress compose both
    // Stage 1 passes on disjoint spans in either order without Stash writes.
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
        assert!(composed.output.contains(HEADER));
        assert!(composed.output.contains(CMAKE_HEADER));
        assert!(composed.stash_writes.is_empty());
        assert_eq!(composed.recoverability, Recoverability::Lossless);
        assert_eq!(
            oracle::decode(&oracle::decode(&composed.output, HEADER), CMAKE_HEADER),
            mixed_both
        );
        let again = BuildLogCompressor.compress_with_recovery(
            &composed.output,
            Some(&store),
            &RecoveryMethod::Shell,
        );
        assert_eq!(again.output, composed.output);
        assert!(again.stash_writes.is_empty());
        assert_eq!(again.recoverability, Recoverability::Lossless);
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
    assert!(!tail_out.output.contains(HEADER));
    assert!(tail_out.output.contains(tail_two_unprofitable));
    assert!(tail_out.stash_writes.is_empty());
    assert_eq!(tail_out.recoverability, Recoverability::Lossless);
    assert_eq!(
        oracle::decode(&tail_out.output, CMAKE_HEADER),
        tail_window_mixed
    );

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
    assert!(BuildLogCompressor::detect(&interleaved_verbose));
    let interleaved_out = BuildLogCompressor.compress_with_recovery(
        &interleaved_verbose,
        Some(&store),
        &RecoveryMethod::Shell,
    );
    assert!(interleaved_out.output.contains(CMAKE_HEADER));
    assert!(!interleaved_out.output.contains(HEADER));
    assert!(interleaved_out.stash_writes.is_empty());
    assert_eq!(interleaved_out.recoverability, Recoverability::Lossless);
    assert_eq!(
        oracle::decode(&interleaved_out.output, CMAKE_HEADER),
        interleaved_verbose
    );

    let blank_separated_after_claimed_compiles = format!(
        "{}{}",
        (1..=3)
            .map(|i| format!(
                "[{i}/27] /usr/bin/cc -O3 -I/work/include -MD -MT CMakeFiles/app.dir/src/f_{i:02}.c.o -MF CMakeFiles/app.dir/src/f_{i:02}.c.o.d -o CMakeFiles/app.dir/src/f_{i:02}.c.o -c /work/src/f_{i:02}.c\n"
            ))
            .collect::<String>(),
        (4..=27)
            .map(|i| format!(
                "[{i}/27] Building C object CMakeFiles/app.dir/mod_{i:02}.c.o\n\n"
            ))
            .collect::<String>()
    );
    for (recovery_store, method) in [
        (None, RecoveryMethod::None),
        (
            Some(&store as &dyn tokenless_ccr::StashStore),
            RecoveryMethod::Shell,
        ),
    ] {
        assert!(BuildLogCompressor::detect(
            &blank_separated_after_claimed_compiles
        ));
        let out = BuildLogCompressor.compress_with_recovery(
            &blank_separated_after_claimed_compiles,
            recovery_store,
            &method,
        );
        assert!(out.output.contains(HEADER));
        assert!(out.output.contains(CMAKE_HEADER));
        assert!(out.stash_writes.is_empty());
        assert_eq!(out.recoverability, Recoverability::Lossless);
        assert_eq!(
            oracle::decode(&oracle::decode(&out.output, HEADER), CMAKE_HEADER),
            blank_separated_after_claimed_compiles
        );
        let again = BuildLogCompressor.compress_with_recovery(&out.output, recovery_store, &method);
        assert_eq!(again.output, out.output);
        assert!(again.stash_writes.is_empty());
        assert_eq!(again.recoverability, Recoverability::Lossless);
    }
}

#[test]
fn unpaired_cmake_percent_progress_stays_on_cmake_build_pass() {
    const CMAKE_HEADER: &str = "CMake/Ninja templates (id<TAB>fields; interleave with literals): ";
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
    let verbose = (49..=60)
        .map(|i| {
            format!(
                "[{i}/60] /usr/bin/cc -O3 -I/work/include -MD -MT CMakeFiles/app.dir/src/f_{i:02}.c.o -MF CMakeFiles/app.dir/src/f_{i:02}.c.o.d -o CMakeFiles/app.dir/src/f_{i:02}.c.o -c /work/src/f_{i:02}.c\n"
            )
        })
        .collect::<String>();

    let store = InMemoryStore::new();
    for input in [
        format!("{progress}{verbose}"),
        format!("{progress}gmake[1]: Leaving directory '/work/build'\n{verbose}"),
    ] {
        let lines = input.split_inclusive('\n').collect::<Vec<_>>();
        let cmake_only_edits = super::super::cmake_build::plan(&lines, &[]);
        let cmake_only = super::super::render_reduced(
            &lines,
            &[],
            &[],
            &RecoveryMethod::None,
            &cmake_only_edits,
        );

        assert!(BuildLogCompressor::detect(&input));
        let out =
            BuildLogCompressor.compress_with_recovery(&input, Some(&store), &RecoveryMethod::Shell);
        assert!(out.output.contains(CMAKE_HEADER));
        assert!(out.output.contains(HEADER));
        assert!(out.output.contains("libalpha.dir/src/alpha/"));
        assert!(out.output.contains("libbeta.dir/src/beta/"));
        let cc_header_line = out
            .output
            .lines()
            .find(|line| line.starts_with(HEADER))
            .unwrap();
        let cc_json: serde_json::Value =
            serde_json::from_str(cc_header_line.trim_start_matches(HEADER)).unwrap();
        assert_eq!(cc_json["rows"], 12);
        assert!(
            tokenless_protocol::estimate_tokens(&out.output)
                < tokenless_protocol::estimate_tokens(&cmake_only)
        );
        assert!(out.stash_writes.is_empty());
        assert_eq!(out.recoverability, Recoverability::Lossless);
        assert_eq!(
            oracle::decode(&oracle::decode(&out.output, HEADER), CMAKE_HEADER),
            input
        );
        let again = BuildLogCompressor.compress_with_recovery(
            &out.output,
            Some(&store),
            &RecoveryMethod::Shell,
        );
        assert_eq!(again.output, out.output);
        assert!(again.stash_writes.is_empty());
        assert_eq!(again.recoverability, Recoverability::Lossless);
    }
}

fn assert_cc_and_cmake_compose_symmetrically(cc_part: &str, cmake_part: &str) {
    const CMAKE_HEADER: &str = "CMake/Ninja templates (id<TAB>fields; interleave with literals): ";
    let store = InMemoryStore::new();
    let make_first = format!("{cc_part}{cmake_part}");
    let ninja_first = format!("{cmake_part}{cc_part}");

    let mut token_counts = Vec::new();
    for mixed in [make_first, ninja_first] {
        assert!(BuildLogCompressor::detect(&mixed));
        let out =
            BuildLogCompressor.compress_with_recovery(&mixed, Some(&store), &RecoveryMethod::Shell);
        assert!(out.output.contains(HEADER));
        assert!(out.output.contains(CMAKE_HEADER));
        assert!(out.stash_writes.is_empty());
        assert_eq!(out.recoverability, Recoverability::Lossless);
        assert!(
            tokenless_protocol::estimate_tokens(&out.output)
                < tokenless_protocol::estimate_tokens(&mixed)
        );
        assert_eq!(
            oracle::decode(&oracle::decode(&out.output, HEADER), CMAKE_HEADER),
            mixed
        );
        let again = BuildLogCompressor.compress_with_recovery(
            &out.output,
            Some(&store),
            &RecoveryMethod::Shell,
        );
        assert_eq!(again.output, out.output);
        assert!(again.stash_writes.is_empty());
        assert_eq!(again.recoverability, Recoverability::Lossless);
        token_counts.push(tokenless_protocol::estimate_tokens(&out.output));
    }
    assert_eq!(token_counts[0], token_counts[1]);
}

#[test]
fn zstd_make_and_brotli_ninja_mixed_logs_compose_in_either_order() {
    let fixtures_root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/build_logs");
    let zstd =
        std::fs::read_to_string(fixtures_root.join("cc_compile/zstd_make_lib_v1_gcc.txt")).unwrap();
    let brotli =
        std::fs::read_to_string(fixtures_root.join("cmake_build/brotli_ninja_cold.txt")).unwrap();

    assert_cc_and_cmake_compose_symmetrically(&zstd, &brotli);
}

#[test]
fn zstd_make_and_blank_separated_ninja_j1_compose_in_either_order() {
    let fixtures_root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/build_logs");
    let zstd =
        std::fs::read_to_string(fixtures_root.join("cc_compile/zstd_make_lib_v1_gcc.txt")).unwrap();
    let ninja_blank = (1..=24)
        .map(|i| format!("[{i}/24] Building C object CMakeFiles/app.dir/src/mod_{i:02}.c.o\n\n"))
        .collect::<String>();

    assert_cc_and_cmake_compose_symmetrically(&zstd, &ninja_blank);
}
