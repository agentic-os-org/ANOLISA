use super::*;

fn make_input(command: &str, output: &str) -> HookInput {
    HookInput {
        command: command.to_string(),
        cwd: "/tmp".to_string(),
        exit_code: 0,
        duration_ms: 100,
        output_ref: None,
        output_bytes: output.len() as u64,
        output_preview: output.to_string(),
    }
}

#[test]
fn table_schema_detects_semantic_columns() {
    let tokens = split_tokens("PID COMMAND %MEM RSS ARGS");
    let schema = detect_table_schema(
        &tokens,
        &[
            ColumnSpec::first(ColumnSemantic::Pid, &["PID"]),
            ColumnSpec::first(ColumnSemantic::MemPct, &["%MEM"]),
            ColumnSpec::first(ColumnSemantic::Rss, &["RSS"]),
            ColumnSpec::last(ColumnSemantic::Command, &["COMMAND", "ARGS"]),
        ],
    );

    assert_eq!(schema.index(ColumnSemantic::Pid), Some(0));
    assert_eq!(schema.index(ColumnSemantic::MemPct), Some(2));
    assert_eq!(schema.index(ColumnSemantic::Rss), Some(3));
    assert_eq!(schema.index(ColumnSemantic::Command), Some(4));
    assert_eq!(
        schema
            .columns
            .iter()
            .find(|column| column.semantic == ColumnSemantic::Command)
            .map(|column| column.raw_name.as_str()),
        Some("ARGS")
    );
}

#[test]
fn ps_aux_high_mem_hits() {
    let output = "\
USER       PID %CPU %MEM    VSZ   RSS TTY      STAT START   TIME COMMAND
root      1234  3.1 45.2 5120000 2376420 ?     Sl   10:00   1:23 java -jar app.jar
root      2234  1.0  3.2 1024000  262144 ?     S    10:01   0:10 nginx
";
    let hook = HighMemoryProcessHook::new();
    let finding = hook
        .evaluate(&make_input("ps aux --sort=-%mem | head", output))
        .unwrap();
    assert_eq!(finding.hook_id, "high-memory-process");
    assert_eq!(finding.severity, FindingSeverity::Warning);
    assert!(finding.title.contains("1234"));
    assert!(finding.description.contains("RSS 2321 MiB"));
}

#[test]
fn ps_aux_high_rss_low_percent_misses() {
    let output = "\
USER       PID %CPU %MEM    VSZ   RSS TTY      STAT START   TIME COMMAND
root      1234  3.1  1.2 5120000 8376420 ?     Sl   10:00   1:23 java -jar app.jar
";
    let hook = HighMemoryProcessHook::new();
    assert!(hook.evaluate(&make_input("ps aux", output)).is_none());
}

#[test]
fn ps_eo_reordered_columns_hit() {
    let output = "\
  PID   RSS %MEM COMMAND
 1234 2376420 45.2 java -jar app.jar
";
    let rows = parse_ps_process_rows(output);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].pid, "1234");
    assert_eq!(rows[0].mem_pct, 45.2);
}

#[test]
fn ps_pid_mem_and_rss_without_command_header_misses() {
    let output = "\
PID %MEM RSS
1234 45.2 2376420
";

    assert!(parse_ps_process_rows(output).is_empty());
    assert!(HighMemoryProcessHook::new()
        .evaluate(&make_input("ps -eo pid,%mem,rss", output))
        .is_none());
}

#[test]
fn truncated_top_row_without_command_fails_closed() {
    let output = "\
top - 10:00:00 up 1 day,  load average: 0.10, 0.20, 0.30
Tasks: 1 total, 1 running, 0 sleeping, 0 stopped, 0 zombie
%Cpu(s): 1.0 us, 1.0 sy, 0.0 ni, 98.0 id
MiB Mem : 1000.0 total, 100.0 free, 800.0 used, 100.0 buff/cache
  PID USER      PR  NI    VIRT    RES    SHR S  %CPU  %MEM     TIME+ COMMAND
 1234 root      20   0 1000000 900000 100000 S   1.0  90.0   0:10.00
";

    assert!(parse_top_process_rows(output).is_empty());
}

#[test]
fn ps_custom_column_order_hit() {
    let output = "\
RSS PID COMMAND %MEM
2376420 1234 java 45.2
";
    let rows = parse_ps_process_rows(output);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].command, "java");

    let hook = HighMemoryProcessHook::new();
    let finding = hook
        .evaluate(&make_input(
            "ps -eo rss,pid,comm,%mem --sort=-%mem | head",
            output,
        ))
        .unwrap();
    assert_eq!(finding.severity, FindingSeverity::Warning);
    assert!(finding.title.contains("java"));
    assert!(finding.title.contains("1234"));
}

#[test]
fn ps_tail_command_preserves_full_args() {
    let output = "\
PID %MEM RSS ARGS
1234 45.2 2376420 java -jar app.jar
";
    let rows = parse_ps_process_rows(output);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].command, "java -jar app.jar");
}

#[test]
fn ps_duplicate_command_header_uses_tail_args() {
    let output = "\
PID COMMAND             %MEM        RSS COMMAND
1234 java               45.2    2376420 java -jar app.jar --config /etc/app.yaml
";
    let rows = parse_ps_process_rows(output);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].command, "java -jar app.jar --config /etc/app.yaml");

    let hook = HighMemoryProcessHook::new();
    let finding = hook
        .evaluate(&make_input(
            "ps -eo pid:1,comm:20,%mem:5,rss:10,args --sort=-%mem | head",
            output,
        ))
        .unwrap();

    assert_eq!(finding.severity, FindingSeverity::Warning);
    assert!(finding.title.contains("java"));
    assert!(finding.description.contains("RSS 2321 MiB"));
}

