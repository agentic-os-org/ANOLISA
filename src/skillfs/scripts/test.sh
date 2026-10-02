#!/usr/bin/env bash

set -euo pipefail

PROFILE="debug"
CARGO_FLAGS=""
for arg in "$@"; do
	case "$arg" in
		--release)
			PROFILE="release"
			CARGO_FLAGS="--release"
			;;
		*)
			echo "Unknown argument: $arg" >&2
			exit 1
			;;
	esac
done

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$REPO_ROOT/target/$PROFILE/skillfs"
TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/skillfs-e2e.XXXXXX")"
SOURCE_DIR="$TMP_ROOT/source"
MOUNT_DIR="$TMP_ROOT/mount"
PID_FILE="$TMP_ROOT/skillfs.pid"
LOG_FILE="$TMP_ROOT/skillfs.log"
MOUNT_PID=""
MANAGED_MOUNT_DIR=""
FAIL_XDG=""
FAIL_MOUNT_DIR=""

info() {
	echo "[skillfs-e2e] $1"
}

pass() {
	echo "[pass] $1"
}

fail() {
	echo "[fail] $1" >&2
	if [[ -f "$LOG_FILE" ]]; then
		echo "--- skillfs log ---" >&2
		cat "$LOG_FILE" >&2 || true
	fi
	exit 1
}

cleanup() {
	set +e
	if [[ -n "$FAIL_MOUNT_DIR" ]]; then
		if [[ -n "$FAIL_XDG" ]]; then
			XDG_RUNTIME_DIR="$FAIL_XDG" "$BIN" stop "$FAIL_MOUNT_DIR" >/dev/null 2>&1 || true
		fi
		force_unmount "$FAIL_MOUNT_DIR" || true
	fi
	if [[ -n "$MOUNT_PID" ]] && kill -0 "$MOUNT_PID" 2>/dev/null; then
		kill "$MOUNT_PID" 2>/dev/null || true
		wait "$MOUNT_PID" 2>/dev/null || true
	fi
	if [[ -n "$MANAGED_MOUNT_DIR" ]]; then
		"$BIN" stop "$MANAGED_MOUNT_DIR" >/dev/null 2>&1 || true
		force_unmount "$MANAGED_MOUNT_DIR" || true
	fi
	force_unmount "$MOUNT_DIR" || true
	cleanup_mounts_under_tmp
	rm -rf "$TMP_ROOT"
}
trap cleanup EXIT

is_mounted() {
	local mountpoint="$1"
	grep -Fq " $mountpoint " /proc/mounts 2>/dev/null
}

force_unmount() {
	local mountpoint="$1"

	for _ in $(seq 1 50); do
		if ! is_mounted "$mountpoint"; then
			return 0
		fi
		fusermount3 -u "$mountpoint" >/dev/null 2>&1 \
			|| fusermount3 -u -z "$mountpoint" >/dev/null 2>&1 \
			|| umount -l "$mountpoint" >/dev/null 2>&1 \
			|| true
		sleep 0.1
	done
	return 1
}

cleanup_mounts_under_tmp() {
	awk -v root="$TMP_ROOT" '$2 == root || index($2, root "/") == 1 { print $2 }' /proc/mounts 2>/dev/null \
		| sort -r \
		| while read -r mountpoint; do
			[[ -n "$mountpoint" ]] || continue
			force_unmount "$mountpoint" || true
		done
}

assert_contains() {
	local haystack="$1"
	local needle="$2"
	local label="$3"

	if grep -Fq "$needle" <<<"$haystack"; then
		pass "$label"
	else
		fail "$label: missing '$needle'"
	fi
}

assert_not_contains() {
	local haystack="$1"
	local needle="$2"
	local label="$3"

	if grep -Fq "$needle" <<<"$haystack"; then
		fail "$label: unexpected '$needle'"
	else
		pass "$label"
	fi
}

assert_equals() {
	local actual="$1"
	local expected="$2"
	local label="$3"

	if [[ "$actual" == "$expected" ]]; then
		pass "$label"
	else
		fail "$label: expected '$expected', got '$actual'"
	fi
}

