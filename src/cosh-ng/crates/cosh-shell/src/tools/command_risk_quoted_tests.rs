//! Quoted SAFE_OUTPUT_SINK redirection coverage for issue #1752.
//!
//! Declared only from `lib.rs` (the `wrap_tests` pattern) so the cases
//! stay out of the lib/bin test overlap ratchet
//! (`scripts/check-test-inventory.sh`): the module is compiled for the
//! `--lib` target only, while `main.rs` does not declare it.

use crate::tools::command_risk::{
    assess_shell_command, AssessmentPolicy, AssessmentSource, AutoAllowEvidence,
    AutoExecutionPolicy, AutoExecutionRoute, CommandAssessment, ExecutionDecision, RiskImpact,
};

fn auto(command: &str) -> CommandAssessment {
    assess_shell_command(
        command,
        AutoExecutionPolicy::current_runtime()
            .assessment_policy(AssessmentSource::ProviderShellTool),
    )
}

fn ask(command: &str) -> CommandAssessment {
    assess_shell_command(
        command,
        AssessmentPolicy::ask(AssessmentSource::ProviderShellTool),
    )
}

#[test]
fn quoted_safe_output_sink_redirection_is_null_suppression() {
    // Issue #1752: an agent-emitted stderr suppression whose target word
    // is wholly quoted (`2>"/dev/null"`, `2>'/dev/null'`) undergoes quote
    // removal to the same literal sink path as the unquoted form — the
    // SAFE_OUTPUT_SINK entries contain no `$`, backtick or backslash, so
    // the quotes cannot introduce expansion — and must join the issue
    // #1667 null-suppression channel instead of the fail-closed
    // RedirectionWrite path.
    for command in [
        "ps aux 2>\"/dev/null\"",
        "ps aux 2>'/dev/null'",
        "cat x 2>>\"/dev/null\"",
        "cat x 2>>'/dev/null'",
        "du -sh /var 2> '/dev/null'",
        "ls >\"/dev/null\"",
        "ls 1>'/dev/null'",
        "find /tmp -maxdepth 3 -name '*cosh*' 2>\"/dev/null\"",
    ] {
        let assessment = ask(command);
        assert_ne!(assessment.impact, RiskImpact::High, "{command}");
        assert!(
            !assessment.reasons.contains(&"redirection-write"),
            "{command}: {:?}",
            assessment.reasons
        );
        assert!(
            assessment.reasons.contains(&"output-suppressed"),
            "{command}: {:?}",
            assessment.reasons
        );
    }

    // Stdout suppression still requires approval even for quoted null sinks.
    let auto_policy = auto("ps aux >\"/dev/null\"");
    assert_eq!(auto_policy.execution, ExecutionDecision::AskUser);
    assert!(auto_policy.auto_allow.is_none());
}

#[test]
fn quoted_non_sink_redirection_targets_stay_fail_closed() {
    // Issue #1752 narrows the issue #1667 V-M8 fail-closed rule only for
    // whole-word quoted SAFE_OUTPUT_SINK targets. Every other quoted
    // target keeps the RedirectionWrite classification: regular files,
    // expansion, and suffix concatenation (`'/dev/null'x` builds the
    // different word `/dev/nullx` in every shell).
    for command in [
        "cat log 2>\"/tmp/evil\"",
        "cat log 2>'/tmp/evil'",
        "cat log 2>\"$F\"",
        "cat log 2>$FILE",
        "cat log 2>'/dev/null'x",
        "cat log 2>'/dev/nul*'",
        "ls 2>' /dev/null'",
        "ls &>'/dev/null'",
    ] {
        let assessment = ask(command);
        assert_eq!(assessment.impact, RiskImpact::High, "{command}");
        assert!(
            assessment.reasons.contains(&"redirection-write"),
            "{command}: {:?}",
            assessment.reasons
        );
    }
}

