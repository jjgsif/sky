#!/usr/bin/env bash
# bench/isolation.sh
# Worker isolation benchmark.
#
# Tests whether CPU-bound streaming on some workers degrades unary
# latency on others. Runs two wrk2 processes simultaneously:
#
#   Process A — hammers /hello (unary, fast)
#   Process B — hammers /stream/fibonacci (CPU-bound streaming, slow)
#
# Compares /hello latency during isolation vs during concurrent streaming.
# A well-isolated worker pool should show minimal /hello degradation.
#
# Usage:
#   ./bench/isolation.sh
#   ISOLATION_DURATION=60s ISOLATION_RATE=5000 ./bench/isolation.sh
#
# Requires: wrk2

set -euo pipefail

BASE_URL="${BASE_URL:-http://127.0.0.1:8080}"
ISOLATION_DURATION="${ISOLATION_DURATION:-30s}"
ISOLATION_RATE="${ISOLATION_RATE:-2000}"
ISOLATION_THREADS="${ISOLATION_THREADS:-4}"
ISOLATION_CONNECTIONS="${ISOLATION_CONNECTIONS:-50}"
RESULTS_MD="${RESULTS_MD:-./bench/CLAUDE.md}"
WARMUP_DURATION="5s"

if ! command -v wrk2 &>/dev/null; then
    echo "error: wrk2 not found" >&2
    exit 1
fi

HELLO_BASELINE="$(mktemp /tmp/sky-isolation-baseline-XXXXXX.txt)"
HELLO_CONCURRENT="$(mktemp /tmp/sky-isolation-concurrent-XXXXXX.txt)"
FIB_CONCURRENT="$(mktemp /tmp/sky-isolation-fib-XXXXXX.txt)"
trap 'rm -f "$HELLO_BASELINE" "$HELLO_CONCURRENT" "$FIB_CONCURRENT"' EXIT

echo "==> Isolation benchmark"
echo "    Duration    : ${ISOLATION_DURATION}"
echo "    Rate        : ${ISOLATION_RATE} req/s per process"
echo "    Threads     : ${ISOLATION_THREADS}"
echo "    Connections : ${ISOLATION_CONNECTIONS}"
echo ""

# ── Phase 1: Unary baseline (no streaming load) ───────────────────────────────
echo "── Phase 1: /hello baseline (no concurrent load)"
echo "   Warming up..."
wrk2 -t"$ISOLATION_THREADS" -c"$ISOLATION_CONNECTIONS" \
    -d"$WARMUP_DURATION" -R"$ISOLATION_RATE" \
    -s bench/wrk/hello.lua \
    "${BASE_URL}/hello" >/dev/null 2>&1

echo "   Benchmarking..."
wrk2 -t"$ISOLATION_THREADS" -c"$ISOLATION_CONNECTIONS" \
    -d"$ISOLATION_DURATION" -R"$ISOLATION_RATE" \
    --latency \
    -s bench/wrk/hello.lua \
    "${BASE_URL}/hello" > "$HELLO_BASELINE" 2>&1

echo "   Done."
echo ""

# ── Phase 2: Concurrent load — /hello + fibonacci streaming ──────────────────
echo "── Phase 2: /hello + /stream/fibonacci concurrently"
echo "   Warming up..."
wrk2 -t"$ISOLATION_THREADS" -c"$ISOLATION_CONNECTIONS" \
    -d"$WARMUP_DURATION" -R"$ISOLATION_RATE" \
    -s bench/wrk/hello.lua \
    "${BASE_URL}/hello" >/dev/null 2>&1 &
wrk2 -t"$ISOLATION_THREADS" -c"$ISOLATION_CONNECTIONS" \
    -d"$WARMUP_DURATION" -R500 \
    -s bench/wrk/stream-fibonacci.lua \
    "${BASE_URL}/stream/fibonacci?limit=20" >/dev/null 2>&1 &
wait

echo "   Benchmarking concurrently..."
wrk2 -t"$ISOLATION_THREADS" -c"$ISOLATION_CONNECTIONS" \
    -d"$ISOLATION_DURATION" -R"$ISOLATION_RATE" \
    --latency \
    -s bench/wrk/hello.lua \
    "${BASE_URL}/hello" > "$HELLO_CONCURRENT" 2>&1 &
HELLO_PID=$!

wrk2 -t"$ISOLATION_THREADS" -c"$ISOLATION_CONNECTIONS" \
    -d"$ISOLATION_DURATION" -R500 \
    --latency \
    -s bench/wrk/stream-fibonacci.lua \
    "${BASE_URL}/stream/fibonacci?limit=20" > "$FIB_CONCURRENT" 2>&1 &
FIB_PID=$!

wait "$HELLO_PID" "$FIB_PID"
echo "   Done."
echo ""

# ── Report ────────────────────────────────────────────────────────────────────

extract_stat() {
    local file="$1"
    local pattern="$2"
    grep -E "$pattern" "$file" | head -1 | awk '{print $NF}' || echo "n/a"
}

extract_pct() {
    local file="$1"
    local pct="$2"
    grep -E "^\s+${pct}%" "$file" | awk '{print $2}' || echo "n/a"
}

echo "── /hello latency: baseline vs concurrent streaming ─────────────────"
printf "  %-12s  %-20s  %-20s  %s\n" "Percentile" "Baseline" "w/ Fib Streaming" "Delta"
printf "  %-12s  %-20s  %-20s  %s\n" "----------" "--------" "----------------" "-----"

for pct in "50.000%" "90.000%" "99.000%" "99.900%"; do
    base=$(extract_pct "$HELLO_BASELINE" "$pct")
    conc=$(extract_pct "$HELLO_CONCURRENT" "$pct")
    printf "  %-12s  %-20s  %-20s\n" "$pct" "$base" "$conc"
done
echo ""

# ── Append to CLAUDE.md ───────────────────────────────────────────────────────
RUN_DATE="$(date -u '+%Y-%m-%d %H:%M UTC')"

{
    echo ""
    echo "### Isolation Test — ${RUN_DATE}"
    echo ""
    echo "Measures /hello latency degradation while CPU-bound streaming is active."
    echo ""
    echo "| Percentile | /hello baseline | /hello + fib streaming |"
    echo "|------------|-----------------|------------------------|"
    for pct in "50.000%" "90.000%" "99.000%" "99.900%"; do
        base=$(extract_pct "$HELLO_BASELINE" "$pct")
        conc=$(extract_pct "$HELLO_CONCURRENT" "$pct")
        echo "| ${pct}   | ${base}          | ${conc}                |"
    done
    echo ""
    echo "<details>"
    echo "<summary>Raw wrk2 output</summary>"
    echo ""
    echo "\`\`\`"
    echo "=== /hello baseline ==="
    cat "$HELLO_BASELINE"
    echo ""
    echo "=== /hello concurrent ==="
    cat "$HELLO_CONCURRENT"
    echo ""
    echo "=== /stream/fibonacci concurrent ==="
    cat "$FIB_CONCURRENT"
    echo "\`\`\`"
    echo ""
    echo "</details>"
    echo ""
    echo "---"
} >> "$RESULTS_MD"

echo "==> Results appended to ${RESULTS_MD}"
