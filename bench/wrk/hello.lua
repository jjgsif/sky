-- bench/wrk/hello.lua
-- POST /hello — unary JSON request/response
--
-- Measures pure transport + validation + handler overhead.
-- Baseline for all other comparisons.

wrk.method  = "POST"
wrk.headers["Content-Type"] = "application/json"
wrk.headers["Accept"]       = "application/json"
wrk.body    = '{"name":"bench"}'

local counter    = 0
local errors     = 0
local non200     = 0
local bytes_recv = 0

function response(status, headers, body)
    counter    = counter + 1
    bytes_recv = bytes_recv + #body

    if status == 0 then
        errors = errors + 1
    elseif status >= 400 then
        non200 = non200 + 1
        -- Print first few error bodies to catch validation failures early
        if non200 <= 3 then
            io.write(string.format("\n[error] status=%d body=%s\n", status, body:sub(1, 200)))
        end
    end
end

function done(summary, latency, requests)
    local total = summary.requests
    io.write("\n── Response breakdown ───────────────────────────────\n")
    io.write(string.format("  Total requests  : %d\n",    total))
    io.write(string.format("  Errors (conn)   : %d\n",    errors))
    io.write(string.format("  Non-2xx         : %d\n",    non200))
    io.write(string.format("  Success rate    : %.2f%%\n", (total - errors - non200) / total * 100))
    io.write(string.format("  Bytes received  : %.2f MB\n", bytes_recv / 1024 / 1024))
    io.write("\n── Latency percentiles ──────────────────────────────\n")
    local pcts = {50, 75, 90, 95, 99, 99.9}
    for _, p in ipairs(pcts) do
        io.write(string.format("  p%-5g : %.2f ms\n", p, latency:percentile(p) / 1000))
    end
    io.write("─────────────────────────────────────────────────────\n")
end
