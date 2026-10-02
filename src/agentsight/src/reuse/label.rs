//! The trajectory label as the dashboard and the retrieval layer see it: an
//! automatic verdict, an optional human one, and the rule for which wins.
//!
//! The governing decision is that an unconfirmed automatic label is still a
//! usable label. Waiting for someone to click would leave every freshly
//! collected trajectory unusable, so the automatic verdict takes effect
//! immediately and confirmation only records that a human agrees. What
//! confirmation *does* buy is the right to be treated as top-priority
//! evidence — see [`SessionLabel::is_human_backed`].

use serde::{Deserialize, Serialize};

use super::triage::{TriageMetrics, TriageOutcome};

/// Verdict on a whole trajectory.
///
/// Deliberately about the trajectory and nothing finer: a user asked to judge
/// "was this analysis right" should not have to rule on individual findings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrajectoryLabel {
    /// Produced something that held up; a source of positive candidates.
    Good,
    /// Contains a re-checkable problem. Not the same as worthless — this is
    /// where counterexamples come from.
    Bad,
    /// Carries nothing reusable, so it leaves the retrieval scope entirely.
    Useless,
    /// Not enough evidence to say. The default, and not a defect.
    Unknown,
}

impl TrajectoryLabel {
    pub const ALL: [TrajectoryLabel; 4] = [Self::Good, Self::Bad, Self::Useless, Self::Unknown];

    /// Wire and storage representation, matching the serde renaming.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Good => "good",
            Self::Bad => "bad",
            Self::Useless => "useless",
            Self::Unknown => "unknown",
        }
    }

    /// Parses a stored or query-parameter token. `None` for anything unknown so
    /// callers reject it rather than silently filtering on the wrong label.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "good" => Some(Self::Good),
            "bad" => Some(Self::Bad),
            "useless" => Some(Self::Useless),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }

    /// Whether trajectories with this label must be kept out of
    /// retrieval results altogether.
    ///
    /// Only `Useless` qualifies. `Bad` explicitly does not: a trajectory that
    /// went wrong is the raw material for counterexamples.
    pub fn excluded_from_retrieval(&self) -> bool {
        matches!(self, Self::Useless)
    }

    /// Labels a user may assign by hand.
    ///
    /// `Unknown` is absent on purpose: it means "the rules could not tell",
    /// which is not something a person asserts.
    pub fn user_assignable() -> [TrajectoryLabel; 3] {
        [Self::Good, Self::Bad, Self::Useless]
    }
}

/// Whether a human has weighed in on the automatic label, and how.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfirmState {
    /// Nobody has looked. The automatic label is in force regardless.
    Unconfirmed,
    /// A human agreed with the automatic label.
    Confirmed,
    /// A human replaced it.
    Overridden,
}

impl ConfirmState {
    /// Wire and storage representation, matching the serde renaming.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Unconfirmed => "unconfirmed",
            Self::Confirmed => "confirmed",
            Self::Overridden => "overridden",
        }
    }

    /// Parses a stored token; `None` when unrecognised.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "unconfirmed" => Some(Self::Unconfirmed),
            "confirmed" => Some(Self::Confirmed),
            "overridden" => Some(Self::Overridden),
            _ => None,
        }
    }
}

/// A human's verdict on the automatic label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "action", content = "label")]
pub enum LabelAction {
    /// Accept the automatic label as it stands.
    Confirm,
    /// Replace it.
    Override(TrajectoryLabel),
}

/// How a label row reached its current state, for the audit trail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LabelEventKind {
    Confirm,
    Override,
    /// The automatic verdict was recomputed, which never overwrites a human
    /// decision but is recorded so a later disagreement can be explained.
    AutoRetriage,
    /// The second-level model judge produced a verdict.
    LlmJudge,
}