#[test]
fn ps_aux_forest_preserved_header_hit() {
    let output = "\
USER       PID %CPU %MEM    VSZ   RSS TTY      STAT START   TIME COMMAND
root      1234  3.1 45.2 5120000 2376420 ?     Sl   10:00   1:23  \\_ java -jar app.jar
";
    let hook = HighMemoryProcessHook::new();
    let finding = hook
        .evaluate(&make_input("ps auxf --sort=-%mem | head", output))
        .unwrap();

    assert_eq!(finding.severity, FindingSeverity::Warning);
    assert!(finding.title.contains("java"));
    assert!(finding.description.contains("RSS 2321 MiB"));
}

#[test]
fn ps_custom_header_aliases_hit() {
    let output = "\
PID PMEM RSZ COMM
1234 45.2 2376420 java
";
    let hook = HighMemoryProcessHook::new();
    let finding = hook
        .evaluate(&make_input(
            "ps -eo pid,pmem=PMEM,rss=RSZ,comm=COMM --sort=-pmem | head",
            output,
        ))
        .unwrap();

    assert_eq!(finding.severity, FindingSeverity::Warning);
    assert!(finding.title.contains("java"));
    assert!(finding.description.contains("RSS 2321 MiB"));
}

#[test]
fn ps_twenty_percent_is_candidate_info_only() {
    let output = "\
USER       PID %CPU %MEM    VSZ   RSS TTY      STAT START   TIME COMMAND
root      1234  3.1 25.2 5120000 2376420 ?     Sl   10:00   1:23 java -jar app.jar
";
    let hook = HighMemoryProcessHook::new();
    let finding = hook.evaluate(&make_input("ps aux", output)).unwrap();
    assert_eq!(finding.severity, FindingSeverity::Info);
}

#[test]
fn ps_percent_threshold_boundaries_use_percentage_units() {
    for (mem_pct, expected) in [
        ("19.9", None),
        ("20.0", Some(FindingSeverity::Info)),
        ("30.0", Some(FindingSeverity::Warning)),
        ("50.0", Some(FindingSeverity::Critical)),
    ] {
        let output = format!("PID %MEM RSS COMMAND\n1234 {mem_pct} 2376420 java\n");
        let finding = HighMemoryProcessHook::new().evaluate(&make_input("ps aux", &output));
        assert_eq!(
            finding.map(|finding| finding.severity),
            expected,
            "{mem_pct}"
        );
    }
}

#[test]
fn ps_awk_preserved_header_hit() {
    let output = "\
PID %MEM RSS COMMAND
1234 45.2 2376420 java
";
    let hook = HighMemoryProcessHook::new();
    assert!(hook
        .evaluate(&make_input(
            "ps aux | awk 'NR==1 || $4 > 30 {print $2,$4,$6,$11}'",
            output
        ))
        .is_some());
}

#[test]
fn ps_env_wrapper_preserved_header_hit() {
    let output = "\
USER       PID %CPU %MEM    VSZ   RSS TTY      STAT START   TIME COMMAND
root      1234  3.1 45.2 5120000 2376420 ?     Sl   10:00   1:23 java -jar app.jar
";
    let hook = HighMemoryProcessHook::new();
    let finding = hook
        .evaluate(&make_input("env LANG=C ps aux --sort=-%mem | head", output))
        .unwrap();

    assert_eq!(finding.hook_id, "high-memory-process");
    assert!(finding.title.contains("1234"));
}

#[test]
fn ps_grep_head_preserved_header_hit() {
    let output = "\
USER       PID %CPU %MEM    VSZ   RSS TTY      STAT START   TIME COMMAND
root      1234  3.1 45.2 5120000 2376420 ?     Sl   10:00   1:23 java -jar app.jar
";
    let hook = HighMemoryProcessHook::new();
    assert!(hook
        .evaluate(&make_input(
            "ps aux --sort=-%mem | head -20 | grep -E 'PID|java'",
            output
        ))
        .is_some());
}

#[test]
fn ps_filter_argument_named_bash_keeps_header_hit() {
    let output = "\
  PID COMMAND         %MEM   RSS
 1234 bash            45.2 2376420
";
    let hook = HighMemoryProcessHook::new();
    assert!(hook
        .evaluate(&make_input(
            "ps -C bash -o pid,comm,%mem,rss --sort=-%mem",
            output
        ))
        .is_some());
}

#[test]
fn ps_without_header_misses() {
    let output = "root 1234 3.1 45.2 5120000 2376420 ? Sl 10:00 1:23 java -jar app.jar\n";
    let hook = HighMemoryProcessHook::new();
    for command in [
        "ps aux | awk '$4 > 30 {print $2,$4,$6,$11}'",
        "ps aux --sort=-%mem | tail -n +2 | head -20",
        "ps aux --sort=-%mem | sed 1d | head -20",
        "ps aux --sort=-%mem | grep -v '^USER' | head -20",
    ] {
        assert!(
            hook.evaluate(&make_input(command, output)).is_none(),
            "{command}"
        );
    }
}

