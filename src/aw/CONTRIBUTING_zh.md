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

仅 PR 修改 `src/aw/**` 时，[AW CI](../../.github/workflows/aw-ci.yml)
会对候选合并提交运行上述检查。提交 AW 改动前，请先在 Linux 上运行上面的命令。
CI 失败时，从任务日志中找到失败的命令，再到本地 `src/aw` 目录单独运行，定位问题。

## 回归矩阵与文件大小

[tests/regression-matrix.json](tests/regression-matrix.json) 把已注册 Agent、Hook
事件/效果和生命周期关联到具体可运行的 Rust 测试。编译后的 Adapter catalog
必须与矩阵、配置 schema 一致。共享 PreTool/PostTool 用例遍历矩阵中的所有
Agent，并使用明确的原生输入；新增 Agent 缺少 fixture、Hook 单元格漏填、
引用的测试被删除或标为 ignored，都会使现有 `AW / required` 门禁失败。

安全回归覆盖拒绝、扫描错误、取消和离线观察；Tokenless 覆盖新调用的重试归属、
禁止原地改参、最终文本替换、不支持/失败/无收益结果、托管 RTK 隔离和可选视图
隔离。这些 Provider fixture 与回调测试在每次相关推送执行，无需凭证。
真实 SDK/CLI 采纳、真实 Tokenless/RTK 包验收仍单独显式执行；不能把 ignored
的原生验收测试填入必需覆盖。

门禁通过 Cargo JSON 构建产物一次编译工作区测试，直接列举测试可执行文件，
再复用构建运行完整工作区回归。集成测试目标须包含可运行测试，新增目标
也会自动检查。显式声明的 `opt_in_targets` 将已安装包验收保留在独立流程；这些
目标必须存在且仅包含 ignored 测试。范围判断、候选 SHA 校验和必需状态的失败阻断保持原有行为；
不会根据修改了哪个 Adapter 而跳过共享安全或 Tokenless 回归。

Rust、Python 和 JavaScript（`.js`、`.mjs`、`.cjs`）源码、脚本和 fixture
统一限制为 699 行，达到 600 行提示。仅已有 `src/validation.rs` 保留固定的
711 行上限。按职责拆分并保留入口与断言，不给测试 fixture 豁免。
依赖/布局检查位于 [check_layout.py](scripts/check_layout.py)，编译测试清单位于
[check_inventory.py](scripts/check_inventory.py)，矩阵与 README 一致性检查位于
[check_matrix.py](scripts/check_matrix.py)。

更新支持能力后，在 `src/aw` 重新生成两份 README 的表格并运行门禁：

```bash
python3 scripts/check_matrix.py --write-readmes
python3 scripts/check.py
```

只开发门禁时，可用 Python 3.11+ 运行以下测试，无需 Rust 或 Node：

```bash
python3 -B -m unittest discover -s tests -p 'test_ci_*.py'
```

## 日常 CI 与 nightly 集成

只有改动 `src/aw/**` 的 PR 才运行 AW 检查。日常门禁保留全部无需凭证的安全、
Tokenless、生命周期、Agent/Hook 矩阵契约，以及原生验收清单和文件大小检查。
其他组件的内部改动由各自 CI 负责；AW 消费的公开契约发生变化时，需要对候选
SHA 显式运行兼容性验证。

[AW Integration](../../.github/workflows/aw-preview.yml) 每晚北京时间 03:23 运行，
也接受完整 40 位 `candidate_sha`，用于发布前手动验收。它一次构建全部四种
Preview 包，验证真实 sec-core health、Tokenless/RTK 契约、安装、服务生命周期、
可回滚的并存目录和卸载。并存验收使用同一候选构建，不认证跨版本数据迁移。
原生任务复用该次构建的二进制，并校验提交 SHA 和文件摘要。

[tests/native-matrix.json](tests/native-matrix.json) 为每个已注册 Agent 记录固定
版本、责任人、有界命令和明确的验收边界，并生成 nightly 任务矩阵。AgentScope、
Claude SDK/CLI、Hermes、OpenClaw、Pi 和 QwenPaw 有自动验收；Qoder 因 CI 未配置
原生 CLI/模型环境，明确显示 **MANUAL — not tested**，不会计为原生通过。
OpenClaw 覆盖官方 Hook runner。QwenPaw 套件覆盖原生 AgentScope middleware，
以及 AW plugin 对模拟 API 的注册契约；QwenPaw 插件加载、App/API 启动和模型
流程仍需人工验收。完整 OpenClaw Gateway 模型流程也需单独验收。
本地确定性模型无需云端密钥。

每个原生命令都必须声明报告契约：`unittest`/`tap` 要求确切的测试数量；`json`
要求结果文件及确切的用例名称或实际完成数量。执行端与最终汇总都会校验这些证据；
仅退出码为零或外层标记 `passed` 不能算作套件通过。报告计数须来自实际完成的检查，
不能写死为常量；覆盖范围变化时须同步更新清单。
缺少套件、SHA 过期或报告不完整都会使集成汇总失败。证据 artifact 保存候选 SHA、
组件包清单、已安装 SDK 版本/npm lock、命令/PID/截止时间和逐套件结果。
@kongche-jbw 负责 AW 集成失败，后端契约失败转对应组件 CODEOWNERS。发布前须
对确切发布提交重新运行，并完成已注明的人工原生验收。

日常 Rust 缓存由 PR 读取，仅 main 推送写入；重型构建缓存由 nightly 写入。
原生 Python/npm 依赖分别缓存，原生任务最多三个并行。工作流合入 main 前，
可以通过已有 AW 工作流的 `integration=true` 在分支上验证：

```bash
gh workflow run aw-ci.yml --ref <branch> -f integration=true
# After the integration workflow is on the default branch:
gh workflow run aw-preview.yml --ref <branch> -f candidate_sha=<40-character-SHA>
```

本地清单检查与原生执行使用和 CI 相同的入口：

```bash
python3 -B scripts/check_native.py --candidate-sha "$(git rev-parse HEAD)"
python3 -B scripts/native_acceptance.py --agent agentscope \
  --candidate-sha "$(git rev-parse HEAD)" --bin target/native-binaries \
  --output target/native-evidence/agentscope
```

二进制目录须包含 nightly 的 bundle 和校验文件。先安装清单指定的 Python 版本、
Node 24.15.0 和 uv 0.11.13。原生执行使用一个所属短临时目录容纳 Linux Unix
socket，并在退出时删除；保留证据放在 `target/`。原生任务使用独立 Python 和
Hermes 私有工具/环境，避免为 runner 的 loader 覆盖或共享可写缓存放宽原生
准入规则。不能通过重试或省略框架将
全量失败改报为成功。

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
| `aw-provider-tokenless` | 使用公开 Tokenless v2 CLI 的 PostTool Provider 二进制，不依赖 Tokenless crate |
| `aw-provider-sec-core` | 具体 Provider 二进制，通过 `aw-exec` 调用公开 sec-core CLI，不依赖 sec-core crate |
| `aw-package` | Rust Preview 构建、清单、安装及双 Agent 配置；`aw-service` 仅用于测试 |

原生框架接入不属于这些库或服务；进程执行属于 `aw-exec`。
Provider 消息解析和离线准入保留在 `aw-provider`；`aw-host` 负责它们的执行边界，
不向原始命令传输加入 Provider 语义，也不替代 Core 的 Host/Journal 合同。
依赖及源码布局检查位于 [scripts/check_layout.py](scripts/check_layout.py)，
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
`aw-service/v1alpha3` 本地协议，Provider 继续使用 `aw-provider/v1alpha1`；二者
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