impl LabelEventKind {
    /// Wire and storage representation, matching the serde renaming.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Confirm => "confirm",
            Self::Override => "override",
            Self::AutoRetriage => "auto_retriage",
            Self::LlmJudge => "llm_judge",
        }
    }

    /// Parses a stored token; `None` when unrecognised.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "confirm" => Some(Self::Confirm),
            "override" => Some(Self::Override),
            "auto_retriage" => Some(Self::AutoRetriage),
            "llm_judge" => Some(Self::LlmJudge),
            _ => None,
        }
    }
}

/// Who a trajectory is, as opposed to what the rules think of it.
///
/// Copied from the trajectory store rather than joined at read time. A reviewer
/// working down a list needs to recognise the conversation, and a session id is
/// a UUID — it identifies without describing. Denormalising is safe here because
/// these fields only change when the source file does, and that already forces a
/// re-triage through the content digest.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrajectoryIdentity {
    /// The opening request, which is the closest thing to a title a captured
    /// conversation has. Absent for tool-only transcripts, which record no user
    /// text at all — real Qoder captures are frequently of that shape.
    pub title: Option<String>,
    /// Working directory the session ran in, encoded as the collector stores it.
    pub project: String,
    /// Which product wrote the file: `qoder`, `claude-code`, `codex`.
    pub source: String,
    pub agent_name: String,
    /// Start of the session as the capture recorded it, left as text because
    /// that is how it arrives and no arithmetic is done on it here.
    pub started_at: Option<String>,
    /// Sub-agent transcripts share a parent's id prefix and are rarely worth
    /// reviewing on their own; flagged so the list can say so.
    pub is_subagent: bool,
}

/// Longest title kept. Enough to tell two requests apart on one line.
const MAX_TITLE_CHARS: usize = 160;

impl TrajectoryIdentity {
    /// Trims a first-user-message preview into a single-line title.
    ///
    /// Newlines become spaces because the list renders one row per trajectory,
    /// and a blank message is stored as absent rather than as an empty string —
    /// the two mean the same thing to a reader and one of them sorts oddly.
    pub fn title_from_message(message: Option<&str>) -> Option<String> {
        let cleaned: String = message?
            .chars()
            .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
            .collect();
        let trimmed = cleaned.trim();
        if trimmed.is_empty() {
            return None;
        }
        if trimmed.chars().count() <= MAX_TITLE_CHARS {
            return Some(trimmed.to_string());
        }
        Some(trimmed.chars().take(MAX_TITLE_CHARS).collect::<String>() + "…")
    }
}

/// The label of one trajectory: automatic verdict, human verdict, and the
/// bookkeeping needed to keep them straight.
///
/// Both verdicts are kept for the row's whole life. The automatic one is never
/// erased by a human decision because it is the only evidence available for
/// judging which rules misfire; the human one is never erased by a recompute
/// because that is the promise made when the user was asked to decide.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionLabel {
    pub session_id: String,
    /// Who the trajectory is. Empty until a triage pass has filled it in.
    pub identity: TrajectoryIdentity,
    pub auto_label: TrajectoryLabel,
    pub auto_reason: String,
    /// Rule identifiers behind `auto_label`; see
    /// [`crate::reuse::triage::GroundingSummary::rules`].
    pub auto_rules: Vec<String>,
    /// `None` until somebody decides. Set by both `Confirm` and `Override` so
    /// [`Self::effective_label`] has a single rule to follow.
    pub human_label: Option<TrajectoryLabel>,
    pub human_reason: Option<String>,
    pub confirm_state: ConfirmState,
    /// Who decided. `None` while unconfirmed.
    pub decided_by: Option<String>,
    pub decided_at_ns: Option<i64>,
    /// The automatic verdict changed after a human decided, and still disagrees
    /// with them — the trajectory grew new rounds, most likely. A prompt to
    /// take another look, never a reason to stop honouring the human label.
    pub auto_changed_since_decision: bool,
    /// Verdict of the second-level model judge, when one has run.
    ///
    /// Kept in its own column rather than replacing [`Self::auto_label`]. The
    /// two are reached by different means and both have to stay visible: the
    /// rules' verdict is what `label-stats` measures misfires against, and
    /// overwriting it would erase the record of what the cheap path concluded.
    pub llm_label: Option<TrajectoryLabel>,
    pub llm_reason: Option<String>,
    /// Steps the model's verdict rests on. Empty for anything but `bad`.
    pub llm_cited_steps: Vec<usize>,
    /// Whether the model said `bad` without citing a step and was reduced to
    /// `unknown`. Recorded so the rate can be counted rather than guessed at.
    pub llm_downgraded: bool,
    pub llm_at_ns: Option<i64>,
    pub metrics: TriageMetrics,
    /// Number of grounding findings behind `auto_label`; see
    /// [`crate::reuse::triage::TriageOutcome::n_findings`].
    pub n_findings: usize,
    /// Hash of the ATIF document the automatic verdict was computed from.
    pub source_content_hash: String,
    /// Version of the rules that produced `auto_label`, so a rule change can
    /// trigger a recompute without discarding human decisions.
    pub triage_version: String,
    pub created_at_ns: i64,
    pub updated_at_ns: i64,
}

