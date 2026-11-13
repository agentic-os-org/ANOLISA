# OS Skills

[English](README.md)

面向 AI Agent 的运维技能集合，涵盖系统管理、监控、安全、DevOps 和云集成。OS Skills 是 [ANOLISA](../../README.md) 的技能库，安装到 `/usr/share/anolisa/skills/` 后由 Copilot Shell 自动发现。

## 技能分类

| 分类 | 目录 | 说明 |
|------|------|------|
| **AI 工具** | `ai/` | AI 编程工具集成（Claude Code、OpenClaw、QwenPaw、MCP 配置） |
| **系统管理** | `system-admin/` | 包管理、存储、网络、内核、Shell 脚本 |
| **开发运维** | `devops/` | Git 工作流、CI/CD、内核开发、系统诊断 |
| **阿里云** | `aliyun/` | ECS 实例管理、云网络、GPU/AI 部署 |
| **安全** | `security/` | CVE 查询、合规检查、系统加固 |
| **监控与性能** | `monitor-perf/` | sysAK 诊断、keentune 调优、sysctl 管理 |
| **其他** | `others/` | 实验性功能与第三方扩展 |

## 包含的技能

### AI 工具
- **install-claude-code** — 安装和配置 Claude Code IDE
- **install-qwenpaw** — 部署 QwenPaw AI 助手（支持钉钉集成）
- **install-openclaw** — 安装和配置 OpenClaw
- **install-tokenless** — 安装和配置 Tokenless（LLM token 优化）
- **qwenpaw-usage** — QwenPaw 使用指南
- **setup-mcp** — 在 Copilot Shell 中配置 MCP 服务器

### 系统管理
- **alinux-admin** — ALinux 4 系统管理（systemd、SSH、firewalld、NetworkManager）
- **backup-restore** — 系统备份与恢复
- **storage-resize** — 阿里云磁盘扩容（XFS/EXT4/Btrfs）

### 开发运维
- **kernel-dev** — ALinux 4 内核研发自动化（SRPM 和 Upstream 方式）
- **sysom-agentsight** — 系统诊断工具
- **sysom-diagnosis** — SysOM 诊断与调优

### 阿里云
- **aliyun-ecs** — 通过阿里云 CLI 管理 ECS 实例生命周期

### 安全
- **alinux-cve-query** — 查询 Alibaba Cloud Linux CVE 漏洞信息

## 已退场技能

为聚焦系统运维，OS Skills 已将下列八个技能移出默认技能包，并结束对内置副本的常规维护。
仍需这些能力的用户可按需使用上游技能、工具或官方文档。

- `github` — [GitHub CLI](https://github.com/cli/cli/blob/trunk/skills/gh/SKILL.md)
- `humanizer` — [Humanizer](https://github.com/blader/humanizer)
- `image-gen`
- `pdf-reader`
- `xlsx` — [MiniMax XLSX](https://github.com/MiniMax-AI/skills/blob/main/skills/minimax-xlsx/SKILL.md)
- `regex-mastery`
- `shell-scripting`
- `upgrade-alinux-kernel`

## 技能格式

每个技能由独立目录组织，至少包含一个 `SKILL.md` 文件：

```
skill-name/
├── SKILL.md              # 必需：技能定义（YAML 前置元数据 + Markdown）
├── scripts/              # 可选：可执行脚本（.sh、.py）
├── reference/            # 可选：配置模板和参考文件
└── docs/                 # 可选：详细文档
```

### SKILL.md 结构

```yaml
---
name: my-skill
version: 1.0.0
description: 技能用途说明
layer: application          # application | system | core
lifecycle: production       # production | operations | usage | maintenance
tags: [example, demo]
platforms: [cosh, claude-code, gemini-cli]
---

# 我的技能

Markdown 内容，包含使用说明、前置条件和示例。
```

## 安装

### ANOLISA

```bash
anolisa install os-skills --backend raw
```

GitHub Release 和 Preview 工作流从同一个源码提交生成 Linux x86_64 与 ARM64
raw 压缩包。每个平台提供 `.tar.gz`、SHA256 校验和、CycloneDX SBOM 及其校验和。
压缩包将[发布清单](packaging/released-skills.txt)中的技能安装到 `{datadir}/skills`。
正式发布时 CI 校验 tag 与源码版本；本地发布流水线继续使用现有配置。

### 扩展安装

一次安装全部已发行技能：

```
/extensions install https://github.com/alibaba/ANOLISA?path=src/os-skills
```

或按分类单独安装：

```
/extensions install https://github.com/alibaba/ANOLISA?path=src/os-skills/ai
/extensions install https://github.com/alibaba/ANOLISA?path=src/os-skills/system-admin
/extensions install https://github.com/alibaba/ANOLISA?path=src/os-skills/devops
/extensions install https://github.com/alibaba/ANOLISA?path=src/os-skills/security
/extensions install https://github.com/alibaba/ANOLISA?path=src/os-skills/aliyun
```

或从本地路径安装：

```
/extensions install /path/to/os-skills
```

> **注意：** 扩展安装方式目前仅支持 Copilot Shell（cosh）。

### RPM 安装

```bash
sudo yum install os-skills
```

技能安装到 `/usr/share/anolisa/skills/`，由 Copilot Shell 自动发现。

### 手动安装

将技能目录复制到以下搜索路径之一：
1. 项目级：`.copilot-shell/skills/`
2. 用户级：`~/.copilot-shell/skills/`
3. 系统级：`/usr/share/anolisa/skills/`

## 编写新技能

1. 在对应分类目录下创建技能文件夹（如 `system-admin/my-skill/`）。
2. 添加包含 YAML 前置元数据和 Markdown 内容的 `SKILL.md`。
3. 可选添加 `scripts/` 和 `reference/` 目录。
4. 在 Copilot Shell 中测试技能。
5. 提交 Pull Request。

脚本回归测试统一放在本组件的 `tests/` 目录。依赖、测试命令与组件归属规则见
[贡献指南](../../CONTRIBUTING_zh.md#skill-回归测试与路径归属)。

### 孵化与默认交付

通过评审的技能可以留在原分类目录孵化。源码安装、发行包和 COSH 扩展都由发布清单
决定默认交付范围。试用者可以按上面的手动安装说明，从经过评审的源码版本复制指定技能。

源码安装和发布清单维护需要 Python 3.6 或更新版本及标准库。
解释器选择方法见[源码安装说明](../../docs/user-guide/zh/os-skills/index.md#安装)。

维护者同意纳入默认交付后，将目录加入 [`packaging/released-skills.txt`](packaging/released-skills.txt)，
在仓库根目录运行 `python3 src/os-skills/packaging/cosh_manifests.py --write`，
并在 `component.toml` 中维护适用的适配器登记。CI 核对 COSH 扩展和发布清单是否一致。

## 许可证

Apache License 2.0 — 详见 [LICENSE](../../LICENSE)。
