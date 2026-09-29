//! Final PostTool candidate arbitration.

use tokenless_compressors::Recoverability;
use tokenless_protocol::{Disposition, estimate_tokens};

/// Inputs to the one final PostTool decision.
pub(super) struct ArbitrationInput<'a> {
    pub(super) original: &'a str,
    pub(super) candidate: &'a str,
    pub(super) has_operations: bool,
    pub(super) min_token_savings: usize,
    pub(super) recoverability: Recoverability,
    pub(super) require_reversibility: bool,
    pub(super) dry_run: bool,
    pub(super) timed_out: bool,
}

/// Whether the candidate reaches the model or is measured/rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Verdict {
    Apply,
    DryRun,
    Reject(Disposition),
}

pub(super) fn decide(input: &ArbitrationInput<'_>) -> Verdict {
    if input.timed_out {
        return Verdict::Reject(Disposition::Timeout);
    }
    if !input.has_operations {
        return Verdict::Reject(Disposition::NoSavings);
    }
    if input.require_reversibility && input.recoverability == Recoverability::Unrecoverable {
        return Verdict::Reject(Disposition::RecoverabilityUnavailable);
    }
    let saves_chars = input.candidate.chars().count() < input.original.chars().count();
    let original_tokens = estimate_tokens(input.original);
    let candidate_tokens = estimate_tokens(input.candidate);
    let saves_tokens = candidate_tokens < original_tokens
        && original_tokens - candidate_tokens >= input.min_token_savings;
    if !(saves_chars && saves_tokens) {
        return Verdict::Reject(Disposition::NoSavings);
    }
    if input.dry_run {
        Verdict::DryRun
    } else {
        Verdict::Apply
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input<'a>(original: &'a str, candidate: &'a str) -> ArbitrationInput<'a> {
        ArbitrationInput {
            original,
            candidate,
            has_operations: true,
            min_token_savings: 1,
            recoverability: Recoverability::Lossless,
            require_reversibility: false,
            dry_run: false,
            timed_out: false,
        }
    }

    #[test]
    fn candidate_must_save_both_characters_and_tokens() {
        assert_eq!(decide(&input("abcdefgh", "abc")), Verdict::Apply);
        assert_eq!(
            decide(&input("abcd", "你")),
            Verdict::Reject(Disposition::NoSavings)
        );
    }

    #[test]
    fn minimum_savings_includes_the_entire_candidate_and_applies_to_dry_run() {
        let original = "x".repeat(400);
        // The final candidate includes the recovery hint and display wrapper.
        let below = format!("{}{}", "[retrieve original]\n", "x".repeat(321));
        let boundary = "x".repeat(336);
        let mut case = input(&original, &below);
        case.min_token_savings = 16;
        assert_eq!(decide(&case), Verdict::Reject(Disposition::NoSavings));
        case.dry_run = true;
        assert_eq!(decide(&case), Verdict::Reject(Disposition::NoSavings));
        case.candidate = &boundary;
        assert_eq!(decide(&case), Verdict::DryRun);
        case.dry_run = false;
        assert_eq!(decide(&case), Verdict::Apply);
    }

    #[test]
    fn dry_run_and_reversibility_have_explicit_verdicts() {
        let mut case = input("abcdefgh", "abc");
        case.dry_run = true;
        assert_eq!(decide(&case), Verdict::DryRun);

        case.dry_run = false;
        case.require_reversibility = true;
        case.recoverability = Recoverability::Unrecoverable;
        assert_eq!(
            decide(&case),
            Verdict::Reject(Disposition::RecoverabilityUnavailable)
        );

        case.require_reversibility = false;
        case.timed_out = true;
        assert_eq!(decide(&case), Verdict::Reject(Disposition::Timeout));
    }
}