#[test]
fn ps_header_suppressed_custom_columns_miss() {
    let output = "\
 1234 java            45.2 2377216
 5678 worker          12.0  524288
";
    let hook = HighMemoryProcessHook::new();
    assert!(hook
        .evaluate(&make_input("ps -eo pid=,comm=,%mem=,rss=", output))
        .is_none());
    assert!(hook
        .evaluate(&make_input(
            "ps -eo pid,comm,%mem,rss --no-headers --sort=-%mem",
            output
        ))
        .is_none());
    assert!(hook
        .evaluate(&make_input(
            "ps h -eo pid,comm,%mem,rss --sort=-%mem",
            output
        ))
        .is_none());
    assert!(hook
        .evaluate(&make_input(
            "ps axho pid,comm,%mem,rss --sort=-%mem",
            output
        ))
        .is_none());
    assert!(hook
        .evaluate(&make_input("ps auxh --sort=-%mem", output))
        .is_none());
}

#[test]
fn free_low_available_critical() {
    let output = "\
               total        used        free      shared  buff/cache   available
Mem:           32768       30200         380          16        2188        1400
Swap:           8192        5200        2992
";
    let hook = MemoryPressureHook::new();
    let finding = hook.evaluate(&make_input("free -m", output)).unwrap();
    assert_eq!(finding.severity, FindingSeverity::Critical);
    assert!(finding.title.contains("1400 MiB / 32768 MiB"));
}

#[test]
fn free_available_ratio_threshold_boundaries_are_inclusive() {
    for (available, expected) in [
        (101, None),
        (100, Some(FindingSeverity::Warning)),
        (50, Some(FindingSeverity::Critical)),
    ] {
        let output = format!(
            "total used free shared buff/cache available\nMem: 1000 800 10 0 90 {available}\n"
        );
        let finding = MemoryPressureHook::new().evaluate(&make_input("free -m", &output));
        assert_eq!(
            finding.map(|finding| finding.severity),
            expected,
            "available={available}"
        );
    }
}

#[test]
fn free_env_wrapper_low_available_hits() {
    let output = "\
               total        used        free      shared  buff/cache   available
Mem:           32768       30200         380          16        2188        1400
Swap:           8192        5200        2992
";
    let hook = MemoryPressureHook::new();
    let finding = hook
        .evaluate(&make_input("env LANG=C free -m", output))
        .unwrap();

    assert_eq!(finding.hook_id, "memory-pressure");
    assert_eq!(finding.severity, FindingSeverity::Critical);
}

#[test]
fn free_env_wrapper_preserves_unit_options() {
    let output = "\
               total        used        free      shared  buff/cache   available
Mem:             8.0         7.5         0.1         0.0         0.4         0.4
Swap:            1.0         0.4         0.6
";
    let hook = MemoryPressureHook::new();
    let finding = hook
        .evaluate(&make_input("env -u LC_ALL LANG=C free --gibi", output))
        .unwrap();

    assert_eq!(finding.severity, FindingSeverity::Critical);
    assert!(finding.title.contains("410 MiB / 8192 MiB"));
    assert!(finding.description.contains("swap used 40.0%"));
}

#[test]
fn free_sudo_options_are_supported_as_wrapper_target() {
    let output = "\
               total        used        free      shared  buff/cache   available
Mem:           32768       30200         380          16        2188        1400
Swap:           8192        5200        2992
";
    let hook = MemoryPressureHook::new();
    let finding = hook
        .evaluate(&make_input("sudo -n -E free -m", output))
        .unwrap();

    assert_eq!(finding.hook_id, "memory-pressure");
    assert_eq!(finding.severity, FindingSeverity::Critical);
    assert!(finding.title.contains("1400 MiB / 32768 MiB"));
}

#[test]
fn free_human_units_are_normalized_to_mib() {
    let output = "\
               total        used        free      shared  buff/cache   available
Mem:           7.6Gi       7.1Gi        58Mi        4Mi       451Mi       306Mi
Swap:          1.0Gi       463Mi       561Mi
";
    let hook = MemoryPressureHook::new();
    for command in ["free -h", "free --human"] {
        let finding = hook.evaluate(&make_input(command, output)).unwrap();
        assert_eq!(finding.severity, FindingSeverity::Critical, "{command}");
        assert!(finding.title.contains("306 MiB / 7782 MiB"), "{command}");
        assert!(finding.description.contains("swap used 45.2%"), "{command}");
    }
}

#[test]
fn free_byte_units_are_normalized_to_mib() {
    let output = "\
               total        used        free      shared  buff/cache   available
Mem:      8589934592 8160010240   60817408     4194304   473956352   419430400
Swap:     1073741824  524288000  549453824
";
    let hook = MemoryPressureHook::new();
    for command in ["free -b", "free --bytes"] {
        let finding = hook.evaluate(&make_input(command, output)).unwrap();
        assert_eq!(finding.severity, FindingSeverity::Critical, "{command}");
        assert!(finding.title.contains("400 MiB / 8192 MiB"), "{command}");
        assert!(finding.description.contains("swap used 48.8%"), "{command}");
    }
}

#[test]
fn free_decimal_units_are_normalized_to_mib() {
    let decimal_output = "\
               total        used        free      shared  buff/cache   available
Mem:            9000        8500          60           4         440         400
Swap:           1000         500         500
";
    let hook = MemoryPressureHook::new();
    let decimal = hook
        .evaluate(&make_input("free --mega", decimal_output))
        .unwrap();
    assert_eq!(decimal.severity, FindingSeverity::Critical);
    assert!(decimal.title.contains("381 MiB / 8583 MiB"));

    let si_human_output = "\
               total        used        free      shared  buff/cache   available
Mem:            9.0G        8.5G         60M        4.0M        440M        400M
Swap:           1.0G        500M        500M
";
    for command in ["free -h --si", "free --human --si"] {
        let si_human = hook
            .evaluate(&make_input(command, si_human_output))
            .unwrap();
        assert_eq!(si_human.severity, FindingSeverity::Critical, "{command}");
        assert!(si_human.title.contains("381 MiB / 8583 MiB"), "{command}");
    }
}

