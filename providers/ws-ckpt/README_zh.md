# AW 的 ws-ckpt Provider

[English](README.md)

此 Provider 在 AW 的 `input.submit` Hook 调用已安装的 `ws-ckpt` CLI。
自动快照默认关闭。请分别安装 AW 和兼容的 ws-ckpt CLI、守护进程。
需要在对话里绑定工作区和管理开关时，一并安装 ws-ckpt 的 Skill。

## 构建和安装

在 Linux 上从仓库根目录构建。

```bash
./scripts/build-all.sh --component aw-provider-ws-ckpt --no-install
make -C providers/ws-ckpt install PREFIX=/usr/local DESTDIR=/absolute/path/to/staging-root
```

原生安装把 Provider 放在 `<prefix>/libexec/aw/providers/ws-ckpt/`，
完整示例放在 `<prefix>/share/aw/providers/ws-ckpt/examples/`。如果设置了
`AWPRJ`，使用该命名空间下的路径。原生安装只复制 Provider 文件。

如果使用 AW 归档，解压后用 `aw install --bundle` 传入归档的绝对解压目录
和 Provider 安装前缀。

## 配置和启动

归档的 `templates/ws-ckpt.json` 可通过 `aw config init --template` 配置
Qoder 的 `input.submit` Hook。用 `--template-var` 将 `PROVIDERS_DIR` 设为
已安装的 `ws-ckpt` Provider 目录的绝对父路径，将 `WS_CKPT_CLI` 设为后端
可执行文件的绝对路径。完整的 `examples/aw.yaml` 还展示了如何设置 Skill
安装源和目标目录。Agent 的 `PATH` 和 Provider 的 `--cli` 应指向同一个
`ws-ckpt` 可执行文件。

用 ws-ckpt 注册工作区后，从工作区外执行下面的命令。`PROVIDERS_DIR` 和
`WS_CKPT_CLI` 使用生成配置时的路径。`WS_CKPT_AW_TARGET` 填与 `--agent`
一致的目标名，`WS_CKPT_AW_PROVIDER` 指向已安装的 Provider 可执行文件。

```bash
PROVIDERS_DIR=/absolute/provider/prefix/libexec/aw/providers
WS_CKPT_CLI=/opt/bin/ws-ckpt

WS_CKPT_AW_TARGET=qoder \
  WS_CKPT_AW_PROVIDER="$PROVIDERS_DIR/ws-ckpt/aw-provider-ws-ckpt" \
  PATH="$(dirname "$WS_CKPT_CLI"):$PATH" \
  aw run --config /absolute/path/to/aw.yaml --agent qoder
```

进入 Agent 会话后，通过 ws-ckpt Skill 绑定工作区并开启自动快照。
无人值守部署可以用已安装的 Provider 执行
`aw-provider-ws-ckpt bind --agent <目标> --workspace <绝对路径>`。
Provider 将绑定和开关保存在系统账户主目录下的
`.local/state/ws-ckpt/aw-policy.json`。卸载由 AW 管理的 Provider 时，执行
`aw uninstall --prefix /absolute/provider/prefix aw-provider-ws-ckpt`。
