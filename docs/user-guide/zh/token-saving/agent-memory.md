# Agent 记忆（agent-memory）

[English](../../en/token-saving/agent-memory.md)

agent-memory 是 ANOLISA 的文件形态记忆 MCP 服务器，为 AI Agent 提供持久化、可搜索、受沙箱保护的记忆空间。Agent 像操作文件系统一样读写记忆，系统通过 BM25/向量混合检索、自动捕获与召回机制，把相关上下文注入后续对话，从而减少重复沟通并提升任务连贯性。

- **文件形态记忆**：通过 MCP 工具以文件系统语义读写记忆，支持命名空间隔离和路径沙箱。
- **混合语义搜索**：BM25 + 稠密向量 + RRF 融合，支持自动 fallback，召回相关记忆片段。
- **自动捕获与召回**：在对话结束时自动提取观察并去重，在下一轮构建提示时注入相关记忆。
- **安全注入机制**：对注入 LLM 提示的记忆内容做提示注入检测和转义包装，降低攻击面。
- **版本化与快照**：可选 git 自动提交 + tar.gz 快照，提供文件级与 mount 级回滚。

需要跨会话保留上下文时使用 Agent Memory。Tokenless 解决的是另一类问题：压缩
仍需进入当前上下文窗口的内容。

---

## 运行要求

- Linux x86_64 或 aarch64
- 支持 stdio MCP 服务器的 Agent 运行时

---

## 安装

### 通过 anolisa CLI（推荐）

```bash
anolisa install agent-memory
```

安装产物：`agent-memory` 二进制、默认配置、MCP 服务描述符、systemd user 模板、tmpfiles 规则、OpenClaw 适配器 bundle。

### RPM 包（AnolisOS / RHEL 系）

```bash
sudo yum install agent-memory
```

RPM 安装到系统级 FHS 路径：

| 用途 | 路径 |
|------|------|
| 服务二进制 | `/usr/bin/agent-memory` |
| 默认配置 | `/usr/share/anolisa/agent-memory/default.toml` |
| MCP 服务描述符（自动发现） | `/usr/share/anolisa/mcp-servers/agent-memory.json` |
| systemd user 模板 | `/usr/lib/systemd/user/anolisa-memory@.service` |
| tmpfiles 规则（创建 `/run/anolisa/{,sessions}`） | `/usr/lib/tmpfiles.d/anolisa-memory.conf` |
| OpenClaw 适配器 bundle | `/usr/share/anolisa/adapters/agent-memory/` |
| 文档 | `/usr/share/doc/agent-memory/` |

### 源码构建（开发者）

```bash
git clone https://github.com/alibaba/anolisa.git
cd anolisa/src/agent-memory

make build         # cargo build --release --locked
sudo make install  # 安装到 /usr/local 下
```

构建依赖：Rust ≥ 1.85（edition 2024；CI 钉到 1.89 与 monorepo 共享 toolchain）、cmake（libgit2 vendored 构建）、systemd-devel（journald 审计 fan-out）。

### 跨平台开发

运行时仅支持 Linux（依赖 user_namespace、mount(2)、cgroup v2、inotify、journald）。macOS / Windows 请用远端构建：

```bash
make remote-build   # push 分支并 ssh 到 Linux 主机执行 cargo build
make remote-test    # 同上 + 跑测试 + clippy
```

---

## 集成配置

### Claude Code / Cursor / Continue / 任意 stdio MCP 客户端

在 MCP 配置中添加：

```json
{
  "mcpServers": {
    "agent-memory": {
      "command": "/usr/bin/agent-memory",
      "args": [],
      "env": {
        "USER_ID": "alice",
        "MEMORY_PROFILE": "advanced"
      }
    }
  }
}
```

`/usr/share/anolisa/mcp-servers/agent-memory.json` 描述符列出全部 37 个工具名，支持自动发现的客户端可直接识别。

### OpenClaw

随包附带的插件把 4 个 memory contract 工具（`memory_search`、`memory_get`、`memory_observe`、`memory_get_context`）转发到 agent-memory：

```bash
bash /usr/share/anolisa/adapters/agent-memory/openclaw/scripts/install.sh
openclaw gateway restart
```

或通过 anolisa 适配器管理：

```bash
anolisa adapter enable agent-memory openclaw
anolisa adapter status agent-memory
```

**前置条件**：`openclaw` CLI 在 `$PATH` 上。脚本缺失时输出明确日志并以 0 退出，安装 OpenClaw 后重跑即可。`yum remove agent-memory` 时 spec 的 `%preun` 自动调用 uninstall 脚本，配置不残留孤立项。

执行 `anolisa adapter enable agent-memory openclaw` 或 agent-memory 的 OpenClaw `install.sh` 即同意插件声明的能力。两个入口仅在 `plugins install --help` 列出完整的 `--accept-capabilities` 参数时传递它，以兼容旧版宿主。运行 `install.sh` 时设置 `AGENT_MEMORY_ACCEPT_CAPABILITIES=0` 可拒绝授予同意——带门禁的宿主将拒绝安装，直至自行授予（例如交互式执行 `openclaw plugins install`）。

