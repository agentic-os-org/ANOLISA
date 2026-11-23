# ANOLISA Omarchy 使用说明

[English](../../en/omarchy/index.md)

## 官方发布前安装候选包

需要 Omarchy Linux x86_64 和 Python 3.11+。
从同一次 Preview 或 review 交付中取得经过校验的安装包及相邻 `.sha256` 文件。
进入这两个文件所在目录：

```bash
sha256sum -c anolisa-omarchy-0.1.3-linux-x86_64.tar.gz.sha256
tar -xzf anolisa-omarchy-0.1.3-linux-x86_64.tar.gz
cd anolisa-omarchy-0.1.3
bash ./install
~/.local/bin/anolisa-omarchy status
```

在图形桌面的终端中，以当前用户运行，不要用 root 运行整个安装器。
保留安装包和摘要文件以便复用。源码安装入口也接受
`bash ./install --archive /absolute/path/anolisa-omarchy-0.1.3-linux-x86_64.tar.gz`，
摘要文件必须在安装包旁边。官方同版本下载地址要等维护者发布匹配 Release 后才能使用。

安装器检查摘要和运行库，用 sudo 安装 root 所有的安全服务，再注册顶栏插件，
最后输出完成状态和配置入口。`--non-interactive` 在 sudo 需要输入时直接失败；
`--no-enable` 只部署用户文件，不安装系统依赖、不配置服务，也不启用桌面插件。

## 配置与使用

点击 **ANOLISA → Start setup**，或运行 `anolisa-omarchy setup`。
每个组件都可以稍后配置，再从 **Settings → Set up cosh / Set up Qoder** 返回。

- **cosh：**输入 `1`，阅读说明，再按 Enter 打开 cosh 进行配置。
  输入 `/auth`，按 cosh 的提示配置模型服务。模型选择界面由 cosh 提供，插件不会替换。
  完成后输入 `exit` 返回向导。
- **Qoder：**继续配置。没有可用 CLI 时，选择推荐固定版本 1.1.84、Latest，或稍后设置。
  **Configure in Qoder CLI** 打开 Qoder 自己的浏览器/PAT 流程；
  **Enter your Qoder PAT here** 在插件内隐藏输入，并保存到私有配置。
  同一用户下都能复用，区别是由谁保存和提供凭据。保存成功不能证明模型请求成功。
- **语言：**默认英文，**Settings → Language** 可选择英文或简体中文。
  新开的 cosh 会话接收 `COSH_SHELL_LANG`；Qoder 使用自己的界面设置。

打开工作终端，单独输入 `/agent` 并按 Enter，再在 Agent 提示符下输入问题。
Esc 返回 shell，`exit` 关闭终端。
直接输入自然语言依赖 cosh 的 shell hook。
插件在 shell 启动前设置 `MISE_NOT_FOUND_AUTO_INSTALL=0`，避免 mise 替换缺失命令处理器；
不会修改 `~/.bashrc`。排查路由时可用 `/agent` 显式进入。

文件任务通过 Qoder 完成。插件使用 `aw run` 启动 Qoder；
AW 将原生工具事件接到 sec-core 和 Tokenless Provider。
cosh 自己的 Agent 不会自动将任务转交给该 Qoder 进程。

```bash
anolisa-omarchy help
anolisa-omarchy status
anolisa-omarchy doctor
anolisa-omarchy setup --component cosh
anolisa-omarchy setup --component qoder
anolisa-omarchy qoder-install --channel recommended
anolisa-omarchy qoder-install --channel latest
```

面板页脚的 **Help / 帮助** 提供上述命令及组件简介。
`status --json` 和 `doctor --json` 是可选的机器可读输出。

## 本地看板与边界

sec-core 显示最近最多 200 条服务扫描和风险记录。
发现风险不代表已拦截操作；没有独立拦截证据时显示 `—`。
这是 Bash 工具 hook 检查，不是 OS 沙箱。

