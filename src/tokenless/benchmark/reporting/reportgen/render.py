# Copyright 2026 Alibaba Cloud
# Licensed under the Apache License, Version 2.0.
"""两份独立结果报告：四项指标、优势不足与未测范围同时呈现。"""

import re
from collections import defaultdict
from statistics import median

from .privacy import anonymous, safe_choice
from .statistics import describe, independent, percentile, summary
from .terms import (
    CATEGORIES,
    DISPOSITIONS,
    MODES,
    OPERATIONS,
    PRODUCTS,
    RECOVERY,
    glossary,
)


def pct(value):
    return "未测/不适用" if value is None else f"{value * 100:.1f}%"


def fixed(value, unit=""):
    return "未测/不适用" if value is None else f"{value:.3f}{unit}"


def table(header, rows):
    def cell(value):
        return str(value).replace("|", "\\|").replace("\n", " ")

    return ["| " + " | ".join(header) + " |", "|" + "---|" * len(header)] + [
        "| " + " | ".join(cell(value) for value in row) + " |" for row in rows
    ]


def mode_name(row):
    title = safe_choice(row.mode, MODES, "未记录的配置（不推断默认值）")
    if row.processing_condition:
        title += f"（显式RTK条件：{anonymous(row.processing_condition)}）"
    return title


def dataset_name(row):
    return anonymous(row.dataset) if row.dataset else "未记录数据集"


def tokenizer_name(row):
    return safe_choice(
        row.tokenizer,
        {"o200k_base": "o200k_base", "cl100k_base": "cl100k_base", "bytes/4": "bytes/4 辅助估算"},
        "计数工具未记录或未识别",
    )


def joined_choices(value, choices):
    return (
        "；".join(dict.fromkeys(safe_choice(part.strip(), choices) for part in value.split(" + ")))
        if isinstance(value, str)
        else "未记录/不适用"
    )


def operation_name(row):
    names = [title for key, title in OPERATIONS.items() if key in row.mechanism]
    return "；".join(names) or "原测量未提供已知处理标记；需查看私有原始记录"


def limitations(row):
    allowed = {
        "单组件尝试结果，不是生产入口最终结果",
        "显式手动 RTK 路径，不代表默认执行前处理",
        "处理后 Token 由原报告比例还原；不是新增实测",
        "原报告共享参考流程时延；不是本分支独立计时",
        "模型测题作答不完整，保持率不作为完整成绩",
        "同一输入观察到不同输出；不能声明确定性",
        "生产入口计时：含运行时仲裁；命令改写另计包装执行减原执行耗时，含进程启动",
        "进程内调用计时，不含进程启动、模型或网络",
        "参考进程内内容分派计时，不含进程启动、管道传输或JSON封装",
        "包装执行减原执行耗时，包含包装器进程启动",
        "具体计时范围未记录或无法识别；不同路径时延不可直接排名",
        "基础会话流程未独立测量时延和重要信息；不借用内容分派流程成绩",
        "未记录模型计量",
        "选定任务的实例计量缺失",
        "逐回合计量与总量不一致",
        "计量字段缺失或非法，不作为零费用",
        "实例计量可读；不认证完整费用",
        "官方评分只证明补丁任务结果；不证明压缩已生效或语义保持率",
        "选定任务保留在分母清单中；未完成评分不发布解决率",
        "参考侧未记录处理结果或恢复状态时保持未知，不从节省率推断",
        "源汇总结果，不反推逐行样本、Token实测或独立N",
        "历史 bytes/4 辅助估算，不作为主计数或账单",
        "生产入口不适用或恢复能力不可用；保留原计量，不参与节省率或配对统计",
        "运行标识未记录；仅作本次输入定位，不能跨采集关联",
    }
    visible, unknown = [], 0
    for note in dict.fromkeys(row.notes):
        if note in allowed:
            visible.append(note)
        elif isinstance(note, str) and re.fullmatch(
            r"实例计量可读；不认证完整费用；未选中任务计量已排除：\d+", note
        ):
            visible.append(note)
        else:
            unknown += 1
    if unknown:
        visible.append(f"未登记限制说明已隐藏（{unknown}条）；需检查输入契约")
    return "；".join(visible) or "按原测量计时范围解释；原始记录保持私有"