安装期环境变量（运行期 `MEMORY_*` 变量见「环境变量」一节）：

| 变量 | 默认 | 作用 |
|---|---|---|
| `AGENT_MEMORY_ACCEPT_CAPABILITIES` | 接受 | `1`/`true`/`yes`/`on` 在宿主声明该参数时授予同意；`0`/`false`/`no`/`off` 拒绝授予，带门禁的宿主将拒绝安装；其他取值在安装前直接报错中止（退出码 2） |
| `AGENT_MEMORY_SAFE_INSTALL` | 未设置 | `1` 时拒绝 `--dangerously-force-unsafe-install`（只对仍会传递它的宿主有意义）；把它标注为 deprecated no-op 的宿主无论如何都不会收到该参数 |
| `OPENCLAW_BIN` | `openclaw` | 要调用的 openclaw CLI |
| `OPENCLAW_STATE_DIR` | `~/.openclaw` | 传递给每次 openclaw CLI 调用的 state 目录 |
| `OPENCLAW_HOME` | `~/.openclaw` | 仅作为 `OPENCLAW_STATE_DIR` 的默认值；不会传给 CLI（每次调用均 unset） |

独立的 `install.sh` 用同样的方式协商 unsafe-install 覆盖参数：只有安装器仍声明 `--dangerously-force-unsafe-install` 有效时才传递它。OpenClaw 2026.6.1 及更早版本会在安装期执行安全扫描，非交互场景下拦截使用 `child_process` 的插件（本插件通过 stdio 拉起 agent-memory MCP Server），这类宿主会收到该覆盖参数。OpenClaw 2026.6.5 及之后版本已移除安装期危险代码拦截，把该参数标注为 deprecated no-op，因此不会收到它——此时安装期安全由运维自有的 `security.installPolicy` 决定，任何脚本参数都无法覆盖。`AGENT_MEMORY_SAFE_INSTALL=1` 在该参数仍有效的宿主上拒绝这一覆盖，在当前宿主上不产生任何差别；安装日志会说明命中的是哪一种情况。如果 `plugins install --help` 探测本身失败，宿主就无法分类：脚本会保留该覆盖参数以便 2026.6.2 之前的宿主仍能安装，同时打印 WARNING，此时同样可以用 `AGENT_MEMORY_SAFE_INSTALL=1` 拒绝它。

安装失败时，脚本只报告它自己能核实的部分。`${OPENCLAW_STATE_DIR}/extensions` 不可写会被点名为足以独立导致安装失败的文件系统权限问题——应修目录权限，不要为此去动安全策略。其余情况以脚本提示上方的 `openclaw` 输出为准，`security.installPolicy` 只作为供运维在该输出中确认的条件句出现，绝不会被断言为失败原因：「宿主把该参数标注为 deprecated no-op」本身并不能说明安装为何失败。

`install.sh` 还会执行 `openclaw plugins disable memory-core`。即使别的插件已经占据 `memory` slot，OpenClaw 仍会把内置的 `memory-core` 插件作为记忆整理（dreaming）sidecar 保留加载，而 `memory_get` / `memory_search` 这两个工具名归它所有。OpenClaw 的插件工具注册表是「先到先得」：名字已被占用的插件工具会被丢弃，只在 gateway 日志里留下 `plugin tool name conflict (memory-anolisa): memory_get`，永远不会进入 agent 工具集；于是 `memory_get` 绑定到 `memory-core` 的 workspace 文件读取器，对 `~/.anolisa/memory` 下的任何路径都返回 `disabled: true`——哪怕文件确实在磁盘上（#3218）。禁用 `memory-core` 才能释放这两个名字，而 memory slot 本来就已经把记忆流量交给本插件。本插件持有 memory 期间有两个后果：`openclaw memory` 子命令与 `MEMORY.md` 的 dreaming/整理都来自 `memory-core`，在卸载前不可用。如果禁用这一步本身失败，`install.sh` 只打印 WARNING 并仍以 0 退出——`memory_observe`、auto-recall、auto-capture 照常工作，但 `memory_get` / `memory_search` 会一直是坏的，直到你自己执行 `openclaw plugins disable memory-core` 并重启 gateway。

只有这个入口会做上述交接。`anolisa adapter enable agent-memory openclaw` 走的是 anolisa 内置的 OpenClaw driver：它自己执行 `openclaw plugins install`，从不调用适配器的 `install.sh`，因此这条路径上 `memory-core` 仍处于加载状态，`memory_get` 会继续返回 `disabled: true`——而 `adapter enable` 依旧报告成功，因为 `memory-anolisa` 自身确实已加载，那条路径上没有任何环节会观察到工具名冲突。通过适配器管理器启用后，请自行执行 `openclaw plugins disable memory-core` 与 `openclaw gateway restart`；同时注意 `anolisa adapter disable` 也不会替你把 `memory-core` 重新启用。把这次交接纳入 driver 的 receipt 生命周期由 #3225 跟踪。

