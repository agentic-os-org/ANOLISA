# ws-ckpt Plugin 设计文档

> ws-ckpt 插件在 Agent 对话生命周期中**自动**为工作区建快照,出错能 rollback。本文档同时覆盖 [Hermes 插件](../src/plugins/hermes/)（Python）和 [OpenClaw 插件](../src/plugins/openclaw/)（TypeScript）——两者行为一致,差异仅在宿主 runtime 的 hook API 与语言生态。
>
> 架构总览见 [ws-ckpt-design.md](./ws-ckpt-design.md)。

---

## 设计哲学

**Plugin 存在的唯一首要理由是:让每个 Agent 回合自动产生一个快照,用户和 Agent 都不用记得这件事。** 其他能力(暴露 tool 给 LLM、配置查询、错误翻译)都是围绕这一目标的支撑。

为什么必须自动?因为非自动方案有三个弱点:

- **不能指望用户记得**:让用户每次都手动 `ws-ckpt checkpoint` 是不可行的。Agent 自动化的卖点就是"用户不用盯",反过来要求用户盯着按"保存"等于自我否定。
- **不能指望 LLM 记得**:把 `ws-ckpt-checkpoint` 暴露成 tool 让 LLM 自主调用同样会失败——LLM 会在该调用时忘记、在不该调用时滥用、在失败重试链路里搞不清要不要再建一次。**安全兜底不应该依赖被兜底者的自觉。**
- **不能指望每个 Agent 框架自己实现**:Hermes / OpenClaw 各写一遍"在合适生命周期点 fork ws-ckpt",会重复造轮子并在边角情况(cwd 守卫、空工作区、错误降级)上犯一样的错。

工具暴露(七个 `ws-ckpt-*` tool)是次级目标:自动快照已经覆盖了 95% 的场景,剩下 5%(LLM 想在 turn 中间建个语义化 milestone / 用户问"列一下快照" / 显式回滚到具体 id)交给 tool。**没有自动快照,plugin 就没存在必要——用户直接装 ws-ckpt cli + skill 就行了。**

**一句话:plugin = 把 ws-ckpt 的能力从"需要被记得调用的工具"升级为"runtime 自动提供的兜底"。**

---

## 两个实现,一份设计

| 维度         | Hermes plugin                                                | OpenClaw plugin                                                        |
| ------------ | ------------------------------------------------------------ | ---------------------------------------------------------------------- |
| 宿主 runtime | Hermes(Python CLI)                                           | OpenClaw(TypeScript)                                                   |
| 语言         | Python 3                                                     | TypeScript → Node ESM                                                 |
| 入口         | `register(ctx)`                                            | `register(api)`                                                      |
| 配置位置     | `~/.hermes/config.yaml` 的 `plugins.ws-ckpt` 节          | `~/.openclaw/openclaw.json` 的 `plugins.entries.ws-ckpt.config` 节 |
| 子进程调用   | `subprocess.run(["ws-ckpt", ...], timeout=240)`            | `execFile("ws-ckpt", [...], { timeout: 240_000 })`                   |
| Hook 三件套  | `on_session_start` / `pre_llm_call` / `on_session_end` | `session_start` / `message_received` / `agent_end`               |

两个实现共享同一组核心抽象:

- **CommandExecutor / CheckpointManager** — 薄薄一层 `ws-ckpt` CLI 包装,构造 argv → spawn → 解析 exit code / stdout / stderr
- **环境检查** — CLI 在不在 PATH、daemon 在不在线
- **CWD 守卫** — 拒绝 agent 进程 cwd 及其父目录作为工作区
- **错误映射** — 把 CLI stderr 翻译成对 LLM 友好的提示
- **七个 tool** — `ws-ckpt-{config,checkpoint,rollback,list,diff,delete,status}`

差异只在"宿主 runtime 长什么样、怎么注册 hook / tool",业务逻辑 1:1 对应。

---

## 自动快照策略

