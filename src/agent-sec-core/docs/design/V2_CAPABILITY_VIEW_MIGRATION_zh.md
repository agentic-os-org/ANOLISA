# V2 Capability View（环境变量能力视图）迁移记录

> 本文记录 V1 `agent-sec-cli capabilities` 迁移到 V2 Rust CLI 的契约、等价性证据，以及
> **本次迁移刻意留下的能力缺口**。缺口表是后续能力迁移工作包的输入，必须随对应能力落地更新。

## 1. 迁移范围与定位

| 项 | 内容 |
|---|---|
| V1 实现 | `agent-sec-cli/src/agent_sec_cli/capabilities/{cli.py,view.py}` |
| V2 实现 | `v2/apps/asc-cli/src/capabilities.rs` + `capabilities/{manifest,resolve,render}.rs`，命令层 `v2/apps/asc-cli/src/commands/capabilities.rs` |
| 命令名 | `agent-sec-cli capabilities`（保持不变，argv、短选项、输出、退出码均与 V1 一致） |
| 执行位置 | **全部在 CLI 进程内**，不新增 daemon method，不建立 UDS 连接 |

该能力回答的问题是「**当前这个环境**里的 Hook 会怎么做」，因此必须在与 Agent 同一环境变量
上下文的进程里求值。V2 因此把它实现为本地命令：`Cli::plan()` 返回 `Plan::Local` 时，
`main` 调用 `capabilities::process_environment()` 构造环境快照并渲染，完全不解析 daemon socket。
该函数遍历 `std::env::vars_os()`，只保留 manifest 声明的变量名。这里**不能**用
`std::env::vars()`：它在进程里任何一个变量的值不是合法 UTF-8 时就会 panic，等于让一个与本视图
无关的变量既能中断只读命令（exit 101），又能把自己的值写进 stderr 的 panic 文本。不可解码字节
按 CPython 在 Unix 上的 surrogateescape 方式解码，使这类值对已解析变量保持「非法 → 回落默认 +
诊断」，对近原样上报的 L2 变量与 V1 输出一致。该行为由下面这个真实进程用例锁定（单测注入
`Environment`，走不到 `std::env`，因此必须走进程级用例）：

`tests/capabilities.rs::a_non_utf8_environment_never_aborts_the_view_or_echoes_its_value`

这也意味着 `AGENT_SEC_DAEMON_SOCKET` 缺失、为空或为相对路径时，`capabilities` 仍必须成功——
该边界由 E2E 用例 `test_capabilities_never_depends_on_a_daemon_endpoint` 锁定。

### 1.1 视图边界（environment-only scope）

沿用 V1 的既有边界，V2 不扩大也不缩小：

- 只读 CLI 进程继承到的环境变量；
- 不读取 Agent 配置文件、Agent home 目录、任何磁盘状态（`XDG_DATA_HOME` 仅做**语法**校验，不访问文件系统）；
- `enabled` 表示「环境变量允许该 Hook 运行」，**不证明** Hook 已在目标 Agent 进程中加载；
- 输出永不回显原始环境变量值：非法值一律折叠为「有效值 + 诊断」，唯一接近原样上报的
  `PROMPT_SCANNER_L2_MODEL` 也要经过转义与 80 字符截断。

## 2. 等价性证据

### 2.1 共享 E2E：同一份用例跑两个环境

`tests/e2e/cli/test_capabilities_e2e.py` 是唯一一份用例集（80 个参数化用例），由两个安装环境共用：

| 目标 | 环境 | 说明 |
|---|---|---|
| `make test-e2e-rpm` | V1 RPM | 一直在跑 |
| `make test-e2e-rpm-v2` | V2 RPM | 本次从 `--ignore` 列表中移除该文件，并删除对应的 pending 注释行 |

用例本身不含任何 V1/V2 分支：`_engine_l2_default()` 先向被测 CLI 探测它自己上报的 L2 默认值，
再据此决定是否断言 unsupported backend 诊断，因此同一份断言在两种引擎可用性下都成立。

本次开发期在 macOS 上的交叉验证结果（源码环境）：

- V1（Python CLI，`_native` 扩展已构建）：80 passed
- V2（`v2/target/debug/agent-sec-cli` 置于 PATH）：80 passed

### 2.2 逐字节输出比对

