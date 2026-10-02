//! Whether each claim can be traced back to something the trajectory observed.
//!
//! The evidence pool is built from what the agent was *given*: the user's own
//! messages and the results of tool calls that actually succeeded. What the
//! agent itself wrote is never evidence for what the agent later asserts —
//! otherwise a fabrication would ground itself.
//!
//! Three exclusions matter as much as the inclusions, each corresponding to a
//! way a claim looks grounded without being so:
//!
//! * results of failed calls, whose error text routinely quotes the very path or
//!   value being claimed;
//! * command lines echoed back in a result, which are the agent's own words;
//! * tool definitions and system prompts, whose example URLs and paths would
//!   otherwise ground half the corpus.
//!
//! The pool spans every step up to the end of the round under analysis, not just
//! the round itself. A fact retrieved in round 1 and restated in round 3 is
//! grounded, and scoping the pool to one round is the single largest source of
//! false "ungrounded" verdicts.

use std::ops::Range;

use agentsight_atif::{AtifTrajectory, Step, StepSource, same_call_id};

use super::claims::{Claim, ClaimClass, extract_claims, numbers_agree};
use super::outcome::{CallStatus, CallVerdict, Confidence, classify_call, result_text};

/// Cap on pool numbers considered when testing whether a claim was computed from
/// evidence. The check is pairwise, so this bounds it to a fixed cost.
const DERIVED_SEARCH_CAP: usize = 200;

/// Where a claim's support comes from, or why it has none.
///
/// Self-evident values have no variant here: arithmetic and small indices never
/// become claims in the first place, so there is nothing to ground.
#[derive(Debug, Clone, PartialEq)]
pub enum Grounding {
    /// Traced to an observation or to the user's own words.
    Grounded {
        step_id: usize,
        source_call_id: Option<String>,
    },
    /// Computed from values that are themselves in evidence. Not a fabrication,
    /// so it must never anchor a finding.
    Derived,
    /// No support found by string matching. A candidate for semantic review,
    /// not yet a verdict.
    Unresolved,
    /// String matching found nothing, but a semantic review recognised the same
    /// fact in the evidence — a reformatting, a translation, a rounding.
    ///
    /// Deliberately distinct from [`Grounding::Grounded`]: the model may clear a
    /// claim, never promote one to re-checkable evidence.
    ClearedByModel,
}

/// One claim with its origin step and its grounding outcome.
#[derive(Debug, Clone)]
pub struct GroundedClaim {
    pub claim: Claim,
    /// Step in which the agent asserted it.
    pub step_id: usize,
    pub grounding: Grounding,
}

/// A deterministic observation about the round, stated so a reader can re-check
/// it. Findings never assert intent — that is the LLM's job, on a much narrower
/// question.
#[derive(Debug, Clone)]
pub enum Finding {
    /// Earliest step asserting a fact with no traceable source.
    UngroundedOnset { step_id: usize, claim: Claim },
    /// The same call was repeated unchanged after it had already failed, and
    /// kept failing. Retrying an identical request that just failed cannot
    /// succeed for a different reason, so this is a fault in the agent's control
    /// flow rather than an environment problem.
    RepeatedIdenticalFailure {
        step_id: usize,
        function_name: String,
        attempts: usize,
        quote: Option<String>,
    },
    /// A call failed, and a later step asserted a fact of the kind that call was
    /// supposed to supply. This is the "the tool broke, then it made something
    /// up" shape, and the edge between the two steps is evidence rather than
    /// inference.
    FailureThenFabrication {
        failed_step_id: usize,
        function_name: String,
        failure_quote: Option<String>,
        claim_step_id: usize,
        claim: Claim,
    },
}

/// What became of a failed call.
///
/// A single failure says almost nothing on its own: across the measured corpus
/// the agent adapted and carried on after most of them, so treating every error
/// as a defect would condemn ordinary behaviour. What distinguishes a defect is
/// the aftermath.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Aftermath {
    /// The same call, with the same arguments, succeeded later. The agent
    /// recovered; the failure cost time and nothing else.
    Recovered,
    /// The same call was repeated with the same arguments and kept failing.
    /// Repeating an identical request that already failed is not adaptation.
    Persisted,
    /// Failed once and never attempted again in this form. Whether that was
    /// wise depends on what the round needed, which rules cannot settle.
    NotRetried,
}