impl SessionLabel {
    /// Builds an unconfirmed row from a fresh automatic verdict.
    pub fn from_outcome(
        session_id: impl Into<String>,
        identity: TrajectoryIdentity,
        outcome: TriageOutcome,
        source_content_hash: impl Into<String>,
        triage_version: impl Into<String>,
        now_ns: i64,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            identity,
            auto_label: outcome.label,
            auto_reason: outcome.reason,
            auto_rules: outcome.rules,
            human_label: None,
            human_reason: None,
            confirm_state: ConfirmState::Unconfirmed,
            decided_by: None,
            decided_at_ns: None,
            auto_changed_since_decision: false,
            llm_label: None,
            llm_reason: None,
            llm_cited_steps: Vec::new(),
            llm_downgraded: false,
            llm_at_ns: None,
            metrics: outcome.metrics,
            n_findings: outcome.n_findings,
            source_content_hash: source_content_hash.into(),
            triage_version: triage_version.into(),
            created_at_ns: now_ns,
            updated_at_ns: now_ns,
        }
    }

    /// The label every downstream consumer must obey.
    ///
    /// Precedence runs human, then model, then rules. A person who read the
    /// trajectory outranks a model that only read the last round, and the model
    /// outranks the rules because it can tell an asserted fact from a sentence
    /// that merely contains a slash — the distinction the rules got wrong on
    /// every real trajectory measured.
    ///
    /// Falling back to the rules while nothing else has spoken is what makes a
    /// freshly collected trajectory immediately usable.
    pub fn effective_label(&self) -> TrajectoryLabel {
        self.human_label
            .or(self.llm_label)
            .unwrap_or(self.auto_label)
    }

    /// Records a model judgement, returning the event kind to audit.
    ///
    /// Leaves the human decision and the rules' verdict alone. A model may not
    /// overturn a person, and erasing what the rules concluded would take the
    /// misfire statistics with it.
    pub fn apply_judgement(
        &mut self,
        label: TrajectoryLabel,
        reason: String,
        cited_steps: Vec<usize>,
        downgraded: bool,
        now_ns: i64,
    ) -> LabelEventKind {
        self.llm_label = Some(label);
        self.llm_reason = Some(reason);
        self.llm_cited_steps = cited_steps;
        self.llm_downgraded = downgraded;
        self.llm_at_ns = Some(now_ns);
        self.updated_at_ns = now_ns;
        LabelEventKind::LlmJudge
    }

    /// Whether a person actually signed off on this label.
    ///
    /// The gate needs this separately from [`Self::effective_label`]: an
    /// unconfirmed automatic label is good enough to filter retrieval by, but
    /// not good enough to count as a person having settled the label
    /// be promoted to verified. Conflating the two would let every trajectory
    /// arrive pre-blessed.
    pub fn is_human_backed(&self) -> bool {
        !matches!(self.confirm_state, ConfirmState::Unconfirmed)
    }

    /// Whether the human contradicted the rules, which is the signal
    /// `label-stats` aggregates against [`Self::auto_rules`].
    pub fn was_overridden(&self) -> bool {
        matches!(self.confirm_state, ConfirmState::Overridden)
    }

    /// Records a human decision and returns the event kind to audit.
    ///
    /// `Confirm` stores the label that was in force as the human one — the
    /// verdict the person actually read and agreed with. Storing the *automatic*
    /// label here (as an earlier version did, when the rules were the only
    /// automatic source) silently overruled the model judge: confirming a row
    /// the model had called `good` recorded `unknown`, because that was what the
    /// rules had said, and the effective label moved under a decision meant to
    /// endorse it. Pinning the in-force label also keeps `effective_label` to a
    /// single rule: a later recompute cannot move it out from under a decision
    /// somebody already made.
    pub fn apply_decision(
        &mut self,
        action: LabelAction,
        by: impl Into<String>,
        reason: Option<String>,
        now_ns: i64,
    ) -> LabelEventKind {
        let (label, state, kind) = match action {
            LabelAction::Confirm => (
                self.effective_label(),
                ConfirmState::Confirmed,
                LabelEventKind::Confirm,
            ),
            LabelAction::Override(label) => {
                (label, ConfirmState::Overridden, LabelEventKind::Override)
            }
        };
        self.human_label = Some(label);
        self.human_reason = reason;
        self.confirm_state = state;
        self.decided_by = Some(by.into());
        self.decided_at_ns = Some(now_ns);
        // A decision settles the current disagreement by definition.
        self.auto_changed_since_decision = false;
        self.updated_at_ns = now_ns;
        kind
    }

    /// Applies a recomputed automatic verdict, leaving any human decision
    /// untouched.
    ///
    /// Sets [`Self::auto_changed_since_decision`] only when the rules actually
    /// changed their mind *and* still disagree with the human — a recompute that
    /// merely comes round to the human's view is not worth interrupting them
    /// for, and flagging every recompute would make the marker meaningless on
    /// rows that were overridden precisely because the rules were wrong.
    pub fn apply_retriage(
        &mut self,
        identity: TrajectoryIdentity,
        outcome: TriageOutcome,
        source_content_hash: impl Into<String>,
        triage_version: impl Into<String>,
        now_ns: i64,
    ) {
        let previous_auto = self.auto_label;
        self.auto_label = outcome.label;
        self.auto_reason = outcome.reason;
        self.auto_rules = outcome.rules;
        self.metrics = outcome.metrics;
        self.n_findings = outcome.n_findings;
        self.source_content_hash = source_content_hash.into();
        self.triage_version = triage_version.into();
        // Identity is refreshed here too: a changed source file forces a
        // re-triage, and a re-titled conversation should show its new title.
        self.identity = identity;
        if let Some(human) = self.human_label {
            if previous_auto != outcome.label && human != outcome.label {
                self.auto_changed_since_decision = true;
            }
        }
        self.updated_at_ns = now_ns;
    }
}

