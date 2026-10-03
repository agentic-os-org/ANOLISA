#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
#
# docker_df_summary.sh — Docker 磁盘占用速查（只读）。
#
# 对照展示 Docker 各类对象的磁盘占用与宿主机文件系统剩余空间，
# 用于磁盘告急时的第一步诊断。本脚本不执行任何删除或写操作。
#
# 用法:
#   sh docker_df_summary.sh
#
# 退出码:
#   0 — 正常输出
#   1 — docker 命令不存在，或当前用户无权访问 daemon

set -eu

if ! command -v docker >/dev/null 2>&1; then
    echo "ERROR: docker command not found (PATH=$PATH)" >&2
    exit 1
fi

if ! docker info >/dev/null 2>&1; then
    echo "ERROR: cannot talk to the docker daemon (is it running? are you in the 'docker' group?)" >&2
    docker info 2>&1 | head -3 >&2 || true
    exit 1
fi

docker_root="$(docker info --format '{{.DockerRootDir}}' 2>/dev/null || echo /var/lib/docker)"

echo "=== Host filesystem (docker root: ${docker_root}) ==="
# DockerRootDir 可能位于 VM 内（Docker Desktop 等），宿主机上不存在；
# 此时退回根文件系统展示整体余量。
df_target="${docker_root}"
if [ ! -d "${docker_root}" ]; then
    echo "(docker root ${docker_root} not present on this host; showing / instead)"
    df_target="/"
fi
df -h "${df_target}" || true

echo
echo "=== Docker object usage (docker system df) ==="
docker system df || true

echo
echo "=== Per-object detail (docker system df -v, trimmed) ==="
docker system df -v || true

echo
echo "NOTE: read-only summary. Before deleting anything run docker_prune_plan.sh"
echo "      and confirm the affected objects with the user."