`plugins disable` 是幂等的——无论 `memory-core` 原本是否启用都以 0 退出——所以 `install.sh` 会先读取 `plugins.entries.memory-core.enabled`，只有在本次调用确实造成了状态变化时，才把这件事记录到 `${OPENCLAW_STATE_DIR}/.anolisa-memory-anolisa-disabled-memory-core`。`uninstall.sh` 只依据这个记录重新启用 `memory-core`，因此你自己手动禁用的 `memory-core` 不会被改动。当探测无法给出答案时——宿主没有 `config get`，或内置默认值从未被写入过——记录仍会写下，安装日志会说明这一点。这个偏向是刻意的：不写记录会让宿主在卸载后彻底没有任何 memory 插件，因为 `plugins uninstall` 会把 memory slot 重置回它的 `memory-core` 默认值，而 config 里仍写着 `enabled=false`；相反，一次多余的恢复只花费一条 `openclaw plugins disable memory-core`。如果你正处在这种情况下并希望 `memory-core` 保持关闭，删除该记录文件即可。

记录并不是恢复的唯一条件。`uninstall.sh` 在重新启用 `memory-core` 之前，还会读取 `plugins.slots.memory` 当前归属于谁——先用 `openclaw config get`，CLI 答不上来再退回 `openclaw.json`——因为 `plugins enable` 会连带重跑 OpenClaw 的排他 slot 选择。如果你在安装之后把 memory slot 换给了别的后端（`openclaw plugins enable memory-lancedb`，或直接修改 `plugins.slots.memory`），卸载会把你选的后端留在 slot 里、让 `memory-core` 继续处于禁用状态，并在输出中说明；想把它换回来就自己执行 `openclaw plugins enable memory-core`，然后删除记录文件。只有被明确识别出的第三方归属才会阻止恢复——当 slot 属于本插件、属于 `memory-core`，或根本读不到时，`uninstall.sh` 仍照旧恢复，因为在那几种情况下跳过恢复，才会让宿主在卸载后彻底没有任何 memory 后端。

插件 contract 名 ↔ agent-memory MCP 工具映射：

| OpenClaw contract | agent-memory MCP 工具 |
|---|---|
| `memory_search` | `memory_search`（BM25 默认；配置 embedding 后支持 `mode=vector\|hybrid`） |
| `memory_get` | `mem_read` |
| `memory_observe` | `memory_observe` |
| `memory_get_context` | `memory_get_context` |

插件配置（通过 OpenClaw UI 或 `openclaw.json` 的 `plugins.entries["memory-anolisa"].config`）：

| 键 | 默认 | 作用 |
|---|---|---|
| `binaryPath` | 自动发现：`$PATH` → `/usr/bin/agent-memory` → `/usr/local/bin/agent-memory` → `~/.local/bin/agent-memory` | 二进制绝对路径 |
| `userId` | env `USER_ID` → OS `uid` → env `$USER` | 命名空间 `user_id`；校验规则与 Rust 侧一致 |
| `profile` | `advanced` | profile 门控，以 `MEMORY_PROFILE` env 启动子进程 |
| `maxReadBytes` | `1048576`（1 MiB） | 单次 `mem_read` 上限，以 `MEMORY_MAX_READ_BYTES` env 传入 |
| `maxWriteBytes` | `16777216`（16 MiB） | 单次 `mem_write` 上限，以 `MEMORY_MAX_WRITE_BYTES` env 传入 |
| `sessionId` | env `MEMORY_SESSION_ID` → 新生成 `ses_<random>` | 命名空间挂载会话，必须固定 |
| `sessionDir` | env `MEMORY_SESSION_DIR` → `/run/anolisa/sessions` | session scratch + log 根目录 |

插件给子进程传最小 env allowlist（`PATH`、`HOME`、`USER`、`USER_ID`、`LANG`/`LC_ALL`/`LC_CTYPE`、`TZ`、`TMPDIR`、`XDG_RUNTIME_DIR` 及所有 `MEMORY_`/`RUST_` 前缀变量），其它 env 不泄漏。`USER_ID` 精确匹配，`USER_IDX` 等前缀变量不放行。

---

## MCP 工具集（37 个）

所有工具通过 MCP `tools/call` 调用，参数为 JSON 对象。错误以 `CallToolResult { isError: true }` 返回，客户端可据此区分"成功但内容含 failed 字面"与真实错误。Profile 在 `tools/list` 与 `tools/call` 两层校验。

### Tier A — 文件操作（11 个）

