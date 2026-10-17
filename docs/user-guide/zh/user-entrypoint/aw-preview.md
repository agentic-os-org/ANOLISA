# 安装并使用 AW Preview

[English](../../en/user-entrypoint/aw-preview.md)

只安装 AW core 即可运行已有的 Qoder 或 OpenClaw，无需 Provider。按需添加 sec-core 或自定义布尔判断脚本，使用同一份 AW 配置。AW 和 sec-core 的构建、安装及运行使用 Rust 二进制，不依赖 Python；用户脚本自行承担其运行依赖。OpenClaw 仍需 Node.js，各 Agent 的模型账户由其原生配置管理。

此 Preview 尚未通过 `anolisa install` 或 RPM 发布。请从运行成功的
**CI / AW Integration** 下载 `aw-preview-linux-x86_64-<candidate_sha>`。
该流程每天北京时间 03:23 在 `main` 上运行；验收已评审提交或发布候选时，
手动运行并传入该提交完整的 40 位 `candidate_sha`。使用包前，须等待包括
**AW / Integration result** 在内的整个流程通过。日常 PR 检查不生成 Preview 包。
手动触发方式和原生验收边界见
[集成验收说明](../../../../src/aw/CONTRIBUTING_zh.md#日常-ci-与-nightly-集成)。

从源码到组件接入的连续步骤及无需后端的策略示例见
[组件教程](../../../developer-guide/zh/aw/integration.md)。需要 QwenPaw 或 Hermes 共用
Provider 时，复制[四框架配置](aw.md#四框架使用同一份配置)，填写已安装的二进制路径。
下方 `aw-package configure` 仅生成 Qoder/OpenClaw 条目，不初始化原生 Agent profile。
容器状态目录与卷要求见[运行指南](aw.md#容器与持久化数据)。

## 1. 安装包

需要 Linux、glibc 2.39 或更新版本（Ubuntu 24.04），选择与 CPU 架构匹配的包。CI 构建 x86_64，同一原生构建命令支持 aarch64。先解压下载的 Actions artifact ZIP。

```bash
sha256sum -c SHA256SUMS
AW_PREVIEW_VERSION=0.1.0-preview.1
AW_PREVIEW_BUNDLE="aw-core-$AW_PREVIEW_VERSION-linux-$(uname -m)"
tar -xzf "$AW_PREVIEW_BUNDLE.tar.gz"
sudo install -d -m 755 /opt/aw-preview
export AW_PREVIEW_PREFIX="/opt/aw-preview/$AW_PREVIEW_VERSION"
sudo "./$AW_PREVIEW_BUNDLE/aw-package" install --prefix "$AW_PREVIEW_PREFIX"
```

默认构建只生成 core；`--component all` 生成四个包。`aw-core` 包含 `aw` 和 `aw-package`；`aw-provider-sec-core` 包含 Provider 和 Rust V2 sec-core CLI/daemon；`aw-provider-tokenless` 包含桥接、Tokenless CLI 和固定版本带补丁的 RTK；`aw-all-in-one` 包含完全相同的三个组件。分包安装时，分别解压 core 和 Provider，依次用包内 `aw-package install` 安装到同一个新前缀，**先装 core**，之后不要再往该前缀安装 all-in-one。

安装器校验哈希、权限、架构、版本及源码提交，拒绝覆盖，并在 `.aw-packages` 记录文件归属。安装过程中收到 Ctrl-C（SIGINT）或 SIGTERM 会回滚本次写入，随后可重试；强制终止（SIGKILL）不在自动回滚范围内。校验和用于发现损坏，不是发布者签名。安装父目录须归安装用户所有，且不可被其他用户写入。系统后端的安装由 root 管理；若连接已有后端，也支持普通用户安装到自己的目录。

## 无 Provider 使用 core

只需安装所选 Agent：保留[原生命令 Hook 合同](aw.md#构建并启动-qoder)的 Qoder CLI >=1.1.64，或 OpenClaw **2026.9.6** 与 Node.js。以普通用户生成 Qoder 的基础配置：

```bash
export AW_PREVIEW_PREFIX="/opt/aw-preview/0.1.0-preview.1"
export AW_QODER=/absolute/path/to/qodercli
export AW_DEMO="$HOME/aw-preview"
install -d -m 700 "$AW_DEMO" "$AW_DEMO/workspace"
"$AW_PREVIEW_PREFIX/bin/aw-package" configure --prefix "$AW_PREVIEW_PREFIX" \
  --config "$AW_DEMO/aw-core.yaml" --state-dir "$AW_DEMO/core-state" \
  --qoder "$AW_QODER"
"$AW_PREVIEW_PREFIX/bin/aw" validate --config "$AW_DEMO/aw-core.yaml"
"$AW_PREVIEW_PREFIX/bin/aw" run --config "$AW_DEMO/aw-core.yaml" --agent qoder
```

只使用 OpenClaw 时，将 `--qoder "$AW_QODER"` 换成 `--node "$AW_NODE" --openclaw "$AW_OPENCLAW"`；启动时按第 5 步提供原生配置和状态目录。也可同时提供两个 Agent 入口。不需要 sec-core socket 或 Provider 包。空 `providers` 和 `events` 表示没有 AW 策略检查或每次工具调用的 AW 审计；原生权限和已有 Hook/插件继续生效。AW 仍创建实例并启动或复用共享服务。退出 Agent 后用 `aw stop --config "$AW_DEMO/aw-core.yaml"` 停止该服务。

`configure` 就是初始化命令：安装 core 后，它生成包含 Agent、daemon、execution
和 audit 设置的完整 `aw.yaml`。不传 `--provider`（或传 `--provider none`），
`providers` 和 `events` 都为空。它只创建新文件，拒绝覆盖已有配置。若要一次生成
带策略的配置，按[布尔脚本指南](aw.md#布尔判断脚本)选择 `--provider command`。
后续逐项添加规则则按 `configure` → `config add-provider` → `config add-event` →
`config add-hook` → `config validate` 执行。所有 `config` 命令（包括 `add-*`）都要求
已有合法文件，不会自动初始化；只有事件不存在时才需要 `add-event`。
QwenPaw/Hermes 或四框架共用配置则复制
[共用模板](../../../developer-guide/zh/aw/integration.md#2-准备四框架共用配置)并填写路径；
当前 `configure` 只生成 Qoder/OpenClaw 条目，不初始化框架原生 profile。

## 编辑已有 Provider 与 Hook 配置

以配置文件所属用户运行 `aw-package config`，查看、校验和修改已有 YAML 或 JSON
文件。这些命令离线执行，不运行 Provider 或 Hook、不安装原生回调、不启动 Agent，
也不重载服务。修改前退出当前 Agent 会话，并使用原配置停止服务；修改后使用新文件
重新运行 Agent。新配置字节选择新的 revision，已有服务不会自动采用修改。

所有操作均要求 `--config` 指定无符号链接的绝对文件路径。文件须为当前用户拥有的
普通文件，不能有硬链接、特殊权限位、扩展访问 ACL 或组/其他用户写权限；父目录也须归当前用户
所有且不可被组/其他用户写入。不支持原生 NFSv4/rich/CIFS 访问 ACL，即使等价于普通权限位
也会拒绝；请使用不带这些属性的配置文件及父目录。暂存目录继承的原生 ACL 也会被拒绝。
未知、重复或不完整选项都会报错。定义文件可用绝对
路径或相对于当前目录的路径，须为普通文件，只包含一个完整 YAML/JSON 对象，
不支持标准输入。输入及展开 JSON 上限为 4 MiB，深度上限 32。重复键、非字符串键、
YAML merge key/自定义 tag、非有限数值和多文档输入都会被拒绝。未知公共字段报错，
Provider 私有 `config` 中的键保持不透明。

必须挂载 Linux procfs。祖先目录改名后，已打开的编辑器仍绑定原父目录；替换路径
不能将其编辑重定向到其他目录。

| 命令 | 行为 |
| --- | --- |
| `aw-package config show --config ABS_FILE` | 以格式化 JSON 打印完整工作配置 |
| `aw-package config show --config ABS_FILE --provider NAME` | 打印一个 Provider |
| `aw-package config show --config ABS_FILE --event EVENT [--id ID]` | 打印一个事件，或其中按事件作用域 ID 选择的步骤 |
| `aw-package config validate --config ABS_FILE` | 校验完整配置，成功时打印 `Valid configuration` |
| `aw-package config add-provider --config ABS_FILE --name NAME --definition FILE` | 添加完整 Provider 定义 |
| `aw-package config remove-provider --config ABS_FILE --name NAME` | 删除没有引用的 Provider |
| `aw-package config add-event --config ABS_FILE --event EVENT --definition FILE` | 添加显式配置的事件 |
| `aw-package config add-hook --config ABS_FILE --event EVENT --definition FILE` | 追加完整步骤，定义中包含其 `id` |
| `aw-package config remove-hook --config ABS_FILE --event EVENT --id ID` | 删除指定事件下的步骤 |

`show` 会有意显示 Provider 私有值，须自行选择输出的展示或保存位置。Provider 和
事件选择器互斥，`--id` 必须与 `--event` 一起使用；指定的对象不存在时会报错。
`show` 和 `validate` 不改变原文件。

添加已有的相同定义时保留文件；同名对象或同一事件下同 ID 步骤已有不同定义时报告
冲突。删除不存在的对象或步骤也保留文件。成功的修改操作打印 `Updated configuration`
或 `Configuration unchanged`。删除 Provider 前须先删除所有引用步骤，包括禁用的
步骤。删除事件最后一个步骤后仍保留事件选项及空 `steps` 数组。`add-hook` 要求事件
已存在；`add-event` 显式指定是否启用事件，不覆盖已有事件的选项。

每条修改命令原子发布一个操作。多条命令分别发布，后续命令失败时，先前已成功的
命令仍然生效。

例如，在上方创建的空 core 配置中添加缺省布尔工具前 Hook。先完成初始化；若已
启动 Agent，退出并停止服务后再编辑。演示脚本读取标准化事件，输入包含
`AW_DENY_TEST` 时返回 JSON true，不执行输入。Python 3 是这个可选脚本的依赖。
写 Provider 定义时由 shell 展开 `AW_DEMO`，保存的 argv 已是绝对脚本路径；
AW 不会对 YAML 中的变量进行插值。

```bash
cat > "$AW_DEMO/check-tool.py" <<'PYTHON'
import json
import sys

event = json.load(sys.stdin)
print(json.dumps("AW_DENY_TEST" in json.dumps(event["tool"]["input"], ensure_ascii=False)))
PYTHON
cat > "$AW_DEMO/provider.yaml" <<YAML
transport:
  type: stdio
  location: agent
  argv: [/usr/bin/python3, "$AW_DEMO/check-tool.py"]
timeout_ms: 1000
max_output_bytes: 32
YAML
cat > "$AW_DEMO/event.yaml" <<'YAML'
enabled: true
required: true
steps: []
YAML
cat > "$AW_DEMO/hook.yaml" <<'YAML'
id: check-marker
provider: company
effects: [block]
reason_code: demo_marker_denied
on_error: block
YAML
chmod 600 "$AW_DEMO/check-tool.py" "$AW_DEMO/provider.yaml" "$AW_DEMO/event.yaml" "$AW_DEMO/hook.yaml"
"$AW_PREVIEW_PREFIX/bin/aw-package" config add-provider --config "$AW_DEMO/aw-core.yaml" \
  --name company --definition "$AW_DEMO/provider.yaml"
"$AW_PREVIEW_PREFIX/bin/aw-package" config add-event --config "$AW_DEMO/aw-core.yaml" \
  --event tool.before --definition "$AW_DEMO/event.yaml"
"$AW_PREVIEW_PREFIX/bin/aw-package" config add-hook --config "$AW_DEMO/aw-core.yaml" \
  --event tool.before --definition "$AW_DEMO/hook.yaml"
"$AW_PREVIEW_PREFIX/bin/aw-package" config show --config "$AW_DEMO/aw-core.yaml" \
  --event tool.before --id check-marker
"$AW_PREVIEW_PREFIX/bin/aw-package" config validate --config "$AW_DEMO/aw-core.yaml"
```

Provider 省略 `protocol` 就使用 `aw-hook/v1alpha1`，步骤省略 `operation` 就默认
`invoke`。AW 只在已准入事件触发时调用脚本，脚本无需 method 分发或 response
对象。true 请求步骤中的 `block` 效果与原因码，false 不请求效果；错误按
`on_error: block` 处理。将脚本替换为自己的布尔规则即可。已有原生脚本使用显式
[原生 Hook 形式](aw.md#接入自己的程序)。用上方的 `aw run` 命令和编辑后的配置
验证原生行为；离线校验不执行脚本。

后续撤销演示策略时，先退出 Agent 并停止服务，再删除引用步骤及 Provider。
空事件会保留；脚本保留到不再使用它或清理演示目录时。

```bash
"$AW_PREVIEW_PREFIX/bin/aw-package" config remove-hook --config "$AW_DEMO/aw-core.yaml" \
  --event tool.before --id check-marker
"$AW_PREVIEW_PREFIX/bin/aw-package" config remove-provider --config "$AW_DEMO/aw-core.yaml" \
  --name company
rm "$AW_DEMO/provider.yaml" "$AW_DEMO/event.yaml" "$AW_DEMO/hook.yaml"
```

实际修改会将文件原子替换为序列化后的 YAML，保留权限位、所属组及无关值，但重写注释、
格式和键顺序，不保留扩展属性及 ACL。实际修改要求文件可写并使用排他锁；无变化操作
使用只读描述符和共享锁，保留完整原字节及 inode。校验
错误、冲突、仍有引用、锁竞争和暂存失败都保留原文件。并发修改会报告快照过期，
不自动合并或重试。其他写入方须遵守同一文件锁，才能避免发布时的竞争。需要保留
原文本用于回滚时，应先备份；这是原子发布，不是断电持久性保证。通过校验仅证明
字段形态及静态引用正确，不代表 Provider 准入或原生效果已被采用。

## 2. 按需安装并启动 sec-core

纯 core 使用可跳过第 2–5 步。安全演示先将匹配版本的扩展安装到 core 前缀：

```bash
AW_PREVIEW_VERSION=0.1.0-preview.1
AW_PREVIEW_PROVIDER="aw-provider-sec-core-$AW_PREVIEW_VERSION-linux-$(uname -m)"
export AW_PREVIEW_PREFIX="/opt/aw-preview/$AW_PREVIEW_VERSION"
tar -xzf "$AW_PREVIEW_PROVIDER.tar.gz"
sudo "./$AW_PREVIEW_PROVIDER/aw-package" install --prefix "$AW_PREVIEW_PREFIX"
```

在独立终端以 root 启动包内后端，保持前台运行，Ctrl-C 停止。配置和数据放在安装前缀之外。

```bash
export AW_PREVIEW_PREFIX="/opt/aw-preview/0.1.0-preview.1"
sudo install -d -m 755 /run/aw-preview-sec /etc/aw-preview
sudo install -d -m 700 /var/lib/aw-preview
printf '{"stateDir":"/var/lib/aw-preview/skillsec"}\n' | sudo tee /etc/aw-preview/skillsec.json
sudo chmod 600 /etc/aw-preview/skillsec.json
sudo env AGENT_SEC_DATA_DIR=/var/lib/aw-preview/sec-data OTEL_SDK_DISABLED=true \
  "$AW_PREVIEW_PREFIX/libexec/aw/providers/sec-core/agent-sec-daemon" serve \
  --socket /run/aw-preview-sec/daemon.sock --skillsec-config /etc/aw-preview/skillsec.json
```

包内还包含认证探测 `agent-sec-daemon-health` 和由回执校验的官方服务/配置
参考资产。持久后台应复用已有 root 系统服务或独立安装 sec-core 系统包。
AW 安装/卸载不会修改该服务或 `/etc`，不得让 root 服务执行用户可写前缀中的程序。
参见[后台归属和健康诊断](aw-sec-core.md#包内后台与显式健康诊断)。

## 3. 生成一份 AW 配置

另外准备 Qoder CLI >=1.1.64、OpenClaw **2026.9.6** 及兼容的 Node.js。Qoder 须保留
`--settings`、以字面量 `args` 执行的命令 Hook、`PreToolUse`/`PostToolUse`、
回调 ID/输入及以退出码 2 阻断工具的合同，详见 [Qoder 支持合同](aw.md#构建并启动-qoder)。
AW 记录成功探测到的非空版本（最多 128 个 UTF-8 字节），以 1.1.64 为语义版本下限；版本探测
不代表原生效果已被采用。OpenClaw 保留版本检查。将下面绝对路径替换为实际
安装路径，其余步骤以普通用户执行。

```bash
export AW_PREVIEW_PREFIX="/opt/aw-preview/0.1.0-preview.1"
export AW_QODER=/absolute/path/to/qodercli
export AW_NODE=/absolute/path/to/node
export AW_OPENCLAW=/absolute/path/to/openclaw/openclaw.mjs
export AW_DEMO="$HOME/aw-preview"
install -d -m 700 "$AW_DEMO" "$AW_DEMO/workspace" "$AW_DEMO/openclaw"
"$AW_PREVIEW_PREFIX/bin/aw-package" configure --prefix "$AW_PREVIEW_PREFIX" \
  --config "$AW_DEMO/aw.yaml" --state-dir "$AW_DEMO/state" \
  --provider sec-core --socket /run/aw-preview-sec/daemon.sock --qoder "$AW_QODER" \
  --node "$AW_NODE" --openclaw "$AW_OPENCLAW"
```

Qoder 和 Node 入口须可由当前用户执行；OpenClaw `.mjs` 文件须可读，无需执行权限。配置和 state 路径需为 Unix socket 留出空间（socket 路径少于 108 字节）。配置文件不能与 state 目录同路径或位于其祖先路径；已有 state 路径须为当前用户所有、权限为 `0700` 的目录。`configure` 从校验到发布全程持有包的共享锁，普通用户读取 root 所有的安装前缀时也适用；安装或卸载正在进行时，会报告可重试的锁错误。它仅复核 core 与显式选用的 Provider 文件哈希及权限，必需文件缺失或被修改时拒绝生成配置；未选用的 Provider 不成为初始化依赖。默认无策略；`--socket` 必须显式搭配 `--provider sec-core`，避免旧安全命令静默生成空策略。配置先写入私有临时文件，再排他提交；写入失败不会留下半份配置，可直接重试。两个 Agent 共用同一个 Provider 和 before/after 步骤：Qoder 的 `Bash`、OpenClaw 的 `exec` 都将 `/command` 输入交给 sec-core Bash 扫描。风险命令或执行前检查失败会阻断；执行后记录框架上报的结果（OpenClaw 也会上报被阻断的工具结果），其他工具未扫描。这里提供原生 Hook 拦截，不是 OS 沙箱或跨框架审批。

## 4. 测试 Qoder

若尚未登录，先通过 Qoder 完成登录。在新的工作目录中分别运行两次会话。提示词要求只调用一次工具，不重试、不换命令。

```bash
cd "$AW_DEMO/workspace"
"$AW_PREVIEW_PREFIX/bin/aw" run --config "$AW_DEMO/aw.yaml" --agent qoder -- \
  -p 'Use Bash exactly once to execute `printf AW_QODER_OK > qoder-allowed.marker`. Report the tool result; do not retry or use another tool.' \
  --dangerously-skip-permissions --no-session-persistence
cat qoder-allowed.marker
"$AW_PREVIEW_PREFIX/bin/aw" run --config "$AW_DEMO/aw.yaml" --agent qoder -- \
  -p 'Use Bash exactly once to execute `git -c http.sslVerify=false --version > qoder-blocked.marker`. Report the tool result; do not retry or use another tool.' \
  --dangerously-skip-permissions --no-session-persistence
test ! -e qoder-blocked.marker
```

第一条应写入 `AW_QODER_OK`。第二条应明确报告 `Tool blocked by AW policy`，且没有 blocked 标记文件。该 Git 命令仅打印版本，即使拦截失效也不会访问网络或修改 Git 配置。这里仅在受控演示会话跳过原生审批，便于直接观察 AW 结果；日常使用不需要该选项。模型拒绝、账号错误或没有调用工具，都不能算拦截成功。

## 5. 用同一份配置测试 OpenClaw

在 Gateway 和客户端两个终端中，先按相同路径重新执行第 3 步的 export 命令，再设置以下 profile 变量。使用独立原生 profile，保留已有 Agent 设置。首次执行 OpenClaw 引导，配置真实模型账号，跳过 daemon 安装、channels 和 skills。这个原生文件保存模型/Gateway 设置，不是第二份 AW 策略。

```bash
export OPENCLAW_STATE_DIR="$AW_DEMO/openclaw"
export OPENCLAW_CONFIG_PATH="$AW_DEMO/openclaw/openclaw.json"
"$AW_NODE" "$AW_OPENCLAW" onboard --classic --mode local \
  --gateway-bind loopback --gateway-port 18799 --no-install-daemon \
  --skip-channels --skip-skills --skip-health --skip-ui \
  --workspace "$AW_DEMO/workspace"
"$AW_NODE" "$AW_OPENCLAW" config set tools.allow '["exec"]' --strict-json
"$AW_NODE" "$AW_OPENCLAW" config set tools.exec.mode full
"$AW_PREVIEW_PREFIX/bin/aw" run --config "$AW_DEMO/aw.yaml" --agent openclaw \
  --native-settings "$OPENCLAW_CONFIG_PATH" --native-state-dir "$OPENCLAW_STATE_DIR"
```

保持该终端运行，等待 `AW openclaw native hooks ready`。在另一个设置了相同变量的终端中，通过此 Gateway 发送两轮请求（不要用绕过 Gateway 的 `--local`）。

```bash
"$AW_NODE" "$AW_OPENCLAW" agent --session-id aw-allow-$(date +%s) --json \
  --message 'Use exec exactly once to execute `printf AW_OPENCLAW_OK > openclaw-allowed.marker`. Report the tool result; do not retry or use another tool.'
cat "$AW_DEMO/workspace/openclaw-allowed.marker"
"$AW_NODE" "$AW_OPENCLAW" agent --session-id aw-block-$(date +%s) --json \
  --message 'Use exec exactly once to execute `git -c http.sslVerify=false --version > openclaw-blocked.marker`. Report the tool result; do not retry or use another tool.'
test ! -e "$AW_DEMO/workspace/openclaw-blocked.marker"
"$AW_PREVIEW_PREFIX/bin/aw" status --config "$AW_DEMO/aw.yaml"
```

第一轮应写入 `AW_OPENCLAW_OK`，第二轮应报告 AW 策略阻断且不创建标记文件。连同 Qoder 的结果，即可证明同一份 `aw.yaml` 和 sec-core 后端对两条原生工具链路均生效。AW 在外部 state 目录保留仅含元数据的审计，重启后仍可查询。两个 Agent 的模型凭据与会话属于各自框架，原生 profile 仍分别维护。

## 6. 停止、升级与卸载

退出 Qoder，Ctrl-C 停止前台 OpenClaw Gateway，再停止 AW。sec-core 终端也用 Ctrl-C 停止。

```bash
"$AW_PREVIEW_PREFIX/bin/aw" stop --config "$AW_DEMO/aw.yaml"
sudo "$AW_PREVIEW_PREFIX/bin/aw-package" uninstall --prefix "$AW_PREVIEW_PREFIX"
```

卸载先检查所有包属文件，遇到被修改或缺失的文件，会在删除任何文件之前报错。删除失败或完成前收到 SIGINT/SIGTERM 时，恢复已删除的文件和收据，随后可重试；SIGKILL 不在自动恢复范围内。若恢复也失败，诊断会列出保留的备份和未恢复路径。所有 payload 和收据删除后，卸载即成功；若备份清理失败，警告会列出残留备份路径，供手动删除。未知文件、前缀和锁、外部配置、凭据及审计数据均保留，确认不再需要后再自行删除。

升级时安装到新前缀、生成新配置，保留旧包和配置即可回退。此 Preview 不提供热加载或审计格式迁移；升级前备份 state，后续 Preview 若改变格式，应使用新的 state 目录。

## 构建产物（开发者）

在干净且已提交的工作树中，准备 Rust 1.97.1、C 编译器、pkg-config 和 OpenSSL 开发头文件。以下命令不需要 Python，输出目录必须是空的绝对路径。

```bash
cargo test --locked --manifest-path src/aw/Cargo.toml -p aw-package
cargo run --locked --release --manifest-path src/aw/Cargo.toml \
  -p aw-package --bin aw-build -- --version 0.1.0-preview.1 \
  --output "$PWD/target/aw-preview-packages"
```

Rust 构建器编译锁定依赖的 release 二进制，检查原生 ELF 架构，只生成所选压缩包及 `SHA256SUMS`。默认 `--component core` 不编译或读取 sec-core；`--component sec-core` 生成扩展包，`--component all` 生成四种包。扩展安装仍要求同一源码提交和版本的 core。发布失败或取消时，只清理本次创建的链接，以便重试；清理失败会报告具体残留路径。安装包 CI 使用真实 sec-core 扫描器，通过模拟事件验证两种工具映射及审计恢复，不将其作为原生 Agent 验收。真实模型会话按上文另行验证。AW 通用开发门禁仍保留交付链路之外的 Python 测试工具。

CI / AW Integration 每晚运行，也可针对确切源码提交手动触发；PR 不会自动运行包验收。两种触发方式的安装包产物均保留 3 天。完整集成结果通过后才能分发安装包；失败的任务保留诊断证据。支持重新构建，但不保证压缩包逐字节一致；统一发布入口后续交付。

## 可选 Tokenless PostTool 包

`--component tokenless` 构建 `aw-provider-tokenless`；`--component all` 也将它纳入
`aw-all-in-one`。扩展包含桥接、Tokenless CLI 和固定版本带补丁的 RTK，要求匹配的 core，沿用 sec-core 的
收据与完整性规则。通过 `configure --provider tokenless` 或
`--provider sec-core+tokenless --socket ABS_FILE` 显式启用压缩。四框架配置、原生
结果槽和故障行为见 [Tokenless 指南](aw-tokenless.md)。
