# 参与 AW 开发

[English](CONTRIBUTING.md)

本文说明 AW 的开发检查。仓库通用贡献流程和提交规则见[仓库贡献指南](../../CONTRIBUTING_zh.md)。

## 运行检查

通过 rustup 准备 Rust，并安装 Python 3 和 Node.js。Rust、rustfmt 和 Clippy
由 [rust-toolchain.toml](rust-toolchain.toml) 固定。在仓库根目录运行：

```bash
python3 src/aw/scripts/check.py
```

入口依次运行 CI 行为测试、格式检查、Clippy、完整的 locked workspace 测试、
Python/JavaScript 摘要向量和 rustdoc。缺少工具，配置、Provider 协议/准入、Provider Host、
合同、计划、Core 执行、命令执行、本地服务或 Journal 测试目标为空或全部
ignored、向量错误及命令失败均返回非零。每条命令都有超时限制，失败或中断时
回收其子进程组。日志标明失败命令，可在 `src/aw` 单独运行对应命令定位问题。

这些检查可由普通用户运行，无需启动 Agent 或登录服务。Cargo 会下载尚未缓存的
依赖，Schema 校验只读取随包资源。检查入口、本地服务、Provider Host 执行、命令执行及 FileJournal 要求 Linux；
本门禁不认证其他操作系统或最低支持版本。

[AW CI](../../.github/workflows/aw-ci.yml) 响应分支 push、pull request、merge group
和手动触发，校验候选提交；PR 校验合成的 merge 结果。无关变化明确返回 no-op；
范围判定错误、意外跳过或受测提交不一致均使 `AW / required` 失败。仓库管理员
需要在分支保护中选择该检查才能强制执行。工作流取消不代表门禁通过。

上游 CI 使用自部署 `anolisa-k8s-general-ci-x64` runner；fork CI 使用 GitHub 托管
Ubuntu 24.04。范围判断与完整验证分别执行：

- `AW / scope` 仅检出门禁脚本及其测试，保留完整 Git 历史，以判断整个 PR 的改动
  和基线推进带入合并结果的变化。使用 runner 自带的 Python 3.9 或更新版本运行
  范围判定及 required 结果测试，无需安装 Python 或 Node.js。
- `AW / contracts` 保留浅历史的完整工作树，供 Cargo 枚举包文件；使用 Python
  3.12.3、Node.js 24.15.0 和固定 Rust 工具链运行完整门禁，包括全部门禁行为测试。
- `AW / required` 仅检出门禁脚本，使用 runner 自带的 Python 3.9 或更新版本
  核对 job 结果及候选 SHA。

两个控制 job 使用 sparse checkout 限制文件内容的下载范围，同时保留范围判定所需的提交。
三个 job 的超时限制仍分别为 5、25、5 分钟。本地验证另使用 Linux ARM64。

## Crate 职责

| Crate | 职责 |
| --- | --- |
| `aw-contracts` | 版本化能力 Schema 及记录间关系校验 |
| `aw-config` | 期望配置解析与静态校验 |
| `aw-provider` | 外部 Provider 协议和能力准入；依赖 `aw-config` |
| `aw-core` | 通过可信运行时端口执行计划；依赖 `aw-contracts` |
| `aw-exec` | Linux 有界命令传输与所属进程组清理；独立于 Provider 协议 |
| `aw-host` | 组合配置、Provider 准入和有界传输，提供本地准备与调用；依赖 `aw-config`、`aw-provider` 和 `aw-exec` |
| `aw-service` | 独立服务、可复用客户端与原生启动器；通过 `aw-host` 执行 Hook，`aw-exec` 管理前台进程，`aw-core` Journal 保存元数据 |
| `aw-provider-sec-core` | 具体 Provider 二进制，通过 `aw-exec` 调用公开 sec-core CLI，不依赖 sec-core crate |

原生框架接入不属于这些库或服务；进程执行属于 `aw-exec`。
Provider 消息解析和离线准入保留在 `aw-provider`；`aw-host` 负责它们的执行边界，
不向原始命令传输加入 Provider 语义，也不替代 Core 的 Host/Journal 合同。
依赖及源码布局检查位于 [scripts/check.py](scripts/check.py)，
回归测试位于 [tests/test_ci_checks.py](tests/test_ci_checks.py)。
调整 crate 边界时需要同步更新检查和测试。

## 构建服务

在 Linux 的 `src/aw` 中构建 CLI 和样例 Provider：

```bash
cargo build --locked -p aw-service --bin aw
cargo build --locked -p aw-provider --example policy
```

[使用指南](../../docs/user-guide/zh/user-entrypoint/aw.md#运行本地服务演示)提供前台
服务与独立客户端的完整演示，以及状态目录和审计历史的清理说明。服务使用
`aw-service/v1alpha1` 本地协议，Provider 继续使用 `aw-provider/v1alpha1`；二者
分别版本化。

## 运行时验收

本地服务定向检查：

```bash
cargo test --locked -p aw-service
```

服务测试使用真实 Unix socket、样例 Provider 子进程和私有临时状态目录，不需要
模型、Agent 安装或云端密钥。检查共享事件预算、调用关联与单次执行、取消和关闭、
持久审计、陈旧句柄及端点所有权。fixture 负责等待所属进程并删除临时目录；构建
产物保留在 `target/`。验收边界见[本地服务合同](docs/design/local-service_zh.md)。

在 Linux 的 `src/aw` 目录中运行 Provider Host 定向检查：

```bash
cargo test --locked -p aw-host
cargo test --locked -p aw-provider-sec-core
```

这些测试使用本地 fixture 进程检查准备、请求绑定、失败报告、共享截止时间、取消和
事件步骤的单次调用。[本地示例](docs/design/provider-host_zh.md#本地示例)使用合成
Adapter 证据和工具事件运行样例 Provider，不代表原生 Agent 验收或效果已被采用。

协议与 Core 测试使用合成输入和 Host。原生接入需要单独取证，确认回调已安装、
工具按预期执行或被阻断，以及返回效果已被 Agent 采用。验证命令和结果写入 PR，
实验日志不放入 README。
