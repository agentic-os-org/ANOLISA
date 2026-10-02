# Bash 脚本编写指南

本文档提供 ALinux 4 环境下编写健壮 bash 脚本的详细指南。

---

## 安全三要素

始终在每个非平凡脚本的顶部使用：

```bash
#!/usr/bin/env bash
set -euo pipefail
# -e: 出错时退出
# -u: 将未设置变量视为错误
# -o pipefail: 管道中任何命令失败则管道失败
```

---

## 退出时清理

使用 `trap` 确保在成功、错误和信号时都能清理临时资源：

```bash
TMPDIR_WORK=""
cleanup() {
    local exit_code=$?
    [[ -n "$TMPDIR_WORK" ]] && rm -rf "$TMPDIR_WORK"
    exit "$exit_code"
}
trap cleanup EXIT INT TERM

# 使用 mktemp 创建安全的临时目录
TMPDIR_WORK=$(mktemp -d)
```

---

## 参数解析

带默认值和验证的参数解析模板：

```bash
usage() {
    echo "用法: $0 [-e 环境] [-d] <目标>"
    echo "  -e 环境   环境(默认: staging)"
    echo "  -d       试运行模式"
    exit 1
}

ENV="staging"
DRY_RUN=false

while getopts ":e:dh" opt; do
    case $opt in
        e) ENV="$OPTARG" ;;
        d) DRY_RUN=true ;;
        h) usage ;;
        :) echo "选项 -$OPTARG 需要参数。" >&2; usage ;;
        \?) echo "未知选项: -$OPTARG" >&2; usage ;;
    esac
done
shift $((OPTIND - 1))

[[ $# -lt 1 ]] && { echo "错误: 需要目标" >&2; usage; }
TARGET="$1"
```

---

## 日志记录

带时间戳的日志函数：

```bash
log() { echo "[$(date '+%Y-%m-%dT%H:%M:%S')] $*"; }
log "开始部署: env=$ENV target=$TARGET"
```

---

## 试运行模式

包装器函数实现试运行：

```bash
run() {
    if [[ "$DRY_RUN" == true ]]; then
        echo "[试运行] $*"
    else
        "$@"
    fi
}

run rsync -av --exclude='.git' "./" "deploy@${TARGET}:/opt/app/"
```

---

## 完整脚本模板

```bash
#!/usr/bin/env bash
set -euo pipefail

# 退出时清理
TMPDIR_WORK=""
cleanup() {
    local exit_code=$?
    [[ -n "$TMPDIR_WORK" ]] && rm -rf "$TMPDIR_WORK"
    exit "$exit_code"
}
trap cleanup EXIT INT TERM

# 参数解析
usage() {
    echo "用法: $0 [-e 环境] [-d] <目标>"
    echo "  -e 环境   环境(默认: staging)"
    echo "  -d       试运行模式"
    exit 1
}

ENV="staging"
DRY_RUN=false

while getopts ":e:dh" opt; do
    case $opt in
        e) ENV="$OPTARG" ;;
        d) DRY_RUN=true ;;
        h) usage ;;
        :) echo "选项 -$OPTARG 需要参数。" >&2; usage ;;
        \?) echo "未知选项: -$OPTARG" >&2; usage ;;
    esac
done
shift $((OPTIND - 1))

[[ $# -lt 1 ]] && { echo "错误: 需要目标" >&2; usage; }
TARGET="$1"

# 创建临时目录
TMPDIR_WORK=$(mktemp -d)

# 日志函数
log() { echo "[$(date '+%Y-%m-%dT%H:%M:%S')] $*"; }

# 试运行包装器
run() {
    if [[ "$DRY_RUN" == true ]]; then
        echo "[试运行] $*"
    else
        "$@"
    fi
}

# 主逻辑
log "开始部署: env=$ENV target=$TARGET dry_run=$DRY_RUN"
run rsync -av --exclude='.git' "./" "deploy@${TARGET}:/opt/app/"
log "部署完成"
```

---

## 常见陷阱与解决

| 问题 | 原因 | 解决方案 |
|------|------|----------|
| 脚本意外退出且无错误消息 | `set -e` 被返回非零的命令触发 | 添加 `\|\|` 逻辑或使用 `set +e` 临时禁用 |
| 变量未定义导致退出 | `set -u` 检测到未设置变量 | 使用 `${VAR:-default}` 提供默认值 |
| 管道中某命令失败被忽略 | 未使用 `pipefail` | 确保 `set -o pipefail` |
| 空格导致参数拆分 | 变量未加引号 | 始终使用 `"$VAR"` 而非 `$VAR` |
