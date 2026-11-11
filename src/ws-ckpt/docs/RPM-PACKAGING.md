# ws-ckpt RPM 打包说明

目标系统：Alinux 4（基于 RHEL/CentOS 系列）

## 快速打包

```bash
cd anolisa/src/ws-ckpt
make rpm          # 生成 SRPM 与 4 个二进制 RPM
make srpm         # 只生成 SRPM（用于 Koji 提交）
```

`bash ./build-rpm.sh` 是兼容入口，等价于 `make rpm`；`bash ./build-rpm.sh --srpm` 等价于 `make srpm`。

> **前置依赖**：`yum install -y rpm-build cargo rust gcc make pkgconfig systemd-rpm-macros nodejs npm`（要求 `rust >= 1.70`、`nodejs >= 18`）。脚本不预检这些依赖，缺失时在中途失败。

打包流程（`packaging/rpm/build.sh`）：

1. 从 `src/Cargo.toml` 读取版本号，用 `packaging/rpm/ws-ckpt.spec.in` 生成 `rpmbuild/SPECS/ws-ckpt.spec`
2. 生成源码包 `rpmbuild/SOURCES/ws-ckpt-<VERSION>.tar.gz`
3. 执行 `cargo vendor --locked`，生成依赖包 `rpmbuild/SOURCES/ws-ckpt-<VERSION>-vendor.tar.gz`
4. 调用 `rpmbuild`，在 spec 的 `%build` 阶段离线编译 Rust 二进制，并构建 OpenClaw 插件（`npm install` + `npm run build`）

构建产物：

- 二进制 RPM：`anolisa/src/ws-ckpt/rpmbuild/RPMS/x86_64/` 下的 4 个包（见下节）
- SRPM：`anolisa/src/ws-ckpt/rpmbuild/SRPMS/`

脚本结束时会打印上述文件路径，以及可直接执行的安装、升级命令。

注意：

- `cargo vendor` 需要能访问 crates.io；`%build` 中的 `npm install` 需要能访问 npm registry（Rust 部分使用 vendor 包离线编译）
- 源码包从工作目录直接复制 `src/plugins` 等目录，未被 git 跟踪的文件（如本地 `node_modules/`、`dist/`）也会被打入，正式打包建议在干净的检出中执行
- 打包过程不运行测试，也不安装 RPM、不签名
- `rpmbuild/` 中旧版本的产物不会被清理

## 安装到系统

一次构建生成 4 个 RPM 包：

| 包 | 内容 |
|---|---|
| `ws-ckpt` | 兼容元包，本身不含文件，通过精确版本依赖拉取下面三个包 |
| `ws-ckpt-core` | `ws-ckpt` 可执行文件（CLI 与 daemon）、systemd 服务、`/etc/ws-ckpt/` 配置 |
| `ws-ckpt-plugins` | OpenClaw / Hermes 插件及注册脚本（依赖 `nodejs >= 18`、`python3`） |
| `ws-ckpt-skills` | 工作区 checkpoint Skill |

```bash
yum install -y rsync btrfs-progs
cd anolisa/src/ws-ckpt/rpmbuild/RPMS/x86_64
rpm -ivh ws-ckpt-<VERSION>-1.<DIST>.x86_64.rpm \
    ws-ckpt-core-<VERSION>-1.<DIST>.x86_64.rpm \
    ws-ckpt-plugins-<VERSION>-1.<DIST>.x86_64.rpm \
    ws-ckpt-skills-<VERSION>-1.<DIST>.x86_64.rpm
```

> 将 `<VERSION>` 替换为实际版本号（如 `0.7.0`），与 `src/Cargo.toml` 的 `version` 字段一致；`<DIST>` 为构建机的 dist 标记（ALinux 4 上为 `alnx4`）。只需要 CLI 与 daemon 时，可以只安装 `ws-ckpt-core`。

安装 `ws-ckpt-core` 时会自动：

- 将 `ws-ckpt` 二进制部署到 `/usr/bin/ws-ckpt`（spec 中为 `%{_bindir}`，ALinux 4 上 `rpm --eval '%{_bindir}'` 为 `/usr/bin`）。0.7.0 之前的 RPM 装在 `/usr/local/bin/ws-ckpt`，升级后需更新引用旧路径的脚本
- 安装 systemd 服务文件到 `/usr/lib/systemd/system/ws-ckpt.service`
- 安装配置文件 `/etc/ws-ckpt/config.toml` 和示例 `/etc/ws-ckpt/config.toml.sample`。`config.toml` 标记为 `%config(noreplace)`，升级时不覆盖管理员修改过的配置，新版本默认配置有变化时另存为 `config.toml.rpmnew`
- 执行 `systemctl daemon-reload`，`enable` 服务并 `restart`：首次安装即启动 daemon，升级时重启 daemon

## 源码安装（make install）

`make install` 不经过 RPM，按 `PREFIX` 布局安装：可执行文件装到 `PREFIX/bin`，默认 `PREFIX=/usr/local`，即 `/usr/local/bin/ws-ckpt`，与 RPM 的 `/usr/bin/ws-ckpt` 不同。

```bash
cd anolisa/src/ws-ckpt
make build
make layout                        # 只打印安装布局，不写文件
sudo make install                  # 可执行文件装到 /usr/local/bin/ws-ckpt
sudo make install PREFIX=/usr      # 可执行文件装到 /usr/bin/ws-ckpt
```

`make install` 只复制已构建的文件，不执行 `systemctl daemon-reload`、`enable` 或启动服务，也不注册 Agent 插件。

## 验证安装

```bash
# 检查服务状态
systemctl status ws-ckpt

# 确认可执行文件位置（RPM 安装为 /usr/bin/ws-ckpt）
command -v ws-ckpt

# 查看帮助
ws-ckpt --help

# 检查已安装的 RPM 信息
rpm -qi ws-ckpt-core
```

## 卸载

```bash
rpm -e ws-ckpt ws-ckpt-plugins ws-ckpt-skills ws-ckpt-core
```

二进制与服务由 `ws-ckpt-core` 提供，只执行 `rpm -e ws-ckpt` 仅删除元包，服务仍在运行。其他三个包都依赖 `ws-ckpt-core`，需要一起卸载；只安装了 core 时执行 `rpm -e ws-ckpt-core`。

卸载 `ws-ckpt-core` 时会自动：

- 先执行 `ws-ckpt recover --all --force` 恢复所有工作区，再停止并禁用 systemd 服务
- 卸载 BtrfsLoop 挂载、解除 loop 设备，并清理 `/var/lib/ws-ckpt`；recover 失败时保留 `/var/lib/ws-ckpt`（及 `/data/ws-ckpt`）供人工检查
- 修改过的 `/etc/ws-ckpt/config.toml` 另存为 `config.toml.rpmsave`

## 相关文件

```
packaging/rpm/ws-ckpt.spec.in           # RPM spec 模板
packaging/rpm/build.sh                  # 一键打包脚本（build-rpm.sh 为其兼容入口）
packaging/systemd/ws-ckpt.service.in    # systemd 服务文件模板
docs/RPM-PACKAGING.md                   # 本说明文档
```