/// Per-call classification, kept alongside its step so findings can point at it.
#[derive(Debug, Clone)]
pub struct StepCallVerdict {
    pub step_id: usize,
    pub function_name: String,
    pub tool_call_id: String,
    /// For a failed call, what happened to it afterwards. `None` for calls that
    /// did not fail.
    pub aftermath: Option<Aftermath>,
    /// The call's arguments, rendered compactly.
    ///
    /// Carried because the cause of a failure usually sits in the command
    /// itself: an error reading `no such column: complete` is only explicable
    /// once you can see the query quoted the value with double quotes.
    pub arguments: String,
    pub verdict: CallVerdict,
}

/// Everything the deterministic layer concluded about a round.
#[derive(Debug, Clone)]
pub struct GroundingIndex {
    pub claims: Vec<GroundedClaim>,
    pub call_verdicts: Vec<StepCallVerdict>,
    pub findings: Vec<Finding>,
    /// The same evidence the deterministic pass used, capped for prompt use.
    /// Sharing it keeps a semantic review from judging against a different set
    /// of facts than the rules did.
    pub evidence_digest: String,
    /// `step_id` of the round's first step, needed to recompute findings after a
    /// review and to keep a finding from being pinned to the previous round.
    ///
    /// A `step_id`, not an index: ATIF guarantees `step_id == index + 1`
    /// (`validate_step_ids`), and this is compared against
    /// [`StepCallVerdict::step_id`].
    pub round_start_step: usize,
}

impl GroundingIndex {
    /// Claims that string matching could not place, in `claims` order.
    ///
    /// Only classes allowed to accuse are returned: a reworded quote is not
    /// worth an LLM call, let alone a finding.
    pub fn pending_review(&self) -> Vec<(usize, &Claim)> {
        self.claims
            .iter()
            .enumerate()
            .filter(|(_, c)| c.grounding == Grounding::Unresolved && c.claim.can_anchor_finding())
            .map(|(i, c)| (i, &c.claim))
            .collect()
    }

    /// Record the claims a semantic review recognised, then recompute findings.
    ///
    /// Clearing is the only direction a review may push: an unrecognised claim
    /// keeps its `Unresolved` state and the finding that follows from it.
    pub fn apply_review(&mut self, cleared: &[usize]) {
        for &i in cleared {
            if let Some(c) = self.claims.get_mut(i) {
                if c.grounding == Grounding::Unresolved {
                    c.grounding = Grounding::ClearedByModel;
                }
            }
        }
        self.findings = derive_findings(&self.claims, &self.call_verdicts, self.round_start_step);
    }

    /// Share of calls whose status could not be established.
    ///
    /// Past a threshold the caller must decline to name a root cause: a round
    /// mostly made of unknowns cannot support one, and guessing is what the
    /// previous pipeline did.
    pub fn unknown_ratio(&self) -> f64 {
        let (unknown, total) = self
            .call_verdicts
            .iter()
            .filter(|verdict| verdict.step_id >= self.round_start_step)
            .fold((0usize, 0usize), |(unknown, total), verdict| {
                (
                    unknown + usize::from(verdict.verdict.status == CallStatus::Unknown),
                    total + 1,
                )
            });
        if total == 0 {
            0.0
        } else {
            unknown as f64 / total as f64
        }
    }

    /// Claims with no traceable source.
    pub fn unresolved_count(&self) -> usize {
        self.claims
            .iter()
            .filter(|c| c.grounding == Grounding::Unresolved)
            .count()
    }

    /// Whether anything deterministic was found that also carries a
    /// consequence. When false, an LLM opinion is all that is left, and it must
    /// be presented as a suspicion.
    ///
    /// Not every true finding condemns a round. `RepeatedIdenticalFailure` is a
    /// fact about one call and says nothing about whether the round eventually
    /// delivered — a live capture showed an agent retry an identical query three
    /// times, adapt, and still answer correctly. Counting it here condemned that
    /// round, so only findings that tie a failure to a damaged outcome qualify.
    pub fn has_deterministic_finding(&self) -> bool {
        self.findings.iter().any(Finding::may_drive_verdict)
    }
}

