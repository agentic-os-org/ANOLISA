#!/bin/bash
# AgentSight 本地开发一键启动脚本（后端 API + 前端 Dashboard）
#
# 用法:
#   ./dev.sh                      # debug 构建后端 + webpack dev server（热更新）
#   ./dev.sh --release            # release 构建后端
#   ./dev.sh --db ./my.db         # 指定 SQLite 数据库
#   ./dev.sh --frontend-port 3005
#   ./dev.sh --frontend-host 127.0.0.1  # 前端只监听本机（默认监听所有网卡）
#   ./dev.sh --host 0.0.0.0       # 后端 API 也对外监听（默认仅 127.0.0.1）
#   ./dev.sh --no-trace           # 不启动 trace（也就不采集轨迹，且不动已有 trace 进程）
#   ./dev.sh --backend-only | --frontend-only
#
# 前端 dev server（dashboard/webpack.config.js）把 /api 代理到 127.0.0.1:7396，
# 因此后端端口保持 7396 时无需任何额外配置。
#
# 远程访问: webpack dev server 默认监听所有网卡，且 allowedHosts 已含 all，
#           从其他机器直接开 http://<本机IP>:3004 即可；/api 由 dev server
#           在本机侧代理到 127.0.0.1:7396，后端不必对外暴露。
#           真正的公网访问还取决于安全组/防火墙是否放行该端口。
#
# ⚠ 安全: 后端对 loopback 来源免鉴权（src/server/auth.rs 的 is_loopback 分支），
#         而 dev server 的 /api 代理正是从 127.0.0.1 发起的 —— 所以一旦前端端口
#         对外可达，任何人都能免 token 读取全部 API 数据（LLM 请求/响应内容）。
#         仅在可信网络中这样用；否则加 --frontend-host 127.0.0.1，
#         再用 SSH 端口转发访问：ssh -L 3004:127.0.0.1:3004 <host>
#
# 轨迹采集: 默认以 root 身份连带启动 `agentsight trace`，并写一份开发用配置
#           （.dev/config.json，由仓库 agentsight.json 派生）把
#           features.trajectory_collection.enabled 打开、扫描间隔压到 5s，
#           这样 /api/trajectories 在 dev 环境有数据。非 root 会自动跳过 trace。
#           注意 trace 会连带启动全部 eBPF 探针（kernel >= 5.8 + BTF）。
#           若机器上已有 agentsight trace 在跑，脚本会先把它停掉（TERM→KILL）再启动
#           自己的实例 —— 那个进程大概率用的是系统配置（轨迹采集默认关闭），留着
#           dev 环境依然没有轨迹数据。想保留它请加 --no-trace。
#           serve 只在 trajectories.db 已存在时才打开该 store，而 collector 线程
#           要等 uprobe 挂完才 spawn（agent 进程多时 >1min），所以脚本预建空 DB
#           文件，schema 由首个打开者建表；首轮数据稍后到，刷新页面即可，不用重启。

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")" && pwd)"
LOG_DIR="$ROOT_DIR/.dev/logs"

# ─── 默认参数 ─────────────────────────────────────────────────────────────────

PROFILE="debug"
HOST="127.0.0.1"
BACKEND_PORT="7396"
FRONTEND_PORT="3004"
# 空 = 沿用 webpack-dev-server v4 默认行为（监听所有网卡），便于远程访问
FRONTEND_HOST=""
CONFIG="${AGENTSIGHT_CONFIG:-}"
DB=""
RUN_BACKEND=1
RUN_FRONTEND=1
RUN_TRACE=1
SKIP_INSTALL=0
# 开发用配置：由仓库 agentsight.json 派生，打开轨迹采集并缩短扫描间隔
DEV_CONFIG="$ROOT_DIR/.dev/config.json"
DEV_SCAN_INTERVAL=5
TRAJ_DB="/var/log/sysak/.agentsight/trajectories.db"

# 与 Makefile 保持一致
LIBBPF_SYS_LIBRARY_PATH="${LIBBPF_SYS_LIBRARY_PATH:-$(pkg-config --variable=libdir libbpf 2>/dev/null || echo /usr/lib64:/usr/lib)}"
NPM_REGISTRY="${NPM_REGISTRY:-https://registry.npmmirror.com}"

GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m'

info()  { echo -e "${BLUE}[dev]${NC} $*"; }
ok()    { echo -e "${GREEN}[dev]${NC} $*"; }
warn()  { echo -e "${YELLOW}[dev]${NC} $*"; }
error() { echo -e "${RED}[dev]${NC} $*" >&2; }

usage() {
  # 打印文件开头的注释块（跳过 shebang），不依赖硬编码行号
  awk 'NR == 1 { next } /^#/ { sub(/^# ?/, ""); print; next } { exit }' "$0"
  exit 0
}

