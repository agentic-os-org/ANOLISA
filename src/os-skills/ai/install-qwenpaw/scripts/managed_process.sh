#!/usr/bin/env bash
# Stop only the process started by a previous run of this installer.

stop_managed_qwenpaw() {
  local pid_file="$1" old_pid command
  [[ -f "$pid_file" ]] || return 0

  old_pid=$(<"$pid_file")
  if [[ ! "$old_pid" =~ ^[1-9][0-9]*$ ]]; then
    echo "  警告: 无效的 QwenPaw PID 文件，跳过停止旧进程: $pid_file" >&2
    return 0
  fi

  command=$(ps -p "$old_pid" -o args= 2>/dev/null) || return 0
  if [[ ! "$command" =~ ^([^[:space:]]*/)?qwenpaw[[:space:]]+app([[:space:]]|$) ]]; then
    echo "  警告: PID $old_pid 已不是 QwenPaw app，跳过停止" >&2
    return 0
  fi

  echo "  停止之前由安装脚本启动的 QwenPaw 进程 (PID: $old_pid)..."
  if kill "$old_pid" 2>/dev/null; then
    sleep 2
  fi
}
