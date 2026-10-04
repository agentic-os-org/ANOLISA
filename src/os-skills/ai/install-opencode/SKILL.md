---
name: install-opencode
version: 1.0.0
description: 在 Linux/macOS 上为 OpenCode 接入 ANOLISA Tokenless 压缩插件。覆盖 opencode CLI 预检、全局本地插件注册（anolisa adapter enable tokenless opencode，npm 直装回退到适配器自带脚本）、配置目录解析（OPENCODE_CONFIG_DIR / XDG_CONFIG_HOME / ~/.config/opencode）、plugins/tokenless.js 链接验证、重启要求与卸载。当用户需要在 OpenCode 中启用 Tokenless 压缩或排查 tokenless 插件链接问题时使用此技能。
layer: application
lifecycle: usage
---

# OpenCode Tokenless 插件接入

OpenCode（命令名 `opencode`）是开源命令行编码代理。Tokenless 为 OpenCode
提供本地插件（`plugin.js`，注册 `tool.execute.before/after` 与
`tool.definition` 钩子），压缩后的输出会**替换**原始模型可见响应。OpenCode
在启动时发现全局本地插件，本技能把 Tokenless 插件注册进去。

命令事实与 ANOLISA 自带的 tokenless opencode 适配器
（`src/tokenless/adapters/tokenless/opencode/scripts/install.sh`）及
`docs/user-guide/en/token-saving/tokenless/framework-integration.md` 的
OpenCode 一节保持一致。

## 系统要求

- **OS**: Linux 或 macOS
- **opencode CLI**: 已安装且可被发现（`OPENCODE_BIN` 指定，或在 PATH 上；
  本仓库文档未提供 opencode 本体的安装命令，`anolisa adapter scan` 找不到时
  先从 OpenCode 官方渠道安装 CLI 再扫描）
- **注册入口（二选一）**:
  - `anolisa` CLI（推荐，写入组件记录），或
  - npm/curl 直装的适配器资源
    `~/.local/share/anolisa/adapters/tokenless/opencode/scripts/install.sh`

## 接入工作流

复制此清单跟踪进度：

```
Task Progress:
- [ ] Step 1: 预检环境（opencode / 注册入口）
- [ ] Step 2: 注册 Tokenless OpenCode 插件
- [ ] Step 3: 验证 plugins/tokenless.js
- [ ] Step 4: 重启 OpenCode 并实测
```

### 一键脚本（推荐）

```bash
sh scripts/install.sh                  # 注册 tokenless 插件并验证
sh scripts/install.sh --skip-tokenless # 只做环境预检
```

脚本自动完成：预检 → 注册（优先 `anolisa adapter enable`，npm 直装回退到
适配器自带 install.sh）→ 验证插件链接。

### Step 1: 预检环境

```bash
command -v opencode || command -v "$OPENCODE_BIN"
```

注册入口检查：`command -v anolisa`，或存在
`~/.local/share/anolisa/adapters/tokenless/opencode/scripts/install.sh`。

### Step 2: 注册 Tokenless OpenCode 插件

优先使用 anolisa CLI（推荐，写入组件记录）：

```bash
anolisa adapter enable tokenless opencode
```

驱动按 `OPENCODE_CONFIG_DIR` → `XDG_CONFIG_HOME/opencode` →
`~/.config/opencode` 解析配置目录，并把插件的 `.js` 入口注册为
`plugins/tokenless.js`。自定义目录值必须是绝对路径；status/disable 时保持
相同的目录配置。若旧的独立安装器用过 `TOKENLESS_OPENCODE_CONFIG_DIR`，先把它
的值赋给 `OPENCODE_CONFIG_DIR` 再 enable。

npm 直装且无 anolisa 记录时，直接运行适配器自带脚本（它额外把
`TOKENLESS_OPENCODE_CONFIG_DIR` 作为最高优先级覆盖）：

```bash
bash ~/.local/share/anolisa/adapters/tokenless/opencode/scripts/install.sh
```

底层动作（适配器实际执行）：在配置目录的 `plugins/` 下创建指向适配器
`plugin.js` 的符号链接 `tokenless.js`。两种路径都会拒绝冲突的文件/链接；
enable 会收养指向同一插件源的既有符号链接（含独立安装器创建的）。

### Step 3: 验证

```bash
# 插件链接应存在且指向 tokenless 的 plugin.js
ls -l "${TOKENLESS_OPENCODE_CONFIG_DIR:-${OPENCODE_CONFIG_DIR:-${XDG_CONFIG_HOME:-$HOME/.config}/opencode}}/plugins/tokenless.js"

# anolisa 管理的安装：验证链接与包源
anolisa adapter status tokenless
```

status 会校验链接与包源；运行时加载状态报告为 `unknown`——链接存在不代表
已在运行的 OpenCode 进程中加载。

### Step 4: 重启并实测

enable/disable 后**必须重启 OpenCode**：已有进程会保留已加载的插件（包括
工具输出替换），直到重启。重启后运行一次工具调用并检查统计：

```bash
tokenless stats list
```

## 故障排查

| 症状 | 处理 |
|------|------|
| `opencode: command not found` | opencode CLI 未安装；从官方渠道安装后重跑预检 |
| `anolisa: command not found` 且适配器脚本缺失 | 先安装 Tokenless，见 install-tokenless 技能 |
| enable 提示保留的冲突条目 | `plugins/tokenless.js` 已被占用且非同源链接；按报错解析冲突路径后重试（收养只接受同一插件源） |
| 旧独立安装的链接挡路 | 用原安装配置先卸载：`make opencode-uninstall`（源码构建）或原 bundle 的 `scripts/uninstall.sh`，再 anolisa enable |
| 换了 OPENCODE_CONFIG_DIR 后 disable 失败 | 恢复 enable 时的目录环境再重试 |
| 插件未加载 | 完全重启 OpenCode；确认链接非悬空（`readlink` 目标存在） |
| 更换目录配置后找不到链接 | 两套入口的目录解析顺序不同；保持与注册时相同的 `OPENCODE_CONFIG_DIR`/`TOKENLESS_OPENCODE_CONFIG_DIR` |

## 卸载

```bash
# 1. 注销插件（anolisa 管理的安装；保持相同的目录配置）
anolisa adapter disable tokenless opencode

# 2. 或直接通过适配器脚本移除链接（npm 直装）
bash ~/.local/share/anolisa/adapters/tokenless/opencode/scripts/uninstall.sh
```

适配器的 uninstall 只删除它管理的符号链接，不动未托管的路径。卸载后重启
OpenCode。
