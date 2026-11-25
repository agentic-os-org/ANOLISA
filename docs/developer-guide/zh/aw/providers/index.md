# Provider 开发与分发

[English](../../../en/aw/providers/index.md)

Provider 将安全检查、输出压缩等能力接入 AW。它通过进程协议工作，可以独立开发、
分发和升级，也可以收录到 [Provider 协作目录](../../../../../providers/README_zh.md)。

## 目录与职责

```text
ANOLISA/
├── src/aw/crates/           # AW 通用框架
│   └── aw-provider/         # 通用协议与准入
└── providers/               # 各实现独立 workspace
    ├── agent-sec-core/
    └── tokenless/
```

`aw-provider` 是供 Host 和实现方复用的协议库。`providers/` 中的每个目录则对应一项
具体集成，可以包含程序、Schema、配置示例和测试。现有二进制仍叫
`aw-provider-sec-core`、`aw-provider-tokenless`，安装包和启动命令保持兼容。

## OS 分发设计

组件 owner 负责 Provider 的实现、测试、依赖与发布；OS 发行方选择组件版本，
验证来源和组合兼容性，再交付安装包与升级路径。各组件维护自己的发布周期。

一方 Provider 的目标是仅依赖固定版本的公共 AW 库，在仓库外完成构建、测试和发布，
自身后端依赖由 owner 管理。第三方可直接实现公开进程协议，通过 YAML 接入，
无需链接 Rust 库或检出 ANOLISA。分发层消费各组件产物，负责随包依赖的组合验证。

完整职责和迁移范围见[架构边界](../../../../../src/aw/docs/design/zh/architecture-boundaries.md)。

## 独立接入

可以在 ANOLISA 仓库外使用任意语言开发、测试和部署 Provider，无需链接 Rust crate。
每个方法以一次性进程执行：从 stdin 读取单个 UTF-8 JSON 请求，向 stdout 输出单个
UTF-8 JSON 响应（允许放行和策略 `block` 均以 `0` 退出；非零退出码表示执行失败）。

### 1. 三方法最小 Provider 骨架

将以下脚本保存为 `/absolute/path/to/my-provider.py`（`chmod 755`），完整实现
`aw-provider/v1alpha1` 的三个方法（`describe`、`validate_config` 与 `invoke`）：

```python
#!/usr/bin/env python3
import json
import sys


def valid_config(cfg):
    return (
        isinstance(cfg, dict)
        and isinstance(cfg.get("blocked_tools"), list)
        and all(isinstance(name, str) for name in cfg["blocked_tools"])
    )


def main():
    req = json.load(sys.stdin)
    reply = {
        "api_version": "aw-provider/v1alpha1",
        "request_id": req["request_id"],
        "status": "ok",
    }
    method = req["method"]

    if method == "describe":
        reply["operations"] = [
            {
                "name": "check",
                "events": ["tool.before", "tool.after"],
                "effects": ["observe", "block"],
            }
        ]
    elif method == "validate_config":
        if not valid_config(req.get("config")):
            reply.update(status="error", error_code="invalid_config")
    elif method == "invoke":
        cfg = req.get("config")
        if req.get("operation") != "check" or not valid_config(cfg):
            reply.update(status="error", error_code="invalid_operation_or_config")
        else:
            tool = req["event"]["tool"]["native_name"]
            blocked = req["event"]["name"] == "tool.before" and tool in cfg["blocked_tools"]
            reply["input_digest"] = req["input_digest"]
            reply["effects"] = (
                [{"type": "block", "reason_code": "tool_policy"}]
                if blocked and "block" in req["allowed_effects"]
                else []
            )
    else:
        reply.update(status="error", error_code="unsupported_method")

    json.dump(reply, sys.stdout)
    sys.stdout.write("\n")


if __name__ == "__main__":
    main()
```

### 2. 脱离 AW 本地冒烟测试

直接通过管道发送 `describe` 和 `validate_config` 请求，确认退出码为 `0` 且 stdout 返回合法 JSON：

```bash
printf '%s\n' '{"api_version":"aw-provider/v1alpha1","method":"describe","request_id":"req-1"}' \
  | /usr/bin/python3 /absolute/path/to/my-provider.py
printf '%s\n' '{"api_version":"aw-provider/v1alpha1","method":"validate_config","request_id":"req-2","config":{"blocked_tools":["Bash"]}}' \
  | /usr/bin/python3 /absolute/path/to/my-provider.py
```

### 3. 在 `aw.yaml` 中挂接并验证