`my_scripts/compare_capabilities_v1_v2.sh`（本地开发脚本，不进入交付物）对 55 个场景运行两个
CLI 并 `diff` stdout/stderr/exit code，覆盖默认全矩阵（table 与 json）、mode 别名与 allowlist
回退、int/float timeout 全部边界、legacy PII 开关优先级、broad boolean 词表、`XDG_DATA_HOME`
合法与非法、L2 值转义与截断、大小写与空格归一化、三类参数校验错误、以及 6×5 单对矩阵。
在这些场景内，除下文 G1 掩码的两个字段外输出**逐字节一致**，包括表格最后一列的补齐空格。

注意该结论的边界：它覆盖的是上述场景集合，**不是全部可能取值**。已知落在集合之外的取值差异见
3.1 节 G8（Python 专有的数字字面量语法与浮点 `repr` 形态）。

### 2.3 V2 单元测试

- `capabilities/manifest.rs`：矩阵完备性、timeout 来源唯一性、L2 变量作用域
- `capabilities/resolve.rs`：strict/broad 布尔、hook 策略别名、int/float timeout 边界与 clamp、
  数据根语法、L2 上报与转义截断、scan_mode 作用域
- `capabilities/render.rs`：分组、列序、诊断合并
- `commands/capabilities.rs`：长短选项、三类错误的文案与退出码、默认表格、help 文案
- `tests/capabilities.rs`：公共接口的稳定排序、逐对可选、无原始值泄漏、本地 Plan 判定

## 3. 能力缺口表

本表只收录**必须在未来某个特定能力迁移时动手处理**的两项。沿用 V1 既有设计的边界、
滚动迁移方式本身决定的形态（V2 用 Rust 重写即意味着实现 fork）、以及已接受的实现取舍，
都不是缺口，列在 3.1 节。

两项缺口都**不会自动消失**：它们不在 `Makefile` 的待迁移清单里，只能靠本节识别。

| ID | 现象 | 影响面 | 处理时机与动作 | 关联位置 |
|---|---|---|---|---|
| **G1** | `PROMPT_SCANNER_L2_MODEL` 的 `default` 上报空字符串，且**不产生** `not a supported L2 backend` 诊断 | prompt-scan 的 L2 配置在 V2 上看不到默认值，配错 backend 时不会被提前提示。注意 V1 也有这条降级路径（原生扩展未构建时），但已部署的 V1 RPM 一定带扩展，因此这是与部署态 V1 的真实差异 | **prompt-scan 引擎迁入 V2 时**：改为向真实引擎查询默认 backend 与可选 backend 集合，并恢复 unsupported 诊断。本次不处理。用户文档（组件 README 与 user guide）同样**不加**过渡期说明：V1→V2 的用户面切换以 prompt-scan 引擎在 V2 补齐为前提，届时本缺口已消失、现有描述自然成立 | `capabilities/resolve.rs` 的 `EnvKind::Identifier` 分支；V1 对照 `agent-sec-cli/src/lib.rs::scanner_engine_info` |
| **G4** | ANOLISA 数据根语法校验（绝对路径、无 `.`/`..` 段）将在 V2 内部出现两份 | 与 V1 的 fork 属预期（见 3.1 节 G2）；真正的待办是 skill-ledger 迁入 V2 后，V2 内部会同时存在本视图的副本与 skill-ledger 自己的实现 | **skill-ledger 迁入 V2 时**：把校验收敛到 V2 内单一实现并让视图复用。该迁移本身不会自动删掉本副本 | `capabilities/resolve.rs::valid_data_home` 与 `agent_sec_cli/skill_ledger/paths.py::valid_anolisa_data_home` |

### 3.1 非缺口：沿用 V1 设计 / 迁移方式决定的形态 / 已接受的取舍

以下六项不是待办、不需要补全。记录它们只为两个目的：说明视图的语义边界，以及避免后人误以为
迁移遗漏而去「补」V1 本来就没有的行为、或把既定的实现 fork 与已接受的取舍当成缺陷。

