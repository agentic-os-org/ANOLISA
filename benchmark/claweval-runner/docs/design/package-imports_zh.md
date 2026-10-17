# Package 导入边界

[English](package-imports.md)

`ce_runner` 在 package 初始化时暴露 version 和无参数 `main` callable，不导入评估
runtime。这样 metadata 探测和各 submodule 可以建立各自需要的依赖边界。

调用 `main()` 时才导入并委托给 `ce_runner.run_task.main`。Console script 注册仍为
`ce_runner:main`；exception 和 `SystemExit` 保持原有行为。真实 CLI 执行仍需要其 runtime
library、service 和受支持平台；这个边界不会让全部 submodule 或完整 runner 无依赖。

package initializer 必须保持轻量。Runtime import 应放在委托 callable 内，避免未来
metadata consumer 无意中加载 agent 或 host-service module。