# ─── 参数解析 ─────────────────────────────────────────────────────────────────

while [ $# -gt 0 ]; do
  case "$1" in
    --release)        PROFILE="release"; shift ;;
    --host)           HOST="$2"; shift 2 ;;
    --port)           BACKEND_PORT="$2"; shift 2 ;;
    --frontend-port)  FRONTEND_PORT="$2"; shift 2 ;;
    --frontend-host)  FRONTEND_HOST="$2"; shift 2 ;;
    --db)             DB="$2"; shift 2 ;;
    --config)         CONFIG="$2"; shift 2 ;;
    --no-trace)       RUN_TRACE=0; shift ;;
    --backend-only)   RUN_FRONTEND=0; shift ;;
    --frontend-only)  RUN_BACKEND=0; RUN_TRACE=0; shift ;;
    --skip-install)   SKIP_INSTALL=1; shift ;;
    -h|--help)        usage ;;
    *) error "未知参数: $1（--help 查看用法）"; exit 2 ;;
  esac
done

if [ "$BACKEND_PORT" != "7396" ] && [ "$RUN_FRONTEND" = "1" ]; then
  warn "后端端口为 $BACKEND_PORT，但 webpack devServer 的 /api 代理硬编码指向 127.0.0.1:7396；"
  warn "请同步修改 dashboard/webpack.config.js 的 proxy target，否则前端请求会 502。"
fi

# ─── trace / 轨迹采集前置检查 ─────────────────────────────────────────────────
# collector 线程只在 trace 路径（src/unified.rs）里启动，serve 侧纯只读，
# 所以想让 /api/trajectories 有数据就必须把 trace 一起拉起来。

# 找出真正在跑的 trace 进程：先按可执行名匹配（comm == agentsight），再确认参数里
# 带 trace 子命令。不能用 pgrep -f 'agentsight trace' —— 那会把 cmdline 里恰好含这串
# 字符的 shell / nohup wrapper（甚至调用本脚本的那个 shell）一起匹配上，既会误杀，
# 也会让「是否已退出」的检查永远为真。
trace_pids() {
  local pid args
  for pid in $(pgrep -x agentsight 2>/dev/null || true); do
    args="$(tr '\0' ' ' < "/proc/$pid/cmdline" 2>/dev/null || true)"
    case " $args" in *" trace "*) echo "$pid" ;; esac
  done
}

# 已在跑的 trace 会占着 uprobe / ring buffer，而且几乎肯定用的是系统配置
# （trajectory_collection 默认关闭）—— 留着它，dev 环境照样拿不到轨迹数据。
# 所以这里直接清掉，再由本脚本用 .dev/config.json 重新拉起。
# 不想动机器上现有的 trace 就加 --no-trace。
kill_stale_trace() {
  local pids i=0
  pids="$(trace_pids | tr '\n' ' ')"
  [ -n "${pids// /}" ] || return 0

  warn "已有 agentsight trace 在运行 (pid ${pids% })，先清理再启动本脚本自己的实例。"
  warn "不想动它请改用 --no-trace（那样 /api/trajectories 不会有新数据）。"
  kill -TERM $pids 2>/dev/null || true
  # trace 退出前要做 WAL checkpoint，给它几秒优雅收尾
  while [ $i -lt 15 ] && [ -n "$(trace_pids)" ]; do
    sleep 1
    i=$((i + 1))
  done
  pids="$(trace_pids | tr '\n' ' ')"
  if [ -n "${pids// /}" ]; then
    warn "TERM 后仍在运行 (pid ${pids% })，改用 KILL。"
    kill -KILL $pids 2>/dev/null || true
    sleep 1
  fi
  if [ -n "$(trace_pids)" ]; then
    error "无法清理已有 agentsight trace，请手动停止后重试（或用 --no-trace 跳过）。"
    exit 1
  fi
  rm -f "$ROOT_DIR/.dev/agentsight.pid"
  ok "旧 trace 进程已清理。"
}

if [ "$RUN_TRACE" = 1 ]; then
  if [ "$(id -u)" != "0" ]; then
    warn "非 root，跳过 trace（eBPF 需 root/CAP_BPF）；/api/trajectories 不会有新数据。"
    RUN_TRACE=0
  elif ! command -v python3 >/dev/null 2>&1; then
    warn "未找到 python3，无法派生开发配置，跳过 trace。"
    RUN_TRACE=0
  else
    kill_stale_trace
  fi
fi

# 未显式指定 --config 时：需要 trace 就派生一份打开轨迹采集的开发配置，
# 否则沿用系统配置。注意用户配置是「整体替换」内嵌默认值，所以必须从仓库里
# 完整的 agentsight.json 派生，不能只写一个 features 片段。
if [ -z "$CONFIG" ]; then
  if [ "$RUN_TRACE" = 1 ]; then
    mkdir -p "$(dirname "$DEV_CONFIG")"
    python3 - "$ROOT_DIR/agentsight.json" "$DEV_CONFIG" "$DEV_SCAN_INTERVAL" <<'PY'