#[test]
fn free_default_kib_units_are_normalized_to_mib() {
    let output = "\
               total        used        free      shared  buff/cache   available
Mem:         8388608     7968752       59392        4096      462848      409600
Swap:        1048576      512000      536576
";
    let hook = MemoryPressureHook::new();
    for command in ["free", "free -k", "free --kibi"] {
        let finding = hook.evaluate(&make_input(command, output)).unwrap();
        assert_eq!(finding.severity, FindingSeverity::Critical, "{command}");
        assert!(finding.title.contains("400 MiB / 8192 MiB"), "{command}");
    }
}

#[test]
fn free_mib_units_are_normalized_to_mib() {
    let output = "\
               total        used        free      shared  buff/cache   available
Mem:            8192        7780          58           4         354         400
Swap:           1024         500         524
";
    let hook = MemoryPressureHook::new();
    for command in ["free -m", "free --mebi"] {
        let finding = hook.evaluate(&make_input(command, output)).unwrap();
        assert_eq!(finding.severity, FindingSeverity::Critical, "{command}");
        assert!(finding.title.contains("400 MiB / 8192 MiB"), "{command}");
        assert!(
            !finding.description.contains("Confidence is lower"),
            "{command}"
        );
    }
}

#[test]
fn free_gib_integer_units_are_low_confidence() {
    let output = "\
               total        used        free      shared  buff/cache   available
Mem:               8           7           0           0           0           0
Swap:              1           0           1
";
    let hook = MemoryPressureHook::new();
    for command in ["free -g", "free --gibi"] {
        let finding = hook.evaluate(&make_input(command, output)).unwrap();
        assert_eq!(finding.severity, FindingSeverity::Critical, "{command}");
        assert!(finding.title.contains("0 MiB / 8192 MiB"), "{command}");
        assert!(
            finding.description.contains("Confidence is lower"),
            "{command}"
        );
    }
}

#[test]
fn free_wide_buffers_cache_columns_hit() {
    let output = "\
               total        used        free      shared     buffers       cache   available
Mem:           32768       30200         380          16          88        2100        1400
Swap:           8192        5200        2992
";
    let hook = MemoryPressureHook::new();
    for command in ["free -w -m", "free --wide --mebi"] {
        let finding = hook.evaluate(&make_input(command, output)).unwrap();
        assert_eq!(finding.severity, FindingSeverity::Critical, "{command}");
        assert!(finding.title.contains("1400 MiB / 32768 MiB"), "{command}");
        assert!(finding.description.contains("swap used 63.5%"), "{command}");
    }
}

#[test]
fn free_total_and_committed_extra_rows_are_ignored() {
    let output = "\
               total        used        free      shared  buff/cache   available
Mem:           32768       30200         380          16        2188        1400
Swap:           8192        5200        2992
Total:         40960       35400        3372
Comm:          22000       18000        4000
";
    let hook = MemoryPressureHook::new();
    for command in ["free -t -v -m", "free --total --committed --mebi"] {
        let finding = hook.evaluate(&make_input(command, output)).unwrap();
        assert_eq!(finding.severity, FindingSeverity::Critical, "{command}");
        assert!(finding.title.contains("1400 MiB / 32768 MiB"), "{command}");
        assert!(finding.description.contains("swap used 63.5%"), "{command}");
    }
}

#[test]
fn free_low_high_extra_rows_are_ignored() {
    let output = "\
               total        used        free      shared  buff/cache   available
Mem:           32768       30200         380          16        2188        1400
Low:           32768       30400         180
High:              0           0           0
Swap:           8192        5200        2992
";
    let hook = MemoryPressureHook::new();
    let finding = hook.evaluate(&make_input("free -l -m", output)).unwrap();
    assert_eq!(finding.severity, FindingSeverity::Critical);
    assert!(finding.title.contains("1400 MiB / 32768 MiB"));
    assert!(finding.description.contains("swap used 63.5%"));
    assert!(hook
        .evaluate(&make_input("free --lohi --mebi", output))
        .is_some());
}

#[test]
fn free_line_mode_without_available_misses() {
    let output =
        "SwapUse         483 CachUse         506  MemUse         833 MemFree        6698\n";
    let hook = MemoryPressureHook::new();
    assert!(hook.evaluate(&make_input("free -L -m", output)).is_none());
    assert!(hook
        .evaluate(&make_input("free --line --mebi", output))
        .is_none());
}

#[test]
fn free_multi_sample_output_misses() {
    let output = "\
               total        used        free      shared  buff/cache   available
Mem:           32768       30200         380          16        2188        1400
Swap:           8192        5200        2992

               total        used        free      shared  buff/cache   available
Mem:           32768       30300         280          16        2188        1300
Swap:           8192        5200        2992
";
    let hook = MemoryPressureHook::new();
    assert!(hook
        .evaluate(&make_input("free -s 1 -c 2 -m", output))
        .is_none());
}

#[test]
fn free_sampling_command_misses_even_with_single_sample_output() {
    let output = "\
               total        used        free      shared  buff/cache   available
Mem:           32768       30200         380          16        2188        1400
Swap:           8192        5200        2992
";
    let hook = MemoryPressureHook::new();
    for command in [
        "free -s 1 -c 1 -m",
        "free -ms 1 -c 1",
        "free -c 1 -m",
        "free -c1 -m",
        "free --count=1 -m",
        "LANG=C free --seconds=1 --count=1 -m",
        "sudo -n free --seconds 1 --count 1 -m",
    ] {
        assert!(
            hook.evaluate(&make_input(command, output)).is_none(),
            "{command}"
        );
    }
}

