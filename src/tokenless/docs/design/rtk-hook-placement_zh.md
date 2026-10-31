# RTK Hook 接入位置评估

[English](rtk-hook-placement.md)

## 目的

记录"把 RTK 从 PreTool 命令重写迁移到 PostTool 输出过滤"的评估结论，并发布在重写仍是唯一交付方式期间、
宿主风险分类器需要遵守的包装形态契约。

Tokenless 在 PreTool hook 中接入 RTK：Core 先向 `rtk rewrite` 询问模型 shell 命令的 RTK 形态，
再为其套上带归属信息的包装（`crates/tokenless-runtime/src/entry.rs` 中的
`pre_tool_with_optional_rtk` 与 `anchor_rtk_prefix`）。因此，在 PreTool hook 之后才做命令风险分类的宿主看到的是

```
env TOKENLESS_AGENT_ID=… TOKENLESS_SESSION_ID=… TOKENLESS_TOOL_USE_ID=… TOKENLESS_DATA_DIR=… /usr/bin/rtk <payload>
```

而不是模型真正请求的命令。这个耦合已经让一个宿主失去只读命令免审批能力（#3415），
修复方式是在该宿主内部维护一张私有识别表（#3436），随后又通过把重写改为显式开启来缩小影响面（#3432）。
三者都没有消除耦合本身：任何带风险判定、审批或审计链路的宿主，仍然要识别 tokenless 的内部命令形态，
并且仍然要自行判断哪些 `TOKENLESS_*` 赋值可以忽略。

## 待评估的问题

RTK 能否迁移到 PostTool —— 原始命令原样执行，由 hook 在结果返回模型前完成压缩 ——
从而让任何宿主都不再需要识别包装形态？

**结论：不能作为替代方案。** Post-tool 过滤在无法替换实时工具输出的宿主上会损失全部 RTK 收益，
只能覆盖 RTK 当前重写命令的大约四分之一，在不知道命令请求了什么的情况下做固定策略过滤，
不记录任何统计，并且在其能覆盖的族上与原生 PostTool Pipeline 重复。因此改为采用下文的包装形态契约；
"审批后再重写"是唯一在结构上完整的方案，单独跟踪。

## RTK 当前的接入方式

三个事实构成评估前提。

1. **PreTool 掌握命令。** `rtk rewrite` 退出码 0 与 3 表示"重写"，1 与 2 表示"保持原样"。
   重写时 Core 替换工具参数，所以包装形态既是 shell 实际执行的内容，也是后续分类器看到的内容。
2. **PostTool 掌握输出，且 RTK 输出绕过它。** Adapter 按 Tool Call 记录
   `output_optimization: "rtk"`（`mark_rtk_optimized`），PostTool hook 消费该标记
   （`consume_output_optimization`），因此 RTK 产出的结果不会被二次压缩。
3. **构建与测试命令已经保留给 PostTool。** `is_build_log_owned_command` 对 cargo
   build/check/clippy/install/test、pytest、npm 与 pnpm 的 test/build/jest、npx jest、
   go build/test/vet 以及 make 直接返回 passthrough，这些输出由原生 `BuildLogCompressor`
   而不是 RTK 负责。

## 路线 A —— 把 RTK 迁到 PostTool（`rtk pipe`）

RTK 确实提供事后模式：`rtk pipe [-f <filter>]` 读取 stdin，应用一个具名过滤器并输出结果。
以下六项发现决定了该路线的结论。代码事实均取自 `scripts/setup-rtk.sh` 固定的 RTK v0.49.0。

### A1. 只有部分宿主能替换实时工具输出

PreTool 重写只需要一种能力 —— 替换参数 —— 所有带 `rewrite` hook 的宿主都具备。
PostTool 压缩则要求宿主能替换模型可见的结果：

| 宿主 | Post-tool 实时替换 | 机制 |
|------|-------------------|------|
| Cosh-NG | 支持 | `hookSpecificOutput.updatedToolResponse` |
| Claude Code ≥ 2.1.121 | 支持 | `hookSpecificOutput.updatedToolOutput` |
| Claude Code < 2.1.121 | 不支持 | 版本门禁 fail-open，压缩关闭 |
| Qoder CLI | 支持 | `updatedToolOutput`（字符串槽位） |
| OpenCode | 支持 | Adapter 映射到 `tool.execute.after` |
| Hermes | 支持 | `transform_tool_result` |
| QwenPaw | 支持 | 插件声明 `replace_output` |
| DSH | 部分支持 | `tools/post-execute`，仅一种可替换内容形态 |
| Codex | **不支持** | `PostToolUse` 拒绝抑制与替换输出 |
| OpenClaw | **不支持** | `tool_result_persist` 只改写持久化 transcript |
| Qwen Code | **不支持** | 共享 hook 走 `additionalContext`-only 分支 |