wait_for_mount_state() {
	local expected="$1"
	local attempts="${2:-50}"

	for _ in $(seq 1 "$attempts"); do
		if grep -Fq " $MOUNT_DIR " /proc/mounts 2>/dev/null; then
			[[ "$expected" == "mounted" ]] && return 0
		else
			[[ "$expected" == "unmounted" ]] && return 0
		fi
		sleep 0.1
	done
	return 1
}

if ! command -v fusermount3 >/dev/null 2>&1; then
	info "fusermount3 不存在，跳过端到端挂载测试"
	exit 0
fi

if [[ ! -e /dev/fuse ]]; then
	info "/dev/fuse 不存在，跳过端到端挂载测试"
	exit 0
fi

info "构建 skillfs ($PROFILE)"
cargo build $CARGO_FLAGS --bin skillfs --manifest-path "$REPO_ROOT/Cargo.toml" >/dev/null
[[ -x "$BIN" ]] || fail "二进制不存在: $BIN"

info "构造端到端测试数据"
mkdir -p "$SOURCE_DIR/primary-skill/assets" "$SOURCE_DIR/secondary-skill" "$SOURCE_DIR/tertiary-skill" "$MOUNT_DIR"

cat > "$SOURCE_DIR/primary-skill/SKILL.md" <<'EOF'
---
name: primary-skill
description: Primary skill exposed in the default view.
version: 1.0.0
tags: [primary]
enabled: true
---

# Primary Skill

This skill should appear directly under /skills.
EOF

cat > "$SOURCE_DIR/secondary-skill/SKILL.md" <<'EOF'
---
name: secondary-skill
description: Secondary skill only visible through skill-discover.
version: 1.0.0
tags: [secondary]
enabled: true
---

# Secondary Skill

This skill should only be referenced by skill-discover.
EOF

cat > "$SOURCE_DIR/tertiary-skill/SKILL.md" <<'EOF'
---
name: tertiary-skill
description: Another skill in the secondary view.
version: 1.0.0
tags: [secondary]
enabled: true
---

# Tertiary Skill

This skill also lives in the secondary view.
EOF

cat > "$SOURCE_DIR/skillfs-views.toml" <<'EOF'
[[view]]
name = "major"
default = true
description = "Skills mounted by default"
skills = ["primary-skill"]

[[view]]
name = "other"
default = false
description = "Skills listed via skill-discover"
skills = ["secondary-skill", "tertiary-skill"]
EOF

printf 'passthrough-ok\n' > "$SOURCE_DIR/primary-skill/assets/info.txt"

info "启动 FUSE 挂载"
"$BIN" mount "$SOURCE_DIR" "$MOUNT_DIR" \
	--foreground \
	--skill-discover-root "$MOUNT_DIR/skills" \
	--pid-file "$PID_FILE" \
	--log-file "$LOG_FILE" \
	>/dev/null 2>&1 &
MOUNT_PID=$!

if ! wait_for_mount_state mounted; then
	fail "挂载超时"
fi
pass "FUSE 挂载成功"

ROOT_LIST="$(ls -1 "$MOUNT_DIR")"
assert_contains "$ROOT_LIST" "skills" "根目录暴露 skills"

SKILLS_LIST="$(ls -1 "$MOUNT_DIR/skills")"
assert_contains "$SKILLS_LIST" "primary-skill" "默认视图技能可见"
assert_contains "$SKILLS_LIST" "skill-discover" "skill-discover 始终可见"
assert_not_contains "$SKILLS_LIST" "secondary-skill" "secondary 技能不直接出现在 /skills"

PRIMARY_MD="$(cat "$MOUNT_DIR/skills/primary-skill/SKILL.md")"
assert_contains "$PRIMARY_MD" "name: primary-skill" "可读取 primary SKILL.md"

PASSTHROUGH_CONTENT="$(cat "$MOUNT_DIR/skills/primary-skill/assets/info.txt")"
assert_equals "$PASSTHROUGH_CONTENT" "passthrough-ok" "物理文件透传正确"