| 工具 | 必填 | 可选 | 返回 |
|------|------|------|------|
| `mem_read` | `path` | — | UTF-8 文件内容 |
| `mem_write` | `path`、`content` | `overwrite` | `wrote N bytes to <path>` |
| `mem_append` | `path`、`content` | — | `appended N bytes to <path>` |
| `mem_edit` | `path`、`old_str`、`new_str` | — | `edited <path>`（`old_str` 须唯一命中） |
| `mem_list` | — | `dir`、`recursive`、`glob` | `{name, type, size, mtime}` 数组 |
| `mem_grep` | `pattern` | `dir`、`type`、`max`、`case_insensitive` | `{path, line, text}` 数组 |
| `mem_diff` | `path1`、`path2` | — | unified diff |
| `mem_mkdir` | `path` | — | `created <path>` |
| `mem_remove` | `path` | `recursive` | `removed <path>` |
| `mem_promote` | `session_path`、`store_path` | — | 把会话 scratch 文件原子移入持久化仓 |
| `mem_session_log` | — | — | 当前会话 JSONL |

### Tier B — 结构化检索（6 个）

| 工具 | 必填 | 可选 | 返回 |
|------|------|------|------|
| `memory_search` | `query` | `top_k`（默认 5）、`mode`（bm25/vector/hybrid）、`category` | `{path, score, snippet, suspicious}` 数组 |
| `memory_observe` | `content` | `hint`、`type` | `observed at notes/observed/<ulid>.md` |
| `memory_get_context` | — | `max_tokens`（默认 2048） | 最近修改文件的 markdown 预览，每条含 `suspicious` |
| `memory_sessions` | — | `limit`（默认 10） | 历史会话列表 |
| `memory_timeline` | `session_id` | `limit`（默认 50） | 指定会话的工具调用时间线 |
| `mem_index_refresh` | — | — | 强制重建 FTS5 索引 |

### Tier C — 治理与版本（7 个）

| 工具 | 必填 | 可选 | 返回 |
|------|------|------|------|
| `mem_snapshot` | — | `name` | `{id, name, created_at, size, backend}` |
| `mem_snapshot_list` | — | — | 按 `created_at` 升序数组 |
| `mem_snapshot_restore` | `id` | — | `restored <id>` |
| `mem_log` | — | `limit`（默认 20）、`path` | `{hash, summary, author, time}` 数组（需启用 git） |
| `mem_revert` | `path` | — | `reverted <path> (commit <hash>)`（需启用 git） |
| `mem_consolidate` | — | — | `consolidation complete: N facts written` |
| `mem_compact` | — | — | `compacted N files to cold storage` |

### 主权与导入导出（13 个）

| 工具 | 必填 | 可选 | 返回/说明 |
|------|------|------|------|
| `memory_about` | `topic` | `limit`（默认 10） | 按 topic 检索匹配记忆路径与 snippet |
| `memory_auto_created` | — | `limit`（默认 20） | 自动提取事实列表（JSON 数组） |
| `memory_consent` | — | `action`（query/allow/deny）、`scope`（all/consolidation/capture） | 同意/撤回记忆操作 |
| `memory_forget` | `topic` | `confirm`（默认 `false`=预览，`true`=删除） | 删除指定 topic 的记忆条目 |
| `mem_export` | — | `category`、`source` | 导出记忆仓为 AMA JSON 字符串（不写文件） |
| `mem_import` | `json_data` | `strategy`（skip-existing/overwrite，默认 skip-existing）、`dry_run`（默认 false） | 从 AMA JSON 字符串导入记忆 |
| `memory_task_save` | `title` | `status`、`progress`、`next_steps`、`blockers`、`files_modified`、`decisions`、`context`、`id` | 保存/更新任务，返回 task id（传 `id` 更新已有任务） |
| `memory_task_list` | — | `status`（in-progress/blocked/done/cancelled） | 任务摘要 JSON 数组 |
| `memory_task_resume` | `id` | — | 恢复任务上下文（格式化为新会话续作用） |
| `memory_task_close` | `id` | `reason` | 关闭任务（标记 done） |
| `memory_summary` | — | `recent_limit`（默认 10） | 记忆仓统计概览 JSON |
| `memory_session_context` | — | `limit` | 会话启动上下文注入 |
| `mem_dream` | — | — | 用户画像合成 JSON |

### 错误码语义

| MCP 错误码 | 含义 |
|------------|------|
| `-32601` METHOD_NOT_FOUND | 当前 profile 隐藏了该工具 |
| `-32602` INVALID_PARAMS | 缺参或类型错 |
| `-32603` INTERNAL_ERROR | 服务端故障 |
| `isError: true` | 工具运行了但返回业务错误（路径不存在、被沙箱拒绝、大小超限等） |

---

## 核心特性

### 文件形态记忆

Agent 用路径组织记忆，与人类文件系统模型一致：

```
notes/day1.md
decisions/2026-05/db-pick.md
context/project-overview.md
```

命名空间内的目录结构：

```
~/.anolisa/memory/user-<uid>/        # mount root
├── README.md                        # 自动生成的概览
├── notes/                           # 自由形态笔记
├── decisions/                       # 用户自定义子目录
└── .anolisa/                        # OS 管理，Agent 不可写
    ├── manifest.toml                # 命名空间元数据
    ├── audit.log                    # JSONL 工具调用审计
    ├── index.db                     # FTS5 SQLite
    ├── snapshots/                   # tar.gz 归档 + sidecar
    ├── trash/                       # restore 时保留的旧条目
    └── git/                         # bare git 镜像（启用 git 后才有）
```

