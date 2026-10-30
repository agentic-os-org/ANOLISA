# 接入外部进程

[English](../../../en/aw/deployment/process-attach.md)

容器或进程控制器可先启动原生进程，再于首个请求前安装 AW。控制器继续负责 PID、终端、任务派发和进程退出。

## 支持入口

| 入口 | 支持范围 |
| --- | --- |
| Qoder 1.1.73 TUI | 输入前通过 ConfigChange 确认；请求结束等待原生 idle-prompt |
| QwenPaw 2.2.2.post1 / AgentScope 2.0.10 App | 已初始化 profile，loopback HTTP；可直接通过 API 使用 |
| Qoder print、headless、SDK；QwenPaw ACP/TUI/desktop | 不支持 PID attach，SDK 使用[受管工厂](../agents/qoder-sdk.md) |

普通 Qoder CLI 的最低版本范围不适用于此接入方式。

## 准备并接入

1. 为每个进程使用独占 profile。AW 和 Agent 使用相同用户、PID 和 mount namespace；QwenPaw 还需相同 network namespace。
2. profile 使用规范绝对路径，归运行用户所有，权限 `0700`。
3. 启动原生进程并记录 `NATIVE_PID`，暂停首请求派发。
4. 按[Agent 指南](../native-agents.md)准备 `current.yaml`，运行 connect。成功返回实例、策略版本和进程身份后再派发。

```bash
aw agent connect --config current.yaml --agent TARGET --pid "$NATIVE_PID"
aw update --config current.yaml --agent TARGET
aw agent connect --config current.yaml --agent TARGET --pid "$NATIVE_PID" --remove
```

上面三条分别是接入、[空闲更新](policy-updates.md)和移除；按所需阶段执行。
移除前同样暂停派发并等待请求完成。`busy` 保留挂载，稍后显式重试。成功移除保留原生进程、其他 Hook 和用户配置。

## 原生准备

- Qoder 使用 `$HOME/.qoder/settings.json`，启动时不传 `--settings`、自定义配置根目录或禁用 Hook 的参数。已有 settings 须为同用户所有且不可被组或其他用户写入的普通文件。
- QwenPaw 在初始进程环境显式设置绝对 `QWENPAW_WORKING_DIR`，App 监听 `127.0.0.1`。同环境的其他 App 也应显式设置 profile；无法确认 peer 的启动目录时，按报告停止或重新配置该 peer。

控制器应持续保证 profile 独占。先移除 AW，再停止外部进程。

## 清理失败

无法确认原生卸载时，AW 保留所有权及桥接文件。恢复原生回调或插件 API 后，用同一条 `--remove` 命令重试。
插件目录被删除但仍加载时，先恢复原目录。Qoder 的 AW 配置组被移动时，先恢复原组。

强制终止安装可能留下暂存文件或未记录绑定。按所属 supervisor 停止受影响服务，核对并清理其私有状态；当前没有自动崩溃回收器。
完整所有权和移除合同见[策略更新设计](../../../../../src/aw/docs/design/zh/runtime/policy-reload.md)。