import json, sys

src, dst, interval = sys.argv[1], sys.argv[2], int(sys.argv[3])
with open(src) as f:
    cfg = json.load(f)
cfg.setdefault("features", {})["trajectory_collection"] = {
    "enabled": True,
    "scan_interval_secs": interval,
}
with open(dst, "w") as f:
    json.dump(cfg, f, indent=2, ensure_ascii=False)
    f.write("\n")
PY
    CONFIG="$DEV_CONFIG"
    info "开发配置: $CONFIG（trajectory_collection=on, 扫描间隔 ${DEV_SCAN_INTERVAL}s）"
  else
    CONFIG="/etc/agentsight/config.json"
  fi
fi

# ─── 数据库路径 ───────────────────────────────────────────────────────────────
# 默认与 `agentsight trace` 写入的位置一致（/var/log/sysak/.agentsight/genai_events.db）。
# 非 root 环境下该目录通常不可访问，退化到仓库内 .run/ 目录（数据为空但能起服务）。

SYSTEM_DB_DIR="/var/log/sysak/.agentsight"
if [ -z "$DB" ]; then
  if [ -r "$SYSTEM_DB_DIR/genai_events.db" ] || [ -w "$SYSTEM_DB_DIR" ]; then
    DB="$SYSTEM_DB_DIR/genai_events.db"
  else
    DB="$ROOT_DIR/.dev/genai_events.db"
    warn "$SYSTEM_DB_DIR 不可访问（非 root？），改用 $DB —— 里面不会有 trace 采集的数据。"
    warn "想读真实数据: sudo ./dev.sh 或 ./dev.sh --db $SYSTEM_DB_DIR/genai_events.db"
  fi
fi

port_busy() {
  command -v ss >/dev/null 2>&1 || return 1
  ss -ltnH "sport = :$1" 2>/dev/null | grep -q .
}

kill_port() {
  local port=$1 pids
  # fuser 比 lsof 轻量，直接拿占用该端口的 PID
  pids="$(fuser "$port/tcp" 2>/dev/null | tr -s ' ')" || true
  if [ -n "${pids// /}" ]; then
    warn "端口 $port 被占用 (pid${pids})，正在清理…"
    kill -TERM $pids 2>/dev/null || true
    sleep 1
    # 仍存活则强杀
    pids="$(fuser "$port/tcp" 2>/dev/null | tr -s ' ')" || true
    [ -n "${pids// /}" ] && kill -KILL $pids 2>/dev/null || true
    sleep 0.5
    ok "端口 $port 已释放。"
  fi
}

for p in $([ "$RUN_BACKEND" = 1 ] && echo "$BACKEND_PORT") \
         $([ "$RUN_FRONTEND" = 1 ] && echo "$FRONTEND_PORT"); do
  if port_busy "$p"; then
    kill_port "$p"
  fi
  if port_busy "$p"; then
    error "端口 $p 清理后仍被占用，请手动释放。"
    exit 1
  fi
done

mkdir -p "$LOG_DIR" "$(dirname "$DB")"

# ─── 构建 ─────────────────────────────────────────────────────────────────────

BIN=""
if [ "$RUN_BACKEND" = 1 ] || [ "$RUN_TRACE" = 1 ]; then
  info "构建后端（$PROFILE）…"
  CARGO_FLAGS=()
  [ "$PROFILE" = "release" ] && CARGO_FLAGS+=(--release)
  ( cd "$ROOT_DIR" && env -u DESTDIR -u MAKEFLAGS -u MFLAGS -u MAKEOVERRIDES \
      LIBBPF_SYS_LIBRARY_PATH="$LIBBPF_SYS_LIBRARY_PATH" \
      cargo build "${CARGO_FLAGS[@]}" --bin agentsight )
  BIN="$ROOT_DIR/target/$PROFILE/agentsight"
  ok "后端二进制: $BIN"
fi

if [ "$RUN_FRONTEND" = 1 ]; then
  if ! command -v npm >/dev/null 2>&1; then
    error "未找到 npm，请先安装 Node.js（或用 --backend-only）。"
    exit 1
  fi
  if [ "$SKIP_INSTALL" = 0 ] && [ ! -d "$ROOT_DIR/dashboard/node_modules" ]; then
    info "安装前端依赖（首次较慢）…"
    ( cd "$ROOT_DIR/dashboard" && npm install --registry="$NPM_REGISTRY" )
  fi
fi

# ─── 启动 ─────────────────────────────────────────────────────────────────────

