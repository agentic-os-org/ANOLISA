# Hermes

[English](../../../en/aw/agents/hermes.md)

## 准备

使用官方提交 `952c941e741e922a9be8fc403c8944c6e96318bb`，已跟踪文件保持干净。安装 Python console script，使用绝对 Python shebang；入口为本地 `chat`。
按[教程](../integration.md)构建 AW，复制[Hermes 配置](../../../../../src/aw/crates/aw-service/examples/aw.hermes.yaml)，把 argv 改为该安装的绝对路径。
先用 Hermes 初始化 profile 和模型账号。

## 安装插件并启动

```bash
cd src/aw
target/debug/aw agent connect --config /absolute/aw.hermes.yaml --agent hermes \
  --native-profile /absolute/hermes-profile
target/debug/aw run --config /absolute/aw.hermes.yaml --agent hermes \
  --native-profile /absolute/hermes-profile
```

connect 备份配置后安装 AW 插件，报告 `backup` 和 `displaced` 路径。保留这两份文件。
运行时使用同一 profile，等待 Hook 就绪后按[原生验收](../validation.md)验证工具。

## 部署约束

| 项目 | 要求 |
| --- | --- |
| 入口 | Python console script 的本地 chat；不支持 shell wrapper、`python -m`、TUI、Gateway、ACP 或会话/profile/worktree 切换 |
| 路径 | 工作目录、profile、venv、checkout 和 AW 路径须为 UTF-8；profile 与 `.env` 归当前用户且不可被他人写入 |
| Git 环境 | 取消 `GIT_DIR`、`GIT_COMMON_DIR`；HOME/XDG 和 Git 配置路径使用绝对路径；拒绝 filter 与 fsmonitor helper |
| 原生选项 | safe mode 和 chat 帮助参数会在接入前被拒绝；直接用 `hermes chat --help` 查看帮助 |
| 工具参数 | 启用 AW before 时禁止 Hook/middleware 改写输入，包含 `execute_code` 内部调用 |
| 预算 | 事件最多 55000 ms，并须满足原生回调期限 |

before 阻断后，其他原生回调仍可运行；after 可观察失败或被阻断的尝试。
配置 AW 回调时，工具执行中的强制插件重载会被拒绝。空策略仍须安装插件，运行时注册零个工具回调。

## 升级与清理

插件不匹配时，先停止 Hermes 与配置写入者，把 `<profile>/plugins/aw-native-hooks` 移到 plugins 目录之外备份，再运行 connect。
回退时核对 `backup` 和 `displaced`，恢复所需配置并保留安装后有意修改。AW 没有 Hermes 插件卸载命令。
退出 Hermes 后停止 AW 服务；配置发布或清理失败按报告路径核对，不覆盖未知文件。
详细探测、原子发布与冲突处理见[Hermes 合同](../../../../../src/aw/docs/design/zh/adapters/hermes-adapter.md)。
