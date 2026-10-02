use crate::atif::AtifTrajectory;
use crate::llm::ChatMessage;
use crate::types::PerfCandidateSet;

const SYSTEM_PROMPT: &str = include_str!("../../../prompts/perf_identification.md");

/// A strategy definition for per-strategy parallel evaluation.
pub struct StrategyDef {
    pub id: &'static str,
    pub name: &'static str,
    pub applies_signal: &'static str,
    pub method: &'static str,
}

/// All strategies to evaluate in parallel (3 core strategies).
pub const STRATEGIES: &[StrategyDef] = &[
    StrategyDef {
        id: "prefix_cache",
        name: "前缀缓存优化",
        applies_signal: "cache 命中率低（<60%）或逐轮下降，说明 prompt 前缀不稳定（动态变量插入、工具定义顺序变化、上下文结构不一致）",
        method: "保持 system prompt 和前缀消息稳定（不插入动态变量）；将动态内容移到消息尾部；工具定义放在 system 最前面且顺序固定。原理：LLM API prompt caching 按前缀匹配 KV cache，命中后推理延迟降低 50-80%",
    },
    StrategyDef {
        id: "fast_tool",
        name: "快速工具替代",
        applies_signal: "工具调用慢且频次多；出现 grep、find、cat 等 Unix 工具；单次搜索/查找耗时 >2s",
        method: "用 ripgrep (rg) 替代 grep（快 5-10x）；用 fd 替代 find（快 3-5x）；避免 cat 大文件，用 head/tail 或 rg 定向提取",
    },
    // TODO(#opt-experience-dedup): overlaps the cost dimension's detour
    // candidate (`cost/prompts/detour.rs`), which judges the same pitfalls
    // against a full turn ledger and emits the reusable lesson. Until this is
    // converged, one pitfall can surface on both the perf and the cost page.
    // Same overlap applies to `accuracy::strategies::experience_library`.
    StrategyDef {
        id: "experience_library",
        name: "经验库沉淀",
        applies_signal: "存在低效轮次（对最终结果无贡献的探索、踩坑、方向错误后回退）；失败重试模式明显",
        method: "识别低效轮次的具体踩坑点，形成结构化经验条目（场景 + 错误做法 + 正确做法）；注入 Agent 的 system prompt 或 skill 文件，避免同类错误重复发生",
    },
];

/// Build the per-strategy evaluation prompt with strategy-specific data slicing.
pub fn build_strategy_prompt(
    candidates: &PerfCandidateSet,
    strategy: &StrategyDef,
    trajectory: &AtifTrajectory,
) -> Vec<ChatMessage> {
    let data_section = build_data_section(candidates, strategy.id, trajectory);
    vec![
        ChatMessage::system(SYSTEM_PROMPT),
        ChatMessage::user(format!(
            "## 待评估策略\n\n\
             **{id} — {name}**\n\n\
             - 适用信号：{applies}\n\
             - 优化手段：{method}\n\n\
             {data}\n\n\
             请判断该策略是否适用。仅返回 JSON。",
            id = strategy.id,
            name = strategy.name,
            applies = strategy.applies_signal,
            method = strategy.method,
            data = data_section,
        )),
    ]
}

