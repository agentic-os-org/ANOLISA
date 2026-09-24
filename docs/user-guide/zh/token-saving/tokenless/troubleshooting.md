# Tokenless 故障排查

[English](../../../en/token-saving/tokenless/troubleshooting.md)

先判断问题发生在哪一层：组件安装、Adapter 接入、压缩处理、统计落盘或 Stash 取回。不要一开始就删除配置或重新安装。

## 快速诊断

按顺序运行：

```bash
tokenless --version
anolisa status tokenless
anolisa doctor tokenless
anolisa adapter status tokenless
tokenless stats status
tokenless env-check --all --json
```

如果某条命令失败，先处理该层问题，再继续后面的检查。查看安装计划但不修改系统：

```bash
anolisa --dry-run install tokenless
anolisa --dry-run --verbose install tokenless
```

请用拥有目标 Agent 配置和 adapter receipt 的用户运行 adapter 诊断。该用户
可以同时查看 user 状态和可读的 system 状态。

```bash
anolisa doctor tokenless
```

## `tokenless: command not found`

普通用户安装通常把命令放在 `~/.local/bin`。检查：

```bash
command -v tokenless
printf '%s\n' "$PATH"
ls -l ~/.local/bin/tokenless
```

如果 `~/.local/bin` 不在 `PATH`，按照 shell 的启动文件规则加入后重新打开终端。不要为了解决 PATH 问题重复执行 system 安装。

npm 用户还应检查：

```bash
npm prefix -g
npm list -g --depth=0 anolisa-tokenless
```

如果 npm 日志提示跳过 optional dependencies，重新安装：

```bash
npm install -g --include=optional anolisa-tokenless
```

Linux npm 二进制只支持 glibc；Alpine 等 musl 系统需要在 Linux 上从源码构建。

## 输入和 JSON 错误

| 错误 | 原因 | 处理 |
|------|------|------|
| `No input provided` | 未传 `--file`，stdin 也是终端 | 使用 `-f <path>` 或管道 |
| `Input exceeds 64 MiB limit` | 单次输入超过上限 | 拆分输入，不要提高系统内存限制绕过 |
| `JSON parse error` | 输入不是合法 JSON | 先运行 `jq . < input.json` |
| `Expected a JSON array for --batch mode` | `--batch` 输入不是数组 | 移除 `--batch` 或修正输入结构 |
| 输出仍是原文 | 压缩后没有估算收益 | 属正常行为，查看 stderr 提示 |

## 启用后没有产生统计记录

### 1. 验证独立 CLI

```bash
printf '%s\n' \
  '{"status":"ok","debug":{"trace":"verbose"},"metadata":null,"data":{"items":[1,2,3]}}' \
  | tokenless compress-response

tokenless stats list --limit 5
```

如果这里也没有记录，检查：

```bash
tokenless stats status
ls -ld ~/.tokenless
ls -l ~/.tokenless/stats.db
```

压缩无收益时不会记录。测试输入应包含可删除或可截断内容。

### 2. 验证 Adapter

```bash
anolisa adapter scan
anolisa adapter status tokenless
```

确认：

- 目标框架已被检测。
- Tokenless Adapter 已启用。
- adapter 命令由目标框架配置和 receipt 的所属用户执行。
- 启用后已经重启 Agent CLI 或 IDE。

### 3. 验证 Agent 任务

执行一个确实会经过 Hook 的工具任务，例如有明显输出的 Shell 命令。纯聊天、短响应或框架不提供对应 Hook 时不会产生记录。

### 4. 检查环境覆盖

```bash
env | grep '^TOKENLESS_'
```

确认没有意外设置 `TOKENLESS_STATS_ENABLED=0`，并检查自定义数据库路径是否仍位于真实用户 home 或选定的数据目录下。

## Schema 压缩没有统计记录

Schema 压缩的接入方式因宿主而异：

