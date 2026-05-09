#!/usr/bin/env bash
# bench/run.sh
# Benchmark harness for sky-gateway.
#
# Modes:
#   ./bench/run.sh               — standard three-scenario benchmark
#   ./bench/run.sh --ramp        — ramp req/s from MIN to MAX, find saturation point
#   ./bench/run.sh --ttfb        — TTFB measurement only (delegates to ttfb.sh)
#   ./bench/run.sh --isolation   — worker isolation test (delegates to isolation.sh)
#   ./bench/run.sh --all         — run all of the above in sequence
#
# Rate overrides (standard mode):
#   wrk2_RATE=5000 wrk2_THREADS=8 wrk2_CONNECTIONS=200 ./bench/run.sh
#
# Ramp overrides:
#   RAMP_MIN=1000 RAMP_MAX=300000 RAMP_STEP=10000 RAMP_DURATION=15s ./bench/run.sh --ramp
#
# Requires: wrk2, curl, bun, cargo (release build)

set -euo pipefail

GATEWAY_BIN="./target/release/sky-gateway"
BENCH_CONFIG="./bench/sky-bench.toml"
RESULTS_MD="./bench/CLAUDE.md"
BASE_URL="http://127.0.0.1:8080"
READY_URL="${BASE_URL}/hello"
READY_TIMEOUT=30

# ── Standard benchmark defaults ───────────────────────────────────────────────
wrk2_RATE="${wrk2_RATE:-2000}"
wrk2_THREADS="${wrk2_THREADS:-4}"
wrk2_CONNECTIONS="${wrk2_CONNECTIONS:-100}"
wrk2_DURATION="${wrk2_DURATION:-30s}"
WARMUP_DURATION="5s"

# ── Ramp defaults ─────────────────────────────────────────────────────────────
RAMP_MIN="${RAMP_MIN:-5000}"
RAMP_MAX="${RAMP_MAX:-250000}"
RAMP_STEP="${RAMP_STEP:-5000}"
RAMP_DURATION="${RAMP_DURATION:-15s}"
# Latency threshold — if p99 exceeds this (ms) treat as saturated
RAMP_LATENCY_THRESHOLD="${RAMP_LATENCY_THRESHOLD:-50}"

MODE="${1:---standard}"

# ── Sanity checks ─────────────────────────────────────────────────────────────
if ! command -v wrk2 &>/dev/null; then
    echo "error: wrk2 not found. Install: brew install wrk2  or  apt install wrk2" >&2
    exit 1
fi

# ── Build ─────────────────────────────────────────────────────────────────────
# echo "==> Building sky-gateway (release)..."
# cargo build -p sky-gateway --release --quiet

# ── Start gateway ─────────────────────────────────────────────────────────────
start_gateway() {
    echo "==> Connecting to Gateway"
    echo "==> Starting gateway (config: $BENCH_CONFIG)..."
    "$GATEWAY_BIN" --config "$BENCH_CONFIG" &>/tmp/sky-bench-gateway.log &
    GATEWAY_PID=$!

    echo "==> Waiting for gateway..."
    for i in $(seq 1 "$READY_TIMEOUT"); do
        if curl -sf -X POST "$READY_URL" \
                -H "Content-Type: application/json" \
                -d '{"name":"probe"}' >/dev/null 2>&1; then
            echo "    Ready after ${i}s."
            return
        fi
        if [ "$i" -eq "$READY_TIMEOUT" ]; then
            echo "error: gateway not ready within ${READY_TIMEOUT}s" >&2
            cat /tmp/sky-bench-gateway.log >&2
            exit 1
        fi
        sleep 1
    done
}

stop_gateway() {
    kill "$GATEWAY_PID" 2>/dev/null || true
    wait "$GATEWAY_PID" 2>/dev/null || true
}

GATEWAY_PID=""
trap 'rm -f "$CAPTURE_FILE" 2>/dev/null; stop_gateway' EXIT
CAPTURE_FILE="$(mktemp /tmp/sky-bench-XXXXXX.txt)"

# ── Helpers ───────────────────────────────────────────────────────────────────