Tokenless 显示当前用户最近最多 200 条托管记录，估算受支持工具输出的精简量。
短内容或不支持的输出可能没有节省记录。
缺少数据时显示 `—`，实际测得零时显示 `0`。这些数字不是账单节省量。

下拉窗提供入口和本地看板，任务在普通 Omarchy 终端窗口中运行。
关闭终端会结束前台会话；本版不提供后台任务恢复或完整的多任务看板。

## 升级、路径与移除

运行新包中的 `bash ./install` 升级。托管设置、凭据和统计会保留。
如果用户修改了托管文件，安装器会停止启用并输出待比对配置的位置，不会直接覆盖。

- 程序：`~/.local/share/anolisa-omarchy/releases/`，`current` 为当前版本链接。
- 命令：`~/.local/bin/anolisa-omarchy`。
- 配置：`~/.config/anolisa-omarchy/`；状态：`~/.local/state/anolisa-omarchy/`。
- 顶栏插件：`~/.config/omarchy/plugins/community.anolisa/`。
- root 所有的守护程序：`/opt/anolisa-omarchy/sec-core/`。
- 服务：`/etc/systemd/system/anolisa-omarchy-security.service`。

XDG_CONFIG_HOME、XDG_DATA_HOME 和 XDG_STATE_HOME 会覆盖 ANOLISA 对应的用户路径。
顶栏插件固定安装到 `$HOME/.config/omarchy/plugins`，与 Omarchy 的扫描路径一致。
Qoder 原生账户文件仍由 Qoder 自己管理。收集日志时保留这些目录，不能把凭据放进验收材料。

```bash
omarchy plugin remove community.anolisa
sudo systemctl disable --now anolisa-omarchy-security.service
```

上述操作移除插件、停止安全服务，保留程序和用户数据。本版没有自动彻底清理功能。
不要混用用户目录安装与 pacman 原生包安装，切换前先移除旧安装方式。

## 恢复中断的安装

如果安装因未完成的系统服务事务而停止，先确认其他安装程序已停止。
在同一份经过校验的解压安装包中运行：

```bash
bash ./install --recover
```

程序先恢复日志记录的旧服务状态，再重试安装。
源码入口也会转发 `--recover`，可与 `--archive` 同用。
不要手动删除 root 管理的事务日志。普通安装错误已会尝试回滚，不一定需要恢复操作。

安装器保存了构建时的特权脚本和守护进程摘要。
提权后，用隔离的 Python 执行经过验证、由 root 管理的脚本副本。
仍应从可信发布渠道取得安装器和摘要；这些校验无法认证在运行前已被替换的安装器。

## 验收方法

1. 在干净 Omarchy 上记录 `/etc/os-release`、`uname -m`、Python 版本、候选包 SHA-256 和完整安装记录。
   检查 `status` 和 `doctor`。
2. 打开原生顶栏插件，检查定位、外部点击关闭、首页进入 Help、设置进入 Help、中英文切换和终端焦点。
3. 配置 cosh，取得真实模型回答；退出重开，不再输入密钥，确认仍能回答。记录成功，不能复制密钥或账户文件。
4. 安装推荐 Qoder，配置账户并重开，在一次性目录运行 Bash 文件任务。
   检查结果文件、AW 原生工具事件、安全服务扫描数和受支持长输出的 Tokenless 记录。
   失败与重试都要保留。
5. 保存设置、正常关机，再启动同一虚拟机，检查服务、凭据和看板数据。
   同版本重装后再测。跨版本迁移需要单独测试，不能用同版本重装证明。
6. 移除插件并停止服务，确认已移除和停用；重装后检查预期保留的账户、设置和数据。

HTTPS 出现证书链错误时，应正确配置虚拟机信任的公司 CA，或使用经过校验的离线包。
不要关闭 TLS 或软件包签名验证。服务出错可查看 `journalctl -u anolisa-omarchy-security.service`。
报告、脱敏记录与准确的候选包摘要一起保存。
