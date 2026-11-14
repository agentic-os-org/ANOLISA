# Package Import Boundary

[中文版](package-imports_zh.md)

`ce_runner` exposes its version and a zero-argument `main` callable without
importing the evaluation runtime during package initialization. This lets
metadata probes and submodules establish their own dependency requirements.

Calling `main()` imports and delegates to `ce_runner.run_task.main`. Console
script registration remains `ce_runner:main`; exceptions and `SystemExit` retain
their existing behavior. Actual CLI execution still needs its runtime libraries,
services and supported platform. This boundary does not make all submodules or
the full runner dependency-free.

The package initializer must stay lightweight. Runtime imports belong inside the
delegating callable so future metadata consumers do not accidentally load agent
or host-service modules.