#[cfg(test)]
mod tests {
    use super::super::triage::TriageMetrics;
    use super::*;

    fn outcome(label: TrajectoryLabel) -> TriageOutcome {
        TriageOutcome {
            label,
            reason: "test".to_string(),
            metrics: TriageMetrics::default(),
            n_findings: 0,
            rules: vec!["R1".to_string()],
        }
    }

    fn row(label: TrajectoryLabel) -> SessionLabel {
        SessionLabel::from_outcome("s1", identity(), outcome(label), "hash-1", "triage-v1", 100)
    }

    #[test]
    fn confirming_pins_the_verdict_that_was_in_force() {
        // Found on a live server: a row the model judge had called `good` (with
        // the rules at `unknown`) became `unknown` when confirmed, because the
        // confirm branch stored the rules' label as the human decision. A person
        // confirms what they read, which is the label in force.
        let mut label = SessionLabel::from_outcome(
            "s1",
            identity(),
            outcome(TrajectoryLabel::Unknown),
            "hash-1",
            "triage-v1",
            100,
        );
        label.apply_judgement(
            TrajectoryLabel::Good,
            "模型理由".to_string(),
            vec![2],
            false,
            200,
        );
        assert_eq!(label.effective_label(), TrajectoryLabel::Good);

        label.apply_decision(LabelAction::Confirm, "alice", None, 300);
        assert_eq!(label.human_label, Some(TrajectoryLabel::Good));
        assert_eq!(label.effective_label(), TrajectoryLabel::Good);
    }

