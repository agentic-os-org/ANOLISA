# agent-sec-core Development Standards

本仓库包含多个组件，请根据你要修改的模块查阅对应章节：

| 组件 | 语言 | 路径 | 章节 |
|------|------|------|------|
| agent-sec-cli | Python + Rust | agent-sec-cli/ | [agent-sec-cli](#agent-sec-cli) |
| hermes-plugin | Python (stdlib) | hermes-plugin/ | [hermes-plugin](#hermes-plugin) |
| cosh-extension | Python (hooks) | cosh-extension/ | [cosh-extension](#cosh-extension) |
| openclaw-plugin | TypeScript | openclaw-plugin/ | [openclaw-plugin](#openclaw-plugin) |
| linux-sandbox | Rust | linux-sandbox/ | [linux-sandbox](#linux-sandbox) |
| skills | Shell/Python | skills/ | [skills](#skills) |

---

## agent-sec-cli

### 1. 项目概述

agent-sec-cli 是面向 AI Agent 的安全 CLI 工具，提供系统加固、沙箱策略生成、资产完整性验证、代码安全扫描、提示词安全检测和安全事件追踪等功能。

**关键目录结构：**

```
agent-sec-cli/
├── src/agent_sec_cli/        # 主 Python 包
│   ├── cli.py                # 统一 CLI 入口
│   ├── asset_verify/         # 资产完整性验证（GPG 签名）
│   ├── code_scanner/         # 代码安全扫描
│   ├── prompt_scanner/       # 提示词安全检测（ML 分类器）
│   ├── sandbox/              # 沙箱策略生成
│   ├── security_events/      # 安全事件日志
│   ├── security_middleware/  # 统一中间件层（路由+后端）
│   └── skill_ledger/         # 技能账本管理
├── src/lib.rs                # Rust 原生模块入口（PyO3）
├── pyproject.toml            # 构建配置 + lint/格式化配置
├── Cargo.toml                # Rust 依赖
└── uv.lock                   # 依赖锁定文件
tests/                        # 测试目录（位于 agent-sec-core/ 下）
├── unit-test/                # 单元测试
├── integration-test/         # 集成测试
└── e2e/                      # 端到端测试
```

### 2. 环境准备

- **Python 版本**: 严格固定 `3.11.6`（`pyproject.toml` 中 `requires-python = "==3.11.6"`）
- **包管理器**: [uv](https://docs.astral.sh/uv/)，管理依赖和虚拟环境
- **Rust 构建**: [maturin](https://www.maturin.rs/)，编译 PyO3 原生扩展为 `.so`
- **初始化环境**:

```bash
cd agent-sec-cli && uv sync
```

> uv 会自动创建 `.venv` 并安装所有依赖（含 dev group）。

### 3. 依赖管理

| 场景 | 命令 | 说明 |
|------|------|------|
| 安装所有依赖（含 dev） | `uv sync` | 自动创建 .venv 并安装 |
| 仅安装运行时依赖 | `uv sync --no-group dev` | 生产环境用 |
| 添加运行时依赖 | `uv add <pkg>` | 自动更新 pyproject.toml 和 uv.lock |
| 添加 dev 依赖 | `uv add --group dev <pkg>` | 写入 [dependency-groups].dev |
| 添加可选依赖 | `uv add --optional <group> <pkg>` | 写入 [project.optional-dependencies]，如 `uv add --optional pgpy pgpy` |
| 删除依赖 | `uv remove <pkg>` | 同时清理 pyproject.toml 和 uv.lock |
| 更新单个依赖 | `uv lock --upgrade-package <pkg>` | 仅升级指定包 |
| 更新所有依赖 | `uv lock --upgrade` | 重新解析所有版本 |
| 运行命令 | `uv run <cmd>` | 在 .venv 环境中执行 |
| 运行测试 | `make test-python` | 从 agent-sec-core 目录执行 |
| 构建 wheel | `make build-cli` | maturin + Python 3.11 |

> **重要**: 修改依赖后务必提交更新后的 `pyproject.toml` 和 `uv.lock`。

### 4. 代码格式化

使用 **black + isort** 进行代码格式化（配置在 `agent-sec-cli/pyproject.toml`）：

- `line-length = 100`
- `target-version = py311`
- `isort` profile = "black"

```bash
# 从 agent-sec-core 目录执行
make python-code-pretty
```

> 格式化排除 `dev-tools/backend-skill/templates/` 目录（含 Jinja 模板）。

### 5. 静态检查 (ruff lint)

使用 [ruff](https://docs.astral.sh/ruff/) 进行静态检查（仅 lint，不做格式化）。

**启用规则：**

| 规则 | 说明 |
|------|------|
| F | pyflakes — 未使用 import、未定义变量等逻辑错误 |
| E, W | pycodestyle — PEP 8 编码风格（E501 行超长已 ignore） |
| I | isort — import 排序 |
| TID252 | 禁止相对导入 |
| PLC0415 | 禁止函数体内导入 |
| ANN001 | 函数参数必须标注类型 |
| ANN201 | 公有函数必须标注返回类型 |
| ANN202 | 私有函数必须标注返回类型 |
| S602 | 禁止 subprocess shell=True |
| S605 | 禁止 os.system() |
| S606 | 禁止 os.popen() |
| S108 | 禁止硬编码 /tmp 路径 |
| PLW1510 | subprocess.run() 必须指定 check |
| SIM115 | open() 必须使用 with |
| B006 | 禁止可变默认参数 |
| B008 | 禁止默认参数中调用函数 |

**已禁用规则：**

| 规则 | 原因 |
|------|------|
| PTH (pathlib 强制) | 存量代码中 os.path 使用过多，暂不启用，待后续逐步治理 |
| E501 (行超长) | 由格式化工具自动处理 |

**豁免规则：**

| 作用范围 | 豁免规则 | 原因 |
|----------|----------|------|
| `tests/**` | ANN（类型注解） | 测试代码标注类型收益低 |
| `tests/**` | S（安全规则） | 测试需构造危险输入验证防护逻辑 |
| ML lazy import 行 | PLC0415 | torch/transformers 等重型依赖延迟加载，用 `# noqa: PLC0415` 豁免 |

**命令：**

```bash
# 全量检查（从 agent-sec-core 目录）
make python-lint

# 增量检查（仅报告相对 upstream/main 变更行的违规，含未提交修改）
make python-lint-ci

# 自定义对比分支
make python-lint-ci COMPARE_BRANCH=origin/main
```

> `python-lint-ci` 对比范围包含 committed + staged + unstaged 变更，无需先 commit。

### 6. 导入规范

- **绝对导入**: 所有 import 使用绝对路径 `from agent_sec_cli.xxx import yyy`
- **禁止相对导入**: `from .xxx import` 或 `from ..xxx import` 一律禁止
- **禁止动态导入**: `importlib.import_module()` 和 `__import__()` 禁止使用
- **禁止函数体内导入**: 所有 import 必须在文件头部

**例外 — ML 延迟加载：** 对于重型 ML 依赖（torch、transformers、modelscope），允许在实际推理时才导入，需添加行内注释：

```python
def predict(self, text: str) -> float:
    import torch  # noqa: PLC0415 - lazy import: only needed when running ML inference
    from transformers import AutoModel  # noqa: PLC0415
    ...
```

### 7. 类型注解

- 所有函数/方法必须标注**参数类型**和**返回类型**
- 使用 Python 3.11 原生语法：`dict[str, Any]`、`str | None`、`list[int]`
- 无需 `from __future__ import annotations`
- `tests/` 目录下所有文件豁免类型注解要求

```python
# 正确
def process(name: str, count: int, items: list[str]) -> dict[str, Any]:
    ...

# 错误 — 缺少类型标注
def process(name, count, items):
    ...
```

### 8. 编码风格

**通用规范：**

- 空函数/抽象方法使用 `pass` 占位，不使用 `...`（Ellipsis）
- 数据类优先使用 `pydantic`
- 路径操作优先使用 `pathlib.Path`，而非 `os.path`
- 禁止使用可变对象（`[]`、`{}`、`set()`）作为函数默认参数（B006）
- 禁止在默认参数中调用函数（B008），如 `def f(x=time.time())` 是错误写法

**Import 规范：**

- import 排序由 isort 自动管理（I）
- 禁止相对导入（TID252）：使用 `from agent_sec_cli.xxx import yyy`
- 禁止函数体内导入（PLC0415）：所有 import 放在文件顶部

**类型标注：**

- 函数参数必须标注类型（ANN001）
- 公有函数必须标注返回类型（ANN201）
- 私有函数必须标注返回类型（ANN202）

**安全规范：**

- 禁止 `subprocess` 使用 `shell=True`（S602）
- 禁止使用 `os.system()`（S605）
- 禁止使用 `os.popen()`（S606）
- 禁止硬编码 `/tmp` 路径（S108），应使用 `tempfile` 模块
- `subprocess.run()` 必须显式指定 `check` 参数（PLW1510）
- `open()` 必须使用 `with` 上下文管理器（SIM115）

### 9. 测试

- **框架**: pytest
- **测试目录结构**:
  - `tests/unit-test/` — 单元测试
  - `tests/integration-test/` — 集成测试
  - `tests/e2e/` — 端到端测试
- **测试文件放置**: 统一放在 `tests/` 目录下，不放入 `agent-sec-cli/` 内部
- **e2e 测试要求**: 必须同时支持两种调用方式：
  1. **二进制 CLI 调用**（subprocess）：`subprocess.run(["agent-sec-cli", "scan-code", "--code", code, "--language", "bash"], ...)`
  2. **Python 模块回退**：`subprocess.run(["python", "-m", "agent_sec_cli.cli", "scan-code", ...], ...)`

  两种方式均以字符串数组传参（不经 shell 解析），保障参数完整性。

**常用命令（从 agent-sec-core 目录执行）：**

```bash
make test-python           # 运行单元 + 集成 + CLI e2e 测试
make test-python-coverage  # 运行测试并生成覆盖率报告
```

### 10. 构建

```bash
make build-cli             # 构建 wheel（maturin + Python 3.11）
make export-requirements   # 从 uv.lock 导出 requirements.txt
```

- Rust 原生扩展通过 PyO3 编译为 `_native.cpython-311-*.so`，随 wheel 分发
- 构建产物位于 `agent-sec-cli/target/wheels/`
- **非 .py 文件打包**: 新增的非 Python 文件（如 `.yaml`、`.conf`、`.asc`、`.json` 等）如果需要随 wheel 分发，必须在 `pyproject.toml` 的 `[tool.maturin].include` 中添加对应路径：

```toml
[tool.maturin]
include = [
    "src/agent_sec_cli/asset_verify/config.conf",
    "src/agent_sec_cli/asset_verify/trusted-keys/*.asc",
    "src/agent_sec_cli/code_scanner/rules/**/*.yaml",
    "src/agent_sec_cli/prompt_scanner/rules/*.yaml",
    # 新增资源文件在此添加
]
```

### 11. CI 检查项

| 检查项 | 范围 | 失败行为 |
|--------|------|----------|
| black + isort 格式化 | 全量代码 | 存在未格式化代码则 CI 失败 |
| ruff lint（增量） | 仅 PR 变更行 | **不卡点**，违规以 warning 显示在 CI Summary |
| pytest --cov | 全量测试 | 测试失败则 CI 失败 |
| 增量代码覆盖率 | 仅 PR 变更行 | 新增/修改代码覆盖率 < 80% 则 CI 失败 |
| uv lock --check | 依赖锁文件 | uv.lock 与 pyproject.toml 不同步则 CI 失败 |

> Lint 检查仅在 PR 触发时对增量代码检查，不检查历史代码。违规信息显示在 PR 的 Job Summary 区域。
> 增量覆盖率门禁仅在 PR 触发，要求本次 PR 新增/修改的代码行中被测试覆盖的比例 ≥ 80%。

### 12. Capability View Maintenance

`agent-sec-cli capabilities` is the canonical read-only environment-variable view of agent-sec plugin hook capabilities. Treat it as part of the hook environment contract, but do not treat it as proof of the target Agent's live runtime state.

The view and the hook runtime have an implicit dependency: they must interpret hook environment variables the same way while remaining deployment-decoupled. Hook code must not import `agent_sec_cli` to share parsing logic, because hooks and the CLI are installed and executed in different raw/RPM/plugin layouts.

When changing any Agent plugin hook environment behavior, update the capability view in the same PR. This includes:

- Adding, removing, or renaming a hook or capability in Qoder, Qwen Code, Codex, Cosh, OpenClaw, or Hermes integrations
- Changing environment variables, defaults, accepted values, timeout behavior, mode/policy semantics, legacy fallback behavior, or enabled/disabled behavior
- Changing hook matchers or hook names that appear in manifests such as `hooks.json`, `qwen-extension.json`, `cosh-extension.json`, `openclaw.plugin.json`, or `hermes-plugin/src/plugin.yaml`

Required synchronized updates:

1. Update `agent-sec-cli/src/agent_sec_cli/capabilities/` metadata and parsing logic.
2. Update CLI/user documentation for `agent-sec-cli capabilities`.
3. Update unit/e2e tests that lock each Agent's capability, hook, and environment-variable mapping.
4. Add or update tests that verify CLI parsing semantics match existing hook semantics for shared environment variables.

Important scope boundary: `agent-sec-cli capabilities` reads only the current CLI process environment variables. It must not read OpenClaw, Hermes, or other Agent configuration files; it must not resolve Agent home directories; and its output must not be described as actual hook load, registration, or runtime-effective state. Config-driven differences are allowed to exist and must be documented as known drift.

Qwen Code PII currently has a specific compatibility fallback: `PII_CHECKER_HOOK_ENABLED` takes precedence, and legacy `PII_CHECKER_ENABLED` is consulted only when the new variable is absent. Do not generalize that fallback to other Agents unless their hook runtime implements it too.

### 13. Rust Migration Architecture

`docs/design/AGENT_SEC_RUST_MIGRATION_zh.md` is the repository source of truth
for the V2 product architecture, process topology, crate boundaries, migration
strategy, and the transition from the V1 implementation and contracts. Keep
the complete rationale and all actionable constraints in repository-accessible
documents; development or acceptance must not depend on a private document.

The current six language-independent behavior contracts are:

- `docs/design/DAEMON_CURRENT_BEHAVIOR_zh.md`
- `docs/design/DAEMON_PROTOCOL_V1_zh.md`
- `docs/design/DAEMON_JOB_CONTRACT_zh.md`
- `docs/design/DAEMON_PROCESS_DEPLOYMENT_CONTRACT_zh.md`
- `docs/design/SECURITY_MIDDLEWARE_CONTRACT_zh.md`
- `docs/design/SECURITY_ACTIONS_REFERENCE_zh.md`

`docs/design/RUST_SECURITY_CORE_EXECUTION_ARCHITECTURE_zh.md` is the current
Rust-specific implementation proposal for the V1 middleware to V2 Action
Runtime/CapabilityExecutor path and observability lifecycle. It is not a
seventh behavior contract. Treat
its `[OPEN]` items as unresolved until they are promoted into the relevant
language-independent contract and executable fixtures.

The six contracts freeze V1 facts, observable behavior, and regression oracles;
they do not override the repository V2 migration architecture. Use the scope labels
consistently: `[CURRENT]` is observable V1 behavior, `[PRESERVE V1]` is behavior
that a supported compatibility surface must retain, `[TARGET V2]` is a target
consistent with the repository V2 migration plan, `[SUPERSEDED]` is an earlier
target replaced by that plan, and `[HISTORICAL]` is evidence only. A V1 interface may change only
through explicit compatibility classification, a versioned replacement and an
approved change record. Do not silently promote Python process layout, PyO3,
per-user deployment, or another V1 implementation detail into a V2 product
requirement.

A change to a supported method, action, field, default, error, lifecycle event,
redaction rule, state format, or side effect must update the relevant contract
and executable conformance fixtures in the same change. During migration,
capture supported V1 behavior with an executable V1 oracle or frozen golden
fixtures, and require the corresponding V2 work package to pass those fixtures
before completion. After an approved versioned transition retires V1 support,
maintain only the current V2 contract and conformance fixtures; retaining or
running the V1 implementation is not required. Compatibility can be supplied by
a Rust binary, protocol adapter, state migrator, or versioned interface; it does
not require retaining Python in the V2 runtime.

Daemon Job changes must update `DAEMON_JOB_CONTRACT_zh.md` and its `DJOB-*`
fixtures. Keep long-lived service health separate from the most recent run
outcome without requiring a generic dual-state-machine framework. For each
concrete background service, define its trigger, readiness relationship,
failure/retry boundary, cancellation, shutdown, health, logging, tracing, and
restart behavior. Do not implement a generic periodic scheduler until a reviewed
production consumer exists, and do not treat the current Python worker process
or class hierarchy as a Rust compatibility requirement.

Daemon entrypoint, package layout, systemd, runtime/singleton, identity,
authorization, query scope, data-directory, or diagnostic-log changes must update
`DAEMON_PROCESS_DEPLOYMENT_CONTRACT_zh.md` and its `DPROC-*` fixtures. Do not
treat systemd `active` as daemon readiness, and do not use local-only sidecar or
healthcheck files that are absent from main as CURRENT/PRESERVE evidence.

1. V2 product runtime code is Rust, including `asc-cli`, `asc-daemon`, security
   capabilities, Policy Engine, JobSupervisor, state migration, and delivery
   entrypoints. V1 Python code is an oracle and migration input, not a V2
   runtime dependency.
2. V2 uses one `asc-daemon` per host as a system-level service: system-scope
   systemd on Linux and one DaemonSet instance per Kubernetes node. Do not add a
   per-user/user-session daemon or place V2 socket/state ownership under HOME or
   `$XDG_RUNTIME_DIR`.
3. `asc-cli` is a Rust daemon client. It must not own daemon lifecycle, use
   PyO3, or automatically execute a local fallback when the daemon is absent.
   Any direct local execution for a pure function requires a separate approved
   product contract and is not a general fallback.
4. All entrypoints use `asc-daemon-core`. The server constructs trusted
   principals from peer credentials and authenticated bindings; caller-supplied
   UID, role, or scope is attribution only. Query scope is server-generated,
   and CLI/TUI must not bypass authorization by reading SQLite directly.
5. `asc-daemon` is the process and composition root. Reusable application logic
   belongs in Action Runtime and Policy Runtime, not in transport handlers or
   the binary bootstrap layer.
6. Do not use `SecurityBackend` as a universal abstraction. Use the bounded V2
   roles `CapabilityExecutor`, `ContextProvider`, `EnforcementAdapter`, and
   `Repository`/`Client`/`Sink`. Security capabilities do not depend on the
   Policy Compiler; they exchange stable Evidence/AttributeBundle contracts.
7. Migrate by contract-first crate work packages with rolling source baselines
   and direct-dependency revisions. Each crate can start after its own
   Definition Review and is accepted with its direct consumers plus the
   relevant integration slice. Do not require backend-first/daemon-last or a
   single global decision freeze.
8. Reuse the V1 contracts as per-crate discovery and differential fixtures.
   Supported CLI/RPC/schema/config/state behavior must remain compatible or use
   an approved, versioned transition. The current 9 daemon methods are V1 fact;
   proposed action methods must be classified in the daemon protocol task and
   must not be presented as already implemented.
9. Expose daemon security actions through explicit allowlisted protocol
   handlers. Each method maps to one Action Runtime operation; do not add a
   generic endpoint that accepts an arbitrary action name. Retain the three
   response layers where the V1 compatibility interface is supported:
   transport failure, daemon failure (`ok=false`), and action result (`ok=true`,
   including failed action results).
10. Treat the current opaque `trace_id` as V1 compatibility behavior only.
    The V2 target uses OpenTelemetry as the sole authority for TraceId, SpanId,
    parentage, and `traceparent/tracestate` propagation. AgentSec owns only
    Agent/security span semantics and bounded attributes; do not add a second
    custom trace ID or map arbitrary V1 values into OTel TraceId. SecurityEvent
    persistence remains independent of sampling, exporters, and collectors.
11. Each crate task must record its V1 relationship and acceptance type, run the
    applicable migration or current-version conformance fixtures, and provide a
    pass/fail matrix, external compatibility report, internal contract change
    record, direct consumer evidence, and rollback procedure. Markdown-only
    acceptance IDs do not complete a gate.

---

## raw packaging

### Adapter Python hooks

- Keep Python hook commands in shared JSON manifests in the existing
  `"command": "python3 ..."` form. `packaging/raw/adapt_payload.py` relies on that
  form to rewrite staged raw hooks to `agent-sec-python`.
- When adding, renaming, or removing a Python hook manifest, update
  `RAW_HOOK_MANIFESTS` in both `packaging/raw/adapt_payload.py` and
  `packaging/raw/verify_release.py`, plus the source/raw manifest lists and bypass
  cases in `tests/packaging/test-package-raw.sh`.
- Run `bash tests/packaging/test-package-raw.sh` after changing an adapter manifest
  or the raw manifest inventory.

---

## hermes-plugin

### 1. 项目概述

hermes-plugin 是面向 [Hermes Agent](https://hermes-agent.nousresearch.com/) 的安全插件，通过 Hook 机制拦截危险操作，底层调用 agent-sec-cli 进行安全扫描。

**设计原则：**

- **Fail-open** — 任何异常都不阻塞 agent 运行，hook 内部捕获所有异常返回 `None` 放行
- **零运行时依赖** — 仅使用 Python 3.11 标准库（tomllib、json、subprocess、logging、dataclasses）
- **可配置行为** — 默认 observe（仅日志）。Code Scanner 使用
  `enable_block = true` 启用阻断；PII Checker 和 Skill Ledger 使用
  `policy = "block"` 在 Hermes 原生支持的 hook 边界阻断

**目录结构：**

```
hermes-plugin/
├── scripts/
│   └── deploy.sh             # 部署脚本
├── src/                      # 运行时文件（部署到 ~/.hermes/plugins/）
│   ├── plugin.yaml           # Hermes 插件 manifest
│   ├── __init__.py           # register(ctx) 入口
│   ├── config.toml           # 能力开关与参数
│   ├── registry.py           # 能力注册器 + safe-wrap
│   ├── cli_runner.py         # agent-sec-cli subprocess 封装
│   └── capabilities/
│       ├── __init__.py       # 能力清单
│       ├── base.py           # AgentSecCoreCapability 抽象基类
│       ├── code_scan.py      # Code Scanner 实现
│       └── pii_scan.py       # PII Checker 实现
└── README.md                 # 开发指南
tests/unit-test/hermes-plugin/ # 单元测试（位于 agent-sec-core/tests/unit-test/ 下）
```

### 2. 导入规范

Hermes 以包形式加载插件，模块间**必须使用相对导入**：

```python
# 正确：相对导入
from .registry import load_config              # 同级模块
from .capabilities import ALL_CAPABILITIES     # 同级子包
from ..cli_runner import call_agent_sec_cli    # 上级模块（在子包中）

# 错误：裸名导入（插件目录不在 sys.path）
# from registry import load_config
```

**依赖分层（无循环依赖）：**

- 底层：`cli_runner.py`（纯 stdlib，无内部依赖）
- 中间层：`registry.py`（纯 stdlib）
- 基类层：`capabilities/base.py`（依赖 registry）
- 实现层：`capabilities/*.py`（继承 base，依赖 cli_runner）
- 顶层：`__init__.py`（依赖 capabilities、registry）

### 3. 编码风格

| 规范 | 要求 |
|------|------|
| 格式化 | black + isort（同 agent-sec-cli） |
| lint | 不适用 ruff（stdlib-only 项目，规则不兼容） |
| 日志 | `logging.getLogger("agent-sec-core")`，f-string 格式 |
| 类型注解 | 不强制（非 ruff 管辖） |
| 注释 | 英文 |

### 4. 新增 Capability

1. 在 `src/capabilities/` 下新建 `xxx.py`
2. 继承 `AgentSecCoreCapability`，定义 `id`、`name`（基类通过 `@property` + `@abstractmethod` 强制），实现 `_on_register()`、`get_hooks_define()` 和回调方法
3. 在 `capabilities/__init__.py` 中导入并加入 `ALL_CAPABILITIES`
4. 在 `config.toml` 中添加对应配置段 `[capabilities.<id>]`（`enabled` 和 `timeout` 必填）

```python
from .base import AgentSecCoreCapability


class MyCapability(AgentSecCoreCapability):
    id = "my-cap"
    name = "My Capability"

    def _on_register(self, config: dict) -> None:
        self._my_option = config.get("my_option", "default")

    def get_hooks_define(self) -> dict:
        return {"pre_tool_call": self._on_pre_tool_call}

    def _on_pre_tool_call(self, tool_name, args, **kwargs):
        ...
```

### 5. 可用 Hook

| Hook | 触发时机 | 回调签名 | 阻断方式 |
|------|----------|----------|----------|
| `pre_tool_call` | 工具执行前 | `(tool_name, args, **kwargs)` | 返回 `{"action": "block", "message": str}` |
| `post_tool_call` | 工具执行后 | `(tool_name, result, **kwargs)` | 无阻断 |
| `pre_llm_call` | LLM 调用前 | `(messages, **kwargs)` | 注入 context |
| `post_llm_call` | LLM turn 完成后 | `(assistant_response, **kwargs)` | 无阻断 |
| `transform_llm_output` | 响应完成后的输出变换（本插件不注册） | `(response_text, session_id, **kwargs)` | 替换 hook 返回值 |

### 6. 配置（config.toml）

```toml
[capabilities.code-scan]
enabled = true          # 是否注册该能力（必填）
timeout = 10            # agent-sec-cli 子进程超时（秒，必填）
enable_block = false    # false=observe(仅日志), true=block(阻断)

[capabilities.pii-scan-user-input]
enabled = true
timeout = 10
include_low_confidence = false
policy = "observe"
```

- `enabled = false` → 能力完全不注册
- `code-scan.enable_block = false` → 检测到风险时仅记 WARNING 日志，不阻断工具调用
- `code-scan.enable_block = true` → 检测到 deny/warn 时阻断工具调用
- Hermes 原生 policy 仅支持 `observe`、`block`；旧 `warn`、`ask` 配置降级为
  `observe` 并写宿主诊断
- PII scanner 覆盖本轮用户输入、tool 参数/结果和最终模型回复；模型回复只在
  `post_llm_call` 审计，不修改或阻断；不扫描 history、memory 或 RAG context
- `block + deny` 在 `pre_tool_call` 返回原生 block；其它不可阻断边界仅审计

### 7. 测试

```bash
# 从 agent-sec-core 目录执行
uv run --project agent-sec-cli pytest tests/unit-test/hermes-plugin/ -v
```

### 8. 部署

```bash
./hermes-plugin/scripts/deploy.sh
```

`deploy.sh` 会将 `src/` 目录内容复制到 `~/.hermes/plugins/agent-sec-core-hermes-plugin/`。

---

## cosh-extension

> TODO: 待补充

---

## openclaw-plugin

> TODO: 待补充

---

## linux-sandbox

> TODO: 待补充

---

## skills

### security-observability

- `skills/security-observability/SKILL.md` intentionally keeps a self-contained
  parameter and output contract for `agent-sec-cli events` and
  `agent-sec-cli observability report`. Do not replace it with instructions that
  require the Agent to run `--help` before each use; that adds avoidable tool
  calls and context-reconstruction cost.
- The duplicated parameter table is an implicit contract with the CLI help text.
  When changing `events` or `observability report` options, defaults,
  mutual-exclusion rules, output formats, or documented fields, update the
  corresponding SKILL.md section and contract tests in the same change.
- Verify the source CLI definitions before editing the skill. Treat `--summary`
  and table/text output as human-display surfaces; structured Agent parsing must
  keep using JSON or JSONL examples.
- The "风险审查" section is a gate, not reference material. Security conclusions
  must come from its aggregation commands over the per-event verdict field
  (`details.result.verdict`). Never let the skill summarize by top-level
  `result` or by `observability report`'s `security_verdicts`: both aggregate
  whether the scanner *ran*, and top-level `result` is `succeeded` for
  practically every event, so summarizing by it answers "no risk"
  unconditionally. A regression once reported "no security events" for a
  session holding a `prompt_scan` deny and a `code_scan` warn.
- The aggregation scope is the four *scan* event types only: `code_scan`,
  `prompt_scan`, `pii_scan`, `skill_ledger`. Non-scan events (`sandbox_prehook`,
  `harden`, `verify`, `summary`) are filtered at the pipe entry via an
  allowlist `select`, not scored. This allowlist is fail-open: a newly added
  *scan* `event_type` is silently dropped until it is added to both `$spec`
  (jq) and `SPEC` (python3), so update the verdict-path table, both aggregation
  commands, and the contract tests in the same change. Verify the event_type
  set against `security_middleware/lifecycle.py`'s `_ACTION_CATEGORY` before
  editing.
- Within an allowlisted type only an explicit `pass` counts as risk-free;
  new/other verdict values (including `error`), missing fields, and non-string
  verdicts must surface as pending (`MISSING`) items rather than silently pass.
  This is what keeps the skill correct despite per-type enum differences, so do
  not rewrite the aggregation into a per-type reject list.
- The verdict enums are not uniform across scan types, and they do not all come
  from a `Verdict` class. `code_scan`, `prompt_scan`, and `pii_scan` use their
  own `Verdict` enums (`pass`/`warn`/`deny`/`error`), but `skill_ledger` events
  carry a projected verdict gated by `_VERDICT_SEVERITY` in
  `security_middleware/backends/skill_ledger.py`, which is wider: `pass`,
  `none`, `warn`, `unmanaged`, `drifted`, `deny`, `tampered`, `error`. Read that
  dict — not `skill_ledger/models/scan.py`'s `_SEVERITY_ORDER`, which is a
  different four-value ordering used for aggregating a skill's own scan status
  and ranks `none` *below* `pass` — when updating the skill's verdict table.
- The "取值语义" table exists so a report explains a verdict instead of echoing
  the raw token. Two entries are counter-intuitive and must keep their explicit
  wording: `error` means the scanner itself failed (`prompt_scanner/result.py`:
  "Scanner execution failed"), not a high-risk finding; `none` means unscanned
  (`status.py` maps an all-`none` ledger to health `unscanned`). Reporting either
  one as "safe" or as "high risk" is a factual error in both directions.
  `drifted` and `tampered` sit on different axes: `drifted` is a `fileHashes`
  mismatch against the signed snapshot (`skill_ledger/core/checker.py`), while
  `tampered` means the ledger metadata or signature itself failed authentication.
- Semantic wording is sourced, not invented. Take per-status phrasing from
  `skill_ledger/cli.py`'s integrity-status help text (note it omits `unmanaged`
  and `error`). The skill deliberately carries **no severity ranking**: it was
  tried and removed as needless complexity, since the per-value semantics plus
  the "no valid verdict" caveat already tell the Agent what to say. If a ranking
  is ever reintroduced, source it from `_VERDICT_SEVERITY` in
  `security_middleware/backends/skill_ledger.py` — the same dict that gates event
  verdicts, and the only ordering covering all eight tokens. Never source it from
  `skill_ledger/core/status.py`'s `_CRITICAL_STATUSES` / `_ATTENTION_STATUSES`:
  those are `health` values of the separate `skill-ledger status` command, never
  appear in `events` output, and omit `unmanaged`. An earlier revision imported
  that critical/attention vocabulary and had to be reverted.
- `skill_ledger` is the only scan type whose events can legitimately carry no
  verdict. Ten `skill-ledger` subcommands route through `invoke()` and emit
  events, but `_project_event_verdict` only projects six (`init`, `scan`,
  `check`, `show`, `certify`, `decide`); the rest (`status`, `audit`,
  `list-scanners`, `export`) produce an audit record with no
  verdict, so the aggregation reports them as `MISSING`. Verified on a live host:
  of 1476 events, `pii_scan`/`code_scan`/`prompt_scan` were 100% verdict-bearing
  while three of four `skill_ledger` events lacked one.
- That `MISSING` noise is handled **in the "取值语义" table, not by filtering**.
  A judgment-command allowlist in the pipeline was considered and rejected as too
  complex for the one-line command constraint. Accepted residual cost: the
  `risk_items` headline count still includes non-judgment `skill_ledger` records,
  so the skill instructs the Agent to report them as "非判定操作" and to treat
  `MISSING` as a real pending item only for the other three scan types. Do not
  "fix" this by making `MISSING` risk-free across the board — that would blind the
  three scanners where a missing verdict is genuinely anomalous.
- The aggregation output is counts + RISK lines only; `pass`/`allow` events
  never get a per-event line and their `details` are not fetched. This is a
  context-cost invariant, not cosmetics: a security report must not flood the
  Agent conversation. Keep the "上下文开销控制" guidance (count first, set an
  explicit `--limit` or paginate until every matching event is covered, expand
  only pending items, drill by `event_id` on demand, aggregate through the
  pipe) intact when editing. Never let the aggregation path rely on the CLI
  default `--limit 100`: counts between 101 and 200 must still be fetched in
  full before reporting risk totals.
- The "参数取值约束" section is a security control, not style. Every documented
  command interpolates `<session_id>`/`<event_id>` into a single-quoted shell
  string, and the skill explicitly accepts user-supplied correlation IDs, so an
  unvalidated value closes the quote and yields command injection (verified
  reproducible). Do not narrow all IDs to UUIDs: adapters persist values such as
  `session-001` or `thread_xxx`. Keep a bounded safe-character full-match
  requirement for generic correlation IDs, and keep the stricter UUID check for
  cosh-ng `runtime_context.provider_session_id`, which is expected to be a UUID.
- The "获取单条事件细节" section exists because `events` has **no `--event-id`
  filter**; single-event drill-down must go through client-side `jq` selection.
  Keep it that way unless the CLI gains such a filter. Detail lives under the
  uniform `details` = `{request, result}` shape, with `details.result.findings[]`
  as the per-hit evidence; document it at that level instead of enumerating
  each scanner's inner finding keys, which differ per scanner and would rot.
- Keep the "报告不得重新引入敏感值" rule: reports may only cite the redacted
  fields the event already carries (`evidence_redacted` et al., produced by
  `pii_checker/audit.py`'s `_sanitize_result`, which drops `raw_evidence` and
  keeps only `text_length`/`text_sha256` on the request side). Recovering the
  original value from conversation history to "explain better" turns a
  read-only query into a fresh leak, because model output is itself PII-scanned
  (`source=model_output`). Describe redaction formats by pattern rather than
  pasting observed values.
- The "获取当前 session_id" section documents a cosh-ng-only path: the cosh-ng
  `runtime_context` tool returns `provider_session_id`, which is the same value
  cosh-ng passes to hooks as `session_id` and therefore the same value stored on
  security events and observability records. Keep that section explicitly scoped
  to cosh-ng and keep the generic fallback next to it; no other agent runtime
  exposes that tool as of this release. If one starts to, update both the skill
  section and its contract test.
- Only cosh-ng lets an Agent resolve its own `session_id`. Every other runtime
  must fall back to a time range or, only when the user explicitly asks for the
  latest recorded session, `--last`, and state the real query scope in its
  answer, so keep the skill from telling those Agents to ask the user for an
  id they cannot obtain. `run_id` and `trace_id` are never self-resolvable.
- Keep the warning that `COSH_SESSION_ID` is not the agent session id. In
  cosh-ng it is the shell/terminal identity recorded as `shell_session_id`, so
  querying with it silently returns zero events and reads as "no security
  events". If cosh-ng changes how the session id is exposed, update the skill
  section and its contract test in the same change.

---

## User-Facing Documentation Guidelines

### Authoring Protocol

Every factual assertion in this section and in user-facing docs MUST be verified against source code before writing. Specifically:

1. **Enum/value-set claims** — read the defining source file that declares the enum
2. **External dependency sources** — grep for download/fetch calls in the relevant module
3. **Config field names** — read the config-loading function for that specific capability
4. **Module/section counts** — count actual headings in the user guide, never rely on memory
5. **Cross-file consistency** — if this file prescribes ordering/structure, verify the user guide matches before commit
6. **Intra-file consistency** — overview tables, section headings, and enumeration lists within the same document must agree

Do NOT write guidelines from design intent or mental models. Write them AFTER verifying the implementation.

### Value Proposition
- Lead with "all-local, zero Token cost" — addresses the common misconception that runtime security = expensive API calls or performance overhead.
- The three-layer defense framing (pre-execution prevention → runtime detection → kernel-level containment) helps users understand why multiple modules exist.

### Content Decisions
- Eight modules in overview table (Sandbox is architecture-only, no dedicated usage section). Seven usage sections: Prompt Scanner, Code Scanner, Skill Ledger, PII Checker, Security Baseline, Observability, Security Events. Do not merge them.
- Agent integration order in docs: CLI (always available) → OpenClaw plugin → Hermes plugin → cosh hook (auto-loaded, no user action needed). This reflects manual-effort-first ordering.
- `loongshield` may be mentioned alongside `agent-sec-cli harden` — loongshield is an Alinux system component users already know; `agent-sec-cli harden` is ANOLISA's unified entry point wrapping it.
- ML model preparation: state that L2 uses ModelScope model `modelscope.cn/ANOLISA/Qwen3Guard-Gen-0.6B-GGUF`; operators must run `ollama pull` first, and `scan-prompt warmup` only verifies availability. Never reference internal model registries.

### Gotchas to Warn About
- Code Scanner verdict enum defines `pass` / `warn` / `deny` / `error`. Built-in rules currently produce `warn` or `pass`; `deny` and `error` are available for custom/LLM-driven rules. Do not invent levels outside this enum (no "critical", no "info").
- Skill Ledger has exactly 6 states: pass / none / drifted / warn / deny / tampered. The state table must always appear in full when documenting Skill Ledger.
- Default plugin behavior should minimize unexpected disruption. Any default behavior that
  interrupts execution or requires user interaction must be an explicit capability-level product
  decision, covered by tests and documented with host-specific fallbacks and non-interactive
  behavior. Infrastructure failures should remain fail-open unless explicitly specified otherwise.

### Terminology
- "Security Baseline" not "hardening scan" (the feature name in CLI is `harden`, but user docs should call the concept "Security Baseline")
- "Skill Ledger" not "skill integrity" or "skill verification" (the latter was v0.3 naming, now superseded)
