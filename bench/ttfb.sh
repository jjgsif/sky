#!/usr/bin/env bash
# bench/ttfb.sh
# Time-to-first-byte measurement for streaming endpoints.
#
# wrk2 buffers the full response before reporting latency, so it cannot
# measure TTFB on streaming responses. This script uses curl's built-in
# timing variables to capture the moment the first byte arrives.
#
# Runs N sequential requests and reports min/median/p95/max TTFB.
#
# Usage:
#   ./bench/ttfb.sh
#   TTFB_RUNS=200 TTFB_CONCURRENCY=20 ./bench/ttfb.sh
#
# Requires: curl, bc, sort, awk

set -euo pipefail

BASE_URL="${BASE_URL:-http://127.0.0.1:8080}"
TTFB_RUNS="${TTFB_RUNS:-100}"
TTFB_CONCURRENCY="${TTFB_CONCURRENCY:-10}"
RESULTS_MD="${RESULTS_MD:-./bench/CLAUDE.md}"

ENDPOINTS=(
    "POST|/hello|{\"name\":\"bench\"}|application/json"
    "GET|/stream/events?count=10||application/x-ndjson"
    "GET|/stream/fibonacci?limit=20||application/x-ndjson"
)

echo "==> TTFB benchmark"
echo "    Runs        : ${TTFB_RUNS}"
echo "    Concurrency : ${TTFB_CONCURRENCY}"
echo "    Base URL    : ${BASE_URL}"
echo ""

# ── measure_ttfb endpoint_label method path body content_type ────────────────
measure_ttfb() {
    local label="$1"
    local method="$2"
    local path="$3"
    local body="$4"
    local accept="$5"

    local url="${BASE_URL}${path}"
    local tmpdir
    tmpdir="$(mktemp -d)"
    local results_file="${tmpdir}/results.txt"

    echo "── ${label} ──────────────────────────────────────────"
    echo "   ${method} ${path}"

    # Fire TTFB_RUNS requests in batches of TTFB_CONCURRENCY
    local completed=0
    while [ "$completed" -lt "$TTFB_RUNS" ]; do
        local batch_size=$(( TTFB_RUNS - completed ))
        if [ "$batch_size" -gt "$TTFB_CONCURRENCY" ]; then
            batch_size="$TTFB_CONCURRENCY"
        fi

        for _ in $(seq 1 "$batch_size"); do
            (
                local curl_args=(
                    -s
                    -o /dev/null
                    -X "$method"
                    -H "Accept: ${accept}"
                    # Only time_starttransfer — the moment the first byte of
                    # the response body arrives. This is true TTFB.
                    -w "%{time_starttransfer}"
                )

                if [ -n "$body" ]; then
                    curl_args+=(-H "Content-Type: application/json" -d "$body")
                fi

                # --no-buffer ensures curl doesn't buffer streaming responses
                curl_args+=(--no-buffer "$url")

                curl "${curl_args[@]}" >> "$results_file" 2>/dev/null
                echo "" >> "$results_file"
            ) &
        done
        wait
        completed=$(( completed + batch_size ))
    done

    # Parse results — curl outputs decimal seconds, convert to ms
    local times
    times=$(grep -E '^[0-9]' "$results_file" | awk '{printf "%.2f\n", $1 * 1000}' | sort -n)

    local count
    count=$(echo "$times" | wc -l | tr -d ' ')

    if [ "$count" -eq 0 ]; then
        echo "   No results collected — is the gateway running?"
        rm -rf "$tmpdir"
        return
    fi

    local min med p90 p95 p99 max
    min=$(echo "$times" | head -1)
    max=$(echo "$times" | tail -1)
    med=$(echo "$times" | awk "NR==int($count*0.50)")
    p90=$(echo "$times" | awk "NR==int($count*0.90)")
    p95=$(echo "$times" | awk "NR==int($count*0.95)")
    p99=$(echo "$times" | awk "NR==int($count*0.99)")

    echo ""
    echo "   Samples : ${count}"
    printf "   %-8s : %s ms\n" "min"  "$min"
    printf "   %-8s : %s ms\n" "p50"  "$med"
    printf "   %-8s : %s ms\n" "p90"  "$p90"
    printf "   %-8s : %s ms\n" "p95"  "$p95"
    printf "   %-8s : %s ms\n" "p99"  "$p99"
    printf "   %-8s : %s ms\n" "max"  "$max"
    echo ""

    # Append to CLAUDE.md
    {
        echo ""
        echo "#### TTFB — ${label}"
        echo ""
        echo "| Percentile | TTFB (ms) |"
        echo "|------------|-----------|"
        echo "| min        | ${min}    |"
        echo "| p50        | ${med}    |"
        echo "| p90        | ${p90}    |"
        echo "| p95        | ${p95}    |"
        echo "| p99        | ${p99}    |"
        echo "| max        | ${max}    |"
        echo ""
    } >> "$RESULTS_MD"

    rm -rf "$tmpdir"
}

# ── Run for each endpoint ─────────────────────────────────────────────────────

{
    echo ""
    echo "### TTFB Results — $(date -u '+%Y-%m-%d %H:%M UTC')"
    echo ""
    echo "Runs: ${TTFB_RUNS} | Concurrency: ${TTFB_CONCURRENCY}"
    echo ""
} >> "$RESULTS_MD"

for endpoint in "${ENDPOINTS[@]}"; do
    IFS='|' read -r method path body accept <<< "$endpoint"
    label="${method} ${path}"
    measure_ttfb "$label" "$method" "$path" "$body" "$accept"
done

echo "==> TTFB results appended to ${RESULTS_MD}"