### 粒度:per-turn

Plugin 在每个 Agent 回合结束时（`agent_end` / `on_session_end`）自动建快照。选 per-turn 的四条理由：

1. **用户心智匹配** — "回退到上一步"的自然含义是"上一回合结束时"
2. **快照数可控** — 典型 10-50 回合/会话，配合 auto-cleanup 完全可控
3. **不和 read-only 纠缠** — per-tool-call 会对 read/search 也建快照，过滤成本高
4. **避开锁冲突** — `agent_end` 时 tool 已停手，`check_workspace_quiescent` 几乎不冲突

代价是 **turn 内中间状态不可回退**。这是有意取舍——Agent 的中间状态本来就不稳定(半写完的代码、中途崩溃的 patch),保留意义不大。需要中间状态的用户走 `ws-ckpt-checkpoint` 工具显式建快照。

另外在 `session_start` 建一个基线快照用于"回退到会话开始"。

### 开销

端到端 ~20-50 ms（btrfs snapshot ~1ms + subprocess ~10-30ms + IPC ~1-5ms），纯元数据操作，增量磁盘空间为 0（CoW）。与 LLM 推理时间重叠，用户无感知。

### 快照清理

Plugin 只管建快照，不主动删——清理交给 daemon auto-cleanup（`Count(N)` 或 `Age("30d")`，见[主设计文档](./ws-ckpt-design.md#后台调度)）。稳态下工作区维持固定数量快照。

### 失败处理

自动 checkpoint **失败不阻塞 Agent**：

| 失败类型 | 处理 |
|----------|------|
| `WriteLockConflict` / `DiskSpaceInsufficient` / `BtrfsError` | 跳过本 turn，warn，下个 turn 继续 |
| `CwdOccupied` | 关闭 `autoCheckpoint` 直到 session 重启 |
| 空工作区 | `CheckpointSkipped` 当成功处理，不报错 |

---

## 生命周期 Hook

上节策略落到具体实现:plugin 在宿主 runtime 的三个生命周期点挂逻辑,完成 per-turn 自动快照 + session baseline。

### Hook 三件套

| 阶段         | Hermes hook                        | OpenClaw hook        | 行为                                                                                    |
| ------------ | ---------------------------------- | -------------------- | --------------------------------------------------------------------------------------- |
| 会话起始     | `on_session_start`               | `session_start`    | 校验环境 →`ws-ckpt init`(idempotent) → 创建 `event=session_start` 的基线快照      |
| 用户消息到达 | `pre_llm_call(user_message=...)` | `message_received` | 截取 user message 前 80 字符,存到 in-memory tracker,下次 checkpoint 当作 commit message |
| 回合结束     | `on_session_end`                 | `agent_end`        | `++turn_count` → 用 tracker 缓存的 message 创建 `event=turn_end` 快照              |

> 命名差异:Hermes 的 `on_session_end` **每个 turn 都会 fire**(per `run_conversation()`),所以语义上等同于 OpenClaw 的 `agent_end`,**不是**"会话结束"。

### Snapshot id 与 metadata

snapshot id 由 plugin 生成,8 字符就够:

- Hermes: `secrets.token_hex(4)`
- OpenClaw: `crypto.randomUUID().slice(0, 8)`

metadata 附在快照上以便后续审计:

```json
{ "event": "session_start", "turn": 0, "timestamp": "<ISO8601>" }
{ "event": "turn_end",      "turn": N, "timestamp": "<ISO8601>", "success": true }
```

`ws-ckpt list` 把 metadata 一起返回,Agent / 用户能区分哪些快照是 session-start、哪些是 turn-end、对应哪条 user message。

### Hook 退出条件

每个 hook 入口都先做这串短路检查,缺一个直接 return:

1. `config.auto_checkpoint == true`(默认 `false`,**必须显式打开**才会自动 checkpoint)
2. `config.workspace` 非空
3. `environmentReady == true`(环境检查通过)
4. `cwd_inside_workspace(workspace) == false`(CWD 守卫,见下文)

任意一项失败:Hermes 直接 print warn 然后 return;OpenClaw `console.warn` 并把 `config.autoCheckpoint = false` 把后续 turn 的 hook 短路掉(避免反复刷屏)。

---

## 工具暴露:自动快照的补充

> **绝大多数用户从不需要显式调用这些工具**——自动 per-turn 快照已经覆盖了"试错回退"的主路径。下列工具的存在意义是:(a) LLM 在 turn 中间想建一个语义化 milestone;(b) 用户对 Agent 说"回退到 X 那个时刻";(c) 配置查询调整。

Plugin 把以下七个工具用 OpenAI Function Calling 格式注册到 runtime:

| 工具 | 描述 | 必填参数 | 可选参数 |
|------|------|----------|----------|
| `ws-ckpt-config` | 查看 / 更新 plugin 配置 | — | `action`, `key`, `value` |
| `ws-ckpt-checkpoint` | 手动创建快照 | `id` | `message`, `workspace` |
| `ws-ckpt-rollback` | 回滚到指定快照或沿 parent 链回退或预览回滚 | — | `target`, `num_ancestors`/`numAncestors`, `workspace`, `preview` |
| `ws-ckpt-list` | 列出工作区所有快照 | — | — |
| `ws-ckpt-diff` | 对比两个快照 | `from`, `to` | — |
| `ws-ckpt-delete` | 删除指定快照 | `snapshot` | `workspace` |
| `ws-ckpt-status` | 查看 daemon + 工作区状态 | — | — |

### ws-ckpt-rollback 的 DAG 祖先回退

`target` 和 `num_ancestors`（hermes snake_case）/ `numAncestors`（openclaw camelCase）互斥，至少提供一个。CLI 的 `-n 1` 表示 head 本身；plugin 在每轮结束后已经为当前状态创建 head，因此面向用户的“回退 N 轮”会映射到 CLI `ws-ckpt rollback -w <ws> -n <N+1>`。两个 plugin 各自校验 `>= 1`，与 `ancestor()` 入口校验形成四层防御。

两个 plugin 均暴露可选布尔参数 `preview`。为 `true` 时透传 CLI `--preview`，返回完整文件变更摘要，不刷新 snapshot cache，也不把成功结果描述为已经完成回滚。

### Workspace 解析:显式参数绕过缓存

调用 tool 时若传了 `workspace` 参数则**绕过 plugin manager 缓存,直接通过 CommandExecutor 跑 CLI**(避免缓存只对默认 workspace 有效的歧义);未传则用 `pluginState.resolvedConfig.workspace` 兜底。

### ws-ckpt-config 的两类 key

| Key                                            | 落点                    | 持久化路径                                                                                                 |
| ---------------------------------------------- | ----------------------- | ---------------------------------------------------------------------------------------------------------- |
| `workspace` / `autoCheckpoint`             | plugin in-memory config | `~/.hermes/config.yaml` / `~/.openclaw/openclaw.json`(需用户手工 / runtime 重启)                       |
| `maxSnapshotsNum` / `maxSnapshotsDuration` | 转发到 daemon (per-ws)  | `ws-ckpt config -w <workspace> --enable-auto-cleanup --auto-cleanup-keep <v>` → daemon 写 `/var/lib/ws-ckpt/indexes/<ws_id>/policy.toml`(单个 plugin 用户改自己 workspace 的策略,不会动其他 workspace 共享的全局默认) |

plugin 配置和 daemon 配置是两层:plugin 管"开不开自动 checkpoint、用哪个工作区",daemon 管"留多少个快照、什么时候清理"。


---

## CWD 守卫

Plugin 在会替换工作区 inode 的 hook/tool 入口自查 `cwd_inside_workspace`——如果自身进程的 cwd 落在工作区内，立即拒绝并返回 NOT retryable 消息。`rollback preview` 只创建临时只读快照并计算差异，不替换工作区 inode，因此不受该守卫限制。

这是 daemon 端 `/proc` guard 的**提前短路优化**（节省一次 RPC 往返 + 给出更友好的错误原因），不是安全边界——即使 plugin 漏检，daemon 也会强制拒绝。

---

## 环境检查与降级模式

`register()` 时检查：CLI 在 PATH + daemon 在线（`ws-ckpt status` 返回 0）。任一失败 → `environmentReady = false` → hook 跳过、tool 返回 UNAVAILABLE_MSG。**不抛异常、不阻塞 runtime 启动**。

环境就绪后缓存一次 per-ws cleanup 策略（`ws-ckpt config -w <workspace> --format json`），解析为四态（`disabled | count | age | parse-error`）。`parse-error` 不等同于 disabled——防止版本不一致误判。plugin 一律走 `-w` scope，不会偷偷操作全局配置。

---

## 配置

两个 plugin 共用同样的配置结构(camelCase):

```yaml
# Hermes: ~/.hermes/config.yaml
plugins:
  ws-ckpt:
    autoCheckpoint: true
    workspace: /home/me/project
```

```json
// OpenClaw: ~/.openclaw/openclaw.json
{
  "plugins": {
    "entries": {
      "ws-ckpt": {
        "config": {
          "autoCheckpoint": true,
          "workspace": "/home/me/project"
        }
      }
    }
  }
}
```

### 字段

| 字段               | 类型   | 默认值                                          | 含义                                                                                         |
| ------------------ | ------ | ----------------------------------------------- | -------------------------------------------------------------------------------------------- |
| `autoCheckpoint` | bool   | `false`                                       | 是否在 hook 中自动创建快照。**默认关闭**:开启需要用户显式同意,避免静默改写文件系统     |
| `workspace`      | string | Hermes: 空 / OpenClaw:`~/.openclaw/workspace` | 默认工作区路径;如果是 symlink,传 link 本身(见[Workspace 解析](#workspace-解析显式参数绕过缓存)) |

### 优先级

`env var > config file > default`:

- Hermes:`WS_CKPT_AUTO_CHECKPOINT` / `WS_CKPT_WORKSPACE`
- OpenClaw:纯 config + 默认值(不读环境变量,由 runtime 自己处理)

---

## 实现细节

- **Reload 安全**：`register()` 可重入，`pluginState` 模块级单例 + 原地更新引用，老 hook closure 不会引用陈旧对象
- **OpenClaw tools.alsoAllow 兜底**：`register()` 时自动把缺失的 `ws-ckpt-*` 写入 `openclaw.json` 的 allowlist（原子写 + 进程级 dedup），Hermes 不需要（toolset 直接注册）

---

## 不做的事

| 排除项                    | 原因                                                                                       |
| ------------------------- | ------------------------------------------------------------------------------------------ |
| 维护本地 snapshot 缓存    | `ws-ckpt list` 已经是单点真相,每次现 spawn 一次 CLI(~ms 级),复制一份易过期的缓存得不偿失 |
| 重试逻辑                  | 留给 Agent / runtime;plugin 只翻译 stderr 标注 retryable,不主动重试                        |
| 直接连 daemon Unix Socket | 多一层 bincode / IPC 类型同步成本,收益不大;CLI 接口是更稳定的公共契约                      |
| 安装 / 部署 ws-ckpt 本体  | 假设 ws-ckpt 已通过 RPM / yum 安装;plugin 自己检测,没装就降级,不试图自举                   |

---

## 前提条件

- 已执行提供 plugin 注册安装脚本(Hermes / OpenClaw)
- ws-ckpt CLI 已安装
- ws-ckpt daemon 在线(`ws-ckpt status` 返回 0)
- 启动 Agent 时 cwd 在工作区**外**——否则 CWD 守卫会自动禁用 `autoCheckpoint`,用户需手工 cd 出来再重启
