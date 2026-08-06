#!/usr/bin/env bash
# 内存 benchmark：测量 ximed-server 启动后的峰值/常驻内存。
# 用法：./bench_memory.sh [二进制路径]
# 目标：< 20MB（设计文档第 11 节）

set -euo pipefail

BIN="${1:-target/release/server}"
if [ ! -x "$BIN" ]; then
  echo "error: binary not found: $BIN (先运行 cargo build --release)"
  exit 1
fi

DATA_DIR="$(mktemp -d)"
export XIMED_AUTH_PASSWORD=benchmark
export XIMED__SERVER__DATA_DIR="$DATA_DIR"
export XIMED__SERVER__ADDR="127.0.0.1:18443"

echo "== 启动 $BIN =="
if command -v /usr/bin/time >/dev/null 2>&1 && [[ "$(uname)" == "Darwin" ]]; then
  /usr/bin/time -l "$BIN" 2>&1 >/dev/null &
  SERVER_PID=$!
elif command -v /usr/bin/time >/dev/null 2>&1 && [[ "$(uname)" == "Linux" ]]; then
  /usr/bin/time -v "$BIN" 2>&1 >/dev/null &
  SERVER_PID=$!
else
  "$BIN" >/dev/null 2>&1 &
  SERVER_PID=$!
fi

# 等待服务就绪
for _ in $(seq 1 50); do
  if curl -sf http://127.0.0.1:18443/healthz >/dev/null 2>&1; then
    break
  fi
  sleep 0.1
done

echo "== 请求 /healthz =="
curl -s http://127.0.0.1:18443/healthz
echo

echo "== 常驻内存 (RSS) =="
RSS_KB=$(ps -o rss= -p "$SERVER_PID" 2>/dev/null | tr -d ' ' || echo 0)
echo "RSS: ${RSS_KB} KB ($((RSS_KB / 1024)) MB)"

echo "== 停止 =="
kill "$SERVER_PID" 2>/dev/null || true
wait "$SERVER_PID" 2>/dev/null || true
rm -rf "$DATA_DIR"

echo
if [ "$RSS_KB" -gt 0 ] && [ $((RSS_KB / 1024)) -lt 20 ]; then
  echo "RESULT: PASS (内存 < 20MB)"
else
  echo "RESULT: 注意 (内存 >= 20MB 或未测得)"
fi
