# AW

[English](README.md)

AW 提供统一配置、版本化能力合同和可嵌入的 Core 编排。`aw-config` 校验配置结构和引用；`aw-contracts` 检查数据结构及记录间关系；`aw-core` 通过调用方提供的 Host 执行固定计划，并持久记录执行事实。AW 没有独立服务进程，原生 Agent 控制和最终工具执行仍由接入方负责。

当前接口仍处于实验阶段。测试使用合成记录，实际运行时接入需要另行验证。

## 运行检查

通过 rustup 准备 Rust，并安装 Python 3 和 Node.js。Rust、rustfmt 和 Clippy
由 [rust-toolchain.toml](rust-toolchain.toml) 固定。在仓库根目录运行：

```bash
python3 src/aw/scripts/check.py
```

入口依次运行 CI 行为测试、格式检查、Clippy、完整的 locked workspace 测试、
Python/JavaScript 摘要向量和 rustdoc。缺少工具，配置、合同、计划、Core 执行或 Journal 测试目标为空或全部
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

## 嵌入 Core

`aw-core` 提供 `Core::prepare`、`Core::execute`、可信 Host/Clock/Journal 端口，
以及支持持久写入的 Linux `FileJournal`。准备阶段在调用任何 Provider 前检查完整
计划；执行阶段先记录调用再分发，Journal 确认后才返回终态结果。失败或中断的事件
仍保留占用记录，不自动重试或恢复。

所有权、取消、失败和接入约束见 [Core 执行与存储](docs/design/core-execution_zh.md)。
测试使用合成 Host；该 crate 尚未连接生产 Provider，也不证明原生宿主已采用结果。
统一检查强制三个 crate 的依赖边界及 Rust 文件 700 行上限（600 行提醒）；现有
合同校验文件单独保持 711 行非增长上限。这些检查辅助代码评审，不替代运行时验收。

## 源码参考

- [用户指南与可用范围](../../docs/user-guide/zh/user-entrypoint/aw.md)、
  [配置参考](../../docs/developer-guide/zh/aw/configuration.md)、
  [起步模板](crates/aw-config/examples/aw.minimal.yaml)、
  [完整示例](crates/aw-config/examples/aw.yaml)与
  [配置 API](crates/aw-config/src/lib.rs)
- [已注册的 Schema](schemas/)与[合成输入输出样例](tests/fixtures/contracts.json)
- [公共 API](src/lib.rs)、[记录校验](src/validation.rs)与[计划校验](src/orchestration.rs)
- [编码测试](tests/canonical.rs)、[Schema 测试](tests/schemas.rs)、
  [记录测试](tests/contracts.rs)与[计划测试](tests/orchestration.rs)

Registry 包含 21 个 Schema 资源。`crates/aw-contracts/schemas/` 中的 8 份 v1 文件仅作参考，未注册到当前库。调用方需要匹配 Schema ID 和摘要，当前没有自动版本转换。

收到 wire 记录后，先用 `canonical::parse` 严格解析字节，再检查结构。结构检查通过不代表记录之间的关系正确，也不授予执行权限。计划级检查的用法见公共 API 文档，证据认证和实际动作仍由调用方负责。

用户配置由独立的 `aw-config` crate 及其 `aw/v1alpha1` Schema 处理。
文件使用一个包含 `apiVersion`、`kind`、`metadata`、`spec` 的 `AWConfiguration`
对象，Provider 实例是 `spec.providers` 中的命名对象。Schema 识别 QwenPaw、
Qoder CLI、OpenClaw、Hermes 及全部 16 个事件名，不表示适配器已经实现。
配置中没有运行时 `status`。Provider 发现、操作与私有配置校验、原生能力准入
由后续增量交付。与既有 wire 合同的关系见[配置设计](docs/design/configuration_zh.md)。
