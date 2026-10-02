use super::*;

#[test]
fn lookup_intent_downgrades_critical_process_to_hint() {
    let findings = vec![finding("high-memory-process", FindingSeverity::Critical)];
    let aggregated = aggregate_hook_findings(findings);
    let block = block_with_command("ps aux | grep java");
    let suppression_key = suppression_key(&block, &aggregated[0]);

    assert_eq!(
        apply_session_interruption_policy(
            &block,
            &aggregated[0],
            display_for_aggregate(&block, &aggregated[0], false),
            &suppression_key,
            &HookRuntimeState::default(),
            false
        ),
        RuntimeHookDisplay::Hint
    );
}

#[test]
fn env_prefixed_ps_pid_lookup_downgrades_critical_process_to_hint() {
    let findings = vec![finding("high-memory-process", FindingSeverity::Critical)];
    let aggregated = aggregate_hook_findings(findings);
    let block = block_with_command("LANG=C ps -p 1234 -o pid,%mem,rss,comm");
    let suppression_key = suppression_key(&block, &aggregated[0]);

    assert_eq!(
        apply_session_interruption_policy(
            &block,
            &aggregated[0],
            display_for_aggregate(&block, &aggregated[0], false),
            &suppression_key,
            &HookRuntimeState::default(),
            false
        ),
        RuntimeHookDisplay::Hint
    );
}