    fn identity() -> TrajectoryIdentity {
        TrajectoryIdentity {
            title: Some("看一下版本".to_string()),
            project: "-root-demo".to_string(),
            source: "qoder".to_string(),
            agent_name: "qoder".to_string(),
            started_at: None,
            is_subagent: false,
        }
    }

    #[test]
    fn an_unconfirmed_auto_label_is_still_in_force() {
        let label = row(TrajectoryLabel::Bad);
        assert_eq!(label.confirm_state, ConfirmState::Unconfirmed);
        assert_eq!(label.effective_label(), TrajectoryLabel::Bad);
    }

    #[test]
    fn unconfirmed_is_not_human_backed() {
        // The distinction the gate depends on: usable as a filter, not usable
        // as the human sign-off on the label itself.
        assert!(!row(TrajectoryLabel::Good).is_human_backed());
    }

    #[test]
    fn confirming_pins_the_label_and_counts_as_human_backed() {
        let mut label = row(TrajectoryLabel::Good);
        let kind = label.apply_decision(LabelAction::Confirm, "alice", None, 200);
        assert_eq!(kind, LabelEventKind::Confirm);
        assert_eq!(label.confirm_state, ConfirmState::Confirmed);
        assert_eq!(label.human_label, Some(TrajectoryLabel::Good));
        assert!(label.is_human_backed());
        assert!(!label.was_overridden());
    }

    #[test]
    fn overriding_wins_over_the_auto_label() {
        let mut label = row(TrajectoryLabel::Bad);
        let kind = label.apply_decision(
            LabelAction::Override(TrajectoryLabel::Good),
            "alice",
            Some("工具报错是预期的".to_string()),
            200,
        );
        assert_eq!(kind, LabelEventKind::Override);
        assert_eq!(label.effective_label(), TrajectoryLabel::Good);
        // The automatic verdict survives for the misfire statistics.
        assert_eq!(label.auto_label, TrajectoryLabel::Bad);
        assert_eq!(label.auto_rules, vec!["R1".to_string()]);
        assert!(label.was_overridden());
    }

    #[test]
    fn retriage_never_overwrites_a_human_decision() {
        let mut label = row(TrajectoryLabel::Bad);
        label.apply_decision(
            LabelAction::Override(TrajectoryLabel::Good),
            "alice",
            None,
            200,
        );
        label.apply_retriage(
            identity(),
            outcome(TrajectoryLabel::Useless),
            "hash-2",
            "triage-v2",
            300,
        );
        assert_eq!(label.effective_label(), TrajectoryLabel::Good);
        assert_eq!(label.human_label, Some(TrajectoryLabel::Good));
        assert_eq!(label.auto_label, TrajectoryLabel::Useless);
    }

    #[test]
    fn a_confirmed_label_survives_the_auto_verdict_changing() {
        // The "1+1 that later got a real follow-up" case: the confirmed value
        // still governs, and the user is prompted rather than overruled.
        let mut label = row(TrajectoryLabel::Useless);
        label.apply_decision(LabelAction::Confirm, "alice", None, 200);
        label.apply_retriage(
            identity(),
            outcome(TrajectoryLabel::Good),
            "hash-2",
            "triage-v1",
            300,
        );
        assert_eq!(label.effective_label(), TrajectoryLabel::Useless);
        assert!(label.auto_changed_since_decision);
    }