Codex 明确写出了这一约束（`adapters/tokenless/codex/README.md`）：其 PostToolUse 不能抑制或替换输出，
追加式注入会让原结果留在原地并放大 prompt —— 所以该 adapter 只提供 `rewrite` 加追加式
`response-diagnostics`。OpenClaw 的 persist hook 改写的是它自己存储的内容，而不是模型本轮已经收到的结果
（[响应压缩](../response-compression.md) 路径 1）。Qwen Code 走共享 hook，其能力分支把只支持
`additionalContext` 的宿主置为 `can_replace = False`
（`adapters/tokenless/common/hooks/compress_response_hook.py`）。

在这些宿主上，post-tool RTK 省下的 token 恰好为零，而 pre-tool 重写仍然有效。
所以路线 A 不是迁移，而是按宿主逐个丢功能。

### A2. `rtk pipe` 只覆盖 `rtk rewrite` 的一小部分

RTK v0.49.0 声明了 86 个 `Commands` 变体。其中 18 个是 RTK 自身的工具子命令、
hook 相关命令或明确不做过滤的透传命令（`init`、`gain`、`config`、`telemetry`、
`rewrite`、`hook`、`recall`、`pipe`、`run`、`proxy` 等），剩下 68 个是输出优化代理。
`pipe_cmd::resolve_filter` 只接受 27 个别名、覆盖 23 个输出族：
cargo-test、pytest、go-test、go-build、ctest、tsc、vitest、grep/rg、find/fd、
git-log、git-diff、git-status、log、mypy、ruff-check、ruff-format、sqlfluff-lint、
prettier、phpunit、pest/paratest/php-test、ecs、phpstan、pint。

对 `rtk rewrite` 的 37 条命令探测（见附录）中有 32 条存在 RTK 等价形态。
这 32 条里只有 12 条能被现有 pipe 过滤器事后服务，其中 3 条还是 PreTool 已经保留给原生
Pipeline 的构建/测试命令 —— 也就是说只有 9 条、约 28% 的重写面能在路线 A 下存活。
其余 23 条（72%）会失去全部 RTK 收益，其中包含探测中输出最大的命令：
`ps aux`（原始 105 638 B，经 `rtk ps aux` 后 2 616 B）没有过滤器，
`ls`、`tree`、`wc`、`git branch`、`docker`、`kubectl`、`curl`、`gh`、`jest`、`read` 同样没有。

### A3. pipe 过滤器不感知命令请求，部分还要求 RTK 自己插桩过的输入

执行路径会用 RTK 自己的参数重新执行命令；pipe 路径只看得到文本，因此无法尊重命令本身的请求：

- `git log`：执行路径注入
  `--pretty=format:%h %s (%ar) <%an>%n%b%n---END---`。`git_log_wrapper` 调用
  `filter_log_output(input, 50, false, false)` —— `user_format = false` ——
  于是过滤器按只有 RTK 自有格式才会产生的 `---END---` 标记切分。原生 `git log`
  输出没有该标记，整段日志被塌缩成一个 commit 块。对 `git log -20` 实测：
  执行模式返回全部 20 个 commit、5 952 B（比原始小 72%）；pipe 模式只返回 1 个 commit
  外加 `[+319 lines omitted]`、208 B。这 99% 的"节省"是被丢掉的答案，不是压缩。
- `git status`：执行路径运行 `git status --porcelain -b`，而 `format_status_output`
  解析的是 porcelain 格式（`## branch` 头、XY 状态码）。用原生人类可读输出喂同一个过滤器，
  结果基本原样返回：原始 286 B，经 `rtk pipe -f git-status` 后 283 B，而 `rtk git status` 是 101 B。
- `git diff`：`compact_diff(input, 200)` 使用固定上限，完全不考虑 `-U`、`--stat`、
  pathspec，也不考虑模型请求了多少 diff。

因此事后过滤可能比执行路径更有损，同时看起来更高效，而且这种损失对宿主不可见：
原生 Pipeline 至少会写 Stash 条目并留下 Retrieve Marker，`rtk pipe` 什么都不留。

### A4. pipe 模式不记录任何统计