impl Finding {
    /// Whether this finding is strong enough to establish that the round went
    /// wrong, as opposed to merely describing something that happened in it.
    pub fn may_drive_verdict(&self) -> bool {
        match self {
            // The tool did not supply the value and the value was stated anyway:
            // the damage is in the finding itself.
            Self::FailureThenFabrication { .. } => true,
            // Ungrounded content is asserted with no source, which is the defect
            // rather than evidence of one elsewhere.
            Self::UngroundedOnset { .. } => true,
            // True, and worth showing, but silent on the outcome.
            Self::RepeatedIdenticalFailure { .. } => false,
        }
    }

    /// Evidence-tier label, ordered lexicographically by the caller so that
    /// `min` picks the strongest. Kept beside [`Self::may_drive_verdict`] so a
    /// new variant cannot be scored without also deciding whether it accuses.
    ///
    /// A finding that may not drive a verdict reports `L4`: it establishes
    /// nothing about the round, so it must not raise the confidence shown.
    pub fn tier(&self) -> &'static str {
        match self {
            // A failed call plus a claim only it could have supplied is a
            // directed, quotable edge.
            Self::FailureThenFabrication { .. } => "L2",
            Self::UngroundedOnset { .. } => "L3",
            Self::RepeatedIdenticalFailure { .. } => "L4",
        }
    }
}

/// One usable piece of evidence.
struct EvidenceEntry {
    step_id: usize,
    source_call_id: Option<String>,
    /// Lowercased text, for containment checks.
    haystack: String,
    numbers: Vec<f64>,
}

/// Build the index for `round`, an index range into `doc.steps`.
///
/// Claims are collected from agent steps inside the range; evidence is collected
/// from every step before the range ends.
pub fn build_index(doc: &AtifTrajectory, round: Range<usize>) -> GroundingIndex {
    let end = round.end.min(doc.steps.len());
    let start = round.start.min(end);
    // `step_id` is 1-based (`validate_step_ids`), so the index cannot be
    // compared against one directly; read it off the step where possible.
    let round_start_step = doc.steps.get(start).map_or(start + 1, |s| s.step_id);

    let mut call_verdicts = classify_calls(&doc.steps[..end]);
    assign_aftermath(&mut call_verdicts);
    let pool = build_pool(&doc.steps[..end], &call_verdicts);
    let claims = ground_claims(&doc.steps[start..end], &pool);
    let findings = derive_findings(&claims, &call_verdicts, round_start_step);
    let evidence_digest = digest_pool(&pool);

    GroundingIndex {
        claims,
        call_verdicts,
        findings,
        evidence_digest,
        round_start_step,
    }
}

/// Upper bound on the evidence handed to a semantic review. Enough to carry the
/// facts, small enough to keep one call cheap.
const EVIDENCE_DIGEST_LIMIT: usize = 12_000;

/// Flatten the pool into text, newest entries first so a cap trims the oldest.
fn digest_pool(pool: &[EvidenceEntry]) -> String {
    let mut out = String::new();
    for entry in pool.iter().rev() {
        if out.len() >= EVIDENCE_DIGEST_LIMIT {
            break;
        }
        let remaining = EVIDENCE_DIGEST_LIMIT - out.len();
        let take: String = entry.haystack.chars().take(remaining).collect();
        out.push_str(&format!("[step{}] {}\n", entry.step_id, take));
    }
    out
}