def data_rows(rows):
    lines = []
    for row in rows:
        if row.evidence_kind != "row":
            continue
        conditions = row.semantic or {"unmeasured": {}}
        for condition, semantic in conditions.items():
            reading = {"inline": "只读压缩正文", "retrieve": "允许找回原文"}.get(
                condition, "模型测题未运行或不能绑定输入"
            )
            evidence = f"{row.facts[0]}/{row.facts[1]}" if row.facts else "没有检查分母"
            if len(row.output_hashes) > 1:
                evidence += "；同输入输出不一致"
            lines.append(
                [
                    anonymous(row.sample)
                    + (
                        " @" + anonymous(row.input_hash) if row.input_hash else "（输入指纹未记录）"
                    ),
                    PRODUCTS[row.side],
                    mode_name(row),
                    safe_choice(row.category, CATEGORIES),
                    reading,
                    f"{row.before if row.before is not None else '未知'}→{row.after if row.after is not None else '未知'}",
                    pct(row.rate),
                    joined_choices(row.disposition, DISPOSITIONS),
                    joined_choices(row.recovery, RECOVERY),
                    operation_name(row),
                    evidence,
                    pct(semantic.get("information_retained"))
                    + (
                        f" [95%: {pct(semantic['information_ci95'][0])}, {pct(semantic['information_ci95'][1])}]"
                        if semantic.get("information_ci95") is not None
                        else ""
                    ),
                    pct(semantic.get("task_original")),
                    pct(semantic.get("task_processed")),
                    pct(semantic.get("task_retained")),
                    fixed(median(row.latency_ms) if row.latency_ms else None, " ms"),
                    fixed(
                        percentile(row.latency_ms, 0.95) if len(row.latency_ms) > 1 else None, " ms"
                    ),
                    fixed(semantic.get("latency_change_s"), " s"),
                    row.observations,
                    dataset_name(row),
                    tokenizer_name(row),
                    limitations(row),
                ]
            )
    return table(
        [
            "匿名样本 / 输入指纹",
            "产品",
            "配置",
            "输入类型",
            "阅读方式",
            "Token 处理前→后",
            "Token 节省率",
            "处理结果",
            "原文存放方式",
            "具体处理方式",
            "重要信息检查通过数",
            "信息答对保持率",
            "原文任务题答对率",
            "处理后任务题答对率",
            "原文会做的任务题保持率",
            "压缩处理时延（中位数）",
            "较慢处理的时延边界（95%）",
            "模型作答时延变化（中位数）",
            "已有测量次数",
            "数据集",
            "Token计数工具",
            "路径与计时限制",
        ],
        lines,
    )


def l4_rows(rows):
    lines = []
    for row in rows:
        value = row.task
        selected, resolved = value.get("selected"), value.get("resolved")
        lines.append(
            [
                anonymous(row.sample),
                PRODUCTS[row.side],
                mode_name(row),
                selected if selected is not None else "未知",
                value.get("submitted"),
                resolved if resolved is not None else "未评分",
                pct(resolved / selected if selected and resolved is not None else None),
                "压缩效果未认证",
                "模型测题未运行",
                value.get("changed_results")
                if value.get("changed_results") is not None
                else "未记录",
                value.get("prompt_tokens")
                if value.get("prompt_tokens") is not None
                else "计量不完整",
                value.get("requests") if value.get("requests") is not None else "计量不完整",
                fixed(value.get("duration_s"), " s"),
                safe_choice(
                    value.get("status"),
                    {
                        "scored": "官方已评分",
                        "completed_no_predictions": "已完成且无补丁（由记录推断）",
                        "unscored": "未评分",
                    },
                ),
                limitations(row),
            ]
        )
    return table(
        [
            "匿名运行组",
            "产品",
            "配置",
            "全部选定任务",
            "产出补丁",
            "官方修复成功",
            "官方任务解决率",
            "Token节省",
            "信息答对保持率",
            "捕获的输出改变数",
            "模型输入Token（含缓存）",
            "模型请求数",
            "整组任务耗时",
            "评分证据状态",
            "限制说明",
        ],
        lines,
    )


