# 测试结果报告生成器

[English](README.md)

从 L1–L4 任意非空测试层组合的已有测量数据生成两份结果报告。
这是离线读取器，不启动压缩器、Docker、模型调用或参考进程。
未运行的层和未知指标保持“未测”，不是零值或满分。
结果保留原测试身份；工具不认证旧数据代表较新源码版本。

## 环境与入口

需要 Python 3.12 或以上，报告生成只使用标准库。从本目录运行：

```bash
python3 -m reportgen --l1 <component-result.json> --out <report-directory>
python3 -m reportgen --l2 <measurements.json> --out <report-directory>
python3 -m reportgen --l3 <scenario-report.json> --out <report-directory>
python3 -m reportgen --l4 <collected-run-directory> --out <report-directory>
```

可组合任意层；同一参数可重复，表示其他轮次或输入：

```bash
python3 -m reportgen --l1 <component-result.json> --l2 <measurements.json> --out <report-directory>
python3 -m reportgen --l2 <round-one.json> --l2 <round-two.json> --out <report-directory>
python3 -m reportgen --l3 <scenario-report.json> --llm l3=<model-results-directory> --out <report-directory>
```

生成 `tokenless-report_zh.md`（仅 Tokenless 与必要未压缩对照）和
`comparison-report_zh.md`（已测双侧数据）。每份包含逐行测量、数据支持的
优势和不足、直白指标解释以及未测范围。报告属于生成产物，不作为代码提交。
输出目录不能在输入运行目录内部。

## 接受的数据格式

| 层 | 输入 | 边界 |
|---|---|---|
| L1 | 当前 `compression_rate --json` 的 canonical 汇总；可选逐行 component_cases、component_domains 或 Headroom 单组件 JSON | 历史字节估算只在源汇总表展示，不反推逐行实测、独立 N 或账单；逐行尝试结果仍不是生产终态。 |
| L2 | 当前 report.json 的类别汇总，或 measurements.json schema 1/2（schema 2 支持 text_store 文本引用） | 汇总不反推样本；逐行输入按相同处理条件合并重复计时，不同 RTK 命令保持独立。 |
| L3 | 含场景的 report.json，以及可选同目录 lifecycle.json | 过程文件提供实际输入、输出身份与模式；缺失则不能绑定模型成绩。共享参考计时明确标注，不说成每条流程都独立计时。 |
| L4 | 含 out/repeat-N/arm-X 和可选 eval/ 的根目录，或含这些结构的逐机采集目录 | 选定任务、当前补丁指纹和唯一评分标记共同决定已解决数。缺评分保留未评分，已完成但无补丁的任务保留在分母中。 |

此 PR 可以独立读取当前生产端的汇总格式；逐行格式是可选的，需要能输出对应格式的
生产端版本。读取汇总无需未合分支或外部转换器，缺失的逐行信息保持未测。
L3 大于 100% 的节省率被拒绝；负节省率表示合法的输出膨胀。

统计汇总按产品、模式和实际处理条件对相同输入字节只计一次，即使它有多个样本别名。
别名仍用于逐行追踪；同输入不同输出不参与稳定结果汇总。只有内容分派流程（Router）
测量时，基础流程（PureStage）的重要信息和时延保持未测，不能借用前者成绩。
报告保留实际计时范围说明，不把不同路径的时延当作可直接排名的指标。

可选模型结果目录包含评价工具生成的 plan.json 与 stats.json。
只有层、样本、产品、模式以及原输入和处理后输出指纹都匹配时才附成绩。
同名不同输入不借用作答结果；作答不完整不记满分。
读取这些文件不是重新运行模型，也不需要模型凭据。

L4 输入/输出/缓存读取/请求数必须是非负整数；null、字符串、布尔值和
已知总量不一致不是有效计费观察。官方补丁判断仍要求原失败测试通过、
原通过测试继续通过。补丁通过不证明压缩生效；读取器不伪造 L4 语义保持率、
Token 节省、云价格或完整费用。

## 隐私与验证

不把原文、原始模型回答、主机目录、认证头或任意环境信息复制到报告。
样本、数据集和展示的指纹使用同次生成随机密钥的 HMAC 假名，原数据集名称不进入
汇总。跨次需要稳定标签时，显式提供私有环境变量 `REPORTGEN_SALT` 并妥善保管，
它不会写入日志或报告。假名本身不构成完整匿名保证；模式、输入类型和计数工具
使用受控词汇。
任何文件落盘前检查完整生成文本中的地址、私人目录、邮箱和凭据材料。
`--forbid <identifier>` 可增加不允许公开的个人/业务标识，这些值不写日志。
正则不能识别一切人名，因此字段白名单与匿名标签不可用正则扫描代替。
原始输入保持私有、字节不变，便于复算。`--verbose` 将详细异常写入私有 stderr，
可能包含输入路径，不可公开。普通校验错误只标明层、行与字段；未登记限制说明会
明确标记，而不是静默丢弃。L4 只累加选定任务的计量，并展示排除的实例数、评分
证据状态与限制。

```bash
python3 -m pytest -q tests
```

离线测试覆盖 15 种非空层组合、当前评分绑定、不完整计量、重复输入、
落盘前拒绝敏感内容和模型输入身份，不需要模型或 Docker 服务。
从 Tokenless 目录运行
`make test-benchmark-reporting REPORT_TEST_PYTHON=<venv-python>` 可调用同一套件。
只有测试需要 pytest，报告生成不需要。