#[test]
fn stderr_only_suppression_requires_readonly_execution_evidence() {
    for command in [
        "find /tmp -maxdepth 3 -name '*cosh*' 2>/dev/null",
        "find /tmp -maxdepth 3 -name '*cosh*' 2>>/dev/null",
        "find /tmp -maxdepth 3 -name '*cosh*' 2> /dev/null",
        "find /tmp -maxdepth 3 -name '*cosh*' 2>\"/dev/null\"",
        "find /tmp -maxdepth 3 -name '*cosh*' 2>'/dev/null'",
        "ls 2>/dev/null",
        "ls\t2>\t'/dev/null'",
        "2>/dev/null ls",
        "ls 2>/dev/null 2>>'/dev/null'",
        "grep needle notes.txt 2>/dev/null",
    ] {
        let assessment = auto(command);
        assert_eq!(
            assessment.execution,
            ExecutionDecision::AutoAllow,
            "{command}"
        );
        assert_eq!(assessment.impact, RiskImpact::Low, "{command}");
        assert_eq!(
            assessment.auto_allow,
            Some(AutoAllowEvidence::StderrSuppressedReadonly),
            "{command}"
        );
        assert!(
            assessment.reasons.contains(&"output-suppressed"),
            "{command}"
        );
        assert_eq!(
            AutoExecutionPolicy::current_runtime().route(&assessment),
            AutoExecutionRoute::CompoundReadonlyExecutor,
            "{command}"
        );
        assert_eq!(
            ask(command).execution,
            ExecutionDecision::AskUser,
            "{command}"
        );
        assert!(ask(command).auto_allow.is_none(), "{command}");
    }

    for command in [
        "ls >/dev/null",
        "ls>/dev/null",
        "ls 1>/dev/null",
        "ls 2 >/dev/null",
        "ls 2>/dev/null >/dev/null",
        "ls >/dev/null 2>/dev/null",
        "ls &>/dev/null",
        "ls 2>&1",
        "ls 2>&-",
        "ls 2>/dev/null 2>&1",
        "ls 2>&1 2>/dev/null",
        "ls 2>/dev/null 2>&-",
        "ls 2>/dev/null 1>&2",
        "ls 0>/dev/null",
        "ls 3>/dev/null",
        "ls 02>/dev/null",
        "ls 2>/dev/full",
        "ls 2>/tmp/errors",
        "ls 2>>/tmp/errors",
        "ls 2>\"$SINK\"",
        "ls 2>'/dev/null'x",
        "ls 2>'/dev/null'{x,y}",
        "ls 2>/dev/null | cat",
        "ls 2>/dev/null && pwd",
        "ls 2>/dev/null\npwd",
        "ls 2>/dev/null;rm -rf x",
        "rm -rf x 2>/dev/null",
        "find . -delete 2>/dev/null",
        "cat /etc/shadow 2>/dev/null",
        "grep password notes.txt 2>/dev/null",
        "cat -- - 2>/dev/null",
        "env 2>/dev/null",
        "which cargo 2>/dev/null",
        "ls $HOME 2>/dev/null",
        "ls $(pwd) 2>/dev/null",
        "ls #comment 2>/dev/null",
        "ls 2>/dev/null #comment",
        "ls \"a\\\"b\\\"c\" 2>/dev/null",
        "ls a\\\nb 2>/dev/null",
        "HOME=/tmp ls 2>/dev/null",
        "not_a_readonly_command 2>/dev/null",
    ] {
        let assessment = auto(command);
        assert_ne!(
            assessment.execution,
            ExecutionDecision::AutoAllow,
            "{command}"
        );
        assert!(assessment.auto_allow.is_none(), "{command}");
    }
    let delete = auto("rm -rf x 2>/dev/null");
    assert_eq!(delete.impact, RiskImpact::High);
    assert!(delete.reasons.contains(&"filesystem-delete"));
}

