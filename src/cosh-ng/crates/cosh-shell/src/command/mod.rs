#[allow(dead_code)]
pub(crate) mod exit_classify;
#[allow(dead_code)]
pub(crate) mod failure_semantics;

#[allow(unused_imports)]
pub(crate) use exit_classify::{
    classify_executed_command_outcome, classify_exit, classify_shell_handoff_command_outcome,
    first_program_token, CommandOutcome, ExitCodeCategory,
};
#[allow(unused_imports)]
pub(crate) use failure_semantics::{
    classify_failure, interactive_cancel_output, BuildOrTestFamily, FailureAutoEligibility,
    FailureClass, FailureConfidence, FailureExcerptDirection, FailureReason, FailureSemantics,
    FailureTerminalSignature,
};
