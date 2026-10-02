//! LLM cost-waste identification: Rust extracts structured candidates and
//! computes ratio metrics (agent-first: no admission pre-filtering — the LLM
//! checks admission criteria in-prompt), then fires one LLM call per
//! candidate/strategy in parallel (mirroring the perf per-strategy
//! architecture). Each call judges whether one candidate is worth optimizing;
//! verdicts are joined back and pass deterministic arbitration (cache-priority
//! net-benefit discount, noise line) to produce the final waste rows.

use anyhow::Result;

use crate::atif::AtifTrajectory;
use crate::cost::prompts::cost_identification::{
    build_strategy_prompt, strategy_for, CostStrategyDef, STRATEGIES,
};
use crate::cost::prompts::detour::{build_detour_prompt, is_detour};
use crate::cost::{ledger_tokens_for, CACHED_PRICE_RATIO, MIN_DETOUR_TURNS, NOISE_LINE};
use crate::llm::{ChatMessage, LlmClient};
use crate::types::{
    DetourVerdict, TurnLedgerRow, WasteCandidate, WasteExperience, WasteItem, WasteReport,
    WasteVerdict,
};

/// Max findings kept from one detour verdict (mirrors the prompt's cap).
const MAX_DETOUR_FINDINGS: usize = 5;

/// Either verdict shape, depending on which prompt the candidate was judged by.
enum RawVerdict {
    Payload(WasteVerdict),
    Detour(DetourVerdict),
}

