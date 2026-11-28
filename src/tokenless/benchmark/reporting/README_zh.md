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
输出目录不能在输入运行目录内部。写入前核对两个固定报告名与源 JSON、模型测题证据
的路径、符号链接和硬链接身份，禁止覆盖输入。

## 接受的数据格式

| 层 | 输入 | 边界 |
|---|---|---|
| L1 | 当前 `compression_rate --json` 的 canonical 汇总；可选逐行 component_cases、component_domains 或 Headroom 单组件 JSON | 历史字节估算只在源汇总表展示，不反推逐行实测、独立 N 或账单；逐行尝试结果仍不是生产终态。 |
| L2 | 当前 report.json 的类别汇总，或 measurements.json schema 1/2（schema 2 支持 text_store 文本引用） | 汇总不反推样本；逐行输入按相同处理条件合并重复计时，不同 RTK 命令保持独立。 |
| L3 | 含场景的 report.json，以及可选同目录 lifecycle.json | 源文档指纹隔离不同运行；过程文件提供实际输入/输出身份与模式。只有确认非空字符串入口数组才算适用；gap 行保留 Token 计量但不发布节省率、事实或模型成绩。缺过程文件不能绑定模型成绩；共享参考计时明确标注。 |
| L4 | 含 out/repeat-N/arm-X 和可选 eval/ 的根目录，或含这些结构的逐机采集目录 | 选定任务、补丁指纹和唯一评分证据决定已解决数。缺生产运行标识必须明示：元数据原始字节摘要仅定位输入窗口，不是稳定运行身份；累计运行和重复观察可见，非法记录 ID 被拒绝。缺评分保留未评分，无补丁任务保留在分母中。 |

此 PR 可以独立读取当前生产端的汇总格式；逐行格式是可选的，需要能输出对应格式的
生产端版本。读取汇总无需未合分支或外部转换器，缺失的逐行信息保持未测。
L3 大于 100% 的节省率被拒绝；负节省率表示合法的输出膨胀。

统计汇总按产品、模式和实际处理条件对相同输入字节只计一次，即使它有多个样本别名。
别名仍用于逐行追踪；同输入不同输出不参与稳定结果汇总。只有内容分派流程（Router）
测量时，基础流程（PureStage）的重要信息和时延保持未测，不能借用前者成绩。
报告保留实际计时范围说明，不把不同路径的时延当作可直接排名的指标。

可选模型结果目录包含评价工具生成的 plan.json 与 stats.json。
只有层、样本、产品、模式以及原输入和处理后输出指纹都匹配时才附成绩。
同名不同输入不借用作答结果；声明指纹须与可用正文相符。模型结果未记录来源运行时，
L3 计划与统计须共同记录相同 `source_run`：对 UTF-8 编码的
`json.dumps(report_document, sort_keys=True, ensure_ascii=False)` 文本取 SHA-256。
即使只导入一个来源，缺标识也保持未测；计划与统计的来源冲突被拒绝。
查询键包含来源运行：只导入某次运行的成绩时，其他运行保持未测；重复提供多个成绩目录
可以分别绑定各运行，不因原文相同而互相覆盖。作答不完整不记满分。
读取这些文件不是重新运行模型，也不需要模型凭据。每个成绩目录先校验来源，空统计
也不能绕过。显式提供但无法绑定的证据会在 stderr 给出不含路径的说明，并进入报告
限制列，不能静默地表现成没有提供模型成绩。

L4 输入/输出/缓存读取/请求数必须是非负整数；null、字符串、布尔值和
已知总量不一致不是有效计费观察。L4 按选定任务、元数据、补丁、实例计量和工具输出
捕获文件的原始字节摘要识别输入窗口；相同采集副本换目录后仍合并为重复观察，不重复
计任务或用量。不同窗口保留独立证据。相同窗口的已验证评分优先于缺评分，且不依赖
导入顺序；互相矛盾的已验证计数被拒绝。多窗口不可累加标注要求生产端记录了运行 ID；
未记录时不能跨采集关联，也不能假设不同窗口可相加。无任何采集证据的 arm 目录被拒绝；
缺预测文件且无完成证据时，产出补丁数保持未知。
官方补丁判断仍要求原失败测试通过、
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
只有测试需要 pytest，报告生成不需要。聚合入口 `make test` 分别使用 L4 与报告生成器
的解释器选择器；仅设置 `L4_TEST_PYTHON` 不会设置报告解释器。Python >= 3.12 环境
下，从 Tokenless 目录同时指定：

```bash
make test L4_TEST_PYTHON=<venv-python> REPORT_TEST_PYTHON=<venv-python>
```