run_bench() {
    local name="$1"
    local url="$2"
    local script="$3"
    local rate="${4:-$wrk2_RATE}"

    {
        echo ""
        echo "### ${name}"
        echo "\`${url}\`"
        echo ""
    } | tee -a "$CAPTURE_FILE"

    echo "==> Warmup (${WARMUP_DURATION})..."
    wrk2 -t"$wrk2_THREADS" -c"$wrk2_CONNECTIONS" \
        -d"$WARMUP_DURATION" -R"$rate" \
        -s "$script" "$url" >/dev/null 2>&1

    echo "==> Benchmark (${wrk2_DURATION}, ${wrk2_THREADS}t/${wrk2_CONNECTIONS}c, R=${rate}/s)..."
    wrk2 -t"$wrk2_THREADS" -c"$wrk2_CONNECTIONS" \
        -d"$wrk2_DURATION" -R"$rate" \
        --latency \
        -s "$script" "$url" \
        | tee -a "$CAPTURE_FILE"
}

# ── Standard mode ─────────────────────────────────────────────────────────────

run_standard() {
    start_gateway

    run_bench "Unary — POST /hello"                       \
        "${BASE_URL}/hello"                               \
        "bench/wrk/hello.lua"

    run_bench "Streaming events — GET /stream/events?count=10"   \
        "${BASE_URL}/stream/events?count=10"              \
        "bench/wrk/stream-events.lua"

    run_bench "Streaming fibonacci — GET /stream/fibonacci?limit=20" \
        "${BASE_URL}/stream/fibonacci?limit=20"           \
        "bench/wrk/stream-fibonacci.lua"

    append_results "Standard benchmark"
}

# ── Ramp mode ─────────────────────────────────────────────────────────────────
# Increases req/s until p99 latency exceeds threshold or max rate is reached.
# Identifies the saturation point precisely.

run_ramp() {
    start_gateway

    echo "==> Ramp mode: ${RAMP_MIN} → ${RAMP_MAX} req/s (step ${RAMP_STEP})"
    echo "    p99 threshold: ${RAMP_LATENCY_THRESHOLD}ms"
    echo "    Duration per step: ${RAMP_DURATION}"
    echo ""

    local ramp_results
    ramp_results="$(mktemp /tmp/sky-ramp-XXXXXX.txt)"
    trap "rm -f $ramp_results" RETURN

    local saturated_at=""
    local peak_rps=0

    printf "  %-12s  %-12s  %-12s  %-12s  %-12s\n" \
        "Target r/s" "Actual r/s" "p50 (ms)" "p99 (ms)" "Status"
    printf "  %-12s  %-12s  %-12s  %-12s  %-12s\n" \
        "----------" "----------" "--------" "--------" "------"

    local rate=$RAMP_MIN
    while [ "$rate" -le "$RAMP_MAX" ]; do
        local step_out
        step_out="$(mktemp /tmp/sky-ramp-step-XXXXXX.txt)"

        wrk2 -t"$wrk2_THREADS" -c"$wrk2_CONNECTIONS" \
            -d"$RAMP_DURATION" -R"$rate" \
            --latency \
            -s bench/wrk/hello.lua \
            "${BASE_URL}/hello" > "$step_out" 2>&1 || true

        local actual_rps p50 p99 status
        actual_rps=$(grep -E "Requests/sec" "$step_out" | awk '{print $2}' | cut -d'.' -f1 || echo "0")
        p50=$(grep -E "^\s+50\.000%" "$step_out" | awk '{print $2}' || echo "0")
        p99=$(grep -E "^\s+99\.000%" "$step_out" | awk '{print $2}' || echo "0")

        # Convert p99 to ms for comparison (wrk2 outputs in us)
        local p99_ms
        p99_ms=$(echo "$p99" | sed 's/us//' | awk '{printf "%.1f", $1/1000}' 2>/dev/null || echo "0")

        if (( $(echo "$p99_ms > $RAMP_LATENCY_THRESHOLD" | bc -l 2>/dev/null || echo 0) )); then
            status="SATURATED"
            saturated_at="$rate"
        else
            status="OK"
            peak_rps="$actual_rps"
        fi

        printf "  %-12s  %-12s  %-12s  %-12s  %-12s\n" \
            "${rate}" "${actual_rps}" "${p50}" "${p99}" "${status}"

        echo "${rate},${actual_rps},${p50},${p99},${p99_ms},${status}" >> "$ramp_results"

        # Append step detail to capture file
        {
            echo ""
            echo "#### Ramp step — ${rate} req/s target"
            cat "$step_out"
        } >> "$CAPTURE_FILE"

        rm -f "$step_out"

        if [ "$status" = "SATURATED" ]; then
            echo ""
            echo "==> Saturation detected at ${rate} req/s (p99=${p99_ms}ms > ${RAMP_LATENCY_THRESHOLD}ms)"
            echo "    Peak sustainable rate: ~${peak_rps} req/s"
            break
        fi

        rate=$(( rate + RAMP_STEP ))
    done

    echo ""

    # Append ramp table to CLAUDE.md
    {
        echo ""
        echo "### Ramp Results — $(date -u '+%Y-%m-%d %H:%M UTC')"
        echo ""
        echo "Endpoint: POST /hello (unary)"
        echo "p99 saturation threshold: ${RAMP_LATENCY_THRESHOLD}ms"
        echo ""
        echo "| Target req/s | Actual req/s | p50 | p99 | Status |"
        echo "|-------------|-------------|-----|-----|--------|"
        while IFS=',' read -r target actual p50 p99 p99ms status; do
            echo "| ${target} | ${actual} | ${p50} | ${p99} | ${status} |"
        done < "$ramp_results"
        if [ -n "$saturated_at" ]; then
            echo ""
            echo "**Saturation point: ${saturated_at} req/s**"
            echo "**Peak sustainable rate: ~${peak_rps} req/s**"
        fi
        echo ""
        echo "---"
    } >> "$RESULTS_MD"

    rm -f "$ramp_results"
}