tokenless 的统计集成是打在 RTK 执行跟踪上的补丁：
`third_party/patches/rtk-tokenless-stats.patch` 在 `impl TimedExecution` 内插入
`record_to_tokenless_stats(original_cmd, rtk_cmd, input, output)`。
而 `Commands::Pipe` 直接调用 `pipe_cmd::run`，从不构造 `TimedExecution`，
`pipe_cmd.rs` 中也没有任何跟踪调用。用打过补丁的 v0.49.0 构建、在
`TOKENLESS_STATS_ENABLED=1` 下实测：`rtk grep -rn 'fn ' crates` 会写入一行 `stats`
记录（`operation = rewrite-command`，带 agent、session、tool-use 归属以及压缩前后的字符数与 token 数）；
同样的输出经 `rtk pipe -f grep` 处理时，连 stats 数据库都不会创建。

所以在路线 A 下，除非 PostTool 自己重新推导，否则 RTK 收益会同时从 `rtk gain` 和
`tokenless stats` 中消失 —— 而 PostTool 推导不出来：它只看得到原始命令和至多一个过滤器名，
今天统计所依赖的 `original_cmd`/`rtk_cmd` 这一对数据没有来源。

### A5. 缓冲、上限与时序

`pipe_cmd::run` 把整个 stdin 读进一个 `String`，硬上限 `RAW_CAP = 10 MiB`，
超过上限直接报错退出。hook 只能把它变成 fail-open 透传，而且已经付掉了一次子进程和一次完整输出拷贝的代价；
执行路径则是在子进程输出的同时流式过滤。宿主自身也会在 hook 看到结果之前限制工具输出长度：
tokenless 自己的 Grep 文档就区分了"收到的全部行"与"宿主截断之前的全部可能匹配"
（[Runtime 设计](runtime-library_zh.md)），所以事后过滤只能压缩宿主限制之后剩下的部分，
而执行路径让大输出根本不产生。最后，PostTool hook 的预算是与原生 Pipeline 共享的 8 秒，
路线 A 会让每次 shell 调用都把其中一部分花在第二个压缩器上。

### A6. 有 pipe 过滤器的地方，原生 Pipeline 已经覆盖

这 23 个 pipe 族是构建/测试日志、grep/find 列表、diff 与 linter 输出 ——
正好是 Rust PostTool Pipeline 已在进程内实现的领域（`BuildLogCompressor`、
`SearchResultsCompressor`、`post_tool/diff.rs`），并且带有方言状态机、Stack Trace 保护、
可恢复的 Stash 缩减与 Retrieve Marker。对同一份输出再跑一次 `rtk pipe` 等于改写两遍，
而 `is_build_log_owned_command` 存在的意义就是防止这种情况。

### 路线 A 结论

作为 PreTool 重写的替代方案予以否决。评估中没有任何证据支持把 RTK 移出执行路径；
而在事后过滤确实能增加价值的那几个窄族上，原生 Pipeline 已经做得更好，而且是可恢复的。

## 路线 B —— 保留重写，发布包装形态契约（已采纳）

路线 B 接受"包装形态对宿主可见"这一事实，并消除各宿主必须逆向实现识别逻辑的理由：
该形态成为一份公开契约，包含文档化的文法、键语义、解包算法、一致性向量与版本规则（见下一节）。
遵守契约的宿主会对有效命令而不是包装形态做分类，也就不再需要维护私有识别表。

路线 B 不能让 tokenless 变得不可见，但它把包装形态从"每个消费者各自的猜测"变成"生产者的承诺"，
而这正是 #3436 为一个宿主不得不自行构建的东西。

## 路线 C —— 审批之后、在宿主执行接缝处重写（后续）

唯一能同时满足"分类器看到原始命令"和"RTK 保留执行路径收益"的设计，是把重写往后移：
宿主先对模型的命令做分类与审批，再在真正 spawn 进程的位置替换为 RTK 形态。
这需要每个宿主提供"审批后、执行前"的接缝，因此它是一个逐宿主的集成工作，而不是 tokenless 单侧的改动，
本文不做尝试。凡是宿主提供这种接缝，路线 C 都严格优于 A 与 B；凡是没有这种接缝，路线 B 是底线。

## RTK 包装形态契约 v1

**生产者。** Tokenless Core 的 `anchor_rtk_prefix`。只在同时携带
`action: replace_arguments` 或 `block_and_suggest` 与 `output_optimization: "rtk"` 的
PreTool 响应中产生。

