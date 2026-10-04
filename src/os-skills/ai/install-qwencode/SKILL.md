---
name: install-qwencode
version: 1.0.0
description: 在 Linux/macOS 上安装 Qwen Code CLI（qwen）并接入 ANOLISA Tokenless 扩展（RTK 命令改写）。覆盖 npm 安装（Node 22+ 预检）、tokenless/rtk 就绪检查、qwen extensions link 注册、验证与卸载。当用户需要安装 Qwen Code、部署 qwen CLI、在 Qwen Code 中启用 Tokenless 或排查 tokenless 扩展注册问题时使用此技能。
layer: application
lifecycle: usage
---

# Qwen Code CLI 安装与 Tokenless 接入

Qwen Code（命令名 `qwen`）是 Qwen 的命令行编码代理。Tokenless 为 Qwen Code
提供 RTK 命令改写（通过 `qwen extensions link` 注册的 `tokenless` 扩展）；当前
宿主版本不支持输出替换，压缩只作用于命令改写。本技能安装 Qwen Code 本体，并把
Tokenless 扩展注册进去。

命令事实与 ANOLISA 自带的 tokenless qwencode 适配器
（`src/tokenless/adapters/tokenless/qwencode/scripts/install.sh`）保持一致。

## 系统要求

- **OS**: Linux 或 macOS
- **网络**: 需要联网（npm registry）
- **Shell**: Bash
- **依赖**: Node.js **22+**（含 `npm`，官方包的引擎要求）、`python3`（hook 脚本依赖）
- **Tokenless CLI**: `tokenless` 与 `rtk` 命令须可用（见 Step 4）

## 安装工作流

复制此清单跟踪进度：

```
Task Progress:
- [ ] Step 1: 预检环境（node 22+/npm/python3）
- [ ] Step 2: 安装 Qwen Code CLI
- [ ] Step 3: 验证 qwen
- [ ] Step 4: 确保 Tokenless/rtk 就绪
- [ ] Step 5: 注册 Tokenless 扩展
- [ ] Step 6: 验证
```

### 一键脚本（推荐）

```bash
bash scripts/install.sh                  # 安装 qwen + 注册 tokenless 扩展
bash scripts/install.sh --skip-tokenless # 只安装 qwen
```

脚本自动完成：预检（含 Node 大版本检查）→ npm 安装 → qwen 探测（含
`~/.local/bin`、`/usr/local/bin`）→ tokenless/rtk 就绪检查 → 扩展注册（优先
`anolisa adapter enable`）→ 验证 `qwen extensions list`。

### Step 1: 预检环境

```bash
node --version && npm --version && command -v python3
```

官方 npm 包要求 Node.js 22+。版本不足时先通过 nvm 安装：

```bash
curl -o- https://raw.githubusercontent.com/nvm-sh/nvm/v0.40.3/install.sh | bash
source ~/.bashrc && nvm install --lts
```

### Step 2: 安装 Qwen Code CLI

官方 npm 包：

```bash
npm install -g @qwen-code/qwen-code
```

不要使用 `sudo npm install -g`（会造成权限问题）。出现 `EACCES` 时修正 npm
prefix：

```bash
mkdir -p ~/.npm-global
npm config set prefix '~/.npm-global'
echo 'export PATH="$HOME/.npm-global/bin:$PATH"' >> ~/.bashrc
source ~/.bashrc
npm install -g @qwen-code/qwen-code
```

### Step 3: 验证 qwen

```bash
qwen --version
```

`command not found` 时确认 npm 全局 bin 目录（如 `~/.npm-global/bin`）在 PATH。

### Step 4: 确保 Tokenless/rtk 就绪

适配器将 `tokenless` 与 `rtk` 列为运行前提（`npm` 安装 tokenless 时会同时
提供这两个命令）：

```bash
command -v tokenless && command -v rtk || \
  curl -fsSL https://raw.githubusercontent.com/alibaba/anolisa/main/src/tokenless/scripts/install.sh | bash
```

### Step 5: 注册 Tokenless 扩展

优先使用 anolisa CLI（推荐，写入组件记录）：

```bash
anolisa adapter enable tokenless qwencode
```

npm 直装且无 anolisa 记录时，直接运行适配器自带脚本：

```bash
bash ~/.local/share/anolisa/adapters/tokenless/qwencode/scripts/install.sh
```

底层动作（适配器实际执行）：`qwen extensions link <适配器目录>`（link 而非
install，适配器目录更新即时生效；重复执行会先 uninstall 再 link，幂等）。

### Step 6: 验证

```bash
qwen extensions list
```

列表中应出现 `tokenless`。

```bash
anolisa adapter status tokenless   # anolisa 管理的安装
```

**新开一个 Qwen Code 会话**并执行一次工具调用，扩展才会加载验证。

## 配置位置

Qwen Code 全局配置与扩展数据都在 `~/.qwen/`：

- `~/.qwen/settings.json` — 全局设置（含 hooks；扩展注册后无需手工添加
  tokenless 条目，扩展清单自带）
- `~/.qwen/extensions/` — 已安装/链接的扩展
- `~/.qwen/extension-enablement.json` — 扩展启停状态

## 故障排查

| 症状 | 处理 |
|------|------|
| `command not found: qwen` | 确认 npm 全局 bin 目录在 PATH：`npm config get prefix` |
| npm 安装报引擎不满足 | 升级到 Node.js 22+（nvm install --lts） |
| `EACCES`（npm 全局安装） | 用 Step 2 的 npm prefix 修正流程 |
| `qwen extensions list` 看不到 tokenless | 重跑 Step 5；确认 python3 存在；重启 qwen-code 会话 |
| 扩展重复注册 | 适配器脚本幂等（先 uninstall 再 link），无需手工处理 |
| hook 未生效 | 新会话才会加载扩展；确认 `command -v tokenless` 与 `command -v rtk` |
| 无响应压缩统计 | 正常：当前 Qwen Code 宿主无输出替换字段，Tokenless 在 Qwen Code 上只做命令改写 |

## 卸载

```bash
# 1. 注销 Tokenless 扩展（anolisa 管理的安装）
anolisa adapter disable tokenless qwencode

# 2. 或直接通过 qwen 注销
qwen extensions uninstall tokenless

# 3. （可选）移除 Qwen Code CLI 本体
npm uninstall -g @qwen-code/qwen-code
```

注意：卸载 Tokenless 扩展不会删除 `~/.tokenless/`（统计数据库被所有适配器共享）。