| ID | 内容 | 为什么不是缺口 | 维护约定 |
|---|---|---|---|
| **G2** | agent/capability/env manifest 在 V1 Python 与 V2 Rust 各存一份 | V2 采用 contract-first 重写，Rust 侧 fork 一份 manifest 是迁移方式的既定形态；两代并存期结束（V1 下线、`view.py` 删除）后自然只剩一份 | 并存期间任何 manifest 改动（新增 agent、新增变量、改默认值/allowlist/timeout 上限）需双改；如要自动守卫，可加一条在同环境下比对两个 CLI JSON 输出（掩码 G1 字段）的漂移测试 |
| **G3** | V2 上报 5 个 capability 的配置，但目前只有 code-scan 在 V2 有执行路径 | 前提是其余能力按与 V1 一致的语义迁回。在该前提下本视图无需任何适配：manifest 除 L2 标识符（G1）外全部是静态常量（bool / keyword / timeout / data-home），不依赖任何运行期代码，因此能力迁入 V2 不会触发视图侧改动 | 若某次迁移**破了「语义与 V1 一致」这个前提**（改变量名、改默认值、改 allowlist），那是该次迁移自带的契约变更，需在其 PR 里同步 manifest，不属于本视图的遗留缺口 |
| **G5** | `enabled` 只反映环境变量意图，不代表 Hook 已在 Agent 进程加载 | V1 的 `capabilities` 从设计上就是 environment-only 视图，从不探测目标 Agent 进程；V2 照搬。若将来确实需要「已加载」证明，那是一个新增的运行期探测能力，不是改本视图语义 | 命令 `--help` 长文本已声明该边界 |
| **G6** | 非打印字符判定按 Unicode general category 复刻 Python `str.isprintable()`（拒绝 `Other` = `Cc`/`Cf`/`Cs`/`Co`/`Cn` 与 `Separator` = `Zs`/`Zl`/`Zp`，ASCII 空格除外），残余差异仅来自两侧 Unicode 数据版本不同 | 判定规则与 V1 一致，且复用 V1 prompt-scanner 已在用的 `unicode-properties`（同组件不引入第二个类别表实现）。该库 0.1.4 用 Unicode 17.0 数据，Python 3.11.6 的 `unicodedata` 是 14.0.0，因此「14.0 未分配、后续版本已分配」的码位上 V1 按 `Cn` 转义而 V2 按其真实类别放行（实测如 U+1E030、U+11F00）。这批码位是新分配的字母与符号，不含双向控制符或零宽字符，不构成视觉欺骗面 | 无。规则本身已收敛，版本偏移随两侧升级自然缩小；不要为此再手写码位表 |
| **G7** | V1 `CapabilityRecord` 的 `hooks`/`source`/`config`/`config_path` 字段未实现 | 这些字段在 V1 的 `to_dict()` 里本就不进入 JSON，也不进入表格，属于内部中间态；V2 不实现即为等价 | 无 |
| **G8** | timeout 采用 Rust 数字语义：`2_0`、全角 `２０` 等 Python 专有字面量判为非法值；小浮点按十进制输出（`1e-7` → `0.0000001`），V1 按 Python `repr` 输出 `1e-07` | 已接受的实现取舍：这些差异源自 Python `int()`/`float()` 与 `repr` 的语言特性，不是视图语义。V2 的行为是安全的——非法值回落到文档化默认值并给出可见诊断，不会被静默重新解释，也不会崩溃；`  20  `、`+20`、`1E5`、`0.5`、`3.0` 等常规形态两代一致 | 无。该取舍由 `resolve.rs` 的 `timeouts_reject_python_only_numeric_syntax` 与 `small_float_timeouts_are_reported_in_decimal_form` 两个单测钉住；这些取值**不得**加入两代共用的差分 e2e（会在其中一侧必然失败） |

## 4. 缺口识别方法

后续接手者按两层清单排查，不要只看其中一层：

1. **capability 级待迁移清单**：`Makefile` 中 `test-e2e-rpm-v2` 剩余的 `--ignore` 行及其
   pending 注释，一行对应一个尚未迁移的能力；能力迁移完成即删除对应行。
2. **capability 内部语义级待补清单**：本文第 3 节缺口表。对应条件满足时，在该能力的迁移 PR 中
   同时更新缺口表（补全后删除该行，并在 PR 描述里说明验证方式）。

两层清单并不重叠：`Makefile` 的清单只回答「哪个能力还没迁」，**不会**提示 G1、G4 这两个
「迁完仍需额外动手」的项；因此迁移 prompt-scan 与 skill-ledger 时，除了删 `--ignore` 行，
必须同时回看第 3 节。