#[test]
fn stderr_suppressed_argv_execution_keeps_stdout_and_failure_status() {
    use crate::tools::readonly_compound::{build_readonly_compound_plan, run_readonly_compound};
    use crate::tools::ReadonlyPipelineConfig;

    let dir = tempfile::tempdir().expect("isolated command directory");
    std::fs::write(dir.path().join("cosh fixture"), "visible output\n").expect("write fixture");
    std::fs::write(dir.path().join("unrelated"), "other\n").expect("write control fixture");
    let search = build_readonly_compound_plan("find . -maxdepth 3 -name '*cosh*' 2>/dev/null")
        .expect("quoted find pattern");
    let found = run_readonly_compound(&search, &ReadonlyPipelineConfig::default(), dir.path())
        .expect("execute search");
    assert_eq!(found.stdout, "./cosh fixture\n");
    assert!(found.stderr.is_empty());
    assert_eq!(found.exit_code, Some(0));

    let plan = build_readonly_compound_plan("cat 'cosh fixture' missing 2>\"/dev/null\"")
        .expect("stderr-only readonly plan");
    assert_eq!(plan.steps.len(), 1);
    assert_eq!(plan.steps[0].argv, ["cat", "cosh fixture", "missing"]);
    assert!(plan.steps[0].suppress_stderr);
    let output = run_readonly_compound(&plan, &ReadonlyPipelineConfig::default(), dir.path())
        .expect("execute readonly plan");
    assert_eq!(output.stdout, "visible output\n");
    assert!(output.stderr.is_empty());
    assert_eq!(output.exit_code, Some(1));

    let mut unsuppressed = plan;
    unsuppressed.steps[0].suppress_stderr = false;
    let control = run_readonly_compound(
        &unsuppressed,
        &ReadonlyPipelineConfig::default(),
        dir.path(),
    )
    .expect("execute control");
    assert_eq!(control.stdout, output.stdout);
    assert_eq!(control.exit_code, output.exit_code);
    assert!(
        !control.stderr.is_empty(),
        "missing file must produce stderr in the control"
    );
}

#[test]
fn dangling_escapes_never_produce_readonly_execution_plans() {
    use crate::tools::command_risk::CommandShape;
    use crate::tools::readonly_compound::build_readonly_compound_plan;

    for command in [
        "ls 2>/dev/null \\",
        "ls 2>/dev/null foo\\",
        "ls\t2>'/dev/null'\t\\",
        "ls 2>\"/dev/null\" \\",
        "ls 2>/dev/null \"\\",
        "ls 2>/dev/null '\\",
        "ls \\",
        "pwd && ls \\",
        "pwd;ls foo\\",
        "pwd\nls \\",
    ] {
        assert!(build_readonly_compound_plan(command).is_none(), "{command}");
        let assessment = auto(command);
        assert_eq!(assessment.shape, CommandShape::Unparseable, "{command}");
        assert_ne!(
            assessment.execution,
            ExecutionDecision::AutoAllow,
            "{command}"
        );
        assert!(assessment.auto_allow.is_none(), "{command}");
    }

    // Complete escapes and quoted literal backslashes remain parseable.
    for command in [r"ls foo\ bar", r"ls \\", r"ls '\'", r#"ls "\\""#] {
        assert_eq!(auto(command).shape, CommandShape::Simple, "{command}");
    }
}

#[test]
fn shell_expansions_require_approval_but_quoted_patterns_remain_literal() {
    use crate::tools::readonly_compound::build_readonly_compound_plan;

    for command in [
        "cat ~/notes 2>/dev/null",
        "ls *.rs 2>/dev/null",
        "ls file?.rs 2>/dev/null",
        "ls [ab].rs 2>/dev/null",
        "ls 'prefix'*.rs 2>/dev/null",
        "ls\t*.rs\t2>'/dev/null'",
        "ls *.rs 2>/dev/null;pwd",
        "pwd\nls *.rs 2>/dev/null",
        "cat =bash 2>/dev/null",
        "ls ^notes 2>/dev/null",
        "cat !! 2>/dev/null",
        "cat \"!!\" 2>/dev/null",
    ] {
        assert!(build_readonly_compound_plan(command).is_none(), "{command}");
        let assessment = auto(command);
        assert_ne!(
            assessment.execution,
            ExecutionDecision::AutoAllow,
            "{command}"
        );
        assert!(assessment.auto_allow.is_none(), "{command}");
    }
    for (command, operand) in [
        ("find . -name '*cosh*' 2>/dev/null", "*cosh*"),
        ("ls '*.rs' 2>/dev/null", "*.rs"),
        ("ls \"file?.rs\" 2>/dev/null", "file?.rs"),
        ("ls '[ab].rs' 2>/dev/null", "[ab].rs"),
        ("cat '~/notes' 2>/dev/null", "~/notes"),
        ("cat '!!' 2>/dev/null", "!!"),
        (r"ls \*.rs 2>/dev/null", "*.rs"),
    ] {
        let plan = build_readonly_compound_plan(command).expect(command);
        assert_eq!(plan.steps[0].argv.last().map(String::as_str), Some(operand));
        assert_eq!(
            auto(command).execution,
            ExecutionDecision::AutoAllow,
            "{command}"
        );
    }
}
