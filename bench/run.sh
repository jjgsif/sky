#!/usr/bin/env bash
# Benchmark harness for sky-gateway POST /hello.
#
# Usage: ./bench/run.sh
# Overrides: wrk2_THREADS=8 wrk2_CONNECTIONS=200 wrk2_DURATION=60s wrk2_RATE=3000 ./bench/run.sh
#
# Requires: wrk2, bun, cargo (release build)
set -euo pipefail

GATEWAY_BIN="./target/release/sky-gateway"
BENCH_CONFIG="./bench/sky-bench.toml"
URL="http://127.0.0.1:8080/hello"

wrk2_RATE="${wrk2_RATE:-2000}"
wrk2_THREADS="${wrk2_THREADS:-4}"
wrk2_CONNECTIONS="${wrk2_CONNECTIONS:-100}"
wrk2_DURATION="${wrk2_DURATION:-30s}"
WARMUP_DURATION="5s"
READY_TIMEOUT=30


if ! command -v wrk2 &>/dev/null; then
    echo "error: wrk2 not found. Install with: brew install wrk2  or  apt install wrk2" >&2
    exit 1
fi

echo "==> Building sky-gateway (release)..."
cargo build -p sky-gateway --release --quiet

echo "==> Starting gateway (config: $BENCH_CONFIG)..."
"$GATEWAY_BIN" --config "$BENCH_CONFIG" &>/tmp/sky-bench-gateway.log &
GATEWAY_PID=$!
trap 'kill "$GATEWAY_PID" 2>/dev/null || true; wait "$GATEWAY_PID" 2>/dev/null || true' EXIT

echo "==> Waiting for gateway to be ready..."
for i in $(seq 1 "$READY_TIMEOUT"); do
    if curl -sf -X POST "$URL" \
            -H "Content-Type: application/json" \
            -d '{"name":"probe"}' >/dev/null 2>&1; then
        echo "    Ready after ${i}s."
        break
    fi
    if [ "$i" -eq "$READY_TIMEOUT" ]; then
        echo "error: gateway did not become ready within ${READY_TIMEOUT}s." >&2
        echo "Gateway log:" >&2
        cat /tmp/sky-bench-gateway.log >&2
        exit 1
    fi
    sleep 1
done

echo "==> Warmup (${WARMUP_DURATION}, ${wrk2_THREADS}t/${wrk2_CONNECTIONS}c)..."
wrk2 -t"$wrk2_THREADS" -c"$wrk2_CONNECTIONS" -d"$WARMUP_DURATION" -R"$wrk2_RATE" \
    -s bench/wrk/hello.lua "$URL" >/dev/null

echo ""
echo "==> Benchmark (${wrk2_DURATION}, ${wrk2_THREADS}t/${wrk2_CONNECTIONS}c)..."
echo ""
wrk2 -t"$wrk2_THREADS" -c"$wrk2_CONNECTIONS" -d"$wrk2_DURATION" -R"$wrk2_RATE"\
    --latency \
    -s bench/wrk/hello.lua "$URL" \
