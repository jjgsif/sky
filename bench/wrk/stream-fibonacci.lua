-- bench/wrk/stream-fibonacci.lua
-- GET /stream/fibonacci?limit=20 — CPU-bound streaming NDJSON
--
-- Unlike stream-events, each chunk requires computation on the worker
-- event loop before it can be yielded. This benchmark stresses the
-- interaction between:
--
--   - Worker event loop CPU saturation (Fibonacci compute)
--   - Concurrent INVOKE frame queuing (gateway dispatch backpressure)
--   - Write queue contention (multiple workers streaming simultaneously)
--
-- Expected behaviour under load:
--   - req/s lower than stream-events (compute bottleneck)
--   - p99 latency should be stable — if it climbs sharply under
--     concurrency that indicates event loop starvation, not just
--     compute time.
--   - Watch: unary /hello latency in a separate wrk2 run while this
--     runs concurrently. Isolation between workers means /hello should
--     not degrade. If it does, check gateway accept loop saturation.

wrk.method = "GET"
wrk.headers["Accept"] = "application/x-ndjson"

-- Vary the limit per request to avoid caching effects and
-- simulate realistic mixed-complexity load.
local limits      = {10, 15, 20, 25, 30}
local limit_idx   = 0

function request()
    limit_idx = (limit_idx + 1) % #limits
    local limit = limits[limit_idx + 1]
    wrk.path = "/stream/fibonacci?limit=" .. limit
    return wrk.format(nil)
end

local counter     = 0
local errors      = 0
local non200      = 0
local bytes_recv  = 0
local chunks_seen = 0
local limit_totals = {}

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
    io.write(string.format("  Fib terms total : %d (%.1f avg/req)\n",
        chunks_seen, success > 0 and chunks_seen / success or 0))
    io.write("\n── Full-stream completion latency ───────────────────\n")
    io.write("  (Includes compute time for each Fibonacci term)\n\n")
    local pcts = {50, 75, 90, 95, 99, 99.9}
    for _, p in ipairs(pcts) do
        io.write(string.format("  p%-5g : %.2f ms\n", p, latency:percentile(p) / 1000))
    end
    io.write("─────────────────────────────────────────────────────\n")
end
