# PreTool 改写延迟

[English](README.md)

使用暂存插件或已安装插件，分别测量千问办公 macOS Hook、Protocol v2
`pre_tool` 和 RTK 命令行。基准不会执行改写后的命令，也不测量 PostTool 输出压缩。

在 `src/tokenless` 目录下使用 Python 3.9 或更新版本运行：

```bash
python3 benchmark/rewrite-latency/bench.py \
  --variant installed="$HOME/.qwenworkcn/plugins-custom/tokenless" \
  --cwd "$PWD" --samples 30 --warmup 5 \
  --output benchmark/rewrite-latency/reports/installed.json
```

对比源码修改时，两个版本必须使用相同的 Rust 工具链、release profile 和目标平台。
将每个二进制与对应版本的适配器资源暂存到独立目录；共享 Hook 必须复制，不能仍然
链接到工作区：

```bash
mkdir -p target/rewrite-bench/before
cp -RL adapters/tokenless/qwenwork/. target/rewrite-bench/before/
mkdir -p target/rewrite-bench/before/bin
cp target/release/tokenless target/rewrite-bench/before/bin/tokenless
```

修改后重新构建，将上述命令中的 `before` 换成 `after` 暂存，再在同一次运行中对比：

```bash
python3 benchmark/rewrite-latency/bench.py \
  --variant before=target/rewrite-bench/before \
  --variant after=target/rewrite-bench/after \
  --cwd "$PWD" --samples 50 --warmup 5 \
  --output benchmark/rewrite-latency/reports/comparison.json
```

加入报告所用的 v0.9.1 时，使用 `tokenless/v0.9.1` 标签及其锁定的 RTK 源码和补丁
构建，将该版本的适配器、`bin/tokenless` 和 `bin/rtk` 一起暂存，再向同一次对比
添加 `--variant v0_9_1=PLUGIN_ROOT`。脚本识别 v0.9.1 的独立 RTK 和旧版 grep
策略，按版本记录预期动作，只在动作一致时比较改写命令。源码重建不复现原始发布
产物的签名或交叉编译环境。

脚本根据同一条命令生成 Hook 和 Core 输入，并为所有进程指定相同工作目录。
Git 和复合命令样本必须替换参数；当前版本的 `find`、`grep`、不支持的命令和空输入
必须透传。v0.9.1 必须改写 grep 样本；不能将其改为透传带来的耗时变化归因于改写
性能。find 和 grep 输入分别与原报告的直接 Core 输入和完整 Hook 输入一致。
异常退出、stderr、格式错误的响应、参数丢失或改写结果变化都会使基准失败。
比较改写结果时，只归一化运行时路径、执行凭据 ID、入口对应的调用 ID，以及独立
`rtk` 与 `tokenless rtk` 启动方式。

每个版本显式指定独立的临时 stats 和 stash 数据库，覆盖继承的数据库路径，并移除
DSH 管理的路径别名。临时根目录会先解析符号链接，使跨版本命令比较时的状态路径
与 Core 输出的规范化路径一致。统计保持开启，SLS 关闭。每个样本和入口复用一个调用 ID，
保持状态文件和数据库记录数量稳定。基准会读取现有 RTK 与项目配置；如果配置改变了
预期动作，基准会报错，而不会测量另一条路径。Bash 启动器从 `PATH` 选择 `python3`，
两个版本必须保持一致。完整千问办公启动器要求 macOS。

预热调用不计入统计。固定随机种子打乱版本、样本和入口的执行顺序，降低顺序影响。
报告保存所有样本耗时、中位数、均值、最近秩 p90/p95、版本和二进制及适配器指纹。
每次测量都会新建进程，但文件系统缓存已经预热，不代表首次安装后的冷启动耗时。
请在机器空闲时运行。

基准父进程在多个样本之间持续运行，但每次完整 Hook 都会新建 Python 父进程来
启动 Core。因此，直接 CLI 样本不包含新 Hook 父进程首次启动二进制所特有的成本。
分析 macOS 开销时，应在新父进程内对比首次和重复执行 `--version`，单独检查该项。
应在同一次 Hook 调用中进行阶段计时，不能将直接 CLI 与 Python 的独立耗时相加
来还原完整 Hook。

应直接比较相同样本和入口在两个版本中的耗时。不要相减不同样本来归因 Python 或
I/O 开销，也不要把透传当作更快的改写。报告是生成产物，不纳入版本控制。
