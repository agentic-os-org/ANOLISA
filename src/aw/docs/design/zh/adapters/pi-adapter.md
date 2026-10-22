# Pi SDK Adapter

[English](../../en/adapters/pi-adapter.md)

Pi Adapter 支持公开的 Coding Agent/Core SDK
`@earendil-works/pi-coding-agent` 和 `@earendil-works/pi-agent-core` 0.84.2，
使用 Linux 与 Node.js >=22.19.0。入口为 `sdk`：AW 管理的前台 worker 导入应用
工厂。原生 Pi CLI 不在这个合同内。

## 工厂与生命周期

`spec.agents.<target>.argv` 为 `[node, /absolute/application.mjs, ...arguments]`。
AW 按 Node 标准 `node_modules` 搜索路径核验实际安装包的元数据及固定导入入口，
预检不导入应用或执行业务工厂。Core 从 Coding Agent 实际安装位置解析，包含嵌套
依赖。Yarn PnP 与其他包布局不支持。运行时从同一路径导入，并要求返回真实
`AgentSession` 与 Core `Agent`。元数据和就绪回执是同用户安装证据，不是可信证明。

应用导出 `createSession({sdk,args,...options})`，返回公开 `createAgentSession`
结果，以及 `run(session,{args,createSession})`。worker 等待应用工厂完成，组合
已安装的 callbacks，写入受保护的 PID/token 回执，再等待 `run` 完成。
new/resume/fork/import 的替换会话必须使用提供的公共 `createSession` 工厂；每次
业务工厂返回后重新组合。业务 callbacks 必须在 `createSession` 返回前配置。
受管期间两个 callback 属性拒绝赋值、删除与重新定义，包括 `run` 和替换会话；捕获
赋值错误也不会移除 AW。dispose 恢复原 callback 值并允许赋值；保护描述符保留到
Agent 生命周期结束，重新绑定使用新的工厂结果。直接调用 SDK 工厂或独立启动进程
会绕过受管边界。
退出时 worker 恢复 callbacks 并 dispose 所有受管会话；AW 释放 binding 并删除
生成文件。共享 daemon 和持久 journal 保留，直到显式停止或删除。

## 工具语义

原 before callback 先执行，其异常与拒绝保留 Pi 原生语义；AW 不授予原生权限。
AW 接收包含原 callback 修改的已校验参数。AW 拒绝返回 `{block:true,reason}`，
Pi 跳过工具执行与 after。未知工具、参数校验失败、提前取消、原生 before 异常
同样不产生 after；工具执行异常进入 after，携带 `isError:true`。

原 after callback 先执行。AW 按 Pi 的 nullish 字段合并语义观察其替换结果，包含
图片归一化与 `isError`。原 callback 异常继续遵循 SDK 错误行为。
AW before 支持 `observe`/`block`，包含 Provider 的字面重试理由。Pi 将拒绝结果加入
下一次模型上下文；AW 不执行建议或修改参数。模型重试是新调用，须重新通过全部
before 检查与原生权限。

AW after 支持 `observe` 和 `replace_result`。步骤按配置顺序处理同一有效原生结果：
观察器和自定义 Hook 位于唯一、最后的结构化替换步骤之前。只有这个受保护步骤
可返回 `aw_replacement`。Bridge 校验只有唯一文本槽发生变化，返回 content override，
保留原 callback 其他字段，不追加模型消息。媒体、多块和失败结果不由 Tokenless
压缩。report 故障保留原 SDK callback 返回值；观察与压缩均不能回滚工具副作用。

