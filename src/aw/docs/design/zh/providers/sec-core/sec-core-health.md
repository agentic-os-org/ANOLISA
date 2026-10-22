# sec-core 健康诊断边界

[English](../../../en/providers/sec-core/sec-core-health.md)

显式入口 `aw-provider-sec-core --health --helper ABS --socket ABS` 支持安装检查，
不调用安全操作。它只执行公开的 `agent-sec-daemon-health` helper；helper 负责校验
Unix socket 的 root peer 凭据及 `daemon.health` 响应。调用方应选择可信且经回执
校验的 helper；诊断入口不提供可执行程序沙箱。

独立 `aw-sec-core-health/v1alpha1` schema 位于
`../../providers/sec-core/schemas/health-v1alpha1.schema.json`。
`helper_installed` 表示文件存在，`socket_configured` 表示输入绝对路径，不验证 AW
配置。只有 helper 成功才返回 `authenticated_reachable`；组件缺失、helper 错误、
输出超限及进程超时均不可用。结果不含路径、环境和 helper 原始诊断。
CLI 期限为一秒，每路输出限制为 4096 字节；子进程环境移除测试专用 peer 检查
绕过开关。AW 不重复实现或放松 sec-core 的 peer 认证。

准入和检查结果始终为 unknown。AW 目标准入属于当前实例/generation，扫描判定属于
具体调用，健康不能代替两者。此入口独立于现有 Provider 事件和 view 协议，未注册到
`aw inspect`，不修改该命令输出。后续统一查询入口需另行评审兼容合同。

Provider 包交付 helper 及官方 systemd/配置参考资产，由现有回执校验；不把资产
注册为系统服务。已有 root 后台继续由系统包拥有；AW 安装/卸载不改 `/etc`、
不停止共享服务、不迁移后台数据。自动系统安装/升级/移除不在本次实现内。
运行时 `scan_code` 保留逐次认证及失败策略，describe、配置验证和完成观察继续离线。
