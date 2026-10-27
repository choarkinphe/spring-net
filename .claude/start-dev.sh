#!/bin/bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export AONET_ROOT="$ROOT"
PORT="${AONET_PORT:-9876}"
URL="http://127.0.0.1:$PORT"
command -v cargo >/dev/null || { printf '需要先安装 Rust/Cargo。\n' >&2; exit 1; }
healthy() {
  local body
  body="$(curl --noproxy '*' -fsS --max-time 2 "$URL/api/health" 2>/dev/null)" || return 1
  [[ "$body" == *'"app":"aonet"'* && "$body" == *'"ready":true'* ]]
}
if healthy; then
  printf '复用健康的 AoNet 实例：%s\n' "$URL"
  while healthy; do sleep 5; done
  exit 0
fi
if lsof -nP -iTCP:"$PORT" -sTCP:LISTEN >/dev/null 2>&1; then
  printf '端口 %s 被非健康 AoNet 或其他程序占用；不会终止该进程。\n' "$PORT" >&2
  exit 1
fi
printf '构建 AoNet（首次需要下载并编译 Rust 依赖）…\n'
cargo build --manifest-path "$ROOT/Cargo.toml"
"$ROOT/target/debug/aonet" &
CHILD=$!
cleanup() {
  if kill -0 "$CHILD" 2>/dev/null; then
    kill -TERM "$CHILD" 2>/dev/null || true
    wait "$CHILD" 2>/dev/null || true
  fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
for ((i=0; i<100; i++)); do
  if healthy; then
    printf 'AoNet 已就绪：%s\n' "$URL"
    wait "$CHILD"
    exit $?
  fi
  kill -0 "$CHILD" 2>/dev/null || { wait "$CHILD"; exit 1; }
  sleep 0.2
done
printf 'AoNet 健康检查超时。\n' >&2
exit 1
