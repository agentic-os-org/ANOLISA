# Development Guide

## 1. 环境准备

- **Python 版本：固定使用 3.11.6**（通过 `.python-version` 和 `pyproject.toml` 双重约束，不支持 3.10 及以下或 3.12 及以上）
- 使用 [uv](https://docs.astral.sh/uv/) 管理 Python 依赖和虚拟环境，uv 会自动读取 `.python-version` 选择正确版本
- `cd agent-sec-cli && uv sync` 安装所有依赖（含 dev group）
- `uv.lock` 必须纳入版本控制，确保 CI 和本地环境一致

## 2. 常用 uv 命令

| 场景 | 命令 | 说明 |
|------|------|------|
| 安装所有依赖（含 dev） | `uv sync` | 自动创建 .venv 并安装 |
| 仅安装运行时依赖 | `uv sync --no-group dev` | 生产环境用 |
| 添加运行时依赖 | `uv add <pkg>` | 自动更新 pyproject.toml 和 uv.lock |
| 添加 dev 依赖 | `uv add --group dev <pkg>` | 写入 [dependency-groups].dev |
| 添加可选依赖 | `uv add --optional pgpy <pkg>` | 写入 [project.optional-dependencies] |
| 删除依赖 | `uv remove <pkg>` | 同时清理 pyproject.toml 和 uv.lock |
| 删除 dev 依赖 | `uv remove --group dev <pkg>` | |
| 更新单个依赖 | `uv lock --upgrade-package <pkg>` | 仅升级指定包 |
| 更新所有依赖 | `uv lock --upgrade` | 重新解析所有版本 |
| 运行命令 | `uv run <cmd>` | 在 .venv 环境中执行 |
| 运行测试 | `uv run pytest tests/ -v` | |
| 运行临时工具 | `uv run --with <pkg> <cmd>` | 不修改项目依赖，临时注入 |
| 构建 wheel | `uv run maturin build --release` | 通过 uv 调用 maturin |

> **注意:** 修改依赖后务必提交更新后的 `pyproject.toml` 和 `uv.lock`。

## 3. 导入规范

- 所有 import 使用绝对路径: `from agent_sec_cli.xxx import yyy`
- 禁止使用相对导入 (`from .xxx import`) 或裸导入 (`from xxx import`)
- 禁止运行时动态导入 (`importlib.import_module()`、`__import__()`)
- 禁止在函数体内导入，所有 import 必须在文件头部引入

## 4. 类型注解

- 所有函数/方法必须标注参数类型和返回类型
- Python >= 3.10 原生支持 `dict[str, Any]`、`str | None` 等语法，无需 `from __future__ import annotations`
- 当前项目固定使用 Python 3.11.x，避免 3.12 引入的 breaking changes（`distutils` 移除、f-string 语法变更等）

## 5. Backend 接口规范

- 所有 Backend 必须继承 `BaseBackend` 并实现 `execute()` 抽象方法
- `execute()` 签名: `def execute(self, ctx: RequestContext, **kwargs: Any) -> ActionResult`

## 6. 测试规范

- 统一使用 pytest
- 运行方式: `make test-python`（从 agent-sec-core 目录）
- 测试文件位于外层 `tests/` 目录，不迁入 agent-sec-cli/

## 7. 标准库优先原则

- 文件路径处理优先使用 `pathlib.Path`，而非 `os.path`
- 数据类优先使用 `pydantic`
- 子进程调用使用 `subprocess.run()`，避免 `os.system()`

## 8. 编码风格

- 空函数/抽象方法使用 `pass` 占位，不使用 `...`（Ellipsis）

## 9. 代码格式化与 Lint

- **格式化**: 使用 [black](https://black.readthedocs.io/) + [isort](https://pycqa.github.io/isort/)（保持现有风格不变）
- **静态检查**: 使用 [ruff](https://docs.astral.sh/ruff/) 进行 lint（仅对增量代码 warning，不卡点）

```bash
# 从 agent-sec-core 目录
make python-code-pretty   # 格式化（black + isort）
make python-lint          # 全量 ruff lint 检查（不修改文件）
```

## 10. CI 检查项

CI 对 Python 代码执行以下检查：

| 检查项 | 范围 | 失败行为 |
|--------|------|----------|
| black + isort 格式化 | 全量代码 | 存在未格式化代码则失败 |
| ruff lint（增量） | 仅 PR 变更行 | 不卡点，违规以 warning 形式显示在 CI Summary |
| `pytest --cov` | 全量测试 | 测试失败则失败 |
| `uv lock --check` | 依赖锁文件 | uv.lock 与 pyproject.toml 不同步则失败 |

> **重要**: Lint 检查仅在 PR 触发时对增量代码报 warning，不检查历史代码，不卡点。