#[test]
fn free_labels_are_case_insensitive() {
    let output = "\
               TOTAL        used        free      shared  buff/cache   AVAILABLE
MEM:           32768       30200         380          16        2188        1400
SWAP:           8192        5200        2992
";
    let hook = MemoryPressureHook::new();
    let finding = hook.evaluate(&make_input("free -m", output)).unwrap();

    assert_eq!(finding.severity, FindingSeverity::Critical);
    assert!(finding.title.contains("1400 MiB / 32768 MiB"));
    assert!(finding.description.contains("swap used 63.5%"));
}

#[test]
fn free_healthy_misses() {
    let output = "\
               total        used        free      shared  buff/cache   available
Mem:           32768       10200         380          16        2188       20000
Swap:           8192         100        8092
";
    let hook = MemoryPressureHook::new();
    assert!(hook.evaluate(&make_input("free -m", output)).is_none());
}

#[test]
fn free_high_swap_is_silent_when_available_memory_is_healthy() {
    let output = "\
               total        used        free      shared  buff/cache   available
Mem:           32768       10200         380          16        2188       20000
Swap:           8192        5200        2992
";
    let hook = MemoryPressureHook::new();
    assert!(hook.evaluate(&make_input("free -m", output)).is_none());
}

#[test]
fn high_memory_typed_facts_are_sorted_capped_and_use_basenames() {
    let output = "\
PID %MEM RSS COMMAND
1 21.0 100 /usr/bin/one
2 55.0 200 two
3 35.0 300 three
4 25.0 400 four
";
    let facts = HighMemoryProcessHook::new()
        .builtin_facts(&make_input("ps -eo pid,%mem,rss,command", output))
        .unwrap();
    let BuiltinFindingFacts::HighMemoryProcesses(facts) = facts else {
        panic!("expected process facts");
    };

    assert_eq!(facts.confidence, MetricsConfidence::High);
    assert_eq!(facts.processes.len(), 3);
    assert_eq!(
        facts
            .processes
            .iter()
            .map(|process| (process.pid.as_str(), process.command_basename.as_str()))
            .collect::<Vec<_>>(),
        vec![("2", "two"), ("3", "three"), ("4", "four")]
    );
    assert_eq!(facts.processes[0].mem_pct, 55.0);
    assert_eq!(facts.processes[0].rss_kib, Some(200));
}

#[test]
fn pressure_typed_facts_preserve_confidence_and_ratios() {
    let output = "\
               total        used        free      shared  buff/cache   available
Mem:            1000         850          20           0          70          80
Swap:            200          50         150
";
    let facts = MemoryPressureHook::new()
        .builtin_facts(&make_input("free -m", output))
        .unwrap();
    let BuiltinFindingFacts::MemoryPressure(facts) = facts else {
        panic!("expected pressure facts");
    };

    assert_eq!(facts.confidence, MetricsConfidence::High);
    assert_eq!(facts.available_ratio, 0.08);
    assert_eq!(facts.swap_ratio, Some(0.25));
}

#[test]
fn top_parses_memory_and_process_table() {
    let output = "\
top - 04:04:49 up 20:38,  0 user,  load average: 0.31, 0.40, 0.42
MiB Mem :  32768.0 total,   1400.0 free,  30200.0 used,   2188.0 buff/cache
MiB Swap:   8192.0 total,   2992.0 free,   5200.0 used.   1400.0 avail Mem

  PID USER      PR  NI    VIRT    RES    SHR S  %CPU  %MEM     TIME+ COMMAND
 1234 root      20   0 5120000   2.3g  100m S  12.0  45.2   1:23.45 java
";
    let pressure = MemoryPressureHook::new()
        .evaluate(&make_input("top -b -n1 | head -20", output))
        .unwrap();
    let process = HighMemoryProcessHook::new()
        .evaluate(&make_input("top -b -n1 | head -20", output))
        .unwrap();
    assert_eq!(pressure.hook_id, "memory-pressure");
    assert_eq!(process.hook_id, "high-memory-process");
    assert!(process.description.contains("RSS 2355 MiB"));
}

#[test]
fn top_common_procps_options_parse() {
    let mib_output = "\
top - 04:04:49 up 20:38,  0 user,  load average: 0.31, 0.40, 0.42
MiB Mem :  32768.0 total,   1400.0 free,  30200.0 used,   2188.0 buff/cache
MiB Swap:   8192.0 total,   2992.0 free,   5200.0 used.   1400.0 avail Mem

  PID USER      PR  NI    VIRT    RES    SHR S  %CPU  %MEM     TIME+ COMMAND
 1234 root      20   0 5120000   2.3g  100m S  12.0  45.2   1:23.45 java
";
    let hook = MemoryPressureHook::new();
    for command in [
        "top -bn 1",
        "top -b -n 1 -w 512",
        "top -b -n1 -w512",
        "top -b -d 1 -n1",
        "top -b -d1 -n1",
        "top -bd 1 -n1",
        "top -b -c -n1",
        "top -bc -n1",
        "top -b -H -n1",
        "top -b -S -n1",
        "top -b -i -n1",
        "top -b -n1 -p 1234",
        "top -b -n1 -u root",
        "top -b -n1 -U root",
        "top -b -n1 -o%MEM",
    ] {
        let finding = hook.evaluate(&make_input(command, mib_output)).unwrap();
        assert_eq!(finding.severity, FindingSeverity::Critical, "{command}");
        assert!(finding.title.contains("1400 MiB / 32768 MiB"), "{command}");
    }

    let gib_output = "\
top - 04:04:49 up 20:38,  0 user,  load average: 0.31, 0.40, 0.42
GiB Mem :     32.0 total,      1.4 free,     29.5 used,      2.1 buff/cache
GiB Swap:      8.0 total,      2.9 free,      5.1 used.      1.4 avail Mem

  PID USER      PR  NI    VIRT    RES    SHR S  %CPU  %MEM     TIME+ COMMAND
 1234 root      20   0 5120000   2.3g  100m S  12.0  45.2   1:23.45 java
";
    let finding = hook
        .evaluate(&make_input("top -b -n1 -E g", gib_output))
        .unwrap();
    assert_eq!(finding.severity, FindingSeverity::Critical);
    assert!(finding.title.contains("1434 MiB / 32768 MiB"));
}