/// Run full cost-waste identification: Rust candidate extraction → parallel per-candidate LLM eval → merge.
/// Parses events only once and reuses them for both cost computation and candidate extraction.
pub async fn identify_waste(
    client: &LlmClient,
    trajectory: &AtifTrajectory,
) -> Result<WasteReport> {
    let cost = crate::cost::compute_cost(trajectory)?;
    let candidates = crate::cost::extract_waste_candidates_from(&cost, trajectory)?;
    if candidates.candidates.is_empty() {
        tracing::info!("Cost: no waste candidates extracted, skipping LLM judgment");
        return Ok(WasteReport {
            items: vec![],
            considered: 0,
            dismissed: 0,
            model: candidates.model,
        });
    }

    let considered = candidates.candidates.len();
    tracing::info!(
        "Cost: {} waste candidates — evaluating {} strategies in parallel...",
        considered,
        considered
    );

    // Fire one LLM call per candidate in parallel. Candidates without a
    // strategy definition are skipped (Rust extraction and STRATEGIES share
    // the same stable id catalog, so this only guards future drift).
    let futures: Vec<_> = candidates
        .candidates
        .iter()
        .filter_map(|cand| {
            let Some(strategy) = strategy_for(&cand.id) else {
                tracing::warn!(
                    "Cost: candidate '{}' has no strategy definition, skipped",
                    cand.id
                );
                return None;
            };
            let detour = is_detour(&cand.id);
            let messages = if detour {
                build_detour_prompt(&candidates, cand, strategy, trajectory)
            } else {
                build_strategy_prompt(&candidates, cand, strategy)
            };
            let label = format!("cost:{}", strategy.id);
            tracing::info!("Cost: strategy '{}' — sending to LLM...", strategy.id);
            let ledger = &candidates.ledger;
            Some(async move {
                let result = if detour {
                    judge_detour(client, messages, &label, ledger)
                        .await
                        .map(RawVerdict::Detour)
                } else {
                    client
                        .chat_json_parsed_labeled::<WasteVerdict>(messages, Some(&label))
                        .await
                        .map(RawVerdict::Payload)
                };
                (cand, strategy, result)
            })
        })
        .collect();

    let results = futures::future::join_all(futures).await;

    let total_billed = (candidates.total_input_tokens + candidates.total_output_tokens).max(1);
    let cache_hit = candidates.metrics.m3_cache_hit_rate;

    let mut items: Vec<WasteItem> = Vec::new();
    let mut dismissed = 0usize;

    for (cand, strategy, result) in results {
        let v = match result {
            Ok(RawVerdict::Detour(dv)) => {
                let rows = expand_detour_items(cand, strategy, &dv, &candidates.ledger);
                if rows.is_empty() {
                    tracing::info!("Cost: strategy '{}' ✗ 未识别出可报告的弯路", cand.id);
                    dismissed += 1;
                } else {
                    tracing::info!("Cost: strategy '{}' ✓ {} 条发现", cand.id, rows.len());
                    items.extend(rows);
                }
                continue;
            }
            Ok(RawVerdict::Payload(v)) if v.worth_optimizing => {
                tracing::info!(
                    "Cost: strategy '{}' ✓ worth optimizing (save_ratio {:.2})",
                    cand.id,
                    v.save_ratio
                );
                v
            }
            Ok(RawVerdict::Payload(_)) => {
                tracing::info!("Cost: strategy '{}' ✗ not worth optimizing", cand.id);
                dismissed += 1;
                continue;
            }
            Err(err) => {
                tracing::warn!("Cost: strategy '{}' LLM call failed: {}", cand.id, err);
                dismissed += 1;
                continue;
            }
        };

        // save_ratio defaults to 1.0 when the LLM marked it worth but omitted a ratio.
        let ratio = if v.save_ratio > 0.0 {
            v.save_ratio.clamp(0.0, 1.0)
        } else {
            1.0
        };
        let mut save_tokens = ((cand.potential_save_tokens as f64) * ratio).round() as usize;
        let mut evidence = if v.evidence.is_empty() {
            cand.facts.clone()
        } else {
            v.evidence.clone()
        };

        // 仲裁——缓存优先原则 (playbook 规则 1): when the cache hit rate is high,
        // history-editing strategies invalidate the KV cache from the edit point
        // on, so their net benefit is the full-price share of the saved tokens.
        // Below the noise line after discounting → dismissed deterministically.
        // (主手段去重——playbook 规则 2——needs no code: candidate sources are
        // disjoint by construction: assistant history vs tool results vs inputs.)
        if let Some(m3) = cache_hit {
            if m3 > 0.5 && matches!(cand.id.as_str(), "history" | "tool_output") {
                let factor = 1.0 - m3 * (1.0 - CACHED_PRICE_RATIO);
                let adjusted = (save_tokens as f64 * factor).round() as usize;
                tracing::info!(
                    "Cost: 缓存优先仲裁 '{}' — M3={:.0}%，净收益 {} → {} tok",
                    cand.id,
                    m3 * 100.0,
                    save_tokens,
                    adjusted
                );
                if (adjusted as f64 / total_billed as f64) < NOISE_LINE {
                    tracing::info!("Cost: strategy '{}' ✗ 缓存折算后低于噪声线，不报", cand.id);
                    dismissed += 1;
                    continue;
                }
                save_tokens = adjusted;
                evidence = format!(
                    "{}（已按 M3={:.0}% 缓存命中折算净收益）",
                    evidence,
                    m3 * 100.0
                );
            }
        }

        items.push(WasteItem {
            symptom: if v.symptom.is_empty() {
                cand.facts.clone()
            } else {
                v.symptom.clone()
            },
            category: cand.category.clone(),
            subtype: cand.subtype.clone(),
            optimization: cand.optimization.clone(),
            evidence,
            save_tokens,
            discount: cand.discount,
            savings_kind: cand.savings_kind.clone(),
            confidence: if v.confidence.is_empty() {
                "中".to_string()
            } else {
                v.confidence.clone()
            },
            needs_confirm: strategy.needs_confirm,
            steps: Vec::new(),
            experience: None,
        });
    }

    // Biggest savings first (by tokens).
    items.sort_by_key(|item| std::cmp::Reverse(item.save_tokens));

    tracing::info!(
        "Cost: {}/{} candidates worth optimizing ({} strategies defined)",
        items.len(),
        considered,
        STRATEGIES.len()
    );

    Ok(WasteReport {
        items,
        considered,
        dismissed,
        model: candidates.model.clone(),
    })
}

/// Judge the detour candidate, retrying once with corrective feedback when the
/// model points every finding at turns that do not exist in the ledger.
/// Structurally invalid output is usually recoverable (the model misread the
/// numbering scheme), so one guided retry beats silently dropping the verdict.
async fn judge_detour(
    client: &LlmClient,
    messages: Vec<ChatMessage>,
    label: &str,
    ledger: &[TurnLedgerRow],
) -> Result<DetourVerdict> {
    let verdict = client
        .chat_json_parsed_labeled::<DetourVerdict>(messages.clone(), Some(label))
        .await?;
    let all_invalid = verdict.detected
        && !verdict.findings.is_empty()
        && verdict
            .findings
            .iter()
            .all(|f| ledger_tokens_for(ledger, &f.turns).1.is_empty());
    if !all_invalid {
        return Ok(verdict);
    }
    tracing::warn!("Cost: detour verdict cites only nonexistent turns, retrying with feedback");
    let mut retry = messages;
    retry.push(ChatMessage::user(
        "⚠️ 你上一次的输出无效：findings 引用的轮号全部不在账本内。\
         `turns` 只能取账本每行开头的 T{n} 数字（例如账本行 `T7 | …` 对应 7）。\
         请重新逐轮扫账本后输出 JSON。",
    ));
    client
        .chat_json_parsed_labeled::<DetourVerdict>(retry, Some(&format!("{label}-retry")))
        .await
}