以 `protocol: aw-provider/v1alpha1` 与 `transport.argv` 声明 Provider，并在事件步骤中绑定对应 `operation`：

```yaml
apiVersion: aw/v1alpha1
kind: AWConfiguration
metadata:
  name: custom-provider-demo
spec:
  daemon:
    startup: on_demand
    endpoint: auto
    state_dir: /absolute/path/to/aw-state
  execution:
    guarantee: native_hook
    default_event_budget_ms: 5000
  audit:
    enabled: true
    payload: metadata_only
  agents:
    qoder:
      adapter: qoder
      argv: [/absolute/path/to/qodercli]
  providers:
    my-policy:
      protocol: aw-provider/v1alpha1
      transport:
        type: stdio
        location: agent
        argv: [/usr/bin/python3, /absolute/path/to/my-provider.py]
      timeout_ms: 1000
      max_output_bytes: 65536
      config:
        blocked_tools: ["Bash"]
  events:
    tool.before:
      enabled: true
      required: true
      steps:
        - id: check-blocked-tools
          provider: my-policy
          operation: check
          effects: [observe, block]
          on_error: block
```

校验配置后，接入真实 Agent（或按[组件接入教程](../integration.md)本地测试）验证放行、阻断、超时与失败处理：

```bash
aw config validate --config /absolute/path/to/aw.yaml
aw run --config /absolute/path/to/aw.yaml --agent qoder
```

### 4. 独立部署

按自己的发布流程交付可执行文件及依赖，在 `transport.argv` 指向安装后的绝对路径（不要求 AW 包安装记录）；
也可按需打成本地扩展包供 `aw install --bundle` 管理（见[本地 Provider 包参考](../reference/packages.md)）。
后端所需凭据、socket 和数据目录在 Provider 自身安装说明中约定。

### 常见 Provider 接入模式

| 场景模式 | 常用事件与效果 | 参考 |
| --- | --- | --- |
| 安全／规则／语义检查 | `input.submit` 或 `tool.before` 使用 `observe`、`block` | [sec-core](sec-core.md) · [policy.rs](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-provider/examples/policy.rs) |
| 命令改写与输出压缩 | `tool.before` 使用 `replace_input`；`tool.after` 使用 `replace_result` | [Tokenless](tokenless.md) |
| 状态快照／审计观测 | `tool.before` 与 `tool.after` 使用 `observe`（外部状态快照或遥测记录） | [Provider 协议](protocol.md) |

Provider 返回候选效果；Adapter 负责在原生框架中采用。自定义实现无需链接 AW，
也不必使用 Rust。用户已有的 Agent 和模型配置继续由原生框架管理。
完整请求／响应 Schema、预算、错误码与能力准入规则见 [Provider 协议](protocol.md)。

## 收录到协作目录

按 `providers/<provider>/` 提交代码，附维护人、来源和许可证、双语使用说明及离线测试。
每个 Rust 包自带 workspace 和锁文件，并更新[布局检查](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/scripts/check_layout.py)。
新测试应接入 AW 门禁；完整贡献约定见[协作目录](../../../../../providers/README_zh.md)。

以下命令从仓库根目录执行。

```bash
cargo build --locked --manifest-path providers/agent-sec-core/Cargo.toml
cargo test --locked --manifest-path providers/agent-sec-core/Cargo.toml
cargo build --locked --manifest-path providers/tokenless/Cargo.toml
cargo test --locked --manifest-path providers/tokenless/Cargo.toml
```

目前两个 workspace 通过仓库内路径复用 AW 库，Tokenless 组合测试还依赖 sec-core
桥接库，尚不能脱离仓库独立构建。固定版本 SDK、独立发布及产物级组合测试留待后续。
集成构建共同验证框架、Provider 与后端版本；本地扩展有独立的 ID 和版本。

`make -C distribution aw-packages VERSION=0.2.2 COMPONENTS='core sec-core tokenless' OUTPUT="$PWD/target/aw-packages"` 调用 `distribution/packaging/aw/`
中的脚本构建独立 core 与 Provider 归档。本地 Provider 扩展安装可接受已核验的独立
`package.json`，格式见[本地 Provider 包参考](../reference/packages.md)，不下载社区
Provider 或解析产品名称。`aw-package` 的 Provider 文件事务保留为私有实现；
通用软件与原生包保留实际安装管理者。外部安装的 Provider 通过配置中的 `transport.argv` 接入协议，不要求
拥有 AW 安装记录。
纳入发行清单需要显式打包输入和安装验收。