会话目录（tmpfs，权限 0700）：

```
/run/anolisa/sessions/<sid>/
├── meta.toml
├── log.jsonl
└── scratch/                         # 仅会话内草稿，通过 mem_promote 持久化
```

### 沙箱保护

每次文件打开通过内核级 `openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS)` 锚定 mount root：

- 拒绝 `..` 路径穿越
- 拒绝符号链接（含调用中途的 symlink 替换；递归删除用 `fdopendir` + `fstatat(AT_SYMLINK_NOFOLLOW)` + `unlinkat`，让 swap 无法 race）
- 拒绝访问元数据目录（`.anolisa`、`.git`、`.gitignore` 由 `TargetIsReserved` 拒绝）
- `mem_snapshot_restore` 中 tar entry-type 过滤拒绝 `Symlink`/`Hardlink`/`Device`/`Fifo`
- payload 超大按 `max_*_bytes` 配置拒绝

**Mount 策略**：

| 策略 | 适用 | 行为 |
|------|------|------|
| `userland`（默认） | 任意环境 | mount 仅普通目录，沙箱由 `openat2` 强制 |
| `userns` | Linux ≥ 4.6 且允许 unprivileged user namespace | `unshare` 进入新 user+mount namespace，挂私有 tmpfs 再 bind-mount backing 目录；宿主侧进程看不到 `/mnt/memory/<ns>/` |
| `auto` | 运行时探测 | 先尝试 `userns`，任何错误回退 `userland` |

### 版本控制

可选自动 Git 提交（libgit2 vendored）：

```bash
MEMORY_GIT_ENABLED=true MEMORY_GIT_AUTO_COMMIT=true agent-memory
```

启用后 `mem_log` 暴露变更历史，配合 `mem_revert` 给 Agent 一个真正的"撤销"按钮。`mem_snapshot*` 提供 mount 范围的 tar.gz 时间点备份，独立于 git。

### 全文搜索

SQLite FTS5 BM25 索引，亚毫秒级查询。后台 tokio 任务通过 `inotify` 监听 mount，事件经 200 ms debounce 聚合后在单事务中应用。分词器用 `trigram`（对 ≥3 字符的子串匹配友好）。trigram 分词器以 3 字符滑窗生成 token，因此少于 3 字符的查询词（如中文的"花名""小云"）不产生任何 token，本会静默命中为空；`memory_search` 会识别这种情况并回退到 `body LIKE '%term%'` 子串扫描，确保短 CJK 查询仍能召回。`IN_Q_OVERFLOW` 时自动触发全量 rescan，不静默丢事件。

### 混合向量搜索

BM25 + 稠密向量混合检索，通过 RRF（Reciprocal Rank Fusion, k=60）融合排序。向量由可插拔 Embedding Provider 生成：

| Provider | 配置方式 | 说明 |
|----------|---------|------|
| OpenAI | `MEMORY_EMBEDDING_BACKEND=openai` + `OPENAI_API_KEY` | 调用 OpenAI Embeddings API |
| Ollama | `MEMORY_EMBEDDING_BACKEND=ollama` + `OLLAMA_BASE_URL` | 本地 Ollama 实例 |

`memory_search` 支持 `mode` 参数：`bm25`（默认）/ `vector`（余弦相似度）/ `hybrid`（RRF 融合）。未配置 embedding 时 `vector`/`hybrid` 自动降级为 BM25，不报错。

### 自动 Consolidation

服务关闭时自动从会话审计日志中提取原子事实（`mem_consolidate`），使用 6 条启发式规则（零 LLM 调用）识别高频路径、搜索模式等行为特征并持久化为结构化记忆。也可通过 `mem_consolidate` 工具手动触发。含情景记忆提取与冲突检测（BM25 阈值）。

### 审计与可观测性

每次成功工具调用向 `<mount>/.anolisa/audit.log` 追加 JSONL，启用会话还写入 `/run/anolisa/sessions/<sid>/log.jsonl`。`audit.journald=true` 时 fan-out 到 systemd-journald，带结构化字段（`MESSAGE_ID`、`AGENT_MEMORY_TOOL` 等），便于 `journalctl --user-unit=anolisa-memory@<user>` 过滤。

---

## 配置

### 配置文件

默认位置：`~/.anolisa/memory.toml`。该文件可选；文件不存在时，Agent Memory
使用内置默认值。所有 struct 启用 `serde(deny_unknown_fields)`，配置项拼写错误
会直接导致加载失败。最小配置：