每次准备读取当前 `sessionManager.getSessionId()`，不使用 Agent 的 provider cache
`sessionId`。每次准备生成新的 UUID v4 `invocation_id`，作为全部 before/after 步骤的
标准化 `tool.call_id`。模型原 `tool_call.id` 保留于 `native`；Pi 后续轮次可复用该 ID。
准备阶段固定会话、invocation UUID 与参数快照，直到完成；会话身份中途改变不会串线。Bridge 最多保留 512 个在途准备调用，在原生完成事件清理，包含跳过
after 的取消路径。保留 Pi 顺序准备、并行执行的调度：after 按完成顺序，结果消息
按原调用顺序，不添加全局工具锁。服务保留关联记录的容量仍限制长期 worker；
每个 instance 最多 32 个 live event 和 1024 个保留的事件关联（包含已关闭的
before/after），达到容量前须释放并重新绑定。在最后配置步骤之前拒绝会显式关闭
事件，不运行剩余步骤，包含 `on_error:block` 下原生非法 JSON、非零/信号退出及
执行故障。关闭失败会报告，且不会把拒绝变成允许。SDK 取消或本地传输上限可能在 helper
确认关闭前终止它；已打开事件随后在原截止时间或 binding release 时终结，不延长
预算。这与已等待完成的策略拒绝路径不同。

## Callback 协议与验证

AW 生成的 binding/config 文件属于前台启动的私有资源。Callback 用字面参数执行
`aw hook --adapter pi`，限制 stdin、stdout、stderr 和等待时间；取消会终止并等待
自己创建的进程组。原生命令输出可为空，或仅包含布尔 `block` 与可选字符串
`reason` 的 JSON 对象，只有 before 可阻断。审批、输入改写、原生结果改写均拒绝。
非零/信号退出、非法输出按配置的失败动作处理。native Hook 不得伪造
`aw_replacement`；只有通过准入的最后结构化替换步骤通过受保护 AW 配置输出。

默认 AW 检查运行合成 launcher 合同 fixture。独立发布包验收运行真实公共工厂、
真实 Core loop 和自定义工具，使用确定性 stream，不调用模型或云服务。
固定 npm lock 保留包 integrity 和传递依赖：

```bash
cd src/aw
cargo build --locked -p aw-cli --bin aw
mkdir -p target/pi-sdk-validation
cp crates/aw-cli/tests/fixtures/pi/package*.json target/pi-sdk-validation/
npm ci --prefix target/pi-sdk-validation --ignore-scripts --no-audit --no-fund
python3 crates/aw-cli/tests/fixtures/pi/public-smoke.py \
  --aw target/debug/aw --root target/pi-sdk-validation
```

脚本记录命令、PID、截止时间，验证允许/拒绝的副作用、两个 Hook 的等待、工具
失败、已有 callback 参数修改/结果替换、两会话与并行完成身份、失败策略、完整
Audit 链核验、binding 释放与 socket 删除。Linux UDS 的 state 路径须保持足够短。
这是 SDK 验收，未验原生 CLI、业务部署、模型或远端执行器。

## Tokenless 验收

复用同一固定 SDK 目录，构建实际 Tokenless CLI 和 Providers：

```bash
cargo build --locked --manifest-path ../../providers/tokenless/Cargo.toml
cargo build --locked --manifest-path ../../providers/agent-sec-core/Cargo.toml
cargo build --locked --manifest-path ../tokenless/Cargo.toml \
  --target-dir target/t -p tokenless-cli
python3 crates/aw-cli/tests/fixtures/pi/tokenless-smoke.py \
  --aw target/debug/aw --root target/pi-sdk-validation \
  --tokenless-provider ../../providers/tokenless/target/debug/aw-provider-tokenless \
  --tokenless-cli target/t/debug/tokenless \
  --sec-core-provider ../../providers/agent-sec-core/target/debug/aw-provider-sec-core
```

这里运行真实 SDK、AW、Tokenless Provider 和 Protocol v2 CLI。模型 stream、bash
工具、RTK rewrite 命令及 scanner backend 使用确定性 fixture，不执行建议的 shell
命令。验证选中的压缩进入模型上下文、新重试重新通过 sec-core、RTK 已处理结果
保持不变。两个遥测 sink 均关闭，状态只位于指定测试 root。8 个 Bridge 授权与
结果字段边界用例也通过默认 Rust Pi 门禁运行。