**文法。**

```
wrapped_segment  := [transparent_prefix] wrapper payload
transparent_prefix := RTK 配置保留的用户前缀 token（例如 `sudo`）
wrapper          := "env" SP assignment SP assignment SP assignment SP assignment SP rtk_path
assignment       := key "=" quoted_value
key              := TOKENLESS_AGENT_ID | TOKENLESS_SESSION_ID
                  | TOKENLESS_TOOL_USE_ID | TOKENLESS_DATA_DIR
```

- 四个键总是全部出现，且顺序固定如上。
- `quoted_value` 在每个字节都是 ASCII 字母数字或 `/ _ - .` 之一时原样输出；
  否则用单引号包裹，并把其中每个 `'` 替换为 `'\''`（`shell_quote`）。
- `rtk_path` 是按同一规则引用的绝对路径，应视为不透明值：包安装时是 `/usr/bin/rtk`，
  其余情况是解析到的本地构建产物，路径中可能含空格。
- `payload` 是 RTK 自己产出的该 segment 形态，其中裸 `rtk` token 被替换为 `wrapper`。
  它以 RTK 子命令开头，而不是以 `rtk` 开头。
- **每个被重写的 segment 都带自己的 wrapper。** `a && b` 会变成 `wrapper a && wrapper b`；
  wrapper 也会出现在 `$(…)` 子 shell 内、`sudo` 之后以及用户自己的环境赋值之后。
  反引号与双引号内的命令替换保持原样，因为它们需要宿主解析器处理。
  因此"命令以 `env TOKENLESS_` 开头"**不是**充分的识别规则。

**消费者需要理解的键语义。**

| 键 | 含义 | 消费者规则 |
|----|------|-----------|
| `TOKENLESS_AGENT_ID` | 统计归属 | 风险分类时忽略 |
| `TOKENLESS_SESSION_ID` | 统计归属，可能为空 | 风险分类时忽略 |
| `TOKENLESS_TOOL_USE_ID` | 统计归属，可能为空 | 风险分类时忽略 |
| `TOKENLESS_DATA_DIR` | RTK 写统计所用的 tokenless 状态目录绝对路径 | 风险分类时忽略；绝不把它当作用户输入 |

**解包算法。**

1. 按 `&&`、`||`、`;`、`|`、换行以及 `$(…)` 边界把命令切成 segment ——
   与 `bare_rtk_offsets_by_segment` 使用的切分方式一致。
2. 在每个 segment 中跳过前导的环境赋值与一个前导 `env` token。当接下来的四个 token 恰好是
   契约中的四个键、按顺序、形如 `KEY=value`，且其后紧跟一个 basename 为 `rtk` 的路径时，
   该 segment 即为被包装。
3. 该 segment 的**有效命令**是这个 `rtk` 路径之后的全部 token。分类时要连同 wrapper 之前的
   `transparent_prefix` 一起考虑：`sudo` 加 payload 仍然是特权命令。
4. 只要有一个 segment 被包装，该命令就是 tokenless 包装命令；未匹配的 segment 保持原样。

**契约不承诺的内容。**

- 无法从包装形态还原模型原始文本。RTK 的重写本身就是 payload 的一部分
  （`cat README.md` → `read README.md`，`head -50 main.rs` → `read main.rs --max-lines 50`）。
  需要审计或展示"模型请求了什么"的宿主，必须在 PreTool 阶段自行记录。
- 包装形态不是来源证明。模型或用户完全可以自己敲出同样的字符串。
  解包用于*分类*是安全的 —— payload 就是实际会执行的内容 ——
  但宿主绝不能仅凭命令匹配包装形态就给予信任、跳过审批或放宽白名单。
  来源判定只能来自宿主自己的 hook 链路：它知道本次调用是否真的执行过 tokenless 的 rewrite hook。

**版本规则。** v1 就是今天 `anchor_rtk_prefix` 产出的形态，并由
`crates/tokenless-runtime/src/entry.rs` 中的测试固定
（`pre_tool_rtk_wrapper_matches_published_contract_v1` 与
`pre_tool_anchor_preserves_quoted_arguments_and_handles_subshells`）。
任何对键集合、键顺序、引用规则或切分方式的修改都会把契约升到 v2、同步更新本文，
并在一个发布周期内继续产出 v1 以便消费者迁移。消费者遇到未知形态时，
必须把整条命令视为不透明并按原样分类 —— fail closed，绝不猜测。

**一致性向量。**

