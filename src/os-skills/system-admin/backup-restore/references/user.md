# 用户备份与恢复

适用范围：家目录、dotfiles、应用设置、SSH/GPG 密钥、个人数据。

---

## 常见备份目标

| 类别 | 典型路径 |
|---|---|
| 家目录 | `/home/用户名` |
| Shell 配置 | `~/.bashrc`、`~/.bash_profile`、`~/.profile` |
| SSH 密钥 | `~/.ssh/` |
| GPG 密钥 | `~/.gnupg/` |
| 应用配置 | `~/.config/`、`~/.local/share/` |
| Vim 配置 | `~/.vimrc`、`~/.vim/` |
| Git 配置 | `~/.gitconfig` |
| tmux 配置 | `~/.tmux.conf` |

---

## 安装必要工具

```bash
sudo yum install -y rsync gnupg2
```

---

## 备份方法

### 方法一：完整家目录归档

```bash
TIMESTAMP=$(date +%Y%m%d_%H%M%S)
DEST="/backups/user"
mkdir -p "$DEST"

# 排除缓存、回收站、下载目录（按需调整）
tar -czf "${DEST}/home_${USER}_${TIMESTAMP}.tar.gz" \
    --exclude="$HOME/.cache" \
    --exclude="$HOME/.local/share/Trash" \
    --exclude="$HOME/Downloads" \
    "$HOME"

echo "用户备份完成：${DEST}/home_${USER}_${TIMESTAMP}.tar.gz"
echo "文件大小：$(du -sh "${DEST}/home_${USER}_${TIMESTAMP}.tar.gz" | cut -f1)"
```

### 方法二：仅备份 dotfiles（轻量）

适合：快速保存 Shell 配置和工具设置

```bash
TIMESTAMP=$(date +%Y%m%d_%H%M%S)
DEST="/backups/dotfiles"
mkdir -p "$DEST"

# --ignore-failed-read 会忽略不存在的文件
tar -czf "${DEST}/dotfiles_${TIMESTAMP}.tar.gz" \
    --ignore-failed-read \
    ~/.bashrc \
    ~/.bash_profile \
    ~/.profile \
    ~/.vimrc \
    ~/.vim/ \
    ~/.tmux.conf \
    ~/.gitconfig \
    ~/.ssh/config \
    ~/.config/htop/ \
    2>/dev/null

echo "dotfiles 备份完成：${DEST}/dotfiles_${TIMESTAMP}.tar.gz"
```

### 方法三：SSH 密钥备份

```bash
DEST="/backups/ssh"
mkdir -p "$DEST"

BACKUP_DIR="${DEST}/ssh_$(date +%Y%m%d_%H%M%S)"

# 复制整个 .ssh 目录，保留权限
cp -rp ~/.ssh "$BACKUP_DIR"

# SSH 要求严格的文件权限，确保备份也保持正确
chmod 700 "$BACKUP_DIR"
chmod 600 "$BACKUP_DIR"/* 2>/dev/null
chmod 644 "$BACKUP_DIR"/*.pub 2>/dev/null

echo "SSH 密钥已备份到 $BACKUP_DIR"
```

### 方法四：GPG 密钥导出

```bash
DEST="/backups/gpg"
mkdir -p "$DEST"

DATESTAMP=$(date +%Y%m%d)

# 导出公钥
gpg --export --armor > "${DEST}/gpg_public_${DATESTAMP}.asc"

# 导出私钥到临时文件
gpg --export-secret-keys --armor > "${DEST}/gpg_private_${DATESTAMP}.asc"

# 立即对私钥文件加密，然后删除明文版本
gpg --symmetric --cipher-algo AES256 "${DEST}/gpg_private_${DATESTAMP}.asc"
rm "${DEST}/gpg_private_${DATESTAMP}.asc"

echo "GPG 密钥已导出，私钥已加密保存"
```

### 方法五：rsync 同步到远程服务器