def findings(rows):
    groups = defaultdict(list)
    for row in rows:
        groups[
            row.side,
            row.mode,
            row.tokenizer,
            row.dataset,
            row.processing_condition,
            row.evidence_kind,
        ].append(row)
    text = ["### 已验证的优势与还存在的问题", ""]
    dispersions = []
    for (side, mode, tokenizer, dataset, condition, kind), items in sorted(groups.items()):
        if kind != "row":
            text.append(
                f"- {PRODUCTS[side]}：只有源汇总数据，不反推逐行样本，不参与新的独立N或配对统计。"
            )
            continue
        if items[0].layer == "l4":
            text.append(
                f"- {PRODUCTS[side]}：只有运行与官方任务评分记录；压缩生效、模型信息保持率与费用收益仍未认证。不能从产出补丁或处理日志推断优势。"
            )
            continue
        facts = summary(items)
        measured = facts["measured"]
        success = facts["saving_rows"]
        rates = facts["rates"] or {}
        interval = rates.get("ci95")
        iqr = rates.get("iqr")
        dispersions.append(
            [
                PRODUCTS[side],
                mode_name(items[0]),
                dataset_name(items[0]),
                tokenizer_name(items[0]),
                measured,
                pct(rates.get("std")),
                f"[{pct(iqr[0])}, {pct(iqr[1])}]" if iqr else "样本不足",
                f"[{pct(interval[0])}, {pct(interval[1])}]" if interval else "样本不足",
            ]
        )
        label = f"{PRODUCTS[side]}／{mode_name(items[0])}／数据集：{dataset_name(items[0])}／计数工具：{tokenizer_name(items[0])}"
        text.append(
            f"- {label}：{len(items)} 个结果行中有 {measured} 个可用于节省率汇总，其中 {success} 个省了Token。总Token节省 {pct(facts['weighted'])}，样本节省率平均 {pct(rates.get('mean'))}，中位 {pct(rates.get('median'))}。"
        )
        if success:
            text.append(
                f"  - 已验证长处：在上述 {success} 个相同输入上产生较短输出；仅对本批已测配置成立，不代表每类输入都有收益。"
            )
        text.append(
            f"  - 限制：可测输入中 {measured - success} 个没有Token节省；重复别名和不完整测量不作为新增独立输入。语义和任务需要另看逐行成绩，未测不能记满分。不同输入类型及计时路径不作全产品排名。"
        )
    if dispersions:
        text += ["", "样本节省率的波动与估计范围（相同输入重复计时不增加独立样本数）：", ""]
        text += table(
            [
                "产品",
                "配置",
                "数据集",
                "Token计数工具",
                "可测输入数",
                "样本间波动（标准差）",
                "中间一半样本范围",
                "平均节省率95%估计范围",
            ],
            dispersions,
        )
    return text + [""]


