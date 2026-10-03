#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
#
# docker_prune_plan.sh — prune 前的只读盘点（先列后删）。
#
# docker system prune 没有 --dry-run，删除不可撤销。本脚本只列出各条
# prune 命令将会删除的对象，并给出建议的执行命令，不执行任何删除。
# 供 docker-admin 技能在执行任何清理操作前生成"影响面清单"。
#
# 用法:
#   sh docker_prune_plan.sh [until-filter]
#
# 参数:
#   until-filter — 可选，时间窗过滤（如 72h、48h），仅用于生成建议命令
#
# 退出码:
#   0 — 正常输出（即使没有任何可清理对象）
#   1 — docker 命令不存在，或当前用户无权访问 daemon

set -eu

UNTIL_FILTER="${1:-}"
if [ "$#" -gt 1 ]; then
    echo "usage: $0 [until-filter]   # e.g. $0 72h" >&2
    exit 1
fi

if ! command -v docker >/dev/null 2>&1; then
    echo "ERROR: docker command not found (PATH=$PATH)" >&2
    exit 1
fi

if ! docker info >/dev/null 2>&1; then
    echo "ERROR: cannot talk to the docker daemon (is it running? are you in the 'docker' group?)" >&2
    docker info 2>&1 | head -3 >&2 || true
    exit 1
fi

until_args=""
if [ -n "${UNTIL_FILTER}" ]; then
    until_args="--filter until=${UNTIL_FILTER}"
    echo "# suggested commands below are limited to objects older than: ${UNTIL_FILTER}"
    echo
fi

count_lines() {
    # count_lines <text> — 数一下非空行数（0 表示该类无对象）
    printf '%s\n' "$1" | grep -c . || true
}

# 每类对象只取一次快照，保证展示清单与末尾统计来自同一时刻。
echo "docker prune plan (READ-ONLY — nothing is deleted by this script)"
echo

echo "=== Stopped containers — docker container prune would remove ==="
stopped_list="$(docker container ls -a --filter status=exited --filter status=created \
    --format '{{.ID}}  {{.Names}}  {{.Image}}  {{.RunningFor}}' 2>/dev/null || true)"
if [ -n "${stopped_list}" ]; then
    printf '%s\n' "${stopped_list}"
else
    echo "(none)"
fi
echo

echo "=== Dangling images — docker image prune would remove ==="
dangling_list="$(docker images --filter dangling=true \
    --format '{{.ID}}  {{.Repository}}:{{.Tag}}  {{.Size}}' 2>/dev/null || true)"
if [ -n "${dangling_list}" ]; then
    printf '%s\n' "${dangling_list}"
else
    echo "(none)"
fi
echo

echo "=== Build cache usage — docker builder prune would reclaim ==="
docker builder du 2>/dev/null || echo "(builder stats unavailable)"
echo

echo "=== Custom networks — docker network prune would remove ==="
docker network ls --filter type=custom --format '{{.Name}}  {{.Driver}}' \
    || echo "(listing failed)"
echo

echo "=== Volumes — PROTECTED: this plan never auto-prunes volumes ==="
docker volume ls --format '{{.Name}}' 2>/dev/null || echo "(listing failed)"
echo "Named volumes may hold persistent data; delete only by explicit name"
echo "after confirming with the user which ones are disposable."
echo

echo "=== Suggested commands (run ONE category at a time, re-check between) ==="
echo "  docker system df                        # baseline"
# until_args 为空或为单个 --filter until=... 词；echo 场景无需引号
# shellcheck disable=SC2086
echo "  docker container prune ${until_args}"
# shellcheck disable=SC2086
echo "  docker image prune ${until_args}"
# shellcheck disable=SC2086
echo "  docker builder prune ${until_args}"
echo "  docker system df                        # compare with baseline"
echo
echo "WARNING: 'docker system prune --volumes' and 'docker volume prune'"
echo "         delete data volumes — only with explicit per-volume confirmation."

stopped="$(count_lines "${stopped_list}")"
dangling="$(count_lines "${dangling_list}")"
echo
echo "Summary: ${stopped} stopped container(s), ${dangling} dangling image(s) listed."
echo "Review the lists above with the user before running any prune command."
