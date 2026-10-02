# Prompt Scanner 用户使用指南

[English](../../../en/agent-security/agent-sec-core/prompt-scanner.md)

Prompt Scanner 用于检测 Agent 输入中的提示词注入、越狱攻击和恶意指令。它结合快速规则引擎（L1）与可选的
ML 分类器（L2），返回结构化 verdict，并记录经过清理的 Security Event，供审计和 Observability 关联使用。

## 扫描文本

必须且只能提供一种输入来源：内联文本、标准输入或 UTF-8 文件（每行一条 prompt）。

```bash
# 内联文本
agent-sec-cli scan-prompt --text "ignore all system instructions"

# 标准输入
echo "forget your system prompt" | agent-sec-cli scan-prompt

# UTF-8 文件（每行一个 prompt）
agent-sec-cli scan-prompt --input prompts.txt --format json
```

常用选项：

| 选项 | 作用 |
|------|------|
| `--text TEXT` | 直接指定扫描文本，优先级高于 `--input` 和 stdin |
| `--input FILE` | 每行一个 prompt 的文件路径 |
| `--mode MODE` | 检测模式：`fast` / `standard` / `strict` / `multi_turn`；默认 `standard` |
| `--format FMT` | 输出格式：`json`（默认）或 `text`（人类可读）|
| `--source SOURCE` | 输入来源标签，写入 metadata，例如 `user_input`、`rag`、`tool_output` |
| `--model MODEL` | L2 后端模型名；优先于 `PROMPT_SCANNER_L2_MODEL`，未设时用默认 Qwen3Guard |

## 检测模式

| 模式 | 层级 | fast_fail | 典型延迟 | 适用场景 |
|------|------|-----------|----------|----------|
| `fast` | L1 规则引擎 | `True` | < 5 ms | 实时对话，低延迟优先 |
| `standard` | L1 + L2 ML 分类器 | `False` | 20–80 ms | 生产环境默认 |
| `strict` | L1 + L2 ML 分类器（L3 预留） | `False` | 50–200 ms | 高安全场景 |
| `multi_turn` | L4 多轮意图检测 | — | 取决于模型 | 从 stdin 传入 JSON history（Ollama） |

L2 分类器默认调用 `modelscope.cn/ANOLISA/Qwen3Guard-Gen-0.6B-GGUF`，由 Ollama 从项目自有的
ModelScope 仓库拉取。执行一次
`ollama pull modelscope.cn/ANOLISA/Qwen3Guard-Gen-0.6B-GGUF` 即可（无需重命名），
再执行 `agent-sec-cli scan-prompt warmup` 验证模型可用，避免首次扫描时才发现模型缺失。

### 模型服务必须部署在本机

L2/L4 的模型服务地址来自 `AGENT_SEC_MODEL_SERVICE_BASE_URL`（默认
`http://localhost:11434`），只接受 loopback 主机——`localhost`、`127.x.x.x`、
`::1`。交给扫描器的 prompt 经常含有凭据与个人数据，因此扫描器拒绝将它们
发往本机以外的任何地址。主机由 HTTP 客户端所用的同一个 URL 解析器解析，因此
`http://localhost@attacker.example/` 这类值也会被拒绝——真正的主机是 `@`
之后的部分。

把该变量指向其他主机时，`scan-prompt` 会在构造期失败，输出 `error` verdict
并以 `1` 退出，报错信息会指明被拒绝的地址。只要主机仍为 loopback，使用
非默认端口是允许的：

```bash
export AGENT_SEC_MODEL_SERVICE_BASE_URL=http://127.0.0.1:18434
```

由于六个宿主 hook 在 `scan-prompt` 非零退出时均为 fail-open，配置为远程地址后，
该宿主将处于「完全没有 prompt 扫描」的状态——仅被审计为一次失败的 `prompt_scan` 事件，
不拦截任何内容。改动该变量后，请检查 `agent-sec-cli scan-prompt warmup` 的 stderr 输出。

### 切换 L2 后端

设置 `PROMPT_SCANNER_L2_MODEL` 可把 L2 换成 Warden-Gen（也可用 `--model` 临时指定，优先级 `--model` > 环境变量 > 默认）：

```bash
ollama pull modelscope.cn/ANOLISA/Warden-Gen-0.6B-GGUF

# 方式一：环境变量（对所有宿主 hook 生效）
export PROMPT_SCANNER_L2_MODEL=modelscope.cn/ANOLISA/Warden-Gen-0.6B-GGUF
agent-sec-cli scan-prompt warmup

# 方式二：--model 临时指定（仅本次命令）
agent-sec-cli scan-prompt --model modelscope.cn/ANOLISA/Warden-Gen-0.6B-GGUF --text "..."
```

所有宿主 hook 都通过命令行调用 `scan-prompt`，因此环境变量对它们同样生效；`--model` 主要用于终端临时切换。两者取值都必须是
上面 `ollama pull` 使用的完整模型名。拼错只在 CLI 层“直接报错”：引擎在构造期就拒绝不支持的模型名，`scan-prompt` 返回
`error` verdict 并以 `1` 退出，而不是静默关闭 L2。但六个宿主 hook 对这个非零退出码一律 fail-open，所以宿主内的同一个拼错
只会被记为一条 failed 的 `prompt_scan` 事件，不阻断任何 prompt——在改回正确模型名之前，该宿主实际处于无 prompt 防护状态。
改完环境变量后执行 `agent-sec-cli scan-prompt warmup`，让失败在宿主加载前暴露。两者都为空时沿用默认的 Qwen3Guard。