/// Build strategy-specific data section — only include relevant signals.
fn build_data_section(
    candidates: &PerfCandidateSet,
    strategy_id: &str,
    trajectory: &AtifTrajectory,
) -> String {
    match strategy_id {
        // prefix_cache: only cache hit rate matters
        "prefix_cache" => {
            let total_prompt: u64 = candidates.cache_turns.iter().map(|c| c.prompt_tokens).sum();
            let total_cached: u64 = candidates.cache_turns.iter().map(|c| c.cached_tokens).sum();
            let hit_rate = if total_prompt > 0 {
                (total_cached as f64 / total_prompt as f64) * 100.0
            } else {
                0.0
            };
            let per_turn: Vec<String> = candidates
                .cache_turns
                .iter()
                .take(15)
                .map(|c| {
                    let r = if c.prompt_tokens > 0 {
                        (c.cached_tokens as f64 / c.prompt_tokens as f64) * 100.0
                    } else {
                        0.0
                    };
                    format!("{:.0}%", r)
                })
                .collect();
            format!(
                "## 性能数据（程序计算）\n\n\
                 - Cache 命中率：总体 {hit_rate:.0}%（cached {cached} / prompt {prompt} tokens）\n\
                 - 每轮命中率：[{turns}]",
                hit_rate = hit_rate,
                cached = total_cached,
                prompt = total_prompt,
                turns = per_turn.join(", "),
            )
        }
        // fast_tool: needs tool call details + per-tool aggregation
        "fast_tool" => {
            let tools_json = serde_json::to_string_pretty(&candidates.top_tools)
                .unwrap_or_else(|_| "[]".to_string());
            // Tool aggregation: name × count × avg duration.
            let agg_lines: Vec<String> = candidates
                .tool_agg
                .iter()
                .take(8)
                .map(|t| {
                    format!(
                        "  - {}：{}次，均{:.1}s，总{:.1}s，最慢{:.1}s",
                        t.name, t.count, t.avg_secs, t.total_secs, t.max_secs
                    )
                })
                .collect();
            format!(
                "## 性能数据（程序计算）\n\n\
                 轨迹总耗时 ≈ {wall:.0}s，工具执行 {tool:.0}s，共 {count} 次工具调用。\n\n\
                 ### 工具调用统计（按总耗时排序）\n\n{agg}\n\n\
                 ### Top 慢调用明细\n\n```json\n{tools}\n```",
                wall = candidates.wall_secs,
                tool = candidates.tool_secs,
                count = candidates.tool_count,
                agg = agg_lines.join("\n"),
                tools = tools_json,
            )
        }
        // experience_library: full raw trajectory (tool outputs trimmed) for semantic recognition
        "experience_library" => {
            let trimmed = crate::atif::render_trimmed(trajectory);
            format!(
                "## 完整执行轨迹（工具输出已裁剪）\n\n\
                 轨迹总耗时 ≈ {wall:.0}s，共 {count} 次工具调用。\n\n\
                 请从以下轨迹中识别低效轮次（做了白做、踩坑后回退、方向错误等）。\n\n\
                 ```\n{raw}\n```",
                wall = candidates.wall_secs,
                count = candidates.tool_count,
                raw = trimmed,
            )
        }
        // fallback: full summary
        _ => {
            let payload =
                serde_json::to_string_pretty(candidates).unwrap_or_else(|_| "{}".to_string());
            format!(
                "## 性能数据（程序计算）\n\n\
                 轨迹总耗时 ≈ {:.0}s，共 {} 次工具调用。

```json
{}
```",
                candidates.wall_secs, candidates.tool_count, payload,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{CacheTurn, ToolAggStats, ToolCallRecord};

    fn trajectory() -> AtifTrajectory {
        AtifTrajectory::from_json(
            r#"{
              "schema_version": "ATIF-v1.6",
              "session_id": "s1",
              "agent": {"name": "test", "model_name": "m"},
              "steps": [
                {"step_id": 1, "source": "agent", "message": "done", "timestamp": "2026-01-01T00:00:00Z"}
              ]
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn renders_prefix_cache_hit_rates() {
        let candidates = PerfCandidateSet {
            cache_turns: vec![
                CacheTurn {
                    prompt_tokens: 100,
                    cached_tokens: 80,
                },
                CacheTurn {
                    prompt_tokens: 100,
                    cached_tokens: 0,
                },
            ],
            ..Default::default()
        };

        let section = build_data_section(&candidates, "prefix_cache", &trajectory());
        assert!(section.contains("Cache 命中率：总体 40%"));
        assert!(section.contains("每轮命中率：[80%, 0%]"));
    }

    #[test]
    fn renders_fast_tool_aggregation_and_details() {
        let candidates = PerfCandidateSet {
            wall_secs: 10.0,
            tool_secs: 6.0,
            tool_count: 2,
            top_tools: vec![ToolCallRecord {
                name: "Bash".into(),
                call_id: "call-1".into(),
                start: 1.0,
                dur: 5.0,
                cmd: "rg foo".into(),
                err: false,
                result_tokens: None,
            }],
            tool_agg: vec![ToolAggStats {
                name: "Bash".into(),
                count: 2,
                total_secs: 6.0,
                avg_secs: 3.0,
                max_secs: 5.0,
            }],
            ..Default::default()
        };

        let section = build_data_section(&candidates, "fast_tool", &trajectory());
        assert!(section.contains("Bash：2次，均3.0s，总6.0s，最慢5.0s"));
        assert!(section.contains("\"name\": \"Bash\""));
    }

    #[test]
    fn builds_prompt_and_fallback_summary() {
        let candidates = PerfCandidateSet {
            wall_secs: 12.0,
            tool_count: 1,
            ..Default::default()
        };
        let messages = build_strategy_prompt(&candidates, &STRATEGIES[0], &trajectory());
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "system");
        assert!(messages[1].content.contains("prefix_cache"));

        let fallback = build_data_section(&candidates, "unknown", &trajectory());
        assert!(fallback.contains("轨迹总耗时 ≈ 12s"));
        assert!(fallback.contains("\"tool_count\": 1"));
    }
}