```bash
# 首次运行建议加 --dry-run（-n）预览，不实际执行
rsync -avzn \
    --exclude='.cache' \
    --exclude='.local/share/Trash' \
    "$HOME/" \
    backup_user@192.168.1.100:/backup/home/$(hostname)/

# 确认无误后去掉 -n 执行同步
rsync -avz \
    --exclude='.cache' \
    --exclude='.local/share/Trash' \
    "$HOME/" \
    backup_user@192.168.1.100:/backup/home/$(hostname)/
```

---

## 软件包列表备份

重装系统后可一键恢复所有已安装软件：

```bash
# 导出当前已安装的软件包列表
yum list installed > /backups/user/packages_$(date +%Y%m%d).txt

# 仅导出手动安装的包（不含自动依赖，更精简）
yum history userinstalled > /backups/user/packages_manual_$(date +%Y%m%d).txt
```

恢复软件包：

```bash
# 从手动安装列表批量安装
awk 'NR>1 {print $1}' /backups/user/packages_manual_20240315.txt \
    | xargs sudo yum install -y
```

---

## 恢复

### 从完整家目录归档恢复

```bash
BACKUP="/backups/user/home_alice_20240315_143022.tar.gz"

# 恢复前预览内容，确认无误
tar -tzf "$BACKUP" | head -30

# 恢复到原始路径（会覆盖现有文件）
tar -xzf "$BACKUP" -C /

# 或恢复到临时目录供选择性恢复
tar -xzf "$BACKUP" -C /tmp/restore_preview/
```

### 从 dotfiles 备份恢复

```bash
tar -xzf /backups/dotfiles/dotfiles_20240315.tar.gz -C ~

# 重新加载 Shell 配置
source ~/.bashrc
echo "配置已重新加载"
```

### SSH 密钥恢复

```bash
BACKUP_DIR="/backups/ssh/ssh_20240315"

cp -rp "$BACKUP_DIR" ~/.ssh

# SSH 对权限要求严格，必须设置正确否则会拒绝使用密钥
chmod 700 ~/.ssh
chmod 600 ~/.ssh/id_* ~/.ssh/authorized_keys 2>/dev/null
chmod 644 ~/.ssh/*.pub 2>/dev/null
chmod 600 ~/.ssh/config 2>/dev/null

# 验证密钥
ssh-add -l
```

### GPG 密钥恢复

```bash
# 如果私钥已加密，先解密
gpg --output gpg_private.asc --decrypt /backups/gpg/gpg_private_20240315.asc.gpg

# 导入公钥和私钥
gpg --import /backups/gpg/gpg_public_20240315.asc
gpg --import gpg_private.asc

# 验证导入结果
gpg --list-secret-keys

# 清理临时解密文件
rm -f gpg_private.asc
```

---

## 自动化用户备份脚本

```bash
#!/bin/bash
set -euo pipefail
trap 'echo "[FAIL] 用户备份在第 $LINENO 行失败"; exit 1' ERR

DEST="/backups/user"
KEEP_DAYS=30
TIMESTAMP=$(date +%Y%m%d_%H%M%S)

mkdir -p "$DEST"

tar -czf "${DEST}/home_${USER}_${TIMESTAMP}.tar.gz" \
    --exclude="$HOME/.cache" \
    --exclude="$HOME/.local/share/Trash" \
    --exclude="$HOME/Downloads" \
    "$HOME"

# 验证备份
tar -tzf "${DEST}/home_${USER}_${TIMESTAMP}.tar.gz" > /dev/null \
    && echo "[OK] 用户备份成功：$(du -sh "${DEST}/home_${USER}_${TIMESTAMP}.tar.gz" | cut -f1)" \
    || echo "[FAIL] 备份失败"

# 清理过期备份
find "$DEST" -name "home_*.tar.gz" -mtime +"${KEEP_DAYS}" -delete
echo "已清理 ${KEEP_DAYS} 天前的旧备份"
```

加入定时任务：

```bash
crontab -e
# 每天凌晨 1 点执行用户备份
0 1 * * * /path/to/user_backup.sh >> /var/log/user_backup.log 2>&1
```

---

## 安全提示

- SSH 私钥和 GPG 私钥**只能存放在加密存储中**，绝不能明文上传到云端
- 恢复密钥文件后立即用 `chmod 600` 设置正确权限
- 建议对备份文件所在目录设置访问限制：`chmod 700 /backups/user`