DISCOVER_MD="$(cat "$MOUNT_DIR/skills/skill-discover/SKILL.md")"
assert_contains "$DISCOVER_MD" "## other" "discover 包含 secondary view 章节"
assert_contains "$DISCOVER_MD" "secondary-skill" "discover 列出隐藏技能"
assert_contains "$DISCOVER_MD" "tertiary-skill" "discover 列出全部 secondary 技能"
assert_contains "$DISCOVER_MD" "| name | description | source_path |" "discover 暴露 source_path 列"
DISCOVERED_SECONDARY="$MOUNT_DIR/skills/secondary-skill/SKILL.md"
assert_contains "$DISCOVER_MD" "$DISCOVERED_SECONDARY" "discover 输出可读挂载路径"
SECONDARY_MD="$(cat "$DISCOVERED_SECONDARY")"
assert_contains "$SECONDARY_MD" "name: secondary-skill" "discover 路径可直接读取隐藏技能"

info "发送 SIGTERM 触发卸载"
kill -TERM "$(cat "$PID_FILE")" >/dev/null 2>&1 || kill -TERM "$MOUNT_PID" >/dev/null 2>&1 || true
wait "$MOUNT_PID" 2>/dev/null || true
MOUNT_PID=""

if ! wait_for_mount_state unmounted; then
	fail "卸载超时"
fi
pass "FUSE 已正确卸载"

# ---------------------------------------------------------------------------
# Managed mount supervisor smoke test
# ---------------------------------------------------------------------------

info "测试 managed mount 监督器"

# Isolate managed runtime state in a dedicated XDG_RUNTIME_DIR so the pid /
# state files for this run are the only ones present.
export XDG_RUNTIME_DIR="$TMP_ROOT/xdg"
mkdir -p "$XDG_RUNTIME_DIR"
RUNTIME_STATE_DIR="$XDG_RUNTIME_DIR/skillfs"
MANAGED_MOUNT_DIR="$TMP_ROOT/managed-mount"
mkdir -p "$MANAGED_MOUNT_DIR"

if ! "$BIN" mount "$SOURCE_DIR" "$MANAGED_MOUNT_DIR" --managed \
	--log-file "$TMP_ROOT/managed-client.log" >/dev/null 2>&1; then
	fail "managed mount 客户端返回非零"
fi

managed_ready=false
for _ in $(seq 1 50); do
	if grep -Fq " $MANAGED_MOUNT_DIR " /proc/mounts 2>/dev/null; then
		managed_ready=true
		break
	fi
	sleep 0.1
done
[[ "$managed_ready" == true ]] || fail "managed 挂载超时"
pass "managed 挂载成功"

WORKER_PID_FILE="$(ls "$RUNTIME_STATE_DIR"/*.worker.pid 2>/dev/null | head -n1)"
SUP_PID_FILE="$(ls "$RUNTIME_STATE_DIR"/*.supervisor.pid 2>/dev/null | head -n1)"
[[ -n "$WORKER_PID_FILE" ]] || fail "找不到 worker pid 文件"
[[ -n "$SUP_PID_FILE" ]] || fail "找不到 supervisor pid 文件"
OLD_WORKER_PID="$(cat "$WORKER_PID_FILE")"
SUP_PID="$(cat "$SUP_PID_FILE")"

info "强杀 worker (pid=$OLD_WORKER_PID)，保留 supervisor (pid=$SUP_PID)"
kill -KILL "$OLD_WORKER_PID" 2>/dev/null || true

# After SIGKILL the endpoint is dead: still in /proc/mounts but access returns
# ENOTCONN ("Transport endpoint is not connected"), or the worker has exited.
# Best-effort observation of that transient state (may race with fast recovery).
for _ in $(seq 1 30); do
	if ! kill -0 "$OLD_WORKER_PID" 2>/dev/null; then
		break
	fi
	if ! ls "$MANAGED_MOUNT_DIR" >/dev/null 2>&1; then
		info "检测到 dead FUSE endpoint (预期)"
		break
	fi
	sleep 0.1
done