#[test]
fn top_numeric_res_is_treated_as_kib() {
    let output = "\
top - 04:04:49 up 20:38,  0 user,  load average: 0.31, 0.40, 0.42
MiB Mem :  32768.0 total,   1400.0 free,  30200.0 used,   2188.0 buff/cache
MiB Swap:   8192.0 total,   2992.0 free,   5200.0 used.   1400.0 avail Mem

  PID USER      PR  NI    VIRT    RES    SHR S  %CPU  %MEM     TIME+ COMMAND
 1234 root      20   0 5120000 524288  100m S  12.0  45.2   1:23.45 java
";
    let finding = HighMemoryProcessHook::new()
        .evaluate(&make_input("top -b -n1 | head -20", output))
        .unwrap();

    assert_eq!(finding.hook_id, "high-memory-process");
    assert!(finding.description.contains("RSS 512 MiB"));
}

#[test]
fn top_gib_units_are_normalized_to_mib() {
    let output = "\
top - 04:04:49 up 20:38,  0 user,  load average: 0.31, 0.40, 0.42
GiB Mem :      8.0 total,      0.2 free,      7.4 used,      0.4 buff/cache
GiB Swap:      1.0 total,      0.4 free,      0.6 used.      0.3 avail Mem

  PID USER      PR  NI    VIRT    RES    SHR S  %CPU  %MEM     TIME+ COMMAND
 1234 root      20   0 5120000   512m  100m S  12.0  45.2   1:23.45 java
";
    let finding = MemoryPressureHook::new()
        .evaluate(&make_input("top -b -n1", output))
        .unwrap();

    assert_eq!(finding.severity, FindingSeverity::Critical);
    assert!(finding.title.contains("307 MiB / 8192 MiB"));
    assert!(finding.description.contains("swap used 60.0%"));
}

#[test]
fn top_tib_units_are_normalized_to_mib() {
    let output = "\
top - 04:04:49 up 20:38,  0 user,  load average: 0.31, 0.40, 0.42
TiB Mem :      2.0 total,      0.1 free,      1.8 used,      0.1 buff/cache
TiB Swap:      1.0 total,      0.6 free,      0.4 used.      0.1 avail Mem
";
    let finding = MemoryPressureHook::new()
        .evaluate(&make_input("top -b -n1", output))
        .unwrap();

    assert_eq!(finding.severity, FindingSeverity::Critical);
    assert!(finding.title.contains("104858 MiB / 2097152 MiB"));
    assert!(finding.description.contains("swap used 40.0%"));
}

#[test]
fn top_unit_labels_are_case_insensitive() {
    let output = "\
top - 04:04:49 up 20:38,  0 user,  load average: 0.31, 0.40, 0.42
mib mem :   1024.0 total,     48.0 free,     900.0 used,      76.0 buff/cache
mib swap:   1024.0 total,    512.0 free,     512.0 used.      48.0 avail mem
";
    let finding = MemoryPressureHook::new()
        .evaluate(&make_input("top -b -n1", output))
        .unwrap();

    assert_eq!(finding.severity, FindingSeverity::Critical);
    assert!(finding.title.contains("48 MiB / 1024 MiB"));
    assert!(finding.description.contains("swap used 50.0%"));
}

#[test]
fn top_kib_without_avail_mem_falls_back_to_free_low_confidence() {
    let output = "\
top - 04:04:49 up 20:38,  0 user,  load average: 0.31, 0.40, 0.42
KiB Mem :   1048576 total,     49152 free,    950272 used,     49152 buff/cache
KiB Swap:   1048576 total,    524288 free,    524288 used.
";
    let metrics = parse_top_memory_metrics(output).unwrap();
    assert_eq!(metrics.confidence, MetricsConfidence::Low);
    assert_eq!(round_mib(metrics.total_mib), 1024);
    assert_eq!(round_mib(metrics.available_mib), 48);

    let finding = MemoryPressureHook::new()
        .evaluate(&make_input("top -b -n1", output))
        .unwrap();
    assert_eq!(finding.severity, FindingSeverity::Critical);
    assert!(finding.title.contains("48 MiB / 1024 MiB"));
    assert!(finding.description.contains("Confidence is lower"));
}

#[test]
fn top_per_cpu_preview_without_avail_mem_is_low_confidence() {
    let output = "\
top - 00:07:44 up 10:34,  0 user,  load average: 0.24, 0.26, 0.32
%Cpu0  :  0.0 us,  0.0 sy,  0.0 ni, 90.9 id,  0.0 wa,  0.0 hi,  9.1 si,  0.0 st
%Cpu2  :  0.0 us,  0.0 sy,  0.0 ni, 91.7 id,  0.0 wa,  0.0 hi,  8.3 si,  0.0 st
MiB Mem :  32768.0 total,   1400.0 free,  30200.0 used,   2188.0 buff/cache

  PID USER      PR  NI    VIRT    RES    SHR S  %CPU  %MEM     TIME+ COMMAND
 1234 root      20   0 5120000   2.3g  100m S  12.0  45.2   1:23.45 java
";
    let finding = MemoryPressureHook::new()
        .evaluate(&make_input("top -b -1 -n1 | head -16", output))
        .unwrap();

    assert_eq!(finding.severity, FindingSeverity::Critical);
    assert!(finding.title.contains("1400 MiB / 32768 MiB"));
    assert!(finding.description.contains("Confidence is lower"));
}

