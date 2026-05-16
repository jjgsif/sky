import { Service, Handler, Query, type ExtractContext } from "sky-framework/decorators";

const encoder = new TextEncoder();

const eventsExtract = { count: Query("count") } as const;
const fibonacciExtract = { limit: Query("limit") } as const;

@Service({ lifetime: "singleton" })
class StreamService {
  @Handler({
    method: "GET",
    path: "/stream/events",
    streaming: true,
    extract: eventsExtract,
  })
  async events({ count }: ExtractContext<typeof eventsExtract>) {
    const n = Math.min(parseInt(count ?? "10", 10), 100);

    return {
      status: 200,
      headers: { "content-type": "application/x-ndjson" },
      body: generateEvents(n),
    };
  }

  @Handler({
    method: "GET",
    path: "/stream/fibonacci",
    streaming: true,
    extract: fibonacciExtract,
  })
  async fibonacci({ limit }: ExtractContext<typeof fibonacciExtract>) {
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