def comparisons(rows):
    groups = defaultdict(list)
    for row in independent(rows):
        if (
            row.layer != "l4"
            and row.input_hash is not None
            and row.rate is not None
            and len(row.output_hashes) <= 1
        ):
            groups[
                row.layer, row.side, row.mode, row.dataset, row.tokenizer, row.processing_condition
            ].append(row)
    lines = []
    left = [key for key in groups if key[1] == "tokenless"]
    for a in sorted(left):
        for b in sorted(
            key for key in groups if key[1] == "headroom" and (key[0], *key[3:5]) == (a[0], *a[3:5])
        ):
            tl = {row.pair_key: row for row in groups[a]}
            hr = {row.pair_key: row for row in groups[b]}
            keys = sorted(tl.keys() & hr.keys())
            if not keys:
                continue
            pairs = [key for key in keys if tl[key].before == hr[key].before]
            if not pairs:
                continue
            differences = [tl[key].rate - hr[key].rate for key in pairs]
            desc = describe(differences) or {}
            interval = desc.get("ci95")
            lines.append(
                [
                    a[0].upper(),
                    mode_name(groups[a][0]),
                    mode_name(groups[b][0]),
                    len(pairs),
                    pct(summary([tl[key] for key in pairs])["weighted"]),
                    pct(summary([hr[key] for key in pairs])["weighted"]),
                    fixed(desc.get("mean") * 100 if desc else None),
                    f"[{interval[0] * 100:.1f}, {interval[1] * 100:.1f}]"
                    if interval
                    else "样本不足",
                    dataset_name(groups[a][0]),
                    tokenizer_name(groups[a][0]),
                ]
            )
    return (
        [
            "## 同一输入的两侧差异",
            "",
            "只比较输入指纹和Token计数工具一致的行；同内容别名不增加配对数，显式处理条件分别比较。缺失侧不记为零；区间是已有样本的描述性抽样，不是新的实测或全产品胜负。",
            "",
        ]
        + table(
            [
                "层",
                "Tokenless配置",
                "Headroom配置",
                "同输入配对数",
                "Tokenless总Token节省率",
                "Headroom总Token节省率",
                "样本平均节省率差（百分点）",
                "差值95%估计范围",
                "数据集",
                "Token计数工具",
            ],
            lines,
        )
        + [""]
    )


def report(rows, comparison=False):
    sides = ("tokenless", "headroom", "baseline") if comparison else ("tokenless", "baseline")
    scoped = [row for row in rows if row.side in sides]
    title = "Tokenless 与 Headroom 测试结果对比" if comparison else "Tokenless 测试结果与能力边界"
    out = [
        f"# {title}",
        "",
        "根据调用方提供的已采集数据生成；未运行层不填写成绩。原测量版本保留在输入文件中，不把旧结果重标为当前源码。",
        "",
    ] + glossary()
    for layer in ("l1", "l2", "l3", "l4"):
        items = [row for row in scoped if row.layer == layer]
        out += [f"## {layer.upper()} 测试数据", ""]
        if not items:
            out += ["未运行或未提供本报告侧的数据；不填零节省、零时延或满分。", ""]
            continue
        out += findings(items)
        out += l4_rows(items) if layer == "l4" else data_rows(items)
        aggregates = [row for row in items if row.evidence_kind == "aggregate"]
        if aggregates:
            out += ["", "### 源汇总结果（不反推逐行测量）", ""]
            out += table(
                ["源数据集", "产品", "输入类型", "计数口径", "原报告节省率", "源报告N", "限制说明"],
                [
                    [
                        dataset_name(row),
                        PRODUCTS[row.side],
                        safe_choice(row.category, CATEGORIES),
                        tokenizer_name(row),
                        pct(row.source_aggregate.get("rate")),
                        row.source_aggregate.get("n")
                        if row.source_aggregate.get("n") is not None
                        else "未记录",
                        limitations(row),
                    ]
                    for row in aggregates
                ],
            )
        out += [
            "",
            "### 指标有效范围",
            "",
            "原始身份、服务器与凭据不复制到报告；同次生成使用稳定 HMAC 假名。未提供私有 salt 时不关联跨次标签，输入指纹不全时不作共同样本排名。",
            "",
        ]
    if comparison:
        out += comparisons(scoped)
    out += [
        "## 结论的使用边界",
        "",
        "保存原文不代表模型读取了它；节省更多Token不代表能做对更多题；压缩处理更快不代表整项任务更快。L1尝试结果与L2生产入口规则不同，L3静态会话与L4完整任务不同。保留未知和失败，不跨范围宣布某产品整体优于另一产品。",
        "",
    ]
    return "\n".join(out)
