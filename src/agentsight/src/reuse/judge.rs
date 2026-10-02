//! Second-level labelling: asks a model whether a round delivered.
//!
//! Deliberately not built on `server::causal`. That pipeline is a diagnosis
//! tool — its prompt names the user's complaint as the 最高优先锚点, so it begins
//! from the premise that something went wrong and works out what. There is no
//! complaint here: nobody has objected to the trajectories being labelled, and
//! synthesising one would tell the model the user was unhappy when they never
//! said so.
//!
//! That distinction is not theoretical. The deterministic rules leaned the same
//! way and produced 7 `bad` labels on 24 real trajectories, every one of them
//! wrong. Paying a model to reach the same conclusion would be worse, not
//! better. So the question asked here is symmetric: did this round deliver what
//! was asked? `good` must be exactly as easy to answer as `bad`, and `unclear`
//! is available whenever the transcript does not settle it.
//!
//! The model may not overturn what the deterministic pass established. Call
//! outcomes come from [`crate::grounding`] and are given as settled facts — the
//! same discipline `server::causal` applies, for the same reason: a model asked
//! to re-adjudicate a non-zero exit code will sometimes excuse it.

use agentsight_atif::{AtifTrajectory, Step, StepSource};
use agentsight_opt::llm::{ChatMessage, LlmClient};
use serde::{Deserialize, Serialize};

use super::label::TrajectoryLabel;
use crate::grounding::evidence::build_index;
use crate::grounding::outcome::CallStatus;

/// Longest excerpt taken from any single message.
///
/// Whole trajectories reach megabytes, while the question is only whether the
/// round delivered.
const EXCERPT_CHARS: usize = 600;

/// Steps included per round. A longer round is elided in the middle rather than
/// cut at the end, so the outcome is never the part that goes missing.
const MAX_STEPS_PER_ROUND: usize = 40;

/// What the model is asked to return.
///
/// Three fields. Every additional one is something more to hallucinate, and none
/// of the candidates would have been checkable.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct JudgeResponse {
    /// `good`, `bad` or `unclear`.
    pub label: String,
    /// One sentence saying why.
    pub reason: String,
    /// Steps the judgement rests on. Required for `bad`; see [`interpret`].
    #[serde(default)]
    pub cited_steps: Vec<usize>,
}

/// A judgement after the citation rule has been applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JudgeVerdict {
    pub label: TrajectoryLabel,
    pub reason: String,
    pub cited_steps: Vec<usize>,
    /// Set when a `bad` arrived with no cited step and was reduced to `unknown`.
    /// Reported rather than applied quietly, so the rate stays visible.
    pub downgraded: bool,
}

/// Why a judgement could not be produced.
#[derive(Debug, thiserror::Error)]
pub enum JudgeError {
    #[error("no LLM configured; second-level labelling needs an API key")]
    NotConfigured,
    #[error("trajectory has no steps to judge")]
    NothingToJudge,
    #[error("model call failed: {0}")]
    Call(String),
    #[error("model returned an unusable label {0:?}")]
    UnusableLabel(String),
}

/// Instruction given to the model.
///
/// Written so that reporting a clean round is as easy as reporting a broken one.
/// The deterministic attempt failed by treating "could not confirm" as "wrong",
/// and a prompt that only explains how to find fault invites the same mistake.
const SYSTEM_PROMPT: &str = "\
你在为一个轨迹库标注可复用性。请判断这一轮对话是否交付了用户要求的东西。

三个选项，地位相同：
- good：本轮达成了用户的要求，过程可供他人参考。
- bad：本轮没有达成，或者交付了错误的结果。
- unclear：记录不足以判断（例如用户意图不明、轮次被截断、无法确认结果是否正确）。

请注意：
1. 大多数轨迹是正常的。如果本轮正常完成，就回答 good；不要为了找出问题而找问题。
2. 「确定性检查结论」是程序核实过的事实，不得推翻。工具失败不等于本轮失败——Agent 完全可以在一次失败后换个方式做成。
3. 判 bad 时必须在 cited_steps 里给出支撑该结论的步骤号。举不出步骤就说明结论没有依据，此时请回答 unclear。
4. Agent 转述、改写、总结观察结果是正常表达，不是编造。只有当它给出的具体事实与工具实际返回的内容矛盾、或来源根本不存在时，才算错误交付。