```toml
[global]
user_id = "alice"

[memory]
profile = "advanced"           # basic | advanced | expert
max_read_bytes = 1048576       # 1 MiB
max_write_bytes = 16777216     # 16 MiB
max_append_bytes = 4194304     # 4 MiB

[memory.paths]
base_dir = "~/.anolisa/memory"

[memory.session]
base_dir = "/run/anolisa/sessions"
end_action = "discard"         # discard | keep

[memory.mount]
strategy = "auto"              # auto | userland | userns

[memory.index]
enabled = true
time_decay_lambda = 0.01
time_decay_alpha = 0.3
cold_after_days = 30
exclude_cold_on_search = true

[memory.audit]
journald = false

[memory.cgroup]
enabled = false
memory_max = "512M"

[memory.git]
enabled = false
auto_commit = true

[memory.consolidation]
enabled = true
max_facts = 20
min_tool_calls = 3
episodic_enabled = true
min_episode_steps = 3
max_episodes_per_session = 10
conflict_detection = true
conflict_bm25_threshold = -2.0
```

### 环境变量

每个配置项都有对应 `MEMORY_*` 环境变量，优先级：**env > config.toml > default**。

| 环境变量 | 说明 | 默认 |
|----------|------|------|
| `USER_ID` | 用户标识（校验；非法值 warn 后忽略） | — |
| `MEMORY_PROFILE` | 配置档位（basic/advanced/expert） | advanced |
| `MEMORY_BASE_DIR` | 记忆仓根目录 | `~/.anolisa/memory` |
| `MEMORY_SESSION_DIR` | 会话根目录 | `/run/anolisa/sessions` |
| `MEMORY_SESSION_ID` | 固定当前会话 id（`mem_promote` 必须设） | 新生成 `ses_<random>` |
| `MEMORY_SESSION_END` | 会话结束动作（discard/keep） | discard |
| `MEMORY_MOUNT_STRATEGY` | mount 策略（auto/userland/userns） | auto |
| `MEMORY_MAX_READ_BYTES` | 单次读取上限 | 1 MiB |
| `MEMORY_MAX_WRITE_BYTES` | 单次写入上限 | 16 MiB |
| `MEMORY_MAX_APPEND_BYTES` | 单次追加上限 | 4 MiB |
| `MEMORY_INDEX_ENABLED` | 启用 FTS5 索引 | true |
| `MEMORY_INDEX_TIME_DECAY_LAMBDA` | 时间衰减系数（≥0） | 0.01 |
| `MEMORY_INDEX_TIME_DECAY_ALPHA` | 时间权重占比（0–1） | 0.3 |
| `MEMORY_INDEX_COLD_AFTER_DAYS` | 冷数据归档天数 | 30 |
| `MEMORY_INDEX_EXCLUDE_COLD` | 搜索排除冷数据 | true |
| `MEMORY_AUDIT_JOURNALD` | fan-out 到 journald | false |
| `MEMORY_CGROUP_ENABLED` | 启用 cgroup 限制 | false |
| `MEMORY_CGROUP_MEMORY_MAX` | cgroup 内存上限 | 512M |
| `MEMORY_GIT_ENABLED` | 启用 git 版本控制 | false |
| `MEMORY_GIT_AUTO_COMMIT` | 自动提交 | true |
| `MEMORY_EMBEDDING_BACKEND` | embedding 后端（none/openai/ollama） | none |
| `MEMORY_OPENAI_API_KEY` | OpenAI API key（空时回退 `OPENAI_API_KEY`） | — |
| `MEMORY_OPENAI_MODEL` | OpenAI embedding 模型 | text-embedding-3-small |
| `MEMORY_OLLAMA_MODEL` | Ollama embedding 模型 | nomic-embed-text |
| `MEMORY_OLLAMA_BASE_URL` | Ollama base URL | http://localhost:11434 |
| `MEMORY_CONSOLIDATION_ENABLED` | 启用自动 consolidation | true |
| `MEMORY_CONSOLIDATION_MAX_FACTS` | 每次最多提取事实数 | 20 |
| `MEMORY_CONSOLIDATION_MIN_CALLS` | 最少调用次数门槛 | 3 |
| `MEMORY_EPISODIC_ENABLED` | 情景记忆提取 | true |
| `MEMORY_MIN_EPISODE_STEPS` | 情景最少步骤数 | 3 |
| `MEMORY_MAX_EPISODES` | 每会话最多情景数 | 10 |
| `MEMORY_CONFLICT_DETECTION` | 冲突检测 | true |
| `MEMORY_CONFLICT_THRESHOLD` | BM25 冲突阈值 | -2.0 |

数据存储：`~/.anolisa/memory/<namespace>/`。

### Profile 含义

Profile 是 UX 提示而非安全边界，但在 `tools/list` 和 `tools/call` 两层校验：

- **basic** —— 37 个工具全部展示；弱模型也能用 Tier B 的结构化 API。
- **advanced**（默认）—— 37 个工具全部展示；强模型应优先使用 Tier A 文件操作。
- **expert** —— 隐藏 Tier B（`memory_search`、`memory_observe`、`memory_get_context`、`mem_consolidate`、`memory_forget`、`memory_consent`），`tools/call` 调用会以 `METHOD_NOT_FOUND` 拒绝。熟练操作文件系统的前沿模型只需 Tier A 与 Tier C.