    #[test]
    fn retriage_agreeing_with_the_human_raises_no_flag() {
        let mut label = row(TrajectoryLabel::Bad);
        label.apply_decision(
            LabelAction::Override(TrajectoryLabel::Good),
            "alice",
            None,
            200,
        );
        label.apply_retriage(
            identity(),
            outcome(TrajectoryLabel::Good),
            "hash-2",
            "triage-v1",
            300,
        );
        assert!(!label.auto_changed_since_decision);
    }

    #[test]
    fn retriage_that_changes_nothing_raises_no_flag_on_an_overridden_row() {
        // Without the "did the auto verdict actually change" clause this would
        // be flagged on every recompute, since an overridden row disagrees with
        // its automatic verdict by construction.
        let mut label = row(TrajectoryLabel::Bad);
        label.apply_decision(
            LabelAction::Override(TrajectoryLabel::Good),
            "alice",
            None,
            200,
        );
        label.apply_retriage(
            identity(),
            outcome(TrajectoryLabel::Bad),
            "hash-2",
            "triage-v1",
            300,
        );
        assert!(!label.auto_changed_since_decision);
    }

    #[test]
    fn retriage_on_an_undecided_row_raises_no_flag() {
        let mut label = row(TrajectoryLabel::Unknown);
        label.apply_retriage(
            identity(),
            outcome(TrajectoryLabel::Bad),
            "hash-2",
            "triage-v1",
            300,
        );
        assert!(!label.auto_changed_since_decision);
        assert_eq!(label.effective_label(), TrajectoryLabel::Bad);
    }

    #[test]
    fn deciding_again_clears_a_pending_disagreement() {
        // Confirm endorses what is in force, so re-confirming after the rules
        // moved keeps the human verdict — the amber note already said the
        // decision stands and revisiting was optional. Wanting the rules' new
        // verdict instead is an override, not a confirm.
        let mut label = row(TrajectoryLabel::Useless);
        label.apply_decision(LabelAction::Confirm, "alice", None, 200);
        label.apply_retriage(
            identity(),
            outcome(TrajectoryLabel::Good),
            "hash-2",
            "triage-v1",
            300,
        );
        assert!(label.auto_changed_since_decision);
        label.apply_decision(LabelAction::Confirm, "alice", None, 400);
        assert!(!label.auto_changed_since_decision);
        assert_eq!(label.effective_label(), TrajectoryLabel::Useless);
    }

    #[test]
    fn only_useless_leaves_the_retrieval_scope() {
        assert!(TrajectoryLabel::Useless.excluded_from_retrieval());
        // Bad is the source of counterexamples, so it must stay searchable.
        assert!(!TrajectoryLabel::Bad.excluded_from_retrieval());
        assert!(!TrajectoryLabel::Good.excluded_from_retrieval());
        assert!(!TrajectoryLabel::Unknown.excluded_from_retrieval());
    }

    #[test]
    fn unknown_is_not_offered_to_users() {
        assert!(!TrajectoryLabel::user_assignable().contains(&TrajectoryLabel::Unknown));
    }

    #[test]
    fn label_and_state_tokens_round_trip() {
        for label in TrajectoryLabel::ALL {
            assert_eq!(TrajectoryLabel::parse(label.as_str()), Some(label));
        }
        for state in [
            ConfirmState::Unconfirmed,
            ConfirmState::Confirmed,
            ConfirmState::Overridden,
        ] {
            assert_eq!(ConfirmState::parse(state.as_str()), Some(state));
        }
        for kind in [
            LabelEventKind::Confirm,
            LabelEventKind::Override,
            LabelEventKind::AutoRetriage,
        ] {
            assert_eq!(LabelEventKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(TrajectoryLabel::parse("nonsense"), None);
        assert_eq!(ConfirmState::parse(""), None);
    }
}