PIDS=()

cleanup() {
  trap - EXIT INT TERM
  echo
  info "正在停止…"
  for pid in ${PIDS[@]+"${PIDS[@]}"}; do
    # setsid 启动，杀整个进程组，避免 npm 退出后 webpack 残留
    kill -TERM "-$pid" 2>/dev/null || kill -TERM "$pid" 2>/dev/null || true
  done
  sleep 1
  for pid in ${PIDS[@]+"${PIDS[@]}"}; do
    kill -KILL "-$pid" 2>/dev/null || true
  done
  ok "已停止（日志保留在 $LOG_DIR）"
}
trap cleanup EXIT INT TERM

start() { # start <name> <logfile> <cmd...>
  local name="$1" log="$2"; shift 2
  : > "$log"
  setsid "$@" >"$log" 2>&1 &
  local pid=$!
  PIDS+=("$pid")
  info "$name 已启动 (pid $pid) → $log"
}

# trace 先起：serve 只在 trajectories.db 已存在时才打开轨迹 store，
# 顺序反了会导致首跑轨迹页一直是空的（要重启 serve 才认）。
if [ "$RUN_TRACE" = 1 ]; then
  # serve 只在 trajectories.db 已存在时才打开轨迹 store（src/server/mod.rs:305），
  # 而 collector 线程在 AgentSight::new() 里排在「给已存在进程挂 uprobe」之后才
  # spawn —— 机器上 agent 进程多时这一步能花 1 分钟以上，等它落盘再起 serve 很脆。
  # 两侧的 TrajectoryStore::new_with_path 都是 CREATE TABLE IF NOT EXISTS
  # （store.rs:83），所以这里预先建出空文件，让谁先打开谁建表，彻底去掉时序依赖。
  if [ ! -f "$TRAJ_DB" ]; then
    mkdir -p "$(dirname "$TRAJ_DB")"
    : > "$TRAJ_DB"
    info "预建空 $TRAJ_DB（schema 由首个打开者创建），避免 serve 漏开轨迹 store"
  fi
  start trace "$LOG_DIR/trace.log" \
    env RUST_LOG="${RUST_LOG:-info}" \
    "$BIN" trace --config "$CONFIG" --pid-file "$ROOT_DIR/.dev/agentsight.pid"
fi

if [ "$RUN_BACKEND" = 1 ]; then
  start backend "$LOG_DIR/backend.log" \
    env RUST_LOG="${RUST_LOG:-info}" \
    "$BIN" serve --host "$HOST" --port "$BACKEND_PORT" --db "$DB" --config "$CONFIG"
fi

if [ "$RUN_FRONTEND" = 1 ]; then
  FE_ARGS=(--port "$FRONTEND_PORT")
  [ -n "$FRONTEND_HOST" ] && FE_ARGS+=(--host "$FRONTEND_HOST")
  start frontend "$LOG_DIR/frontend.log" \
    env NODE_ENV=development npm --prefix "$ROOT_DIR/dashboard" run dev -- "${FE_ARGS[@]}"
fi

echo
[ "$RUN_TRACE" = 1 ]    && ok "eBPF trace:     运行中（轨迹采集已开启 → $TRAJ_DB）"
[ "$RUN_BACKEND" = 1 ]  && ok "后端 API:      http://$HOST:$BACKEND_PORT  (DB: $DB)"
if [ "$RUN_FRONTEND" = 1 ]; then
  ok "前端 Dashboard: http://localhost:$FRONTEND_PORT"
  if [ -z "$FRONTEND_HOST" ] || [ "$FRONTEND_HOST" = "0.0.0.0" ]; then
    # dev server 监听所有网卡，给出可直接分享的地址
    LAN_IP="$(hostname -I 2>/dev/null | awk '{print $1}')"
    [ -n "$LAN_IP" ] && ok "远程访问:       http://$LAN_IP:$FRONTEND_PORT  （需安全组/防火墙放行）"
    warn "前端端口对外可达 = API 免鉴权暴露（代理走 loopback，绕过 dashboard token）。"
    warn "非可信网络请改用 --frontend-host 127.0.0.1 + ssh -L $FRONTEND_PORT:127.0.0.1:$FRONTEND_PORT <host>"
  fi
fi
info "Ctrl-C 停止两端。日志同时输出到下方与 $LOG_DIR/"
echo

# 汇总两份日志到终端，并在任一进程退出时收尾
tail -n +1 -F "$LOG_DIR"/*.log 2>/dev/null &
TAIL_PID=$!

set +e
wait -n ${PIDS[@]+"${PIDS[@]}"}
exit_code=$?
set -e
kill "$TAIL_PID" 2>/dev/null || true
warn "有子进程退出（code $exit_code），一并关闭其余进程。"
exit "$exit_code"
