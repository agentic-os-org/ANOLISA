# 原生与发行验收

[English](../../en/aw/testing.md)

[native-matrix.json](../../../src/aw/tests/native-matrix.json) 是固定版本、依赖、命令、负责人和报告清单。
日常开发先运行[AW 门禁](../../../src/aw/CONTRIBUTING_zh.md)。本页用于发布或原生接点变更的验收。

## 选择验证范围

| 层级 | 验证内容 | 入口 |
| --- | --- | --- |
| fixture 门禁 | 协议、失败路径、安全与压缩组合、生命周期 | `python3 src/aw/scripts/check.py` |
| 原生 SDK / CLI | 真实框架接线与效果采用 | `native_acceptance.py`，按 Agent 选择 |
| 真实后端兼容 | Tokenless/RTK 与 AW Provider 的四项合同 | `check_tokenless.py` |
| 集成发行 | 包、后端、原生清单与常驻容器 | AW Integration workflow |

OpenClaw 自动验收覆盖官方 loader/Hook runner，完整 Gateway 模型流程需另验。
Qoder SDK 托管 MCP 仍有手工容器证据要求。以清单记录的实际边界报告结果。

## Qoder 执行资产覆盖

`qoder-sdk` 原生目标包含以下必需用例组：

| 用例组 | 验收范围 |
| --- | --- |
| `resident-parser` | 固定 SDK parser：审批后发布 wrapper、原生模式拒绝、未使用票据回收及回调/消费者所有权 |
| `native-command` | 官方 Worker：Daemon/直接执行、逻辑权限与 Post 输入、应用拒绝、default/acceptEdits 命令级 deny、超时后台 EOF 和关闭/settle 顺序 |
| `declarative-profile` | 已安装 Bridge：释放提示前确认原生设置、声明式快照可见性、Daemon 复用和 Query 清理 |

Fixture 门禁另覆盖有界资产读取、非法声明、快照清理和缓存失效。Hosted 用例需在运行时提供测试凭据。
按下方目标 runner 执行，保留各组报告，并核验原生进程和 AW scope 已退出。
Parser 的本地 transport 测试需配合真实 Worker 验证原生授权行为。

## 原生单目标

在干净、已提交的 checkout 中准备清单要求的 Python、Node 24.16.0、uv 0.11.13 和 SDK 依赖。
`target/native-binaries` 使用同一候选 SHA 的 nightly bundle 与校验文件。Qoder、QwenPaw 需要 Docker；Qoder 托管模型需运行阶段提供 PAT。
从 `src/aw` 运行，输出目录须尚未存在。

```bash
python3 -B scripts/check_native.py --candidate-sha "$(git rev-parse HEAD)"
python3 -B scripts/native_acceptance.py --agent agentscope \
  --candidate-sha "$(git rev-parse HEAD)" --bin target/native-binaries \
  --output target/native-evidence/agentscope
```

把 `--agent` 换成清单目标。测试失败或证据缺失需修复，不通过省略用例重报成功。

## 集成与常驻容器

完整主线验收使用已合入 main 的 40 位候选 SHA。fork 可运行无凭据模式，保留 Qoder 托管模型待验项目。

```bash
gh workflow run aw-preview.yml --ref main -f candidate_sha=<40-character-SHA>
gh workflow run aw-ci.yml --repo <owner>/anolisa --ref <branch> \
  -f integration=true -f credential_free=true
```

本地常驻容器复用候选 bundle，镜像输入须已提交。普通用户、合成数据和 loopback fixture 在无外网、无凭据、无宿主挂载的容器中执行。

```bash
python3 -B scripts/resident_container.py --candidate-sha "$(git rev-parse HEAD)" \
  --bin target/native-binaries --output target/resident-evidence
```

## 证据与清理

报告须包含候选和测试 SHA、包摘要、SDK 版本、执行命令、期限、逐用例结果与清理结果。
`unittest`/`tap` 核对测试数，JSON 核对具名用例或实际完成数；退出 0 之外仍需检查报告完整。
允许和阻断用例同时核对原生结果、工具标记与 AW 审计；失败和超时用例核对阻断及无延迟补执行。

执行器回收所属进程、临时 profile、socket、容器和唯一镜像标签。Docker 超时或清理失败时，按报告中的精确资源和命令核对后清理。
评审证据后删除本次创建的 `target/native-evidence/agentscope`、`target/resident-evidence`；保留共享构建缓存。
生产 endpoint、真实负载和部署退出链路另在实际部署环境验收。
