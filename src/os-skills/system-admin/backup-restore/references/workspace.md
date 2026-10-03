# 工作区备份与恢复

适用范围：项目目录、源代码、数据库、工作环境内的配置文件。

---

## 安装必要工具

```bash
sudo yum install -y rsync mysql git
```

---

## 备份方法

### 方法一：压缩归档（tar + gzip）

适合：一次性快照、方便传输

```bash
#!/bin/bash
set -euo pipefail

TIMESTAMP=$(date +%Y%m%d_%H%M%S)
SOURCE="/path/to/project"
DEST="/path/to/backups"

mkdir -p "$DEST"

# 排除依赖目录(node_modules/vendor)、缓存(__pycache__)、日志、Git 历史
tar -czf "${DEST}/workspace_${TIMESTAMP}.tar.gz" \
    --exclude='node_modules' \
    --exclude='vendor' \
    --exclude='__pycache__' \
    --exclude='*.log' \
    --exclude='.git' \
    -C "$(dirname "$SOURCE")" "$(basename "$SOURCE")"

echo "备份完成：${DEST}/workspace_${TIMESTAMP}.tar.gz"
echo "文件大小：$(du -sh "${DEST}/workspace_${TIMESTAMP}.tar.gz" | cut -f1)"
```

### 方法二：rsync 增量同步

适合：大型项目、频繁备份、只同步变更文件

```bash
# 增量备份：将变更文件保存到以日期命名的子目录中
rsync -av --delete \
    --exclude='node_modules' \
    --exclude='__pycache__' \
    --backup \
    --backup-dir="/backups/incremental/$(date +%Y%m%d)" \
    /path/to/project/ /backups/current/
```

### 方法三：Git Bundle（代码仓库）

适合：将完整 Git 历史打包为单个可移植文件

```bash
cd /path/to/repo

BUNDLE_FILE="/backups/repo_$(date +%Y%m%d_%H%M%S).bundle"

# 创建包含所有分支和标签的 bundle 文件
git bundle create "$BUNDLE_FILE" --all

# 验证 bundle 文件是否完整
git bundle verify "$BUNDLE_FILE"
```

---

## 数据库备份

### MySQL / MariaDB

```bash
DB_NAME="myapp"
DB_USER="root"
DEST="/backups/db"
mkdir -p "$DEST"

# 备份单个数据库
mysqldump -u "$DB_USER" -p "$DB_NAME" \
    | gzip > "${DEST}/mysql_${DB_NAME}_$(date +%Y%m%d_%H%M%S).sql.gz"

# 备份所有数据库
mysqldump -u root -p --all-databases \
    | gzip > "${DEST}/mysql_all_$(date +%Y%m%d_%H%M%S).sql.gz"
```

### SQLite

```bash
# 使用 SQLite 内置备份命令（比直接复制文件更安全，不会读到半写状态）
sqlite3 /path/to/database.db \
    ".backup '/backups/db/sqlite_$(date +%Y%m%d_%H%M%S).db'"
```

### PostgreSQL

```bash
DEST="/backups/db"
mkdir -p "$DEST"

# 备份单个数据库
pg_dump -U postgres myapp \
    | gzip > "${DEST}/pg_myapp_$(date +%Y%m%d_%H%M%S).sql.gz"

# 备份所有数据库
pg_dumpall -U postgres \
    | gzip > "${DEST}/pg_all_$(date +%Y%m%d_%H%M%S).sql.gz"
```

---

## 恢复

### 从 tar 归档恢复

```bash
BACKUP="/backups/workspace_20240315_143022.tar.gz"
RESTORE_TO="/path/to/restore"

# 第一步：预览归档内容，确认文件无误
tar -tzf "$BACKUP" | head -20

# 第二步：创建恢复目录并解压
mkdir -p "$RESTORE_TO"
tar -xzf "$BACKUP" -C "$RESTORE_TO"

echo "已恢复到：$RESTORE_TO"
```

### 从 rsync 备份恢复

```bash
# 从最新同步恢复
rsync -av /backups/current/ /path/to/restore/

# 从指定日期的增量备份恢复
rsync -av /backups/incremental/20240315/ /path/to/restore/
```

### 从 Git Bundle 恢复

```bash
# 从 bundle 克隆仓库
git clone /backups/repo_20240315.bundle /path/to/restored-repo

# 修复远程地址（bundle 克隆后 origin 指向 bundle 文件）
cd /path/to/restored-repo
git remote set-url origin <原始远程地址>
```

### MySQL / MariaDB 恢复

```bash
# 恢复前建议先备份当前数据库（避免误操作）
mysqldump -u root -p "$DB_NAME" | gzip > /tmp/pre_restore_backup.sql.gz

# 从压缩备份恢复
gunzip -c /backups/db/mysql_myapp_20240315.sql.gz \
    | mysql -u root -p "$DB_NAME"
```

### PostgreSQL 恢复

```bash
# 恢复单个数据库
gunzip -c /backups/db/pg_myapp_20240315.sql.gz \
    | psql -U postgres myapp

# 恢复所有数据库
gunzip -c /backups/db/pg_all_20240315.sql.gz \
    | psql -U postgres
```

---

## 验证备份

```bash
# 验证 tar 归档完整性
tar -tzf backup.tar.gz > /dev/null \
    && echo "[OK] 归档文件正常" \
    || echo "[FAIL] 归档文件损坏"

# 查看 SQL 备份内容（前几行）
zcat backup.sql.gz | head -10

# 列出所有备份并按大小排序
du -sh /backups/workspace_*.tar.gz | sort -h
```

---

## 自动化备份脚本

```bash
#!/bin/bash
set -euo pipefail
trap 'echo "[FAIL] 备份在第 $LINENO 行失败"; exit 1' ERR

SOURCE="/path/to/project"
DEST="/backups/workspace"
KEEP_DAYS=14

mkdir -p "$DEST"
TIMESTAMP=$(date +%Y%m%d_%H%M%S)
BACKUP_FILE="${DEST}/workspace_${TIMESTAMP}.tar.gz"

# 检查目标空间
AVAIL_KB=$(df --output=avail "$DEST" | tail -1)
SOURCE_KB=$(du -sk "$SOURCE" | cut -f1)
if [ "$AVAIL_KB" -lt "$((SOURCE_KB * 2))" ]; then
    echo "[WARN] 目标空间可能不足：可用 $((AVAIL_KB/1024))MB，源数据 $((SOURCE_KB/1024))MB"
fi

# 执行备份
tar -czf "$BACKUP_FILE" \
    --exclude='node_modules' \
    --exclude='__pycache__' \
    --exclude='*.log' \
    -C "$(dirname "$SOURCE")" "$(basename "$SOURCE")"

# 验证备份文件
tar -tzf "$BACKUP_FILE" > /dev/null \
    && echo "[OK] 备份成功：$BACKUP_FILE ($(du -sh "$BACKUP_FILE" | cut -f1))" \
    || echo "[FAIL] 备份失败，请检查日志"

# 清理超过保留期的旧备份
find "$DEST" -name "workspace_*.tar.gz" -mtime +"${KEEP_DAYS}" -delete
echo "已清理 ${KEEP_DAYS} 天前的旧备份"
```

加入定时任务：

```bash
crontab -e
# 每天凌晨 2 点执行备份
0 2 * * * /path/to/workspace_backup.sh >> /var/log/workspace_backup.log 2>&1
```