#[test]
fn top_without_batch_option_misses_even_with_parseable_output() {
    let output = "\
top - 04:04:49 up 20:38,  0 user,  load average: 0.31, 0.40, 0.42
MiB Mem :  32768.0 total,   1400.0 free,  30200.0 used,   2188.0 buff/cache
MiB Swap:   8192.0 total,   2992.0 free,   5200.0 used.   1400.0 avail Mem

  PID USER      PR  NI    VIRT    RES    SHR S  %CPU  %MEM     TIME+ COMMAND
 1234 root      20   0 5120000   2.3g  100m S  12.0  45.2   1:23.45 java
";
    assert!(MemoryPressureHook::new()
        .evaluate(&make_input("top", output))
        .is_none());
    assert!(HighMemoryProcessHook::new()
        .evaluate(&make_input("top", output))
        .is_none());
}

#[test]
fn top_batch_without_one_shot_count_is_not_diagnosed() {
    let output = "\
top - 04:04:49 up 20:38,  0 user,  load average: 0.31, 0.40, 0.42
MiB Mem :  32768.0 total,   1400.0 free,  30200.0 used,   2188.0 buff/cache
MiB Swap:   8192.0 total,   2992.0 free,   5200.0 used.   1400.0 avail Mem

  PID USER      PR  NI    VIRT    RES    SHR S  %CPU  %MEM     TIME+ COMMAND
 1234 root      20   0   12.0g   2.3g  42188 S   3.1  45.2   1:23.45 java
";
    for command in [
        "top -b",
        "top --batch",
        "top -b -n2",
        "top --batch --iterations=2",
    ] {
        assert!(
            MemoryPressureHook::new()
                .evaluate(&make_input(command, output))
                .is_none(),
            "{command}"
        );
        assert!(
            HighMemoryProcessHook::new()
                .evaluate(&make_input(command, output))
                .is_none(),
            "{command}"
        );
    }
}

#[test]
fn interactive_top_without_batch_snapshot_is_not_diagnosed() {
    let output =
        include_str!("../../../tests/fixtures/linux-memory/top_interactive_ansi_replay.txt");
    assert!(MemoryPressureHook::new()
        .evaluate(&make_input("top", output))
        .is_none());
    assert!(HighMemoryProcessHook::new()
        .evaluate(&make_input("top", output))
        .is_none());
}

#[test]
fn top_batch_option_variants_are_supported() {
    assert!(is_batch_top_command("top -b -n1 | head -20"));
    assert!(is_batch_top_command("top -b -n 1 -o %MEM | head -25"));
    assert!(is_batch_top_command("top -bn1"));
    assert!(is_batch_top_command("top -bn 1"));
    assert!(is_batch_top_command("top -b -n 1 -w 512"));
    assert!(is_batch_top_command("top -b -n1 -w512"));
    assert!(is_batch_top_command("top -b -d 1 -n1"));
    assert!(is_batch_top_command("top -b -d1 -n1"));
    assert!(is_batch_top_command("top -bd 1 -n1"));
    assert!(is_batch_top_command("top -b -c -n1"));
    assert!(is_batch_top_command("top -bc -n1"));
    assert!(is_batch_top_command("top -b -H -n1"));
    assert!(is_batch_top_command("top -b -1 -n1"));
    assert!(is_batch_top_command("top -b1 -n1"));
    assert!(is_batch_top_command("top -b -S -n1"));
    assert!(is_batch_top_command("top -b -i -n1"));
    assert!(is_batch_top_command("top -b -n1 -p 1234"));
    assert!(is_batch_top_command("top -b -n1 -u root"));
    assert!(is_batch_top_command("top -b -n1 -U root"));
    assert!(is_batch_top_command("top -b -n1 -o%MEM"));
    assert!(is_batch_top_command("top -b -n1 -E g"));
    assert!(is_batch_top_command("top -b --iterations=1"));
    assert!(is_batch_top_command("LANG=C top --batch -n1"));
    assert!(is_batch_top_command("LANG=C top --batch --iterations=1"));
    assert!(is_batch_top_command("LANG=C top --batch --iterations 1"));
    assert!(is_batch_top_command("sudo -u root top -b -n1"));
    assert!(is_batch_top_command("env LANG=C top -b -n1"));
    assert!(is_batch_top_command("/usr/bin/env -i LANG=C top -b -n1"));
    assert!(is_batch_top_command("env -u LC_ALL LANG=C top -b -n1"));
    assert!(is_batch_top_command("env -i LANG=C sudo -n top -b -n1"));
    assert!(!is_batch_top_command("top"));
    assert!(!is_batch_top_command("top -n1"));
    assert!(!is_batch_top_command("top -b"));
    assert!(!is_batch_top_command("top --batch"));
    assert!(!is_batch_top_command("top -b -n2"));
    assert!(!is_batch_top_command("top --batch --iterations=2"));
    assert!(!is_batch_top_command("env LANG=C echo top -b -n1"));
}