/// Turn one detour verdict into waste rows —— one row per finding.
///
/// Semantic judgment stays with the model (which segments are detours and
/// why); Rust enforces only mechanical invariants:
/// 1. **Savings come from the ledger, never from the model.** The verdict only
///    points at turn ordinals; tokens are summed over the turns that exist.
/// 2. **Materiality gate**: findings with fewer than `MIN_DETOUR_TURNS` valid
///    turns are dropped — the prompt states the same threshold, this is the
///    deterministic backstop.
/// 3. **归因-产出一致性**: 偶发故障 must not carry a fix（随机故障没有可提炼
///    的因果）——a fabricated one is stripped rather than trusted.
/// 4. **Confidence is pinned to 低** for single-trajectory evidence. Raising it
///    needs cross-trajectory recurrence, which has no store yet (see the
///    experience-library TODO).
fn expand_detour_items(
    cand: &WasteCandidate,
    strategy: &CostStrategyDef,
    verdict: &DetourVerdict,
    ledger: &[TurnLedgerRow],
) -> Vec<WasteItem> {
    if !verdict.detected {
        return Vec::new();
    }

    let mut rows = Vec::new();
    for f in verdict.findings.iter().take(MAX_DETOUR_FINDINGS) {
        let (save_tokens, valid_steps) = ledger_tokens_for(ledger, &f.turns);
        if valid_steps.len() < MIN_DETOUR_TURNS {
            tracing::info!(
                "Cost: 弯路 finding 低于重要性门槛（{} 有效轮 < {}），不报：{}",
                valid_steps.len(),
                MIN_DETOUR_TURNS,
                f.what
            );
            continue;
        }
        // 偶发故障 carries no distillable causality — enforce fix = None even
        // when the model fabricated one.
        let fix = if f.why == "偶发故障" {
            if f.fix.is_some() {
                tracing::warn!("Cost: 偶发故障 finding 带了 fix，已剥除：{}", f.what);
            }
            &None
        } else {
            &f.fix
        };
        let experience = fix.as_ref().map(|fx| WasteExperience {
            applicability: fx.applicability.clone(),
            pitfall: fx.pitfall.clone(),
            effective_path: fx.effective_path.clone(),
            root_cause: f.why.clone(),
            fix_locus: fx.locus.clone(),
            ..Default::default()
        });
        // Turn ordinals stay in `steps` (machine-readable); the evidence text
        // only carries the count — an exhaustive T40,T41,… enumeration is
        // reader noise, and the model's prose already cites its anchor turns.
        let turns = format!("共 {} 轮", valid_steps.len());
        rows.push(WasteItem {
            symptom: if f.what.is_empty() {
                cand.facts.clone()
            } else {
                f.what.clone()
            },
            category: cand.category.clone(),
            subtype: if f.why.is_empty() {
                cand.subtype.clone()
            } else {
                f.why.clone()
            },
            optimization: fix
                .as_ref()
                .filter(|fx| !fx.action.is_empty())
                .map(|fx| fx.action.clone())
                .unwrap_or_else(|| cand.optimization.clone()),
            evidence: if f.why_detail.is_empty() {
                turns
            } else {
                format!("{}（{turns}）", f.why_detail)
            },
            save_tokens,
            discount: false,
            savings_kind: "预防".to_string(),
            confidence: "低".to_string(),
            needs_confirm: strategy.needs_confirm,
            steps: valid_steps,
            experience,
        });
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cost::prompts::cost_identification::strategy_for;
    use crate::types::{DetourFinding, DetourFix};

    fn ledger() -> Vec<TurnLedgerRow> {
        (0..8)
            .map(|turn| TurnLedgerRow {
                turn,
                tokens: 100 * (turn + 1),
                ..Default::default()
            })
            .collect()
    }

    fn candidate() -> WasteCandidate {
        WasteCandidate {
            id: "detour".into(),
            category: "减轮次浪费".into(),
            subtype: "弯路".into(),
            optimization: "归因并沉淀修复方案".into(),
            potential_save_tokens: 1000,
            discount: false,
            save_share: 0.3,
            savings_kind: "预防".into(),
            steps: vec![],
            facts: "结构信号".into(),
            snippet: String::new(),
        }
    }

    fn finding(turns: Vec<usize>) -> DetourFinding {
        DetourFinding {
            turns,
            what: "在错误假设上完成构建部署".into(),
            why: "方向选错".into(),
            why_detail: "T1 未验证符号即宣布根因，T6 被实测推翻".into(),
            fix: Some(DetourFix {
                action: "假设验证前置".into(),
                locus: "Skill".into(),
                applicability: "形成根因假设准备动手时".into(),
                pitfall: "凭代码阅读推断即投入实现".into(),
                effective_path: "先用 nm/perf 等廉价手段实测验证".into(),
            }),
        }
    }

    fn expand(verdict: DetourVerdict) -> Vec<WasteItem> {
        expand_detour_items(
            &candidate(),
            strategy_for("detour").expect("strategy exists"),
            &verdict,
            &ledger(),
        )
    }

    /// Savings are summed from the ledger over the turns the model pointed at —
    /// never taken from the model, which is not asked for a token estimate.
    #[test]
    fn sums_savings_from_ledger_not_from_model() {
        let rows = expand(DetourVerdict {
            detected: true,
            findings: vec![finding(vec![0, 1, 2, 3, 4])],
        });
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].save_tokens, 100 + 200 + 300 + 400 + 500);
        assert_eq!(rows[0].savings_kind, "预防");
        assert_eq!(rows[0].confidence, "低"); // pinned: single-trajectory evidence
        assert!(rows[0].needs_confirm);
        assert_eq!(rows[0].steps, vec![0, 1, 2, 3, 4]);
        // 归因 → subtype, fix → optimization/experience.
        assert_eq!(rows[0].subtype, "方向选错");
        assert_eq!(rows[0].optimization, "假设验证前置");
        assert!(rows[0].evidence.contains("共 5 轮"));
        let exp = rows[0].experience.as_ref().expect("fix maps to experience");
        assert_eq!(exp.fix_locus, "Skill");
        assert_eq!(exp.root_cause, "方向选错");
        assert_eq!(exp.effective_path, "先用 nm/perf 等廉价手段实测验证");
    }

    /// The materiality gate: segments shorter than MIN_DETOUR_TURNS valid
    /// turns are dropped — micro-pits are not worth a report row, and turns
    /// pointing nowhere in the ledger do not count toward the gate.
    #[test]
    fn short_or_unpriceable_findings_are_dropped() {
        // 2 valid turns < gate.
        let rows = expand(DetourVerdict {
            detected: true,
            findings: vec![finding(vec![1, 2])],
        });
        assert!(rows.is_empty(), "2-turn micro-pit must be gated out");

        // 5 turns cited but none exists in the ledger.
        let rows = expand(DetourVerdict {
            detected: true,
            findings: vec![finding(vec![41, 42, 43, 44, 45])],
        });
        assert!(rows.is_empty(), "nonexistent turns must not pass the gate");
    }

    /// 偶发故障 has no distillable causality: a fabricated fix is stripped and
    /// the row falls back to the candidate's generic optimization label.
    #[test]
    fn transient_fault_fix_is_stripped() {
        let mut transient = finding(vec![0, 1, 2, 3, 4]);
        transient.why = "偶发故障".into();
        let rows = expand(DetourVerdict {
            detected: true,
            findings: vec![transient],
        });
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].subtype, "偶发故障");
        assert!(rows[0].experience.is_none(), "fix must be stripped");
        assert_eq!(rows[0].optimization, "归因并沉淀修复方案");
    }

    /// One call may report several distinct detours; each becomes its own row,
    /// capped so a runaway verdict cannot flood the table.
    #[test]
    fn expands_each_finding_into_its_own_row_up_to_cap() {
        let rows = expand(DetourVerdict {
            detected: true,
            findings: (0..8).map(|_| finding(vec![0, 1, 2, 3, 4])).collect(),
        });
        assert_eq!(rows.len(), MAX_DETOUR_FINDINGS);
    }

    #[test]
    fn reports_nothing_when_not_detected() {
        let rows = expand(DetourVerdict {
            detected: false,
            findings: vec![finding(vec![0, 1, 2, 3, 4])],
        });
        assert!(rows.is_empty());
    }
}