| 包装后命令 | 有效命令 |
|-----------|---------|
| `env TOKENLESS_AGENT_ID=a TOKENLESS_SESSION_ID=s TOKENLESS_TOOL_USE_ID=t TOKENLESS_DATA_DIR=/d /usr/bin/rtk git status` | `git status` |
| `env … /usr/bin/rtk grep -E 'foo \| rtk bar' src && env … /usr/bin/rtk git status` | `grep -E 'foo \| rtk bar' src`、`git status` |
| `echo $(env … /usr/bin/rtk git status)` | `echo $(…)`，其内部有效命令为 `git status` |
| `sudo env … /usr/bin/rtk git status` | `sudo git status`（特权） |
| `RUST_BACKTRACE=1 env … /usr/bin/rtk cargo test` | `RUST_BACKTRACE=1 cargo test` |
| `env TOKENLESS_AGENT_ID='a b' … /opt/my tools/rtk ps aux` | `ps aux`（带引号的值与含空格的路径） |
| `env TOKENLESS_AGENT_ID=a … /usr/bin/rtk read README.md` | `read README.md` —— 原始的 `cat README.md` **无法**还原 |

## 后续工作

1. 把该契约发布给各宿主负责人，用它替代私有识别表；#3436 在 Cosh-NG 中新增的识别表是第一个候选
   （属于另一处改动，不在本组件范围内）。
2. 需要审计保真度的宿主集成应在 PreTool 阶段记录模型的原始命令，因为包装形态无法携带它。
3. 对暴露"审批后执行接缝"的宿主逐个调研路线 C。
4. 若 RTK 将来提供感知命令请求的事后过滤器（过滤器能拿到原始命令行而不只是其输出），
   或 Codex 与 OpenClaw 支持实时输出替换，则重新执行本评估。

## 附录 —— 复现方式

覆盖率与保真度结论来自固定的 RTK 源码（执行 `bash scripts/setup-rtk.sh` 后阅读
`src/cmds/system/pipe_cmd.rs`、`src/cmds/git/git.rs`、`src/hooks/rewrite_cmd.rs`、
`src/core/stream.rs`）以及 tokenless 统计补丁
（`third_party/patches/rtk-tokenless-stats.patch`）。

体积测量在本组件源码树中进行，使用 `scripts/setup-rtk.sh` 构建出的 RTK v0.49.0，
输入是同一棵源码树，字节数取自 `wc -c`：

```bash
raw=$(git log -20 2>&1 | wc -c)
exec=$(third_party/rtk/target/release/rtk git log -20 2>&1 | wc -c)
pipe=$(git log -20 2>&1 | third_party/rtk/target/release/rtk pipe -f git-log | wc -c)
```

| 命令 | 原始 B | `rtk <cmd>` B | `rtk pipe -f` B | 过滤器 |
|------|--------|---------------|-----------------|--------|
| `git log -20` | 21 212 | 5 952 | 208 | `git-log` |
| `git diff HEAD~3` | 64 649 | 23 186 | 8 413 | `git-diff` |
| `git status` | 286 | 101 | 283 | `git-status` |
| `git branch -a` | 6 022 | 401 | — | 无 |
| `grep -rn 'fn ' crates` | 154 280 | 19 337 | 37 306 | `grep` |
| `find . -name '*.rs'` | 12 989 | 1 314 | 1 986 | `find` |
| `ls -al crates` | 574 | 162 | — | 无 |
| `wc -l entry.rs lib.rs` | 101 | 34 | — | 无 |
| `cat entry.rs` | 71 090 | 71 090 | — | 无 |
| `ps aux` | 105 638 | 2 616 | — | 无 |

`—` 表示 `rtk pipe` 没有对应过滤器，post-tool 路线会原样返回输出。
`cat entry.rs` 说明执行路径同样会透传源码：RTK 的收益是命令相关的，并非普适。

37 条命令的 `rtk rewrite` 探测逐条执行 `rtk rewrite` 并记录退出码与重写结果：
32 条被重写（退出码 3），5 条没有 RTK 等价形态（退出码 1：`git diff`、`git show HEAD`、
`npm test`、`env`、`jq . package.json`）。这 32 条中 12 条能映射到现有 pipe 过滤器，20 条不能。

保真度验证（失败案例）：`rtk git log -20` 返回全部 20 个 commit，
而 `git log -20 | rtk pipe -f git-log` 只返回 1 个 commit，后跟 `[+319 lines omitted]`。
