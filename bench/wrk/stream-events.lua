-- bench/wrk/stream-events.lua
-- GET /stream/events?count=10 — streaming NDJSON response
--
-- Two measurements matter for streaming that wrk2 alone can't split:
--
--   1. TTFB (time to first byte) — how quickly RESPONSE_HEAD + first chunk
--      arrives. Measures gateway dispatch latency, not stream duration.
--
--   2. Full stream completion — total time including all chunks and RESPONSE_END.
--      Measures worker compute + chunk throughput under concurrent load.
--
-- wrk2 buffers the full response before calling response(), so the latency
-- histogram here reflects full stream completion time, not TTFB.
-- Run the companion ttfb.sh script to measure TTFB separately.
--
-- Concurrency note: wrk2 -c100 means 100 streams are open simultaneously.
-- This is the correct way to stress concurrent streaming — not sequential runs.

wrk.method = "GET"
wrk.headers["Accept"] = "application/x-ndjson"

local counter     = 0
local errors      = 0
local non200      = 0
local bytes_recv  = 0
local chunks_seen = 0 -- approximate: count newlines in NDJSON body

function response(status, headers, body)
    counter    = counter + 1
    bytes_recv = bytes_recv + #body

    if status == 0 then
        errors = errors + 1
    elseif status >= 400 then
        non200 = non200 + 1
        if non200 <= 3 then
            io.write(string.format("\n[error] status=%d body=%s\n", status, body:sub(1, 200)))
        end
    else
        -- Count NDJSON lines as a proxy for chunk count
        local _, n = body:gsub("\n", "")
        chunks_seen = chunks_seen + n
    end
end

function done(summary, latency, requests)
    local total   = summary.requests
    local success = total - errors - non200
    io.write("\n── Response breakdown ───────────────────────────────\n")
    io.write(string.format("  Total requests  : %d\n",    total))
    io.write(string.format("  Errors (conn)   : %d\n",    errors))
    io.write(string.format("  Non-2xx         : %d\n",    non200))
    io.write(string.format("  Success rate    : %.2f%%\n", success / total * 100))
    io.write(string.format("  Bytes received  : %.2f MB\n", bytes_recv / 1024 / 1024))
    io.write(string.format("  NDJSON lines    : %d (%.1f avg/req)\n",
        chunks_seen, success > 0 and chunks_seen / success or 0))
    io.write("\n── Full-stream completion latency ───────────────────\n")
    io.write("  (TTFB measured separately — see bench/ttfb.sh)\n\n")
    local pcts = {50, 75, 90, 95, 99, 99.9}
    for _, p in ipairs(pcts) do
        io.write(string.format("  p%-5g : %.2f ms\n", p, latency:percentile(p) / 1000))
    end
    io.write("─────────────────────────────────────────────────────\n")
end
