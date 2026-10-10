# 安装并使用 AW Preview

[English](../../en/user-entrypoint/aw-preview.md)

只安装 AW core 即可运行已有的 Qoder 或 OpenClaw，无需 Provider。按需添加 sec-core 或自定义布尔判断脚本，使用同一份 AW 配置。AW 和 sec-core 的构建、安装及运行使用 Rust 二进制，不依赖 Python；用户脚本自行承担其运行依赖。OpenClaw 仍需 Node.js，各 Agent 的模型账户由其原生配置管理。

此 Preview 尚未通过 `anolisa install` 或 RPM 发布。请下载已评审提交上手动运行成功的 **CI / AW Packages** 产物。PR 自动产物带 `-validation` 后缀，仅用于合并候选验收，不作为发布包分发。

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

默认构建只生成 core；`--component all` 生成三个包。`aw-core` 包含 `aw` 和 `aw-package`；`aw-provider-sec-core` 包含 Provider 和 Rust V2 sec-core CLI/daemon；`aw-all-in-one` 包含完全相同的两个组件。分包安装时，分别解压 core 和 Provider，依次用包内 `aw-package install` 安装到同一个新前缀，**先装 core**，之后不要再往该前缀安装 all-in-one。

安装器校验哈希、权限、架构、版本及源码提交，拒绝覆盖，并在 `.aw-packages` 记录文件归属。安装过程中收到 Ctrl-C（SIGINT）或 SIGTERM 会回滚本次写入，随后可重试；强制终止（SIGKILL）不在自动回滚范围内。校验和用于发现损坏，不是发布者签名。安装父目录须归安装用户所有，且不可被其他用户写入。系统后端的安装由 root 管理；若连接已有后端，也支持普通用户安装到自己的目录。

## 无 Provider 使用 core

只需安装所选 Agent：Qoder CLI **1.1.64**，或 OpenClaw **2026.9.6** 与 Node.js。以普通用户生成 Qoder 的基础配置：

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

## 3. 生成一份 AW 配置

另外准备 Qoder CLI **1.1.64**、OpenClaw **2026.9.6** 及兼容的 Node.js；启动器会核对 Agent 版本。将下方绝对路径替换为实际安装位置。后续步骤使用普通用户运行。

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

Rust 构建器编译锁定依赖的 release 二进制，检查原生 ELF 架构，只生成所选压缩包及 `SHA256SUMS`。默认 `--component core` 不编译或读取 sec-core；`--component sec-core` 生成扩展包，`--component all` 生成三种包。扩展安装仍要求同一源码提交和版本的 core。发布失败或取消时，只清理本次创建的链接，以便重试；清理失败会报告具体残留路径。安装包 CI 使用真实 sec-core 扫描器，通过模拟事件验证两种工具映射及审计恢复，不将其作为原生 Agent 验收。真实模型会话按上文另行验证。AW 通用开发门禁仍保留交付链路之外的 Python 测试工具。

包 CI 自动覆盖 package crate、AW manifests、本指南和工作流变更；其他 AW/sec-core 变更在分发前需要针对确切源码提交手动运行。成功的 PR 产物保留 3 天，手动产物保留 14 天；失败时保留诊断证据，不上传安装包。支持重新构建，但不保证压缩包逐字节一致；统一发布入口后续交付。
