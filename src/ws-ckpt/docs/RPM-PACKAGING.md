# ws-ckpt RPM 打包说明

目标系统：Alinux 4（基于 RHEL/CentOS 系列）

## 快速打包

```bash
cd anolisa/src/ws-ckpt
bash ./build-rpm.sh
```

脚本会自动完成：编译 release 二进制 → 准备 RPM 构建目录 → 调用 rpmbuild 生成 RPM 包。

构建完成后 RPM 包会放到 `anolisa/src/ws-ckpt/rpmbuild/RPMS` 目录下。

> **前置依赖**：需要安装 `rpm-build` 包：`yum install -y rpm-build`

## 安装到系统

```bash
rpm -ivh ws-ckpt-<VERSION>-1.x86_64.rpm
```

> 将 `<VERSION>` 替换为实际版本号(如 `0.3.0`),与 `src/Cargo.toml` 的 `version` 字段一致。

安装过程会自动：

- 将 `ws-ckpt` 二进制部署到 `/usr/local/bin/`
- 安装 systemd 服务文件到 `/usr/lib/systemd/system/`
- 执行 `systemctl daemon-reload` 并 `enable` 服务

## 验证安装

```bash
# 检查服务状态
systemctl status ws-ckpt

# 查看帮助
ws-ckpt --help

# 检查已安装的 RPM 信息
rpm -qi ws-ckpt
```

## 卸载

```bash
rpm -e ws-ckpt
```

卸载时会自动停止并禁用 systemd 服务。

## 相关文件

```
ws-ckpt.spec               # RPM spec 文件（项目根目录）
build-rpm.sh                # 一键打包脚本（项目根目录）
src/ws-ckpt/systemd/       # systemd 服务文件
docs/RPM-PACKAGING.md       # 本说明文档
```