### Embedding 配置

```toml
[memory.embedding]
backend = "openai"                # 或 "ollama"
api_key = ""                      # 空时自动读 OPENAI_API_KEY 环境变量
model = "text-embedding-3-small"
# Ollama: backend = "ollama", model = "nomic-embed-text", base_url = "http://localhost:11434"
```

---

## 适用场景

- Agent 跨会话持久化笔记和决策（Claude Code、Cursor、Continue、自研 rmcp 客户端等）。
- 多 Agent 系统中 Agent A 写、Agent B 读的笔记跨进程共享。
- 操作审计和状态恢复（`mem_log`、JSONL 审计、journald、`mem_revert`、`mem_snapshot_restore`）。
- "先草稿、决定后才持久化"的多回合模式（`mem_promote` 从 session scratch 原子移入持久化仓）。

---

## SDK / 客户端开发指南

### Python（官方 `mcp` SDK）

```python
import asyncio
from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client

async def main():
    server = StdioServerParameters(
        command="/usr/bin/agent-memory", args=[],
        env={"USER_ID": "alice"},
    )
    async with stdio_client(server) as (read, write):
        async with ClientSession(read, write) as session:
            await session.initialize()
            tools = await session.list_tools()
            print([t.name for t in tools.tools])
            result = await session.call_tool(
                "mem_write",
                {"path": "notes/from-python.md", "content": "hello"},
            )
            assert not result.isError

asyncio.run(main())
```

### TypeScript（`@modelcontextprotocol/sdk`）

```typescript
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";

const transport = new StdioClientTransport({
  command: "/usr/bin/agent-memory", args: [],
  env: { USER_ID: "alice" },
});
const client = new Client({ name: "my-app", version: "1.0.0" }, {});
await client.connect(transport);
const result = await client.callTool({
  name: "mem_grep",
  arguments: { pattern: "TODO", recursive: true, max: 50 },
});
```

### Rust（`rmcp`）

```rust
use rmcp::transport::child_process::ChildProcessTransport;
use rmcp::ServiceExt;

let transport = ChildProcessTransport::new(
    tokio::process::Command::new("/usr/bin/agent-memory"),
).await?;
let client = ().serve(transport).await?;
let tools = client.list_tools(Default::default()).await?;
```

### Promote 工作流（多回合模式）

1. 每次 Agent 运行设 `MEMORY_SESSION_ID=<sid>` 和 `MEMORY_SESSION_DIR=/run/anolisa/sessions`。
2. Agent 把草稿写到 `/run/anolisa/sessions/<sid>/scratch/`。
3. Agent 决定"这条值得保留"时调用 `mem_promote` 原子移入持久化仓。

---

## 功能测试与验证

### 自动化测试

```bash
cd src/agent-memory
cargo fmt --check
cargo clippy -- -D warnings
cargo test                              # 全部 suite
cargo test --test e2e_agent_test        # 工具 E2E
cargo test --test mcp_integration_test  # 协议层
cargo test --test linux_userns_test -- --ignored  # 需 unprivileged userns
make smoke                              # 一键端到端冒烟
```

CI 跑 `fmt --check` + `clippy -D warnings` + `cargo test`，Rust 锁定 1.89。

### 交互式 `mcp-harness`

```bash
cargo run --example mcp-harness -- /tmp/mem-test
```

| 命令 | 说明 |
|------|------|
| `list` | 列出当前可见工具 |
| `call <tool> <json_args>` | 调用工具 |
| `help` | 帮助 |
| `quit` | 退出 |

预置场景：`--scenario full` / `git --git` / `promote` / `--verbose`（打印 JSON-RPC）。

### 直发 JSON-RPC（协议级调试）

```bash
mkdir -p /tmp/mem-test/__sessions__
MEMORY_BASE_DIR=/tmp/mem-test \
MEMORY_SESSION_DIR=/tmp/mem-test/__sessions__ \
MEMORY_MOUNT_STRATEGY=userland \
USER_ID=tester \
agent-memory
```

握手 + 工具调用：

```json
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"manual","version":"1.0"}}}
{"jsonrpc":"2.0","method":"notifications/initialized"}
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"mem_write","arguments":{"path":"test.md","content":"hello"}}}
```

### 沙箱越界验证

```json
{"name":"mem_read","arguments":{"path":"../../etc/passwd"}}
```
→ `isError: true`，消息 `path outside mount root`。

```json
{"name":"mem_write","arguments":{"path":".anolisa/audit.log","content":"x"}}
```
→ `isError: true`，消息 `target is reserved`。

---

## 故障排查

### 诊断工具

```bash
# 组件级诊断（按输出的 fix plan 手动处理）
anolisa doctor agent-memory

# 适配器状态
anolisa adapter status agent-memory

# 启动调试
RUST_LOG=agent_memory=debug agent-memory
```

### 常见问题