# ── Append results to CLAUDE.md ───────────────────────────────────────────────

append_results() {
    local label="$1"
    local GIT_HASH RUN_DATE HOST_INFO CPU_INFO GIT_MSG
    GIT_HASH="$(git rev-parse --short HEAD 2>/dev/null || echo "unknown")"
    GIT_MSG="$(git log -1 --format="%s" 2>/dev/null || echo "")"
    RUN_DATE="$(date -u '+%Y-%m-%d %H:%M UTC')"
    HOST_INFO="$(uname -srm)"
    CPU_INFO="$(grep -m1 'model name' /proc/cpuinfo 2>/dev/null \
        | sed 's/model name\s*:\s*//' \
        || sysctl -n machdep.cpu.brand_string 2>/dev/null \
        || echo "unknown")"

    echo ""
    echo "==> Appending results to ${RESULTS_MD}..."

    {
        echo ""
        echo "## ${label} — ${RUN_DATE} — \`${GIT_HASH}\` ${GIT_MSG}"
        echo ""
        echo "| Setting     | Value |"
        echo "|-------------|-------|"
        echo "| Threads     | ${wrk2_THREADS} |"
        echo "| Connections | ${wrk2_CONNECTIONS} |"
        echo "| Duration    | ${wrk2_DURATION} |"
        echo "| Target rate | ${wrk2_RATE} req/s |"
        echo "| Host        | ${HOST_INFO} |"
        echo "| CPU         | ${CPU_INFO} |"
        echo ""
        echo '```'
        cat "$CAPTURE_FILE"
        echo '```'
        echo ""
        echo "---"
    } >> "$RESULTS_MD"

    echo "==> Done. Results saved to ${RESULTS_MD}"
}

# ── Mode dispatch ─────────────────────────────────────────────────────────────

case "$MODE" in
    --standard|"")
        run_standard
        ;;
    --ramp)
        run_ramp
        ;;
    --ttfb)
        start_gateway
        BASE_URL="$BASE_URL" RESULTS_MD="$RESULTS_MD" bash bench/ttfb.sh
        ;;
    --isolation)
        start_gateway
        BASE_URL="$BASE_URL" RESULTS_MD="$RESULTS_MD" bash bench/isolation.sh
        ;;
    --all)
        run_standard
        stop_gateway
        start_gateway
        BASE_URL="$BASE_URL" RESULTS_MD="$RESULTS_MD" bash bench/ttfb.sh
        stop_gateway
        start_gateway
        BASE_URL="$BASE_URL" RESULTS_MD="$RESULTS_MD" bash bench/isolation.sh
        stop_gateway
        run_ramp
        ;;
    *)
        echo "Unknown mode: $MODE" >&2
        echo "Usage: $0 [--standard|--ramp|--ttfb|--isolation|--all]" >&2
        exit 1
        ;;
esac