- **cosh 与 Cosh-NG**：通过 `BeforeModel` Hook 在每次模型调用前运行；本节的告警来自该 Hook。
- **OpenCode**：通过其 `tool.definition` 插件 Hook 逐个压缩工具定义，不走 `BeforeModel`。MCP 工具不经过该 Hook，因此工具集只有 MCP 工具时不会有记录，下面的 `BeforeModel` 告警也不适用。
- **Qwen Code**：扩展清单里带有 `BeforeModel` Hook 条目，但当前 Qwen Code 版本未实现该 Hook 事件：其 Hook 注册器会跳过未知事件名，实际只注册其余 Hook 组，Schema Hook 不会运行。Qwen Code 上没有 `compress-schema` 记录属于预期行为，本节无法用于诊断。

在实际运行该 Hook 的宿主上没有 `compress-schema` 记录时，按以下顺序排查：

### 1. 确认确实有可压缩内容

统计只记录实际产生 Token 节省的调用，压缩结果不比原文更小就不会记录。内置工具的描述通常较短（低于 256 字符函数描述 / 160 字符参数描述的截断阈值，也没有可移除的 `title` 或 `examples`），压缩没有收益，零记录属于预期行为。用当前工具声明直接验证——请把下面的示例数组替换为你的真实工具声明（合法 JSON 数组，不要保留占位文本、尖括号或外侧引号）：

```bash
echo '[{"name":"example_tool","description":"这是一段刻意写得足够长的示例工具描述，用于演示 Schema 压缩效果，它必须超过函数描述二百五十六个字符的截断阈值，才能产生可记录的压缩收益。这是一段刻意写得足够长的示例工具描述，用于演示 Schema 压缩效果，它必须超过函数描述二百五十六个字符的截断阈值，才能产生可记录的压缩收益。这是一段刻意写得足够长的示例工具描述，用于演示 Schema 压缩效果，它必须超过函数描述二百五十六个字符的截断阈值，才能产生可记录的压缩收益。这是一段刻意写得足够长的示例工具描述，用于演示 Schema 压缩效果，它必须超过函数描述二百五十六个字符的截断阈值，才能产生可记录的压缩收益。"}]' | tokenless compress-schema --batch
```

如果 stderr 输出 `did not reduce size`，说明当前工具集没有可压缩内容；带有长描述的工具集（例如部分 MCP 工具）会正常产生记录。

### 2. 确认 BeforeModel Hook 已触发

在 cosh 与 Cosh-NG 上，BeforeModel 事件没有可供 Schema 压缩处理的内容时，Hook 会给出以下警告之一（每条均为每个会话最多一次）并原样放行：

```text
[tokenless] WARNING: BeforeModel payload is not a JSON object ...
[tokenless] WARNING: BeforeModel payload carries no llm_request object ...
[tokenless] WARNING: BeforeModel event carries no tool declarations ...
```

第一条警告表示 Hook 收到的负载不是 JSON 对象；第二条表示负载缺少 `llm_request` 对象；第三条表示宿主已发射 BeforeModel，但事件格式不带工具声明（`llm_request.config.tools` 或 `llm_request.tools`），应升级或检查宿主的 Hook 协议版本。既没有警告也没有记录时，说明 BeforeModel 根本没有触发，确认：

- 扩展或插件已安装并启用（`anolisa adapter status tokenless`）。
- 宿主配置没有禁用 Hooks。
- 宿主版本支持 BeforeModel 事件。