# Recovery must: clear the stale endpoint, start a new worker, and leave the
# mountpoint actually readable again (ls succeeds => ENOTCONN is gone).
restored=false
for _ in $(seq 1 150); do
	if grep -Fq " $MANAGED_MOUNT_DIR " /proc/mounts 2>/dev/null; then
		NEW_WORKER_PID="$(cat "$WORKER_PID_FILE" 2>/dev/null || echo)"
		if [[ -n "$NEW_WORKER_PID" && "$NEW_WORKER_PID" != "$OLD_WORKER_PID" ]] \
			&& kill -0 "$NEW_WORKER_PID" 2>/dev/null \
			&& ls "$MANAGED_MOUNT_DIR" >/dev/null 2>&1; then
			restored=true
			break
		fi
	fi
	sleep 0.1
done
[[ "$restored" == true ]] || fail "worker 被杀后未在超时内恢复挂载"
pass "worker 崩溃后 supervisor 清理 dead endpoint 并重挂"

# Confirm the recovered mount serves content, not a stale endpoint.
if ! ls "$MANAGED_MOUNT_DIR/skills" >/dev/null 2>&1; then
	fail "恢复后 mountpoint 仍不可访问"
fi
pass "恢复后 ls <mountpoint>/skills 成功"

ORPHAN_WORKER_PID="$(cat "$WORKER_PID_FILE")"
info "强杀 supervisor (pid=$SUP_PID)，保留孤儿 worker (pid=$ORPHAN_WORKER_PID)"
kill -KILL "$SUP_PID" 2>/dev/null || true
for _ in $(seq 1 50); do
	if ! kill -0 "$SUP_PID" 2>/dev/null; then
		break
	fi
	sleep 0.1
done
if kill -0 "$SUP_PID" 2>/dev/null; then
	fail "supervisor 被 kill -9 后仍在运行"
fi
if ! kill -0 "$ORPHAN_WORKER_PID" 2>/dev/null; then
	fail "supervisor 被 kill -9 后 worker 未保持孤儿运行"
fi

if ! "$BIN" mount "$SOURCE_DIR" "$MANAGED_MOUNT_DIR" --managed \
	--log-file "$TMP_ROOT/managed-client-recover.log" >/dev/null 2>&1; then
	fail "孤儿 worker 场景下重新执行 managed mount 失败"
fi

SUP_PID_FILE="$(ls "$RUNTIME_STATE_DIR"/*.supervisor.pid 2>/dev/null | head -n1)"
WORKER_PID_FILE="$(ls "$RUNTIME_STATE_DIR"/*.worker.pid 2>/dev/null | head -n1)"
[[ -n "$SUP_PID_FILE" ]] || fail "恢复后找不到 supervisor pid 文件"
[[ -n "$WORKER_PID_FILE" ]] || fail "恢复后找不到 worker pid 文件"
SUP_PID="$(cat "$SUP_PID_FILE")"
NEW_WORKER_PID="$(cat "$WORKER_PID_FILE")"
if [[ "$NEW_WORKER_PID" == "$ORPHAN_WORKER_PID" ]]; then
	fail "重新 managed mount 后仍复用孤儿 worker"
fi
if kill -0 "$ORPHAN_WORKER_PID" 2>/dev/null; then
	fail "重新 managed mount 后孤儿 worker 仍在运行"
fi
if ! kill -0 "$SUP_PID" 2>/dev/null || ! kill -0 "$NEW_WORKER_PID" 2>/dev/null; then
	fail "重新 managed mount 后 supervisor/worker 未运行"
fi
if ! ls "$MANAGED_MOUNT_DIR/skills" >/dev/null 2>&1; then
	fail "清理孤儿 worker 后 mountpoint 不可访问"
fi
pass "supervisor 被杀后的孤儿 worker 可由下一次 managed mount 清理并重建"

# ---------------------------------------------------------------------------
# Fast-failure / ready-timeout crash-loop circuit breaker
# ---------------------------------------------------------------------------

info "测试 managed mount 快速失败熔断"

# Isolate this instance's runtime state in its own XDG_RUNTIME_DIR so its pid /
# state / log files are the only ones present, independent of the still-active
# managed mount above.
FAIL_XDG="$TMP_ROOT/xdg-fail"
mkdir -p "$FAIL_XDG"
FAIL_STATE_DIR="$FAIL_XDG/skillfs"
FAIL_MOUNT_DIR="$TMP_ROOT/managed-fail-mount"
mkdir -p "$FAIL_MOUNT_DIR"
FAIL_CLIENT_LOG="$TMP_ROOT/managed-fail-client.log"

