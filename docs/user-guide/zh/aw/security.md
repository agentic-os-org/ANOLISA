# 使用 sec-core

[English](../../en/aw/security.md)

sec-core 在工具执行前检查命令。发现风险或必需检查失败时，AW 可以阻止该次调用。
生成的 `scan-code` 步骤使用 `native:qoder:Bash` 和 `native:openclaw:exec` 选择
Qoder 的 `Bash` 和 OpenClaw 的 `exec`，只包含生成时选定的 Agent。
扫描其他工具须另行配置选择器和输入映射。
这项能力通过原生 Hook 生效，不提供 OS 沙箱隔离。

## 准备程序与后台

先[安装 core](installation.md)，再在解压后的 artifact 目录安装 sec-core 扩展包。
已安装 all-in-one 时跳过此步。

```bash
export AW_VERSION=0.1.0-preview.1
export AW_SEC_BUNDLE="aw-provider-sec-core-$AW_VERSION-linux-$(uname -m)"
tar -xzf "$AW_SEC_BUNDLE.tar.gz"
sudo "./$AW_SEC_BUNDLE/aw" install --scope system --bundle "$PWD/$AW_SEC_BUNDLE" --prefix "$AW_PREFIX"
```

这里沿用安装页的 system scope。若 core 安装在普通用户自己的前缀中，扩展包也应
去掉 `sudo` 并使用 `--scope user`。core、每个扩展包和卸载都须使用同一个 scope；
`sudo` 不会自动选择 system scope。

AW 不会自动启动 sec-core。
已有受管理后台时复用其 socket，例如 `/run/agent-sec-core/daemon.sock`。
长期运行请使用 [sec-core 的系统服务](../agent-sec-core/index.md)。

首次本地试用也可以在单独终端启动包内后台。下列命令要求 AW 安装在 root 管理的
`/opt/aw-preview` 下；配置文件必须是本次新建的，已有配置请先保留备份。

```bash
export AW_PREFIX=/opt/aw-preview/0.1.0-preview.1
sudo install -d -m 755 /run/aw-preview-sec /etc/aw-preview
sudo install -d -m 700 /var/lib/aw-preview
printf '{"stateDir":"/var/lib/aw-preview/skillsec"}\n' | sudo tee /etc/aw-preview/skillsec.json
sudo chmod 600 /etc/aw-preview/skillsec.json
sudo env AGENT_SEC_DATA_DIR=/var/lib/aw-preview/sec-data OTEL_SDK_DISABLED=true \
  "$AW_PREFIX/libexec/aw/providers/sec-core/agent-sec-daemon" serve \
  --socket /run/aw-preview-sec/daemon.sock --skillsec-config /etc/aw-preview/skillsec.json
```

保持该终端运行，结束试用时用 Ctrl-C 停止。root 后台不能运行来自用户可写目录的程序。

## 生成配置并运行

回到普通用户终端，沿用安装页的 `AW_PREFIX`、`AW_HOME` 和 `AW_QODER`。
`AW_SEC_SOCKET` 应指向实际后台；下例使用上一步启动的 socket。

```bash
export AW_SEC_SOCKET=/run/aw-preview-sec/daemon.sock
aw config init --prefix "$AW_PREFIX" \
  --config "$AW_HOME/security.yaml" --state-dir "$AW_HOME/security-state" \
  --provider sec-core --socket "$AW_SEC_SOCKET" --qoder "$AW_QODER"
aw validate --config "$AW_HOME/security.yaml"
cd "$AW_HOME/workspace"
aw run --config "$AW_HOME/security.yaml" --agent qoder
```

同时需要压缩时，安装 Tokenless 包并将选项改为 `--provider sec-core+tokenless`。
OpenClaw 使用[对应启动参数](configuration.md#openclaw)。

## 验证检查

在原生 Agent 中分两次请求执行下列命令，保持原生权限确认开启。

```bash
printf AW_SECURITY_OK
git -c http.sslVerify=false --version
```

第一条应正常返回。第二条只打印 Git 版本，不访问网络或修改配置，但命令中关闭 TLS
校验的选项应被规则识别，Agent 应报告 AW 策略阻断。
模型自行拒绝或没有调用工具不能算作 AW 检查通过；检查仍未执行时，查看 Agent 的原生提示。

检查失败时先确认 sec-core 后台与 socket。选中工具的输入或扫描错误按生成配置阻断，未选中的工具跳过扫描。

## 停止与扩展

退出 Agent 后执行 `aw stop --config "$AW_HOME/security.yaml"`。
前台试用后台用 Ctrl-C 停止；共享系统服务由其管理者维护。
卸载 AW 保留外部后台配置和数据。

检查其他工具时，同时修改[步骤选择器](configuration.md#为-hook-步骤选择工具)和[Provider 输入映射](../../../developer-guide/zh/aw/providers/sec-core.md#配置-provider)。
