---
name: install-dsh
version: 1.0.0
description: 在 Linux/macOS 上为 DeepSeek Harness（dsh）接入 ANOLISA Tokenless 压缩插件。覆盖 dsh CLI 与 Node.js >= 22 预检、按 profile 注册原生 bundle（anolisa adapter enable tokenless dsh --profile）、cordis.patch.yml 配置覆盖、安装验证与卸载。当用户需要在 dsh 中启用 Tokenless 压缩、管理 dsh profile 的 anolisa-tokenless bundle 或排查注册问题时使用此技能。
layer: application
lifecycle: usage
---

# DeepSeek Harness（dsh）Tokenless 接入

DeepSeek Harness（命令名 `dsh`）是按 profile 运行的编码代理框架
（`dsh --profile <name>`）。Tokenless 为 dsh 提供原生 bundle
（npm 包 `@anolisa/dsh-tokenless`，自包含的 `dist/index.js` +
`cordis.patch.yml`），在 `tools/post-execute` waterfall 上把可替换的单文本
工具结果送入 Tokenless Core 压缩。本技能把该 bundle 注册进 dsh 的 profile。

命令事实与 ANOLISA 自带材料保持一致：dsh 适配器
（`src/tokenless/adapters/tokenless/dsh/`，无独立安装脚本）与
`docs/user-guide/en/token-saving/tokenless/framework-integration.md` 的
DeepSeek Harness native processing 一节。

注意：dsh 与 OpenCode、Qoder 等不同，适配器没有 `scripts/install.sh`，
注册**只能**通过 `anolisa adapter enable tokenless dsh` 完成（需要 anolisa
组件记录）。

## 系统要求

- **OS**: Linux 或 macOS
- **dsh CLI**: 已安装且在 PATH 上（本仓库文档未提供 dsh 本体的安装命令，
  请从 DeepSeek Harness 的发布渠道获取；`anolisa adapter scan` 找不到时先
  确认 CLI 已安装再扫描）
- **Node.js**: >= 22（bundle 的 `engines.node` 要求）
- **Tokenless CLI**: `tokenless` 在 PATH 上（bundle 通过子进程调用它；
  Marker 恢复还要求裸 `tokenless` 与 Core 选中的是同一可执行文件）
- **anolisa CLI**: 必需（组件记录 + 注册入口）

## 接入工作流

复制此清单跟踪进度：

```
Task Progress:
- [ ] Step 1: 预检环境（dsh / node>=22 / tokenless / anolisa）
- [ ] Step 2: 确认 profile 名称
- [ ] Step 3: 注册 Tokenless dsh bundle（一次性传入全部 profile）
- [ ] Step 4: 验证
- [ ] Step 5: 重启 dsh profile
```

### 一键脚本（推荐）

```bash
sh scripts/install.sh --profile web                    # 单 profile
sh scripts/install.sh --profile web --profile headless # 多 profile 一次传入
sh scripts/install.sh --profile web --skip-tokenless   # 只做环境预检
```

脚本自动完成：预检 → 收集 `--profile` 参数 → 单条 enable 命令注册全部
profile（完整集合语义）→ `anolisa adapter status tokenless` 验证。

### Step 1: 预检环境

```bash
command -v dsh && command -v anolisa && command -v tokenless
node --version    # 需要 >= 22
```

### Step 2: 确认 profile 名称

每个名称必须与 `dsh --profile <profile>` 使用的名称一致。查看现有 profile
（位于 `$DSH_HOME/profiles/` 下）后列出全部目标名称。

### Step 3: 注册 Tokenless dsh bundle

一条命令传入全部目标 profile（`--profile` 必填且可重复）：

```bash
anolisa adapter enable tokenless dsh \
  --profile web \
  --profile headless
```

**完整集合语义**：每次 enable/重新 enable 都把本次传入的 profile 集合视为
完整的期望集合——上一次回执记录过、但本次未传的 profile 会被移除 bundle。
因此始终把所有需要保留 Tokenless 的 profile 一并传入。不带 `--profile` 的
通用命令会被拒绝。ANOLISA 会在适配器回执中记录所选 profile 及其解析出的
DSH home，后续 status/disable/重新 enable 会继续指向同一 profile 树。

### Step 4: 验证

```bash
anolisa adapter status tokenless
```

### Step 5: 重启并实测

```bash
dsh --profile web
```

重启该 profile 后运行一个返回可压缩 JSON 的工具，再检查统计：

```bash
tokenless stats list
```

## 配置覆盖（可选）

在 `$DSH_HOME/profiles/<profile>/cordis.patch.yml` 中为已安装的
`anolisa-tokenless` row 添加 `config` 覆盖，然后重启该 profile：

```yaml
- id: anolisa-tokenless
  config:
    responseCompressionEnabled: true
    timeoutMs: 5000
    maxBuffer: 4194304
```

后续 dsh patch 层会整体替换该 row 的 `config` 值；插件为省略的键提供默认值。

| 选项 | 默认值 | 行为 |
|------|--------|------|
| `responseCompressionEnabled` | `true` | 启用响应压缩；设为 `false` 不会关闭环境错误归因 |
| `tokenlessBin` | `$TOKENLESS_BIN`，然后 `tokenless` | 选择 Tokenless CLI 可执行文件；插件的非空值优先于环境变量 |
| `timeoutMs` | `3000` | 单个 Tokenless 子进程的超时（毫秒），仅接受正整数 |
| `maxBuffer` | `2097152` | 子进程输出捕获上限（字节），仅接受正整数 |
| `agentId` | `dsh` | Tokenless 统计中记录的 Agent 归因 |

默认状态目录是会话工作区下的 `.tokenless`（内含自忽略的 `.gitignore`）；
启动 dsh 前设置 `TOKENLESS_DATA_DIR`、`TOKENLESS_STATS_DB` 或
`TOKENLESS_STASH_DB` 可改用 dsh shell 沙箱可访问的其他绝对路径。

## 故障排查

| 症状 | 处理 |
|------|------|
| `dsh: command not found` | dsh CLI 未安装；从 DeepSeek Harness 发布渠道安装后重跑预检 |
| `anolisa: command not found` | 先安装 Tokenless（anolisa CLI 随组件提供），见 install-tokenless 技能 |
| enable 提示缺少 profile | `--profile` 必填；名称须与 `dsh --profile <name>` 一致 |
| 某 profile 的 bundle 消失 | 上次 enable 只传了部分 profile；重新 enable 时把全部 profile 一并传入 |
| Node 版本低于 22 | bundle 要求 Node.js >= 22；升级 Node 后重试 |
| `tokenless` 不在 PATH | bundle 无法调用 CLI；安装后确认裸 `tokenless` 可解析 |
| 压缩未生效 | 重启对应 profile；确认 `responseCompressionEnabled` 未被设为 `false` |

## 卸载

```bash
# 注销 dsh 适配器（回执已记录 profile 名称，disable 不接受 --profile）
anolisa adapter disable tokenless dsh
```

之后重启对应的 dsh profile。卸载 bundle 不会删除 `.tokenless/`
（统计数据库被所有适配器共享）。