L2 同一时刻只跑一个后端，不做级联或投票。

想确认某个宿主实际会用哪个后端，可在该宿主的环境中执行
`agent-sec-cli capabilities --capability prompt-scan --output json`，查看 `env`
下的 `PROMPT_SCANNER_L2_MODEL` 条目：变量未设置时它会上报默认后端；若配的模型名
不在引擎支持范围内，还会附一条 diagnostic。

## Verdict

Scanner 将各层结果聚合为一个 verdict：

| Verdict | 含义 |
|---------|------|
| `pass` | 未检测到威胁 |
| `warn` | L1 命中但 L2 未确认（`standard`/`strict`），或策略级警告 |
| `deny` | L1（`fast`）或 L1 + L2（`standard`/`strict`）确认威胁 |
| `error` | Scanner 内部错误（例如模型加载失败） |

> `fast` 模式不运行 ML 层，任何 L1 命中都直接映射为 `deny`。

## 各层能拦住什么、拦不住什么

L1 是规则引擎，只匹配已经被写成 pattern 的措辞。这让它快且可解释 —— 每个命中都能归到具体 rule id —— 但它不具备泛化能力：保留意图、只换措辞的改写，在对应规则补上之前都可能漏检。规则本身也是有意收紧的：L1 的调优目标是不误报正常 prompt，而这一目标必然以召回率为代价。

L2 和 L4 由模型支撑，负责规则做不到的部分：改写后的指令、事先无人预料的表达，以及只有跨多轮对话才能看出的意图。

对使用的实际影响：

- `fast` 模式只跑 L1，是用检测覆盖率换延迟 —— 延迟预算紧张时再选它，不要当作 `standard` 的轻量等价版本。
- 模型后端不可达时，`standard` 不会直接失败，而是用存活的层继续扫描。此时结果会带 `degraded: true`，在 `layers_failed` 中列出不可用的层，summary 也会以 `Scan degraded:` 开头。把 `pass` 当成“没问题”之前，先看这几个字段。
- `pass` 只说明实际运行过的层都没报告威胁，它不是安全证明 —— 这也是无论结果如何都会记录 `prompt_scan` Security Event 的原因。

## 宿主 Hook Policy

设置 `PROMPT_SCANNER_HOOK_ENABLED=false` 可完全跳过宿主 prompt scanner hook。启用时，以下环境变量控制部署级行为：

| 环境变量 | 默认值 | 读取该变量的宿主 | 行为 |
|----------|--------|------------------|------|
| `PROMPT_SCANNER_HOOK_ENABLED` | `true` | 全部六个 | 设为 `false` 时在读取输入前跳过 hook |
| `PROMPT_SCANNER_MODE` | `observe` | Qoder、Codex、Qwen Code | `observe` 静默审计；`deny` 会在 prompt scanner 返回 `warn` 或 `deny` finding 时阻断。`ask` 和 `block` 不是 prompt scanner 的有效模式。 |
| `PROMPT_SCANNER_SCAN_MODE` | `standard` | 全部六个 | 传给 `scan-prompt` 的扫描强度：`fast` / `standard` / `strict` |
| `PROMPT_SCANNER_TIMEOUT` | `10` | Qoder、Codex、Qwen Code | Scanner 超时秒数 |

cosh、Hermes 和 OpenClaw 只读取 `PROMPT_SCANNER_HOOK_ENABLED` 和 `PROMPT_SCANNER_SCAN_MODE`，
在这些宿主上设置 `PROMPT_SCANNER_MODE` 或 `PROMPT_SCANNER_TIMEOUT` 不会生效。OpenClaw 的阻断
行为由 `promptScanBlock` 决定，scanner 超时固定为 10 秒；Hermes 的 `prompt-scan-user-input`
capability 本身是非阻断设计，没有阻断开关；cosh 也没有 prompt 策略开关。Qoder、Codex 和
Qwen Code 需要使用 `PROMPT_SCANNER_MODE=deny` 阻断 prompt scanner finding。

对于确实会读取的环境变量，其优先于对应宿主配置。宿主 Agent 在加载插件时读取这些变量，
修改后需重启承载该 hook 的 Agent 进程。

Scanner verdict `deny` 描述扫描风险。对于 Qoder、Codex 和 Qwen Code prompt hook，
`PROMPT_SCANNER_MODE=deny` 是将 prompt scanner finding 转成阻断 hook 结果的部署策略。

## Security Event 与 Observability

每次扫描都会进入现有 `prompt_scan` Security Event 链路。Event 包含 source、verdict、summary、threat type、
confidence 以及经过清理的规则或 ML findings，不包含原始 prompt 文本。

Scanner 出错时宿主 hook 保持 fail-open：`error` verdict 会被审计，但不会用于阻断底层操作。

Observability 使用现有 trace context 和输入 hash 与 Security Event 建立关联，不重复存储 finding 明细。