#[test]
fn command_intent_classifier_covers_hook_noise_cases() {
    assert_eq!(
        classify_command_intent("top -b -n1 | head -20"),
        CommandIntent::Diagnostic
    );
    assert_eq!(
        classify_command_intent("ps aux --sort=-%mem | head"),
        CommandIntent::Diagnostic
    );
    assert_eq!(
        classify_command_intent("ps auxf --sort=-%mem | head"),
        CommandIntent::Diagnostic
    );
    assert_eq!(
        classify_command_intent("ps auxh --sort=-%mem"),
        CommandIntent::Other
    );
    assert_eq!(
        classify_command_intent("ps -eo pid=,comm=,%mem=,rss="),
        CommandIntent::Other
    );
    assert_eq!(
        classify_command_intent("ps -eo pid,comm,%mem,rss --no-headers --sort=-%mem"),
        CommandIntent::Other
    );
    assert_eq!(
        classify_command_intent("ps h -eo pid,comm,%mem,rss --sort=-%mem"),
        CommandIntent::Other
    );
    assert_eq!(
        classify_command_intent("ps axho pid,comm,%mem,rss --sort=-%mem"),
        CommandIntent::Other
    );
    assert_eq!(
        classify_command_intent("ps axo pid,comm,%mem,rss --sort=-%mem"),
        CommandIntent::Diagnostic
    );
    assert_eq!(
        classify_command_intent("ps -eo pid:1,comm:20,%mem:5,rss:10,args --sort=-%mem"),
        CommandIntent::Diagnostic
    );
    assert_eq!(
        classify_command_intent("ps -C bash -o pid,comm,%mem,rss --sort=-%mem"),
        CommandIntent::Diagnostic
    );
    assert_eq!(
        classify_command_intent("ps -eo pid,pmem=PMEM,rss=RSZ,comm=COMM --sort=-pmem"),
        CommandIntent::Diagnostic
    );
    assert_eq!(
        classify_command_intent("ps aux --sort=-%mem | tail -n +2 | head -20"),
        CommandIntent::Pipeline
    );
    assert_eq!(
        classify_command_intent("ps aux --sort=-%mem | sed 1d | head -20"),
        CommandIntent::Pipeline
    );
    assert_ne!(
        classify_command_intent("ps aux --sort=-%mem | grep -v '^USER' | head -20"),
        CommandIntent::Diagnostic
    );
    assert_eq!(
        classify_command_intent("ps aux | grep java"),
        CommandIntent::Lookup
    );
    assert_eq!(
        classify_command_intent("LANG=C ps -p 1234 -o pid,%mem,rss,comm"),
        CommandIntent::Lookup
    );
    assert_eq!(
        classify_command_intent("sudo free -m"),
        CommandIntent::Wrapper
    );
    assert_eq!(
        classify_command_intent("LANG=C sudo free -m"),
        CommandIntent::Wrapper
    );
    assert_eq!(
        classify_command_intent("LANG=C sudo -n free -m"),
        CommandIntent::Wrapper
    );
    assert_eq!(
        classify_command_intent("ps aux | awk '{print $2}'"),
        CommandIntent::Pipeline
    );
    assert_eq!(
        classify_command_intent("free -m; echo done"),
        CommandIntent::Script
    );
    assert_eq!(
        classify_command_intent("bash /tmp/memory-report.sh"),
        CommandIntent::Script
    );
    assert_eq!(
        classify_command_intent("LANG=C sh -c 'free -m'"),
        CommandIntent::Script
    );
    assert_eq!(
        classify_command_intent("zsh ./collect-memory.zsh"),
        CommandIntent::Script
    );
    assert_eq!(classify_command_intent("bash"), CommandIntent::Other);
    assert_eq!(
        classify_command_intent("bash --version"),
        CommandIntent::Other
    );
    assert_eq!(
        classify_command_intent("free -m"),
        CommandIntent::Diagnostic
    );
    assert_eq!(
        classify_command_intent("free -k"),
        CommandIntent::Diagnostic
    );
    assert_eq!(
        classify_command_intent("free -g"),
        CommandIntent::Diagnostic
    );
    assert_eq!(
        classify_command_intent("free --bytes"),
        CommandIntent::Diagnostic
    );
    assert_eq!(
        classify_command_intent("free --kibi"),
        CommandIntent::Diagnostic
    );
    assert_eq!(
        classify_command_intent("free --mebi"),
        CommandIntent::Diagnostic
    );
    assert_eq!(
        classify_command_intent("free --gibi"),
        CommandIntent::Diagnostic
    );
    assert_eq!(
        classify_command_intent("free -t -v -m"),
        CommandIntent::Diagnostic
    );
    assert_eq!(
        classify_command_intent("free --total --committed --mebi"),
        CommandIntent::Diagnostic
    );
    assert_eq!(
        classify_command_intent("free -l -m"),
        CommandIntent::Diagnostic
    );
    assert_eq!(
        classify_command_intent("free --lohi --mebi"),
        CommandIntent::Diagnostic
    );
    assert_eq!(
        classify_command_intent("free --wide --mebi"),
        CommandIntent::Diagnostic
    );
    assert_eq!(classify_command_intent("free -L -m"), CommandIntent::Other);
    assert_eq!(
        classify_command_intent("LANG=C free --line --mebi"),
        CommandIntent::Other
    );
    assert_eq!(classify_command_intent("free --help"), CommandIntent::Other);
    assert_eq!(classify_command_intent("free -V"), CommandIntent::Other);
    assert_eq!(
        classify_command_intent("LANG=C free --version"),
        CommandIntent::Other
    );
    assert_eq!(
        classify_command_intent("free -h"),
        CommandIntent::Diagnostic
    );
    assert_eq!(
        classify_command_intent("free --human"),
        CommandIntent::Diagnostic
    );
    assert_eq!(
        classify_command_intent("free --human --si"),
        CommandIntent::Diagnostic
    );
    assert_eq!(
        classify_command_intent("free -s 1 -c 2 -m"),
        CommandIntent::Other
    );
    assert_eq!(
        classify_command_intent("LANG=C free --seconds=1 --count=2 -m"),
        CommandIntent::Other
    );
    assert_eq!(
        classify_command_intent("free -c 1 -m"),
        CommandIntent::Other
    );
    assert_eq!(classify_command_intent("free -c1 -m"), CommandIntent::Other);
    assert_eq!(
        classify_command_intent("free --count=1 -m"),
        CommandIntent::Other
    );
    assert_eq!(classify_command_intent("top"), CommandIntent::Interactive);
    assert_eq!(classify_command_intent("top --help"), CommandIntent::Other);
    assert_eq!(classify_command_intent("top -h"), CommandIntent::Other);
    assert_eq!(classify_command_intent("top -v"), CommandIntent::Other);
    assert_eq!(classify_command_intent("top -O"), CommandIntent::Other);
    assert_eq!(
        classify_command_intent("LANG=C top --version"),
        CommandIntent::Other
    );
    assert_eq!(
        classify_command_intent("LANG=C top -O"),
        CommandIntent::Other
    );
    assert_eq!(
        classify_command_intent("LANG=C top -b -n1"),
        CommandIntent::Diagnostic
    );
    assert_eq!(
        classify_command_intent("LANG=C top --batch --iterations=1"),
        CommandIntent::Diagnostic
    );
    assert_eq!(
        classify_command_intent("top -b --iterations=1"),
        CommandIntent::Diagnostic
    );
    assert_eq!(
        classify_command_intent("top -bn 1"),
        CommandIntent::Diagnostic
    );
    assert_eq!(
        classify_command_intent("top -b -n 1 -w 512"),
        CommandIntent::Diagnostic
    );
    assert_eq!(
        classify_command_intent("top -b -n1 -w512"),
        CommandIntent::Diagnostic
    );
    assert_eq!(
        classify_command_intent("top -b -d 1 -n1"),
        CommandIntent::Diagnostic
    );
    assert_eq!(
        classify_command_intent("top -b -d1 -n1"),
        CommandIntent::Diagnostic
    );
    assert_eq!(
        classify_command_intent("top -bd 1 -n1"),
        CommandIntent::Diagnostic
    );
    assert_eq!(
        classify_command_intent("top -b -c -n1"),
        CommandIntent::Diagnostic
    );
    assert_eq!(
        classify_command_intent("top -bc -n1"),
        CommandIntent::Diagnostic
    );
    assert_eq!(
        classify_command_intent("top -b -H -n1"),
        CommandIntent::Diagnostic
    );
    assert_eq!(
        classify_command_intent("top -b -1 -n1"),
        CommandIntent::Diagnostic
    );
    assert_eq!(
        classify_command_intent("top -b1 -n1"),
        CommandIntent::Diagnostic
    );
    assert_eq!(
        classify_command_intent("top -b -S -n1"),
        CommandIntent::Diagnostic
    );
    assert_eq!(
        classify_command_intent("top -b -i -n1"),
        CommandIntent::Diagnostic
    );
    assert_eq!(
        classify_command_intent("top -b -n1 -p 1234"),
        CommandIntent::Diagnostic
    );
    assert_eq!(
        classify_command_intent("top -b -n1 -u root"),
        CommandIntent::Diagnostic
    );
    assert_eq!(
        classify_command_intent("top -b -n1 -U root"),
        CommandIntent::Diagnostic
    );
    assert_eq!(
        classify_command_intent("top -b -n1 -o%MEM"),
        CommandIntent::Diagnostic
    );
    assert_eq!(
        classify_command_intent("top -b -n1 -E g"),
        CommandIntent::Diagnostic
    );
    assert_eq!(
        classify_command_intent("top -b"),
        CommandIntent::Interactive
    );
    assert_eq!(
        classify_command_intent("top --batch"),
        CommandIntent::Interactive
    );
    assert_eq!(
        classify_command_intent("top -b -n2"),
        CommandIntent::Interactive
    );
}

