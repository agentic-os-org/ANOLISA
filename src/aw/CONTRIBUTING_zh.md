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
Python/JavaScript 摘要向量和 rustdoc。缺少工具，配置、Provider 协议/准入、合同、计划、Core 执行或 Journal 测试目标为空或全部
ignored、向量错误及命令失败均返回非零。每条命令都有超时限制，失败或中断时
回收其子进程组。日志标明失败命令，可在 `src/aw` 单独运行对应命令定位问题。

这些检查可由普通用户运行，无需启动 Agent 或登录服务。Cargo 会下载尚未缓存的
依赖，Schema 校验只读取随包资源。检查入口要求 Linux；库本身仍可移植，但本门禁
不认证其他操作系统或最低支持版本。

[AW CI](../../.github/workflows/aw-ci.yml) 响应分支 push、pull request、merge group
和手动触发，校验候选提交；PR 校验合成的 merge 结果。无关变化明确返回 no-op；
范围判定错误、意外跳过或受测提交不一致均使 `AW / required` 失败。仓库管理员
需要在分支保护中选择该检查才能强制执行。工作流取消不代表门禁通过。

上游 CI 使用自部署 `anolisa-k8s-general-ci-x64` runner；fork CI 使用 GitHub 托管
Ubuntu 24.04。两者均使用 Python 3.12.3、Node.js 24.15.0 和固定 Rust 工具链。
本地验证另使用 Linux ARM64。

## Crate 职责

| Crate | 职责 |
| --- | --- |
| `aw-contracts` | 版本化能力 Schema 及记录间关系校验 |
| `aw-config` | 期望配置解析与静态校验 |
| `aw-provider` | 外部 Provider 协议和能力准入；依赖 `aw-config` |
| `aw-core` | 通过可信运行时端口执行计划；依赖 `aw-contracts` |

框架接入与进程执行由这些库之外的组件负责。
依赖及源码布局检查位于 [scripts/check.py](scripts/check.py)，
回归测试位于 [tests/test_ci_checks.py](tests/test_ci_checks.py)。
调整 crate 边界时需要同步更新检查和测试。

## 运行时验收

协议与 Core 测试使用合成输入和 Host。原生接入需要单独取证，确认回调已安装、
工具按预期执行或被阻断，以及返回效果已被 Agent 采用。验证命令和结果写入 PR，
实验日志不放入 README。