| 症状 | 可能原因 | 处理 |
|------|----------|------|
| 启动报 `unshare(NEWUSER\|NEWNS): EPERM` | unprivileged user namespace 被禁 | `sysctl kernel.unprivileged_userns_clone=1`，或 `MEMORY_MOUNT_STRATEGY=userland` |
| `tmpfs /mnt: EBUSY` | 新 namespace 中 `/mnt` 已被占用 | 重启进程 |
| macOS / Windows `cargo build` 报 `libsystemd`/`nix` 错 | 宿主非 Linux | `make remote-build` / `remote-test` |
| `tools/call memory_search` 返 `METHOD_NOT_FOUND` | `MEMORY_PROFILE=expert` 隐藏 Tier B | 切回 `advanced`，或直接用 Tier A |
| 配置项 typo 被悄悄忽略 | — | 现已硬失败，看启动 stderr 报错并修正 |
| `mem_log` 返回 `[]` 即使有写入 | git 版本控制未启用 | `MEMORY_GIT_ENABLED=true MEMORY_GIT_AUTO_COMMIT=true` |
| 索引检索对刚写入的内容查不到 | 还在 200 ms debounce 窗口内 | 重试，或用 `mem_grep`（直接走文件系统正则，不依赖索引） |
| `mem_promote` 报 `session not found` | `MEMORY_SESSION_ID`/`MEMORY_SESSION_DIR` 未设或 scratch 不存在 | 见 Promote 工作流 |
| OpenClaw 插件未加载 | `openclaw` CLI 不在 PATH | 安装 OpenClaw 后重跑 `install.sh` |
| `memory_get` 对 `~/.anolisa/memory` 下确实存在的文件返回 `disabled: true`，且 gateway 日志反复出现 `plugin tool name conflict (memory-anolisa): memory_get` / `memory_search` | OpenClaw 内置的 `memory-core` 仍被加载——它在别的插件占据 memory slot 后作为整理 sidecar 存活——并占着这两个工具名，OpenClaw「先到先得」的注册表因此丢弃本插件的同名工具 | 执行 `openclaw plugins disable memory-core`，再 `openclaw gateway restart`。`install.sh` 会自动做这件事并留下记录，供 `uninstall.sh` 恢复；`anolisa adapter enable` 不会，因此那条路径上这两条命令要你自己执行，`adapter disable` 也不会替你回退（#3218、#3225） |
| install.sh 报 `Plugin "memory-anolisa" requires capability consent` | OpenClaw >= 2026.8.1 的能力同意门禁；安装参数探测失败、设置了 `AGENT_MEMORY_ACCEPT_CAPABILITIES=0`，或脚本早于修复版本 | 查看安装输出中的探测 WARNING 或 opt-out 拒绝行；升级 agent-memory、取消该环境变量，或手动执行 `openclaw plugins install <插件目录> --force --accept-capabilities`。被拒绝授予且遭门禁拦截的安装以退出码 3 结束；若 OpenClaw 调整拒绝文案，脚本会退回退出码 1 并附带 opt-out 提示 |
| install.sh 报安装目标目录不可写 | `${OPENCLAW_STATE_DIR}/extensions`（或其最近的已存在父目录）对运行脚本的用户不可写，OpenClaw 的 `mkdir extensions/memory-anolisa` 因此以 `EACCES` 失败 | 修正该目录的属主/权限——或把 `OPENCLAW_STATE_DIR` 指向可写的 state 目录——后重跑。这是文件系统权限失败，不是策略拒绝：不要为此放宽 `security.installPolicy` |
| install.sh 在把 `--dangerously-force-unsafe-install` 标注为 deprecated no-op 的宿主上安装失败 | OpenClaw 2026.6.5 及之后已无安装期扫描，脚本没有传递覆盖参数，也就无法影响该宿主的安装期安全；原因在 `openclaw` 自己的输出里 | 阅读脚本提示上方的 CLI 输出。只有当它点名 `security.installPolicy` 时，需要放宽的才是这条运维自有策略——重跑脚本或设置 `AGENT_MEMORY_SAFE_INSTALL` 都无法覆盖它 |
| install.sh 报安全扫描拦截了插件 | OpenClaw 2026.6.1 及更早版本在安装期扫描插件源码，把插件用于 MCP 传输的 `child_process.spawn` 判为危险 | 取消 `AGENT_MEMORY_SAFE_INSTALL`，让脚本传递它原本被拒绝的覆盖参数，或升级 OpenClaw |
| 手动 dnf 操作后 system 状态不同步 | — | `sudo anolisa --install-mode system repair agent-memory`；仅在为仍存在的 RPM 重建记录时使用 system-scoped `forget` / `adopt` |

深入排查：`RUST_LOG=agent_memory=debug` 启动，检查服务端 stderr 与 `<mount>/.anolisa/audit.log`。

---

**许可证**：Apache-2.0
**版本**：0.2.1
**文档版本**：2.0（对齐 ANOLISA-design user-guide 结构）