/// Classify every tool call in `steps`, correlating results by call id.
fn classify_calls(steps: &[Step]) -> Vec<StepCallVerdict> {
    let mut out = Vec::new();
    for (step_idx, step) in steps.iter().enumerate() {
        let Some(calls) = step.tool_calls.as_ref() else {
            continue;
        };
        for call in calls {
            // A result may live on this step or a later one. Each invocation owns
            // the window up to the next call reusing its ID. This protects old
            // trajectories whose provider-omitted IDs restart as `auto_0`: a
            // later call cannot inherit an earlier result, and an earlier call
            // with no result cannot steal the later call's result.
            let end = steps[step_idx + 1..]
                .iter()
                .position(|candidate| {
                    candidate.tool_calls.as_ref().is_some_and(|calls| {
                        calls
                            .iter()
                            .any(|next| same_call_id(&next.tool_call_id, &call.tool_call_id))
                    })
                })
                .map_or(steps.len(), |offset| step_idx + 1 + offset);
            let result = steps[step_idx..end]
                .iter()
                .flat_map(|s| s.observation.iter())
                .flat_map(|o| o.results.iter())
                .find(|r| {
                    r.source_call_id
                        .as_deref()
                        .is_some_and(|id| same_call_id(id, &call.tool_call_id))
                });
            out.push(StepCallVerdict {
                step_id: step.step_id,
                function_name: call.function_name.clone(),
                tool_call_id: call.tool_call_id.clone(),
                arguments: serde_json::to_string(&call.arguments).unwrap_or_default(),
                aftermath: None,
                verdict: classify_call(call, result),
            });
        }
    }
    out
}

/// Decide what became of each failed call by looking at what came after it.
///
/// Identity is the same function with the same arguments: a retry that changes
/// the command is adaptation, not repetition, and must not be read as either
/// recovery of the original or persistence in it.
fn assign_aftermath(verdicts: &mut [StepCallVerdict]) {
    let snapshot: Vec<(String, String, CallStatus, usize)> = verdicts
        .iter()
        .map(|v| {
            (
                v.function_name.clone(),
                v.arguments.clone(),
                v.verdict.status,
                v.step_id,
            )
        })
        .collect();

    for (i, v) in verdicts.iter_mut().enumerate() {
        if v.verdict.status != CallStatus::Failed {
            continue;
        }
        let same_later = snapshot
            .iter()
            .enumerate()
            .filter(|(j, (f, a, _, _))| *j > i && *f == v.function_name && *a == v.arguments)
            .map(|(_, (_, _, status, _))| *status);

        let mut recovered = false;
        let mut repeats = 0usize;
        for status in same_later {
            match status {
                CallStatus::Ok | CallStatus::OkProbe => {
                    recovered = true;
                    break;
                }
                CallStatus::Failed => repeats += 1,
                _ => {}
            }
        }
        v.aftermath = Some(if recovered {
            Aftermath::Recovered
        } else if repeats >= 1 {
            Aftermath::Persisted
        } else {
            Aftermath::NotRetried
        });
    }
}

/// Collect usable evidence from the step prefix.
fn build_pool(steps: &[Step], verdicts: &[StepCallVerdict]) -> Vec<EvidenceEntry> {
    let mut pool = Vec::new();
    for step in steps {
        match step.source {
            // The user's own words are evidence by definition: a fact they
            // supplied needs no further source.
            StepSource::User => push_entry(&mut pool, step.step_id, None, &step.message),
            // System prompts and tool definitions would ground their own example
            // URLs and paths, so they are never evidence.
            StepSource::System => continue,
            StepSource::Agent => {}
        }

        let Some(observation) = step.observation.as_ref() else {
            continue;
        };
        for result in &observation.results {
            let usable = result.is_error() != Some(true)
                && result
                    .source_call_id
                    .as_deref()
                    .and_then(|id| {
                        verdicts
                            .iter()
                            .filter(|v| {
                                v.step_id <= step.step_id && same_call_id(&v.tool_call_id, id)
                            })
                            .max_by_key(|v| v.step_id)
                    })
                    .map(|v| matches!(v.verdict.status, CallStatus::Ok | CallStatus::OkProbe))
                    // A result nobody could link to a call is still an
                    // observation unless the provider explicitly marked it as
                    // an error; dropping every unlinked result would lose real
                    // evidence.
                    .unwrap_or(true);
            if !usable {
                continue;
            }
            let text = result_text(result);
            let cleaned = strip_command_echo(&text, step, result.source_call_id.as_deref());
            push_entry(
                &mut pool,
                step.step_id,
                result.source_call_id.clone(),
                &cleaned,
            );
        }
    }
    pool
}

