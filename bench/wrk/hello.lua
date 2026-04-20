-- POST /hello benchmark script for wrk.
-- Usage: wrk -t<N> -c<N> -d<N>s --latency -s bench/wrk/hello.lua <url>

wrk.method  = "POST"
wrk.headers["Content-Type"] = "application/json"
wrk.body    = '{"name":"bench"}'

local errors = 0

function response(status, headers, body)
    if status ~= 200 then
        errors = errors + 1
    end
end

function done(summary, latency, requests)
    if errors > 0 then
        io.write(string.format("\nNon-200 responses: %d\n", errors))
    end
end
