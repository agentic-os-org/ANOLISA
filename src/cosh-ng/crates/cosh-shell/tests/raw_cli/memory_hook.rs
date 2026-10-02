use super::*;

#[test]
fn raw_cli_provider_foreground_memory_hook_is_internal() {
    let fixture = temp_shell_home("provider-memory-hook-internal");
    let bin_dir = fixture.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    write_executable(
        &bin_dir.join("free"),
        "#!/bin/sh\ncat <<'EOF'\n              total        used        free      shared  buff/cache   available\nMem:          32768       30200         380          16        2188        1400\nSwap:          8192        4096        4096\nEOF\n",
    );
    let path = format!(
        "{}:{}",
        bin_dir.to_string_lossy(),
        std::env::var("PATH").unwrap_or_default()
    );

    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[("PATH", path.as_str())],
        vec![
            (b"?? provider memory hook shell\n".to_vec(), Duration::ZERO),
            (b"\n".to_vec(), Duration::from_millis(2_500)),
            (b"exit\n".to_vec(), Duration::from_millis(4_500)),
        ],
    );
    let _ = fs::remove_dir_all(&fixture);

    assert!(
        output.contains("Approved req-1") || output.contains("Auto-approved req-1"),
        "{output}"
    );
    assert!(output.contains("Bash tool sent to shell"), "{output}");
    assert!(output.contains("Mem:"), "{output}");
    assert!(!output.contains("Available memory is low"), "{output}");
    assert!(!output.contains("[Analyze] [Ignore]"), "{output}");
    assert!(!output.contains("Hook finding"), "{output}");
    assert!(
        !output.contains("PROVIDER MEMORY NATIVE OUTPUT SHOULD NOT RENDER AFTER ALLOW"),
        "{output}"
    );
}

#[test]
fn raw_cli_agent_fallback_memory_hook_is_internal() {
    let fixture = temp_shell_home("agent-fallback-memory-hook-internal");
    let bin_dir = fixture.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let marker = Path::new("/tmp/cosh-shell-fake-memory-hook-marker");
    let _ = fs::remove_file(marker);
    write_executable(
        &bin_dir.join("free"),
        "#!/bin/sh\ncat <<'EOF'\n              total        used        free      shared  buff/cache   available\nMem:          32768       30200         380          16        2188        1400\nSwap:          8192        4096        4096\nEOF\n",
    );
    let path = format!(
        "{}:{}",
        bin_dir.to_string_lossy(),
        std::env::var("PATH").unwrap_or_default()
    );

    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[("PATH", path.as_str())],
        vec![
            (b"?? agent memory hook fallback\n".to_vec(), Duration::ZERO),
            (b"\n".to_vec(), Duration::from_millis(2_500)),
            (b"exit\n".to_vec(), Duration::from_millis(4_500)),
        ],
    );
    let _ = fs::remove_dir_all(&fixture);
    let _ = fs::remove_file(marker);

    assert!(output.contains("Approved req-1"), "{output}");
    assert!(output.contains("Bash tool sent to shell"), "{output}");
    assert!(output.contains("Mem:"), "{output}");
    assert!(!output.contains("Available memory is low"), "{output}");
    assert!(!output.contains("[Analyze] [Ignore]"), "{output}");
    assert!(!output.contains("Hook finding"), "{output}");
}
