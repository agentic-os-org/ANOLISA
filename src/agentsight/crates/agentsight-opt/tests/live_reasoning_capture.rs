//! Live smoke test: the recorded trajectory must explain its own token count.
//!
//! Reasoning models bill their hidden chain-of-thought inside
//! `completion_tokens`, so a 200-token answer can cost 12k output tokens. That
//! only stays diagnosable if the provider's `reasoning_content` and
//! `reasoning_tokens` actually reach the ATIF document — a silent change in the
//! provider dialect or in rig's usage mapping would drop them without any
//! compile or unit-test failure.
//!
//! Requires a reasoning-capable model and network access, so it is `#[ignore]`d:
//!
//! ```sh
//! OPENAI_API_KEY=… OPENAI_BASE_URL=… OPENAI_MODEL=glm-5.2 \
//!   cargo test -p agentsight-opt --test live_reasoning_capture -- --ignored --nocapture
//! ```

use std::sync::Arc;

use agentsight_opt::llm::ChatMessage;
use agentsight_opt::{LlmClient, TrajectoryRecorder};

#[tokio::test]
#[ignore = "requires a live reasoning-capable LLM (OPENAI_API_KEY)"]
async fn recorded_step_accounts_for_its_reasoning_tokens() {
    let recorder = Arc::new(TrajectoryRecorder::new(
        std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "glm-5.2".into()),
        "live/reasoning-capture",
    ));
    let mut client =
        LlmClient::from_env().expect("set OPENAI_API_KEY (and OPENAI_BASE_URL / OPENAI_MODEL)");
    client.set_recorder(Arc::clone(&recorder));

    // A question that forces some deliberation but a one-token answer, so the
    // reasoning share of `completion_tokens` is unmistakable.
    let answer = client
        .chat_labeled(
            vec![ChatMessage::user(
                "一个数的 3 倍加 7 等于 25，这个数是多少？只回数字。",
            )],
            "live:arithmetic",
        )
        .await
        .expect("live LLM call failed");
    assert!(answer.contains('6'), "unexpected answer: {answer:?}");

    let doc = recorder.to_atif();
    let sub = &doc
        .subagent_trajectories
        .as_ref()
        .expect("labeled call should form a subagent trajectory")[0];
    let step = sub
        .steps
        .iter()
        .find(|s| s.metrics.is_some())
        .expect("agent step with metrics");
    let metrics = step.metrics.as_ref().expect("metrics");

    let completion = metrics.completion_tokens.expect("completion_tokens");
    let reasoning_tokens = metrics
        .extra
        .as_ref()
        .and_then(|e| e.get("reasoning_tokens"))
        .and_then(serde_json::Value::as_u64)
        .expect("reasoning_tokens recorded in metrics.extra");

    eprintln!(
        "answer {:?} ({} chars) | completion {completion} tok of which {reasoning_tokens} reasoning | reasoning text {} chars",
        step.message,
        step.message.chars().count(),
        step.reasoning_content.as_deref().unwrap_or("").chars().count(),
    );

    // The gap between a short answer and a large `completion_tokens` has to be
    // attributable, and the reasoning text itself has to be retained.
    assert!(
        reasoning_tokens > 0 && reasoning_tokens <= completion,
        "reasoning {reasoning_tokens} vs completion {completion}"
    );
    assert!(
        !step.reasoning_content.as_deref().unwrap_or("").is_empty(),
        "reasoning_content was dropped"
    );
    assert!(
        metrics.cached_tokens.is_some(),
        "cached_tokens not recorded"
    );
}