fn push_entry(
    pool: &mut Vec<EvidenceEntry>,
    step_id: usize,
    source_call_id: Option<String>,
    text: &str,
) {
    if text.trim().is_empty() {
        return;
    }
    let numbers = extract_claims(text)
        .into_iter()
        .filter(|c| c.class == ClaimClass::Number)
        .filter_map(|c| c.value)
        .collect();
    pool.push(EvidenceEntry {
        step_id,
        source_call_id,
        haystack: text.to_lowercase(),
        numbers,
    });
}

/// Drop lines that merely echo the command the agent asked for.
///
/// Shell results commonly open with the invocation, so leaving it in would let a
/// URL or path the agent invented ground itself through its own command line.
fn strip_command_echo(text: &str, step: &Step, source_call_id: Option<&str>) -> String {
    let Some(command) = step
        .tool_calls
        .as_ref()
        .and_then(|calls| {
            calls.iter().find(|c| {
                source_call_id.is_some_and(|id| same_call_id(c.tool_call_id.as_str(), id))
            })
        })
        .and_then(|call| {
            call.arguments
                .get("command")
                .or_else(|| call.arguments.get("cmd"))
        })
        .and_then(|v| v.as_str())
    else {
        return text.to_string();
    };

    // At most one line is dropped, because a shell echoes the command once.
    // Filtering every line that appears in the command deleted real output:
    // `echo /etc/hosts` printing `/etc/hosts` is a result, not an echo, and
    // losing it makes a grounded claim read as ungrounded.
    let mut lines = text.lines().peekable();
    let mut leading_blanks: Vec<&str> = Vec::new();
    while lines.peek().is_some_and(|l| l.trim().is_empty()) {
        leading_blanks.push(lines.next().unwrap_or_default());
    }
    if let Some(first) = lines.peek() {
        let bare = first.trim().trim_start_matches(['$', '>', '#']).trim();
        if !bare.is_empty() && command.contains(bare) {
            lines.next();
        }
    }
    leading_blanks
        .into_iter()
        .chain(lines)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Decide the grounding of every claim made by agent steps in the round.
fn ground_claims(round: &[Step], pool: &[EvidenceEntry]) -> Vec<GroundedClaim> {
    let mut out = Vec::new();
    for step in round {
        if step.source != StepSource::Agent || step.message.is_empty() {
            continue;
        }
        for claim in extract_claims(&step.message) {
            let grounding = ground_one(&claim, pool);
            out.push(GroundedClaim {
                claim,
                step_id: step.step_id,
                grounding,
            });
        }
    }
    out
}

fn ground_one(claim: &Claim, pool: &[EvidenceEntry]) -> Grounding {
    if let Some(value) = claim.value {
        if let Some(entry) = pool
            .iter()
            .find(|e| e.numbers.iter().any(|n| numbers_agree(value, *n)))
        {
            return Grounding::Grounded {
                step_id: entry.step_id,
                source_call_id: entry.source_call_id.clone(),
            };
        }
        if is_derived(value, pool) {
            return Grounding::Derived;
        }
        return Grounding::Unresolved;
    }

    let needle = claim.text.to_lowercase();
    if let Some(entry) = pool.iter().find(|e| e.haystack.contains(&needle)) {
        return Grounding::Grounded {
            step_id: entry.step_id,
            source_call_id: entry.source_call_id.clone(),
        };
    }
    Grounding::Unresolved
}

/// Whether a value follows arithmetically from two values already in evidence.
///
/// Agents routinely report a total, a difference or a percentage they computed
/// from retrieved figures. Those are honest derivations, so they must not be
/// mistaken for invention.
fn is_derived(value: f64, pool: &[EvidenceEntry]) -> bool {
    let numbers: Vec<f64> = pool
        .iter()
        .flat_map(|e| e.numbers.iter().copied())
        .take(DERIVED_SEARCH_CAP)
        .collect();
    for (i, a) in numbers.iter().enumerate() {
        for b in &numbers[i..] {
            if numbers_agree(value, a + b)
                || numbers_agree(value, (a - b).abs())
                || numbers_agree(value, a * b)
            {
                return true;
            }
            if *b != 0.0 && numbers_agree(value, a / b) {
                return true;
            }
        }
    }
    false
}

/// Turn groundings and call verdicts into findings.
fn derive_findings(
    claims: &[GroundedClaim],
    verdicts: &[StepCallVerdict],
    round_start_step: usize,
) -> Vec<Finding> {
    let mut findings = Vec::new();

    // A failure the agent kept repeating unchanged is provable from the trace
    // alone, so it counts as evidence rather than opinion.
    let mut counted: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    for v in verdicts.iter().filter(|v| {
        v.aftermath == Some(Aftermath::Persisted)
            && v.verdict.confidence == Confidence::High
            && v.step_id >= round_start_step
    }) {
        let key = (v.function_name.clone(), v.arguments.clone());
        if !counted.insert(key) {
            continue;
        }
        let attempts = verdicts
            .iter()
            .filter(|o| {
                o.function_name == v.function_name
                    && o.arguments == v.arguments
                    && o.verdict.status == CallStatus::Failed
            })
            .count();
        findings.push(Finding::RepeatedIdenticalFailure {
            step_id: v.step_id,
            function_name: v.function_name.clone(),
            attempts,
            quote: v.verdict.evidence_quote.clone(),
        });
    }

    // Only hard classes may accuse; a reworded quote is not an accusation.
    let mut accusable: Vec<&GroundedClaim> = claims
        .iter()
        .filter(|c| c.grounding == Grounding::Unresolved && c.claim.can_anchor_finding())
        .collect();
    accusable.sort_by_key(|c| c.step_id);

    if let Some(first) = accusable.first() {
        findings.push(Finding::UngroundedOnset {
            step_id: first.step_id,
            claim: first.claim.clone(),
        });
    }

    // Per claim, not per failed call. Iterating failures paired every one of
    // them with the same value: a live capture emitted eleven findings naming a
    // single claim, which is unusable however true any one of them is. The
    // nearest preceding failure is also the most plausible source.
    for claim in &accusable {
        let culprit = verdicts
            .iter()
            .filter(|v| {
                // Only a call known to have failed may implicate a later claim.
                // An `Unknown` means no result could be correlated, which is
                // absence of evidence rather than evidence of failure —
                // accusing on it produces exactly the confident-but-unfounded
                // finding this layer exists to avoid. Unresolvable calls raise
                // `unknown_ratio` instead.
                v.verdict.status == CallStatus::Failed
                    && v.step_id >= round_start_step
                    // A failure inferred from a weak text signal is not solid
                    // enough to accuse a later step with.
                    && v.verdict.confidence == Confidence::High
                    && v.step_id < claim.step_id
                    && output_domain(&v.function_name).contains(&claim.claim.class)
            })
            .max_by_key(|v| v.step_id);

        if let Some(failed) = culprit {
            findings.push(Finding::FailureThenFabrication {
                failed_step_id: failed.step_id,
                function_name: failed.function_name.clone(),
                failure_quote: failed.verdict.evidence_quote.clone(),
                claim_step_id: claim.step_id,
                claim: claim.claim.clone(),
            });
        }
    }

    findings
}

/// Claim classes a tool could plausibly have supplied.
///
/// Used to keep a failed call from being blamed for an unrelated fact. Unknown
/// tools admit every class, since assuming a narrow output would silently drop
/// real findings.
fn output_domain(function_name: &str) -> &'static [ClaimClass] {
    let name = function_name.to_lowercase();
    if name.contains("fetch") || name.contains("web") || name.contains("http") {
        return &[ClaimClass::Url, ClaimClass::Number, ClaimClass::Quoted];
    }
    if name.contains("read")
        || name.contains("glob")
        || name.contains("grep")
        || name.contains("search")
    {
        return &[
            ClaimClass::Path,
            ClaimClass::Quoted,
            ClaimClass::Identifier,
            ClaimClass::Number,
            ClaimClass::Version,
        ];
    }
    &[
        ClaimClass::Number,
        ClaimClass::Date,
        ClaimClass::Url,
        ClaimClass::Path,
        ClaimClass::Version,
        ClaimClass::Identifier,
        ClaimClass::Quoted,
    ]
}

#[cfg(test)]
#[path = "evidence_tests.rs"]
mod tests;