#[test]
fn env_wrapper_options_are_supported_fail_closed() {
    assert_eq!(
        memory_target_program("env -u LC_ALL LANG=C free -m"),
        "free"
    );
    assert_eq!(
        memory_target_program("/usr/bin/env --ignore-environment LANG=C top -b -n1"),
        "top"
    );
    assert_eq!(memory_target_program("sudo -n -E free -m"), "free");
    assert_eq!(memory_target_program("LANG=C sudo -n free -m"), "free");
    assert_eq!(
        memory_target_program("/usr/bin/sudo -u root top -b -n1"),
        "top"
    );
    assert_eq!(
        memory_target_program("env -i LANG=C sudo -n top -b -n1"),
        "top"
    );
    assert_ne!(
        memory_target_program("sudo --definitely-unknown free -m"),
        "free"
    );
    assert_eq!(
        memory_target_program("env --unset=LC_ALL LANG=C ps aux"),
        "ps"
    );
    assert_eq!(
        memory_target_program("env --chdir=/tmp LANG=C free -m"),
        "free"
    );
    assert_eq!(memory_target_program("env --split-string='free -m'"), "");
    assert_eq!(memory_target_program("env echo top -b -n1"), "echo");
}

#[test]
fn malformed_outputs_miss() {
    assert!(parse_ps_process_rows("hello\nworld\n").is_empty());
    assert!(parse_free_memory_metrics("free -m", "Mem: 1 2 3\n").is_none());
    assert!(parse_top_memory_metrics("\u{1b}[H\u{1b}[Jtop screen").is_none());
}

#[test]
fn busybox_ps_and_top_fail_closed() {
    let ps_output = "\
PID   USER     TIME  COMMAND
    1 root      0:00 sh
   11 root      0:00 ps
";
    let top_output = "\
Mem: 1625832K used, 6399316K free, 2532K shrd, 27900K buff, 789004K cached
CPU:   0% usr   0% sys   0% nic 100% idle   0% io   0% irq   0% sirq
Load average: 0.29 0.34 0.44 1/426 12
  PID  PPID USER     STAT   VSZ %VSZ CPU %CPU COMMAND
    1     0 root     S     1724   0%   4   0% sh
   11     1 root     R     1716   0%   9   0% top -b -n1
";

    assert!(parse_ps_process_rows(ps_output).is_empty());
    assert!(parse_top_process_rows(top_output).is_empty());
    assert!(parse_top_memory_metrics(top_output).is_none());
    assert!(HighMemoryProcessHook::new()
        .evaluate(&make_input("ps", ps_output))
        .is_none());
    assert!(HighMemoryProcessHook::new()
        .evaluate(&make_input("top -b -n1", top_output))
        .is_none());
    assert!(MemoryPressureHook::new()
        .evaluate(&make_input("top -b -n1", top_output))
        .is_none());
}

#[test]
fn procps_fixture_outputs_do_not_false_positive() {
    let ps_aux = include_str!("../../../tests/fixtures/linux-memory/ps_aux_procps.txt");
    let ps_eo = include_str!("../../../tests/fixtures/linux-memory/ps_eo_procps.txt");
    let free_m = include_str!("../../../tests/fixtures/linux-memory/free_m_procps.txt");
    let top = include_str!("../../../tests/fixtures/linux-memory/top_b_n1_procps.txt");
    let docker_ps_aux =
        include_str!("../../../tests/fixtures/linux-memory/docker_alpine_ps_aux_procps.txt");
    let docker_ps_eo =
        include_str!("../../../tests/fixtures/linux-memory/docker_alpine_ps_eo_procps.txt");
    let docker_free_m =
        include_str!("../../../tests/fixtures/linux-memory/docker_alpine_free_m_procps.txt");
    let docker_top =
        include_str!("../../../tests/fixtures/linux-memory/docker_alpine_top_b_n1_procps.txt");

    assert!(!parse_ps_process_rows(ps_aux).is_empty());
    assert!(!parse_ps_process_rows(ps_eo).is_empty());
    assert!(!parse_ps_process_rows(docker_ps_aux).is_empty());
    assert!(!parse_ps_process_rows(docker_ps_eo).is_empty());
    assert!(HighMemoryProcessHook::new()
        .evaluate(&make_input("ps aux --sort=-%mem | head", ps_aux))
        .is_none());
    assert!(HighMemoryProcessHook::new()
        .evaluate(&make_input(
            "ps -eo pid,ppid,comm,%mem,%cpu,rss,vsz,args --sort=-%mem | head",
            ps_eo
        ))
        .is_none());
    assert!(MemoryPressureHook::new()
        .evaluate(&make_input("free -m", free_m))
        .is_none());
    assert!(MemoryPressureHook::new()
        .evaluate(&make_input("top -b -n1 | head -20", top))
        .is_none());
    assert!(HighMemoryProcessHook::new()
        .evaluate(&make_input("top -b -n1 | head -20", top))
        .is_none());
    assert!(HighMemoryProcessHook::new()
        .evaluate(&make_input("ps aux --sort=-%mem | head", docker_ps_aux))
        .is_none());
    assert!(HighMemoryProcessHook::new()
        .evaluate(&make_input(
            "ps -eo pid,ppid,comm,%mem,%cpu,rss,vsz,args --sort=-%mem | head",
            docker_ps_eo
        ))
        .is_none());
    assert!(MemoryPressureHook::new()
        .evaluate(&make_input("free -m", docker_free_m))
        .is_none());
    assert!(MemoryPressureHook::new()
        .evaluate(&make_input("top -b -n1 | head -20", docker_top))
        .is_none());
    assert!(HighMemoryProcessHook::new()
        .evaluate(&make_input("top -b -n1 | head -20", docker_top))
        .is_none());
}
