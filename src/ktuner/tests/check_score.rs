use std::process::{Command, Output};

fn ktuner(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ktuner"))
        .args(arguments)
        .output()
        .expect("run read-only ktuner command")
}

fn check(arguments: &[&str]) -> serde_json::Value {
    let mut args = vec!["check"];
    args.extend_from_slice(arguments);
    let output = ktuner(&args);
    assert!(
        matches!(output.status.code(), Some(0 | 1)),
        "ktuner {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("check must print JSON on stdout")
}

/// Penalty `EvalResult::score()` charges for one finding: high-confidence advice
/// costs 3, workload-dependent advice 2.
fn weight(recommendation: &serde_json::Value) -> usize {
    match recommendation["confidence"].as_str() {
        Some("high") => 3,
        Some("medium") => 2,
        other => panic!("unexpected confidence {other:?} in {recommendation}"),
    }
}

#[test]
fn predicted_score_is_the_score_after_applying_the_shown_recommendations() {
    // `score` is `100 - penalty` floored at 30 (rules::EvalResult::score), and
    // the user guide says it "predicts the score after tuning". Deriving the
    // prediction as `score + shown weight` counted the floor as a gain: once the
    // penalty exceeds 70 the score is pinned at 30, so adding the shown weight
    // promises points the remaining findings still keep off the board (on a
    // 66-finding host, `check --conservative` promised 72 while applying those
    // recommendations still lands on the floor, 30).
    let full = check(&[]);
    let all = full["recommendations"]
        .as_array()
        .expect("check must print a recommendations array");
    if all.is_empty() {
        eprintln!("skip: this host has no recommendations");
        return;
    }

    for view in [
        &["--conservative"][..],
        &["--category", "net"][..],
        &["--category", "security"][..],
        &["--category", "cpu"][..],
    ] {
        let filtered = check(view);
        let shown = filtered["recommendations"]
            .as_array()
            .expect("check must print a recommendations array");
        if shown.is_empty() {
            continue;
        }
        let shown_params: Vec<&str> = shown.iter().filter_map(|r| r["param"].as_str()).collect();
        let hidden_penalty: usize = all
            .iter()
            .filter(|r| {
                !r["param"]
                    .as_str()
                    .is_some_and(|p| shown_params.contains(&p))
            })
            .map(weight)
            .sum();
        // The same arithmetic `score` uses, with the shown recommendations
        // applied: only the hidden findings still cost points.
        let expected = 100usize.saturating_sub(hidden_penalty).max(30);
        let predicted = filtered["predicted_score"]
            .as_u64()
            .expect("check must print predicted_score") as usize;
        assert!(
            predicted <= expected,
            "check {view:?} predicted {predicted} but applying the shown recommendations \
             leaves {hidden_penalty} points of penalty, so the score after tuning is {expected} \
             (score {} is floored at 30): {filtered}",
            filtered["score"],
        );
    }
}