# `--activation-mode bogus` passes client-side source validation but makes every
# foreground worker exit immediately, so the supervisor faces a permanent crash
# loop. The client must fail (not falsely report success) and the supervisor
# must give up rather than retry forever.
if XDG_RUNTIME_DIR="$FAIL_XDG" "$BIN" mount "$SOURCE_DIR" "$FAIL_MOUNT_DIR" \
	--managed --activation-mode bogus \
	--log-file "$FAIL_CLIENT_LOG" >/dev/null 2>&1; then
	fail "快速失败场景下 managed mount 客户端应返回非零"
fi
pass "快速失败场景 managed mount 客户端返回错误"

# No live supervisor or worker may remain after the client returns.
fail_daemon_gone=false
for _ in $(seq 1 100); do
	live=false
	for pf in "$FAIL_STATE_DIR"/*.supervisor.pid "$FAIL_STATE_DIR"/*.worker.pid; do
		[[ -e "$pf" ]] || continue
		p="$(cat "$pf" 2>/dev/null || echo)"
		if [[ -n "$p" ]] && kill -0 "$p" 2>/dev/null; then
			live=true
		fi
	done
	if [[ "$live" == false ]]; then
		fail_daemon_gone=true
		break
	fi
	sleep 0.1
done
[[ "$fail_daemon_gone" == true ]] || fail "快速失败后仍有存活的 supervisor/worker"
pass "快速失败后无存活 supervisor/worker"

# The mountpoint must not stay mounted, and the desired state must not remain
# "mounted" (crash-loop give up marks it stopped and clears the state file).
if grep -Fq " $FAIL_MOUNT_DIR " /proc/mounts 2>/dev/null; then
	fail "快速失败后 mountpoint 仍处于挂载状态"
fi
FAIL_STATE_FILE="$(ls "$FAIL_STATE_DIR"/*.state.json 2>/dev/null | head -n1 || true)"
if [[ -n "$FAIL_STATE_FILE" ]] && grep -Fq '"mounted"' "$FAIL_STATE_FILE" 2>/dev/null; then
	fail "快速失败后 desired state 仍为 mounted"
fi
pass "快速失败后 state 未保持 mounted 且未残留挂载"

# The crash loop must be bounded: once the supervisor gives up (and exits), its
# log stops growing. With no live daemon, two samples must be identical.
FAIL_SUP_LOG="$(ls "$FAIL_STATE_DIR"/*.supervisor.log 2>/dev/null | head -n1 || true)"
if [[ -n "$FAIL_SUP_LOG" ]]; then
	fail_size1="$(wc -c <"$FAIL_SUP_LOG")"
	sleep 1
	fail_size2="$(wc -c <"$FAIL_SUP_LOG")"
	assert_equals "$fail_size2" "$fail_size1" "快速失败后 supervisor 日志不再增长"
fi

# Best-effort cleanup of the failed instance (mountpoint should already be gone).
XDG_RUNTIME_DIR="$FAIL_XDG" "$BIN" stop "$FAIL_MOUNT_DIR" >/dev/null 2>&1 || true

info "执行 skillfs stop"
if ! "$BIN" stop "$MANAGED_MOUNT_DIR" >/dev/null 2>&1; then
	fail "stop 返回非零"
fi

managed_stopped=false
for _ in $(seq 1 50); do
	if ! grep -Fq " $MANAGED_MOUNT_DIR " /proc/mounts 2>/dev/null; then
		managed_stopped=true
		break
	fi
	sleep 0.1
done
[[ "$managed_stopped" == true ]] || fail "stop 后未卸载"

if kill -0 "$SUP_PID" 2>/dev/null; then
	fail "stop 后 supervisor 仍在运行"
fi
if [[ -f "$WORKER_PID_FILE" || -f "$SUP_PID_FILE" ]]; then
	fail "stop 后仍残留 pid 状态文件"
fi
pass "stop 清理 managed 挂载与进程"
MANAGED_MOUNT_DIR=""

info "端到端测试完成"
