# Tokenless Windows 集成

[English](../../en/tokenless/windows.md)

本文面向维护者，说明 Windows Adapter 的宿主契约和路径查找规则。
安装与日常使用见[千问办公用户指南](../../../user-guide/zh/tokenless/framework-integration.md#千问办公windows-x64-本地模式)。

## 千问办公宿主契约

QwenWorkCN 1.3.2-26101704 在安装目录的
`resources/agent-sdk/engine-runtime-manifest.json` 中，通过 `opencode` 和 `qwencode`
的 `env` 项提供 `QWENWORK_PLUGIN_HOOK_COMPATIBILITY=managed-tool-hooks-v1`。
此标记用于识别在 Windows PowerShell 中执行的 `Bash` 事件。Adapter 读取宿主值，
安装器不应强制设置：旧版 Qoder CLI 会话使用 Git Bash，缺少此标记时需要保留 POSIX 改写。

Windows 安装器将每个命令 Hook 的 `shell` 设为 `powershell`。同一版本的千问办公在
`resources/agent-sdk/opencode/qoderwork-plugin-adapter.mjs` 中，将
`configuration.shell ?? true` 传给 Node 的 `spawn`。这段宿主代码负责选择执行 Python
Hook 命令的 PowerShell；前述 manifest 标记识别的是工具使用的 Shell。忽略 `shell`
字段的宿主尚未验证：生成的命令使用 PowerShell 语法，无法在 cmd.exe 下执行。
已验证的宿主会忽略普通 Hook 执行失败，保持工具输入不变；退出码 2 专用于阻断决定。
因此，Python 解释器或共享 Hook 资源缺失时，该次调用会失去优化，无需额外分发脚本。

PreTool 匹配 `Bash`、`PowerShell`、`mcp__workspace__bash` 和
`mcp__qw-builtin__mcp_workspace_bash`，由 RTK 决定每条命令是否改写。PowerShell 调用
绑定原生 RTK，并在该次调用内设置归属环境变量。PostTool 将 PowerShell 输出归类为
命令输出，保留文件内容保护，避免对 RTK 输出重复压缩。不支持的命令透传。

PowerShell 包装脚本在子作用域内设置 `rtk` 别名。在被改写的复合命令中，原本已有的
裸 `rtk` 调用也会绑定到托管二进制；POSIX 绑定只修改本次改写插入的调用。显式可执行
文件路径不受影响。包装脚本恢复归属环境变量后保留 `$?` 和 `$LASTEXITCODE`，包括
原生命令非零退出的状态，且不会新增 stderr 输出或 `$Error` 项。
当此包装脚本是 `powershell -Command` 或 `pwsh -Command` 执行的最后一条命令时，
`$?` 为 `$false` 时进程退出码为 `1`；原生退出码仍可通过会话内的 `$LASTEXITCODE` 获取。

生命周期脚本从 `%LOCALAPPDATA%/Programs/QwenWorkCN` 及其版本子目录查找应用。
`TOKENLESS_QWENWORK_APP` 覆盖安装目录，`TOKENLESS_QWENWORK_HOME` 覆盖数据目录
（默认 `~/.qwenworkcn`）。插件复制到 `plugins-custom/tokenless`，Hook 记录 Python
解释器的绝对路径。安装器不修改 settings，也不启用已禁用插件；被修改及非自身管理的
插件不会被覆盖或删除。桌面应用的 PATH 需要包含 npm 命令目录。

原生 Hook 测试覆盖改写、执行、压缩、文件读取保护、统计、去重及安装生命周期。
可选的已安装宿主测试读取两个引擎的 manifest 环境，并调用已安装的 OpenCode Adapter
验证 Hook Shell 选择、改写、PostTool、Read 保护、统计和共享资源缺失时的透传行为。
这些测试不能证明某个应用会话
已加载或启用插件，仍需在千问办公本地会话中单独确认。

## 二进制查找与支持边界

原生二进制从当前可执行文件同级目录和 PATH 的绝对路径项中查找。与 Unix 不同，
Windows 查找不检查文件和目录的 ACL、所有者或重解析点，而是信任配置的安装目录和
PATH。RTK 只接受 `.exe`；一般依赖查找还接受 `PATHEXT` 中的 `.com`、`.bat` 和 `.cmd` 项。

Windows 支持原生 CLI 和千问办公 Adapter。npm 会复制其他 Adapter 资源，但跳过
Claude 自动启用；其他 Windows Adapter 尚未验证。不支持安全工作区／VM 会话和
Windows ARM64。

## 在 Windows 上构建和测试

在原生 PowerShell 中构建，需安装 Git、Rust 的 `x86_64-pc-windows-msvc` 工具链、
Visual Studio C++ Build Tools（含 Windows SDK）。打包需要 Node.js/npm 和 Python 3.10+，
千问办公 Adapter 同样使用 Python。不需要 Git Bash、WSL 或 `make`，不支持 Windows ARM64。
Windows CI 与此构建入口分开处理。

```powershell
cd src/tokenless
./scripts/build-windows.ps1
node npm/scripts/package-npm.js --target win32-x64
$packages = Get-ChildItem npm/dist/tokenless-win32-x64/*.tgz, npm/dist/tokenless/*.tgz
npm install -g $packages.FullName
```

构建脚本拉取并修补固定版本的 RTK，将两个可执行文件写入
`target/npm-prebuilt/win32-x64`。添加 `-Debug` 可构建开发版本。生成的 npm 包为本地
制品，Windows 软件包尚未发布。现有 Linux/macOS 发布工作流保持不变。

统一发布时，将 Windows 构建主机的完整 `target/npm-prebuilt/win32-x64` 目录复制到
Linux 打包主机 `src/tokenless` 下的同一位置。两台主机使用相同源码版本，备齐另外
四个平台目录后运行 `make npm-package-all`。`--all` 要求五个平台的制品齐全，缺少
任一二进制都会失败；`--target x64` 也包含 Windows。仅打包 Linux 或 macOS 时，
使用 `--target linux` 或 `--target darwin`。当前发布工作流尚未传递 Windows 制品；
Windows CI 将在支持合入后单独添加。

本地验证运行 `cargo fmt --all -- --check`、
`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`，
并在打包后运行 `python -X utf8 -B tests/test_windows.py`。测试 Python workspace
成员时，可通过 `PYO3_PYTHON` 指定 Python 可执行文件。
Windows smoke 测试调用真实 PowerShell Hook 和原生二进制；应用界面的插件加载与激活
需要另行在千问办公本地会话中验证。
将 `TOKENLESS_TEST_QWENWORK_APP` 指向包含 `QwenWorkCN.exe` 的已安装版本目录，
可用真实宿主 manifest 环境测试两个引擎的托管 Bash 及两个 MCP 别名，并通过 PATH
中的 Node 调用已安装的 OpenCode Hook 执行器。设置 `TOKENLESS_TEST_BASH` 为 Git Bash
可执行文件可加入旧版执行测试。数据库拒绝测试使用系统临时目录（Windows 为
`%SystemRoot%/Temp`，Unix 为 `/tmp`），不依赖可能位于用户目录中的 `TEMP`、`TMP` 或
`TMPDIR`；若系统临时目录不可写或处于受信用户目录内，测试会提示 fixture 不可用。
允许写入受信用户目录的行为由另一项独立测试覆盖。设置 `TOKENLESS_TEST_POWERSHELL`
为 `pwsh.exe`，可在 PowerShell 7 上重跑 Rust `powershell_binding` 测试，以及已安装
宿主 smoke 测试中改写后的工具命令；默认使用 Windows PowerShell 5.1。宿主仍按安装器
的 `powershell` 设置执行已注册的 Python Hook。
验证 npm 安装后的启动器时，将 `TOKENLESS_TEST_BIN` 指向已安装的
`anolisa-tokenless/bin`，并将 `TOKENLESS_TEST_NODE` 指向待验证的 Node 可执行文件
（包括最低支持版本 16.7.0）。