之后按[启用后没有产生统计记录](#启用后没有产生统计记录)的通用步骤继续排查。

## Adapter 启用失败

常见原因：

- 目标 Agent 产品未安装或未被扫描到。
- 框架版本不满足 Adapter 要求。
- adapter 命令的执行用户与目标框架配置或 receipt 的所属用户不同。
- 直接安装的 Tokenless RPM 尚未写入 ANOLISA 状态。
- npm 安装没有 anolisa 组件记录，却尝试使用 `anolisa adapter enable`。
- OpenClaw 安全策略拒绝 Plugin 所需的 unsafe-install 覆盖参数。

先运行：

```bash
anolisa adapter scan
anolisa --verbose adapter enable tokenless <framework>
```

npm 安装请使用[Agent 集成 · npm 安装后的手动接入](framework-integration.md#npm-安装后的手动接入)。

直接安装 RPM 后，请先补充状态记录，再用拥有目标框架配置的用户重试 adapter
命令。

```bash
sudo yum install anolisa
sudo anolisa --install-mode system adopt tokenless
```

由 anolisa 管理的安装第一次不会绕过 OpenClaw 安全扫描。只有错误明确给出此建议时，才应在审查报告后重试：

```bash
anolisa adapter enable tokenless openclaw \
  --allow-unsafe-plugin-install
```

npm/手动安装脚本的区别在于“如何同意”而非“是否同意”：在安装器仍声明该参数有效时（旧版宿主），脚本会自动附加 `--dangerously-force-unsafe-install`，因为 Plugin 会启动固定的 `tokenless` 和 `rtk` 子进程。将该参数标记为 deprecated no-op 的宿主（OpenClaw 2026.6.5+）不会收到该参数——此时安全扫描由 `security.installPolicy` 决定，安装被拒时应由运维放宽该策略解决，而不是重跑脚本。应先审查 Adapter 和安全策略；策略禁止该覆盖参数时不要启用。

## QwenPaw 安装提示 SDK wheel 不可用

QwenPaw 插件包会从 GitHub Release 资产安装原生 Python SDK，该资产的版本与软件包版本严格一致，
因此 wheel 与 RPM 总是同版本。当该资产无法下载时（最常见的原因是版本号提升先合入 `main`、
而对应的 `tokenless/vX.Y.Z` Release 尚未发布），安装脚本会在把插件交给 QwenPaw 之前停止：

```text
[tokenless] The Tokenless 0.8.2 Python SDK wheel asset is unavailable (HTTP 404):
[tokenless]   https://github.com/alibaba/anolisa/releases/download/tokenless/v0.8.2/anolisa_tokenless-0.8.2-cp311-abi3-manylinux_2_17_x86_64.manylinux2014_x86_64.whl
[tokenless] This package was built from a source tree already at 0.8.2, but that asset
[tokenless] cannot be downloaded. A maintainer must check whether the GitHub Release
[tokenless] `tokenless/v0.8.2` exists:
```

该 URL 返回 `404` 只能说明这个资产无法下载，不能说明 Release 不存在：资产上传中断会留下
“Release 存在、但缺少本架构 wheel” 的状态。直接请求报错信息中的 URL 即可确认这个 `404`——
返回 `200` 说明 wheel 存在、失败另有原因：

```bash
curl -sIL -o /dev/null -w '%{http_code}\n' "<wheel URL from the message>"
```

- 维护者，`tokenless/vX.Y.Z` Release 不存在：推送该 tag 并审批 `release` environment，
  让发布 workflow 上传 wheel 资产，然后重跑安装脚本。
- 维护者，Release 已存在但缺少该 wheel：说明资产上传不完整。发布 workflow 拒绝覆盖已存在的
  Release，应先删除该 Release 再重跑 workflow（或把缺失资产补传到该 Release），然后重跑安装脚本。
- 其他用户：安装版本已有可下载 wheel 的 Tokenless 软件包。
- 离线或镜像网络（探测无法访问 GitHub，但 pip 能从本地镜像解析 wheel）：
  用 `ANOLISA_SKIP_WHEEL_PREFLIGHT=1` 重跑安装脚本。
  `ANOLISA_TOKENLESS_PROBE_TIMEOUT` 用于设置单次探测的超时秒数（默认 15）。

该探测只是前置提示：没有 `curl` 和 `python3`、Release 站点不可达、
单次探测超过 `ANOLISA_TOKENLESS_PROBE_TIMEOUT`、或返回非 `404` 的状态码时，
安装脚本仍会把判定交给 pip，并原样输出 pip 的报错。

## 命令没有被重写

不是所有命令都有 RTK 重写规则。先独立测试：

```bash
rtk rewrite "ls -la"
```

如果 `rtk` 不存在：

```bash
command -v rtk
```

如果 RTK 正常但 Agent 中不生效，检查框架支持矩阵、Adapter 状态和是否已经重启会话。

`TOKENLESS_COMPRESSION_ENABLED=0` 不会关闭命令重写。命令重写默认关闭；在宿主环境中设置 `TOKENLESS_RTK_ENABLED=0` 可覆盖显式 SDK/插件启用配置，保留原始 Shell 输入。

## Tool Ready 仍然报告 `NOT_READY`

当前构建已硬关闭 Tool Ready，不会输出 `NOT_READY` 或阻断工具。先确认实际生效的二进制：

```bash
tokenless --version
tokenless env-check --tool <name> --json
```

JSON 应包含 `"status":"UNKNOWN"` 和 `"enabled":false`。如果仍得到 `NOT_READY`，说明线上混用了新旧版本。请同时更新 Tokenless 二进制和共享 Adapter 资源，然后重启 Agent。旧的 `TOKENLESS_TOOL_READY_ENABLED` 环境变量不会生效。

## 数据库错误

### `Failed to open database`

```bash
ls -ld ~/.tokenless
ls -l ~/.tokenless/stats.db*
env | grep -E 'TOKENLESS_(DATA_DIR|STATS_DB|STASH_DB)='
```

确认当前用户对选定的数据目录和数据库可写。`TOKENLESS_DATA_DIR` 可以位于真实用户 home 之外，但必须是不包含父目录遍历的绝对非根目录；显式数据目录无效时不会回退到 home。`TOKENLESS_STATS_DB` 和 `TOKENLESS_STASH_DB` 必须位于真实用户 home 或选定的数据目录下，随包 RTK 写入器也执行相同规则。

不要让多个用户共享同一个 `stats.db`。AgentSight 和 Tokenless 应以能访问同一用户数据库的方式运行。

### SLS JSONL 没有记录

```bash
tokenless stats status
test -e /var/log/anolisa/sls/ops/tokenless.jsonl
```

SLS 开关默认开启，但 Tokenless 不创建目标文件。文件不存在时会静默跳过。自定义路径必须位于 `/var/log/` 或 `/tmp/`。

## `retrieve` 返回空或失败

检查：

1. Hash 是否为完整的 24 个十六进制字符。
2. 压缩时是否使用了 `--no-stash`。
3. 压缩是否处于 active，而不是 dry-run。
4. 是否已超过默认 1 小时 TTL，或被 10,000 条容量策略淘汰。
5. 压缩和取回是否使用相同的用户与数据库路径。
6. 压缩时 stderr 是否报告 Stash 写入失败。

```bash
ls -l ~/.tokenless/stash.db*
env | grep '^TOKENLESS_STASH_DB='
```

显式指定同一数据库重试：

```bash
tokenless retrieve <hash> --stash-db ~/.tokenless/stash.db
```

过期或从未成功写入的内容无法恢复。

## 有统计记录但 Prompt 没有变小

先查看[支持矩阵](framework-integration.md#agent-adapter-支持矩阵)中的响应交付路径。Qoder 通过
`updatedToolOutput` 替换输出。Qwen Code 与旧版 Copilot Shell 的当前契约没有替换字段，
因此会透传工具后输出，也不会通过 `additionalContext` 注入压缩副本。Codex 同样不执行响应
压缩，其 PostToolUse Context 只包含识别出的环境失败；实际节省应在经过 RTK 改写的 Shell
调用上测量。

Claude Code 需要 2.1.121 或更高版本才能替换响应；旧版本或无法识别版本时会透传原文。
OpenClaw 在 `post_tool_enabled` 开启时优化受支持的持久化结果。Tokenless 会在 JSON 清理或
TOON 能产生更小合法结果时自动选择对应方式。

## Qoder Plugin 缓存问题

仅在升级后出现以下错误时执行本节：

```text
python3: can't open file '/rewrite_hook.py'
```

刷新 Adapter：

```bash
anolisa adapter disable tokenless qoder
anolisa adapter enable tokenless qoder
```

确认缓存中没有未展开的占位符：

```bash
grep -R -n 'QODER_TOKENLESS_HOOKS' \
  ~/.qoder/plugins/cache/local/tokenless*/*/hooks.json 2>/dev/null
```

预期无输出。之后完全退出并重启 Qoder IDE。

## anolisa 与 RPM 状态不一致

如果曾直接运行 `dnf remove` 或 `rpm -e`：

```bash
sudo yum install anolisa
sudo anolisa --install-mode system repair tokenless
```

按照 repair 输出的计划操作。只有在 RPM 仍存在且输出明确要求重建记录时，才依次执行：

```bash
sudo anolisa --install-mode system forget tokenless
sudo anolisa --install-mode system adopt tokenless
```

`forget` 只删除 anolisa 状态，不卸载 RPM。

## 升级与卸载

### anolisa 安装

升级：

```bash
anolisa update tokenless
anolisa adapter status tokenless
anolisa doctor tokenless
```

system mode：

```bash
sudo anolisa update tokenless
```

升级后重启已启用的 Agent。通常不需要重新启用 Adapter；如果状态报告资源不一致，再按诊断结果 disable/enable。

卸载前先列出并禁用所有 Adapter：

```bash
anolisa adapter status tokenless
anolisa adapter disable tokenless <framework>
anolisa uninstall tokenless
```

system mode 使用相同 scope。当前版本的 `--purge` 仅支持通过 `anolisa --dry-run uninstall --purge tokenless` 预览计划；不带 `--dry-run` 会返回 `NotImplemented`，不会卸载组件，也不会删除配置、缓存或状态。实际卸载请使用 `anolisa uninstall tokenless`，本地数据库处理见[清理数据](configuration-and-privacy.md#清理数据)。

### npm 安装

升级：

```bash
npm install -g anolisa-tokenless@latest
```

npm 会刷新 Adapter 资源，但框架中已注册的 Plugin 可能仍是旧副本。升级后重新运行目标框架的 `scripts/install.sh` 并重启框架。

卸载顺序：

```bash
bash ~/.local/share/anolisa/adapters/tokenless/<framework>/scripts/uninstall.sh
npm uninstall -g anolisa-tokenless
```

确认所有 npm 管理的 Adapter 已卸载后，可以删除 npm 复制到用户数据目录的资源：

```bash
rm -rf -- ~/.local/share/anolisa/adapters/tokenless
```

该命令只应在确认目录属于本次 Tokenless npm 安装后执行。cosh 的手动 Extension 需要单独确认并移除 `~/.copilot-shell/extensions/tokenless`。

这一步的确认由包的 postinstall 代劳：`~/.local/share/anolisa/adapters/tokenless` 与 anolisa CLI 共享，当它已属于受管组件安装时，postinstall 会保持原样不动，并打印包内资源的位置，而不是替换掉组件记录与框架注册仍然指向的目录。确需接管时设置 `ANOLISA_TOKENLESS_FORCE_ADAPTERS=1`，之后请重跑 `anolisa adapter scan`，让组件记录与磁盘内容一致。

这里的所有权必须被**证明**，而不是被假定。postinstall 只刷新带有它自己或独立安装脚本写下的标记（`.tokenless-owner`）的目录；由早于该标记的版本留下的目录、或手工拷贝出来的目录都没有标记，同样会被保留——它们的内容无法说明是谁放的，否则指向它们的框架注册就会悬空。代价是一次性的：从这样的版本升级时，旧资源会保留到该目录被删除或使用了强制接管为止。

卸载脚本同样宁可不做完也不做一半。当某个框架注册无法解除时，Adapter 资源与 receipt **都会保留**，脚本以非 0 退出，因此不会有注册指向已删除的目录，告警里提到的那个脚本也还在；修好该框架后重跑即可完成卸载。安装脚本是同一套 fail-closed 语义：替换之前先把原安装挪到一边，如果这份副本做不出来——建不出 staging 目录，或某个记录在案的文件读不到——就在写入任何内容之前停下。

### curl 独立安装

独立安装脚本会把创建的每一个路径记录到 `~/.local/share/tokenless/install-receipt`：最终走的方式（npm 或源码构建）、版本、安装目录、npm prefix、Adapter 目录、追加过 PATH 的 rc 文件，以及每个安装的文件及其 sha256。

升级就是重新执行安装脚本。它会覆盖记录在案的路径并重写 receipt，因此记录始终准确：

```bash
curl -fsSL https://raw.githubusercontent.com/alibaba/anolisa/main/src/tokenless/scripts/install.sh | bash
```

在同一台机器上切换安装方式——例如 npm 安装之后再用 `TOKENLESS_FORCE_BUILD=1` 重跑——会回收上一种方式创建的内容，因此不会残留新 receipt 不再记录的 `rtk` 启动器、npm 全局包或 Adapter 目录。回收只在新安装验证通过之后发生：脚本先把原安装挪到一边，新安装失败时再原样放回，因此 tag 缺失、构建失败或目录不可写都不会破坏原本可用的 CLI，receipt 也仍然与现实一致。若某个记录路径的身份已不再匹配，说明它已被另一种安装接管，脚本会保留它。

npm 到 npm 的升级同样在事务内。升级会就地替换软件包载荷，而启动器链接解析到的正是该载荷，因此在新 CLI 验证通过之前，脚本会保留旧模块目录、它的 `@anolisa` 平台包以及该 prefix 自己的 bin 链接的副本；新二进制损坏时会把旧的放回去，而不是留下一个仍解析到坏载荷的启动器。

新 receipt 正是退役旧安装的前提，所以它先写：先写到同目录下的临时文件，再移动到位并落盘。如果新 receipt 写不进去、而旧 receipt 也删不掉，本次安装直接失败并把原安装放回——一份描述着已被替换安装的过期 receipt，会让之后的 `scripts/uninstall.sh` 删掉新安装，因为同版本重装会复现记录在案的 sha256 与链接目标。若过期 receipt **能**删除，安装仍会成功，同时告警说明无法使用脚本化卸载。

当上一个 npm prefix 与安装目录重叠时——`npm install -g --prefix ~/.local` 会把 bin 链接放进 `~/.local/bin`，也就是安装脚本的默认安装目录——脚本改为手工回收该全局包，而不执行 `npm uninstall --prefix ~/.local`，否则新安装刚写入的 `~/.local/bin/tokenless` 会被一并删掉。中途失败的 npm 尝试也会按同样方式回滚，因此源码构建回退不会接手无主的软件包、启动器链接或 Adapter 目录。

所有权看的是身份而不只是内容。anolisa 或直接 npm 安装同一版本会留下逐字相同的二进制与 manifest，因此 receipt 还会记录本次安装的 id、每个启动器解析到的链接目标，以及写进 Adapter 目录和 npm 模块目录的所有权标记（`.tokenless-owner`）。凡身份或标记不再匹配的内容，卸载脚本都会保留——文件、Adapter 资源、框架注册与 npm 全局包都一样。

源码构建回退只支持 Linux。在 macOS 上安装脚本要么走 npm 路径，要么直接报错退出，绝不会执行 `cargo`。Intel Mac 也没有已发布的 npm 软件包，因此目前没有受支持的安装路径——见[快速开始](QUICKSTART.md#平台适配性)中的平台表格。

`~/.local/share/anolisa/adapters/tokenless` 与 anolisa CLI 以及直接执行的 `npm install -g` 共享。当该目录已属于其中之一时，npm 的 postinstall 不会碰它，安装脚本会把它与自己事先拍下的快照对比、**只有确实被替换过才恢复**，不记录 Adapter 目录，并明确提示。此后卸载脚本不会触碰这些资源，也不会触碰指向它们的框架注册。快照根本拍不下来时，脚本会在 `npm install -g` 替换任何内容之前停下。

随后重启 Agent。如果走的是 npm 路径，框架中已注册的 Plugin 可能仍是旧副本——按照 [npm 安装](#npm-安装)重新执行该框架的 `scripts/install.sh`。

卸载使用配套脚本，它只删除 receipt 中记录的内容：

```bash
curl -fsSL https://raw.githubusercontent.com/alibaba/anolisa/main/src/tokenless/scripts/uninstall.sh | bash
```

可以先预览计划，或同时清除已收集的统计数据：

```bash
bash src/tokenless/scripts/uninstall.sh --dry-run
bash src/tokenless/scripts/uninstall.sh --purge
```

`--dry-run` 只打印将要删除的内容，不做任何改动。`--purge` 会额外删除运行时数据目录 `~/.tokenless`（其中包含 `stats.db` 和 `stash.db`）；不加该参数时数据保留。`--receipt <path>` 读取非默认的 receipt，与 `TOKENLESS_RECEIPT` 等价。

卸载脚本宁可不做完也不做一半，并以非 0 退出明确表态。无法解除的框架注册会同时保住 Adapter 资源与 receipt——删掉资源只会让那条注册指向不存在的路径。无法删除的 npm 全局包（例如 PATH 上没有 npm）会保住软件包、该 prefix 下的启动器链接与 receipt：此时 Tokenless 仍能从这个 prefix 运行，而一旦回执被删，prefix 与其所有者就再无记录，重跑也无法收尾。排除原因后重跑即可，receipt 正是重试的前提。

重装到不同的 `TOKENLESS_INSTALL_DIR` 时，安装脚本还会回收它为上一个目录追加的 PATH 条目。回执只记录一个 rc 文件和一个目录，否则旧目录那一块会在每次卸载之后都残留下来。

删除范围取决于记录的方式。走 npm 路径时，脚本会从记录的安装目录删除记录的启动器二进制（包括自定义的 `TOKENLESS_INSTALL_DIR`），对记录的 prefix 执行 `npm uninstall -g anolisa-tokenless`，并且只在该次 npm 安装创建了 Adapter 资源副本时才删除它——删除前会先执行每个内置框架自己的 `scripts/uninstall.sh`，因此已启用的 OpenClaw、Hermes 或 Qwen Code 注册会被解除，而不是留下指向已删除目录的引用。走源码构建时只删除 `tokenless` CLI，因为这条路径不安装 `rtk`，也不安装 Adapter 资源。两种方式下，共享同一目录的 anolisa CLI 安装或手动 npm 安装都不会受影响。

不要改用固定的 `rm -f ~/.local/bin/tokenless ~/.local/bin/rtk` 列表：它会漏掉自定义的 `TOKENLESS_INSTALL_DIR` 和 npm 全局包，而在源码构建安装之后，它删除的正是那条路径从未创建过的 `rtk` 和 Adapter 资源。

没有 receipt 时，卸载脚本会拒绝猜测，改为打印按方式区分的手动步骤。请改用实际使用的方式卸载：[anolisa 安装](#anolisa-安装)、[npm 安装](#npm-安装)，或下面的 YUM/RPM 流程。

### YUM/RPM 安装

优先通过 anolisa 的 system scope 管理。如果安装记录不由 anolisa 拥有，先禁用 Adapter，再执行：

```bash
sudo yum update tokenless
sudo yum remove tokenless
```

升级或卸载不会自动清理用户 home 下的 Tokenless 运行时数据库。

## 仍无法解决

收集以下信息时先检查并移除敏感内容：

```bash
tokenless --version
anolisa --version
anolisa doctor tokenless
anolisa adapter status tokenless
tokenless stats status
tokenless env-check --all --json
```

不要附加 `stats.db`、`stash.db` 或未经审查的 `tokenless stats show` 输出。
