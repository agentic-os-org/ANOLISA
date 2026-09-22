# anolisa

[English](README.md)

ANOLISA 统一 CLI 网关——管理组件生命周期、框架适配器、OS 基础层和系统服务。anolisa 是 [ANOLISA](../../README_zh.md) 项目的主用户入口，通过一条命令完成所有组件的安装、更新、诊断和编排。

## 快速开始

```bash
# 列出可用组件
anolisa list

# 安装组件
anolisa install agent-memory

# 检查组件健康
anolisa status agent-memory

# 更新所有
anolisa update all
```

## 命令概览

### 一级命令 — 组件生命周期

| 命令 | 说明 |
|------|------|
| `list` | 列出可用组件（别名：`ls`） |
| `install` | 从配置的后端安装组件（raw / rpm） |
| `uninstall` | 卸载组件 |
| `update` | 更新组件、CLI 自身（`self`）或全部（`all`） |
| `upgrade` | 应用 RPM/system image 升级计划（system scope） |
| `status` | 显示组件健康状态 |
| `doctor` | 诊断问题并给出修复建议 |
| `logs` | 查询组件日志（`--severity`，别名 `--level`） |
| `restart` | 重启组件服务（`--dry-run` 仅列出 unit，不执行重启） |
| `repair` | 手工修改 RPM 后协调 ANOLISA 状态与 rpmdb |
| `adopt` | 将已有 system RPM 记录为已采纳安装 |
| `forget` | 仅删除状态记录，不执行包操作 |

`status` 和 `doctor` 允许修改 raw 安装中 `type = "config"` 文件的内容，
同时继续检查文件是否存在、权限和路径安全。

### 二级命令 — 管理

| 命令 | 说明 |
|------|------|
| `adapter` | 管理组件到框架的适配器 |
| `osbase kernel` | 内核模块与 eBPF 管理 |
| `osbase sandbox` | 沙箱运行时管理（runc、gvisor、firecracker 等） |
| `osbase security` | 安全覆盖层管理 |
| `system` | 系统辅助守护进程生命周期 |
| `register` | 加入/离开 Agentic OS Co-Build Program |
| `env` | 显示环境检测结果 |
| `bug` | 生成 bug 报告（`--component cosh-ng` 时附加 cosh-shell 诊断包摘要；`COSH_SHELL_BIN` 可覆盖二进制路径） |

执行 `anolisa adapter enable <component> openclaw` 即同意插件声明的能力，
CLI 会在宿主支持 capability consent 时传递对应参数。此操作不授予
unsafe-install 覆盖权限。

通过 `anolisa adapter enable tokenless opencode` 启用 Tokenless 本地插件。
ANOLISA 管理插件链接；启用或禁用后需重启 OpenCode。
自定义配置目录及已有安装的接管规则见
[adapter 参考](../../docs/user-guide/zh/user-entrypoint/anolisa-cli.md#adapter)。

## 安装模式

| 模式 | 前缀 | 使用场景 |
|------|------|----------|
| `system` | `/usr/local`（或自定义 `--prefix`） | root 运行时的默认值 |
| `user` | `~/.local` | 普通用户运行时的默认值 |

可通过 `--install-mode user|system` 显式选择。

只读发现范围比修改范围更广：普通用户可以列出、检查和诊断可见的
system 安装，也可以让 adapter 识别它；生命周期修改仍然只作用于所选
scope。因此，即使 system scope 已安装同名组件，
`anolisa --install-mode user install <component>` 仍可创建独立的 user 安装。

## 全局选项

| 参数 | 作用 |
|------|------|
| `--dry-run` | 输出计划但不执行 |
| `--json` | 输出机器可读的 JSON |
| `-v, --verbose` | 增加详细程度 |
| `-q, --quiet` | 隐藏非错误输出 |
| `--no-color` | 禁用彩色输出 |

完整的命令形式、scope 行为与恢复流程见
[CLI 用户指南](../../docs/user-guide/zh/user-entrypoint/anolisa-cli.md)。

RPM 安装支持 Yum 3 和 DNF 4。使用 `sudo` 执行 RPM `--dry-run`，
可在安装前检查依赖和冲突。

## 架构

五 crate Cargo workspace：

| Crate | 职责 |
|-------|------|
| `anolisa-cli` | 命令解析、分发、终端 UI |
| `anolisa-core` | 组件解析、适配器管理、osbase 安装逻辑 |
| `anolisa-env` | 环境检测（发行版、架构、能力） |
| `anolisa-build` | 构建时代码生成与资源嵌入 |
| `anolisa-platform` | 文件系统布局、原生包查询与事务、systemd、IPC、权限辅助 |

支持双后端：**raw**（OSS tar.gz）和 **RPM**（yum/dnf 事务）。生命周期
planner 区分 ANOLISA 自有文件与 native package authority，并在副作用前
记录崩溃恢复意图。组件元数据通过 `component.toml` 声明。

raw backend 每次解析都会重新拉取 distribution index，仓库不可达时命令直接
失败。`repo.toml` 中旧的 `cache_ttl_secs` 和 `offline_fallback` 字段仍可正常
解析以保持向后兼容，但当前 raw backend 不会使用这些值。

## 环境要求

- Linux（x86_64 / aarch64）或 macOS（arm64，功能受限）
- 从源码构建需要 Rust ≥ 1.93

## 许可证

Apache License 2.0 — 详见 [LICENSE](../../LICENSE)。
