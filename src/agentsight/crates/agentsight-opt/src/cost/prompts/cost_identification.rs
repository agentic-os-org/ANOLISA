use crate::llm::ChatMessage;
use crate::types::{CostRatioMetrics, WasteCandidate, WasteCandidateSet};

const SYSTEM_PROMPT: &str = include_str!("../../../prompts/cost_identification.md");

/// A cost strategy definition for per-candidate parallel evaluation.
/// `id` matches the stable candidate id produced by Rust extraction.
/// Agent-first: Rust computes the metrics but does NOT pre-filter — admission
/// criteria are carried in the prompt for the LLM to check itself.
pub struct CostStrategyDef {
    pub id: &'static str,
    pub name: &'static str,
    /// 适用判据（定性描述，无硬阈值）— 由 LLM 结合实测指标自行权衡。
    pub admission: &'static str,
    /// 不推荐条件 — LLM 据此反向排除。
    pub not_recommended: &'static str,
    /// 推荐优化方法 — Agent 侧可落地的具体动作，随 prompt 下发供 LLM 给建议。
    pub method: &'static str,
    /// 语义判断要点（S 类指标，仅 LLM 可判）。
    pub judge_hint: &'static str,
    /// Verdict depends on an S-class semantic call → frontend shows 建议·需确认.
    pub needs_confirm: bool,
}

/// All cost strategies to evaluate in parallel (one LLM call per matched candidate).
pub const STRATEGIES: &[CostStrategyDef] = &[
    CostStrategyDef {
        id: "fixed_overhead",
        name: "前缀缓存（Prefix Caching）",
        admission: "看命中率序列：持续为零或偏低即未在享受缓存；无 usage 数据可能意味着未开启 prompt caching，这本身就是适用信号",
        not_recommended: "命中率已经很高（已在享受缓存）；前缀含时间戳等动态内容时应先修稳定性再谈缓存",
        method: "稳定前缀：系统提示词去时间戳/动态内容，工具定义固定顺序，会话中途不增删工具",
        judge_hint: "这是折价手段（省单价，非删 token）。判断系统提示词/工具定义是否稳定不变；若频繁变动则缓存收益低",
        needs_confirm: false,
    },
    CostStrategyDef {
        id: "history",
        name: "历史消息裁剪（History Compaction）",
        admission: "看每步历史占比序列：持续攀升、占据上下文大头即适用；旧内容是否还被引用由你判断",
        not_recommended: "旧内容仍被后续步骤引用（信息丢失风险）；M3 缓存命中率高时删改历史会使缓存失效，净收益需折算",
        method: "改框架配置：开启/调低 compaction 阈值，旧轮次摘要替换",
        judge_hint: "早期已过时的历史可摘要替换；若后续步骤仍在引用则不值得删",
        needs_confirm: false,
    },
    CostStrategyDef {
        id: "tool_output",
        name: "工具输出截断（Tool Trim）",
        admission: "看超长工具返回列表（步号/工具/token/重放占比）：单条重放开销大且内容多为无关死重即适用",
        not_recommended: "S1 低（通篇都是任务必需信息）；截掉的信息不可恢复，宁保守",
        method: "采用 rtk 等输出压缩工具；tool wrapper 加截断/分页；引导 head/grep 替代全量读取",
        judge_hint: "S1 死重占比：大工具返回里与任务无关内容的比例。大部分是无关\"死重\"才值得截断",
        needs_confirm: true,
    },
    CostStrategyDef {
        id: "user_prompt",
        name: "提示词压缩（Prompt Compression）",
        admission: "看长输入列表（token/重放占比）：重放开销大且被实际引用的部分少即适用",
        not_recommended: "整段输入都是待分析对象本身",
        method: "优化提示词：长材料转文件引用，模板去冗余",
        judge_hint: "用户粘贴的长输入若含大量无关内容可压缩或引导转为文件提供",
        needs_confirm: true,
    },
    // ── 减轮次浪费（prevention 类）──
    // 判定走 `prompts/waste_detour.md`（全量轮次账本 + 反事实删除测试 + 归因），
    // 不走本文件的 system prompt：这条不裁 payload，省的是下次会话少跑的整轮。
    CostStrategyDef {
        id: "detour",
        name: "弯路 · 归因并沉淀修复方案",
        admission: "轨迹里存在产出未进入最终结果因果链的轮段——报错反复、方向选错后被推翻、产物完成后被隐性规范打回，事后看整段可跳过",
        not_recommended: "必要探索（排除选项且结论被后续引用）；删掉任务就失败的必要修复轮；少于 5 轮的孤立小坑（低于报表门槛）",
        method: "对每段弯路先归因（可预知坑/隐性规范/方向选错/偶发故障/无意义重复），再从走通的回合反推修复方案与落点（Skill/项目规范/环境配置），偶发故障只计成本不出经验",
        judge_hint: "核心是反事实删除测试——删掉这段，剩下的轨迹还能走到同样的终点吗；方向弯路看 SAYS 列的宣告-推翻对，坑的等价性按语义判，不看报错文本是否逐字相同",
        needs_confirm: true,
    },
];

/// Look up the strategy definition matching a candidate id.
pub fn strategy_for(candidate_id: &str) -> Option<&'static CostStrategyDef> {
    STRATEGIES.iter().find(|s| s.id == candidate_id)
}