只返回 JSON：{\"label\": \"good|bad|unclear\", \"reason\": \"一句话理由\", \"cited_steps\": [步骤号]}";

/// Builds the messages for one round.
///
/// Separated from the call so the prompt can be checked in tests. Since a
/// request needs an API key, this is the only place its wording is verified.
pub fn build_messages(doc: &AtifTrajectory, round: std::ops::Range<usize>) -> Vec<ChatMessage> {
    let index = build_index(doc, round.clone());
    let steps = &doc.steps[round];

    let mut facts = String::new();
    for verdict in index
        .call_verdicts
        .iter()
        .filter(|v| v.step_id >= index.round_start_step)
    {
        let status = match verdict.verdict.status {
            CallStatus::Ok => "成功",
            CallStatus::OkProbe => "成功（属预期探测）",
            CallStatus::Failed => "失败",
            CallStatus::Blocked => "被用户或策略拦截",
            CallStatus::Unknown => "无法确认结果",
        };
        // The deciding rule travels with the verdict so a wrong call can be traced
        // back to the rule that produced it.
        facts.push_str(&format!(
            "- step{} {} {}（规则 {}）\n",
            verdict.step_id, verdict.function_name, status, verdict.verdict.matched_rule
        ));
    }
    if facts.is_empty() {
        // An empty block would read as "no information", inviting the model to
        // assume calls happened and failed unrecorded.
        facts.push_str("（本轮没有工具调用）\n");
    }

    let transcript = render_window(steps).join("\n");
    vec![
        ChatMessage::system(SYSTEM_PROMPT),
        ChatMessage::user(format!(
            "确定性检查结论（已核实，不得推翻）：\n{facts}\n对话记录：\n{transcript}"
        )),
    ]
}

/// Renders the round, keeping both ends when it is long.
///
/// The opening carries the request and the closing carries the outcome; dropping
/// either is what would make the question unanswerable.
fn render_window(steps: &[Step]) -> Vec<String> {
    let render = |step: &Step| {
        let who = match step.source {
            StepSource::User => "用户",
            StepSource::Agent => "Agent",
            _ => "系统",
        };
        let mut line = format!("step{} {}: {}", step.step_id, who, excerpt(&step.message));
        let tools: Vec<&str> = step
            .tool_calls
            .iter()
            .flatten()
            .map(|call| call.function_name.as_str())
            .collect();
        if !tools.is_empty() {
            line.push_str(&format!(" [调用: {}]", tools.join(", ")));
        }
        line
    };

    if steps.len() <= MAX_STEPS_PER_ROUND {
        return steps.iter().map(render).collect();
    }
    let half = MAX_STEPS_PER_ROUND / 2;
    let mut out: Vec<String> = steps[..half].iter().map(render).collect();
    out.push(format!(
        "…（省略 {} 步）",
        steps.len() - MAX_STEPS_PER_ROUND
    ));
    out.extend(steps[steps.len() - half..].iter().map(render));
    out
}

/// Trims a message to [`EXCERPT_CHARS`], counting characters rather than bytes so
/// CJK text is not cut to a third of the intended length.
fn excerpt(message: &str) -> String {
    let cleaned: String = message
        .chars()
        .map(|c| if c == '\n' { ' ' } else { c })
        .collect();
    if cleaned.chars().count() <= EXCERPT_CHARS {
        return cleaned;
    }
    cleaned.chars().take(EXCERPT_CHARS).collect::<String>() + "…"
}

/// Applies the citation rule to a raw model response.
///
/// A `bad` with no cited step becomes `unknown`. This is the requirement the
/// deterministic layer failed to meet: an accusation that cannot point at a step
/// is an impression, and impressions are what filled the label store with false
/// `bad` verdicts the first time.
///
/// # Errors
/// Returns [`JudgeError::UnusableLabel`] for anything outside the three permitted
/// labels — including `useless`, which is a structural property the deterministic
/// rules read off the step counts and not the model's to assign.
pub fn interpret(response: &JudgeResponse) -> Result<JudgeVerdict, JudgeError> {
    let cited = response.cited_steps.clone();
    match response.label.trim() {
        "good" => Ok(JudgeVerdict {
            label: TrajectoryLabel::Good,
            reason: response.reason.clone(),
            cited_steps: cited,
            downgraded: false,
        }),
        "bad" if cited.is_empty() => Ok(JudgeVerdict {
            label: TrajectoryLabel::Unknown,
            reason: format!(
                "模型判为 bad 但未给出支撑步骤，按证据不足处理：{}",
                response.reason
            ),
            cited_steps: Vec::new(),
            downgraded: true,
        }),
        "bad" => Ok(JudgeVerdict {
            label: TrajectoryLabel::Bad,
            reason: response.reason.clone(),
            cited_steps: cited,
            downgraded: false,
        }),
        "unclear" => Ok(JudgeVerdict {
            label: TrajectoryLabel::Unknown,
            reason: response.reason.clone(),
            cited_steps: cited,
            downgraded: false,
        }),
        other => Err(JudgeError::UnusableLabel(other.to_string())),
    }
}

/// Judges the final round of a trajectory.
///
/// The final round carries the outcome; judging every round would multiply the
/// cost by the round count to produce one label.
///
/// # Errors
/// Returns [`JudgeError::NothingToJudge`] for a trajectory with no steps,
/// [`JudgeError::Call`] when the request fails, and
/// [`JudgeError::UnusableLabel`] for an unrecognised label.
pub async fn judge_last_round(
    client: &LlmClient,
    doc: &AtifTrajectory,
) -> Result<JudgeVerdict, JudgeError> {
    let round = last_round(doc).ok_or(JudgeError::NothingToJudge)?;
    let response: JudgeResponse = client
        .chat_json_parsed(build_messages(doc, round))
        .await
        .map_err(|error| JudgeError::Call(error.to_string()))?;
    interpret(&response)
}

/// Range of the final round.
///
/// Falls back to the whole trajectory when no user step exists at all, which is
/// the shape real Qoder transcripts take: they record only the tool loop.
fn last_round(doc: &AtifTrajectory) -> Option<std::ops::Range<usize>> {
    if doc.steps.is_empty() {
        return None;
    }
    let start = doc
        .steps
        .iter()
        .rposition(|step| step.source == StepSource::User)
        .unwrap_or(0);
    Some(start..doc.steps.len())
}

#[cfg(test)]
#[path = "judge/tests.rs"]
mod tests;