#[test]
fn diagnostic_top_head_keeps_consultation_card() {
    let findings = vec![finding("memory-pressure", FindingSeverity::Critical)];
    let aggregated = aggregate_hook_findings(findings);
    let block = block_with_command("top -b -n1 | head -20");
    let display = display_for_aggregate(&block, &aggregated[0], false);
    let suppression_key = suppression_key(&block, &aggregated[0]);

    assert_eq!(display, RuntimeHookDisplay::Consultation);
    assert_eq!(
        apply_session_interruption_policy(
            &block,
            &aggregated[0],
            display,
            &suppression_key,
            &HookRuntimeState::default(),
            false
        ),
        RuntimeHookDisplay::Consultation
    );
}

#[test]
fn wrapper_intent_downgrades_consultation_to_hint() {
    let findings = vec![finding("memory-pressure", FindingSeverity::Critical)];
    let aggregated = aggregate_hook_findings(findings);
    let block = block_with_command("sudo free -m");
    let key = suppression_key(&block, &aggregated[0]);

    assert_eq!(
        apply_session_interruption_policy(
            &block,
            &aggregated[0],
            RuntimeHookDisplay::Consultation,
            &key,
            &HookRuntimeState::default(),
            false
        ),
        RuntimeHookDisplay::Hint
    );

    let env_sudo_block = block_with_command("LANG=C sudo -n free -m");
    let env_sudo_suppression_key = suppression_key(&env_sudo_block, &aggregated[0]);
    assert_eq!(
        apply_session_interruption_policy(
            &env_sudo_block,
            &aggregated[0],
            RuntimeHookDisplay::Consultation,
            &env_sudo_suppression_key,
            &HookRuntimeState::default(),
            false
        ),
        RuntimeHookDisplay::Hint
    );
}

#[test]
fn non_diagnostic_pipeline_downgrades_consultation_to_hint() {
    let findings = vec![finding("high-memory-process", FindingSeverity::Critical)];
    let aggregated = aggregate_hook_findings(findings);
    let block = block_with_command("ps aux | awk '{print $2}'");
    let suppression_key = suppression_key(&block, &aggregated[0]);

    assert_eq!(
        apply_session_interruption_policy(
            &block,
            &aggregated[0],
            RuntimeHookDisplay::Consultation,
            &suppression_key,
            &HookRuntimeState::default(),
            false
        ),
        RuntimeHookDisplay::Hint
    );
}