/// Render the trajectory-level M-class metrics block shared by every strategy
/// prompt (证据要求: verdicts must cite metric IDs and values).
fn render_metrics(m: &CostRatioMetrics) -> String {
    let m3 = match m.m3_cache_hit_rate {
        Some(v) => format!("{:.0}%（真实账单口径）", v * 100.0),
        None => "无 usage 数据".to_string(),
    };
    format!(
        "- M1 前缀账单占比: {:.0}%\n\
         - M3 缓存命中率: {}\n\
         - M7 历史占比峰值: {:.0}%\n\
         - M15 工具步占比: {:.0}%\n\
         - M16 空转账单占比: {:.1}%",
        m.m1_prefix_share * 100.0,
        m3,
        m.m7_history_peak_share * 100.0,
        m.m15_tool_step_share * 100.0,
        m.m16_churn_share * 100.0,
    )
}

/// Build the per-candidate evaluation prompt: one candidate + its admission /
/// not-recommended criteria + trajectory metrics. Keeps each LLM call small.
pub fn build_strategy_prompt(
    set: &WasteCandidateSet,
    candidate: &WasteCandidate,
    strategy: &CostStrategyDef,
) -> Vec<ChatMessage> {
    let payload = serde_json::to_string_pretty(candidate).unwrap_or_else(|_| "{}".to_string());
    vec![
        ChatMessage::system(SYSTEM_PROMPT),
        ChatMessage::user(format!(
            "## 待评估策略\n\n\
             **{id} — {name}**\n\n\
             - 适用判据（看候选里的实测数据自行权衡，无硬阈值）：{admission}\n\
             - 不推荐条件：{not_recommended}\n\
             - 推荐优化方法（建议需落在 Agent 侧可执行的动作上）：{method}\n\
             - 语义判断要点：{hint}\n\n\
             ## 轨迹背景\n\n\
             轨迹共 {steps} 步，计费 input ≈ {input} tok，output ≈ {output} tok，模型 {model}。\n\n\
             ## 全局指标（Rust 实测，M 类）\n\n\
             {metrics}\n\
             - 本候选预计节省账单占比: {share:.1}%\n\n\
             ## 成本浪费候选（程序预处理）\n\n\
             ```json\n{payload}\n```\n\n\
             请结合实测指标与内容样本权衡该候选是否值得优化，判断标准由你把握。证据必须引用指标 ID 与数值。仅返回 JSON。",
            id = strategy.id,
            name = strategy.name,
            admission = strategy.admission,
            not_recommended = strategy.not_recommended,
            method = strategy.method,
            hint = strategy.judge_hint,
            steps = set.total_steps,
            input = set.total_input_tokens,
            output = set.total_output_tokens,
            model = set.model,
            metrics = render_metrics(&set.metrics),
            share = candidate.save_share * 100.0,
            payload = payload,
        )),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate() -> WasteCandidate {
        WasteCandidate {
            id: "tool_output".into(),
            category: "上下文臃肿".into(),
            subtype: "工具输出多".into(),
            optimization: "工具输出截断".into(),
            potential_save_tokens: 1200,
            discount: false,
            save_share: 0.123,
            savings_kind: "可省".into(),
            steps: vec![3],
            facts: "tool output 1200 tok".into(),
            snippet: "large output".into(),
        }
    }

    #[test]
    fn finds_strategy_by_candidate_id() {
        let strategy = strategy_for("tool_output").unwrap();
        assert_eq!(strategy.id, "tool_output");
        assert!(strategy.name.contains("工具输出截断"));
        assert!(strategy_for("missing").is_none());
    }

    #[test]
    fn builds_prompt_with_metric_values_and_candidate_json() {
        let set = WasteCandidateSet {
            model: "test-model".into(),
            total_steps: 4,
            total_input_tokens: 1000,
            total_output_tokens: 200,
            metrics: CostRatioMetrics {
                m1_prefix_share: 0.25,
                m3_cache_hit_rate: Some(0.75),
                m7_history_peak_share: 0.40,
                m14_thinking_ratio: 0.0,
                m15_tool_step_share: 0.60,
                m16_churn_share: 0.125,
            },
            candidates: vec![candidate()],
            ledger: vec![],
        };
        let messages = build_strategy_prompt(
            &set,
            &set.candidates[0],
            strategy_for("tool_output").unwrap(),
        );

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "system");
        let body = &messages[1].content;
        assert!(body.contains("tool_output"));
        assert!(body.contains("工具输出截断"));
        assert!(body.contains("M1 前缀账单占比: 25%"));
        assert!(body.contains("M3 缓存命中率: 75%（真实账单口径）"));
        assert!(body.contains("M16 空转账单占比: 12.5%"));
        assert!(body.contains("本候选预计节省账单占比: 12.3%"));
        assert!(body.contains("\"id\": \"tool_output\""));
    }

    #[test]
    fn renders_missing_usage_metric() {
        let metrics = CostRatioMetrics {
            m3_cache_hit_rate: None,
            ..Default::default()
        };
        assert!(render_metrics(&metrics).contains("M3 缓存命中率: 无 usage 数据"));
    }
}
