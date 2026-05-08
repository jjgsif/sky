import { Service, Handler, Query } from "@sky/decorators";

const encoder = new TextEncoder();

// NOTE: The gateway currently buffers all chunks before forwarding to the HTTP
// client. The streaming path (worker → gateway over UDS) works correctly —
// RESPONSE_CHUNK frames are sent progressively — but the client receives the
// full body in one shot. True HTTP chunked streaming to clients is a future
// gateway enhancement.

@Service({ lifetime: "singleton" })
class StreamService {
  /**
   * GET /stream/events?count=N
   *
   * Streams N NDJSON events. Each line is a JSON object:
   *   {"seq":1,"time":"...","message":"Event 1"}
   *
   * Demonstrates the streaming response path:
   *   handler returns AsyncGenerator<Uint8Array>
   *   → dispatcher sends RESPONSE_HEAD + multiple RESPONSE_CHUNK frames
   *   → gateway assembles the body from all chunks
   */
  @Handler({
    method: "GET",
    path: "/stream/events",
    extract: [Query("count")],
  })
  async events(count?: string) {
    const n = Math.min(parseInt(count ?? "10", 10), 100);

    return {
      status: 200,
      headers: { "content-type": "application/x-ndjson" },
      body: generateEvents(n),
    };
  }

  /**
   * GET /stream/fibonacci?limit=N
   *
   * Streams Fibonacci numbers up to the given limit as NDJSON.
   * Each line: {"index":0,"value":0}
   *
   * Illustrates lazy generation: values are computed only as the
   * consumer pulls them, without buffering the full sequence.
   */
  @Handler({
    method: "GET",
    path: "/stream/fibonacci",
    extract: [Query("limit")],
  })
  async fibonacci(limit?: string) {
    const max = Math.min(parseInt(limit ?? "20", 10), 200);

    return {
      status: 200,
      headers: { "content-type": "application/x-ndjson" },
      body: generateFibonacci(max),
    };
  }
}

async function* generateEvents(count: number): AsyncGenerator<Uint8Array> {
  for (let i = 1; i <= count; i++) {
    const event = {
      seq: i,
      time: new Date().toISOString(),
      message: `Event ${i} of ${count}`,
    };
    yield encoder.encode(JSON.stringify(event) + "\n");
  }
}

async function* generateFibonacci(limit: number): AsyncGenerator<Uint8Array> {
  let a = 0;
  let b = 1;
  let index = 0;

  while (index < limit) {
    yield encoder.encode(JSON.stringify({ index, value: a }) + "\n");
    [a, b] = [b, a + b];
    index++;
  }
}

export { StreamService };
